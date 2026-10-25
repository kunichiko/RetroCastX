#!/usr/bin/env python3
"""パネル基板のシルク画像を描くための**下絵 PNG**(外形とネジ穴)を作る。

    layouts/panel_console/artwork/panel_console_template.png
    layouts/panel_main/artwork/panel_main_template.png

**2362x2362px / 600dpi = 100x100mm ちょうど**(1px = 0.0423mm)。画像の端が
基板外形の外接四角形と一致するので、仕上げたシルク画像も同じピクセル数・dpi で
書き出せば、KiCad の Image Converter で 600dpi を指定するだけで実寸になる。
向きは**パネルを外側から見た向き**(F.Silkscreen にそのまま置く向き)。

**黒基板に白シルクで描く前提で、下地は黒(基板色)**。描いてあるもの:

    黒の塗り        基板(ソルダーマスク黒)
    明るい灰の線    基板外形(角R3)。角の外は中間の灰
    中間の灰の塗り  ネジ穴 3.2mm(穴なので基板の外と同じ色)
    赤の破線        シルクを置かない範囲(外形から 0.5mm 内側 / 穴の縁から 0.5mm)
    水色の破線      M3 なべ小ねじの頭(φ5.5mm)。この内側のシルクはねじで隠れる
    暗い灰の線      10mm グリッドと中心線

穴の位置は layouts/panel_*/panel_*.kicad_pcb から読む(gen_panels.py で穴を
動かしたら、これも再実行する)。

使い方 (KiCad 同梱の python で、adc-frontend から):

    /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 tools/gen_panel_templates.py
"""
import math
import pathlib

import pcbnew
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
PANELS = ["panel_console", "panel_main"]

DPI = 600
W_MM = H_MM = 100.0
R_MM = 3.0             # 角R
HOLE_MM = 3.2
KEEPOUT_MM = 0.5       # 外形・穴の縁からシルクを離す量
HEAD_MM = 5.5          # M3 なべ小ねじの頭径
GRID_MM = 10.0

PX = round(W_MM / 25.4 * DPI)   # 2362


def px(v_mm):
    return v_mm * PX / W_MM


def dashed_circle(d, cx, cy, r, color, width, dash_deg=8):
    for a in range(0, 360, dash_deg * 2):
        d.arc([cx - r, cy - r, cx + r, cy + r], a, a + dash_deg, fill=color, width=width)


def dashed_rounded_rect(d, inset, r, color, width, dash=px(1.5)):
    """外形から inset 内側の角丸四角を破線で描く。"""
    x1, y1, x2, y2 = inset, inset, PX - 1 - inset, PX - 1 - inset
    for (ax, ay, bx, by) in [(x1 + r, y1, x2 - r, y1), (x2, y1 + r, x2, y2 - r),
                             (x2 - r, y2, x1 + r, y2), (x1, y2 - r, x1, y1 + r)]:
        length = math.hypot(bx - ax, by - ay)
        n = int(length // (dash * 2)) + 1
        for i in range(n):
            t0, t1 = i * 2 * dash / length, min((i * 2 + 1) * dash / length, 1)
            d.line([ax + (bx - ax) * t0, ay + (by - ay) * t0,
                    ax + (bx - ax) * t1, ay + (by - ay) * t1], fill=color, width=width)
    for (cx, cy, a0) in [(x1 + r, y1 + r, 180), (x2 - r, y1 + r, 270),
                         (x2 - r, y2 - r, 0), (x1 + r, y2 - r, 90)]:
        for a in range(a0, a0 + 90, 20):
            d.arc([cx - r, cy - r, cx + r, cy + r], a, a + 10, fill=color, width=width)


def holes(name):
    b = pcbnew.LoadBoard(str(ROOT / "layouts" / name / f"{name}.kicad_pcb"))
    pts = [p for s in b.GetDrawings() if s.GetLayer() == pcbnew.Edge_Cuts
           for p in (s.GetStart(), s.GetEnd())]
    left = min(p.x for p in pts) / 1e6
    top = min(p.y for p in pts) / 1e6
    return [(f.GetPosition().x / 1e6 - left, f.GetPosition().y / 1e6 - top)
            for f in b.GetFootprints() if "MountingHole" in f.GetFPIDAsString()]


def make(name):
    board = (20, 20, 20)        # 黒基板
    outside = (110, 110, 110)   # 基板の外・穴
    edge = (200, 200, 200)
    keepout = (255, 70, 70)
    head = (80, 200, 255)
    img = Image.new("RGB", (PX, PX), board)
    d = ImageDraw.Draw(img)

    grid = (45, 45, 45)
    for i in range(1, int(W_MM // GRID_MM)):
        v = px(i * GRID_MM)
        d.line([v, 0, v, PX], fill=grid, width=2)
        d.line([0, v, PX, v], fill=grid, width=2)
    c = PX / 2
    d.line([c, 0, c, PX], fill=(70, 70, 70), width=3)
    d.line([0, c, PX, c], fill=(70, 70, 70), width=3)

    # 外形(角の外は灰色で塗って、基板の外だと分かるようにする)
    mask = Image.new("L", (PX, PX), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, PX - 1, PX - 1], radius=px(R_MM), fill=255)
    img.paste(outside, (0, 0), Image.eval(mask, lambda v: 255 - v))
    d.rounded_rectangle([0, 0, PX - 1, PX - 1], radius=px(R_MM), outline=edge, width=4)
    dashed_rounded_rect(d, px(KEEPOUT_MM), px(R_MM - KEEPOUT_MM), keepout, 3)

    for (x, y) in holes(name):
        cx, cy = px(x), px(y)
        r = px(HOLE_MM / 2)
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=outside, outline=edge, width=3)
        dashed_circle(d, cx, cy, px(HOLE_MM / 2 + KEEPOUT_MM), keepout, 3)
        dashed_circle(d, cx, cy, px(HEAD_MM / 2), head, 3)

    out = ROOT / "layouts" / name / "artwork" / f"{name}_template.png"
    out.parent.mkdir(parents=True, exist_ok=True)
    img.save(out, dpi=(DPI, DPI))
    print(f"{out.relative_to(ROOT)}  {PX}x{PX}px @ {DPI}dpi = {W_MM}x{H_MM}mm")


if __name__ == "__main__":
    for n in PANELS:
        make(n)
