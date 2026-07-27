#!/usr/bin/env python3
"""RetroCastXStreamer のMigenシミュレーション(実機不要の検証)。

UDPポートを模したエンドポイントを直結し、2シナリオで検証する:

A) プログレッシブ + ライン断片化(小MTU):
   - SUBSCRIBE(ANNOUNCE_ONLY)への ANNOUNCE ユニキャスト返信
   - SUBSCRIBE でのストリーム送り先切替(MODE が LINE に先行)
   - 断片の offset/count が host 側 protocol.fragment_line と一致、
     LAST_FRAGMENT は最終断片のみ、断片タイムスタンプ = ライン先頭 + offset
   - 再構成フレームが pattern.make_frame と(RGB555量子化を除き)ビット一致
   - 購読タイムアウトでストリーム停止
B) インターレース:
   - MODE mflags bit0、vactive=フルフレーム行数
   - frame=フィールド毎+1、FIELD_ODD=フィールド極性、行番号の偶奇=極性
   - 受信側で自然に weave 合成(fill≈0.5/フィールド)、内容の行単位一致

実行: .venv/bin/python sim_stream.py
"""
import os
import random
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "host", "python"))
from retrocastx import pattern, protocol as proto           # noqa: E402
from retrocastx.receiver import FrameAssembler              # noqa: E402

from migen import *                                         # noqa: E402
from migen.sim import run_simulation                        # noqa: E402
from litex.soc.interconnect import stream                   # noqa: E402
from liteeth.common import eth_udp_user_description         # noqa: E402

from retrocastx_stream import RetroCastXStreamer, convert_ip  # noqa: E402

SYS = 1_000_000
W, H = 64, 32
ANN_PERIOD, MODE_PERIOD, SUB_TIMEOUT = 0.004, 0.003, 0.020

PROBER = (convert_ip("192.168.10.2"), 40001)   # discover相当(ANNOUNCE_ONLY)
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)
SUBSCRIBE_AT = 600


class _FakeUDPPort:
    def __init__(self, dw=32):
        self.sink = stream.Endpoint(eth_udp_user_description(dw))
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class DUT(Module):
    def __init__(self, fps, mtu_payload, interlace):
        self.port = _FakeUDPPort()
        self.submodules.streamer = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=fps,
            announce_period=ANN_PERIOD, mode_period=MODE_PERIOD,
            sub_timeout=SUB_TIMEOUT, mtu_payload=mtu_payload,
            interlace=interlace)


def _send_datagram(ep, payload: bytes, ip: int, port: int):
    assert len(payload) % 4 == 0
    words = [int.from_bytes(payload[i:i + 4], "little")
             for i in range(0, len(payload), 4)]
    for i, w in enumerate(words):
        yield ep.data.eq(w)
        yield ep.ip_address.eq(ip)
        yield ep.src_port.eq(port)
        yield ep.last.eq(i == len(words) - 1)
        yield ep.valid.eq(1)
        yield
        while not ((yield ep.valid) and (yield ep.ready)):
            yield
    yield ep.valid.eq(0)
    yield ep.last.eq(0)
    yield


def driver(dut):
    for _ in range(100):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(0, announce_only=True),
                              *PROBER)
    for _ in range(SUBSCRIBE_AT - 110):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(1, announce_only=False),
                              *SUBSCRIBER)


def collector(dut, out, n_cycles):
    rnd = random.Random(1)
    cycle = 0
    words, meta = [], None
    while cycle < n_cycles:
        snk = dut.port.sink
        if (yield snk.valid) and (yield snk.ready):
            if not words:
                meta = (cycle, (yield snk.ip_address), (yield snk.dst_port),
                        (yield snk.length))
            words.append((yield snk.data))
            if (yield snk.last):
                payload = b"".join(w.to_bytes(4, "little") for w in words)
                start, ip, port, length = meta
                assert len(payload) == length, \
                    "length field %d != stream length %d" % (length, len(payload))
                out.append((start, ip, port, payload))
                words, meta = [], None
        yield snk.ready.eq(int(rnd.random() < 0.8))
        yield
        cycle += 1


def run_dut(fps, mtu_payload, interlace, n_cycles):
    dut = DUT(fps, mtu_payload, interlace)
    datagrams = []
    run_simulation(dut, [driver(dut), collector(dut, datagrams, n_cycles)])
    return datagrams


def quantize(img):
    return pattern.rgb555_to_rgb888(pattern.rgb888_to_rgb555(img))


def scenario_a():
    """プログレッシブ + 断片化(MTUペイロード60B → 20px×3 + 4px)。"""
    fps, mtu, n_cycles = 300.0, 60, 30_000
    datagrams = run_dut(fps, mtu, False, n_cycles)
    htotal = int(W * 1.28)
    expected_frags = proto.fragment_line(W, proto.PIXFMT_RGB555, mtu)
    assert len(expected_frags) == 4

    ann_to_prober = 0
    modes, lines = [], []
    first_stream_types = []
    asm = FrameAssembler()
    completed = []
    for start, ip, port, payload in datagrams:
        ptype, pkt = proto.parse(payload)
        if ptype == proto.TYPE_INFO and (ip, port) == PROBER:
            ann_to_prober += 1
        if (ip, port) == SUBSCRIBER and ptype in (proto.TYPE_MODE, proto.TYPE_LINE):
            first_stream_types.append(ptype)
        if ptype == proto.TYPE_MODE:
            modes.append(pkt)
        if ptype == proto.TYPE_LINE:
            assert (ip, port) == SUBSCRIBER
            lines.append((start, pkt))
        completed += asm.feed(payload)
    completed += asm.flush()

    assert ann_to_prober >= 1
    assert first_stream_types and first_stream_types[0] == proto.TYPE_MODE
    m = modes[0]
    assert (m.hactive, m.vactive, m.mflags) == (W, H, 0)

    # 断片構造: offset/countの列がhost実装と一致、LAST_FRAGMENTは最終断片のみ
    by_line = {}
    for _, pkt in lines:
        by_line.setdefault((pkt.frame, pkt.line), []).append(pkt)
    for key, frags in by_line.items():
        got = [(p.offset_px, p.count_px) for p in frags]
        assert got == expected_frags, "frag layout %s != %s at %s" % (
            got, expected_frags, key)
        assert [p.last_fragment for p in frags] == [False, False, False, True]
        # 断片タイムスタンプ = ライン先頭 + offset(ドットクロック)
        base = frags[0].timestamp
        for p in frags:
            assert (p.timestamp - base) & 0xFFFFFFFF == p.offset_px, \
                "fragment timestamp mismatch at %s" % (key,)
    # ライン間のタイムスタンプ歩進 = htotal
    for (f, y), frags in by_line.items():
        nxt = by_line.get((f, y + 1))
        if nxt:
            delta = (nxt[0].timestamp - frags[0].timestamp) & 0xFFFFFFFF
            assert delta == htotal

    assert asm.lost_packets == 0 and asm.orphan_lines == 0
    full = [(fidx, img) for fidx, img, fill in completed if fill == 1.0]
    assert len(full) >= 3, "too few complete frames: %d" % len(full)
    for fidx, img in full:
        assert np.array_equal(img, quantize(pattern.make_frame(W, H, fidx))), \
            "frame %d content mismatch" % fidx

    # 購読タイムアウト後の停止
    sub_expire = SUBSCRIBE_AT + int(SYS * SUB_TIMEOUT)
    last_line = max(start for start, _ in lines)
    assert last_line < sub_expire + 2000
    print("A(fragmentation): OK — %d frames bit-exact, %d LINE pkts "
          "(%d frags/line), stream stopped by %d" % (
              len(full), len(lines), len(expected_frags), last_line))


def scenario_b():
    """インターレース(フィールドレート300、16行/フィールド、単一断片)。"""
    fps, n_cycles = 300.0, 30_000
    datagrams = run_dut(fps, 1472, True, n_cycles)

    modes, lines = [], []
    asm = FrameAssembler()
    completed = []
    for start, ip, port, payload in datagrams:
        ptype, pkt = proto.parse(payload)
        if ptype == proto.TYPE_MODE:
            modes.append(pkt)
        if ptype == proto.TYPE_LINE:
            lines.append(pkt)
        completed += asm.feed(payload)
    completed += asm.flush()

    m = modes[0]
    assert m.mflags & proto.MFLAG_INTERLACE, "MODE interlace flag missing"
    assert (m.hactive, m.vactive) == (W, H)

    for pkt in lines:
        parity = pkt.frame & 1                    # フィールド極性(frame=フィールド毎+1)
        assert (pkt.flags >> 1) & 1 == parity, "FIELD_ODD mismatch"
        assert pkt.line & 1 == parity, "row parity mismatch"
        assert pkt.last_fragment

    # weave: 各フィールド完了時、当該極性の行=当フィールド、逆極性=前フィールドの内容
    fields = {fidx: img for fidx, img, fill in completed
              if abs(fill - 0.5) < 1e-6}
    assert len(fields) >= 3, "too few complete fields: %d" % len(fields)
    checked = 0
    for fidx, img in fields.items():
        if fidx == 0:
            continue  # 逆極性行の期待値は前フィールド由来なのでfidx>=1のみ検査
        cur = quantize(pattern.make_frame(W, H, fidx))
        prv = quantize(pattern.make_frame(W, H, fidx - 1))
        p = fidx & 1
        assert np.array_equal(img[p::2], cur[p::2]), \
            "field %d own-parity rows mismatch" % fidx
        assert np.array_equal(img[1 - p::2], prv[1 - p::2]), \
            "field %d previous-field rows mismatch" % fidx
        checked += 1
    assert checked >= 2
    assert asm.lost_packets == 0 and asm.orphan_lines == 0
    print("B(interlace): OK — %d fields verified (weave row-exact), "
          "%d LINE pkts" % (checked, len(lines)))


def main():
    scenario_a()
    scenario_b()
    print("all scenarios passed")


if __name__ == "__main__":
    main()
