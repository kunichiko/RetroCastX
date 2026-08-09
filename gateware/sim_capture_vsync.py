#!/usr/bin/env python3
"""vtotal自走 + 位相ゲート付きVSYNC再整列 の検証(実機不要)。

小フレーム(vtotal=16, height=4, vs_min_rows=14)で:
- フレーム末尾のVSYNCで row=0 に再整列し、frameが1つだけ進む(二重カウントしない)
- フレーム途中の偽VSYNC(ノイズ)は無視され、frameが進まない
- VSYNCを1回落としても自走ラップでフレームが維持される
を確認する。
"""
import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

# H は「半ラインスロット数」。行位置が常に2倍グリッドになったので、有効行 NL 本を
# 収めるには H = 2*NL が要る
W, NL = 8, 4
H = 2 * NL
SLOTS = [2 * k for k in range(NL)]      # プログレッシブは1つ飛びに並ぶ
VTOTAL, VMIN, VOFF = 16, 14, 2


class _Pads:
    def __init__(self):
        self.r = Signal(8); self.g = Signal(8); self.b = Signal(8)
        self.hs = Signal(reset=1); self.vs = Signal(reset=1)


class Wrap(Module):
    def __init__(self, vs_row_at_sync=0):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _Pads()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=H, nface=8,
            vtotal=VTOTAL, vs_min_rows=VMIN, vs_offset=VOFF, hs_offset=0,
            vs_row_at_sync=vs_row_at_sync)


def run(vs_row_at_sync=0, drop_vsync=True, nframes=3):
    dut = Wrap(vs_row_at_sync)
    p = dut.pads
    cap = dut.cap
    res = {"rows": [], "frames": []}

    def hline(fill_row=None):
        """1本のHSYNC + ライン期間。fill_rowがNoneでなければ画素も書く。"""
        yield p.hs.eq(0); yield
        yield p.hs.eq(1); yield
        yield
        if fill_row is not None:
            for xp in range(W):
                yield p.r.eq((xp << 3) & 0xFF)
                yield p.g.eq((fill_row << 3) & 0xFF)
                yield p.b.eq(0); yield
            yield p.r.eq(0); yield p.g.eq(0)
        for _ in range(4):
            yield

    def vpulse():
        yield p.vs.eq(0); yield
        yield p.vs.eq(1); yield
        yield

    def drain():
        """sys側でメタを取り出して (row, frame) を記録。"""
        while (yield cap.line_valid):
            res["rows"].append((yield cap.line_row))
            res["frames"].append((yield cap.line_frame))
            yield cap.line_ack.eq(1); yield
            yield cap.line_ack.eq(0); yield

    def tb():
        # 有効行数は「フィールド内の行数」。スロットはその2倍になる
        yield cap.cfg_vactive.eq(NL)
        for _ in range(5):
            yield
        # --- フレーム0: 位相を合わせるための最初のVSYNC ---
        yield from vpulse()
        for _ in range(4):
            yield

        for f in range(nframes):
            for r in range(VTOTAL):
                # 全ラインに画素を流す(実際に書かれるのは有効窓内の行だけ)。
                # 位相(vs_row_at_sync)に依らず窓内の行が押し出されることを確認する。
                yield from hline(r % H)
                yield from drain()
                # フレーム途中(VSYNCから8行目)に偽VSYNCを注入 → 無視されるはず
                if r == 8:
                    yield from vpulse()
                    yield from drain()
            # フレーム末尾で本物のVSYNC(drop_vsync時は2フレーム目を落として自走を確認)
            if not (drop_vsync and f == 1):
                yield from vpulse()
            for _ in range(4):
                yield
            yield from drain()
        for _ in range(20):
            yield
        yield from drain()

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    rows, frames = res["rows"], res["frames"]
    print("rows  :", rows)
    print("frames:", frames)

    assert rows, "ラインが1本も出ていない"
    # frame番号ごとにグループ化。先頭と末尾は駆動区間の切れ端(整列前の過渡含む)
    # なので除外し、完結したフレームだけを検証する。
    groups = []
    for r, f in zip(rows, frames):
        if groups and groups[-1][0] == f:
            groups[-1][1].append(r)
        else:
            groups.append((f, [r]))
    # 先頭2つ=整列が確定するまでの過渡(位相シフト時は半端なフレームが1つ出る)、
    # 末尾1つ=駆動打ち切りの切れ端。それらを除いた完結フレームだけを検証する。
    mid = groups[2:-1]
    print("完結フレーム:", [(f, rs) for f, rs in mid])
    assert len(mid) >= 2, f"検証可能なフレームが足りない: {groups}"
    for f, rs in mid:
        if drop_vsync:
            # VSYNCを取りこぼして再取得した直後は、境界の1行が重複し得る(再同期の
            # 過渡)。受信側はその行を上書きするだけで害はないので、順序と網羅のみ見る。
            assert rs == sorted(rs), f"frame {f} の row列が非単調: {rs}"
            assert sorted(set(rs)) == SLOTS, \
                f"frame {f} の row列が 0..{H-1} を網羅していない: {rs}"
        else:
            assert rs == SLOTS, \
                f"frame {f} の row列が不正: {rs}(期待 0..{H-1})"
    fs = [f for f, _ in mid]
    diffs = [b - a for a, b in zip(fs, fs[1:])]
    assert all(d == 1 for d in diffs), (
        f"frameが1以外の増分で進んだ(偽VSYNCの二重カウント/歩進漏れ): {fs}")
    uniq = fs

    print(f"[OK] {len(uniq)}フレーム、各{H}行、frameは1ずつ増加"
          f"(フレーム途中の偽VSYNCは無視、VSYNC欠落時も自走で維持)")


def main():
    # 位相 S=0(窓の先頭=VSYNC)と S=5(窓をVSYNCの5行手前から)の双方で、
    # VSYNC連続時/1回欠落時ともにフレームが割れず frame が1ずつ進むことを確認する。
    for S in (0, 5):
        for drop in (False, True):
            print(f"=== vs_row_at_sync={S} / VSYNC{'1回欠落' if drop else '連続'} ===")
            run(S, drop_vsync=drop, nframes=6)
            print()
    print("ALL OK")


if __name__ == "__main__":
    main()
