#!/usr/bin/env python3
"""rgb21_breakout.pretty の自作フットプリントを生成する。

寸法は下の定数だけで決まる。**★の付いた値は写真からの仮置き**で、実測したら
ここを直して再実行する(`python3 tools/gen_footprints.py`)。生成物は
rgb21_breakout.pretty/*.kicad_mod。手で直さないこと(次の再生成で消える)。

PCB 上に置いたフットプリントは、ライブラリを直しても自動では変わらない。
再生成したら PCB エディタで「フットプリントを更新」(Tools → Update Footprints
from Library)を実行する。

座標は KiCad 流(mm、y は下が正)で、すべて**表面(F.Cu)側から見た**値。
"""

from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "rgb21_breakout.pretty"

# --- J1: RGB21 メス(ケーブル側)の半田端子で基板の端を挟む -------------------
# 端子は2列。片方の列が奇数ピン(1,3,…,19 と 21=シェル)、もう片方が偶数ピン
# (2,4,…,20)で、1,2,3,… と列を交互に行き来しながら並ぶ(SCART と同じ並び)。
# 奇数列を裏(B.Cu)、偶数列を表(F.Cu)に割り当てる。逆なら基板を裏返して置けば
# よいので、どちらでも配線は成り立つ。
RGB21_PITCH = 3.81  # 同じ列の隣り合う端子の間隔(SCART の仕様。列どうしは半ピッチ 1.905mm ずれる)
RGB21_PAD_W = 2.0  # ★パッドの幅(端子の板幅 + 余裕)
RGB21_PAD_L = 4.0  # ★パッドの長さ(端子が基板に乗る長さ + 余裕)
RGB21_EDGE_GAP = 0.3  # パッドを基板の端から離す距離(銅と外形のクリアランス)
# 原点 = 基板の端(差し込む辺)の上で、端子列の中央。基板は +y 側に広がる。
# 外形(Edge.Cuts)の差し込む辺をこのフットプリントの y=0 の線に合わせる。

# --- J2: ミニDIN8 ケーブルの芯線を半田付けするパッド ---------------------------
# 表4個・裏4個。番号はミニDIN8 のピン番号。シールド(編組)は J3 に分けた。
# 5番はどこにも繋がない(FRAMEMEISTER の配置で未使用)が、芯線の端を固定する
# 場所として、また5番を使うケーブルに備えてパッドを設けてある。
#
# ★並びはピン番号順。この基板の目的はケーブルを半田付けしやすくすることなので、
#   基板上の配線が交差してでも、芯線を番号順に付けられる方を優先した。
#   表: 1 2 3 4    表から見て左→右
#   裏: 5 6 7 8    表の 1 2 3 4 のちょうど裏(1 の裏が 5、2 の裏が 6 …)
#   → 裏から見ると右→左に 5 6 7 8 と並ぶ。
WIRE_PITCH = 3.0  # パッド中心の間隔
WIRE_PAD_W = 2.0
WIRE_PAD_L = 3.5
WIRE_TOP = ["1", "2", "3", "4"]  # F.Cu: 音声R / 音声L / CSYNC / GND(表から見て左→右)
WIRE_BOTTOM = ["5", "6", "7", "8"]  # B.Cu: 未使用 / B / G / R(表から見た座標で左→右 = 1〜4 の真裏)

# --- J3: シールド(編組)を半田付けするパッド ------------------------------------
# 編組はより合わせると太く、芯線と並べると隣に触れやすいので、別部品の
# スルーホールパッドにした(表裏どちらからでも付けられ、穴に通せば抜けにくい)。
FG_PAD = 3.5  # ★パッドの直径
FG_DRILL = 1.8  # ★穴径(より合わせた編組が通る太さ)

# --- タイラップ用スロット(回路に無い部品。board_only) ------------------------
TIE_SLOT_W = 1.3  # ★タイラップの厚み + 余裕
TIE_SLOT_L = 3.2  # ★タイラップの幅 + 余裕
TIE_SPAN = 9.0  # ★2つのスロットの中心間隔(ケーブル外径 + スロット幅程度)


def font(size=1.0, thick=0.15):
    return f"(effects (font (size {size} {size}) (thickness {thick})))"


def header(name, descr, attr, ref_y, val_y):
    return [
        f'(footprint "{name}"',
        "\t(version 20241229)",
        '\t(generator "rgb21_breakout_gen_footprints")',
        '\t(generator_version "9.0")',
        '\t(layer "F.Cu")',
        f'\t(descr "{descr}")',
        f"\t(attr {attr})",
        f'\t(property "Reference" "REF**" (at 0 {ref_y} 0) (layer "F.SilkS") {font()})',
        f'\t(property "Value" "{name}" (at 0 {val_y} 0) (layer "F.Fab") {font()})',
    ]


def fab_text(text, x, y, layer="F.Fab", size=0.8):
    return f'\t(fp_text user "{text}" (at {x:.3f} {y:.3f} 0) (layer "{layer}") {font(size, 0.12)})'


def rect(x0, y0, x1, y1, layer, width=0.1):
    return (
        f"\t(fp_rect (start {x0:.3f} {y0:.3f}) (end {x1:.3f} {y1:.3f}) "
        f'(stroke (width {width}) (type default)) (fill no) (layer "{layer}"))'
    )


def smd(num, x, y, w, h, side):
    layers = '"F.Cu" "F.Mask"' if side == "F" else '"B.Cu" "B.Mask"'
    return f'\t(pad "{num}" smd rect (at {x:.3f} {y:.3f}) (size {w:.3f} {h:.3f}) (layers {layers}))'


def rgb21_card_edge():
    half = RGB21_PITCH / 2
    x0 = -10 * half  # ピン1 の x(ピン21 が +10*half)
    y = RGB21_EDGE_GAP + RGB21_PAD_L / 2
    lines = header(
        "RGB21_CardEdge",
        "RGB21 female solder tabs straddling the board edge. "
        "Odd pins (1-19, 21=shell) on B.Cu, even pins (2-20) on F.Cu. "
        "Origin = board edge, centre of the tab rows.",
        "smd",
        RGB21_EDGE_GAP + RGB21_PAD_L + 2.2,
        RGB21_EDGE_GAP + RGB21_PAD_L + 3.7,
    )
    for n in range(1, 22):
        x = x0 + (n - 1) * half
        lines.append(smd(n, x, y, RGB21_PAD_W, RGB21_PAD_L, "B" if n % 2 else "F"))
    # 差し込む辺(外形をここに合わせる)
    span = 10 * half + RGB21_PAD_W
    lines.append(
        f"\t(fp_line (start {-span:.3f} 0) (end {span:.3f} 0) "
        '(stroke (width 0.15) (type dash)) (layer "F.Fab"))'
    )
    lines.append(fab_text("board edge", 0, -1.0))
    lines.append(fab_text(f"pitch {RGB21_PITCH} (SCART)", 0, RGB21_EDGE_GAP + RGB21_PAD_L + 5.2))
    # 表裏それぞれ、端のピン番号を書いておく(半田付けのときの目印)
    ly = RGB21_EDGE_GAP + RGB21_PAD_L + 0.8  # パッドの奥(シルクがパッドに掛からない位置)
    for n in (2, 20):
        lines.append(fab_text(str(n), x0 + (n - 1) * half, ly, "F.SilkS"))
    for n in (1, 21):
        lines.append(
            f'\t(fp_text user "{n}" (at {x0 + (n - 1) * half:.3f} {ly:.3f} 0) (layer "B.SilkS") '
            f"(effects (font (size 0.8 0.8) (thickness 0.12)) (justify mirror)))"
        )
    lines.append(")")
    return "\n".join(lines) + "\n"


def minidin8_wire_pads():
    lines = header(
        "MiniDin8_WirePads",
        "Solder pads for the 8 conductors of a cut mini-DIN 8 cable "
        "(FRAMEMEISTER RGB IN pinout). Pins 1-4 on F.Cu, 5-8 on B.Cu directly behind 1-4. Shield is J3.",
        "smd",
        -WIRE_PAD_L / 2 - 3.8,
        WIRE_PAD_L / 2 + 3.8,
    )
    names = {"5": "NC", "1": "AR", "2": "AL", "3": "CS", "4": "GND", "6": "B", "7": "G", "8": "R"}
    for row, side in ((WIRE_TOP, "F"), (WIRE_BOTTOM, "B")):
        x0 = -(len(row) - 1) * WIRE_PITCH / 2
        for i, num in enumerate(row):
            x = x0 + i * WIRE_PITCH
            lines.append(smd(num, x, 0, WIRE_PAD_W, WIRE_PAD_L, side))
            # 番号順に半田付けするので、パッドの脇にピン番号、その外に信号名を書く
            # (芯線の色は買ったケーブル次第なので、色は書けない)
            for text, dy in ((num, 0.8), (names[num], 1.8)):
                if side == "F":
                    lines.append(fab_text(text, x, -WIRE_PAD_L / 2 - dy, "F.SilkS", 0.8))
                else:
                    lines.append(
                        f'\t(fp_text user "{text}" (at {x:.3f} {WIRE_PAD_L / 2 + dy:.3f} 0) '
                        '(layer "B.SilkS") (effects (font (size 0.8 0.8) (thickness 0.12)) (justify mirror)))'
                    )
    lines.append(")")
    return "\n".join(lines) + "\n"


def fg_wire_pad():
    lines = header(
        "FG_WirePad",
        "Through-hole pad for the braided shield of the mini-DIN 8 cable (FG).",
        "through_hole",
        -FG_PAD / 2 - 2.6,
        FG_PAD / 2 + 2.6,
    )
    lines.append(
        f'\t(pad "1" thru_hole circle (at 0 0) (size {FG_PAD:.3f} {FG_PAD:.3f}) '
        f'(drill {FG_DRILL:.3f}) (layers "*.Cu" "*.Mask"))'
    )
    y = FG_PAD / 2 + 1.0
    lines.append(fab_text("FG", 0, -y, "F.SilkS", 0.8))
    lines.append(
        f'\t(fp_text user "FG" (at 0 {y:.3f} 0) (layer "B.SilkS") '
        "(effects (font (size 0.8 0.8) (thickness 0.12)) (justify mirror)))"
    )
    lines.append(")")
    return "\n".join(lines) + "\n"


def cable_tie_slots():
    lines = header(
        "CableTie_Slots",
        "Two NPTH slots for a cable tie that anchors the mini-DIN 8 cable. "
        "Board-only (not in the circuit).",
        "board_only exclude_from_pos_files exclude_from_bom",
        -TIE_SLOT_L / 2 - 1.5,
        TIE_SLOT_L / 2 + 1.5,
    )
    for x in (-TIE_SPAN / 2, TIE_SPAN / 2):
        lines.append(
            f'\t(pad "" np_thru_hole oval (at {x:.3f} 0) (size {TIE_SLOT_W:.3f} {TIE_SLOT_L:.3f}) '
            f'(drill oval {TIE_SLOT_W:.3f} {TIE_SLOT_L:.3f}) (layers "*.Cu" "*.Mask"))'
        )
    # ケーブルの通り道
    lines.append(rect(-TIE_SPAN / 2 + TIE_SLOT_W, -TIE_SLOT_L / 2, TIE_SPAN / 2 - TIE_SLOT_W, TIE_SLOT_L / 2, "F.Fab"))
    lines.append(fab_text("cable", 0, 0, "F.Fab", 0.6))
    lines.append(")")
    return "\n".join(lines) + "\n"


def main():
    OUT.mkdir(exist_ok=True)
    for name, body in (
        ("RGB21_CardEdge", rgb21_card_edge()),
        ("MiniDin8_WirePads", minidin8_wire_pads()),
        ("FG_WirePad", fg_wire_pad()),
        ("CableTie_Slots", cable_tie_slots()),
    ):
        (OUT / f"{name}.kicad_mod").write_text(body)
        print(f"wrote {OUT.name}/{name}.kicad_mod")


if __name__ == "__main__":
    main()
