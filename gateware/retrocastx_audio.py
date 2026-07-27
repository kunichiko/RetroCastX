"""RetroCastX 音声キャプチャ: I2S(PCM1808×2) + S/PDIF(TOSLINK)。

基板仕様(hardware/adc-frontend):
- 12.288MHz XO(=256fs@48kHz)が両ADCのSCKIとFPGA(F1)へ入る → "aud"クロックドメイン
- FPGAはMCLKを分周して BCK(64fs)/LRCK(fs) を両ADCへ共通供給、DOUTのみ個別
- S/PDIFはTOSLINK受信モジュールのビットストリームがE19に直結 → sysドメインで
  オーバーサンプリングしてBMC(バイフェーズマーク)復号

出力はいずれも sysドメインの stream.Endpoint(data[15:0]=L, [31:16]=R, s16)。
"""
from migen import *

from litex.gen import LiteXModule
from litex.soc.interconnect import stream


AUDIO_LAYOUT = [("data", 32)]  # L(s16le) | R(s16le)<<16


class I2sCapture(LiteXModule):
    """I2Sマスタークロック生成 + スレーブADC(PCM1808)のキャプチャ(複数DOUT共用)。

    audドメイン(=MCLK 256fs)で動作:
    - bck = MCLK/4 (64fs) 、lrck = MCLK/256 (fs, low=左ch)
    - I2S: LRCKエッジの1BCK後がMSB。BCK立ち上がりでDOUTをサンプル
    - 24bit中の上位16bit(スロット1..16)を取得
    """
    def __init__(self, bck_pad, lrck_pad, dout_pads):
        self.sources = []          # sysドメイン側 (dout_padsと同順)

        # # #

        cnt = Signal(8)            # 1サンプルフレーム = 256 MCLKサイクル
        self.cnt = cnt             # (シミュレーションのADCモデル用に公開)
        self.sync.aud += cnt.eq(cnt + 1)
        self.comb += [
            bck_pad.eq(cnt[1]),
            lrck_pad.eq(cnt[7]),
        ]

        for dout in dout_pads:
            shreg = Signal(16)
            left = Signal(16)
            pulse = stream.Endpoint(AUDIO_LAYOUT)
            slot = cnt[2:7]        # 半フレーム内のBCKスロット 0..31
            self.sync.aud += [
                pulse.valid.eq(0),
                # BCK立ち上がり近傍(スロット中央)でサンプル。MSB=スロット1
                If((cnt[0:2] == 3) & (slot >= 1) & (slot <= 16),
                    shreg.eq(Cat(dout, shreg[:15])),   # MSBファーストで左シフト
                ),
                If(cnt == 255,     # 右chスロット16取得後、フレーム確定
                    left.eq(left), # (leftはcnt==127時点で確定済み)
                    pulse.data.eq(Cat(left, shreg)),
                    pulse.valid.eq(1),
                ),
                If(cnt == 127,     # 左ch確定
                    left.eq(shreg),
                ),
            ]
            # aud → sys のCDC(数サンプル分のバッファで十分)
            cdc = stream.ClockDomainCrossing(AUDIO_LAYOUT, cd_from="aud",
                                             cd_to="sys", depth=8)
            self.submodules += cdc
            self.comb += pulse.connect(cdc.sink, omit={"ready"})
            self.sources.append(cdc.source)


class SpdifDecoder(LiteXModule):
    """S/PDIF(BMC)デコーダ。sysクロックでオーバーサンプリングする。

    - パルス幅を 1UI/2UI/3UI に分類(UI長は1UIパルスのEWMAで追従、
      32k〜48kHz: 50MHzで UI≈8.1〜12.7サイクル)
    - 3UIパルス=プリアンブル開始。続く3パルスで B/M(左)/W(右) を判別
    - 以降28セルをBMC復号(2UI=0, 1UI+1UI=1)。スロット12..27(16bit音声、
      LSBファースト)を取得。V/U/C/Pは無視(v0)
    - rate_hz: 1秒間のフレーム数を数えて実サンプルレートを出力
    """
    def __init__(self, pad, sys_clk_freq):
        self.source = stream.Endpoint(AUDIO_LAYOUT)
        self.rate_hz = Signal(32)
        self.locked = Signal()

        # # #

        # 入力同期化 + エッジ検出
        sig = Signal(2)
        self.sync += sig.eq(Cat(pad, sig[0]))
        edge = sig[0] ^ sig[1]

        # パルス幅計測とUI追従
        width = Signal(8)
        ui = Signal(8, reset=8)                 # 1UIのサイクル数(EWMA)
        is_s = Signal()                          # 1UI
        is_d = Signal()                          # 2UI
        is_t = Signal()                          # 3UI以上
        self.comb += [
            is_s.eq(width < (ui + ui[1:])),                  # < 1.5*ui
            is_d.eq(~is_s & (width < ((ui << 1) + ui[1:]))), # < 2.5*ui
            is_t.eq(~is_s & ~is_d),
        ]
        self.sync += [
            If(edge,
                width.eq(1),
                If(is_s & (width >= 4),
                    ui.eq((ui * 3 + width)[2:]),             # (3*ui+w)/4
                ),
            ).Elif(width != 0xFF,
                width.eq(width + 1),
            ),
        ]

        # プリアンブル判別: 3UIの後の3パルス(UI数)が M:1,1,3 / W:2,1,2 / B:3,1,1
        pre_idx = Signal(2)
        pre_ch_right = Signal()                  # 判別結果: 1=右ch(W)
        slot = Signal(6)                         # 4..31
        cell_open = Signal()                     # 1UI消費済み(bit=1の前半)
        bit = Signal()
        audio = Signal(16)
        left = Signal(16)
        have_left = Signal()
        frames = Signal(16)                      # 1秒あたりのフレーム計数

        self.fsm = fsm = FSM(reset_state="HUNT")
        fsm.act("HUNT",
            If(edge & is_t,
                NextValue(pre_idx, 0),
                NextState("PREAMBLE"),
            ),
        )
        fsm.act("PREAMBLE",
            If(edge,
                NextValue(pre_idx, pre_idx + 1),
                # 2パルス目(idx=0)がUI数を決める: M=1, W=2, B=3
                If(pre_idx == 0,
                    NextValue(pre_ch_right, is_d),           # W(右)のみ2UI
                ),
                If(pre_idx == 2,                             # 3パルス消費完了
                    NextValue(slot, 4),
                    NextValue(cell_open, 0),
                    NextState("DATA"),
                ),
                If(is_t & (pre_idx != 2) & (pre_idx != 0),
                    NextState("HUNT"),                       # 想定外の3UI
                ),
            ),
        )
        self.comb += self.source.data.eq(Cat(left, audio))
        fsm.act("DATA",
            If(edge,
                If(is_t,
                    # 次のプリアンブル開始(スロット31完了直後なら正常)
                    NextValue(pre_idx, 0),
                    NextState("PREAMBLE"),
                ).Elif(cell_open,
                    # 1UIパルスの後半 → bit=1(2UI幅ならビット境界ずれ: 再同期)
                    If(is_s,
                        NextValue(cell_open, 0),
                        bit.eq(1),
                    ).Else(
                        NextState("HUNT"),
                    ),
                ).Elif(is_s,
                    NextValue(cell_open, 1),
                ).Else(
                    bit.eq(0),                               # 2UI = bit 0
                ),
            ),
        )
        bit_done = Signal()
        self.comb += bit_done.eq(edge & fsm.ongoing("DATA") &
                                 ((cell_open & is_s) | (~cell_open & is_d)))
        self.sync += [
            self.source.valid.eq(0),
            If(bit_done,
                If((slot >= 12) & (slot <= 27),
                    audio.eq(Cat(audio[1:], (cell_open & is_s))),  # LSBファースト
                ),
                If(slot == 31,
                    If(~pre_ch_right,
                        left.eq(audio),
                        have_left.eq(1),
                    ).Elif(have_left,
                        self.source.valid.eq(1),                   # L/Rペア確定
                        have_left.eq(0),
                    ),
                ).Else(
                    slot.eq(slot + 1),
                ),
            ),
        ]

        # サンプルレート実測(1秒窓のフレーム数)と簡易ロック判定
        sec = Signal(max=int(sys_clk_freq))
        self.sync += [
            If(self.source.valid, frames.eq(frames + 1)),
            If(sec == int(sys_clk_freq) - 1,
                sec.eq(0),
                self.rate_hz.eq(frames),
                frames.eq(0),
            ).Else(sec.eq(sec + 1)),
            self.locked.eq(fsm.ongoing("DATA") | fsm.ongoing("PREAMBLE")),
        ]
