#!/usr/bin/env python3
"""README.md の「主要部品」表を BOM と PCB から作り直す。

## なぜ要るか

この表は 2026-08-13 に「手書きだと実装と食い違う」という理由で生成に切り替えられたが、
**生成が一度きりで再実行されなかった**ため、また食い違った(2026-08-20 発見)。
FT2232HL → CH347F の置き換えで U 系のデジグネータが1つずつ繰り上がったのが原因:

    表の記載                        実際
    U7  FT2232HL-REEL               U7  CH347F(FT2232HL は基板に無い)
    U8  93LC56BT-I/SN(FT2232 EEPROM) 基板に無い
    X2  X322512MSB4SI(12MHz)        X2  X32258MSB4SI(8MHz)
    U10 1473005-4                   U9  1473005-4
    U14,U15 PCM1808PWR              U13,U14
    U16,U17 24AA025E48-I/SN         U15,U16
    U2,U9,U11,U12,U13 SN74LVC2G17   U2,U8,U10,U11,U12
    FB1..FB5                        FB1..FB3(FT2232 用の2個が消えた)

→ ツール化して回路変更のたびに回す。`--check` で CI からも確認できる。

## 使い方

    python3 tools/gen_parts_table.py            # README.md の表を書き換える
    python3 tools/gen_parts_table.py --check    # 食い違いがあれば非0で終了

`ato build` の後、他の追随ツールと一緒に回すこと(README の
「★`ato build` の後に必ず実行すること」節を参照)。

## 表の作り方

    対象       R / C / FID 以外の全部品(= 元の表と同じ範囲。抵抗・コンデンサは除く)
    Ref        デジグネータ。同じ品番はまとめる
    部品       BOM の Partnumber。BOM に無い部品(has_part_removed)はフットプリント名
    メーカー   BOM の Manufacturer。無ければ —
    LCSC       BOM の LCSC Part #。無ければ —
    回路上の位置 PCB の atopile_address プロパティ

★`atopile_address` が `ft2232.*` のままの部品がある。CH347F へ置き換えたときに
  main.ato のインスタンス名 `ft2232 = new UsbProgrammer` を変えなかったため。
  **これは直してはいけない**: tools/lock_designators.py は atopile_address を
  キーにデジグネータを固定しているので、名前を変えると発注済み基板の番号が動く。
"""
import argparse
import csv
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
README = ROOT / "README.md"
BOM = ROOT / "build/builds/default/default.bom.csv"
PCB = ROOT / "layouts/default/default.kicad_pcb"

HEADER = "| Ref | 部品 | メーカー | LCSC | 回路上の位置 |"
SEP = "|---|---|---|---|---|"
SKIP_PREFIX = {"R", "C", "FID"}   # 抵抗・コンデンサ・フィデューシャルは載せない


def split_ref(r):
    m = re.match(r"([A-Za-z]+)(\d+)", r.strip())
    return (m.group(1), int(m.group(2))) if m else (r.strip(), 0)


def read_pcb():
    """{デジグネータ: (atopile_address, フットプリント名)} を PCB から読む。"""
    s = PCB.read_text()
    out = {}
    for m in re.finditer(r'^\t\(footprint "([^"]+)"', s, re.M):
        a = m.start()
        d, j = 0, a
        while True:
            if s[j] == "(":
                d += 1
            elif s[j] == ")":
                d -= 1
                if d == 0:
                    break
            j += 1
        blk = s[a:j + 1]
        ref = re.search(r'\(property "Reference" "([^"]+)"', blk)
        if not ref:
            continue
        addr = re.search(r'\(property "atopile_address" "([^"]+)"', blk)
        fp = m.group(1).split(":")[-1]
        out[ref.group(1)] = (addr.group(1) if addr else "", fp)
    return out


def build_table():
    pcb = read_pcb()
    groups = {}          # key -> {"pn","mfr","lcsc","refs"}
    in_bom = set()

    for x in csv.DictReader(BOM.open()):
        refs = [r.strip() for r in x["Designator"].split(",") if r.strip()]
        in_bom.update(refs)
        refs = [r for r in refs if split_ref(r)[0] not in SKIP_PREFIX]
        if not refs:
            continue
        groups[("bom", x["Partnumber"])] = {
            "pn": x["Partnumber"], "mfr": x["Manufacturer"] or "—",
            "lcsc": x["LCSC Part #"] or "—", "refs": refs,
        }

    # BOM に載らない部品(has_part_removed)はフットプリント名でまとめる
    for ref, (addr, fp) in pcb.items():
        if ref in in_bom or split_ref(ref)[0] in SKIP_PREFIX:
            continue
        g = groups.setdefault(("fp", fp), {"pn": fp, "mfr": "—", "lcsc": "—", "refs": []})
        g["refs"].append(ref)

    rows = []
    for g in groups.values():
        g["refs"].sort(key=split_ref)
        inst = ", ".join(pcb[r][0] for r in g["refs"] if pcb.get(r) and pcb[r][0])
        rows.append({
            "sort": split_ref(g["refs"][0]),
            "line": f'| {", ".join(g["refs"])} | {g["pn"]} | {g["mfr"]} | {g["lcsc"]} | '
                    f'{"`" + inst + "`" if inst else "—"} |',
        })
    rows.sort(key=lambda r: r["sort"])
    return "\n".join([HEADER, SEP] + [r["line"] for r in rows])


def locate(s):
    """README 内の表の範囲 (開始index, 終了index) を返す。"""
    i = s.find(HEADER)
    if i < 0:
        raise SystemExit(f"★README.md に表の見出しが無い: {HEADER}")
    j = i
    for line in s[i:].split("\n"):
        if line.startswith("|"):
            j += len(line) + 1
        else:
            break
    return i, j - 1        # 末尾の改行は含めない


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--check", action="store_true")
    a = p.parse_args()

    s = README.read_text()
    i, j = locate(s)
    old, new = s[i:j], build_table()

    if old == new:
        print("主要部品表は最新(変更なし)")
        return 0

    o, n = old.splitlines(), new.splitlines()
    removed = [l for l in o if l not in n]
    added = [l for l in n if l not in o]
    print(f"{'食い違いあり' if a.check else '書き換えた'}: {len(o)}行 → {len(n)}行 "
          f"(消える {len(removed)} / 増える {len(added)})")
    for l in removed:
        print(f"  - {l}")
    for l in added:
        print(f"  + {l}")
    if a.check:
        print("\n--check なので書き換えていない", file=sys.stderr)
        return 1
    README.write_text(s[:i] + new + s[j:])
    return 0


if __name__ == "__main__":
    sys.exit(main())
