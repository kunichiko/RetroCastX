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

from litex.gen import LiteXModule
from litex.soc.cores.clock import ECP5PLL
from litex.soc.integration.soc_core import SoCMini
from litex.soc.integration.builder import Builder

# --- ネットワーク設定(暫定; 将来はSPIフラッシュの設定ページから読む) ---
MAC_ADDRESS = 0x025243580001          # ローカル管理アドレス "RCX"
FPGA_IP     = "192.168.10.50"
HOST_IP     = "192.168.10.1"          # SUBSCRIBE到着までのANNOUNCE宛先(初期値)
UDP_PORT    = 34600

# パケットタイプ(FSM内部コード。プロトコル上のtype値とは別)
_T_LINE, _T_MODE, _T_ANN = 0, 1, 2


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
        self.comb += [
            If(border,
                self.pix.eq(0x7FFF),
            ).Elif(sweep,
                self.pix.eq(Cat(b8[3:], g8[3:], r8[3:])),
            ).Else(
                self.pix.eq(bars[self.x[bar_shift:]]),
            ),
        ]


class RetroCastXStreamer(LiteXModule):
    """プロトコルv0のTX(ANNOUNCE/MODE/LINE)とRX(SUBSCRIBE)を1本のUDPポートで捌く。

    32bitデータパス。ワード内バイト順はリトルエンディアン詰め
    (LiteX StrideConverterは下位バイトから送受する)。
    """
    def __init__(self, udp_port, sys_clk_freq, width=512, height=512, fps=30.0,
                 announce_period=1.0, mode_period=1.0, sub_timeout=10.0,
                 announce_ip=HOST_IP, udp_port_nr=UDP_PORT):
        # --- タイミング諸元(host/python の sender_sim.Sender と同一の算出式) ---
        htotal = int(width * 1.28)
        vtotal = int(height * 1.06)
        dotclk = int(htotal * vtotal * fps)
        hfreq_mhz = int(dotclk / htotal * 1000)
        vfreq_mhz = int(fps * 1000)
        frame_ticks = htotal * vtotal      # フレーム当たりのドットクロック数(ts歩進)
        mode_id, pixfmt = 1, 1             # RGB555 固定

        line_bytes = 8 + 12 + 2 * width    # 共通ヘッダ+LINEヘッダ+RGB555ピクセル
        line_words = line_bytes // 4
        assert line_bytes % 4 == 0 and line_bytes <= 1472, "no fragmentation in step2"
        line_interval = int(sys_clk_freq / (fps * height))
        assert line_interval >= 1

        ann_payload = make_announce_payload()
        ann_words = [int.from_bytes(ann_payload[i:i + 4], "little")
                     for i in range(0, len(ann_payload), 4)]
        n_ann_words = len(ann_words)       # 10

        # --- 状態レジスタ ---
        seq      = Signal(16)
        frame    = Signal(16)
        line     = Signal(max=height)
        ts_frame = Signal(32)              # 現フレーム先頭のドットクロックカウンタ
        ts_line  = Signal(32)              # 現ライン先頭(= ts_frame + line*htotal)

        ann_ip   = Signal(32, reset=convert_ip(announce_ip))
        ann_port = Signal(16, reset=udp_port_nr)
        sub_ip   = Signal(32)
        sub_port = Signal(16)
        sub_timer = Signal(max=max(int(sys_clk_freq * sub_timeout), 2))
        sub_valid = Signal()
        self.comb += sub_valid.eq(sub_timer != 0)

        # --- RX: SUBSCRIBE(先頭ワードに magic/version/type/flags が揃う) ---
        rx = udp_port.source
        rx_first = Signal(reset=1)
        self.comb += rx.ready.eq(1)
        self.sync += If(rx.valid, rx_first.eq(rx.last))
        rx_is_sub = (rx.valid & rx_first &
                     (rx.data[0:8] == 0x52) &    # magic
                     (rx.data[8:16] == 0x00) &   # version
                     (rx.data[16:24] == 4))      # TYPE_SUBSCRIBE
        sub_hit = rx_is_sub & ~rx.data[24]       # flags bit0 = ANNOUNCE_ONLY
        self.sync += [
            If(rx_is_sub,
                ann_ip.eq(rx.ip_address),
                ann_port.eq(rx.src_port),
            ),
            If(sub_hit,
                sub_ip.eq(rx.ip_address),
                sub_port.eq(rx.src_port),
                sub_timer.eq(int(sys_clk_freq * sub_timeout) - 1),
            ).Elif(sub_timer != 0,
                sub_timer.eq(sub_timer - 1),
            ),
        ]

        # --- 送出要求(sticky; RXセットとFSMクリアの同時発生はセット優先) ---
        ann_pending  = Signal()
        mode_pending = Signal()
        line_pending = Signal()
        ann_clr  = Signal()
        mode_clr = Signal()
        line_clr = Signal()

        ann_cnt  = Signal(max=max(int(sys_clk_freq * announce_period), 2))
        mode_cnt = Signal(max=max(int(sys_clk_freq * mode_period), 2))
        line_cnt = Signal(max=max(line_interval, 2))
        self.sync += [
            If(ann_cnt == 0,
                ann_cnt.eq(int(sys_clk_freq * announce_period) - 1),
            ).Else(ann_cnt.eq(ann_cnt - 1)),
            # MODE/LINEのタイマは購読中のみ進める(非購読時はリセット保持)
            If(~sub_valid,
                mode_cnt.eq(int(sys_clk_freq * mode_period) - 1),
                line_cnt.eq(line_interval - 1),
            ).Else(
                If(mode_cnt == 0,
                    mode_cnt.eq(int(sys_clk_freq * mode_period) - 1),
                ).Else(mode_cnt.eq(mode_cnt - 1)),
                If(line_cnt == 0,
                    line_cnt.eq(line_interval - 1),
                ).Else(line_cnt.eq(line_cnt - 1)),
            ),
            # クリアより後に書く=セット優先
            If(ann_clr, ann_pending.eq(0)),
            If(mode_clr, mode_pending.eq(0)),
            If(line_clr, line_pending.eq(0)),
            If((ann_cnt == 0) | rx_is_sub, ann_pending.eq(1)),
            If((sub_valid & (mode_cnt == 0)) | sub_hit, mode_pending.eq(1)),
            If(sub_valid & (line_cnt == 0), line_pending.eq(1)),
        ]

        # --- テストパターン(1ワード=2ピクセル) ---
        x = Signal(max=width)              # 常に偶数(ピクセルペアの左)
        self.pix0 = pix0 = PatternPixel(width, height)
        self.pix1 = pix1 = PatternPixel(width, height)
        self.comb += [
            pix0.x.eq(x),          pix0.y.eq(line), pix0.frame.eq(frame),
            pix1.x.eq(x | 1),      pix1.y.eq(line), pix1.frame.eq(frame),
        ]

        # --- TX FSM ---
        ptype    = Signal(2)
        dst_ip   = Signal(32)
        dst_port = Signal(16)
        length   = Signal(16)
        nwords   = Signal(max=line_words + 1)
        word_idx = Signal(max=line_words)

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
        self.comb += [
            If(ptype == _T_ANN,
                w1.eq(Cat(C(0, 16), seq)),           # ANNOUNCEはframe=0
            ).Else(
                w1.eq(Cat(frame, seq)),
            ),
            Case(ptype, {
                _T_ANN: Case(word_idx, {i: hdr.eq(ann_words[i])
                                        for i in range(n_ann_words) if i != 1}),
                _T_MODE: Case(word_idx, {
                    0: hdr.eq(0x52 | (1 << 16)),                       # type=MODE
                    2: hdr.eq(mode_id | (pixfmt << 8)),                # mflags=0
                    3: hdr.eq(width | (htotal << 16)),
                    4: hdr.eq(height | (vtotal << 16)),
                    5: hdr.eq(dotclk),
                    6: hdr.eq(hfreq_mhz),
                    7: hdr.eq(vfreq_mhz),
                }),
                _T_LINE: Case(word_idx, {
                    0: hdr.eq(0x52 | (0 << 16) | (1 << 24)),           # flags=LAST_FRAGMENT
                    2: hdr.eq(Cat(line, C(0, 32))[:32]),               # line | offset_px=0
                    3: hdr.eq(width | (pixfmt << 16) | (mode_id << 24)),
                    4: hdr.eq(ts_line),
                }),
            }),
            If(word_idx == 1, hdr.eq(w1)),
            If((ptype == _T_LINE) & (word_idx >= 5),
                sink.data.eq(Cat(pix0.pix, pix1.pix)),
            ).Else(
                sink.data.eq(hdr),
            ),
        ]

        self.fsm = fsm = FSM(reset_state="IDLE")
        fsm.act("IDLE",
            NextValue(word_idx, 0),
            If(ann_pending,
                ann_clr.eq(1),
                NextValue(ptype, _T_ANN),
                NextValue(dst_ip, ann_ip),
                NextValue(dst_port, ann_port),
                NextValue(length, len(ann_payload)),
                NextValue(nwords, n_ann_words),
                NextState("SEND"),
            ).Elif(mode_pending & sub_valid,
                mode_clr.eq(1),
                NextValue(ptype, _T_MODE),
                NextValue(dst_ip, sub_ip),
                NextValue(dst_port, sub_port),
                NextValue(length, 32),
                NextValue(nwords, 8),
                NextState("SEND"),
            ).Elif(line_pending & sub_valid,
                line_clr.eq(1),
                NextValue(ptype, _T_LINE),
                NextValue(dst_ip, sub_ip),
                NextValue(dst_port, sub_port),
                NextValue(length, line_bytes),
                NextValue(nwords, line_words),
                NextValue(x, 0),
                NextState("SEND"),
            ),
        )
        fsm.act("SEND",
            sink.valid.eq(1),
            sink.last.eq(word_idx == nwords - 1),
            If(sink.ready,
                If(word_idx == nwords - 1,
                    NextValue(seq, seq + 1),
                    If(ptype == _T_LINE,
                        If(line == height - 1,
                            NextValue(line, 0),
                            NextValue(frame, frame + 1),
                            NextValue(ts_frame, ts_frame + frame_ticks),
                            NextValue(ts_line, ts_frame + frame_ticks),
                        ).Else(
                            NextValue(line, line + 1),
                            NextValue(ts_line, ts_line + htotal),
                        ),
                    ),
                    NextState("IDLE"),
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


class RetroCastXStream(SoCMini):
    def __init__(self, revision="7.0", sys_clk_freq=int(50e6)):
        from litex_boards.platforms import colorlight_i5
        from liteeth.phy.ecp5rgmii import LiteEthPHYRGMII
        from liteeth.core import LiteEthUDPIPCore

        platform = colorlight_i5.Platform(board="i5", revision=revision,
                                          toolchain="trellis")
        SoCMini.__init__(self, platform, sys_clk_freq,
                         ident="RetroCastX test-pattern streamer (step2)")
        self.crg = _CRG(platform, sys_clk_freq)

        # Ethernet PHY (RGMII) + hardware UDP/IP core
        self.ethphy = LiteEthPHYRGMII(
            clock_pads = platform.request("eth_clocks", 0),
            pads       = platform.request("eth", 0),
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

        udp_port = self.ethcore.udp.crossbar.get_port(UDP_PORT, dw=32)
        self.streamer = RetroCastXStreamer(udp_port, sys_clk_freq)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--build", action="store_true")
    ap.add_argument("--revision", default="7.0", help="i5 board revision")
    # タイミングは with_sys_datapath=True + sys 50MHz で収束。step2はseed3で
    # eth_rx 130.8/125MHz, sys 52.9/50MHz(seed2はeth_rx 123.5MHzで僅かに未達)。
    # seedは今後のリグレッション時の調整用ノブとして残す
    ap.add_argument("--seed", type=int, default=3, help="nextpnr placement seed")
    args = ap.parse_args()
    soc = RetroCastXStream(revision=args.revision)
    builder = Builder(soc, output_dir="build/colorlight_i5", compile_software=False)
    builder.build(run=args.build, seed=args.seed)


if __name__ == "__main__":
    main()
