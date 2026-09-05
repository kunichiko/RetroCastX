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
        # TVPが測るフレーム当たりライン数。方式1はVSOUTがフレームに1回なので
        # VSOUT間の行数(VTOTAL)とフレーム当たり行数が一致する → il_frame=1
        yield cap.cfg_lpf_tvp.eq(VTOTAL)
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
    ★2026-09-03 に**ラインバッファの表現が変わった**。以前は RGB555 に
      変換済みの16bitを2つ詰めた32bitだったが、いまは 8bit の生値
      (byte0=R byte1=G byte2=B)を32bitスロットに入れた64bit。
      伝送形式への変換は送出側で行う。

      rd_data = {奇数px(32bit), 偶数px(32bit)}
      偶数px の G は bit15:8 なので、その上位5bit = bit15:11 が tag。
    """
    return (px[2] >> 11) & 0x1F


LINE_CYCLES = 12          # 方式2の検証で使う1ラインの長さ[pixクロック]
VT2 = 8                   # 方式2の1フィールドあたり行数
# 方式2の有効行数。**VT2/2 より大きくすること。**
# 折り返し点は vtotal/2 = VT2/2 = 4 なので、有効行がそこを超えていないと
# 「誤って折り返す」不具合がテストに現れない(以前 VACT=4 でちょうど境界に
# 乗っており、il_frame を間違えても結果が変わらなかった)。
VACT2 = 6


def run_mode2(nframes=6):
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
        # 方式2はVSOUTがフィールドごとに来るので、VSOUT間の行数(VT2)に対して
        # TVPのフレーム当たりライン数はその2倍になる → il_frame=0(折り返さない)。
        # ここを与えないとフォールバックで「フレーム単位」と誤判定し、
        # 折り返し点 VT2/2 で1フィールドを真ん中から割ってしまう
        # (実機のMSXインターレースで絵がスカスカのストライプになった症状)
        yield cap.cfg_lpf_tvp.eq(VT2 * 2)
        yield cap.cfg_hs_total.eq(LINE_CYCLES)
        yield cap.cfg_vtotal.eq(VT2)
        yield cap.cfg_vs_min_rows.eq(VT2 - 2)
        yield cap.cfg_vs_row_at_sync.eq(0)
        yield cap.cfg_vactive.eq(VACT2)
        for _ in range(5):
            yield
        for f in range(nframes):
            # 偶数フィールドはVSYNCをラインの先頭付近、奇数は中央付近に置く
            vs_at = 0 if f % 2 == 0 else LINE_CYCLES // 2
            for r in range(VT2):
                tag = r if r < VACT2 else None
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


# --- 生同期からのインターレース判定(2026-09-03 追加) ---
#
# ★**これが無かったせいで実機が壊れた。** X68000 24kHz 1024x848 で、TVPの
#   P/I検出(38h bit5)が progressive と誤答した(実測 0x57=0x21。reg22h bit0 を
#   立ててVSOUTをフィールドごとにしても変わらず)。il_det が 0 だと fld_pos も
#   in_f2 も 0 に固定されるので織り込みが丸ごと止まり、2枚のフィールドが上下に
#   積まれた絵になった。
#
#   生VSYNCの半ライン位相はそのとき 715/11 を 106:94 で交互に取っていて、判定
#   材料としては完璧だった。ここではその経路だけでインターレースと判定できるかを
#   確かめる。**cfg_il_detect は 0 のまま**(=TVPが誤答している状況の再現)。
RAW_LINE = 96         # 1ラインの長さ[pixクロック]。raw_ok の下限64を超えること
RAW_VT = 16           # TVPのVSOUT間の行数(2フィールド分)
RAW_F2 = RAW_VT // 2  # 第2フィールドの先頭 row
RAW_VACT = 4          # フィールドあたりの有効行数


class _PadsRaw(_Pads):
    """生HSYNC/生VSYNCを持つ基板(v0.9.0)相当。"""

    def __init__(self):
        super().__init__()
        self.hs_raw = Signal(reset=1)
        self.vs_raw = Signal(reset=1)


class WrapRaw(Module):
    def __init__(self):
        self.clock_domains.cd_sys = ClockDomain()
        self.clock_domains.cd_pix = ClockDomain()
        self.pads = _PadsRaw()
        self.submodules.cap = TvpCapture(
            self.pads, width=W, height=RAW_VT, nface=8,
            vtotal=RAW_VT, vs_min_rows=RAW_VT - 2, vs_offset=0, hs_offset=0,
            vs_row_at_sync=0)


def run_raw_il(nframes=10):
    """TVPはprogressiveと誤答。生VSYNCの半ライン位相だけで織り込ませる。"""
    dut = WrapRaw()
    p = dut.pads
    cap = dut.cap
    got = []

    def hline(tag=None, vs_raw_at=None):
        """1ライン。TVPのHSYNCと生HSYNCを同時に立てる。

        vs_raw_at を与えると、行内のその位置で生VSYNCを1クロック落とす。
        フィールドごとにこの位置を変えるのが半ライン位相そのもの。
        """
        yield p.hs.eq(0); yield p.hs_raw.eq(0); yield
        yield p.hs.eq(1); yield p.hs_raw.eq(1); yield
        for i in range(RAW_LINE - 2):
            if vs_raw_at is not None and i == vs_raw_at:
                yield p.vs_raw.eq(0)
            if vs_raw_at is not None and i == vs_raw_at + 1:
                yield p.vs_raw.eq(1)
            if tag is not None and i < W:
                yield p.r.eq((i << 3) & 0xFF)
                yield p.g.eq((tag << 3) & 0xFF)
                yield p.b.eq(0)
            else:
                yield p.r.eq(0); yield p.g.eq(0)
            yield

    def vpulse():
        """TVPのVSOUT。フレーム(2フィールド)に1回しか来ない。"""
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
        # ★**TVPは「progressive」と言っている。** これが実機で起きたこと
        yield cap.cfg_il_detect.eq(0)
        # TVPのVSOUTはフレームに1回 → cfg_vtotal が2フィールドを覆う → il_frame=1
        yield cap.cfg_lpf_tvp.eq(RAW_VT)
        yield cap.cfg_hs_total.eq(RAW_LINE)
        yield cap.cfg_vtotal.eq(RAW_VT)
        yield cap.cfg_vactive.eq(RAW_VACT)
        for _ in range(5):
            yield
        yield from vpulse()
        for _ in range(4):
            yield
        for _ in range(nframes):
            for r in range(RAW_VT):
                if r < RAW_VACT:
                    tag = r                            # 第1フィールド
                elif RAW_F2 <= r < RAW_F2 + RAW_VACT:
                    tag = 8 + (r - RAW_F2)             # 第2フィールド
                else:
                    tag = None
                # 生VSYNCはフィールドごと。片方は行頭、もう片方は行の中央
                vs_at = 0 if r == 0 else (RAW_LINE // 2 if r == RAW_F2 else None)
                yield from hline(tag, vs_at)
                yield from drain()
            yield from vpulse()
            for _ in range(4):
                yield
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
    return groups


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

    print("\n=== 方式2: VSOUTがフィールドごとに来る(MSX等、C-SYNCをSOGで受ける機種) ===")
    # このときVSOUT間の行数は1フィールド分なので、折り返してはいけない。
    # 折り返すか否かは cfg_lpf_tvp(TVPのフレーム当たりライン数)との比で決まる。
    for frame, lines in run_mode2():
        rows = [r for r, _ in lines]
        print(f"  field {frame}: slots {rows}")
        # 1フィールド分の行が、折り返されずに等間隔(2つ飛び)で並ぶこと。
        # 誤って折り返すと frow が途中で0へ戻り、スロットが重複/逆行する。
        assert len(rows) == VACT2, f"行数が合わない: {rows} (期待 {VACT2}行)"
        assert rows == sorted(rows), f"スロットが逆行している(折り返しの疑い): {rows}"
        assert all(b - a == 2 for a, b in zip(rows, rows[1:])), \
            f"スロットが等間隔でない(折り返しの疑い): {rows}"
        # 折り返しの検出はスロット側(等間隔・単調増加)が担う。折り返すと
        # frow が途中で0へ戻り、スロットが 0,2,4,6,1,3 のように逆行する。
        # 中身は「1フィールド分の全行が重複なく揃っているか」を見る。
        # (並びが1つ回るのは、VSYNCを行0の途中で立てる模擬のためで折り返しではない)
        tags = [tag_of(px) for _, px in lines]
        assert sorted(tags) == list(range(VACT2)), \
            f"1フィールド分の行が揃っていない: {tags}"
        print(f"           中身 tag {tags}")
    print(f"  [OK] 折り返さずに1フィールド{VACT2}行が等間隔・連番で並ぶ")

    print("\n=== 生同期の半ライン位相だけで織り込む(TVPはprogressiveと誤答) ===")
    # 判定はヒステリシス付きカウンタなので、立ち上がるまで十数フィールドかかる。
    # 後半のフレームだけを見る
    groups = run_raw_il()
    assert len(groups) >= 4, f"検証可能なフレームが足りない: {len(groups)}"
    checked = 0
    for frame, lines in groups[-3:-1]:
        rows = [r for r, _ in lines]
        tags = [tag_of(px) for _, px in lines]
        print(f"  frame {frame}: slots {rows}  中身 tag {tags}")
        assert len(rows) == 2 * RAW_VACT, f"行数が合わない: {rows}"
        assert sorted(rows) == list(range(2 * RAW_VACT)), (
            f"織り込まれていない: {rows} "
            f"(期待 0..{2 * RAW_VACT - 1} が1つずつ)")
        # 2枚のフィールドが**別々の偶奇**に入っていること。どちらが偶数側かは
        # 位相の物理で決まるので固定しない
        p1 = {r & 1 for r in rows[:RAW_VACT]}
        p2 = {r & 1 for r in rows[RAW_VACT:]}
        assert len(p1) == 1 and len(p2) == 1 and p1 != p2, (
            f"フィールドが同じ偶奇に入っている: {rows}")
        # 中身: 第1フィールドは tag 0..3、第2フィールドは 8..11 の順
        assert tags[:RAW_VACT] == list(range(RAW_VACT)), f"第1フィールドの中身: {tags}"
        assert tags[RAW_VACT:] == [8 + k for k in range(RAW_VACT)], \
            f"第2フィールドの中身: {tags}"
        checked += 1
    print(f"  [OK] {checked}フレーム: TVPが誤答していても生同期だけで織り込めた")

    print("\nALL OK")


if __name__ == "__main__":
    main()
