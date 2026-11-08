#!/usr/bin/env python3
"""パネル基板のシルク画像(PNG)を、KiCad のシルク(塗りつぶしポリゴン)にして
パネルの .kicad_pcb に入れる。

    layouts/panel_*/artwork/*.png  →  layouts/panel_*/panel_*.kicad_pcb の F.Silkscreen

## 画像の作り方

gen_panel_templates.py の下絵と同じく **100x100mm ちょうど**(600dpi なら 2362px)。
画像の端 = 基板外形の外接四角形。向きはパネルを外側から見た向き。
シルクにする部分の決め方:

- **透明つき PNG**: 不透明な部分がシルク(色は見ない)。おすすめ。
- **透明なし PNG**: 明るい部分がシルク(黒背景に白で描いたもの)。

白背景に白で描いて透明なしで書き出すと、線が区別できないのでエラーにする。

## 入れ方

ロゴ1個ぶんのフットプリント(参照 "ART1"、BOM・座標ファイルから除外)として、
基板の中心に置く。もう入っていれば中身を差し替える(位置はそのまま)。
**KiCad でパネルを開いているときは実行しない**(保存で上書きされる)。

## 使い方 (KiCad 同梱の python で、adc-frontend から)

    PY=/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3
    $PY tools/png_to_silk.py layouts/panel_console/artwork/RetroCastX-panel_console.png layouts/panel_console/panel_console.kicad_pcb

OpenCV(輪郭の取り出し)を使う。KiCad の python に入っていないので、最初に一度:

    $PY -m pip install --user "opencv-python-headless<4.12"
"""
import argparse
import pathlib
import sys

import numpy as np
import pcbnew
from PIL import Image

try:
    import cv2
except ImportError:
    sys.exit('OpenCV がない: $PY -m pip install --user "opencv-python-headless<4.12"')

SIZE_MM = 100.0
REF = "ART1"
MIN_AREA_MM2 = 0.01     # これより小さい点(ゴミ)は捨てる
SIMPLIFY_MM = 0.01      # 輪郭の間引き(JLC のシルク精度より十分細かい)

mm = pcbnew.FromMM


def silk_mask(path):
    im = Image.open(path)
    if im.mode in ("RGBA", "LA") or "transparency" in im.info:
        a = np.array(im.convert("RGBA"))[..., 3]
        if a.min() < 255:
            return a >= 128, "透明度"
    lum = np.array(im.convert("L"))
    m = lum >= 128
    if m.mean() > 0.9:
        sys.exit(f"{path}: 透明がなく、ほぼ全面が明るい(白背景に白?)。"
                 "背景を透明にするか黒にして書き出し直す")
    return m, "明るさ"


def build_polyset(mask):
    h, w = mask.shape
    s = SIZE_MM / w                   # 1px あたり mm(画像の幅 = 100mm)
    contours, hier = cv2.findContours(mask.astype(np.uint8), cv2.RETR_CCOMP,
                                      cv2.CHAIN_APPROX_NONE)
    ps = pcbnew.SHAPE_POLY_SET()
    if hier is None:
        return ps, s
    hier = hier[0]
    eps = SIMPLIFY_MM / s

    def chain(c):
        c = cv2.approxPolyDP(c, eps, True).reshape(-1, 2)
        ch = pcbnew.SHAPE_LINE_CHAIN()
        for x, y in c:
            # ピクセルの角を座標にする(中心は 0.5 ずれる)。画像の中心 = 原点
            ch.Append(mm(float((x + 0.5) * s - SIZE_MM / 2)), mm(float((y + 0.5) * s - SIZE_MM / 2)))
        ch.SetClosed(True)
        return ch, len(c)

    for i, c in enumerate(contours):
        if hier[i][3] != -1:          # 穴は親のところで足す
            continue
        if cv2.contourArea(c) * s * s < MIN_AREA_MM2:
            continue
        outer, n = chain(c)
        if n < 3:
            continue
        idx = ps.AddOutline(outer)
        j = hier[i][2]
        while j != -1:
            if cv2.contourArea(contours[j]) * s * s >= MIN_AREA_MM2:
                hole, n = chain(contours[j])
                if n >= 3:
                    ps.AddHole(hole, idx)
            j = hier[j][0]
    ps.Simplify()
    ps.Fracture()                     # 穴を切れ目でつないだ単純な多角形にする
    return ps, s


def board_center(b):
    pts = [p for d in b.GetDrawings() if d.GetLayer() == pcbnew.Edge_Cuts
           for p in (d.GetStart(), d.GetEnd())]
    return pcbnew.VECTOR2I((min(p.x for p in pts) + max(p.x for p in pts)) // 2,
                           (min(p.y for p in pts) + max(p.y for p in pts)) // 2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("png")
    ap.add_argument("pcb")
    args = ap.parse_args()

    mask, by = silk_mask(args.png)
    ps, s = build_polyset(mask)

    b = pcbnew.LoadBoard(args.pcb)
    old = b.FindFootprintByReference(REF)
    pos = old.GetPosition() if old else board_center(b)
    if old:
        b.Remove(old)

    f = pcbnew.FOOTPRINT(b)
    f.SetReference(REF)
    f.SetValue(pathlib.Path(args.png).stem)
    f.Reference().SetVisible(False)
    f.Value().SetVisible(False)
    f.Reference().SetLayer(pcbnew.F_Fab)
    f.Value().SetLayer(pcbnew.F_Fab)
    f.SetAttributes(pcbnew.FP_EXCLUDE_FROM_BOM | pcbnew.FP_EXCLUDE_FROM_POS_FILES)
    b.Add(f)
    for k in range(ps.OutlineCount()):
        p = pcbnew.PCB_SHAPE(f, pcbnew.SHAPE_T_POLY)
        p.SetPolyShape(pcbnew.SHAPE_POLY_SET(ps.Outline(k)))
        p.SetLayer(pcbnew.F_SilkS)
        p.SetFilled(True)
        p.SetWidth(0)
        f.Add(p)
    f.SetPosition(pos)
    b.Save(args.pcb)

    h, w = mask.shape
    print(f"{args.png}: {w}x{h}px → {SIZE_MM}mm角 (1px = {s * 1000:.1f}µm)、{by}でシルクを判定")
    print(f"  シルク面積 {mask.mean() * 100:.1f}%、ポリゴン {ps.OutlineCount()} 個 → {args.pcb} の {REF}")


if __name__ == "__main__":
    main()
