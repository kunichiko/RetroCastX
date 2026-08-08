#!/usr/bin/env python3
"""TvpCapture の実測タイミング(meas_*)を検証する(実機不要)。

MODEパケットで報告する dotclk/hfreq/vfreq/htotal/vtotal がちゃんと駆動されて
いるかを確かめる。以前、別のリファクタで測定ブロックを丸ごと消してしまい、
meas_* が宣言だけ残って全部0になっていたのに、どのSIMも見ていなかったため
気付けなかった(実機のMODE表示で発覚)。その再発防止。

1秒窓のままだとSIMが長くなるので sys_clk_freq を小さくして窓を短縮する。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W, H = 8, 4
VTOTAL = 16          # 1フレーム=16ライン
LINE_CYCLES = 20     # 1ライン=20 pixクロック(HSYNCパルス込み)
WIN = 400            # 測定窓[sysクロック] = sys_clk_freq に渡す値


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
            vtotal=VTOTAL, vs_min_rows=VTOTAL - 2, vs_offset=1, hs_offset=1,
            sys_clk_freq=WIN)


def main():
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    res = {}

    def tb():
        # sysとpixを同じ周期で回すので、WIN sysクロック = WIN pixクロック。
        # その窓の中に入るライン数/フレーム数が期待値になる。
        nline = 0
        for _ in range(5):
            yield
        for _ in range(WIN * 3 // LINE_CYCLES + VTOTAL * 2):
            # 1ライン
            yield p.hs.eq(0); yield
            yield p.hs.eq(1)
            for _ in range(LINE_CYCLES - 1):
                yield
            nline += 1
            if nline % VTOTAL == 0:          # フレーム末尾でVSYNC
                yield p.vs.eq(0); yield
                yield p.vs.eq(1); yield
        res["dotclk"] = (yield cap.meas_dotclk)
        res["hfreq"] = (yield cap.meas_hfreq)
        res["htotal"] = (yield cap.meas_htotal)
        res["vtotal"] = (yield cap.meas_vtotal)

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    print(f"  meas_dotclk {res['dotclk']}   (窓={WIN}サイクル ≒ この値)")
    print(f"  meas_hfreq  {res['hfreq']}   (窓中のライン数 ≒ {WIN // LINE_CYCLES})")
    print(f"  meas_htotal {res['htotal']}   (期待 {LINE_CYCLES})")
    print(f"  meas_vtotal {res['vtotal']}   (期待 {VTOTAL})")

    # 「駆動されているか」が主眼なので、値は概ね合っていればよい
    assert res["dotclk"] > 0, "meas_dotclk が駆動されていない(測定ブロックの消失?)"
    assert res["hfreq"] > 0, "meas_hfreq が駆動されていない"
    assert res["htotal"] == LINE_CYCLES, \
        f"meas_htotal={res['htotal']} 期待={LINE_CYCLES}"
    assert res["vtotal"] == VTOTAL, \
        f"meas_vtotal={res['vtotal']} 期待={VTOTAL}"
    # 窓はWINサイクルちょうどなので、dotclkは窓幅と一致するはず(±数サイクル)
    assert abs(res["dotclk"] - WIN) <= 4, \
        f"meas_dotclk={res['dotclk']} は窓幅{WIN}と大きく違う"
    exp_h = WIN // LINE_CYCLES
    assert abs(res["hfreq"] - exp_h) <= 2, \
        f"meas_hfreq={res['hfreq']} 期待≒{exp_h}"

    print("\n[OK] 実測タイミング: dotclk/hfreq/htotal/vtotal が正しく駆動されている")


if __name__ == "__main__":
    main()
