#!/usr/bin/env python3
"""行位置が「VSYNCから何半ライン後か」で決まることを検証(実機不要)。

インターレースの原理そのもので、第2フィールドのVSYNCは半ライン分ずれた位置に来る。
そのフィールドのラインは第1フィールドのラインの物理的に間へ落ちるので、位置を
半ライン単位で決めれば織り込みは設定無しで勝手に成立する。以前は il/f2_row/
swap/field_src の4つで教えていたが、どれも「行番号で並べる」実装の都合から生まれた
もので、信号自体には無かった情報である。

確認すること:
  - VSYNCの位相が交互(ライン先頭/ライン中央)なら、スロットが偶数/奇数に分かれる
  - 位相が一定(プログレッシブ)なら、スロットは1つ飛びに並ぶ
    (空くスロットは受信側が「次のラインまでの間隔」ぶん太らせて埋める)

なお実機のTVP7002はVSOUTの半ライン位相を保たない(24kHz 1024x848 で実測:
生信号はオシロでVSYNCトリガごとにHSYNCが半ラインずれるのに、VSOUTは931ラインに
1パルスしか出さず位相も完全固定。同期制御レジスタ0x0Eを256通り振っても現れず)。
そのため生のHSYNC/VSYNCをFPGAへ直接入れる配線を追加した。
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
        # TVPの検出ビット相当。1 ならフレームを折り返して偶奇へ振り分ける。
        # 折り返し点は常に vtotal/2(F2_ROW = VTOTAL//2 と一致)。手で指定する
        # 設定は撤去した(信号から測れる量なので)
        yield cap.cfg_il_detect.eq(1 if interlace else 0)
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


LINE_CYCLES = 12          # 方式2の検証で使う1ラインの長さ[pixクロック]
VT2 = 8                   # 方式2の1フィールドあたり行数


def run_mode2(swap=0, nframes=5):
    """方式2: VSYNCがフィールドごとに来る。極性はVSYNCの水平位相で決まる。

    片方のフィールドはVSYNCがラインの先頭付近、もう片方は中央付近に来る。
    これが実際のインターレース信号の作りで、受信側はこの位相差でフィールドを
    識別する(TVP7002のFIDOUTも同じ判定をしている)。
    """
    dut = Wrap()
    p = dut.pads
    cap = dut.cap
    got = []

    def hline(tag=None, vs_at=None):
        """1本のライン。vs_at を与えると行内のその位置でVSYNCを立てる。"""
        yield p.hs.eq(0); yield
        yield p.hs.eq(1); yield
        for i in range(LINE_CYCLES - 2):
            if vs_at is not None and i == vs_at:
                yield p.vs.eq(0)
            if vs_at is not None and i == vs_at + 1:
                yield p.vs.eq(1)
            if tag is not None and i < W:
                yield p.r.eq((i << 3) & 0xFF)
                yield p.g.eq((tag << 3) & 0xFF)
                yield p.b.eq(0)
            else:
                yield p.r.eq(0); yield p.g.eq(0)
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
        yield cap.cfg_il_detect.eq(1)
        yield cap.cfg_hs_total.eq(LINE_CYCLES)
        yield cap.cfg_vtotal.eq(VT2)
        yield cap.cfg_vs_min_rows.eq(VT2 - 2)
        yield cap.cfg_vs_row_at_sync.eq(0)
        yield cap.cfg_vactive.eq(VACT)
        for _ in range(5):
            yield
        for f in range(nframes):
            # 偶数フィールドはVSYNCをラインの先頭付近、奇数は中央付近に置く
            vs_at = 0 if f % 2 == 0 else LINE_CYCLES // 2
            for r in range(VT2):
                tag = r if r < VACT else None
                yield from hline(tag, vs_at if r == 0 else None)
                yield from drain()
        for _ in range(20):
            yield
        yield from drain()

    run_simulation(dut, tb(), clocks={"sys": 10, "pix": 10}, vcd_name=None)

    groups = []
    for row, frame, px in got:
        if groups and groups[-1][0] == frame:
            groups[-1][1].append((row, px))
        else:
            groups.append((frame, [(row, px)]))
    return groups[2:-1]


def main():
    print("=== インターレース検出 → フレームを折り返して偶奇へ振り分ける ===")
    # TVPはインターレース入力でも Lines per Frame をフレーム全体で報告し、VSOUTも
    # フレームに1回しか出さない(データシート Table 16: 480i60Hz も 480p60Hz も
    # lines per frame = 525)。よって row は両フィールドを通して数える。折り返し点で
    # 前半/後半に分け、後半を1スロット下へ置けば織り込みになる。
    mid = run(interlace=True)
    assert len(mid) >= 2, f"検証可能なフレームが足りない: {mid}"
    for frame, lines in mid:
        rows = [r for r, _ in lines]
        print(f"  frame {frame}: slots {rows}")
        assert rows == [0, 2, 4, 6, 1, 3, 5, 7], (
            f"織り込まれていない: {rows} "
            f"(期待 第1フィールド 0,2,4,6 → 第2フィールド 1,3,5,7)")
        # 中身の照合。スロット n の中身は tag = (n//2) または 8+(n//2)
        for row, px in lines:
            want = (row >> 1) + (8 if row & 1 else 0)
            assert tag_of(px) == want, (
                f"スロット {row} の中身が不正: tag={tag_of(px)} 期待={want}")
    print(f"  [OK] {len(mid)}フレーム: 2枚のフィールドが交互のスロットへ入り、"
          f"中身も一致")

    print("\n=== 検出が無い(プログレッシブ) → スロットは1つ飛び ===")
    mid = run(interlace=False)
    assert len(mid) >= 2, f"検証可能なフレームが足りない: {mid}"
    for frame, lines in mid:
        rows = [r for r, _ in lines]
        print(f"  frame {frame}: slots {rows}")
        assert rows == [2 * k for k in range(VACT)], \
            f"スロットが1つ飛びになっていない: {rows}"
    print(f"  [OK] スロット 0,2,..,{2 * (VACT - 1)} に並ぶ")

    print("\nALL OK")


if __name__ == "__main__":
    main()
