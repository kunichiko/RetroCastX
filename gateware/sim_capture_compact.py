#!/usr/bin/env python3
"""帯域を減らす伝送形式と送出範囲の固定を検証: fake TVP → TvpCapture → RetroCastXStreamer。

`sim_capture_rgb888.py` と同じ駆動で、次の3つを確かめる。

1. YCC422(pixfmt 4、2B/px): 2画素で Y0 Cb0 Y1 Cr0。色差は偶数画素の値
   (チャネルの割り当ては R=Pr G=Y B=Pb)。コンポーネントを RGB888 の 2/3 に
   する形式で、送出範囲の固定と組み合わせて1ラインを1パケットに収める
2. CVBS8(pixfmt 5、1B/px): 緑ch(コンポジットの CVBS)だけを1バイトずつ。
   capture 側が1エントリに4画素を詰めるので、語の組み立てとアドレスの
   進み方(x+=4、entry=x/4)が正しいかをここで見る。ライン全体を送る
3. 送出範囲の固定(key 0x38/0x39): 指定した [start, start+len) だけが届く

駆動値: r = 0x80 + x、g = (row << 5) | x、b = 0x40 + 2x。どの画素がどこへ
入ったかをバイト値から読み取れるようにしてある。

実行: .venv/bin/python sim_capture_compact.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *                                          # noqa: E402
from migen.sim import run_simulation                         # noqa: E402
from litex.soc.interconnect import stream                    # noqa: E402
from liteeth.common import eth_udp_user_description          # noqa: E402
from retrocastx_capture import TvpCapture                    # noqa: E402
from retrocastx_stream import (RetroCastXStreamer, convert_ip,  # noqa: E402
                               PIXFMT_YCC422, PIXFMT_CVBS8)

SYS = 1_000_000
W, NL, NF, VOFF = 16, 4, 8, 2
H = 2 * NL
BOARD_MAC = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
SUBSCRIBER = (convert_ip("192.168.10.3"), 34600)
TYPE_LINE = 0


def drive(x, row):
    return (0x80 + x) & 0xFF, ((row << 5) | x) & 0xFF, (0x40 + 2 * x) & 0xFF


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class _FakeUDPPort:
    def __init__(self, dw=32):
        self.sink = stream.Endpoint(eth_udp_user_description(dw))
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class DUT(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _Pads()
        self.port = _FakeUDPPort()
        self.submodules.cap = TvpCapture(self.pads, width=W, height=H, nface=NF,
                                         hs_offset=1, vs_offset=VOFF)
        self.submodules.streamer = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=200.0,
            announce_period=0.02, mode_period=0.01, sub_timeout=0.05,
            mtu_payload=1472, capture=self.cap)
        # CVBS8 の詰め方は伝送形式から決まる(実機では RetroCastXSoC が結線する)
        self.comb += self.cap.cfg_cvbs8.eq(self.streamer.cfg_pixfmt == PIXFMT_CVBS8)


def _send_datagram(ep, payload, ip, port):
    words = [int.from_bytes(payload[i:i + 4], "little")
             for i in range(0, len(payload), 4)]
    for i, w in enumerate(words):
        yield ep.data.eq(w); yield ep.ip_address.eq(ip)
        yield ep.src_port.eq(port); yield ep.last.eq(i == len(words) - 1)
        yield ep.valid.eq(1); yield
        while not ((yield ep.valid) and (yield ep.ready)):
            yield
    yield ep.valid.eq(0); yield ep.last.eq(0); yield


def subscribe_payload():
    # protocol-v0: magic 'R', ver 0, type 4(SUBSCRIBE), flags 0, frame 0, seq 1, mac, 0
    return bytes([0x52, 0, 4, 0, 0, 0, 1, 0]) + BOARD_MAC + bytes(2)


def run(pixfmt, win=None):
    dut = DUT()
    grams = []

    def setup():
        yield dut.streamer.cfg_pixfmt.eq(pixfmt)
        # 送出範囲の x は取り込み窓の先頭からの画素位置。pll_divide は
        # ライン全体を送る(CVBS8)ときの終端 = 1ライン W 画素
        yield dut.streamer.cfg_pll_divide.eq(W)
        if win:
            yield dut.streamer.cfg_win_start.eq(win[0])
            yield dut.streamer.cfg_win_len.eq(win[1])
        yield

    def subscriber():
        for _ in range(60):
            yield
        yield from _send_datagram(dut.port.source, subscribe_payload(), *SUBSCRIBER)

    def tvp():
        p = dut.pads
        for _ in range(120):
            yield
        for f in range(4):
            yield p.vs.eq(0); yield
            yield p.vs.eq(1)
            for _ in range(4):
                yield
            first = VOFF - 1
            for L in range(first + NL + 1):
                yield p.hs.eq(0); yield
                yield p.hs.eq(1); yield
                yield
                row = L - first
                if 0 <= row < NL:
                    for xp in range(W):
                        r, g, b = drive(xp, row)
                        yield p.r.eq(r); yield p.g.eq(g); yield p.b.eq(b); yield
                    yield p.r.eq(0); yield p.g.eq(0); yield p.b.eq(0)
                    for _ in range(20):
                        yield
                else:
                    for _ in range(W + 8):
                        yield
        for _ in range(400):
            yield

    def collector():
        words = []
        yield dut.port.sink.ready.eq(1)
        for _ in range(40_000):
            snk = dut.port.sink
            if (yield snk.valid) and (yield snk.ready):
                words.append((yield snk.data))
                if (yield snk.last):
                    grams.append(b"".join(w.to_bytes(4, "little") for w in words))
                    words = []
            yield

    run_simulation(dut, [setup(), subscriber(), tvp(), collector()],
                   clocks={"sys": 10, "pix": 10})
    lines = []
    for g in grams:
        if len(g) >= 20 and g[0] == 0x52 and g[2] == TYPE_LINE:
            line, off, cnt = (int.from_bytes(g[o:o + 2], "little") for o in (8, 10, 12))
            lines.append((line, off, cnt, g[14], g[20:]))
    return lines


def check_ycc422():
    lines = run(PIXFMT_YCC422)
    assert lines, "LINE が出ていない"
    n = 0
    for line, off, cnt, fmt, pay in lines:
        assert fmt == PIXFMT_YCC422, fmt
        assert len(pay) >= 2 * cnt, (len(pay), cnt)
        row = line // 2
        for j in range(0, cnt, 2):
            x0 = off + j
            r0, g0, b0 = drive(x0, row)
            _, g1, _ = drive(x0 + 1, row)
            got = tuple(pay[2 * j:2 * j + 4])
            assert got == (g0, b0, g1, r0), f"row{row} x{x0}: {got} 期待 {(g0, b0, g1, r0)}"
            n += 2
    print(f"[OK] YCC422: {len(lines)}本 {n}画素が Y0 Cb0 Y1 Cr0 で届く")


def check_cvbs8():
    lines = run(PIXFMT_CVBS8)
    assert lines, "LINE が出ていない"
    n = 0
    for line, off, cnt, fmt, pay in lines:
        assert fmt == PIXFMT_CVBS8, fmt
        assert off == 0 and cnt == W, f"ライン全体を送っていない: off={off} cnt={cnt}"
        assert len(pay) >= cnt, (len(pay), cnt)
        row = line // 2
        for j in range(cnt):
            assert pay[j] == drive(off + j, row)[1], \
                f"row{row} x{off + j}: {pay[j]:#x} 期待 {drive(off + j, row)[1]:#x}"
            n += 1
    rows = sorted({ln // 2 for ln, *_ in lines})
    assert rows == list(range(NL)), rows
    print(f"[OK] CVBS8: {len(lines)}本 {n}画素が1バイトずつ届く(ライン全体)")


def check_window():
    start, length = 4, 8
    lines = run(PIXFMT_YCC422, win=(start, length))
    assert lines, "LINE が出ていない"
    for line, off, cnt, fmt, pay in lines:
        assert (off, cnt) == (start, length), f"範囲が違う: off={off} cnt={cnt}"
    print(f"[OK] 送出範囲の固定: 全{len(lines)}本が [{start}, {start + length}) だけ")


if __name__ == "__main__":
    check_ycc422()
    check_cvbs8()
    check_window()
