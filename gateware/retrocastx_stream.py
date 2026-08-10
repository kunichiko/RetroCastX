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
                 cfg_vbp=43, cfg_hs_offset=0, cfg_pll_divide=1104,
                 cfg_interlace=0):
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
        mode_id, pixfmt = 1, 1             # RGB555 固定
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

        # --- ライン断片化(host/python の protocol.fragment_line と同一の分割) ---
        frag_px_max = ((mtu_payload - 20) // 2) & ~1  # RGB555 2B/px、偶数px(ワード整列)
        assert frag_px_max >= 2, "MTU too small"
        FRAG_PX = frag_px_max          # 1断片の最大ピクセル数(偶数)
        frags = []
        off = 0
        while off < width:
            n = min(frag_px_max, width - off)
            frags.append((off, n))
            off += n
        n_frags = len(frags)
        frag_off_arr = Array(Constant(o, bits_sign=16) for o, _ in frags)
        frag_cnt_arr = Array(Constant(n, bits_sign=16) for _, n in frags)
        frag_len_arr = Array(Constant(20 + 2 * n, bits_sign=16) for _, n in frags)
        frag_nwords_arr = Array(Constant(5 + n // 2, bits_sign=16) for _, n in frags)
        aud_nwords = 5 + audio_nsamples
        max_nwords = max(10, 8, aud_nwords if n_aud else 0,
                         *(5 + n // 2 for _, n in frags))

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
        self.cfg_interlace  = Signal(2, reset=cfg_interlace)     # key 0x13
        self.cfg_field_src  = Signal()                           # key 0x16
        self.cfg_video_bw   = Signal(4, reset=0xF)               # key 0x17
        self.cfg_fine_clamp = Signal(8, reset=0x87)              # key 0x18
        # H-PLL制御(reg 03h)。VCOレンジはピクセルクロックで決まる(データシート
        # Table 4 / 03h の定義)。X68000は全モード PCLK < 36MHz で Ultra low。
        # 既定は 31kHz 768x512 用の 18h。Viewerがモードから計算して送り直す。
        self.cfg_pll_ctl    = Signal(8, reset=0x18)              # key 0x19 (reg 03h)
        self.cfg_clamp_start= Signal(8, reset=0x32)              # key 0x1A (reg 05h)
        self.cfg_clamp_width= Signal(8, reset=0x20)              # key 0x1B (reg 06h)
        self.cfg_gain_b     = Signal(8, reset=35)                # key 0x1C (reg 08h)
        self.cfg_gain_g     = Signal(8, reset=33)                # key 0x1D (reg 09h)
        self.cfg_gain_r     = Signal(8, reset=39)                # key 0x1E (reg 0Ah)
        self.cfg_phase      = Signal(5, reset=16)                # key 0x1F (reg 04h)
        self.cfg_sync_ctl   = Signal(8, reset=0x52)              # key 0x22 (reg 0Eh)
        # TVPのステータス 38h(bit5=P/I detect)。上位から供給される読み出し専用
        self.stat_lpf_hi    = Signal(8)
        # ラインごとのHSYNC周期プローブ。key 0x27 で行を選び 0x28/0x29 で読む
        self.cfg_hs_probe_row = Signal(13)
        self.stat_hs_raw    = Signal(16)
        self.stat_hs_tvp    = Signal(16)
        self.cfg_f2_row     = Signal(13, reset=0)                # key 0x14
        self.cfg_field_swap = Signal()                           # key 0x15
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
                    If((cfg_target == 0) & (cfg_key == 0x13),
                        self.cfg_interlace.eq(rx.data[:2]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x14),
                        self.cfg_f2_row.eq(rx.data[:13]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x15),
                        self.cfg_field_swap.eq(rx.data[0]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x16),
                        self.cfg_field_src.eq(rx.data[0]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x17),
                        self.cfg_video_bw.eq(rx.data[:4]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x1F),
                        self.cfg_phase.eq(rx.data[:5]),
                    ),
                    If((cfg_target == 0) & (cfg_key == 0x22),
                        self.cfg_sync_ctl.eq(rx.data[:8]),
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
        # 応答値(SET/GETとも現在値を返す)
        cfg_reply_val = Signal(32)
        self.comb += [
            If(cfg_target == 1,
                cfg_reply_val.eq(argus_reg),
            ).Elif(cfg_key == 1,
                cfg_reply_val.eq(audio_mask),
            ).Elif(cfg_key == 0x10,
                cfg_reply_val.eq(self.cfg_vbp),
            ).Elif(cfg_key == 0x11,
                cfg_reply_val.eq(self.cfg_hs_offset),
            ).Elif(cfg_key == 0x12,
                cfg_reply_val.eq(self.cfg_pll_divide),
            ).Elif(cfg_key == 0x13,
                cfg_reply_val.eq(self.cfg_interlace),
            ).Elif(cfg_key == 0x14,
                cfg_reply_val.eq(self.cfg_f2_row),
            ).Elif(cfg_key == 0x15,
                cfg_reply_val.eq(self.cfg_field_swap),
            ).Elif(cfg_key == 0x16,
                cfg_reply_val.eq(self.cfg_field_src),
            ).Elif(cfg_key == 0x22,
                cfg_reply_val.eq(self.cfg_sync_ctl),
            ).Elif(cfg_key == 0x1F,
                cfg_reply_val.eq(self.cfg_phase),
            ).Elif(cfg_key == 0x17,
                cfg_reply_val.eq(self.cfg_video_bw),
            ).Elif(cfg_key == 0x18,
                cfg_reply_val.eq(self.cfg_fine_clamp),
            ).Elif(cfg_key == 0x19,
                cfg_reply_val.eq(self.cfg_pll_ctl),
            ).Elif(cfg_key == 0x1A,
                cfg_reply_val.eq(self.cfg_clamp_start),
            ).Elif(cfg_key == 0x1B,
                cfg_reply_val.eq(self.cfg_clamp_width),
            ).Elif(cfg_key == 0x1C,
                cfg_reply_val.eq(self.cfg_gain_b),
            ).Elif(cfg_key == 0x1D,
                cfg_reply_val.eq(self.cfg_gain_g),
            ).Elif(cfg_key == 0x1E,
                cfg_reply_val.eq(self.cfg_gain_r),
            ),
        ]

        # 診断用の読み出し。フィールド極性をどちらから取るべきかを実機で判断する
        # ための生データ(位相が0付近と中央付近で交互になるか、FIDOUTが交互に
        # なるかを見る)。書き込みは無視される読み取り専用。
        if cap_mode:
            self.comb += If(cfg_key == 0x28,
                cfg_reply_val.eq(self.stat_hs_raw),
            ).Elif(cfg_key == 0x29,
                cfg_reply_val.eq(self.stat_hs_tvp),
            ).Elif(cfg_key == 0x27,
                cfg_reply_val.eq(self.cfg_hs_probe_row),
            ).Elif(cfg_key == 0x26,
                cfg_reply_val.eq(self.stat_lpf_hi),      # 38h(bit5=P/I detect)
            ).Elif(cfg_key == 0x24,
                cfg_reply_val.eq(capture.stat_vs_x_raw),
            ).Elif(cfg_key == 0x25,
                cfg_reply_val.eq(capture.stat_hs_len_raw),
            ).Elif(cfg_key == 0x23,
                cfg_reply_val.eq(capture.meas_vtotal),
            ).Elif(cfg_key == 0x20,
                cfg_reply_val.eq(capture.stat_vs_x),
            ).Elif(cfg_key == 0x21,
                cfg_reply_val.eq(capture.stat_fid),
            )

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
                2: hdr.eq(Cat(C(mode_id, 8), C(pixfmt, 8), mflags)),
                3: hdr.eq(Cat(C(width, 16), mode_htotal)),
                4: hdr.eq(Cat(mode_vactive, mode_vtotal)),
                5: hdr.eq(mode_dotclk),
                6: hdr.eq(mode_hfreq),                             # mHz
                7: hdr.eq(mode_vfreq),                             # mHz
            }),
            _T_LINE: Case(word_idx, {
                0: hdr.eq(Cat(C(0x52, 8), C(0, 8), C(0, 8), line_flags)),
                2: hdr.eq(Cat(row, C(0, 16 - len(row)), frag_off)),
                3: hdr.eq(Cat(frag_cnt, C(pixfmt, 8), C(mode_id, 8))),
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
        self.comb += mflags.eq(Cat(self.stat_interlaced, C(0, 15)))
        line_pixdata = capture.rd_data if cap_mode else Cat(pix0.pix, pix1.pix)
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
            self.comb += capture.rd_word.eq(x_next[1:])   # entry = x_next/2
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
            # 組合せで作るとFIFO先頭から加算・比較・muxを経てFSMのレジスタ入力まで
            # 伸び、sysが45MHzを割った(実測 48.1 → 42.4MHz)。レジスタに落とす。
            # FIFO先頭はラインが送出待ちの間ずっと安定しているので、1クロック遅れても
            # 送出開始時には正しい値になっている。
            self.sync += [
                # ライン内の絶対位置にする。hs_offset を足しておけば、受信側は
                # offset_px をそのままライン内の位置として使える(hs_offset が
                # 描画位置に影響しなくなる = ドットクロック再生と描画の分離)。
                span_lo.eq(Cat(C(0, 1), capture.line_first) + self.cfg_hs_offset),
                # line_last は内包。全黒の行では line_first > line_last になるので
                # その場合は空(送るピクセル0)にする。行自体は送る(落とすと受信側が
                # 「黒い行」と「届かなかった行」を区別できない)。
                If(capture.line_last >= capture.line_first,
                    span_hi.eq(Cat(C(0, 1), capture.line_last) + 2
                               + self.cfg_hs_offset),
                ).Else(
                    span_hi.eq(span_lo),
                ),
                If(span_hi - span_lo > FRAG_PX,
                    span_n.eq(FRAG_PX),
                ).Else(
                    span_n.eq(span_hi - span_lo),
                ),
            ]
            line_span = [
                NextValue(x, span_lo),
                NextValue(frag_off, span_lo),
                NextValue(frag_cnt, span_n),
                NextValue(px_end, span_hi),
                NextValue(frag_last, span_lo + span_n >= span_hi),
                NextValue(length, 20 + Cat(C(0, 1), span_n)),
                NextValue(nwords, 5 + span_n[1:]),
            ]
        else:
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
            NextState("SEND"),
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
                        NextValue(x, x + 2),
                        NextValue(frag_idx, frag_idx + 1),
                        NextValue(frag_off, nx_off),
                        NextValue(frag_cnt, nx_cnt),
                        NextValue(frag_last, nx_off + nx_cnt >= px_end),
                        NextValue(length, 20 + Cat(C(0, 1), nx_cnt)),
                        NextValue(nwords, 5 + nx_cnt[1:]),
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
                        NextValue(x, x + 2),
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
                 measure=True, mclk_out=True, auto_vtotal=True, vbp=43,
                 interlace_cap=0):
        from litex_boards.platforms import colorlight_i5
        from liteeth.phy.ecp5rgmii import LiteEthPHYRGMII
        from liteeth.core import LiteEthUDPIPCore

        platform = colorlight_i5.Platform(board="i5", revision=revision,
                                          toolchain="trellis")
        SoCMini.__init__(self, platform, sys_clk_freq,
                         ident="RetroCastX test-pattern streamer (step2)")
        self.crg = _CRG(platform, sys_clk_freq)

        # Ethernet PHY (RGMII) + hardware UDP/IP core
        # eth 1 = U19/U20ボール = U28 = ETH2側PHY(SO-DIMM ETH2_*配線に対応)。
        # eth 0(G1/G2=U29)はETH1側。試作ではMagJackをETH2へ配線したのでindex=1。
        self.ethphy = LiteEthPHYRGMII(
            clock_pads = platform.request("eth_clocks", 1),
            pads       = platform.request("eth", 1),
            tx_delay   = 0e-9)
        self.ethcore = LiteEthUDPIPCore(
            phy         = self.ethphy,
            mac_address = MAC_ADDRESS,
            ip_address  = FPGA_IP,
            clk_freq    = sys_clk_freq,
            dw          = 32,
            # 幅変換・CRC等をsysドメインで実行(eth_rx/txドメインは8bit@125MHzの軽い経路のみ
            # にする。これ無しだとeth_rxの125MHzタイミングが閉じない: 実測93MHz)
            with_sys_datapath = True)

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
            # vs_row_at_sync=525: VSYNC時にrowへ入れる値。525なら 525..567 の43行が
            #   過ぎた所でrowが0(=キャプチャ窓の先頭)になる → 窓はVSYNCの43行後から
            #   512行。vtotal568のうちブランキング56行、その上側43行という妥当な値で、
            #   実機で文字枠が画面中央(row256)に来る位置。実測から算出した調整値。
            # hs_offset: 水平バックポーチ[DATACLK]。pll_divide を変えるとサンプルレートが
            #   変わるので、この値も同じ比率で見直す必要がある。RGB入力を1段レジスタで
            #   受けるようにした分データが1サイクル遅れるので、151→152 に補正している。
            # width はラインを丸ごと保持できる大きさにする(15kHz 1216 / 24kHz 1408 /
            # 31kHz 1104 の htotal がすべて入る)。hs_offset=0 でラインの頭から
            # 取り込み、送るのは中身のある範囲だけにするので、hs_offset は調整
            # 項目でなくなる。height は行位置が半ライン単位のスロットになった分
            # 2倍必要(BRAMはラインバッファ nface 本ぶんだけなので縦は費用ゼロ)。
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
        self.streamer = RetroCastXStreamer(
            udp_port, sys_clk_freq, width=2048, height=2048, fps=60.0,
            audio_sources=[(self.i2s.sources[0], 48000),
                           (self.i2s.sources[1], 48000),
                           (self.spdif.source, self.spdif.rate_hz)],
            capture=capture_obj,
            cfg_vbp=vbp, cfg_hs_offset=hs_offset, cfg_pll_divide=pll_divide,
            cfg_interlace=int(interlace_cap))

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
                self.capture.cfg_interlace.eq(self.streamer.cfg_interlace),
                # TVPが検出したインターレース(38h bit5 P/I detect は 0=インターレース)
                self.capture.cfg_il_detect.eq(~self.status.lpf_hi[5]),
                self.streamer.stat_lpf_hi.eq(self.status.lpf_hi),
                self.capture.cfg_hs_probe_row.eq(self.streamer.cfg_hs_probe_row),
                self.streamer.stat_hs_raw.eq(self.capture.stat_hs_probe_raw),
                self.streamer.stat_hs_tvp.eq(self.capture.stat_hs_probe_tvp),
                # 38h bit5 は 0=インターレース。手動上書き(cfg_interlace)も反映する
                self.streamer.stat_interlaced.eq(
                    ~self.status.lpf_hi[5] | (self.streamer.cfg_interlace != 0)),
                self.capture.cfg_f2_row.eq(self.streamer.cfg_f2_row),
                self.capture.cfg_field_src.eq(self.streamer.cfg_field_src),
                self.capture.cfg_hs_total.eq(pll_use),
                self.capture.cfg_field_swap.eq(self.streamer.cfg_field_swap),
                self.status.cfg_pll_divide.eq(pll_use),
                self.status.cfg_video_bw.eq(self.streamer.cfg_video_bw),
                self.status.cfg_phase.eq(self.streamer.cfg_phase),
                self.status.cfg_sync_ctl.eq(self.streamer.cfg_sync_ctl),
                self.status.cfg_fine_clamp.eq(self.streamer.cfg_fine_clamp),
                self.status.cfg_pll_ctl.eq(self.streamer.cfg_pll_ctl),
                self.status.cfg_clamp_start.eq(self.streamer.cfg_clamp_start),
                self.status.cfg_clamp_width.eq(self.streamer.cfg_clamp_width),
                self.status.cfg_gain_b.eq(self.streamer.cfg_gain_b),
                self.status.cfg_gain_g.eq(self.streamer.cfg_gain_g),
                self.status.cfg_gain_r.eq(self.streamer.cfg_gain_r),
                self.capture.cfg_clear_from.eq((pll_use - hs_use)[1:]),
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
    ap.add_argument("--seed", type=int, default=2, help="nextpnr placement seed")
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
    ap.add_argument("--vbp", type=int, default=43,
                    help="キャプチャ窓の先頭をVSYNCの何行後にするか(垂直バックポーチ)")
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
                           vbp=args.vbp, interlace_cap=args.interlace)
    builder = Builder(soc, output_dir="build/colorlight_i5", compile_software=False)
    builder.build(run=args.build, seed=args.seed)


if __name__ == "__main__":
    main()
