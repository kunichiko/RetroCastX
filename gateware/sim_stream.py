#!/usr/bin/env python3
"""RetroCastXStreamer のMigenシミュレーション(実機不要のstep2検証)。

UDPポートを模したエンドポイントを直結し、
- SUBSCRIBE(ANNOUNCE_ONLY)への ANNOUNCE ユニキャスト返信
- SUBSCRIBE でのストリーム送り先切替(MODE が LINE に先行すること)
- LINE/MODE パケットが host/python の protocol.py でそのままパースできること
- 再構成フレームが pattern.make_frame と(RGB555量子化を除き)ビット一致すること
- 購読タイムアウトでストリームが停止すること
を検証する。実行: .venv/bin/python sim_stream.py

縮小構成(64x32)を使う。パターン生成はビット幅パラメトリックなので
512x512のビルド構成と同一ロジックが検証される。
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

# 縮小構成(実行時間のため)。タイマ類も短縮。
SYS = 1_000_000
W, H, FPS = 64, 32, 400.0
ANN_PERIOD, MODE_PERIOD, SUB_TIMEOUT = 0.004, 0.003, 0.020
N_CYCLES = 26_000

PROBER = (convert_ip("192.168.10.2"), 40001)   # discover相当(ANNOUNCE_ONLY)
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)
SUBSCRIBE_AT = 600                              # このサイクル付近で購読開始


class _FakeUDPPort:
    def __init__(self, dw=32):
        self.sink = stream.Endpoint(eth_udp_user_description(dw))
        self.source = stream.Endpoint(eth_udp_user_description(dw))


class DUT(Module):
    def __init__(self):
        self.port = _FakeUDPPort()
        self.submodules.streamer = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=FPS,
            announce_period=ANN_PERIOD, mode_period=MODE_PERIOD,
            sub_timeout=SUB_TIMEOUT)


def _send_datagram(ep, payload: bytes, ip: int, port: int):
    """8/32bit境界を仮定した簡易ドライバ(SUBSCRIBEは8Bなので2ワード)。"""
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
    """発見プローブ → 購読、の順でSUBSCRIBEを注入する。"""
    for _ in range(100):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(0, announce_only=True),
                              *PROBER)
    while True:
        # おおよそ SUBSCRIBE_AT サイクルまで待つ(自身の消費分は誤差)
        for _ in range(SUBSCRIBE_AT - 110):
            yield
        break
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(1, announce_only=False),
                              *SUBSCRIBER)


def collector(dut, out):
    """TX側を確率的なreadyで受け、(開始サイクル, 宛先, データグラム)を集める。"""
    rnd = random.Random(1)
    cycle = 0
    words, meta = [], None
    while cycle < N_CYCLES:
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


def main():
    dut = DUT()
    datagrams = []
    run_simulation(dut, [driver(dut), collector(dut, datagrams)])
    print("captured %d datagrams" % len(datagrams))

    # --- 期待値(sender_sim.Sender と同一の算出式) ---
    htotal, vtotal = int(W * 1.28), int(H * 1.06)
    dotclk = int(htotal * vtotal * FPS)

    ann_to_prober = 0
    ann_names = set()
    modes = []
    lines = []
    asm = FrameAssembler()
    completed = []
    first_stream_types = []   # 購読先へ送られたパケットの順序(MODE先行の確認用)
    for start, ip, port, payload in datagrams:
        ptype, pkt = proto.parse(payload)
        if ptype == proto.TYPE_INFO:
            ann_names.add((pkt.name, pkt.mac))
            if (ip, port) == PROBER:
                ann_to_prober += 1
        if (ip, port) == SUBSCRIBER and ptype in (proto.TYPE_MODE, proto.TYPE_LINE):
            first_stream_types.append(ptype)
        if ptype == proto.TYPE_MODE:
            assert (ip, port) == SUBSCRIBER, "MODE sent to non-subscriber"
            modes.append(pkt)
        if ptype == proto.TYPE_LINE:
            assert (ip, port) == SUBSCRIBER, "LINE sent to non-subscriber"
            lines.append((start, pkt))
        completed += asm.feed(payload)
    completed += asm.flush()

    # 発見: プローブへのANNOUNCE返信(+定期ANNOUNCE)
    assert ann_to_prober >= 1, "no ANNOUNCE reply to prober"
    assert ann_names == {("retrocastx-i5", bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01]))}

    # MODE の内容と、購読開始直後に MODE が LINE に先行すること
    assert first_stream_types and first_stream_types[0] == proto.TYPE_MODE, \
        "stream did not start with MODE"
    m = modes[0]
    assert (m.hactive, m.vactive, m.htotal, m.vtotal) == (W, H, htotal, vtotal)
    assert m.pixfmt == proto.PIXFMT_RGB555 and m.mode_id == 1
    assert m.dotclk_hz == dotclk
    assert m.hfreq_mhz_x1000 == int(dotclk / htotal * 1000)
    assert m.vfreq_mhz_x1000 == int(FPS * 1000)

    # LINE: ロス/迷子なし、タイムスタンプの歩進 = htotal/ライン
    assert asm.lost_packets == 0, "phantom packet loss: %d" % asm.lost_packets
    assert asm.orphan_lines == 0
    by_key = {(pkt.frame, pkt.line): pkt for _, pkt in lines}
    for (f, y), pkt in by_key.items():
        if (f, y + 1) in by_key:
            delta = (by_key[(f, y + 1)].timestamp - pkt.timestamp) & 0xFFFFFFFF
            assert delta == htotal, "timestamp step %d != htotal %d" % (delta, htotal)

    # フレーム内容: pattern.make_frame とビット一致(RGB555量子化込み)
    full = [(fidx, img) for fidx, img, fill in completed if fill == 1.0]
    assert len(full) >= 3, "too few complete frames: %d" % len(full)
    for fidx, img in full:
        exp = pattern.rgb555_to_rgb888(pattern.rgb888_to_rgb555(
            pattern.make_frame(W, H, fidx)))
        assert np.array_equal(img, exp), "frame %d content mismatch" % fidx

    # 購読タイムアウト後にストリームが止まること
    sub_expire = SUBSCRIBE_AT + int(SYS * SUB_TIMEOUT)
    last_line = max(start for start, _ in lines)
    slack = int(SYS / (FPS * H)) * 3 + 200
    assert last_line < sub_expire + slack, \
        "LINE after subscription expiry (last at %d, expiry %d)" % (last_line, sub_expire)
    assert lines, "no LINE packets at all"

    print("OK: %d frames verified bit-exact, %d LINE / %d MODE / %d ANNOUNCE names, "
          "stream stopped by %d (expiry %d)" % (
              len(full), len(lines), len(modes), len(ann_names),
              last_line, sub_expire))


if __name__ == "__main__":
    main()
