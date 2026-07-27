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
    def __init__(self, fps, mtu_payload, interlace, n_audio=0, audio_nsamples=16):
        self.port = _FakeUDPPort()
        self.audio_eps = [stream.Endpoint([("data", 32)]) for _ in range(n_audio)]
        self.audio_rates = [48000, 44100, 32000][:n_audio]
        self.submodules.streamer = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=fps,
            announce_period=ANN_PERIOD, mode_period=MODE_PERIOD,
            sub_timeout=SUB_TIMEOUT, mtu_payload=mtu_payload,
            interlace=interlace,
            audio_sources=list(zip(self.audio_eps, self.audio_rates)),
            audio_nsamples=audio_nsamples)


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


def run_dut(fps, mtu_payload, interlace, n_cycles, n_audio=0,
            audio_nsamples=16, extra_gens=()):
    dut = DUT(fps, mtu_payload, interlace, n_audio, audio_nsamples)
    datagrams = []
    gens = [driver(dut), collector(dut, datagrams, n_cycles)]
    gens += [g(dut) for g in extra_gens]
    run_simulation(dut, gens)
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


CONFIGURER = (convert_ip("192.168.10.4"), 5000)
NSAMP = 16
MASK_SET_AT = 8000


def scenario_c():
    """音声2ソース + CONFIG(マスク変更・ArgusXレジスタ・応答)。"""
    fps, n_cycles = 300.0, 24_000

    def audio_pusher(k, period):
        def gen(dut):
            ep = dut.audio_eps[k]
            for _ in range(700):
                yield
            # 注意: 無限ループにするとrun_simulationが終わらない(全ジェネレータ
            # 終了で完了する仕様)ため、サイクル数に見合う回数で打ち切る
            for i in range((n_cycles - 700) // period):
                yield ep.data.eq(((0xF000 | (k << 8) | (i & 0xFF)) << 16)
                                 | (k << 12) | (i & 0xFFF))
                yield ep.valid.eq(1)
                yield
                yield ep.valid.eq(0)
                for _ in range(period - 1):
                    yield
        return gen

    def config_sender(dut):
        for _ in range(MASK_SET_AT):
            yield
        # src1(bit1)を無効化(src0のみ残す)
        yield from _send_datagram(dut.port.source,
                                  proto.pack_config(10, proto.CFG_TARGET_BOARD,
                                                    proto.CFG_OP_SET,
                                                    proto.CFG_KEY_AUDIO_ENABLE,
                                                    0b001),
                                  *CONFIGURER)
        for _ in range(3000):
            yield
        yield from _send_datagram(dut.port.source,
                                  proto.pack_config(11, proto.CFG_TARGET_ARGUSX,
                                                    proto.CFG_OP_SET,
                                                    proto.CFG_KEY_ARGUSX_INPUT,
                                                    0x1234),
                                  *CONFIGURER)
        for _ in range(2000):
            yield
        yield from _send_datagram(dut.port.source,
                                  proto.pack_config(12, proto.CFG_TARGET_ARGUSX,
                                                    proto.CFG_OP_GET,
                                                    proto.CFG_KEY_ARGUSX_INPUT),
                                  *CONFIGURER)

    datagrams = run_dut(fps, 1472, False, n_cycles, n_audio=2,
                        audio_nsamples=NSAMP,
                        extra_gens=(audio_pusher(0, 25), audio_pusher(1, 25),
                                    config_sender))

    audio = {0: [], 1: []}
    replies = []
    asm = FrameAssembler()
    for start, ip, port, payload in datagrams:
        ptype, pkt = proto.parse(payload)
        if ptype == proto.TYPE_AUDIO:
            assert (ip, port) == SUBSCRIBER, "AUDIO sent to non-subscriber"
            audio[pkt.source].append((start, pkt))
        if ptype == proto.TYPE_CONFIG:
            assert (ip, port) == CONFIGURER, "CONFIG reply to wrong sender"
            replies.append((start, pkt))
        asm.feed(payload)

    # AUDIOパケットの構造とペイロード連続性(FIFO経由でサンプル欠落なし)
    for k in (0, 1):
        assert audio[k], "no AUDIO packets for source %d" % k
        first = audio[k][0][1]
        assert first.nsamples == NSAMP and first.format == proto.AUDIO_FMT_PCM16
        assert first.rate_hz == (48000, 44100)[k]
        seqs = []
        for _, pkt in audio[k]:
            for i in range(pkt.nsamples):
                l = int.from_bytes(pkt.samples[4 * i:4 * i + 2], "little")
                r = int.from_bytes(pkt.samples[4 * i + 2:4 * i + 4], "little")
                assert (l >> 12) == k and (r >> 8) & 0xF == k, \
                    "source %d sample tagged wrong: %04x/%04x" % (k, l, r)
                seqs.append(l & 0xFFF)
        expect = list(range(seqs[0], seqs[0] + len(seqs)))
        assert seqs == [s & 0xFFF for s in expect], \
            "source %d sample continuity broken" % k

    # マスク変更後: src1のAUDIOは止まり、src0は続く
    slack = 2500  # 変更前に溜まった分+送出中の1パケット
    last1 = max(start for start, _ in audio[1])
    assert last1 < MASK_SET_AT + slack, "src1 still streaming after mask change"
    assert max(start for start, _ in audio[0]) > MASK_SET_AT + slack, \
        "src0 stopped unexpectedly"

    # CONFIG応答: 値・エコー・REPLYフラグ
    assert len(replies) == 3, "expected 3 CONFIG replies, got %d" % len(replies)
    vals = [(p.target, p.op, p.key, p.value, p.is_reply) for _, p in replies]
    assert vals[0] == (proto.CFG_TARGET_BOARD, proto.CFG_OP_SET,
                       proto.CFG_KEY_AUDIO_ENABLE, 0b001, True)
    assert vals[1] == (proto.CFG_TARGET_ARGUSX, proto.CFG_OP_SET,
                       proto.CFG_KEY_ARGUSX_INPUT, 0x1234, True)
    assert vals[2] == (proto.CFG_TARGET_ARGUSX, proto.CFG_OP_GET,
                       proto.CFG_KEY_ARGUSX_INPUT, 0x1234, True)

    assert asm.lost_packets == 0, "phantom loss with audio: %d" % asm.lost_packets
    print("C(audio+config): OK — %d/%d AUDIO pkts (src0/src1), samples "
          "contiguous, 3 CONFIG replies verified" % (
              len(audio[0]), len(audio[1])))


def main():
    scenario_a()
    scenario_b()
    scenario_c()
    print("all scenarios passed")


if __name__ == "__main__":
    main()
