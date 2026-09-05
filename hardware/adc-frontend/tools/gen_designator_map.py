#!/usr/bin/env python3
"""docs/designator-map.md の対応表を実装から作り直す。

## なぜ要るか

この表は手書きのままで、**実装と全面的に食い違っていた**(2026-09-03 発見。
195行中129行の番号が違い、さらに設計から消えた部品の行が17残っていた)。
デジグネータの振り直しで番号が繰り上がったのに表を追随させていなかったため。

実害が出た: 音声が鳴らない件の切り分けで「アナログ5Vのバルクコンデンサ」を
この表から C69 と読んだが、実装では **C60**(C69 は別の100nF)だった。
**ハードのデバッグ中に部品番号を間違えると、測る場所を間違える。**

`tools/gen_parts_table.py` が同じ理由で 2026-08-13 に生成化されている
(その docstring に「生成が一度きりで再実行されなかったのでまた食い違った」と
記録がある)。この表も同じ扱いにする。

## 使い方

    python3 tools/gen_designator_map.py           # 表を書き換える
    python3 tools/gen_designator_map.py --check   # 食い違いがあれば非0で終了

`ato build` の後、他の追随ツールと一緒に回すこと。

## 表の作り方

- **現在の番号**: `.kicad_pcb` の Reference。`ato build` がこれを読んで維持するので
  実装の正典。`tools/designator_lock.json` は**行の並び**(main.ato の宣言順)と
  照合にだけ使い、PCB と食い違ったら警告して PCB を採る。
- **値**: `.kicad_pcb` の各フットプリントの Value。**BOMからは引かない**。
  `build/` の BOM はビルド成果物で古いことがあり(実際 2026-08-09 のまま残っていて、
  その後のデジグネータ振り直しを反映していなかった)、**古いBOMのデジグネータで
  現在の対応表を引くと値がずれる**。PCB なら Reference / Value / atopile_address が
  同じフットプリントに入っているので、ファイル間のずれが原理的に起きない。
- **V0列**: 手組み試作機の番号で、実装からは導けない**歴史的な情報**。
  既存の表から atopile_address をキーに引き継ぐ。引き継げないものは空欄。
"""
import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOCK = ROOT / "tools/designator_lock.json"
PCB = ROOT / "layouts/default/default.kicad_pcb"
DOC = ROOT / "docs/designator-map.md"

HEADER = "| V0 | 現在 | 回路上の位置 | 値 |"
SEP = "|---|---|---|---|"
ROW_RE = re.compile(r"^\|\s*([^|]*?)\s*\|\s*\*\*([A-Za-z]+\d+)\*\*\s*\|\s*`([^`]+)`\s*\|")


def load_pcb():
    """atopile_address → (デジグネータ, 値)。PCBが実装の正典。"""
    out = {}
    text = PCB.read_text(encoding="utf-8")
    for blk in re.split(r"\n\s*\(footprint ", text)[1:]:
        ref = re.search(r'"Reference"\s+"([^"]+)"', blk)
        val = re.search(r'"Value"\s+"([^"]*)"', blk)
        addr = re.search(r'"atopile_address"\s+"([^"]+)"', blk)
        if ref and addr:
            v = (val.group(1) if val else "").replace("ℝ+V", "").strip()
            out[addr.group(1)] = (ref.group(1), v)
    return out


def load_v0():
    """既存の表から V0 列を引き継ぐ(実装からは導けないため)。"""
    out = {}
    if not DOC.exists():
        return out
    for line in DOC.read_text(encoding="utf-8").splitlines():
        m = ROW_RE.match(line)
        if m:
            v0, _cur, addr = m.group(1), m.group(2), m.group(3)
            if v0 and v0 != "—":
                out[addr] = v0
    return out


def build_rows():
    lock = json.loads(LOCK.read_text(encoding="utf-8"))["map"]
    pcb, v0 = load_pcb(), load_v0()
    # 行の並びは lock の順(= main.ato の宣言順)。PCB にしか無いものは末尾に足す。
    order = [a for a in lock if a in pcb] + [a for a in pcb if a not in lock]
    rows = [HEADER, SEP]
    for addr in order:
        ref, val = pcb[addr]
        if addr in lock and lock[addr] != ref:
            print("警告: %s は lock=%s だが PCB=%s。PCB を採る"
                  % (addr, lock[addr], ref), file=sys.stderr)
        rows.append("| %s | **%s** | `%s` | %s |"
                    % (v0.get(addr, ""), ref, addr, val or "—"))
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true",
                    help="書き換えず、食い違いがあれば非0で終了する")
    args = ap.parse_args()

    lines = DOC.read_text(encoding="utf-8").splitlines()
    try:
        start = lines.index(HEADER)
    except ValueError:
        sys.exit("表の見出し行が見つかりません: %s" % HEADER)
    end = start + 2
    while end < len(lines) and lines[end].startswith("|"):
        end += 1

    new = build_rows()
    if lines[start:end] == new:
        print("designator-map.md: 実装と一致")
        return
    if args.check:
        old = {m.group(3): m.group(2)
               for m in (ROW_RE.match(l) for l in lines[start:end]) if m}
        cur = {m.group(3): m.group(2)
               for m in (ROW_RE.match(l) for l in new) if m}
        bad = [(a, o, cur.get(a)) for a, o in old.items() if cur.get(a) != o]
        print("designator-map.md が実装と食い違っています "
              "(番号違い %d / 表にしか無い部品 %d)"
              % (len([b for b in bad if b[2]]), len([b for b in bad if not b[2]])))
        for a, o, c in bad[:10]:
            print("  %-28s 表=%-6s 実装=%s" % (a, o, c or "(設計に無い)"))
        sys.exit(1)

    DOC.write_text("\n".join(lines[:start] + new + lines[end:]) + "\n",
                   encoding="utf-8")
    print("designator-map.md を更新: %d 行" % (len(new) - 2))


if __name__ == "__main__":
    main()
