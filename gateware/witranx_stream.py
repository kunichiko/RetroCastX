#!/usr/bin/env python3
"""WiTranX gateware step1: ANNOUNCE beacon on Colorlight i5.

プロトコルv0の ANNOUNCE(TYPE_INFO)パケットを毎秒UDP送出する最小構成。
PC側の `python3 -m witranx.discover` がこれを受信できればstep1完了。
(ブロードキャスト送信はLiteEthの通常UDPパスに無いため、step1では
 HOST_IP宛ユニキャスト。step2でSUBSCRIBE受信→送り先切替を実装する)

Build:
    .venv/bin/python witranx_stream.py --build
Load (board via EXT DAPLink):
    openFPGALoader -c cmsisdap build/colorlight_i5/gateware/colorlight_i5.bit
"""
import argparse
import struct

from migen import *

from litex.gen import LiteXModule
from litex_boards.platforms import colorlight_i5

from litex.soc.cores.clock import ECP5PLL
from litex.soc.integration.soc_core import SoCMini
from litex.soc.integration.builder import Builder

from liteeth.phy.ecp5rgmii import LiteEthPHYRGMII
from liteeth.core import LiteEthUDPIPCore

# --- ネットワーク設定(暫定; 将来はSPIフラッシュの設定ページから読む) ---
MAC_ADDRESS = 0x025754580001          # ローカル管理アドレス "WTX"
FPGA_IP     = "192.168.10.50"
HOST_IP     = "192.168.10.1"
UDP_PORT    = 34600


def make_announce_packet() -> bytes:
    """host/python/witranx/protocol.py の Announce と同一フォーマット(40B)。"""
    common = struct.pack("<BBBBHH", 0x57, 0x00, 3, 0, 0, 0)  # magic,ver,INFO,flags,frame,seq
    mac = bytes([0x02, 0x57, 0x54, 0x58, 0x00, 0x01])
    ip = bytes(int(x) for x in FPGA_IP.split("."))
    info = struct.pack("<6s4sHHH16s", mac, ip, UDP_PORT, 0x0001, 0x0000, b"witranx-i5")
    return common + info


class AnnounceBeacon(LiteXModule):
    """固定40byteのANNOUNCEパケットを period_s ごとにUDP送出する。

    32bitデータパス。ワード内バイト順(リトルエンディアン詰め)は
    実機のWiresharkで要確認(逆なら struct.unpack の "<" を ">" に変える)。
    """
    def __init__(self, udp_port, dst_ip, dst_udp_port, sys_clk_freq, period_s=1.0):
        payload = make_announce_packet()
        assert len(payload) % 4 == 0
        words = [int.from_bytes(payload[i:i+4], "little") for i in range(0, len(payload), 4)]
        n_words = len(words)
        rom = Array(Constant(w, bits_sign=32) for w in words)

        word_idx = Signal(max=n_words)
        tick = Signal(32)
        period_ticks = int(sys_clk_freq * period_s)

        sink = udp_port.sink
        self.comb += [
            sink.ip_address.eq(convert_ip(dst_ip)),
            sink.src_port.eq(dst_udp_port),
            sink.dst_port.eq(dst_udp_port),
            sink.length.eq(len(payload)),
            sink.data.eq(rom[word_idx]),
            sink.last_be.eq(0b1000),
        ]

        self.fsm = fsm = FSM(reset_state="WAIT")
        fsm.act("WAIT",
            NextValue(tick, tick + 1),
            If(tick >= period_ticks - 1,
                NextValue(tick, 0),
                NextValue(word_idx, 0),
                NextState("SEND"),
            ),
        )
        fsm.act("SEND",
            sink.valid.eq(1),
            sink.last.eq(word_idx == n_words - 1),
            If(sink.ready,
                If(word_idx == n_words - 1,
                    NextState("WAIT"),
                ).Else(
                    NextValue(word_idx, word_idx + 1),
                ),
            ),
        )


def convert_ip(ip_str):
    ip = 0
    for x in ip_str.split("."):
        ip = (ip << 8) | int(x)
    return ip


class _CRG(LiteXModule):
    def __init__(self, platform, sys_clk_freq):
        self.cd_sys = ClockDomain()
        clk25 = platform.request("clk25")
        self.pll = pll = ECP5PLL()
        pll.register_clkin(clk25, 25e6)
        pll.create_clkout(self.cd_sys, sys_clk_freq)


class WiTranXStream(SoCMini):
    def __init__(self, revision="7.0", sys_clk_freq=int(60e6)):
        platform = colorlight_i5.Platform(board="i5", revision=revision,
                                          toolchain="trellis")
        SoCMini.__init__(self, platform, sys_clk_freq,
                         ident="WiTranX announce beacon (step1)")
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
            dw          = 32)

        udp_port = self.ethcore.udp.crossbar.get_port(UDP_PORT, dw=32)
        self.beacon = AnnounceBeacon(udp_port, HOST_IP, UDP_PORT, sys_clk_freq)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--build", action="store_true")
    ap.add_argument("--revision", default="7.0", help="i5 board revision")
    args = ap.parse_args()
    soc = WiTranXStream(revision=args.revision)
    builder = Builder(soc, output_dir="build/colorlight_i5", compile_software=False)
    builder.build(run=args.build)


if __name__ == "__main__":
    main()
