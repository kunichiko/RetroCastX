#!/usr/bin/env python3
"""偽VSYNCで vtotal 追従が壊されないことを確かめる(実機不要)。

**この試験が守っているもの**
実機(X68000 768x512 / vtotal 568)で、数十秒に一度だけ vtotal が
210 / 311 / 367 / 1136 / 2446 といった値に化けた。原因は2つ重なっていた:

  1. 測定用VSYNC(vs_meas)には固定ガード64行しか掛かっていない。捕捉用の
     vs_edge は cfg_vs_min_rows で守られているが、測定側は「モードが分から
     ないうちに測る」ためにあえて緩い。64行より後に出た偽VSYNCは素通りする。
  2. それを cfg_vtotal に流し込まない砦であるヒステリシスが効いていなかった。
     連続回数を **sysクロックごと** に数えていたのに、meas_vtotal は1秒に
     1回しか変わらない。新しい値が現れた最初のサイクルには既に飽和しており、
     単発の異常値がそのまま採用されていた。

cfg_vtotal は自走フレームカウンタの一周点なので、化けると絵が壊れる。さらに
MODEパケットにも生値が載っていたため、Viewer がモード変更と受け取って
ウィンドウのリサイズと再フィットを始めていた。

いまはフレーム単位(VSYNCごと)に連続一致を見てから通す。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W, H = 8, 4
VTOTAL = 200          # 平常時のライン数(測定ガード64より十分大きく取る)
NEWTOTAL = 260        # 本物のモード変更後のライン数
LINE_CYCLES = 8
WIN = 2000            # 測定窓[sysクロック]


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
            vtotal=VTOTAL, vs_min_rows=VTOTAL - 8, vs_offset=1, hs_offset=1,
            sys_clk_freq=WIN, auto_vtotal=True)


def main():
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    log = []          # (何をしたか, その時点の cfg_vtotal, meas_vtotal_stable)
    seen = set()      # cfg_vtotal が一瞬でも取った値すべて

    def line():
        yield p.hs.eq(0); yield
        yield p.hs.eq(1)
        for _ in range(LINE_CYCLES - 1):
            yield

    def vsync():
        yield p.vs.eq(0); yield
        yield p.vs.eq(1); yield

    def frame(nlines, glitch_at=None):
        """nlines 本のラインを流す。glitch_at 行目で偽VSYNCを1発混ぜる。

        最後の値だけ見ていると「化けてから戻った」のを見逃すので、
        1ラインごとに cfg_vtotal を覗いて通過した値を全部記録する。
        """
        for i in range(nlines):
            yield from line()
            if glitch_at is not None and i == glitch_at:
                yield from vsync()
            seen.add((yield cap.cfg_vtotal))
        yield from vsync()

    def snap(tag):
        log.append((tag, (yield cap.cfg_vtotal), (yield cap.meas_vtotal_stable)))

    def tb():
        for _ in range(5):
            yield
        # 平常フレームで vt_ok を確定させる
        for _ in range(6):
            yield from frame(VTOTAL)
        yield from snap("平常")

        # 偽VSYNCを位置を変えて何度も混ぜる。
        # **1発では足りない。** 1秒窓のサンプリングが偽値の載っている短い間
        # (次の本物のVSYNCまで)に当たらないと meas_vtotal には現れないので、
        # 修正前のコードでも運良く素通りしてしまうことがある。実機で「数十秒に
        # 一度」しか出なかったのも同じ理由。位置をずらして繰り返し、必ず当てる。
        for k in range(12):
            yield from frame(VTOTAL, glitch_at=100 + k * 3)
            yield from frame(VTOTAL)
        yield from snap("偽VSYNC12発の後")

        # 本物のモード変更にはちゃんと追従すること
        for _ in range(8):
            yield from frame(NEWTOTAL)
        yield from snap("モード変更後")

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    for tag, cfg, stable in log:
        print(f"  {tag:<22} cfg_vtotal {cfg:>5}   meas_vtotal_stable {stable:>5}")

    print(f"  cfg_vtotal が通過した値すべて: {sorted(seen)}")

    d = dict((t, (c, s)) for t, c, s in log)
    # 一瞬でも別の値を通っていたら、そのフレームは壊れて送られている
    assert seen <= {VTOTAL, NEWTOTAL}, \
        f"偽VSYNCで cfg_vtotal が化けた: {sorted(seen - {VTOTAL, NEWTOTAL})}"
    assert d["平常"] == (VTOTAL, VTOTAL), \
        f"平常時に追従できていない: {d['平常']} 期待 ({VTOTAL}, {VTOTAL})"
    for tag in ("偽VSYNC12発の後",):
        cfg, stable = d[tag]
        assert cfg == VTOTAL, f"{tag}: 偽VSYNCで cfg_vtotal が {cfg} に化けた"
        assert stable == VTOTAL, f"{tag}: 偽VSYNCで報告値が {stable} に化けた"
    assert d["モード変更後"] == (NEWTOTAL, NEWTOTAL), \
        f"本物のモード変更に追従できていない: {d['モード変更後']} 期待 ({NEWTOTAL}, {NEWTOTAL})"

    print("\n[OK] 偽VSYNCは通さず、本物のモード変更には追従する")


if __name__ == "__main__":
    main()
