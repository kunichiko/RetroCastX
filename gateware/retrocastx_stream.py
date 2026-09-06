#!/usr/bin/env python3
"""RetroCastX gateware step2: test-pattern streamer on Colorlight i5.

プロトコルv0の実装(ハードウェアのみ、CPU不介在):

- ANNOUNCE(TYPE_INFO): 毎秒送出 + SUBSCRIBE受信時に送信元へ即時返信
- SUBSCRIBE(type=4)受信: flags bit0(ANNOUNCE_ONLY)=0 なら映像ストリームの
  送り先をそのパケットの送信元(IP/ポート)に切替。最後のSUBSCRIBEから
  10秒で購読失効(PC側 receiver --subscribe が定期再送する)
- MODE(type=1): 購読開始時に即時 + 毎秒再送
- LINE(type=0): テストパターン(host/python の pattern.make_frame と同一の
  カラーバー+スイープライン+白枠)を RGB555 で1ライン=1パケット送出

Build:
    .venv/bin/python retrocastx_stream.py --build
Load (board via EXT DAPLink):
    openFPGALoader -c cmsisdap build/colorlight_i5/gateware/colorlight_i5.bit
Verify on PC (same L2 segment):
    python3 -m retrocastx.receiver --subscribe --dump out
Simulation (no hardware):
    .venv/bin/python sim_stream.py
"""
import argparse
import struct

from migen import *

from litex.build.generic_platform import IOStandard, Pins, Subsignal, Misc
from litex.gen import LiteXModule
from litex.soc.cores.clock import ECP5PLL
from litex.soc.integration.soc_core import SoCMini
from litex.soc.integration.builder import Builder

# --- ネットワーク設定(暫定; 将来はSPIフラッシュの設定ページから読む) ---
MAC_ADDRESS = 0x025243580001          # ローカル管理アドレス "RCX"
FPGA_IP     = "192.168.10.50"
HOST_IP     = "192.168.10.1"          # SUBSCRIBE到着までのANNOUNCE宛先(初期値)
UDP_PORT    = 34600
# --- 伝送ピクセル形式(docs/protocol-v0.md / host の protocol.py と一致必須) ---
PIXFMT_RGB888 = 0             # 3B/px byte0=R byte1=G byte2=B
PIXFMT_RGB555 = 1             # 2B/px 0RRRRRGGGGGBBBBB
PIXFMT_YC8    = 3             # 2B/px 下位=緑ch8bit(CVBS/Y) 上位=赤ch8bit(S-VideoのC)
SCROLL_PX   = 4               # テストパターンのカラーバー横スクロール量[px/frame](host pattern.pyと一致必須)

# パケットタイプ(FSM内部コード。プロトコル上のtype値とは別)
_T_LINE, _T_MODE, _T_ANN, _T_AUDIO, _T_CFG = 0, 1, 2, 3, 4


def convert_ip(ip_str):
    ip = 0
    for x in ip_str.split("."):
        ip = (ip << 8) | int(x)
    return ip


def make_announce_payload() -> bytes:
    """host/python/retrocastx/protocol.py の Announce と同一フォーマット(40B)。
    共通ヘッダの seq はゲートウェアが動的に差し替える。"""
    common = struct.pack("<BBBBHH", 0x52, 0x00, 3, 0, 0, 0)  # magic,ver,INFO,flags,frame,seq
    mac = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
    ip = bytes(int(x) for x in FPGA_IP.split("."))
    info = struct.pack("<6s4sHHH16s", mac, ip, UDP_PORT, 0x0001, 0x0000, b"retrocastx-i5")
    return common + info


class PatternPixel(LiteXModule):
    """host/python/retrocastx/pattern.py の make_frame と同一パターンの1ピクセルを
    (x, y, frame) から組み合わせ回路で生成し、RGB555(0RRRRRGGGGGBBBBB)で出力する。

    make_frame との一致条件: width/height は2のべき乗(バー幅 = width/8、
    スイープ位置 = frame % height がビットスライスで表せるため)。
    """
    def __init__(self, width, height):
        assert width & (width - 1) == 0 and width >= 8
        assert height & (height - 1) == 0
        self.x     = Signal(max=width)
        self.y     = Signal(max=height)
        self.frame = Signal(16)
        self.pix   = Signal(16)  # bit15 = 0

        # # #

        def to555(r, g, b):
            return ((r >> 3) << 10) | ((g >> 3) << 5) | (b >> 3)

        bars888 = [(255, 255, 255), (255, 255, 0), (0, 255, 255), (0, 255, 0),
                   (255, 0, 255), (255, 0, 0), (0, 0, 255), (32, 32, 32)]
        bars = Array(Constant(to555(*c), bits_sign=16) for c in bars888)
        bar_shift = log2_int(width) - 3

        # スイープラインの色 = (frame*7 % 256, 255 - frame*5 % 256, frame % 256)
        f8 = self.frame[:8]
        r8 = Signal(8)
        g8 = Signal(8)
        b8 = Signal(8)
        self.comb += [
            r8.eq((f8 * 7)[:8]),
            g8.eq(255 - (f8 * 5)[:8]),
            b8.eq(f8),
        ]

        border = ((self.x == 0) | (self.x == width - 1) |
                  (self.y == 0) | (self.y == height - 1))
        sweep = self.y == self.frame[:log2_int(height)]
        # カラーバーをフレーム毎に横スクロール(SCROLL_PX/frame)。x に frame*SCROLL_PX を
        # 加算し width幅で自動ラップ。host pattern.make_frame と同一(検証一致を維持)。
        scroll_x = Signal(max=width)
        self.comb += scroll_x.eq(self.x + self.frame * SCROLL_PX)
        self.comb += [
            If(border,
                self.pix.eq(0x7FFF),
            ).Elif(sweep,
                self.pix.eq(Cat(b8[3:], g8[3:], r8[3:])),
            ).Else(
                self.pix.eq(bars[scroll_x[bar_shift:]]),
            ),
        ]


class RetroCastXStreamer(LiteXModule):
    """プロトコルv0のTX(ANNOUNCE/MODE/LINE)とRX(SUBSCRIBE)を1本のUDPポートで捌く。

    32bitデータパス。ワード内バイト順はリトルエンディアン詰め
    (LiteX StrideConverterは下位バイトから送受する)。
    """
    def __init__(self, udp_port, sys_clk_freq, width=512, height=512, fps=30.0,
                 announce_period=1.0, mode_period=1.0, sub_timeout=10.0,
                 announce_ip=HOST_IP, udp_port_nr=UDP_PORT,
                 mtu_payload=1472, interlace=False,
                 audio_sources=None, audio_nsamples=240,
                 mac_address=MAC_ADDRESS, capture=None,
                 cfg_vbp=0, cfg_hs_offset=0, cfg_pll_divide=1104,
                 cfg_in_mux1=0xAA,
                 extra_stats=None,
                 ):
        # extra_stats: {CONFIGキー: 読み出し専用のSignal}。ストリーマの外側にある
        # 診断値(いまはARP学習)をCONFIGのGETで読めるようにするための口。
        # capture: TvpCapture(sysドメインI/F) を渡すと、テストパターンの代わりに
        #   実キャプチャライン(line_valid/line_row/line_frame/rd_*)を源にする。
        #   None の場合は従来どおり自走テストパターン。
        cap_mode = capture is not None
        # 自MAC(SUBSCRIBE/CONFIGの宛先照合とCONFIG応答に使用)。
        # リトルエンディアンのワード表現(伝送バイト順=MAC表記順)
        mac_bytes = mac_address.to_bytes(6, "big")
        my_mac_lo = int.from_bytes(mac_bytes[0:4], "little")
        my_mac_hi = int.from_bytes(mac_bytes[4:6], "little")
        # audio_sources: [(stream.Endpoint(AUDIO_LAYOUT/sysドメイン), rate_hz), ...]
        #   インデックスがプロトコルのsource値(0=RGB端子音声,1=LINE,2=S/PDIF)。
        #   rate_hz は int 定数(水晶由来)または Signal(32)(S/PDIF実測)
        audio_srcs = audio_sources or []
        n_aud = len(audio_srcs)
        assert 20 + 4 * audio_nsamples <= mtu_payload or n_aud == 0
        # --- タイミング諸元(host/python の sender_sim.Sender と同一の算出式) ---
        # interlace時: fpsはフィールドレート、height/vactiveはフルフレーム行数。
        # frameカウンタはフィールド毎+1、LINE.lineはフルフレーム行(protocol-v0.md)
        htotal = int(width * 1.28)
        vtotal = int(height * 1.06)
        dotclk = int(htotal * vtotal * fps) if not interlace else \
                 int(htotal * vtotal * fps / 2)
        hfreq_mhz = int(dotclk / htotal * 1000)
        vfreq_mhz = int(fps * 1000)
        mode_id = 1
        # 伝送ピクセル形式(protocol-v0.md の pixfmt)。1=RGB555 が既定。
        # 3=YC8 は同じ2B/pxで「下位=緑ch8bit / 上位=赤ch8bit」を生のまま運ぶ。
        # コンポジット/S-Video は5bitでは副搬送波の位相が推定できないので必要。
        # 2B/px は変わらないので断片化・MTU計算・受信側のバッファ確保は共通。
        pixfmt = Signal(8, reset=PIXFMT_RGB555)   # key 0x36
        # ★**伝送形式は実行時に切り替えられる**(key 0x36)。
        #   RGB888(3B/px)と RGB555・YC8(2B/px)は 1語に入る画素数が違うので、
        #   断片長・語数・xの進み・読出アドレス・語の組み立てがすべて変わる。
        #   それらをこの信号で選ぶ。
        #   ※当初ビルド時の選択にしていたが、**YC8(コンポジット/S端子)が
        #     使えなくなる**という機能の後退を伴うので実行時切替へ直した。
        #     懸念していたタイミングは実測で余裕があった
        #     (eth_rx 132.57MHz / 制約125、sys 54.94MHz / 制約45)。
        fmt888 = Signal()
        self.cfg_pixfmt = pixfmt
        # MODE の mflags。bit0 = MFLAG_INTERLACE。
        #
        # 実行時に変わる。織り込むと1VSYNC周期あたりのスロット数が半分になる
        # (プログレッシブは1ラインが2スロット、インターレースは折り返して
        #  2フィールドが交互に入るので1ラインが1スロット)。受信側はこの差を
        # 知らないと縦が2倍に引き伸ばされる。
        mflags = Signal(16, reset=1 if interlace else 0)
        self.stat_interlaced = Signal()

        lines_per_unit = height // 2 if interlace else height  # 伝送単位あたりの行数
        unit_ticks = htotal * (vtotal // 2 if interlace else vtotal)  # ts歩進/伝送単位
        assert not interlace or height % 4 == 0

        self.comb += fmt888.eq(pixfmt == PIXFMT_RGB888)

        # --- ライン断片化(host/python の protocol.fragment_line と同一の分割) ---
        #
        # ★**伝送形式は実行時に切り替える**(key 0x36)。
        #
        #     RGB555 / YC8 : 2B/px。2画素=1語
        #     RGB888       : 3B/px。4画素=3語(12バイト)
        #
        #   ピクセル数はワード整列のため4の倍数にする(3n が4の倍数になるのは
        #   n が4の倍数のときだけ。RGB555 でも4の倍数なら2の倍数を満たす)。
        # ★整列は**常に4画素**にする。RGB888 が4画素=3語なので4の倍数が要り、
        #   RGB555 でも4の倍数は2の倍数を満たすため両立する。実行時に切り替える
        #   以上、整列単位を形式で変えると断片の途中で辻褄が合わなくなる。
        PX_ALIGN = 4
        FRAG_PX_555 = ((mtu_payload - 20) // 2) & ~(PX_ALIGN - 1)
        FRAG_PX_888 = ((mtu_payload - 20) // 3) & ~(PX_ALIGN - 1)
        assert min(FRAG_PX_555, FRAG_PX_888) >= PX_ALIGN, "MTU too small"
        FRAG_PX = Signal(16)           # 1断片の最大ピクセル数(形式で決まる)
        self.comb += FRAG_PX.eq(Mux(fmt888, FRAG_PX_888, FRAG_PX_555))
        def _nw(n):                    # パターン生成側(RGB555固定)用
            return 5 + n // 2
        # パターン生成側(cap_mode でない)は RGB555 固定。断片表はビルド時に作る
        frags = []
        off = 0
        while off < width:
            n = min(FRAG_PX_555, width - off)
            frags.append((off, n))
            off += n
        assert all(n % PX_ALIGN == 0 for _, n in frags), \
            "断片のピクセル数が %d の倍数にならない (width=%d)" % (PX_ALIGN, width)
        n_frags = len(frags)
        frag_off_arr = Array(Constant(o, bits_sign=16) for o, _ in frags)
        frag_cnt_arr = Array(Constant(n, bits_sign=16) for _, n in frags)
        frag_len_arr = Array(Constant(20 + 2 * n, bits_sign=16) for _, n in frags)
        frag_nwords_arr = Array(Constant(_nw(n), bits_sign=16) for _, n in frags)
        # 実行時の値(Signal)から length / nwords を作る式。形式で分ける。
        #   RGB555/YC8: length = 20 + 2n   nwords = 5 + n/2
        #   RGB888    : length = 20 + 3n   nwords = 5 + 3n/4  (n は4の倍数)
        def _len_expr(n):
            # 20 + 2n / 20 + 3n
            return 20 + Mux(fmt888, n + Cat(C(0, 1), n), Cat(C(0, 1), n))
        def _nw_expr(n):
            # 5 + n/2 / 5 + 3n/4(n は4の倍数)
            return 5 + Mux(fmt888, n[2:] + Cat(C(0, 1), n[2:]), n[1:])
        aud_nwords = 5 + audio_nsamples
        # ★nwords / word_idx の幅は**両方の形式の最悪値**から決める。
        #
        #   捕捉モードでは断片のピクセル数が実行時に決まる(n_c = min(FRAG_PX, 幅))
        #   ので、ビルド時の frags 表(RGB555固定)だけから幅を取ると RGB888 で
        #   足りなくなる。足りないと nwords が黙って切り捨てられ、`last` が
        #   早く立って**先頭1語だけのパケット**が出る(2026-09-03 に W=16 の
        #   試験で発覚。13→4bit に対し RGB888 は 17 語で 17&0xF=1 になっていた)。
        #   width=1024 の実機ビルドは 555 が 367 語で 9bit になり 888 の 368 語が
        #   たまたま収まっていた。偶然で成立していただけなので必ず両方見る。
        px_max = ((width + PX_ALIGN - 1) // PX_ALIGN) * PX_ALIGN
        nw_555_max = _nw(min(FRAG_PX_555, px_max))
        nw_888_max = 5 + 3 * min(FRAG_PX_888, px_max) // 4
        max_nwords = max(10, 8, aud_nwords if n_aud else 0,
                         *(_nw(n) for _, n in frags),
                         nw_555_max, nw_888_max)

        line_interval = int(sys_clk_freq / (fps * lines_per_unit))
        assert line_interval >= 1

        ann_payload = make_announce_payload()
        ann_words = [int.from_bytes(ann_payload[i:i + 4], "little")
                     for i in range(0, len(ann_payload), 4)]
        n_ann_words = len(ann_words)       # 10

        # --- 状態レジスタ ---
        seq      = Signal(16)
        frame    = Signal(16)              # interlace時はフィールドカウンタ
        line     = Signal(max=max(lines_per_unit, 2))  # 伝送単位内の行番号
        field    = Signal()                # インターレースのフィールド極性(0=偶)
        frag_idx = Signal(max=max(n_frags, 2))
        # 断片パラメータは実行時に決める。ライン毎に「黒でない範囲」だけを送るので、
        # 送出開始位置も長さも一定ではない。これにより hs_offset が不要になる
        # (取り込みはラインの頭から。どこを送るかはFPGAが中身を見て決める)。
        frag_off = Signal(16)          # この断片の先頭ピクセル位置(ライン内絶対)
        frag_cnt = Signal(16)          # この断片のピクセル数
        frag_last = Signal()           # 最終断片か
        px_end = Signal(16)            # このラインで送る範囲の終端(この値は含まない)
        # RGB888(4画素=3語)の語内位相 0..2。2B/px のときは使わない
        gph = Signal(2)
        # RGB888 の語1は2エントリを跨ぐため、前エントリをラッチして持つ
        ent_prev = Signal(64)

        def _px_adv():
            """ペイロード1語を送出したときの前進動作(形式で変わる)。

            RGB555/YC8: 1語=2画素なので毎語 x += 2
            RGB888    : 4画素=3語。位相を回し、群の最後(位相2)で x += 4
            """
            return [If(fmt888,
                       If(gph == 2,
                           NextValue(gph, 0),
                           NextValue(x, x + 4),
                       ).Else(
                           NextValue(gph, gph + 1),
                       ),
                   ).Else(
                       NextValue(x, x + 2),
                   )]
        ts_frame = Signal(32)              # 現伝送単位先頭のドットクロックカウンタ
        ts_line  = Signal(32)              # 現ライン先頭(= ts_frame + line*htotal)

        # MODEで報告する諸元。capture時はFPGAが実信号から測った値を使う(ビルド時の
        # 仮定値ではない)。周波数フィールドはmHz単位なので実測Hzを1000倍する。
        mode_htotal = Signal(16); mode_vtotal = Signal(16)
        # 送出する行数。vtotalが小さいモードでは512行送ると下が空くので実測に従う
        mode_vactive = Signal(16)
        mode_dotclk = Signal(32); mode_hfreq = Signal(32); mode_vfreq = Signal(32)
        if cap_mode:
            self.comb += [
                mode_htotal.eq(capture.meas_htotal),
                mode_vtotal.eq(capture.meas_vtotal),
                mode_vactive.eq(capture.out_vactive),
                mode_dotclk.eq(capture.meas_dotclk),
                mode_hfreq.eq(capture.meas_hfreq * 1000),
                mode_vfreq.eq(capture.meas_vfreq),   # 既にmHz(8秒積算で0.125Hz分解能)
            ]
        else:
            self.comb += [
                mode_htotal.eq(htotal), mode_vtotal.eq(vtotal),
                mode_vactive.eq(height),
                mode_dotclk.eq(dotclk), mode_hfreq.eq(hfreq_mhz),
                mode_vfreq.eq(vfreq_mhz),
            ]

        # 実アナログキャプチャ模擬: 源はGbEの状態に無関係に一定間隔で走り続ける。
        # cap_* が「今キャプチャ中」の位置(自走)。line/frame/ts_* は送信開始時に
        # cap_* からスナップショットする(送信中は固定)。送信FSMがビジーで間に合わ
        # なかったラインは送られない=ライン単位でドロップ(バックプレッシャしない)。
        cap_line     = Signal(max=max(lines_per_unit, 2))
        cap_frame    = Signal(16)
        cap_ts_frame = Signal(32)
        cap_ts_line  = Signal(32)

        # フルフレーム座標の行番号。capture時は送信開始時にラッチした行を使う。
        row = Signal(max=height)
        cap_row = Signal(max=height)               # capture: ラッチした送信対象行
        if cap_mode:
            self.comb += row.eq(cap_row)
        elif interlace:
            self.comb += row.eq(Cat(field, line))   # row = line*2 + field
        else:
            self.comb += row.eq(line)

        ann_ip   = Signal(32, reset=convert_ip(announce_ip))
        ann_port = Signal(16, reset=udp_port_nr)
        sub_ip   = Signal(32)
        sub_port = Signal(16)
        sub_timer = Signal(max=max(int(sys_clk_freq * sub_timeout), 2))
        sub_valid = Signal()
        self.comb += sub_valid.eq(sub_timer != 0)

        # --- RX: SUBSCRIBE(16B=4ワード) / CONFIG(24B=6ワード)。いずれも
        #     w2/w3に宛先MACを含み、自MACかFF×6(ワイルドカード)のみ受理する ---
        rx = udp_port.source
        rx_first = Signal(reset=1)
        self.comb += rx.ready.eq(1)
        self.sync += If(rx.valid, rx_first.eq(rx.last))
        rx_magic_ok = (rx.data[0:8] == 0x52) & (rx.data[8:16] == 0x00)

        in_pkt = Signal()                        # magic/version OK
        rx_type = Signal(8)
        rx_flags = Signal(8)
        rx_widx = Signal(3)                      # 先頭以降のワード番号(1..7飽和)
        rx_src_ip = Signal(32)
        rx_src_port = Signal(16)
        rx_mac_lo = Signal(32)
        cfg_target = Signal(8)
        cfg_op = Signal(8)
        cfg_key = Signal(16)
        cfg_mac_ok = Signal()                    # CONFIG: w3時点でMAC確定
        cfg_ip = Signal(32)
        cfg_port = Signal(16)

        # SUBSCRIBEはMAC上位2BがW3=最終ビートに乗るため、その場で判定する
        sub_done = rx.valid & rx.last & in_pkt & (rx_type == 4) & (rx_widx == 3)
        sub_mac_ok = (((rx_mac_lo == my_mac_lo) & (rx.data[0:16] == my_mac_hi)) |
                      ((rx_mac_lo == 0xFFFFFFFF) & (rx.data[0:16] == 0xFFFF)))
        sub_hit_any = sub_done & sub_mac_ok
        sub_hit = sub_hit_any & ~rx_flags[0]     # flags bit0 = ANNOUNCE_ONLY
        cfg_done = (rx.valid & rx.last & in_pkt & (rx_type == 5) &
                    (rx_widx == 5) & cfg_mac_ok)

        # --- CONFIG(target=0)で実行時に変えられる画枠パラメータ ---
        # モードごとに最適値が違い、ビルドし直していては追い込めないため、
        # Viewerから即時に調整できるようにする。見つけた値がモード表の中身になる。
        self.cfg_vbp        = Signal(13, reset=cfg_vbp)         # key 0x10
        self.cfg_hs_offset  = Signal(13, reset=cfg_hs_offset)   # key 0x11
        self.cfg_pll_divide = Signal(12, reset=cfg_pll_divide)  # key 0x12
        # 診断用: 1ラインまるごと送る(非黒範囲の最適化を切る)。key 0x30。
        # 「全黒行の直後の数行だけ count_px=0 で届く」現象が、範囲の判定
        # (ln_first/ln_last)の問題なのか、バッファへの書き込み自体の問題なのかを
        # 実行時のA/Bで切り分けるために入れた。1にすると範囲を無視して 0..htotal
        # を送るので、範囲判定が原因なら直り、書き込みが原因なら直らない。
        self.cfg_full_line  = Signal(reset=0)                    # key 0x30
        self.cfg_video_bw   = Signal(4, reset=0xF)               # key 0x17
        self.cfg_fine_clamp = Signal(8, reset=0x87)              # key 0x18
        # H-PLL制御(reg 03h)。VCOレンジはピクセルクロックで決まる(データシート
        # Table 4 / 03h の定義)。X68000は全モード PCLK < 36MHz で Ultra low。
        # 既定は 31kHz 768x512 用の 18h。Viewerがモードから計算して送り直す。
        self.cfg_pll_ctl    = Signal(8, reset=0x18)              # key 0x19 (reg 03h)
        self.cfg_clamp_start= Signal(8, reset=0x32)              # key 0x1A (reg 05h)
        self.cfg_clamp_width= Signal(8, reset=0x20)              # key 0x1B (reg 06h)
        # ★★**細ゲインの電源投入時の実効値はここ。**
        #   retrocastx_i2c.py 側にも同名の Signal と reset があるが、
        #   下の方(SoC の結線)で `status.cfg_gain_* <- streamer.cfg_gain_*` を
        #   平文の comb で毎サイクル駆動しているため、**i2c 側の reset は死んでいる**。
        #   2026-09-03、i2c 側だけ直して「直したつもり」になり、焼き直しても
        #   古い値が出て遠回りした。値の根拠(実測)は retrocastx_i2c.py のコメント。
        self.cfg_gain_b     = Signal(8, reset=57)                # key 0x1C (reg 08h)
        self.cfg_gain_g     = Signal(8, reset=61)                # key 0x1D (reg 09h)
        self.cfg_gain_r     = Signal(8, reset=64)                # key 0x1E (reg 0Ah)
        self.cfg_phase      = Signal(5, reset=16)                # key 0x1F (reg 04h)
        self.cfg_sync_ctl   = Signal(8, reset=0x52)              # key 0x22 (reg 0Eh)
        # --- 映像レベルCSYNCをSOG経由で受けるための分離パラメータ(2026-08-11) ---
        # MSXのようにC-SYNCしか出ない機種を SOGIN に1nFで結合して受ける運用で使う。
        # cfg_sync_ctl(0Eh)=0x5B でHもVもSOGから取るようにしたうえで、下の3つを振る。
        # reg 10h は複合レジスタで、bit0=Red CS / bit2=Blue CS のクランプ選択と
        # SOG Threshold[7:3] が同居している。下位3bitは 0x58 で確定した値(R/G/B全て
        # bottom-levelクランプ。ここを崩すと黒が R,B≈128 になって背景が紫がかる)。
        # よって**閾値だけを5bitで持ち**、書き込み時に Cat(0b000, thresh) に組み立てる。
        # 生の8bitを持たせるとクランプ修正を巻き込んで壊す(実際に踏みかけた)。
        self.cfg_sog_thresh = Signal(5, reset=0x58 >> 3)          # key 0x50 (reg 10h[7:3])
        # reg 11h は TVP既定 0x20 では MIN の真上でマージンが無く、C-SYNC運用で
        # 水平パルスをVと誤判定する(実測: Lines per Frame が 1 になる)。
        # データシートの 480i60Hz 中央値 0x75 を既定にする。詳細は retrocastx_i2c.py
        self.cfg_sep_thresh = Signal(8, reset=0x75)               # key 0x51 (reg 11h)
        # Pre/Post-Coast は TVP既定0で「coastが生成されない」(データシート:
        # 「A minimum setting of 1 is required to guarantee generation of an
        # internal coast signal.」)。単位はHSYNC周期。推奨値の表では
        # 480i/p と 576i/p が 3/3、1080i/p・720p・PC SOG Graphics が 1/0。
        # 15kHz機(MSX等)は480i/p族なので 3/3 を既定にする。
        self.cfg_precoast   = Signal(8, reset=3)                  # key 0x52 (reg 12h)
        self.cfg_postcoast  = Signal(8, reset=3)                  # key 0x53 (reg 13h)
        # 同期処理制御(レジスタ 22h)。既定 08h。**bit0 VS Bypass が本命。**
        # 既定(VS Select=0, VS Bypass=0)では「同期セパレータの活動が無いときは
        # ハーフライン積算器が VSOUT を生成する」という条件分岐が働く。積算器は
        # インターレース用でNTSCの525ライン/フレームを前提にするため、262ライン
        # progressive の MSX では **2フレームに1回** しか V を出さない。どちらが勝つかは
        # ロック取得時に決まるので、実測では 1×/2× がほぼ50/50で双安定になった
        # (VSOUT間のHSOUT数が 523 と 1046 を行き来する = 画面が上下に二重化する)。
        # bit0=1 にすると VSOUT が同期セパレータ直結になり決定的になる。
        # 注: VSOUTの遅延が reg 11h に依存するようになり、reg 35h は無効になる。
        self.cfg_sync_ctl2  = Signal(8, reset=0x08)               # key 0x5B (reg 22h)
        # 同期バイパス(レジスタ 36h)。既定 00h。bit0 HS BP / bit1 VS BP。
        # HS BP=1 で HSOUT が「生の未処理HSYNC」になる。HSOUTが入力の2倍で出ている
        # 問題が、SOGスライサ由来か後段の同期処理/PLL由来かを切り分けるのに使う
        # (データシートは通常運用には非推奨としているので、診断用)。
        self.cfg_sync_bypass = Signal(8, reset=0x00)              # key 0x5C (reg 36h)
        # 入力Mux選択2(レジスタ 1Ah)。bit[7:6]=SOG LPF, bit[5:4]=クランプLPF。
        # 既定 C2h は SOG LPF バイパス + クランプ 4.8MHz(HDTV向け)。15kHz機は
        # SOG=2.5MHz / クランプ=0.5MHz(SDTV向け) が適正なので 0x12 を既定にする。
        # 詳細な根拠は retrocastx_i2c.py 側のコメント参照
        self.cfg_in_mux2    = Signal(8, reset=0x12)               # key 0x5D (reg 1Ah)
        # --- コンポジット/S端子を受けるためのアナログ前段(2026-08-14) ---
        # 全部「モード固有の設定」で、RGB入力では既定値に戻すこと。
        # 根拠と数値の内訳は retrocastx_i2c.py 側の同名信号のコメントに全部ある。
        #
        #                                RGB入力  コンポジット  S端子
        #   clamp_sel      reg 10h[2:0]  0b000    0b010(緑ミッド) 0b001(赤ミッド)
        #   coarse_gain_gb reg 1Bh       0x77     0x07(緑0.5倍)  0x77(Yは既定でよい)
        #   coarse_gain_r  reg 1Ch       0x07     0x07           0x07(Cも既定でよい)
        #   coarse_off_g   reg 1Fh[5:0]  0x10     0x10           0x10
        #   coarse_off_r   reg 20h[5:0]  0x10     0x10           0x10
        #
        # ★コンポジットでミッドレベルにするだけでは白が飽和する。ADCフルスケールは
        #   1Vppで、ブランキングが512に座ると +100 IRE(=714mV)に使えるのは残り
        #   512コードだけ。データシート1Bhの条件「Vpp × Gain < 1Vpp」も既定1.2倍では
        #   破れる。**クランプ選択と粗ゲインは必ず一緒に動かす。**
        # ★S端子は Y(緑)がボトム・C(赤)がミッドで、どちらも既定ゲインで収まる。
        #   Yは輝度専用でバースト用のヘッドルームが要らず、Cは輝度と範囲を分け合わない
        #   ため。**1Bhは Blue/Green しか持たないので、Cの調整には 1Ch が要る。**
        self.cfg_clamp_sel  = Signal(3, reset=0x58 & 0x07)        # key 0x5E (reg 10h[2:0])
        self.cfg_coarse_gain_gb = Signal(8, reset=0x77)           # key 0x5F (reg 1Bh)
        self.cfg_coarse_off_g   = Signal(6, reset=0x10)           # key 0x65 (reg 1Fh[5:0])
        self.cfg_coarse_gain_r  = Signal(8, reset=0x07)           # key 0x67 (reg 1Ch)
        self.cfg_coarse_off_r   = Signal(6, reset=0x10)           # key 0x68 (reg 20h[5:0])
        # 入力MUX(reg 19h)。[7:6]=SOG [5:4]=Red [3:2]=Green [1:0]=Blue、00=_1/01=_2/10=_3。
        # **これが実行時に振れないと入力方式を切り替えられない。** v0.9.0 は入力ごとに
        # 系統が分かれているので、方式ごとに 19h が丸ごと別の系統を指す:
        #   S端子 / コンポジット (J5 2x4) → 0x00 (全て _1)
        #   第2入力 MSX等 (aux 2x5)      → 0x55 (全て _2、CSYNCは SOGIN_2)
        #   X68000 RGB (D-SUB)           → 0xAA (全て _3)
        # 初期値はビルド引数由来(retrocastx_i2c.py の MUX1 と同じ式)。
        self.cfg_in_mux1        = Signal(8, reset=cfg_in_mux1)    # key 0x69 (reg 19h)
        # SOGOUTのLow期間を「垂直ブロードパルス」とみなす閾値[pixクロック]。
        # MSXの水平同期は約4.7us、垂直ブロードパルスは約27us。DATACLK 21.48MHz なら
        # それぞれ約101と約580クロックなので、その間の400を既定にする。
        # 機種で幅が違うので実行時に振れるようにしてある (key 0x64)。
        self.cfg_sog_vth    = Signal(16, reset=400)               # key 0x64
        # インターレースのフィールド極性の向き。どちらのフィールドが半ライン下かは
        # 物理で決まるが、SOGOUTの極性やスライスの都合で逆に出ることがあるので、
        # 実機で見て入れ替えられるようにしておく (key 0x66)
        self.cfg_field_invert = Signal()                          # key 0x66
        # 1 で**生同期の位相を使わず SOGOUT の位相からフィールド極性を決める**。
        #
        # ★**未接続の生同期入力は自己発振する。** コンポジット/S端子/MSX では
        #   D-SUB の HSYNC/VSYNC に何も繋がらないが、5Vトレラントのシュミット
        #   バッファ(main.ato の buf_sync)の入力が浮いて発振し、その周期が
        #   raw_ok の窓 (64, 0xF000) にたまたま入る。すると捕捉側は
        #   **存在しない同期の位相 ph_raw** でフィールド極性を決めてしまい、
        #   極性が交互にならず**両フィールドが同じスロットに交互に上書きされて
        #   絵が1ライン上下に震える**(実機 2026-09-06、コンポジット480i。
        #   il: raw0 rawok1 P/I1 frame0 / SOG lowmax 773 = SOG側は健全だった)。
        #   raw_ok の「配線が無いなら周期は0か飽和値」という前提が成り立たない。
        #
        #   ★**1 にして直ることを実機で確認した**(2026-09-06、同じコンポジット480i)。
        #     震えが消え、Viewer の有効映像の高さが **217/240 → 432** になった
        #     (それまでは片フィールドぶんしか無く、織り込みが成立していなかった)。
        #     rawok は 1 のままなので、効いたかどうかは rawok ではなく
        #     「織り込みが成立したか」で見ること。
        #
        #   ★raw_ok の条件を厳しくする案は採らない。fld_pos は組合せ経路が
        #     eth_rx まで伸びていて、**以前この経路に比較を1つ足しただけで
        #     123.59MHz まで落ちて要求125MHzを割った**。こちらは既に式の中にある
        #     信号を定数0から実信号にするだけなので、論理段数が増えない。
        self.cfg_no_raw_phase = Signal()                          # key 0x6A
        # TVPのステータス 38h(bit5=P/I detect)。上位から供給される読み出し専用
        self.stat_lpf_hi    = Signal(8)
        # TVP自身が測った値。**キャプチャロジックに依存しない絶対値**なので、
        # 「H/Vが入力にロックしているか」の判定はこれで行う。OLEDには出ていたが
        # CONFIGに出ていなかったため、SOG運用の切り分けが手探りになっていた。
        self.stat_syncdet   = Signal(8)      # key 0x54 (reg 14h 同期検出ステータス)
        self.stat_lpf       = Signal(16)     # key 0x55 (reg 37h:38h Lines/Frame)
        self.stat_cpl       = Signal(16)     # key 0x56 (reg 39h:3Ah Clocks/Line)
        self.stat_lpf_msbs  = Signal(8)      # key 0x57 (reg 38h 生値。bit5=P/I detect)
        # ★S/PDIFの生存確認。**「マスクは立っているのにパケットが出ない」を
        #   切り分けるために要る。** 実機で S/PDIF だけ止まり、電源再投入まで
        #   戻らない事象が出たが、ボード側の状態を読む手段が無く、
        #   「デコーダがロックを失った」までしか絞れなかった(2026-09-06)。
        #     key 0x02 = 実測レート[Hz]。0 ならデコーダがサンプルを出していない
        #     key 0x03 = 送出FIFOの滞留。レートが出ているのに0なら詰まりは後段
        self.stat_spdif_level = Signal(16)   # key 0x03
        #     key 0x04 = UI長 ×16。**縮んだまま戻らないのが停止の機構**だった。
        #              45MHz なら 48kHz で約117、44.1kHz で約128
        #     key 0x05 = 立て直した回数。増えていれば罠に落ちて復帰している
        self.stat_spdif_ui     = Signal(12)  # key 0x04(×16 の固定小数点)
        self.stat_spdif_resync = Signal(16)  # key 0x05
        # ラインごとのHSYNC周期プローブ。key 0x27 で行を選び 0x28/0x29 で読む
        self.cfg_hs_probe_row = Signal(13)
        self.stat_hs_raw    = Signal(16)
        self.stat_hs_tvp    = Signal(16)
        audio_mask = Signal(3, reset=0b111)      # key 0x0001: 音声ソース有効マスク
        argus_reg = Signal(32)                   # target=1(ArgusX)の仮レジスタ
                                                 # (実機step4でI2C書き込みに置換)
        self.sync += [
            If(rx.valid,
                If(rx_first,
                    rx_widx.eq(1),
                    in_pkt.eq(rx_magic_ok),
                    rx_type.eq(rx.data[16:24]),
                    rx_flags.eq(rx.data[24:32]),
                    rx_src_ip.eq(rx.ip_address),
                    rx_src_port.eq(rx.src_port),
                ).Else(
                    If(rx_widx != 7, rx_widx.eq(rx_widx + 1)),
                    If(in_pkt & (rx_widx == 2), rx_mac_lo.eq(rx.data)),
                    If(in_pkt & (rx_widx == 3),
                        cfg_mac_ok.eq(
                            ((rx_mac_lo == my_mac_lo) &
                             (rx.data[0:16] == my_mac_hi)) |
                            ((rx_mac_lo == 0xFFFFFFFF) &
                             (rx.data[0:16] == 0xFFFF))),
                    ),
                    If(in_pkt & (rx_type == 5) & (rx_widx == 4),
                        cfg_target.eq(rx.data[0:8]),
                        cfg_op.eq(rx.data[8:16]),
                        cfg_key.eq(rx.data[16:32]),
                    ),
                ),
                If(rx.last, in_pkt.eq(0)),
            ),
            # SUBSCRIBE受理: ANNOUNCE返信先を更新、本購読なら送り先も切替
            If(sub_hit_any,
                ann_ip.eq(rx_src_ip),
                ann_port.eq(rx_src_port),
            ),
            If(sub_hit,
                sub_ip.eq(rx_src_ip),
                sub_port.eq(rx_src_port),
                sub_timer.eq(int(sys_clk_freq * sub_timeout) - 1),
            ).Elif(sub_timer != 0,
                sub_timer.eq(sub_timer - 1),
            ),
            # CONFIG受理: SET適用(w5=valueは最終ビートで直接読む) + 応答先ラッチ
            If(cfg_done,
                cfg_ip.eq(rx_src_ip),
                cfg_port.eq(rx_src_port),
                If(cfg_op == 0,
                    If((cfg_target == 0) & (cfg_key == 1),
                        audio_mask.eq(rx.data[:3]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x10),
                        self.cfg_vbp.eq(rx.data[:13]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x11),
                        self.cfg_hs_offset.eq(rx.data[:13]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x12),
                        self.cfg_pll_divide.eq(rx.data[:12]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x17),
                        self.cfg_video_bw.eq(rx.data[:4]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x30),
                        self.cfg_full_line.eq(rx.data[0]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x36),
                        self.cfg_pixfmt.eq(rx.data[:8]),
                    ),
                    *([If((cfg_target == 0) & (cfg_key == 0x31),
                          capture.cfg_span_probe_row.eq(rx.data[:13]),
                      ),
                       If((cfg_target == 0) & (cfg_key == 0x35),
                          capture.cfg_frame_skip.eq(rx.data[:4]),
                      ),
                       # 非黒判定の閾値(5bit = 8bitコードの上位5bit)。key 0x37。
                       # ★**暗い「絵の中身」まで黒と見なすと、その画素は範囲から
                       #   外れて送られず、受信側では 0 のままになる。**
                       #   既定2は code>=24 でないと非黒にならないので、
                       #   X68000 の32階調のうち下2段(code 8,16)が消えていた
                       #   (2026-09-03 に実測で確定)。映像源によって適切な値が
                       #   違うので実行時に振れるようにする。
                       If((cfg_target == 0) & (cfg_key == 0x37),
                          capture.cfg_black_th.eq(rx.data[:5]),
                      )] if cap_mode else []),
                    If((cfg_target == 0) & (cfg_key == 0x1F),
                        self.cfg_phase.eq(rx.data[:5]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x22),
                        self.cfg_sync_ctl.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x50),
                        self.cfg_sog_thresh.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x51),
                        self.cfg_sep_thresh.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x52),
                        self.cfg_precoast.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x5B),
                        self.cfg_sync_ctl2.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x5C),
                        self.cfg_sync_bypass.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x5D),
                        self.cfg_in_mux2.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x5E),
                        self.cfg_clamp_sel.eq(rx.data[:3]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x5F),
                        self.cfg_coarse_gain_gb.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x65),
                        self.cfg_coarse_off_g.eq(rx.data[:6]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x67),
                        self.cfg_coarse_gain_r.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x68),
                        self.cfg_coarse_off_r.eq(rx.data[:6]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x69),
                        self.cfg_in_mux1.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x64),
                        self.cfg_sog_vth.eq(rx.data[:16]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x66),
                        self.cfg_field_invert.eq(rx.data[0]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x6A),
                        self.cfg_no_raw_phase.eq(rx.data[0]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x53),
                        self.cfg_postcoast.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x27),
                        self.cfg_hs_probe_row.eq(rx.data[:13]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x18),
                        self.cfg_fine_clamp.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x19),
                        self.cfg_pll_ctl.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1A),
                        self.cfg_clamp_start.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1B),
                        self.cfg_clamp_width.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1C),
                        self.cfg_gain_b.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1D),
                        self.cfg_gain_g.eq(rx.data[:8]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1E),
                        self.cfg_gain_r.eq(rx.data[:8]),
                    ),
                    If(cfg_target == 1,
                        argus_reg.eq(rx.data),
                    ),
                ),
            ),
        ]
        # 応答値(SET/GETとも現在値を返す)。
        #
        # キーごとの If/Elif 連鎖で書くと段数ぶんの優先順位マルチプレクサになる。
        # キーが28個まで増えた時点で cfg_reply_val → hdr → sink.data の経路が伸び、
        # sys が要求45MHzを割った(シードによって38〜44MHz)。Case にすると
        # デコード+平坦なマルチプレクサになり、段数に依らない。
        #
        # さらに1段レジスタを挟む。応答は次のパケットで送るので1クロック遅れても
        # 影響しない(cfg_key はパケット受信時にラッチされ、送出まで固定されている)。
        cfg_reply_val = Signal(32)
        reply_mux = Signal(32)
        reply_cases = {
            1:    reply_mux.eq(audio_mask),
            0x10: reply_mux.eq(self.cfg_vbp),
            0x11: reply_mux.eq(self.cfg_hs_offset),
            0x12: reply_mux.eq(self.cfg_pll_divide),
            0x30: reply_mux.eq(self.cfg_full_line),
            0x36: reply_mux.eq(self.cfg_pixfmt),
            0x17: reply_mux.eq(self.cfg_video_bw),
            0x18: reply_mux.eq(self.cfg_fine_clamp),
            0x19: reply_mux.eq(self.cfg_pll_ctl),
            0x1A: reply_mux.eq(self.cfg_clamp_start),
            0x1B: reply_mux.eq(self.cfg_clamp_width),
            0x1C: reply_mux.eq(self.cfg_gain_b),
            0x1D: reply_mux.eq(self.cfg_gain_g),
            0x1E: reply_mux.eq(self.cfg_gain_r),
            0x1F: reply_mux.eq(self.cfg_phase),
            0x22: reply_mux.eq(self.cfg_sync_ctl),
            0x50: reply_mux.eq(self.cfg_sog_thresh),
            0x51: reply_mux.eq(self.cfg_sep_thresh),
            0x52: reply_mux.eq(self.cfg_precoast),
            0x53: reply_mux.eq(self.cfg_postcoast),
            0x54: reply_mux.eq(self.stat_syncdet),
            0x55: reply_mux.eq(self.stat_lpf),
            0x56: reply_mux.eq(self.stat_cpl),
            0x57: reply_mux.eq(self.stat_lpf_msbs),
            0x5B: reply_mux.eq(self.cfg_sync_ctl2),
            0x5C: reply_mux.eq(self.cfg_sync_bypass),
            0x5D: reply_mux.eq(self.cfg_in_mux2),
            0x5E: reply_mux.eq(self.cfg_clamp_sel),
            0x5F: reply_mux.eq(self.cfg_coarse_gain_gb),
            0x65: reply_mux.eq(self.cfg_coarse_off_g),
            0x67: reply_mux.eq(self.cfg_coarse_gain_r),
            0x68: reply_mux.eq(self.cfg_coarse_off_r),
            0x69: reply_mux.eq(self.cfg_in_mux1),
            0x64: reply_mux.eq(self.cfg_sog_vth),
            0x66: reply_mux.eq(self.cfg_field_invert),
            0x6A: reply_mux.eq(self.cfg_no_raw_phase),
        }
        # S/PDIF(source 2)の診断。定数レートのアナログ2系統と違い、ここだけ
        # gateware の実測値なので、0 かどうかで「生きているか」が分かる。
        if len(audio_srcs) > 2:
            _spdif_rate = audio_srcs[2][1]
            if not isinstance(_spdif_rate, int):
                reply_cases[0x02] = reply_mux.eq(_spdif_rate)
            reply_cases[0x03] = reply_mux.eq(self.stat_spdif_level)
            reply_cases[0x04] = reply_mux.eq(self.stat_spdif_ui)
            reply_cases[0x05] = reply_mux.eq(self.stat_spdif_resync)
        # 診断用の読み出し(書き込みは無視される読み取り専用)。
        # フィールド極性をどちらから取るべきか、ラインごとのHSYNC周期が揺れて
        # いないか等を実機で判断するための生データ。
        # ★dict.update で足すと既存キーを黙って上書きしてしまう(実際に 0x23〜0x26 の
        #   設定キーが診断キーに潰され、SETは効くのにGETで読めない状態になった)。
        #   下の _add_reply() 経由で足して、重複は必ずビルド時に落とす。
        def _add_reply(mapping):
            for k, stmt in mapping.items():
                assert k not in reply_cases, f"CONFIG key {k:#x} が重複している"
                reply_cases[k] = stmt

        if cap_mode:
            _add_reply({
                0x20: reply_mux.eq(capture.stat_vs_x),
                0x21: reply_mux.eq(capture.stat_fid),
                0x23: reply_mux.eq(capture.meas_vtotal),
                0x24: reply_mux.eq(capture.stat_vs_x_raw),
                0x25: reply_mux.eq(capture.stat_hs_len_raw),
                0x26: reply_mux.eq(self.stat_lpf_hi),       # 38h(bit5=P/I detect)
                0x27: reply_mux.eq(self.cfg_hs_probe_row),
                0x28: reply_mux.eq(self.stat_hs_raw),
                0x29: reply_mux.eq(self.stat_hs_tvp),
                # 生同期から測った絶対値(pll_divideに依存しない)
                0x2A: reply_mux.eq(capture.meas_fh_raw),
                0x2B: reply_mux.eq(capture.meas_fv_raw),
                0x2C: reply_mux.eq(capture.meas_lines_raw),
                # インターレース判定の内訳。bit0=生位相が交互 bit1=生同期OK
                # bit2=TVPのP/I検出 bit3=TVPのvtotalが2フィールド分
                0x2D: reply_mux.eq(capture.stat_il),
                # TVPのHSOUT/VSOUTをsysクロック基準で測った絶対値。SOG運用では
                # 0x2A〜0x2C が無信号で0になるので、ロック判定はこちらを見る
                0x58: reply_mux.eq(capture.meas_fh_tvp),
                0x59: reply_mux.eq(capture.meas_fv_tvp),
                0x5A: reply_mux.eq(capture.meas_lines_tvp),
                # SOGOUT(スライサ直後)から測ったコンポジット同期の内訳。
                # TVP経由の経路が半ライン位相を失うため、これが唯一の手掛かり
                0x60: reply_mux.eq(capture.stat_sog_hlen),    # 水平周期
                0x61: reply_mux.eq(capture.stat_sog_lowmax),  # 最長Low期間
                0x62: reply_mux.eq(capture.stat_sog_vphase),  # ★半ライン位相
                0x63: reply_mux.eq(capture.stat_sog_vlines),  # 垂直間隔[水平エッジ数]
                # 指定行の範囲(push時点、pixドメインの生値)と捨てた数
                0x31: reply_mux.eq(capture.cfg_span_probe_row),
                0x32: reply_mux.eq(capture.stat_span_probe),
                0x33: reply_mux.eq(capture.cap_drops),
                0x34: reply_mux.eq(capture.stat_pop_probe),
                0x35: reply_mux.eq(capture.cfg_frame_skip),
                0x37: reply_mux.eq(capture.cfg_black_th),
            })
        _add_reply({k: reply_mux.eq(sig) for k, sig in (extra_stats or {}).items()})
        self.comb += Case(cfg_key, reply_cases)
        self.sync += cfg_reply_val.eq(Mux(cfg_target == 1, argus_reg, reply_mux))

        # --- 送出要求(sticky; RXセットとFSMクリアの同時発生はセット優先) ---
        ann_pending  = Signal()
        mode_pending = Signal()
        line_pending = Signal()
        cfg_pending  = Signal()
        ann_clr  = Signal()
        mode_clr = Signal()
        line_clr = Signal()
        cfg_clr  = Signal()
        self.sync += [
            If(cfg_clr, cfg_pending.eq(0)),
            If(cfg_done, cfg_pending.eq(1)),
        ]

        ann_cnt  = Signal(max=max(int(sys_clk_freq * announce_period), 2))
        mode_cnt = Signal(max=max(int(sys_clk_freq * mode_period), 2))
        line_cnt = Signal(max=max(line_interval, 2))
        # ANNOUNCE/MODE タイマは両モード共通
        _timer = [
            If(ann_cnt == 0,
                ann_cnt.eq(int(sys_clk_freq * announce_period) - 1),
            ).Else(ann_cnt.eq(ann_cnt - 1)),
            If(~sub_valid,
                mode_cnt.eq(int(sys_clk_freq * mode_period) - 1),
            ).Else(
                If(mode_cnt == 0,
                    mode_cnt.eq(int(sys_clk_freq * mode_period) - 1),
                ).Else(mode_cnt.eq(mode_cnt - 1)),
            ),
        ]
        if not cap_mode:
            # テストパターン: 源(cap_*)を line_interval 毎に自走させる
            _timer += [
                If(~sub_valid,
                    line_cnt.eq(line_interval - 1),
                    cap_line.eq(0),
                ).Else(
                    If(line_cnt == 0,
                        line_cnt.eq(line_interval - 1),
                        If(cap_line == lines_per_unit - 1,
                            cap_line.eq(0),
                            cap_frame.eq(cap_frame + 1),
                            cap_ts_frame.eq(cap_ts_frame + unit_ticks),
                            cap_ts_line.eq(cap_ts_frame + unit_ticks),
                            *([field.eq(~field)] if interlace else []),
                        ).Else(
                            cap_line.eq(cap_line + 1),
                            cap_ts_line.eq(cap_ts_line + htotal),
                        ),
                    ).Else(line_cnt.eq(line_cnt - 1)),
                ),
            ]
        _timer += [
            # クリアより後に書く=セット優先
            If(ann_clr, ann_pending.eq(0)),
            If(mode_clr, mode_pending.eq(0)),
            If((ann_cnt == 0) | sub_hit_any, ann_pending.eq(1)),
            If((sub_valid & (mode_cnt == 0)) | sub_hit, mode_pending.eq(1)),
        ]
        if cap_mode:
            # capture: FIFOに実データが有る間だけ送出要求(組合せ)。
            # sticky レジスタにすると、送信中ずっと line_valid=1 で再セットされ、
            # 送信完了時の pop で FIFO が空になった直後に「空ヘッドの残留値」で
            # 余分なラインを送ってしまう(古いframe番号が混ざり、受信側で
            # frameが N↔N+1 を往復=実フレームの5倍のfpsに見える)。
            #
            # ただし「FIFOに有る」だけでは送り始められない。送る範囲(span_*)は
            # FIFO先頭から2段のレジスタなので、先頭が変わった直後の2サイクルは
            # まだ前のラインの範囲を指している。FSMは pop した次のサイクルには
            # 次のラインを掴めるので、FIFOに次が溜まっていると必ずそこを踏む。
            #
            # 実機での見え方: 全幅の明るい行は大きなパケットになって送出が遅れ、
            # その間にFIFOが溜まる。続く数行が立て続けに送られて前のラインの範囲を
            # 掴み、範囲がずれた分だけ絵が横にずれた(「明るい横線の直後3行」)。
            # 範囲の3つの値が別ラインから来ていた頃はさらに減算がアンダーフロー
            # して過大なパケットになり、揃えたあとは「前が全黒なら空範囲」で行が
            # まるごと消えた。どちらも根は同じで、範囲が確定する前に送り始めること。
            #
            # 範囲は row/ts と同じ瞬間に先頭からラッチするようにしたので、
            # 「範囲が追いつくまで待つ」仕掛けは要らなくなった(以前は2サイクル
            # 待たせていたが、待っても別エントリを指す可能性が残っていた)。
            self.comb += line_pending.eq(capture.line_valid)
        else:
            _timer.append(If(line_clr, line_pending.eq(0)))
            _timer.append(If(sub_valid & (line_cnt == 0), line_pending.eq(1)))
        self.sync += _timer

        # --- 画素源(1ワード=2ピクセル)。x はライン内ピクセル位置(常に偶数)---
        x = Signal(max=width + 2)          # +2: フラグ境界で最終+2しても飽和しない
        if not cap_mode:
            self.pix0 = pix0 = PatternPixel(width, height)
            self.pix1 = pix1 = PatternPixel(width, height)
            self.comb += [
                pix0.x.eq(x),          pix0.y.eq(row), pix0.frame.eq(frame),
                pix1.x.eq(x | 1),      pix1.y.eq(row), pix1.frame.eq(frame),
            ]
        else:
            # 送信中は FIFO 先頭(head)の面を読む。head は送信完了時に pop するまで
            # 上書きされない(FIFO満杯保護で書込ポインタが head 面へ進めない)ので安全。
            self.comb += capture.rd_face.eq(capture.line_face)

        # --- 音声バッファ(sysドメイン)。audio_nsamplesサンプル溜まったらパケット化 ---
        from litex.soc.interconnect import stream as _stream
        asrc = Signal(max=max(n_aud, 2))
        aud_pending = Signal(max(n_aud, 1))
        aud_pop = Signal(max(n_aud, 1))
        aud_data = Signal(32)
        aud_rate = Signal(32)
        aud_fifos = []
        aud_ts_arr = []
        for k, (ep, rate) in enumerate(audio_srcs):
            # buffered=True(同期読み出し)にするとBRAMへ載る。非バッファ版は
            # 非同期読み出しなのでBRAMにマップできず、480深さ×32bit×3系統が
            # 分散RAM(TRELLIS_DPR16X4)としてLUTを約3500個食っていた。
            # BRAMは9/56しか使っておらず余っている。
            fifo = _stream.SyncFIFO([("data", 32)], 2 * audio_nsamples,
                                    buffered=True)
            self.submodules += fifo
            ts_first = Signal(32)
            self.comb += [
                # 無効ソースは受けずに捨てる。FIFO満杯時も捨てる(音声は非再送)
                fifo.sink.valid.eq(ep.valid & audio_mask[k]),
                fifo.sink.data.eq(ep.data),
                ep.ready.eq(1),
                fifo.source.ready.eq(aud_pop[k] | ~audio_mask[k]),
                aud_pending[k].eq(audio_mask[k] & sub_valid &
                                  (fifo.level >= audio_nsamples)),
            ]
            # パケット先頭サンプルのタイムスタンプ(FIFOが空→非空になる瞬間に
            # ドットクロックカウンタ相当をラッチする近似。level>0のまま連続
            # パケット化される場合は最大1パケット分古くなり得る)
            self.sync += If(fifo.sink.valid & fifo.sink.ready &
                            (fifo.level == 0),
                            ts_first.eq(ts_line))
            if k == 2:
                self.comb += self.stat_spdif_level.eq(fifo.level)
            aud_fifos.append(fifo)
            aud_ts_arr.append(ts_first)
        if n_aud:
            self.comb += [
                Case(asrc, {k: aud_data.eq(aud_fifos[k].source.data)
                            for k in range(n_aud)}),
                Case(asrc, {k: aud_rate.eq(rate)
                            for k, (_, rate) in enumerate(audio_srcs)}),
            ]
        aud_ts = Array(aud_ts_arr) if n_aud else None

        # --- TX FSM ---
        ptype    = Signal(3)
        dst_ip   = Signal(32)
        dst_port = Signal(16)
        length   = Signal(16)
        nwords   = Signal(max=max_nwords + 1)
        word_idx = Signal(max=max_nwords)

        sink = udp_port.sink
        self.comb += [
            sink.ip_address.eq(dst_ip),
            sink.src_port.eq(udp_port_nr),
            sink.dst_port.eq(dst_port),
            sink.length.eq(length),
            sink.last_be.eq(0b1000),       # 全パケット長が4の倍数
        ]

        # ヘッダワードの組み立て(全てリトルエンディアン詰め)
        hdr = Signal(32)
        w1 = Signal(32)                    # 共通ヘッダ後半: frame(u16) | seq(u16)<<16
        # LINE flags: bit0=LAST_FRAGMENT(最終断片), bit1=FIELD_ODD
        line_flags = Signal(8)
        ts_frag = Signal(32)               # 断片先頭ピクセル時点のドットクロック
        cases = {
            _T_ANN: Case(word_idx, {i: hdr.eq(ann_words[i])
                                    for i in range(n_ann_words) if i != 1}),
            _T_MODE: Case(word_idx, {
                0: hdr.eq(0x52 | (1 << 16)),                       # type=MODE
                2: hdr.eq(Cat(C(mode_id, 8), pixfmt, mflags)),
                3: hdr.eq(Cat(C(width, 16), mode_htotal)),
                4: hdr.eq(Cat(mode_vactive, mode_vtotal)),
                5: hdr.eq(mode_dotclk),
                6: hdr.eq(mode_hfreq),                             # mHz
                7: hdr.eq(mode_vfreq),                             # mHz
            }),
            _T_LINE: Case(word_idx, {
                0: hdr.eq(Cat(C(0x52, 8), C(0, 8), C(0, 8), line_flags)),
                2: hdr.eq(Cat(row, C(0, 16 - len(row)), frag_off)),
                3: hdr.eq(Cat(frag_cnt, pixfmt, C(mode_id, 8))),
                4: hdr.eq(ts_frag),
            }),
            _T_CFG: Case(word_idx, {
                0: hdr.eq(0x52 | (5 << 16) | (1 << 24)),           # flags=REPLY
                2: hdr.eq(my_mac_lo),                              # 応答元=自MAC
                3: hdr.eq(my_mac_hi),
                4: hdr.eq(Cat(cfg_target, cfg_op, cfg_key)),
                5: hdr.eq(cfg_reply_val),
            }),
        }
        if n_aud:
            cases[_T_AUDIO] = Case(word_idx, {
                0: hdr.eq(0x52 | (2 << 16)),                       # type=AUDIO
                2: hdr.eq(Cat(asrc, C(0, 8 - len(asrc)), C(0, 8),   # source|format
                              C(audio_nsamples, 16))),
                3: hdr.eq(aud_rate),
                4: hdr.eq(aud_ts[asrc]),
            })
        # キャプチャ経路では実行時の検出結果、パターン生成側はビルド時の指定を使う
        # (パターン側に検出の仕組みは無い)
        if cap_mode:
            self.comb += mflags.eq(Cat(self.stat_interlaced, C(0, 15)))
        else:
            self.comb += mflags.eq(C(1 if interlace else 0, 16))
        # --- ラインバッファの生値(8bit×3)→ 伝送形式へ ---
        #
        # ★**変換はここで行う。** バッファは 8bit のまま持っているので、
        #   伝送形式を増やしてもキャプチャ側は変わらない。
        #   スロットは 32bit で byte0=R byte1=G byte2=B。
        #   ただし YC8 だけは書込側で「byte0=緑ch byte1=赤ch」に詰め替えてある
        #   (復調に8bitの生値が要るため)。
        if cap_mode:
            rdd = capture.rd_data
            line_pixdata = Signal(32)
            def _slot555(p):
                # 0RRRRRGGGGGBBBBB。Cat は第1引数がLSB側
                return Cat(p[19:24], p[11:16], p[3:8], C(0, 1))
            s0, s1 = rdd[0:32], rdd[32:64]
            w2b = Signal(32)          # 2B/px(RGB555 or YC8)の語
            w3b = Signal(32)          # 3B/px(RGB888)の語
            self.comb += [
                w2b.eq(Mux(pixfmt == PIXFMT_YC8,
                           Cat(s0[0:16], s1[0:16]),           # YC8 はそのまま
                           Cat(_slot555(s0), _slot555(s1)))),
                line_pixdata.eq(Mux(fmt888, w3b, w2b)),
            ]
            if True:
                # ★RGB888 のギアボックス: 4画素(2エントリ)→ 3語(12バイト)
                #
                #   entry e   → slot0(px0) slot1(px1)   ← ent_prev にラッチ
                #   entry e+1 → slot2(px2) slot3(px3)   ← rd_data
                #
                #   W0 = r0 g0 b0 r1    entry e のみ
                #   W1 = g1 b1 r2 g2    e(ラッチ)と e+1 の両方
                #   W2 = b2 r3 g3 b3    entry e+1 のみ
                #
                #   スロットは byte0=R byte1=G byte2=B。Cat は第1引数がLSB側で、
                #   32bit語はリトルエンディアンで線に出るので、Cat の順が
                #   そのままバイト順になる。
                self.comb += Case(gph, {
                    0: w3b.eq(Cat(rdd[0:24], rdd[32:40])),
                    1: w3b.eq(Cat(ent_prev[40:56], rdd[0:16])),
                    2: w3b.eq(Cat(rdd[16:24], rdd[32:56])),
                })
        else:
            line_pixdata = Cat(pix0.pix, pix1.pix)
        self.comb += [
            line_flags.eq(Cat(frag_last, field)),
            ts_frag.eq(ts_line + frag_off),
            If((ptype == _T_ANN) | (ptype == _T_CFG),
                w1.eq(Cat(C(0, 16), seq)),           # frame=0
            ).Else(
                w1.eq(Cat(frame, seq)),
            ),
            Case(ptype, cases),
            If(word_idx == 1, hdr.eq(w1)),
            If((ptype == _T_LINE) & (word_idx >= 5),
                sink.data.eq(line_pixdata),
            ).Elif((ptype == _T_AUDIO) & (word_idx >= 5),
                sink.data.eq(aud_data),
            ).Else(
                sink.data.eq(hdr),
            ),
        ]
        if cap_mode:
            # ラインバッファ読出(BRAM 1cyc遅延)。アドレスは「次サイクルのx」を先出し
            # → dat_r が常に現在の x に対応(バックプレッシャ時も保持されて正しい)。
            x_adv = Signal()
            self.comb += x_adv.eq(
                sink.valid & sink.ready & (ptype == _T_LINE) & (
                    ((word_idx >= 5) & (word_idx != nwords - 1)) |
                    ((word_idx == nwords - 1) & ~frag_last)))
            x_next = Signal(max=width + 2)
            self.comb += x_next.eq(x + Mux(x_adv, 2, 0))
            if True:
                # ★位相ごとに必要なエントリが違う。BRAM は1サイクル遅延なので
                #   「次サイクルに必要なエントリ」を先出しする(2B/px と同じ流儀)。
                #
                #     位相       0      1      2
                #     必要entry  base   base+1 base+1
                #     前進時の次 base+1 base+1 base+2(=次群のbase)
                #
                #   バックプレッシャで前進しないときは「今の位相が必要な方」を保持。
                ent_base = Signal(max=max(width // 2 + 3, 2))
                self.comb += ent_base.eq(x[1:])
                self.comb += capture.rd_word.eq(
                    Mux(fmt888,
                        ent_base + Mux(x_adv,
                                       Mux(gph == 2, 2, 1),
                                       Mux(gph == 0, 0, 1)),
                        x_next[1:]))          # 2B/px: entry = x_next/2
                # 位相0から前進する瞬間の rd_data が entry base。次サイクルには
                # base+1 に変わるので、ここでラッチしておく
                self.sync += If(fmt888 & x_adv & (gph == 0),
                                ent_prev.eq(capture.rd_data))
            # ts はキャプチャのDATACLK自走カウンタ(LINE開始時にラッチ)。定数
            # (htotal×frame番号)からの算出だと仮定したモードでしか合わないが、
            # 実カウンタなら常に正確で音声との同期もモードに依らず成立する。
        # AUDIOペイロード送出時のFIFOポップ(最終ワードも含めword>=5で1ワード=1ポップ)
        for k in range(n_aud):
            self.comb += aud_pop[k].eq(
                (ptype == _T_AUDIO) & (asrc == k) & (word_idx >= 5) &
                sink.valid & sink.ready)

        idle_if = If(ann_pending,
            ann_clr.eq(1),
            NextValue(ptype, _T_ANN),
            NextValue(dst_ip, ann_ip),
            NextValue(dst_port, ann_port),
            NextValue(length, len(ann_payload)),
            NextValue(nwords, n_ann_words),
            NextState("SEND"),
        ).Elif(cfg_pending,
            cfg_clr.eq(1),
            NextValue(ptype, _T_CFG),
            NextValue(dst_ip, cfg_ip),
            NextValue(dst_port, cfg_port),
            NextValue(length, 24),
            NextValue(nwords, 6),
            NextState("SEND"),
        ).Elif(mode_pending & sub_valid,
            mode_clr.eq(1),
            NextValue(ptype, _T_MODE),
            NextValue(dst_ip, sub_ip),
            NextValue(dst_port, sub_port),
            NextValue(length, 32),
            NextValue(nwords, 8),
            NextState("SEND"),
        )
        # 送る範囲(ライン内の「黒でない範囲」)を決める。
        #
        # キャプチャはラインの頭から取り込む(hs_offset=0)。そのままライン全体を
        # 送ると帯域が1.4倍になって入らないので、中身のある範囲だけを送り、位置は
        # offset_px で伝える。受信側は offset_px をライン内の絶対位置として扱うので、
        # 「どこから取り込むか」という調整項目そのものが無くなる。
        if cap_mode:
            span_lo = Signal(16)
            span_hi = Signal(16)
            span_n = Signal(16)
            # 範囲は row/ts/frame と「同じ瞬間に」FIFO先頭から取る。
            #
            # 以前は先頭を毎サイクル追いかける2段レジスタから取っていた。row/ts は
            # 送出開始時の直接スナップショットなので、両者が同じエントリを指す保証が
            # なく、実機では範囲だけが fifo_depth ライン遅れて乗った(depth 4 → 4
            # ライン、8 → 8ライン。key 0x32/0x34 の窓で「pixドメインもFIFO出口も
            # 正しいのに電線だけ違う」ことを確認済み)。内容のある行が空で送られ、
            # 行が欠けて見えていた。
            #
            # 段を分けるのは組合せ経路の長さのため(組合せで作るとFIFO先頭から
            # 加算・比較・muxを経てFSMのレジスタ入力まで伸び、sysが45MHzを割った。
            # 実測 48.1 → 42.4MHz)。ただし段を分けるなら「同じエントリから出た値
            # どうしを揃える」ことが要る。ここでは
            #   IDLE  : 先頭から span_lo/span_hi を1段でラッチ(row/tsと同時)
            #   LPREP : そのラッチ値だけから派生値(断片数・長さ)を作る
            # という2段にしてある。1ライン当たり1サイクル増えるだけ(31.7µsに対し20ns)。
            lo_c = Signal(16)
            hi_c = Signal(16)
            # どちらもライン内の絶対位置[画素]にする。hs_offset を足しておけば、
            # 受信側は offset_px をそのままライン内の位置として使える
            # (hs_offset が描画位置に影響しなくなる = ドットクロック再生と描画の分離)。
            #
            # 1ラインまるごと送るか。key 0x30(診断)のほかに、**YC8では強制**する。
            # YC8はブランキングもカラーバーストもライン全体が「中身」なので、
            # 「黒でない範囲」という概念が成立しない(そもそも capture 側の判定式は
            # RGB555のビット割りを前提にしているので結果が無意味になる)。
            # 設定漏れで壊れるより、形式から決まる方が事故が少ない。
            send_full = Signal()
            self.comb += send_full.eq(self.cfg_full_line |
                                      (self.cfg_pixfmt == PIXFMT_YC8))
            self.comb += [
                If(send_full,
                    # 範囲を無視してラインの先頭から htotal ぶん送る。
                    # (lo_c も hs_offset に戻すこと。line_first のままだと
                    #  「範囲を無視する」はずが左端だけ内容依存で動いてしまう)
                    lo_c.eq(self.cfg_hs_offset),
                    hi_c.eq(self.cfg_pll_divide),
                # line_last は内包。全黒の行では line_first > line_last になるので
                # その場合は空(送るピクセル0)にする。行自体は送る(落とすと受信側が
                # 「黒い行」と「届かなかった行」を区別できない)。
                ).Elif(capture.line_last >= capture.line_first,
                    lo_c.eq(Cat(C(0, 1), capture.line_first) + self.cfg_hs_offset),
                    hi_c.eq(Cat(C(0, 1), capture.line_last) + 2
                            + self.cfg_hs_offset),
                ).Else(
                    lo_c.eq(Cat(C(0, 1), capture.line_first) + self.cfg_hs_offset),
                    hi_c.eq(Cat(C(0, 1), capture.line_first) + self.cfg_hs_offset),
                ),
            ]
            # 全黒行では lo_c が窓の外(line_first が初期値 entries-1 のまま)を
            # 指すので、空範囲なら位置0に寄せる。特異値を残すと、他がわずかに
            # 狂ったときに一気に壊れる形で効く(実際アンダーフローの引き金だった)。
            lo_use = Signal(16)
            hi_use = Signal(16)
            # ★**両端を PX_ALIGN に整列させる**(ワード整列のため)。
            #   下端は切り下げ、上端は切り上げてから **width でクランプ**する。
            #   width 自体が PX_ALIGN の倍数なので、クランプしても差は倍数のまま。
            #   切り上げだけにするとバッファの端を超えて読み、面が折り返して
            #   別の行の内容が出る(RGB888 の試験でこの形で出た)。
            lo_a = Signal(16)
            hi_a = Signal(16)
            hi_up = Signal(16)
            ALB = 1 if PX_ALIGN == 2 else 2      # 整列させるビット数
            self.comb += [
                lo_a.eq(Cat(C(0, ALB), lo_c[ALB:])),
                hi_up.eq(hi_c + (PX_ALIGN - 1)),
                hi_a.eq(Mux(Cat(C(0, ALB), hi_up[ALB:]) > width,
                            width, Cat(C(0, ALB), hi_up[ALB:]))),
            ]
            self.comb += If(hi_c == lo_c,
                lo_use.eq(0), hi_use.eq(0),
            ).Else(
                lo_use.eq(lo_a), hi_use.eq(hi_a),
            )
            # IDLE でのラッチ。row/ts/frame と同じ瞬間の先頭の値
            line_span = [
                NextValue(span_lo, lo_use),
                NextValue(span_hi, hi_use),
            ]
            # LPREP で作る派生値。span_lo/span_hi(同一エントリ由来)だけを使う。
            # 経路はレジスタ→減算→比較→mux→レジスタで、以前 line_span が
            # span_lo/span_n から作っていたのと同じ深さ。
            # span_lo/span_hi は既に PX_ALIGN へ整列済み(上の lo_a/hi_a)なので、
            # 差もそのまま倍数になる。FRAG_PX も倍数なので断片も整列する。
            d_al = Signal(16)
            self.comb += d_al.eq(span_hi - span_lo)
            n_c = Signal(16)
            self.comb += n_c.eq(Mux(d_al > FRAG_PX, FRAG_PX, d_al))
            line_prep = [
                NextValue(span_n, n_c),
                NextValue(x, span_lo),
                NextValue(frag_off, span_lo),
                NextValue(frag_cnt, n_c),
                NextValue(px_end, span_lo + d_al),
                NextValue(frag_last, n_c >= d_al),
                NextValue(length, _len_expr(n_c)),
                NextValue(nwords, _nw_expr(n_c)),
                NextValue(gph, 0),
            ]
        else:
            line_prep = []
            line_span = [
                NextValue(x, 0),
                NextValue(frag_off, 0),
                NextValue(frag_cnt, frags[0][1]),
                NextValue(px_end, width),
                NextValue(frag_last, n_frags == 1),
                NextValue(length, frag_len_arr[0]),
                NextValue(nwords, frag_nwords_arr[0]),
            ]

        # LINE送出開始時に「送るライン」を源からスナップショット(送信中は固定)。
        if cap_mode:
            # 行/フレーム/フィールドを固定(面は head を直読)。pop は送信完了時。
            line_snap = [
                NextValue(cap_row, capture.line_row),
                NextValue(frame, capture.line_frame),
                NextValue(field, capture.line_field),
                NextValue(ts_line, capture.line_ts),   # 実DATACLKカウンタ
            ]
        else:
            line_snap = [
                NextValue(line, cap_line),
                NextValue(frame, cap_frame),
                NextValue(ts_frame, cap_ts_frame),
                NextValue(ts_line, cap_ts_line),
            ]
        idle_if = idle_if.Elif(line_pending & sub_valid,
            line_clr.eq(1),
            NextValue(ptype, _T_LINE),
            NextValue(dst_ip, sub_ip),
            NextValue(dst_port, sub_port),
            NextValue(frag_idx, 0),
            *line_span,
            *line_snap,
            NextState("LPREP" if cap_mode else "SEND"),
        )
        for k in range(n_aud):
            idle_if = idle_if.Elif(aud_pending[k],
                NextValue(ptype, _T_AUDIO),
                NextValue(asrc, k),
                NextValue(dst_ip, sub_ip),
                NextValue(dst_port, sub_port),
                NextValue(length, 20 + 4 * audio_nsamples),
                NextValue(nwords, aud_nwords),
                NextState("SEND"),
            )

        # 次断片のパラメータ(組合せ)。frag_off/frag_cnt から積算で出すので
        # 乗算は要らない
        # こちらもレジスタに落とす。frag_off/frag_cnt が変わるのは断片境界だけで、
        # 次の境界までに何十サイクルもあるので1クロック遅れは問題にならない。
        nx_off = Signal(16)
        nx_cnt = Signal(16)
        nx_rem = Signal(16)
        self.sync += [
            nx_off.eq(frag_off + frag_cnt),
            nx_rem.eq(px_end - (frag_off + frag_cnt)),
            If(px_end - (frag_off + frag_cnt) > FRAG_PX,
                nx_cnt.eq(FRAG_PX),
            ).Else(
                nx_cnt.eq(px_end - (frag_off + frag_cnt)),
            ),
        ]

        self.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            NextValue(word_idx, 0),
            idle_if,
        )
        if cap_mode:
            # ラッチした span_lo/span_hi だけから派生値を作る1サイクル。
            # ここを挟むことで「行番号・ts と範囲が同じFIFOエントリ由来」に
            # なることが構造的に保証される。
            fsm.act("LPREP", *line_prep, NextState("SEND"))
        fsm.act("SEND",
            sink.valid.eq(1),
            sink.last.eq(word_idx == nwords - 1),
            If(sink.ready,
                If(word_idx == nwords - 1,
                    NextValue(seq, seq + 1),
                    NextValue(word_idx, 0),
                    If((ptype == _T_LINE) & ~frag_last,
                        # 同一ラインの次断片へ(宛先/種別は不変)。最終ワードも
                        # ピクセルペアなのでxを進めておく(次断片先頭=offset+count)
                        *_px_adv(),
                        NextValue(frag_idx, frag_idx + 1),
                        NextValue(frag_off, nx_off),
                        NextValue(frag_cnt, nx_cnt),
                        NextValue(frag_last, nx_off + nx_cnt >= px_end),
                        NextValue(length, _len_expr(nx_cnt)),
                        NextValue(nwords, _nw_expr(nx_cnt)),
                    ).Else(
                        # ライン(全断片)完了。capture時はここで FIFO を pop(head前進)。
                        # pattern時は源(cap_*)が自走するのでIDLEへ戻るだけ。
                        *([If(ptype == _T_LINE, capture.line_ack.eq(1))]
                          if cap_mode else []),
                        NextState("IDLE"),
                    ),
                ).Else(
                    NextValue(word_idx, word_idx + 1),
                    If((ptype == _T_LINE) & (word_idx >= 5),
                        *_px_adv(),
                    ),
                ),
            ),
        )


class _CRG(LiteXModule):
    def __init__(self, platform, sys_clk_freq):
        self.cd_sys = ClockDomain()
        clk25 = platform.request("clk25")
        self.pll = pll = ECP5PLL()
        pll.register_clkin(clk25, 25e6)
        pll.create_clkout(self.cd_sys, sys_clk_freq)


# TVP7002 映像出力(RGB[9:2]=RGB888, DATACLK, HSOUT/VSOUT/FIDOUT)。
# ボール割当は hardware/adc-frontend/main.ato と一致(sodimm io_* 名=ボール名)。
# r/g/b の Pins は bit0(=TVP r2/g2/b2, LSB)→bit7(=r9/g9/b9, MSB)の順。
_capture_io = [
    ("tvp_capture", 0,
        Subsignal("dataclk", Pins("E2")),                        # PCLKC7_0(P3唯一のクロック)
        Subsignal("r", Pins("T18 R18 R17 P17 M17 T17 U18 U17")), # R[9:2]
        Subsignal("g", Pins("P18 N17 N18 M18 L20 L18 K20 J20")), # G[9:2]
        Subsignal("b", Pins("F20 D20 B20 B19 B18 A19 C17 A18")), # B[9:2]
        Subsignal("hs",  Pins("B4")),                            # HSOUT
        Subsignal("vs",  Pins("C3")),                            # VSOUT
        Subsignal("fid", Pins("E3")),                            # FIDOUT(未使用)
        # SOGOUT: 同期スライサの出力そのもの。TVPの同期処理ブロック(H-PLLによる
        # HSOUT再生成、VSOUTの整数ラインへの丸め)を**通る前**の信号なので、
        # インターレースの半ライン位相が残っている。
        # C-SYNCしか出ない機種(MSX等)には生HSYNC/生VSYNCが無く、hs_raw/vs_raw が
        # 使えない。実測でTVP経由の経路は半ラインを失っており(LINEパケットのts を
        # 12フィールド見て位相が全て0、フィールド間隔も1368×262と1368×263の整数で
        # 262.5にならない)、SOGOUTが唯一の手掛かりになる。
        # 使うには reg 17h bit1(SOG En)=0 が必要(retrocastx_i2c.py で 0x00 を書く)。
        Subsignal("sogout", Pins("C4")),                          # P4-4 pin131
        # TVPを通さない生の同期。TVPのVSOUTはインターレース入力の半ライン位相を
        # 保たない(24kHz 1024x848 で実測: 生信号はオシロでVSYNCトリガごとにHSYNCが
        # 半ラインずれるのに、VSOUTは931ラインに1パルスしか出さず位相も完全固定。
        # 同期制御レジスタ0x0Eを256通り振っても現れなかった)。半ライン位相が読めれば
        # インターレースの判定と第2フィールドの位置決めが測定で決まる。
        # 既存の SN74LVC2G17 の出力から分岐しているので新規部品は無い。
        Subsignal("hs_raw", Pins("F2")),                          # P4-6 pin155 生HSYNC
        Subsignal("vs_raw", Pins("E1")),                          # P4-5 pin153 生VSYNC(RC遅延後)
        IOStandard("LVCMOS33")),
]

# hardware/adc-frontend のピン割当。ADC側の信号はP3に集約(139-147)、
# S/PDIFとXO入力のみP4(128/130)。
# XOは専用クロックピン F1=PCLKC6_1 で受けて aud ドメインを駆動し、その clock を
# D2(P3 147)から出して両PCM1808のSCKIへ配る。P3のクロック対応ボールはE2=151のみで
# DATACLKが使用中のため、XOを直接P3で受けることはできない。
#
# I2Sの出力(mclk_out/bck/lrck)は SLEWRATE=SLOW + DRIVE=4mA にする。試作配線では
# これらのジャンパがアナログRGBを直交して横切っており、特に12.288MHzのMCLKの
# 速いエッジが容量結合して映像に点状ノイズとして現れる(RGB555の1LSBは約22mVなので
# 数mV〜数十mVの結合で見える)。最大12.288MHzと低速なのでエッジを鈍らせても
# 波形品質には余裕があり、放射・結合を確実に減らせる。
_audio_io = [
    ("audio", 0,
        Subsignal("mclk",      Pins("F1")),   # ← 12.288MHz XO(PCLKC6_1, P4 pin130)
        Subsignal("mclk_out",  Pins("D2"),    # → PCM1808×2 SCKI(P3 pin147, バッファ)
                  Misc("SLEWRATE=SLOW"), Misc("DRIVE=4")),
        Subsignal("bck",       Pins("B1"),    # → PCM1808×2(64fs, P3 pin143)
                  Misc("SLEWRATE=SLOW"), Misc("DRIVE=4")),
        Subsignal("lrck",      Pins("C1"),    # → PCM1808×2(fs, P3 pin145)
                  Misc("SLEWRATE=SLOW"), Misc("DRIVE=4")),
        Subsignal("dout_dsub", Pins("C2")),   # ← U1(D-SUB15音声, P3 pin141)
        Subsignal("dout_line", Pins("A3")),   # ← U2(LINE入力, P3 pin139)
        Subsignal("spdif",     Pins("E4")),   # ← TOSLINK受信モジュール(P4 pin128)
        IOStandard("LVCMOS33")),
]


class RetroCastXStream(SoCMini):
    # sys 45MHz: 45M×32bit=180MB/s でGbE線速(125MB/s)に対し十分。
    # 50MHzは音声パス追加後にタイミングが閉じなくなったため下げた
    # (S/PDIFのUI分解能も45MHzで7.3サイクル/UI@48kHzと十分)
    def __init__(self, revision="7.0", sys_clk_freq=int(45e6), capture=True,
                 green_input=3, red_input=3, blue_input=3,
                 pll_divide=1104, hs_offset=0, vs_row_at_sync=525,
                 measure=True, mclk_out=True, auto_vtotal=True, vbp=0,
                 interlace_cap=0, eth_phy=1):
        from litex_boards.platforms import colorlight_i5
        from liteeth.phy.ecp5rgmii import LiteEthPHYRGMII
        from retrocastx_net import RetroCastXUDPIPCore

        platform = colorlight_i5.Platform(board="i5", revision=revision,
                                          toolchain="trellis")
        SoCMini.__init__(self, platform, sys_clk_freq,
                         ident="RetroCastX test-pattern streamer (step2)")
        self.crg = _CRG(platform, sys_clk_freq)

        # Ethernet PHY (RGMII) + hardware UDP/IP core
        #
        # ★**litex の index とコネクタ表記 ETH1/ETH2 は入れ替わっている**。
        #   litex_boards の colorlight_i5.py に明記されている:
        #     "The order of the two PHYs is swapped with the naming of the
        #      connectors on the board so to match with the configuration of
        #      their PHYA[0] pins."
        #   したがって:
        #     eth 0 (G1/G2)   = コネクタ ETH2 = SO-DIMM eth2_* = v0.9.0 の J11
        #     eth 1 (U19/U20) = コネクタ ETH1 = SO-DIMM eth1_* = v0.9.0 の J12
        #
        #   以前ここには「eth 1 = ETH2側PHY」と書いてあったが**逆だった**
        #   (2026-09-02 修正)。手組み試作機が動いていたのは index=1 = ETH1側で、
        #   v0.9.0 でそこに繋がるのは J12。J11 で使うには index=0 にする。
        #
        #   既定は 1 (=J12) のまま。2026-09-02 の v0.9.0 実機確認で、J12 の
        #   はんだ不良を直したところ index=1 のまま ping/発見/ストリームが
        #   全て通った(26フレーム, lost_pkts=0)。index=0 は seed 3 で
        #   eth_rx 122.55MHz(制約125)で閉じないため、J11 へ移すならシード探索が要る。
        self.ethphy = LiteEthPHYRGMII(
            clock_pads = platform.request("eth_clocks", eth_phy),
            pads       = platform.request("eth", eth_phy),
            tx_delay   = 0e-9)
        # LiteEthUDPIPCore ではなく自前のコア。中身は同じ構成(MAC + ARP + IP +
        # ICMP + UDP)で、受信パケットから相手のMACを学習する層を挟んである。
        # ARPに応答しない相手(別サブネットのWindows)へ返せるようにするため。
        # 理由と挟む位置は retrocastx_net.py の冒頭を参照。
        self.ethcore = RetroCastXUDPIPCore(
            phy         = self.ethphy,
            mac_address = MAC_ADDRESS,
            ip_address  = FPGA_IP,
            clk_freq    = sys_clk_freq,
            udp_port_nr = UDP_PORT,
            dw          = 32,
            # 幅変換・CRC等をsysドメインで実行(eth_rx/txドメインは8bit@125MHzの軽い経路のみ
            # にする。これ無しだとeth_rxの125MHzタイミングが閉じない: 実測93MHz)
            with_sys_datapath = True,
            # ★CDC FIFO の深さは**既定の32のまま**にしてある。
            #
            #   eth_rx のクリティカルパスは LiteEth の CDC 非同期FIFO の中で、
            #     グレイポインタFF → 残量比較LUT列 → 読ポインタ → RAM読出 → 出力FF
            #   と1サイクルに繋がっている(実測 8.13ns、うち配線 6.18ns = 76%)。
            #   **配線律速なので配置シード次第で 122.6〜143.7 MHz と ±9% 振れる。**
            #
            #   2026-09-03 に3つの手を実測して、いずれも効かないことを確認した。
            #   同じ道を再び掘らないように結果を残す:
            #
            #     ・目標周波数を上げる(125→145MHz)  … **完全に無効**。
            #       制約は効いている(ログに 145.01 MHz と出る)のに、5シード全部が
            #       125MHz制約時とビット単位で同一の結果。nextpnr の配置はこの
            #       目標値に反応しない。
            #     ・深さ512にしてBRAM化          … **差し引きゼロ**。RAMはパスから
            #       消えるが、ポインタが10bitになり readable の比較が5段のLUT列に
            #       なって新しい律速になる。床 122.6→116.0 / 天井 143.7→135.0 と
            #       むしろ悪化。通過率は 9/12 で同じ。
            #     ・--router router2              … **大幅悪化**。9/12 → 0/12
            #       (102〜124MHz)。既定の router1 の方が良い。
            #
            #   結論: ばらつきは消せない。代わりに tools/build_closed.sh で
            #   **タイミングが閉じるまでシードを自動で振る**ようにしてある。
            )

        # 音声: 12.288MHz XO入力の "aud" ドメインでI2Sキャプチャ、
        # S/PDIFはsysドメインでオーバーサンプリング復号
        from migen.genlib.resetsync import AsyncResetSynchronizer
        from retrocastx_audio import I2sCapture, SpdifDecoder
        platform.add_extension(_audio_io)
        audio_pads = platform.request("audio")
        self.cd_aud = ClockDomain()
        self.comb += self.cd_aud.clk.eq(audio_pads.mclk)
        self.specials += AsyncResetSynchronizer(self.cd_aud, ResetSignal("sys"))
        platform.add_period_constraint(audio_pads.mclk, 1e9 / 12.288e6)
        # XO入力をそのまま出し直して両PCM1808のSCKIへ配る(P3 147)。ADCはスレーブで
        # SCKIは過サンプリング用、実データはFPGAが出すBCK/LRCKに従うので、出力段で
        # 数nsの遅延が付いても問題にならない。
        # mclk_out=False なら0固定にする: 試作配線ではこの12.288MHzがアナログRGBを
        # 横切っており、実測でノイズ源であることが確認できたため、切り分け用に
        # 止められるようにしてある(音声は動かなくなる)。
        if mclk_out:
            self.comb += audio_pads.mclk_out.eq(ClockSignal("aud"))
        else:
            self.comb += audio_pads.mclk_out.eq(0)
        self.i2s = I2sCapture(audio_pads.bck, audio_pads.lrck,
                              [audio_pads.dout_dsub, audio_pads.dout_line])
        self.spdif = SpdifDecoder(audio_pads.spdif, sys_clk_freq)

        # --- ステータス表示 + TVP7002 I2C (共有バス) ---
        # 1本のI2Cマスタで TVP(0x5C)のRESETB解除/レジスタR/W と OLED(0x3C)描画を時分割。
        # SDA=U16, SCL=K18, RESETB(TVP)=C18。TVP応答/レジスタ値を OLED にライブ表示。
        from retrocastx_i2c import StatusDisplay
        _i2c_io = [("tvp_oled_i2c", 0,
                    Subsignal("scl",    Pins("K18")),
                    Subsignal("sda",    Pins("U16")),
                    Subsignal("resetb", Pins("C18")),
                    IOStandard("LVCMOS33"), Misc("PULLMODE=UP"))]
        platform.add_extension(_i2c_io)
        # I2Cは100kHz(長い手配線+弱プルアップでも読出しの取りこぼしを避ける)
        self.status = StatusDisplay(platform.request("tvp_oled_i2c"), sys_clk_freq,
                                    i2c_freq=100e3, green_input=green_input,
                                    red_input=red_input, blue_input=blue_input,
                                    pll_divide=pll_divide)

        # --- TVP7002 映像キャプチャ(cd_pix=DATACLK)---
        capture_obj = None
        if capture:
            from retrocastx_capture import TvpCapture
            platform.add_extension(_capture_io)
            cap_pads = platform.request("tvp_capture")
            self.cd_pix = ClockDomain()
            self.comb += self.cd_pix.clk.eq(cap_pads.dataclk)
            self.specials += AsyncResetSynchronizer(self.cd_pix,
                                                    ResetSignal("sys"))
            # DATACLK は 31.5kHz×分周比。X68000 各モードの最大~70MHz を見込み75MHz制約
            platform.add_period_constraint(cap_pads.dataclk, 1e9 / 75e6)
            # HSOUT/VSOUT は active-low 前提(SYNC=0x91: IHSPD=0/VSPD=0)。画がずれる
            # 場合は hs/vs_active_low と hs/vs_offset で調整。
            # vtotal=568(TVP実測 Lines/Frame): HSYNC行数の自走カウンタでフレーム境界を
            #   決めるのでVSOUTにノイズが乗っても崩れない。
            # vs_min_rows=497: フレーム末尾付近のVSYNCだけを受理して row=0 に再整列
            #   → 垂直位置が本物のVSYNCへ自動で合う(自走のみだと開始位相が任意で、
            #     実機より上にずれて見えていた)。フレーム途中の偽VSYNCは無視。
            # hs_offset: 水平バックポーチをスキップ(右寄りの内容を中央へ)。実機調整値。
            # vs_offset: 垂直バックポーチ分。VSYNC整列後の残りずれを詰める。
            # vbp=0(既定): キャプチャ窓はVSYNC直後から始め、vtotal 行すべてを
            #   取り込む。水平を hs_offset=0 でラインの頭から取り込むのと同じ方針で、
            #   「どこから取り込むか」を調整項目にしない。垂直位置は管面のV位置で決まる。
            #   以前は 43(31kHzの vtotal 568 − 有効512 = ブランキング56行の上側)に
            #   していたが、これは31kHz専用の値で、vbp はモードに追従しないため
            #   他のモードでは画面上部が切れた。増える43行はブランキング=全黒なので
            #   count_px=0 の20バイトパケットになり、帯域はほとんど増えない。
            #   vs_row_at_sync は auto_vtotal が vbp から毎サイクル導くので、
            #   vbp=0 のときは 0(=VSYNCが窓の先頭)に折り返される。
            # hs_offset: 水平バックポーチ[DATACLK]。pll_divide を変えるとサンプルレートが
            #   変わるので、この値も同じ比率で見直す必要がある。RGB入力を1段レジスタで
            #   受けるようにした分データが1サイクル遅れるので、151→152 に補正している。
            # width はラインを丸ごと保持できる大きさにする(15kHz 1216 / 24kHz 1408 /
            # 31kHz 1104 の htotal がすべて入る)。hs_offset=0 でラインの頭から
            # 取り込み、送るのは中身のある範囲だけにするので、hs_offset は調整
            # 項目でなくなる。height は行位置が半ライン単位のスロットになった分
            # 2倍必要(BRAMはラインバッファ nface 本ぶんだけなので縦は費用ゼロ)。
            # nface/fifo_depth は既定(8/4)。行欠けを追う過程で 16/8 に振って
            # 「範囲の遅れが fifo_depth に比例する」ことを確認したが、原因は
            # 送信側のヘッダ組み立てだったので大きくする理由は無い。既定でも
            # 通常モード・全ライン送信モードのどちらも cap_drops(key 0x33)は0
            # (実測: 送出は最悪でもライン周期の1/3しか使わない)。
            self.capture = TvpCapture(cap_pads, width=2048, height=2048,
                                      hs_active_low=True, vs_active_low=True,
                                      vtotal=568, vs_min_rows=497,
                                      vs_row_at_sync=vs_row_at_sync,
                                      # 実測vtotalに追従(モードが変わると
                                      # ラップ点が合わず絵が縦に繰り返すため)
                                      auto_vtotal=auto_vtotal, vbp=vbp,
                                      interlace=interlace_cap,
                                      hs_offset=hs_offset, vs_offset=0,
                                      hs_total=pll_divide or 1650,
                                      sys_clk_freq=sys_clk_freq,
                                      measure=measure)
            capture_obj = self.capture

        udp_port = self.ethcore.udp.crossbar.get_port(UDP_PORT, dw=32)
        # ARP学習の診断(key 0x40..0x45)。実機で「学習できていないのか」
        # 「学習した表で答えているのか」を切り分けるために読めるようにする。
        # 0x40 が増えない = 受信から学習できていない、
        # 0x41 が増えて 0x42 が止まっている = 学習した表だけで解決できている。
        learner = self.ethcore.arp_learner
        self.streamer = RetroCastXStreamer(
            udp_port, sys_clk_freq, width=2048, height=2048, fps=60.0,
            audio_sources=[(self.i2s.sources[0], 48000),
                           (self.i2s.sources[1], 48000),
                           (self.spdif.source, self.spdif.rate_hz)],
            capture=capture_obj,
            cfg_vbp=vbp, cfg_hs_offset=hs_offset, cfg_pll_divide=pll_divide,
            # reg 19h の初期値。retrocastx_i2c.py の MUX1 と同じ式にする
            # (SOG=_3固定 + R/G/Bはビルド引数)。実行時は key 0x69 で上書き。
            cfg_in_mux1=((2 << 6) | ((red_input - 1) << 4) |
                         ((green_input - 1) << 2) | (blue_input - 1)),
            extra_stats={
                0x40: learner.learn_count,
                0x41: learner.hit_count,
                0x42: learner.miss_count,
                0x43: learner.last_ip,
                0x44: learner.last_mac[:32],
                0x45: learner.last_mac[32:],
            },
            )

        if capture:
            # CONFIGで書き換わる画枠パラメータをキャプチャ/TVPへ配る。
            # 末尾クリアの開始entryは (1ラインのサンプル数 - 水平オフセット)/2。
            # pll_divide や hs_offset を変えると必要範囲も変わるので実行時に算出する。
            # pll_divide(=1ライン当たりDATACLK数)を安全な範囲に制限する。
            # これを大きくするとDATACLKが上がり、pixドメインのタイミング制約
            # (75MHz)を超えると動作が壊れる。実機で pll_divide=4095
            # (24kHz入力でDATACLK 101MHz)にすると14秒でボードごとハングした
            # (Ethernetも応答しなくなる)。2304 では60秒間安定。
            # 2304なら 31.5kHz でも 72.6MHz に収まり、実用上必要な最大値
            # (15kHzの÷2候補 2176)より大きいので不足しない。
            # CONFIGはツールや他のクライアントからも送れるので、UI側の制限では
            # 足りずここで止める必要がある。
            PLL_MAX = 2304
            PLL_MIN = 200
            # 組合せのままだと比較器がファンアウトの多い経路の前に挟まり、
            # eth_rx が 125MHz 要求に対し 108〜120MHz まで落ちた。設定値は
            # めったに変わらないのでレジスタに落とす(1クロック遅れるだけ)。
            pll_use = Signal(12)
            self.sync += If(self.streamer.cfg_pll_divide > PLL_MAX,
                pll_use.eq(PLL_MAX),
            ).Elif(self.streamer.cfg_pll_divide < PLL_MIN,
                pll_use.eq(PLL_MIN),
            ).Else(
                pll_use.eq(self.streamer.cfg_pll_divide),
            )

            # hs_offset は必ずラインの前半に収める。これを超えるとキャプチャ窓が
            # ライン終端より後ろから始まり、x >= hs_offset が一度も成立せずライン
            # が1本も出なくなる(映像が止まる)。CONFIGはツールや他のクライアント
            # からも送れるので、UI側の制限だけでなくここでも止める。
            # 末尾クリアの開始entry (pll_divide - hs_offset)/2 の桁借りも防げる。
            hs_lim = Signal(13)
            hs_use = Signal(13)
            self.comb += hs_lim.eq(pll_use[1:])                # pll_divide / 2
            self.sync += If(self.streamer.cfg_hs_offset < hs_lim,
                hs_use.eq(self.streamer.cfg_hs_offset),
            ).Else(
                hs_use.eq(hs_lim),
            )
            self.comb += [
                self.capture.cfg_vbp.eq(self.streamer.cfg_vbp),
                self.capture.cfg_hs_offset.eq(hs_use),
                # TVPが検出したインターレース(P/I detect は 0=インターレース)。
                # ★status の lpf_hi/lpf_lo は名前が逆で、lpf_hi←reg37h(L_FRAME_STAT_LSBS)、
                #   lpf_lo←reg38h(L_FRAME_STAT_MSBS)。P/I detect は 38h bit5 なので
                #   参照先は lpf_lo。以前は lpf_hi[5](=ライン数下位バイトのデータビット)を
                #   見ていて、インターレース判定が実質乱数になっていた。
                #   レジスタ名の根拠: Linux drivers/media/i2c/tvp7002_reg.h
                self.capture.cfg_il_detect.eq(~self.status.lpf_lo[5]),
                # TVPが測ったフレーム当たりライン数(37h=LSBs, 38h[3:0]=MSBs)。
                # VSOUTがフィールド単位かフレーム単位かの判別に使う
                self.capture.cfg_lpf_tvp.eq(Cat(self.status.lpf_hi,
                                                self.status.lpf_lo[0:4])),
                self.capture.cfg_sog_vth.eq(self.streamer.cfg_sog_vth),
                self.capture.cfg_field_invert.eq(self.streamer.cfg_field_invert),
                self.capture.cfg_no_raw_phase.eq(self.streamer.cfg_no_raw_phase),
                # S/PDIF デコーダの内部状態(停止の切り分け用)
                self.streamer.stat_spdif_ui.eq(self.spdif.ui_now),
                self.streamer.stat_spdif_resync.eq(self.spdif.resyncs),
                self.streamer.stat_lpf_hi.eq(self.status.lpf_hi),
                self.streamer.stat_syncdet.eq(self.status.syncdet),
                self.streamer.stat_lpf_msbs.eq(self.status.lpf_lo),
                # 12bit値。MSBs側の上位4bitはステータスなのでマスクして組む
                self.streamer.stat_lpf.eq(Cat(self.status.lpf_hi,
                                              self.status.lpf_lo[0:4])),
                self.streamer.stat_cpl.eq(Cat(self.status.cpl_hi,
                                              self.status.cpl_lo[0:4])),
                self.capture.cfg_hs_probe_row.eq(self.streamer.cfg_hs_probe_row),
                self.streamer.stat_hs_raw.eq(self.capture.stat_hs_probe_raw),
                self.streamer.stat_hs_tvp.eq(self.capture.stat_hs_probe_tvp),
                # 38h bit5 は 0=インターレース
                # mflags bit0 = 「スロットが vtotal 個」(フレーム単位の織り込み)。
                # フィールド単位のVSYNCでは折り返さないので 2×vtotal 個になる。
                self.streamer.stat_interlaced.eq(self.capture.il_slot1),
                self.capture.cfg_hs_total.eq(pll_use),
                self.status.cfg_pll_divide.eq(pll_use),
                self.status.cfg_video_bw.eq(self.streamer.cfg_video_bw),
                self.status.cfg_phase.eq(self.streamer.cfg_phase),
                self.status.cfg_sync_ctl.eq(self.streamer.cfg_sync_ctl),
                self.status.cfg_sog_thresh.eq(self.streamer.cfg_sog_thresh),
                self.status.cfg_sep_thresh.eq(self.streamer.cfg_sep_thresh),
                self.status.cfg_precoast.eq(self.streamer.cfg_precoast),
                self.status.cfg_postcoast.eq(self.streamer.cfg_postcoast),
                self.status.cfg_sync_ctl2.eq(self.streamer.cfg_sync_ctl2),
                self.status.cfg_sync_bypass.eq(self.streamer.cfg_sync_bypass),
                self.status.cfg_in_mux2.eq(self.streamer.cfg_in_mux2),
                self.status.cfg_clamp_sel.eq(self.streamer.cfg_clamp_sel),
                self.status.cfg_coarse_gain_gb.eq(self.streamer.cfg_coarse_gain_gb),
                self.status.cfg_coarse_off_g.eq(self.streamer.cfg_coarse_off_g),
                self.status.cfg_coarse_gain_r.eq(self.streamer.cfg_coarse_gain_r),
                self.status.cfg_coarse_off_r.eq(self.streamer.cfg_coarse_off_r),
                self.status.cfg_in_mux1.eq(self.streamer.cfg_in_mux1),
                self.status.cfg_fine_clamp.eq(self.streamer.cfg_fine_clamp),
                self.status.cfg_pll_ctl.eq(self.streamer.cfg_pll_ctl),
                self.status.cfg_clamp_start.eq(self.streamer.cfg_clamp_start),
                self.status.cfg_clamp_width.eq(self.streamer.cfg_clamp_width),
                self.status.cfg_gain_b.eq(self.streamer.cfg_gain_b),
                self.status.cfg_gain_g.eq(self.streamer.cfg_gain_g),
                self.status.cfg_gain_r.eq(self.streamer.cfg_gain_r),
                self.capture.cfg_clear_from.eq((pll_use - hs_use)[1:]),
                # 画素の詰め方は伝送形式から決まる(capture側で pix ドメインへ同期)
                self.capture.cfg_raw_yc.eq(
                    self.streamer.cfg_pixfmt == PIXFMT_YC8),
            ]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--build", action="store_true")
    ap.add_argument("--revision", default="7.0", help="i5 board revision")
    # 以前は eth_rx の125MHzが常にぎりぎりで、ロジックを足すとシード次第で落ちていた。
    # 原因は音声FIFOが分散RAM(LUT)に載っていたことで、LUT使用率が63%あった。
    # buffered=True でBRAMへ移して36%まで下げたところ、seed 1/2/3/5 のいずれでも
    # 全ドメインが余裕を持って通るようになった(eth_rx 130〜136MHz)。
    # ビルド後は必ず全ドメインの PASS/FAIL を確認すること(途中経過ではなく
    # ルーティング後の最終値を見る。dataclk だけ見ると eth_rx の違反を見落とす)。
    # それでも eth_rx はシード次第で125MHzを割ることがある。span計算を2段にした
    # あと seed 2 は eth_rx 123.17MHz で落ち、seed 3 は 131.51MHz で通った
    # (同ビルドの sys は 52.92 → 55.39MHz。要求45MHzに対して余裕があるので、
    # 段を足したこと自体は効いていない)。既定を通る方へ寄せてある。
    #
    # 2026-08-12: インターレースのフィールド単位判定(cfg_lpf_tvp)を足したところ、
    # 今度は seed 3 が eth_rx 123.59MHz で落ちた。判定をsysドメインへ移して
    # pix側の比較器と16bitのCDCを無くしても 124.19MHz で変わらず、ロジック量では
    # なく配置運の問題だった。他のシードは全て余裕を持って通ったので既定を 5 にする:
    #   seed 1 = 128.01MHz / seed 5 = 140.61MHz / seed 7 = 126.69MHz (いずれもPASS)
    # ロジックを足して eth_rx が落ちたら、まず数シード試すこと。
    #
    # 2026-08-14: コンポジット用のCONFIGキー(clamp_sel/coarse_gain/pixfmt)と
    # YC8の詰め替えを足したところ、今度は **seed 5 が eth_rx 123.23MHz で落ちた**。
    # クリティカルパスは LiteEth の `mac_core_tx_cdc` 非同期FIFO内で完結していて
    # (grayカウンタ → readable → DPRAM → 出力FF)、**足したロジックとは無関係**。
    # 内訳も配線 6.17ns / 論理 1.94ns で配置運。既定を通る方へ寄せる:
    #   seed 1 = eth_rx 130.68MHz / sys 53.54MHz (PASS)
    #   seed 3 = eth_rx 136.48MHz / sys 55.17MHz (PASS) ← 余裕が最大なのでこれ
    #   seed 5 = eth_rx 123.23MHz (FAIL)
    #
    # ★レポートは**配置後と配線後の2回**出る。判定はルーティング後(最後の4行)を
    #   見ること。配置後だけ見ると seed 1/3 も FAIL に見える(実際 seed 3 の配置後は
    #   sys 43.66MHz だが配線後は 55.17MHz)。
    ap.add_argument("--seed", type=int, default=3, help="nextpnr placement seed")
    ap.add_argument("--no-capture", action="store_true",
                    help="実キャプチャを無効化しテストパターンを送出")
    # 入力mux(0x19)の切り替え。既定は全て _3 = 基板配線。緑のクランプ異常の
    # 切り分け用で、R/Bも動かして「切り替えでクランプ電圧が別ピンへ移動するか」
    # の対照実験ができる。
    ap.add_argument("--green-input", type=int, default=3, choices=(1, 2, 3, 4),
                    help="緑に使うTVPの GIN_n (既定3=基板配線)")
    ap.add_argument("--red-input", type=int, default=3, choices=(1, 2, 3),
                    help="赤に使うTVPの RIN_n (既定3=基板配線)")
    ap.add_argument("--blue-input", type=int, default=3, choices=(1, 2, 3),
                    help="青に使うTVPの BIN_n (既定3=基板配線)")
    # 画枠の調整ノブ(実機を見ながら追い込む)
    ap.add_argument("--pll-divide", type=int, default=1104,
                    help="H-PLL帰還分周比=1ライン当たりDATACLK数。入力の実水平トータル"
                         "[ドット]に合わせると1サンプル=1ドット(X68000 31kHz≒1104)。"
                         "0でTVP既定1650のまま。実行時は200〜2304に制限される"
                         "(DATACLKがpixドメインのタイミング制約を超えると破綻するため)")
    ap.add_argument("--hs-offset", type=int, default=0,
                    help="水平バックポーチ[DATACLK]。増やすと画が左へ寄る")
    ap.add_argument("--vs-row-at-sync", type=int, default=525,
                    help="VSYNC時にrowへ入れる値。増やすと画が下へ寄る")
    ap.add_argument("--no-measure", action="store_true",
                    help="実測タイミング(周波数カウンタ)を作らない。ノイズ切り分け用")
    ap.add_argument("--no-auto-vtotal", action="store_true",
                    help="実測vtotalへの自動追従を切る(固定値で動かす)")
    ap.add_argument("--interlace", type=int, default=0, choices=(0, 1, 2),
                    help="起動時のウィーブ方式(実行時にCONFIG key 0x13でも切替可)。"
                         "1=1回のVSYNCにフィールドが2枚(X68000 24kHz 1024x848)、"
                         "2=フィールドごとにVSYNC(15kHz 512x512。標準的なインターレース)")
    ap.add_argument("--vbp", type=int, default=0,
                    help="キャプチャ窓の先頭をVSYNCの何行後にするか(垂直バックポーチ)。"
                         "既定0=VSYNC直後から全行取り込む。水平を hs_offset=0 で"
                         "ラインの頭から取り込むのと同じ方針で、垂直位置は管面の"
                         "V位置で決める。0以外にすると上が切れるモードが出る")
    ap.add_argument("--eth-phy", type=int, default=1, choices=(0, 1),
                    help="litex の eth index。1=コネクタETH1=v0.9.0のJ12(既定), "
                         "0=コネクタETH2=v0.9.0のJ11。J11で使うにはシード探索が要る "
                         "(seed 3 では eth_rx 122.55MHz で閉じない)")
    ap.add_argument("--no-mclk-out", action="store_true",
                    help="P3 147のMCLK出力を0固定にする(音声は止まる)。ノイズ切り分け用")
    args = ap.parse_args()
    soc = RetroCastXStream(revision=args.revision, capture=not args.no_capture,
                           green_input=args.green_input,
                           red_input=args.red_input,
                           blue_input=args.blue_input,
                           pll_divide=args.pll_divide,
                           hs_offset=args.hs_offset,
                           vs_row_at_sync=args.vs_row_at_sync,
                           measure=not args.no_measure,
                           mclk_out=not args.no_mclk_out,
                           auto_vtotal=not args.no_auto_vtotal,
                           vbp=args.vbp, interlace_cap=args.interlace,
                           eth_phy=args.eth_phy)
    builder = Builder(soc, output_dir="build/colorlight_i5", compile_software=False)
    builder.build(run=args.build, seed=args.seed)


if __name__ == "__main__":
    main()
