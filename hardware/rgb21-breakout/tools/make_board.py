#!/usr/bin/env python3
"""rgb21_breakout.kicad_pcb を最初の1回だけ作る(部品とネットを入れた状態)。

KiCad 同梱の Python で動かす(pcbnew モジュールが要る):

    /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 \
        tools/make_board.py build/rgb21_breakout.net

★PCB がすでにあれば何もしない(配置・配線を消さないため)。回路を変えたあとは
  このスクリプトではなく、PCB エディタの File → Import → Netlist… で
  build/rgb21_breakout.net を読み込む(「タイムスタンプで照合」を選ぶ)。

外形(Edge.Cuts)は入れない。シェルを実測してから描く(README「残りの作業」)。
"""

import re
import sys
from pathlib import Path

import pcbnew

HERE = Path(__file__).resolve().parent.parent
PCB = HERE / "rgb21_breakout.kicad_pcb"
LIB = "rgb21_breakout"
PRETTY = HERE / f"{LIB}.pretty"

# 仮の置き場所(mm)。J1 の原点 = 差し込む辺。J2 とタイラップはその奥。
PLACE = {
    "J1": (100.0, 80.0),
    "J2": (100.0, 105.0),
}
TIE = ("H1", "CableTie_Slots", (100.0, 116.0))


def parse_netlist(text):
    comps = []
    for m in re.finditer(
        r'\(comp \(ref "([^"]+)"\)\s*\(value "([^"]*)"\)\s*\(footprint "([^"]+)"\)\s*\(tstamp "([^"]+)"\)',
        text,
    ):
        comps.append({"ref": m.group(1), "value": m.group(2), "footprint": m.group(3), "uuid": m.group(4)})
    nets = {}
    for m in re.finditer(r'\(net \(code "\d+"\) \(name "([^"]+)"\)(.*?)\n    \)', text, re.S):
        nets[m.group(1)] = re.findall(r'\(node \(ref "([^"]+)"\) \(pin "([^"]+)"\)', m.group(2))
    return comps, nets


def main():
    if PCB.exists():
        print(f"{PCB.name} はすでにある。上書きしない(回路の変更は File → Import → Netlist で入れる)")
        return 1
    netlist = Path(sys.argv[1] if len(sys.argv) > 1 else HERE / "build" / "rgb21_breakout.net")
    comps, nets = parse_netlist(netlist.read_text())
    if not comps or not nets:
        print("ネットリストを読めなかった", file=sys.stderr)
        return 1

    board = pcbnew.CreateEmptyBoard()
    board.SetCopperLayerCount(2)

    fps = {}
    for c in comps:
        lib, name = c["footprint"].split(":")
        assert lib == LIB, c["footprint"]
        fp = pcbnew.FootprintLoad(str(PRETTY), name)
        fp.SetFPID(pcbnew.LIB_ID(lib, name))
        fp.SetReference(c["ref"])
        fp.SetValue(c["value"])
        # ネットリストの tstamp と同じ path にしておくと、あとで Import Netlist
        # (タイムスタンプで照合)をしたときに同じ部品として扱われる。kinema の照合もこれを見る。
        fp.SetPath(pcbnew.KIID_PATH(f"/{c['uuid']}"))
        x, y = PLACE[c["ref"]]
        fp.SetPosition(pcbnew.VECTOR2I_MM(x, y))
        board.Add(fp)
        fps[c["ref"]] = fp

    tie_ref, name, (x, y) = TIE
    tie = pcbnew.FootprintLoad(str(PRETTY), name)
    tie.SetFPID(pcbnew.LIB_ID(LIB, name))
    tie.SetReference(tie_ref)
    tie.SetPosition(pcbnew.VECTOR2I_MM(x, y))
    board.Add(tie)

    for net_name, nodes in nets.items():
        ni = pcbnew.NETINFO_ITEM(board, net_name)
        board.Add(ni)
        for ref, pin in nodes:
            pads = [p for p in fps[ref].Pads() if p.GetNumber() == pin]
            assert len(pads) == 1, (ref, pin)
            pads[0].SetNet(ni)

    pcbnew.SaveBoard(str(PCB), board)
    print(f"wrote {PCB.name}: {len(comps)} parts + {tie_ref}, {len(nets)} nets")
    return 0


if __name__ == "__main__":
    sys.exit(main())
