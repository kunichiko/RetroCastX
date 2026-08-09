#!/usr/bin/env python3
"""インターレース(ウィーブ)の検証(実機不要)。

X68000の 24kHz 1024x848 は VSYNC がフレームに1回しか来ず、その1周期の中に
「縦半分の解像度で画面全体を描いたフィールド」が2枚並ぶ。そのまま送ると同じ絵が
2回出る(実機実測: vtotal 931 に対し絵は 466行周期で繰り返した)。

ここでは vtotal=16 の小フレームで、前半8行=第1フィールド、後半8行=第2フィールド
とし、各フィールドの有効4行が

    フィールド1の行 0,1,2,3 → 出力行 0,2,4,6
    フィールド2の行 0,1,2,3 → 出力行 1,3,5,7

に織り込まれることを確認する。あわせて画素の中身も、その行に流した値と一致する
ことを見る(行番号だけ合っていて中身が入れ替わる取り違えを検出するため)。

cfg_interlace=0 のときは従来どおり(織り込まない)ことも確認する。
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_capture import TvpCapture

W = 8                 # 幅[px]
H = 16                # height(=ウィーブ後の最大行数)
VTOTAL = 16           # 1VSYNCあたりの行数
F2_ROW = 8            # 第2フィールドが始まる row
VACT = 4              # フィールドあたりの有効行数


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
            vs_row_at_sync=0)


def run(interlace: bool, nframes=4):
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    got = []          # (row, frame, [pixels])

    def hline(tag=None):
        """1本のHSYNC + ライン期間。tagがNoneでなければ画素を書く。

        画素は g=tag にして「どのラインの中身か」を後で判別できるようにする。
        """
        yield p.hs.eq(0); yield
        yield p.hs.eq(1); yield
        yield
        if tag is not None:
            for xp in range(W):
                yield p.r.eq((xp << 3) & 0xFF)
                yield p.g.eq((tag << 3) & 0xFF)
                yield p.b.eq(0); yield
            yield p.r.eq(0); yield p.g.eq(0)
        for _ in range(4):
            yield

    def vpulse():
        yield p.vs.eq(0); yield
        yield p.vs.eq(1); yield
        yield

    def drain():
        while (yield cap.line_valid):
            row = (yield cap.line_row)
            frame = (yield cap.line_frame)
            face = (yield cap.line_face)
            px = []
            for word in range(W // 2):
                yield cap.rd_face.eq(face)
                yield cap.rd_word.eq(word)
                yield
                yield
                px.append((yield cap.rd_data))
            got.append((row, frame, px))
            yield cap.line_ack.eq(1); yield
            yield cap.line_ack.eq(0); yield

    def tb():
        yield cap.cfg_interlace.eq(1 if interlace else 0)
        yield cap.cfg_f2_row.eq(F2_ROW if interlace else 0)
        yield cap.cfg_vactive.eq(VACT)
        for _ in range(5):
            yield
        yield from vpulse()
        for _ in range(4):
            yield

        for _ in range(nframes):
            for r in range(VTOTAL):
                # 有効行にだけ画素を流す。tagは「フィールド番号*16 + フィールド内行」
                if r < VACT:
                    tag = r                      # 第1フィールド
                elif F2_ROW <= r < F2_ROW + VACT:
                    tag = 8 + (r - F2_ROW)       # 第2フィールド
                else:
                    tag = None
                yield from hline(tag)
                yield from drain()
            yield from vpulse()
            for _ in range(4):
                yield
            yield from drain()
        for _ in range(20):
            yield
        yield from drain()

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    # frameごとにまとめ、過渡(先頭2つ)と切れ端(末尾)を除く
    groups = []
    for row, frame, px in got:
        if groups and groups[-1][0] == frame:
            groups[-1][1].append((row, px))
        else:
            groups.append((frame, [(row, px)]))
    mid = groups[2:-1]
    return mid


def tag_of(px):
    """ラインの画素から、流した tag を復元する。

    先頭の画素はテストベンチ側の1サイクル遅れ(信号を立ててから実際に入力へ現れる
    までの分)で前のラインの値を拾うので、行の中ほど(px[2]=画素4,5)を見る。
    rd_data = {奇数px, 偶数px} の32bit で、RGB555 の G は bit9:5。
    """
    return (px[2] >> 5) & 0x1F


def main():
    print("=== cfg_interlace=1 (ウィーブ) ===")
    mid = run(interlace=True)
    assert len(mid) >= 2, f"検証可能なフレームが足りない: {mid}"
    for frame, lines in mid:
        rows = [r for r, _ in lines]
        print(f"  frame {frame}: rows {rows}")
        assert rows == [0, 2, 4, 6, 1, 3, 5, 7], (
            f"ウィーブ後の行番号が不正: {rows} "
            f"(期待 第1フィールド 0,2,4,6 → 第2フィールド 1,3,5,7)")
        # 中身の照合: 出力行 n の中身は tag = (n//2) または 8+(n//2)
        for row, px in lines:
            want = (row >> 1) + (8 if row & 1 else 0)
            assert tag_of(px) == want, (
                f"行 {row} の中身が不正: tag={tag_of(px)} 期待={want}")
    print(f"  [OK] {len(mid)}フレーム: 2枚のフィールドが元の行番号に織り込まれ、"
          f"中身も一致")

    print("\n=== cfg_interlace=0 (従来どおり) ===")
    mid = run(interlace=False)
    assert len(mid) >= 2, f"検証可能なフレームが足りない: {mid}"
    for frame, lines in mid:
        rows = [r for r, _ in lines]
        print(f"  frame {frame}: rows {rows}")
        assert rows == list(range(VACT)), \
            f"非インターレース時の行番号が不正: {rows} (期待 0..{VACT-1})"
    print(f"  [OK] 織り込みを無効にすると従来と同じ 0..{VACT-1} が出る")

    print("\nALL OK")


if __name__ == "__main__":
    main()
