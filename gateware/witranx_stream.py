#!/usr/bin/env python3
"""WiTranX gateware step1: UDP packet streamer on Colorlight i5.

*** UNTESTED SKELETON — ボード未入手のため実機・ビルド未検証 ***

構成は enjoy-digital/colorlite(5A-75B での LiteEth UDP デモ)と
litex-boards の colorlight_i5 ターゲットに倣う。まずは固定ペイロードの
UDP パケットを一定周期で送出し、PC 側で受信できることを確認する(step1)。
その後、プロトコル v0 の MODE/LINE パケタイザに置き換える(step2)。

Build:
    python3 witranx_stream.py --build
"""
import argparse

from migen import *

from litex.gen import LiteXModule
from litex.build.io import DDROutput
from litex_boards.platforms import colorlight_i5

from litex.soc.cores.clock import ECP5PLL
from litex.soc.integration.soc_core import SoCMini
from litex.soc.integration.builder import Builder

from liteeth.phy.ecp5rgmii import LiteEthPHYRGMII
from liteeth.core import LiteEthUDPIPCore

# ネットワーク設定(暫定): FPGA=192.168.10.50 -> PC=192.168.10.1:34600
FPGA_IP = "192.168.10.50"
HOST_IP = "192.168.10.1"
UDP_PORT = 34600
MAC_ADDRESS = 0x10E2D5000001


class _CRG(LiteXModule):
    def __init__(self, platform, sys_clk_freq):
        self.cd_sys = ClockDomain()
        clk25 = platform.request("clk25")
        self.pll = pll = ECP5PLL()
        pll.register_clkin(clk25, 25e6)
        pll.create_clkout(self.cd_sys, sys_clk_freq)


class CounterPayloadStreamer(LiteXModule):
    """step1: 32bitカウンタを含む固定長UDPパケットを周期送出する。

    LiteEth の UDP user port (sink) は param として ip_address / src_port /
    dst_port / length を取り、data ストリームを流し込むとカプセル化される。
    step2 ではここをプロトコル v0 の MODE/LINE パケタイザに差し替える。
    """
    PAYLOAD_WORDS = 16  # 64 bytes

    def __init__(self, udp_port, host_ip, udp_dst, sys_clk_freq, period_s=0.001):
        counter = Signal(32)
        word = Signal(max=self.PAYLOAD_WORDS)
        tick = Signal(max=int(sys_clk_freq * period_s))

        sink = udp_port.sink
        self.comb += [
            sink.ip_address.eq(host_ip),
            sink.src_port.eq(udp_dst),
            sink.dst_port.eq(udp_dst),
            sink.length.eq(self.PAYLOAD_WORDS * 4),
            sink.data.eq(counter),
            sink.last_be.eq(0b1000),  # 32bit datapath, full last word
        ]

        self.fsm = fsm = FSM(reset_state="WAIT")
        fsm.act("WAIT",
            NextValue(tick, tick + 1),
            If(tick == int(sys_clk_freq * period_s) - 1,
                NextValue(tick, 0),
                NextValue(word, 0),
                NextState("SEND"),
            ),
        )
        fsm.act("SEND",
            sink.valid.eq(1),
            sink.last.eq(word == self.PAYLOAD_WORDS - 1),
            If(sink.ready,
                NextValue(counter, counter + 1),
                NextValue(word, word + 1),
                If(word == self.PAYLOAD_WORDS - 1,
                    NextState("WAIT"),
                ),
            ),
        )


class WiTranXStream(SoCMini):
    def __init__(self, revision="7.0", sys_clk_freq=int(60e6)):
        platform = colorlight_i5.Platform(board="i5", revision=revision)
        self.crg = _CRG(platform, sys_clk_freq)
        SoCMini.__init__(self, platform, sys_clk_freq,
                         ident="WiTranX UDP streamer (step1)")

        # Ethernet PHY (RGMII) + hardware UDP/IP core
        self.ethphy = LiteEthPHYRGMII(
            clock_pads=platform.request("eth_clocks", 0),
            pads=platform.request("eth", 0),
            tx_delay=0e-9)
        self.ethcore = LiteEthUDPIPCore(
            phy=self.ethphy,
            mac_address=MAC_ADDRESS,
            ip_address=FPGA_IP,
            clk_freq=sys_clk_freq,
            dw=32)

        udp_port = self.ethcore.udp.crossbar.get_port(UDP_PORT, dw=32)
        host_ip = int.from_bytes(bytes(map(int, HOST_IP.split("."))), "big")
        self.streamer = CounterPayloadStreamer(
            udp_port, host_ip, UDP_PORT, sys_clk_freq)

        # 動作表示LED(パケット送出でトグル)
        # TODO: i5のLEDピン名をプラットフォーム定義で確認
        # led = platform.request("user_led_n", 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--build", action="store_true")
    ap.add_argument("--revision", default="7.0", help="i5 board revision")
    args = ap.parse_args()
    soc = WiTranXStream(revision=args.revision)
    builder = Builder(soc, output_dir="build/colorlight_i5")
    builder.build(run=args.build)


if __name__ == "__main__":
    main()
