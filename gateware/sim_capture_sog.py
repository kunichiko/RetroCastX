#!/usr/bin/env python3
"""SOGOUTの測定が壊れた状態から**自力で復帰する**ことを検証(実機不要)。

## なぜこの試験が要るか

`lowmax`(SOGOUTのLow期間の最大値)は最大値ホールドなのに、以前はリセット経路が
無かった。一度 0xFFFF に飽和すると二度と下がらず、`sog_ok` がその飽和値を
「SOGOUTから同期を分離できている」と判定して、凍った半ライン位相を使い続けた。

実機(S端子)ではこう出た:

    最長Low = 65535(飽和したまま)  垂直間隔 = 33  半ライン位相 = 1419 で固定
    → フィールド極性が判定できず、2枚のフィールドが同じスロットへ交互に上書き
    → 絵が半ライン上下に震える

★**質が悪いのは、スライス閾値を0〜31まで振っても値が1つも動かなかったこと。**
  「信号が悪い」ようにしか見えないが、実際は測定が凍っていた。FPGAを再ロード
  したら生き返って、そこで初めて閾値掃引に意味が出た。**電源を入れ直すまで
  復帰しない**のが最大の問題だった。

だから直したのは2つ:
  - lowmax は HSOUT を数えた固定窓で作り直す(垂直検出が来なくなっても回る)
  - sog_ok は上限も見る(飽和値や、ゲートが作る値では通らないようにする)

Run:  python3 sim_capture_sog.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W = 8
H = 16
VTOTAL = 16
HS_TOTAL = 32          # 1ラインのpixクロック数(短くして回す)
SYNC_LOW = 6           # 通常ラインでSOGOUTがLowな長さ


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)
        self.sogout = Signal(reset=1)


class Wrap(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _Pads()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=H, nface=8,
            vtotal=VTOTAL, vs_min_rows=VTOTAL - 2, vs_offset=0, hs_offset=0,
            vs_row_at_sync=0)


def run():
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    result = {}

    def line(low_len=SYNC_LOW):
        """HSOUT 1発 + SOGOUT の同期パルス1発ぶんのライン。"""
        yield p.hs.eq(0); yield
        yield p.hs.eq(1); yield
        yield p.sogout.eq(0)
        for _ in range(low_len):
            yield
        yield p.sogout.eq(1)
        for _ in range(max(HS_TOTAL - low_len - 2, 1)):
            yield

    def tb():
        yield cap.cfg_hs_total.eq(HS_TOTAL)
        yield cap.cfg_sog_vth.eq(20)
        yield

        # --- 1) 病的な状態を作る: SOGOUTを長時間Lowに張り付かせる ---
        #     low が 0xFFFF まで数え上がり、旧実装ではここで lowmax が
        #     永久に 65535 になった。
        yield p.sogout.eq(0)
        for _ in range(0x10200):
            yield
        yield p.sogout.eq(1)
        yield
        # HSOUTを少し進めて lowmax を窓へ確定させる
        for _ in range(600):
            yield from line()
        result["stuck"] = (yield cap.stat_sog_lowmax)

        # --- 2) 健全な信号へ戻す。**固定窓で作り直されるので自力で復帰する** ---
        for _ in range(1100):
            yield from line()
        result["healed"] = (yield cap.stat_sog_lowmax)

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10})
    return result


def main():
    r = run()
    ok = []

    def check(name, cond, detail=""):
        print("%s %s%s" % ("PASS" if cond else "FAIL", name,
                           "" if cond else "  <- " + detail))
        ok.append(cond)

    # 張り付いた直後は飽和値が見えていてよい(それ自体は測定の事実)
    check("長時間Lowで lowmax が大きくなる", r["stuck"] > 0x8000,
          "lowmax=%d(飽和を作れていない=試験になっていない)" % r["stuck"])
    # ★本題。**電源を入れ直さずに戻ること。**
    check("健全な信号に戻すと lowmax が自力で復帰する",
          r["healed"] <= SYNC_LOW + 2,
          "lowmax=%d のまま(期待 %d 前後)。固定窓のリセットが効いていない"
          % (r["healed"], SYNC_LOW))

    print("\n%d/%d PASS" % (sum(ok), len(ok)))
    return 0 if all(ok) else 1


if __name__ == "__main__":
    raise SystemExit(main())
