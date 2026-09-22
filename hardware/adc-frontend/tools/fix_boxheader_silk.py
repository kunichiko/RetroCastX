#!/usr/bin/env python3
"""EasyEDA から取り込んだ 2x5 ボックスヘッダのシルクを、実際の本体外形に直す。

## 何が問題だったか (2026-09-21)

`ato create part` / `tools/ingest_parts.py` で取り込んだ XFCN BH254V-10P
(LCSC C492442)のフットプリント `IDC-TH_10P-P2.54-V-R2-C5-S2.54.kicad_mod` は、
**F.SilkS に「内側の空洞」を描いていた**。

    メーカー図面(XFCN BH254V-xxP)   本体 20.35 x 8.9mm(内側の空洞 6.5mm)
    取り込んだ F.SilkS               18.03 x 7.45mm  ← 空洞の輪郭
    取り込んだ F.Fab                 20.30 x 8.70mm  ← 本体の実寸(こちらは正しい)

pcbnew では F.SilkS が目立つので「ボックスヘッダにしては小さい」と見える。実害は
**隣の部品との間隔をシルクで見積もると 1.2mm 近く楽観的になる**こと。
(v0.9.0 の J4 で使っていた自作 `parts/_mech/BoxHeader_2x5_P2.54mm.kicad_mod` は
 16.16 x 9.74mm で、長さが 4.2mm 足りずパッドに対して非対称だった。こちらも同罪。)

## 直したもの

    外形     x ±10.175 / y ±4.45(= 20.35 x 8.9mm)の連続した矩形
    キー溝   長辺(+y側)の中央に 4.5mm。**実物は壁の切れ目**で外形は途切れないので、
             壁の内側に 4.5mm の線を1本引いて位置を示す
    ピン1    パッド1(x=-5.08, y=+1.27)の外側に三角マーク
    コートヤード 本体 +0.25mm

★**再取り込みするとベンダー提供のシルクに戻る。** そのときはこのスクリプトを
  もう一度走らせること(tools/fix_esd_land.py と同じ扱い)。

使い方:  python3 tools/fix_boxheader_silk.py
"""
import pathlib
import re
import sys

FP = pathlib.Path(__file__).resolve().parent.parent / \
    "parts/XFCN_BH254V_10P/IDC-TH_10P-P2.54-V-R2-C5-S2.54.kicad_mod"

# メーカー図面の実寸
HX, HY = 20.35 / 2, 8.9 / 2      # 本体の半寸
KEY = 4.5 / 2                    # キー溝の半幅(長辺 +y 側の中央)
CY = 0.25                        # コートヤードの余白

def line(x1, y1, x2, y2, layer, width, uid):
    return (f'\t(fp_line\n\t\t(start {x1} {y1})\n\t\t(end {x2} {y2})\n'
            f'\t\t(stroke\n\t\t\t(width {width})\n\t\t\t(type solid)\n\t\t)\n'
            f'\t\t(layer "{layer}")\n\t\t(uuid "{uid}")\n\t)\n')

def main():
    s = FP.read_text()
    # 既存の F.SilkS / F.CrtYd の線を全部落とす(F.Fab は本体実寸で正しいので残す)
    def drop(layer, text):
        out = []
        for m in re.finditer(r'\t\(fp_(?:line|rect|poly|circle|arc)\b[\s\S]*?\n\t\)\n', text):
            if f'(layer "{layer}")' not in m.group(0):
                out.append(m.group(0))
        # 元のブロックを消して、残すものを戻す
        rest = re.sub(r'\t\(fp_(?:line|rect|poly|circle|arc)\b[\s\S]*?\n\t\)\n', '', text)
        i = rest.index('\t(pad "')
        return rest[:i] + "".join(out) + rest[i:]

    before_silk = len(re.findall(r'\(layer "F.SilkS"\)', s))
    s = drop("F.SilkS", s)
    s = drop("F.CrtYd", s)

    U = "bh254v01-0000-4000-8000"
    g = ""
    # 本体外形(連続した矩形)
    g += line(-HX, -HY,  HX, -HY, "F.SilkS", 0.12, f"{U}-000000000001")
    g += line( HX, -HY,  HX,  HY, "F.SilkS", 0.12, f"{U}-000000000002")
    g += line( HX,  HY, -HX,  HY, "F.SilkS", 0.12, f"{U}-000000000003")
    g += line(-HX,  HY, -HX, -HY, "F.SilkS", 0.12, f"{U}-000000000004")
    # キー溝(壁の切れ目)の位置を示す線。壁の内側 0.9mm に 4.5mm
    g += line(-KEY, HY - 0.9,  KEY, HY - 0.9, "F.SilkS", 0.12, f"{U}-000000000005")
    g += line(-KEY, HY - 0.9, -KEY, HY,       "F.SilkS", 0.12, f"{U}-000000000006")
    g += line( KEY, HY - 0.9,  KEY, HY,       "F.SilkS", 0.12, f"{U}-000000000007")
    # ピン1マーク(パッド1 = x=-5.08, y=+1.27 の外側)
    g += line(-HX - 0.9,  0.5, -HX - 0.9,  2.0, "F.SilkS", 0.25, f"{U}-000000000008")
    g += line(-HX - 0.9,  0.5, -HX - 0.2,  1.25, "F.SilkS", 0.25, f"{U}-000000000009")
    g += line(-HX - 0.9,  2.0, -HX - 0.2,  1.25, "F.SilkS", 0.25, f"{U}-00000000000a")
    # コートヤード
    g += line(-HX-CY, -HY-CY,  HX+CY, -HY-CY, "F.CrtYd", 0.05, f"{U}-000000000011")
    g += line( HX+CY, -HY-CY,  HX+CY,  HY+CY, "F.CrtYd", 0.05, f"{U}-000000000012")
    g += line( HX+CY,  HY+CY, -HX-CY,  HY+CY, "F.CrtYd", 0.05, f"{U}-000000000013")
    g += line(-HX-CY,  HY+CY, -HX-CY, -HY-CY, "F.CrtYd", 0.05, f"{U}-000000000014")

    i = s.index('\t(pad "')
    s = s[:i] + g + s[i:]
    FP.write_text(s)
    print(f"{FP.name}: F.SilkS を {before_silk} 本 → 本体実寸 20.35 x 8.9mm に描き直した")
    print("  ★PCBへ反映するには、j_drgb のフットプリントブロックを消して ato build を2回")

if __name__ == "__main__":
    sys.exit(main())
