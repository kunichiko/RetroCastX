#!/usr/bin/env python3
"""表示遅延の計測(retrocastx_latency.py + ストリーマの白/黒差し替え)の検証。

キャプチャ → ストリーマ → (UDP) → 「画面」→ センサー の輪を閉じて回す。
画面は受け取った LINE をそのまま覚えておき、センサーの行が白くなってから
DISPLAY_DELAY サイクル後にセンサーを光らせる(暗くなるのも同じ遅れ)。

確かめること:

  1. 計れた値が「白いフレームの最初のラインを送り始めてから、センサーの行が
     届いて DISPLAY_DELAY 経つまで」と数µsの範囲で一致する
  2. 差し替えはフレーム単位: 1つのフレームの中で白と黒が混ざらない
  3. 黒のラインは空(count_px=0)、白のラインは1ラインまるごとで中身が白
  4. 計測をやめると元の映像に戻る
  5. センサーが光らなければ時間切れを数えて次へ進む

実行: .venv/bin/python sim_latency.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "host", "python"))

from migen import *                                        # noqa: E402
from litex.soc.interconnect import stream                  # noqa: E402
from liteeth.common import eth_udp_user_description        # noqa: E402

from retrocastx import protocol as proto                   # noqa: E402
from retrocastx_capture import TvpCapture                  # noqa: E402
from retrocastx_latency import PhotoLatency                # noqa: E402
from retrocastx_stream import RetroCastXStreamer, convert_ip  # noqa: E402

SYS = 1_000_000                  # 1サイクル = 1µs(PhotoLatency の刻みと一致)
W, NL, VOFF, GAP = 64, 16, 2, 8
H = 2 * NL
BOARD_MAC = bytes([0x02, 0x52, 0x43, 0x58, 0x00, 0x01])
SUBSCRIBER = (convert_ip("192.168.10.3"), proto.DEFAULT_PORT)
SENSOR_ROW = 10                  # センサーを貼った行
DISPLAY_DELAY = 700              # 画面に届いてから光るまで [サイクル]
WHITE555 = 0x7FFF


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
        self.sensor = Signal(reset=1)       # 既定の極性: 光ると Low
        self.submodules.cap = TvpCapture(self.pads, width=W, height=H, nface=8,
                                         hs_offset=1, vs_offset=VOFF)
        self.submodules.streamer = s = RetroCastXStreamer(
            self.port, SYS, width=W, height=H, fps=200.0,
            announce_period=0.02, mode_period=0.01, sub_timeout=0.5,
            mtu_payload=1472, capture=self.cap, cfg_pll_divide=W)
        # 試験を短くするため、暗転待ちとばらつきを縮める
        self.submodules.lat = lat = PhotoLatency(
            self.sensor, SYS, dark_hold_us=300, timeout_us=6000,
            debounce_us=3, jitter_bits=6)
        self.comb += [
            s.lat_force.eq(lat.force),
            s.lat_white_req.eq(lat.white_req),
            lat.white_go.eq(s.lat_white_go),
            lat.frame_white.eq(s.lat_frame_white),
        ]


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


def subscriber(dut):
    for _ in range(60):
        yield
    yield from _send_datagram(dut.port.source,
                              proto.pack_subscribe(1, announce_only=False,
                                                   mac=BOARD_MAC), *SUBSCRIBER)


def src_pix(x, row):
    """入力の映像。中身があり、白ではない(差し替えと区別できる)"""
    return 0x40 + (x & 0x1F), 0x80, 0x20 + row


def tvp_driver(dut, nframes):
    p = dut.pads
    for _ in range(120):
        yield
    for _f in range(nframes):
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
                    r8, g8, b8 = src_pix(xp, row)
                    yield p.r.eq(r8); yield p.g.eq(g8); yield p.b.eq(b8)
                    yield
                yield p.r.eq(0); yield p.g.eq(0); yield p.b.eq(0)
                for _ in range(GAP):
                    yield
            else:
                for _ in range(W + 8):
                    yield


class World:
    """UDP の出口・画面・センサー・時刻の記録をまとめて回す。"""
    def __init__(self):
        self.cycle = 0
        self.grams = []              # (受け取り終えたサイクル, payload)
        self.go = []                 # white_go のサイクル
        self.results = []            # (サイクル, 計れた値)
        self.events = []             # (サイクル, センサーを光らせるか)
        self.sensor_row_white = []   # (サイクル, センサーの行が白で届いたか)


def world_proc(dut, w, n_cycles, script):
    words = []
    lit = False
    last_count = 0
    while w.cycle < n_cycles:
        if w.cycle % 10_000 == 0:
            print(f"  … {w.cycle}/{n_cycles} サイクル  計測 {len(w.results)} 回",
                  flush=True)
        for at, fn in script:
            if at == w.cycle:
                yield from fn(dut)
        snk = dut.port.sink
        if (yield snk.valid) and (yield snk.ready):
            words.append((yield snk.data))
            if (yield snk.last):
                payload = b"".join(x.to_bytes(4, "little") for x in words)
                words = []
                w.grams.append((w.cycle, payload))
                ptype, pkt = proto.parse(payload)
                if ptype == proto.TYPE_LINE and pkt.line // 2 == SENSOR_ROW:
                    white = pkt.count_px > 0 and all(
                        v == WHITE555 for v in _pixels(pkt))
                    w.sensor_row_white.append((w.cycle, white))
                    w.events.append((w.cycle + DISPLAY_DELAY, white))
        if (yield dut.streamer.lat_white_go):
            w.go.append(w.cycle)
        for at, on in w.events:
            if at == w.cycle:
                lit = on
        yield dut.sensor.eq(0 if (lit and not w.blind) else 1)
        cnt = (yield dut.lat.stat_count) & 0xFFFF
        if cnt != last_count:
            last_count = cnt
            w.results.append((w.cycle, (yield dut.lat.stat_last)))
        yield snk.ready.eq(1)
        yield
        w.cycle += 1


def _pixels(pkt):
    d = pkt.pixels
    return [int.from_bytes(d[i:i + 2], "little") for i in range(0, len(d), 2)]


def set_ctl(v):
    def f(dut):
        yield dut.lat.cfg_ctl.eq(v)
    return f


def run(script, n_cycles, nframes, blind=False):
    dut = DUT()
    w = World()
    w.blind = blind
    run_simulation(dut, [subscriber(dut), tvp_driver(dut, nframes),
                         world_proc(dut, w, n_cycles, script)],
                   clocks={"sys": 10, "pix": 10})
    return dut, w


def lines_by_frame(w):
    frames = {}
    for cyc, payload in w.grams:
        ptype, pkt = proto.parse(payload)
        if ptype != proto.TYPE_LINE:
            continue
        frames.setdefault(pkt.frame, []).append((cyc, pkt))
    return frames


def classify(pkt):
    if pkt.count_px == 0:
        return "black"
    px = _pixels(pkt)
    if all(v == WHITE555 for v in px):
        return "white"
    return "src"


def main():
    bad = []
    STOP = 60_000
    _, w = run([(2_000, set_ctl(1)), (STOP, set_ctl(0))],
               n_cycles=STOP + 8_000, nframes=48)

    # 1. 計れた値 = 起点から「センサーの行が白で届いて DISPLAY_DELAY」まで
    print(f"計測 {len(w.results)} 回")
    if len(w.results) < 3:
        bad.append(f"計測が {len(w.results)} 回しかできていない")
    for at, val in w.results:
        go = max(g for g in w.go if g < at)
        arrive = min(c for c, white in w.sensor_row_white if white and c > go)
        want = arrive + DISPLAY_DELAY - go
        print(f"  起点 {go:6d}  センサー行の到着 +{arrive - go:5d}  "
              f"期待 {want:5d}µs  計測 {val:5d}µs")
        if not (want - 1 <= val <= want + 2):
            bad.append(f"計測値 {val} が期待 {want} とずれている")

    # 2/3/4. フレームの中身
    frames = lines_by_frame(w)
    seen = set()
    for f, lst in sorted(frames.items()):
        kinds = {classify(p) for _, p in lst}
        seen |= kinds
        if len(kinds) > 1:
            bad.append(f"frame {f} で白/黒/元の映像が混ざっている: {kinds}")
        for _, p in lst:
            k = classify(p)
            if k == "white" and p.count_px < W - 4:
                bad.append(f"frame {f} line {p.line}: 白なのに {p.count_px}px しかない")
    for k in ("src", "black", "white"):
        if k not in seen:
            bad.append(f"{k} のフレームが1つも無い")
    last = max(frames)
    if {classify(p) for _, p in frames[last]} != {"src"}:
        bad.append("計測をやめた後、元の映像に戻っていない")

    # 5. センサーが光らない → 時間切れ
    dut, w2 = run([(2_000, set_ctl(1))], n_cycles=24_000, nframes=16, blind=True)
    if w2.results:
        bad.append("センサーが光っていないのに計れたことになっている")
    print("時間切れの検査: 白いフレームを出した回数", len(w2.go))
    if len(w2.go) < 2:
        bad.append("時間切れのあと次の計測へ進んでいない")

    print()
    if bad:
        for b in bad:
            print("[NG]", b)
        sys.exit(1)
    print("[OK] 起点からセンサーまでの時間を正しく数え、差し替えはフレーム単位")


if __name__ == "__main__":
    main()
