#!/usr/bin/env python3
"""1フレームの長さ(sys クロック数)とライン数を同じフレームから取ることを検証する(実機不要)。

ボードの fH は1秒窓で数えていたので、モードが切り替わると
- 窓の途中で切り替わった「混ざった値」(15.98kHz と 31.5kHz で約24kHz)
- ライン数だけ新しく fH は古い MODE
が出ていた。TvpCapture は VSOUT ごとに「そのフレームの sys クロック数とライン数」を
組で取り、受信側が割る。ここでは
- 切り替え前後の組が、それぞれのモードの値と一致すること(混ざった組を出さない)
- 切り替わってから数フレームで新しい組になること
- 新しい組を採ったときに meas_frame_new が立つこと(MODE をすぐ送るのに使う)
- VSOUT が止まったら 0 になること
を確かめる。

切り替え前後でフレームの長さをほぼ同じにしてある(16クロック×100行 と 8クロック×200行)。
クロック数だけで同じフレームと見なすと切り替わりを見落とす。ライン数も見ていることの試験。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W, H = 8, 4
OLD = (16, 100)      # (1ラインのクロック数, 1フレームのライン数)
NEW = (8, 200)
VS_CYC = 2           # 下の lines() が VSYNC で足すクロック数


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class Wrap(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _Pads()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=H, nface=8,
            vtotal=OLD[1], vs_min_rows=OLD[1] - 8, vs_offset=1, hs_offset=1,
            sys_clk_freq=4000)


def expect(mode):
    """直近2回の VSYNC 間の合計(MEAS_FRAME_VSYNCS)"""
    period, vt = mode
    return (2 * (vt * period + VS_CYC), 2 * vt)


def main():
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    log = {"pairs": [], "new": [], "switch_at": None, "stop_at": None}
    t = [0]

    @passive
    def watch():
        """組が変わるたびと meas_frame_new を記録する。止まらないので passive にする"""
        last = None
        while True:
            yield
            t[0] += 1
            pair = ((yield cap.meas_frame_clocks), (yield cap.meas_frame_lines))
            if pair != last:
                log["pairs"].append((t[0], pair))
                last = pair
            if (yield cap.meas_frame_new):
                log["new"].append((t[0], pair))

    def lines(mode, nframes, partial=0):
        period, vt = mode
        for n in range(nframes * vt + partial):
            yield p.hs.eq(0); yield
            yield p.hs.eq(1)
            for _ in range(period - 1):
                yield
            if (n + 1) % vt == 0:
                yield p.vs.eq(0); yield
                yield p.vs.eq(1); yield

    def tb():
        for _ in range(5):
            yield
        # 切り替え前: 数フレーム回して落ち着かせ、フレームの途中で切り替える
        yield from lines(OLD, 6, partial=OLD[1] // 3)
        log["switch_at"] = t[0]
        yield from lines(NEW, 6)
        log["stop_at"] = t[0]
        # VSOUT も HSOUT も止める(無信号)。FR_MAX まで待つと長いので、ここでは
        # 「止まっても前の組を保つ」ことだけ見る
        for _ in range(3000):
            yield

    run_simulation(dut, [tb(), watch()], clocks={"sys": 10, "pix": 10}, vcd_name=None)

    sw = log["switch_at"]
    pairs = log["pairs"]
    print(f"  切り替え t={sw}")
    for ts, pr in pairs:
        print(f"    t={ts:6d}  clocks={pr[0]:5d} lines={pr[1]:4d}")
    print(f"  meas_frame_new {[(ts, pr) for ts, pr in log['new']]}")

    old, new = expect(OLD), expect(NEW)
    seen = [pr for _, pr in pairs if pr != (0, 0)]
    assert seen, "組を一度も採っていない"
    assert seen[0] == old, f"切り替え前の組 {seen[0]} が {old} でない"
    assert all(pr in (old, new) for pr in seen), \
        f"前後どちらでもない組を出した(混ざった): {seen}"
    assert seen[-1] == new, f"切り替え後の組 {seen[-1]} が {new} でない"
    first_new = next(ts for ts, pr in pairs if pr == new)
    frame_new = NEW[0] * NEW[1] + VS_CYC   # 1フレーム分
    lag = first_new - sw
    assert lag < frame_new * 5, f"新しい組になるのが遅い({lag}サイクル = {lag / frame_new:.1f}フレーム)"
    news = [pr for ts, pr in log["new"]]
    assert news[:1] == [old] and new in news, f"meas_frame_new が新しい組で立っていない: {news}"
    assert len(news) == 2, f"meas_frame_new はモードごとに1回(立ち続けない): {news}"
    assert pairs[-1][1] == new, "VSOUT が止まった直後に組を捨てている"
    print(f"OK: 切り替えから {lag / frame_new:.1f} フレームで新しい組になり、混ざった組は出なかった")


def interlace():
    """インターレース: VSYNC 間の HSYNC 数が 100/101 と交互(1フィールド 100.5 ライン)。
    1回分だと fH が交互に揺れる。2回分の合計なら値は1つに揃う"""
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    PERIOD = 8
    seen = set()

    @passive
    def watch():
        while True:
            yield
            pair = ((yield cap.meas_frame_clocks), (yield cap.meas_frame_lines))
            if pair != (0, 0):
                seen.add(pair)

    def tb():
        for _ in range(5):
            yield
        for f in range(10):
            for _ in range(100 + f % 2):
                yield p.hs.eq(0); yield
                yield p.hs.eq(1)
                for _ in range(PERIOD - 1):
                    yield
            yield p.vs.eq(0); yield
            yield p.vs.eq(1); yield

    run_simulation(dut, [tb(), watch()], clocks={"sys": 10, "pix": 10}, vcd_name=None)
    want = (201 * PERIOD + 2 * VS_CYC, 201)
    print(f"  インターレースの組 {sorted(seen)}(期待 {want})")
    assert seen == {want}, f"2フィールド分で揃っていない: {sorted(seen)}"
    print("OK: インターレースでも組は1つの値に揃った")


if __name__ == "__main__":
    main()
    interlace()
