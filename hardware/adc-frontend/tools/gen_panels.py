#!/usr/bin/env python3
"""ケース代わりのパネル基板2枚(配線なし・ネジ穴とシルクだけ)の土台を作る。

## 構成

メイン基板と小亀基板を上下に重ね、さらに外側を2枚のパネル基板で挟む:

    パネル(小亀側) | 小亀基板 | メイン基板 | パネル(メイン側)

部品は2枚の基板の**内側(向かい合う面)**に並ぶので、パネルの表(外側)から見ると
各基板の穴位置は**左右反転**して見える。パネルの穴は相手の基板から読み取って
左右反転して置く(手で座標を書かない。基板側の穴を動かしたらこれを再実行する)。

    layouts/panel_console/  小亀基板側のパネル(console-frontend の穴を反転)
    layouts/panel_main/     メイン基板側のパネル(adc-frontend の穴を反転)

外形は 100x100mm、角R3(メイン基板と同じ)。どちらも (100,50)-(200,150) に置く。
穴は M3 用 3.2mm の**非メッキ穴**(銅なし)。シルクのイラストは KiCad 上で描き足す。

## ★再実行すると上書きする

パネルの .kicad_pcb を作り直すので、**シルクを描き始めたら再実行しないこと**。
穴だけ合わせ直したいときは KiCad 上で動かす。

## 使い方 (KiCad 同梱の python で、adc-frontend から)

    /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 tools/gen_panels.py
"""
import json
import pathlib

import pcbnew

ROOT = pathlib.Path(__file__).resolve().parent.parent
HW = ROOT.parent
FP_LIB = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/MountingHole.pretty"
HOLE_FP = "MountingHole_3.2mm_M3"   # 非メッキ 3.2mm(銅なし)

X0, Y0, W, H, R = 100.0, 50.0, 100.0, 100.0, 3.0

PANELS = [
    # (名前, 穴を読む基板, タイトル)
    ("panel_console", HW / "console-frontend/layouts/default/default.kicad_pcb", "RetroCast X Panel (Console side)"),
    ("panel_main", ROOT / "layouts/default/default.kicad_pcb", "RetroCast X Panel (Main side)"),
]
PRO_TEMPLATE = HW / "console-frontend/layouts/default/default.kicad_pro"   # 2層の JLC ルール

mm = pcbnew.FromMM


def holes_mirrored(src_path):
    """基板の取付穴を、基板外形の左上基準で読み、左右反転してパネル座標にする。"""
    b = pcbnew.LoadBoard(str(src_path))
    # 外形の左上は Edge.Cuts の端点から取る(BoundingBox は線幅ぶん外へ膨らみ、
    # 基板ごとに線幅が違うので使わない)
    pts = [p for d in b.GetDrawings() if d.GetLayer() == pcbnew.Edge_Cuts
           for p in (d.GetStart(), d.GetEnd())]
    left = min(p.x for p in pts) / 1e6
    top = min(p.y for p in pts) / 1e6
    out = []
    for f in b.GetFootprints():
        if "MountingHole" in f.GetFPIDAsString():
            rx = round(f.GetPosition().x / 1e6 - left, 3)
            ry = round(f.GetPosition().y / 1e6 - top, 3)
            out.append((f.GetReference(), X0 + (W - rx), Y0 + ry))
    return sorted(out)


def seg(b, x1, y1, x2, y2):
    s = pcbnew.PCB_SHAPE(b, pcbnew.SHAPE_T_SEGMENT)
    s.SetStart(pcbnew.VECTOR2I(mm(x1), mm(y1)))
    s.SetEnd(pcbnew.VECTOR2I(mm(x2), mm(y2)))
    s.SetLayer(pcbnew.Edge_Cuts)
    s.SetWidth(mm(0.05))
    b.Add(s)


def arc(b, cx, cy, sx, sy, deg):
    a = pcbnew.PCB_SHAPE(b, pcbnew.SHAPE_T_ARC)
    a.SetCenter(pcbnew.VECTOR2I(mm(cx), mm(cy)))
    a.SetStart(pcbnew.VECTOR2I(mm(sx), mm(sy)))
    a.SetArcAngleAndEnd(pcbnew.EDA_ANGLE(deg, pcbnew.DEGREES_T), True)
    a.SetLayer(pcbnew.Edge_Cuts)
    a.SetWidth(mm(0.05))
    b.Add(a)


def outline(b):
    x1, y1, x2, y2 = X0, Y0, X0 + W, Y0 + H
    seg(b, x1 + R, y1, x2 - R, y1)
    seg(b, x2, y1 + R, x2, y2 - R)
    seg(b, x2 - R, y2, x1 + R, y2)
    seg(b, x1, y2 - R, x1, y1 + R)
    arc(b, x1 + R, y1 + R, x1, y1 + R, 90)
    arc(b, x2 - R, y1 + R, x2 - R, y1, 90)
    arc(b, x2 - R, y2 - R, x2, y2 - R, 90)
    arc(b, x1 + R, y2 - R, x1 + R, y2, 90)


def make(name, src, title):
    d = ROOT / "layouts" / name
    d.mkdir(parents=True, exist_ok=True)
    pcb = d / f"{name}.kicad_pcb"

    b = pcbnew.CreateEmptyBoard()
    b.SetCopperLayerCount(2)
    tb = b.GetTitleBlock()
    tb.SetTitle(title)
    tb.SetRevision("v1.0.0")
    outline(b)

    holes = holes_mirrored(src)
    for ref, x, y in holes:
        f = pcbnew.FootprintLoad(FP_LIB, HOLE_FP)
        f.SetReference(ref)
        f.Reference().SetVisible(False)
        f.Value().SetVisible(False)
        f.SetPosition(pcbnew.VECTOR2I(mm(x), mm(y)))
        b.Add(f)
    b.Save(str(pcb))

    pro = json.loads(PRO_TEMPLATE.read_text())
    pro["board"]["design_settings"]["drc_exclusions"] = []
    pro["meta"]["filename"] = f"{name}.kicad_pro"
    (d / f"{name}.kicad_pro").write_text(json.dumps(pro, indent=2, ensure_ascii=False) + "\n")

    print(f"{pcb.relative_to(ROOT)}  ({title})")
    for ref, x, y in holes:
        print(f"    {ref}  ({x:.2f}, {y:.2f})  = 外形左上から ({x - X0:.2f}, {y - Y0:.2f})")


if __name__ == "__main__":
    for name, src, title in PANELS:
        make(name, src, title)
