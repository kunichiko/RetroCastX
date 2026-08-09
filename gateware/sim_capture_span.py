#!/usr/bin/env python3
"""ライン毎の「黒でない範囲」の記録を検証する(実機不要)。

hs_offset を 0 にしてラインの頭から取り込む代わりに、送るのはこの範囲だけにする。
そうすると hs_offset は調整項目でなくなり(ドットクロック再生と描画位置が分離される)、
帯域もライン全体ではなく有効映像の幅ぶんで済む。

範囲は entry 単位(=2画素)で持つ。パケタイザが32bitワード(2画素)単位で読むので、
そこに合わせておくと端の扱いが要らない。

確認すること:
  - 黒→内容→黒 のラインで、内容のある entry だけが範囲になる
  - しきい値以下(暗いノイズ相当)は黒として扱われ、範囲を広げない
  - 全部黒のラインは「空の範囲」として送られる(行自体は落とさない)
  - 範囲はラインごとに独立(前のラインの範囲を引きずらない)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W = 64                # バッファ幅[px] → entries = 32
H = 8
VTOTAL = 8
LINE = 96             # 1ラインの長さ[pixクロック]。バッファ幅より長い


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
            vtotal=VTOTAL, vs_min_rows=VTOTAL - 2, vs_offset=0, hs_offset=0,
            vs_row_at_sync=0, hs_total=LINE)


def run(lines):
    """lines: 各ラインの [(x, level), ...]。level は 0..255 の輝度(R=G=B)"""
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    got = []

    def hline(spec):
        yield p.hs.eq(0); yield
        yield p.hs.eq(1); yield
        vals = dict(spec)
        for i in range(LINE - 2):
            v = vals.get(i, 0)
            yield p.r.eq(v); yield p.g.eq(v); yield p.b.eq(v)
            yield

    def drain():
        while (yield cap.line_valid):
            got.append((
                (yield cap.line_row),
                (yield cap.line_first),
                (yield cap.line_last),
            ))
            yield cap.line_ack.eq(1); yield
            yield cap.line_ack.eq(0); yield

    def tb():
        yield cap.cfg_vactive.eq(H)
        for _ in range(4):
            yield
        yield p.vs.eq(0); yield
        yield p.vs.eq(1); yield
        for spec in lines:
            yield from hline(spec)
            yield from drain()
        for _ in range(20):
            yield
        yield from drain()

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)
    # 先頭は過渡(VSYNC直後の切れ端で、まだ内容が流れていない)。他のSIMと同様に外す
    return got[1:]


def main():
    # 期待値は entry 単位で ±1 の幅を許す。
    # ペアは「どちらかの画素が非黒」なら範囲に含めるので、明部の始まり/終わりが
    # ペアの途中に来ると1つ手前/先の entry も入る。さらにテストベンチが信号を
    # 立ててからモジュールが見るまで1クロックずれる。ここで確かめたいのは
    # 「範囲がバッファ全体に広がらず、内容のある所だけに絞られること」なので、
    # 端1entryのずれは許容する。
    def near(got, want, tol=1):
        return abs(got - want) <= tol
    print("=== 黒→内容→黒 ===")
    # 画素 x=20..27 だけ明るいライン。entry は 10..13
    got = run([[(x, 200) for x in range(19, 27)]] * 4)
    assert got, "ラインが1本も出てこない"
    for row, first, last in got:
        print(f"  row {row}: entry {first}..{last}")
        assert near(first, 10) and near(last, 13), \
            f"範囲が不正: {first}..{last} (期待 10..13 ±1)"
    print(f"  [OK] {len(got)}本すべて entry 10..13 付近(バッファ全幅 32 に対し4entry)")

    print("\n=== しきい値以下は黒として扱う ===")
    # RGB555 の 1 = RGB888 の 8..15。既定しきい値 2 なので level 16 (555で2) は
    # 「> 2」を満たさず黒。level 200 (555で25) は非黒。
    got = run([[(x, 16) for x in range(10, 40)] +
               [(x, 200) for x in range(19, 23)]] * 4)
    for row, first, last in got:
        print(f"  row {row}: entry {first}..{last}")
        assert near(first, 10) and near(last, 11), \
            f"暗い画素が範囲を広げている: {first}..{last} (期待 10..11 ±1)"
    print("  [OK] 暗い画素は範囲を広げない")

    print("\n=== 全部黒のラインは空の範囲になる ===")
    # 行自体は落とさない。落とすと Viewer が「黒い行」と「届かなかった行」を
    # 区別できず、前フレームの内容が残る。
    bright = [(x, 200) for x in range(19, 27)]
    got = run([bright, [], bright, [], bright, []])
    kinds = [(f, l) for _r, f, l in got]
    print(f"  {kinds}")
    empty = [(f, l) for f, l in kinds if f > l]
    filled = [(f, l) for f, l in kinds if f <= l]
    assert empty, f"全黒の行が空範囲になっていない: {kinds}"
    assert filled, f"明るい行が落ちている: {kinds}"
    for f, l in filled:
        assert near(f, 10) and near(l, 13), f"範囲が不正: {f}..{l}"
    print(f"  [OK] 明部の行 {len(filled)}本は範囲あり、全黒の行 {len(empty)}本は空範囲")

    print("\n=== 範囲はラインごとに独立 ===")
    got = run([
        [(x, 200) for x in range(19, 27)],    # entry 10..13
        [(x, 200) for x in range(39, 47)],    # entry 20..23
        [(x, 200) for x in range(19, 27)],
        [(x, 200) for x in range(39, 47)],
    ])
    seen = [(f, l) for _r, f, l in got]
    print(f"  {seen}")
    assert any(near(f, 10) and near(l, 13) for f, l in seen), \
        f"前半の範囲が見つからない: {seen}"
    assert any(near(f, 20) and near(l, 23) for f, l in seen), \
        f"前のラインの範囲を引きずっている: {seen}"
    print("  [OK] ラインごとに独立している")

    print("\nALL OK")


if __name__ == "__main__":
    main()
