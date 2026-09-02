#!/usr/bin/env python3
"""RGB888 伝送(wire_bpp=3)の検証: fake TVP → TvpCapture → RetroCastXStreamer。

`sim_capture_stream.py` の RGB555 版と同じ駆動で、**3バイト/画素**の
LINE ペイロードが r/g/b のバイト列として正しく出るかを検証する。

★検証の主眼は**4画素=3語のギアボックス**。語1が2エントリを跨ぐため、
  前エントリのラッチ(ent_prev)とアドレス先出しの整合が崩れると、
  ここで画素がずれる形で出る。

    entry e   → slot0(px0) slot1(px1)
    entry e+1 → slot2(px2) slot3(px3)
    W0 = r0 g0 b0 r1 / W1 = g1 b1 r2 g2 / W2 = b2 r3 g3 b3

RGB符号化: r=xpix<<3, g=row<<3, b=0(RGB555版と同一の駆動)。
"""
import collections, os, sys
sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "host", "python"))
from migen import *
from litex.soc.interconnect import stream
from liteeth.common import eth_udp_user_description
from retrocastx import protocol as proto
from retrocastx_capture import TvpCapture
from retrocastx_stream import RetroCastXStreamer, convert_ip

SYS = 1_000_000
# H は「半ラインスロット数」。行位置が常に2倍グリッドになったので、有効行 NL 本を
# 収めるには H = 2*NL が要る(row_bits = log2(H) がスロット番号の幅で、これを
# 超えると折り返して上書きされる)。
W, NL, NF, VOFF = 16, 4, 8, 2
H = 2 * NL
BOARD_MAC = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)


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
            mtu_payload=1472, capture=self.cap, wire_bpp=3)


def expect_pix(x, row):
    """駆動側の r=xpix<<3 / g=row<<3 / b=0 をそのまま8bitで期待する。

    RGB555 版と違い**丸めが入らない**ので、駆動値と1バイトずつ一致するはず。
    """
    return (((x & 0x1F) << 3) & 0xFF, ((row & 0x1F) << 3) & 0xFF, 0)


def _send_datagram(ep, payload, ip, port):
    words = [int.from_bytes(payload[i:i+4], "little")
             for i in range(0, len(payload), 4)]
    for i, w in enumerate(words):
        yield ep.data.eq(w); yield ep.ip_address.eq(ip)
        yield ep.src_port.eq(port); yield ep.last.eq(i == len(words)-1)
        yield ep.valid.eq(1); yield
        while not ((yield ep.valid) and (yield ep.ready)):
            yield
    yield ep.valid.eq(0); yield ep.last.eq(0); yield


def subscriber(dut):
    for _ in range(60):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(1, announce_only=False,
                                                   mac=BOARD_MAC), *SUBSCRIBER)


def tvp_driver(dut):
    p = dut.pads
    for _ in range(120):
        yield
    for f in range(4):                         # 4フレーム
        yield p.vs.eq(0); yield
        yield p.vs.eq(1)
        for _ in range(4):
            yield
        first = VOFF - 1
        for L in range(first + NL + 1):
            yield p.hs.eq(0); yield
            yield p.hs.eq(1); yield
            yield
            row_eff = L - first
            if 0 <= row_eff < NL:
                for xp in range(W):
                    yield p.r.eq((xp << 3) & 0xFF)
                    yield p.g.eq((row_eff << 3) & 0xFF)
                    yield p.b.eq(0); yield
                yield p.r.eq(0); yield p.g.eq(0)
                for _ in range(20):            # ライン間ギャップ(TXが追いつく余裕)
                    yield
            else:
                for _ in range(W + 8):
                    yield
    for _ in range(400):
        yield


def collector(dut, out, n_cycles):
    cycle = 0
    words, meta = [], None
    yield dut.port.sink.ready.eq(1)
    while cycle < n_cycles:
        snk = dut.port.sink
        if (yield snk.valid) and (yield snk.ready):
            if not words:
                meta = ((yield snk.length),)
            words.append((yield snk.data))
            if (yield snk.last):
                payload = b"".join(w.to_bytes(4, "little") for w in words)
                out.append(payload)
                words, meta = [], None
        yield
        cycle += 1


def main():
    dut = DUT()
    grams = []
    N = 40_000
    run_simulation(
        dut,
        [subscriber(dut), tvp_driver(dut), collector(dut, grams, N)],
        clocks={"sys": 10, "pix": 10})

    lines = []
    for payload in grams:
        ptype, pkt = proto.parse(payload)
        if ptype == proto.TYPE_LINE:
            lines.append(pkt)
    print(f"datagrams={len(grams)} LINE packets={len(lines)}")
    rows_seen = sorted(set(p.line for p in lines))
    print("rows seen:", rows_seen)

    assert lines, "LINEパケットが1つも出ていない"
    checked = 0
    for pkt in lines:
        assert pkt.pixfmt == proto.PIXFMT_RGB888, \
            f"pixfmt が RGB888 でない: {pkt.pixfmt}"
        assert len(pkt.pixels) >= 3 * pkt.count_px, (
            f"ペイロードが短い: {len(pkt.pixels)}B < 3×{pkt.count_px}")
        px = [tuple(pkt.pixels[3*j:3*j+3]) for j in range(pkt.count_px)]
        for j, v in enumerate(px):
            x = pkt.offset_px + j
            # pkt.line は半ライン単位のスロット。中身は「何行目を流したか」で
            # 符号化してあるので、スロットを2で割って行番号に戻す
            exp = expect_pix(x, pkt.line // 2)
            assert v == exp, (
                f"row{pkt.line} x{x}: got={v} exp={exp} "
                f"(off={pkt.offset_px} cnt={pkt.count_px}, 位相={j % 3})")
            checked += 1
    # プログレッシブなのでスロットは1つ飛び(0,2,4,...)。空くスロットは受信側が
    # 「次のラインまでの間隔」ぶん太らせて埋める
    want = set(range(0, H, 2))
    assert set(rows_seen) == want, f"全行が揃っていない: {rows_seen} (期待 {sorted(want)})"
    # 重複ライン検出: (frame,row) は一意でなければならない。sticky な送出要求で
    # FIFOが空になった直後に「空ヘッドの残留値」を再送すると、ここで重複が出る
    # (実機では受信側のframe番号が N↔N+1 を往復し、見かけfpsが5倍になった)。
    keys = [(p.frame, p.line) for p in lines]
    dups = [k for k, c in collections.Counter(keys).items() if c > 1]
    assert not dups, f"重複ライン(frame,row)が {len(dups)} 件: {dups[:5]}"
    # タイムスタンプ(キャプチャのDATACLK自走カウンタ)は送出順に単調増加すること。
    # 定数計算ではなく実カウンタ由来なので、CDC/ラッチ経路の健全性がここで効く。
    ts = [p.timestamp for p in lines]
    bad = [(a, b) for a, b in zip(ts, ts[1:]) if b <= a]
    assert not bad, f"タイムスタンプが単調増加でない: {bad[:5]}"
    print(f"timestamp: {ts[0]} → {ts[-1]} (単調増加, {len(ts)}本)")
    print(f"\n[OK] RGB888(4画素=3語)伝送: {len(lines)}本のLINEを"
          f"バイト単位で検証 (全{H}行, 総画素{checked})")


if __name__ == "__main__":
    main()
