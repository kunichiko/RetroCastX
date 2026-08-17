#!/usr/bin/env python3
"""発注済み基板のデジグネータを凍結し、以後は欠番を再利用させない。

## なぜ要るか

`ato build` は `keep_designators`(既定true)で **既存部品の番号を .kicad_pcb から読んで
維持する**(`faebryk/libs/app/designators.py` の `load_kicad_pcb_designators`)。
ここまでは望みどおり。

問題は新規部品の採番で、`attach_random_designators` の

    def _get_first_available_number(used):
        \"\"\"Find the first gap in the sequence, or the next number after the highest.\"\"\"

というとおり **最初の空き番号を埋める**。つまり U8 の部品を消すと、次に足した部品が
U8 を名乗る。**発注済み基板の資料・写真・オシロのメモと食い違う**ので避けたい。

このツールは「一度使った番号は二度と使わない」を強制する:

    発注時に      python3 tools/lock_designators.py --freeze v0.9.0
    以後 build 後に python3 tools/lock_designators.py

- ロックにある `atopile_address` は**必ず記録された番号**に戻す
- ロックに無い(=新規)部品は、**接頭辞ごとの最高値+1**を与える。欠番は埋めない
- 与えた番号は最高値として記録され、その部品を後で消しても再利用されない

## renumber_designators.py との使い分け

    renumber_designators.py   宣言順に振り直す。**発注前だけ**。番号が大きく動く
    lock_designators.py       発注後。番号を動かさない

**発注後に renumber_designators.py を走らせてはいけない。**

## ロックファイル

`tools/designator_lock.json`。git に入れて履歴を残すこと。

    {
      "frozen_at": "v0.9.0",
      "map":        { "tvp": "U1", ... },      # atopile_address → デジグネータ
      "high_water": { "U": 17, "C": 91, ... }  # 接頭辞ごとに使い切った最大値
    }

キーが**デジグネータではなく `atopile_address`** なのが要点。部品を差し替えても
回路上の位置が同じなら同じ番号を保てる。
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"
LOCK = ROOT / "tools/designator_lock.json"
SPLIT = re.compile(r"^([A-Za-z]+)(\d+)$")


def read_pcb():
    """(atopile_address, designator, ブロック範囲) を取り出す。"""
    s = PCB.read_text()
    out = []
    for m in re.finditer(r"^\t\(footprint ", s, re.M):
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
        addr = re.search(r'\(property "atopile_address" "([^"]+)"', blk)
        out.append({
            "addr": addr.group(1) if addr else "",
            "ref": ref.group(1),
            "span": (a, j + 1),
            "ref_span": (a + ref.start(1), a + ref.end(1)),
        })
    return s, out


def freeze(tag):
    _, fps = read_pcb()
    m, hw = {}, {}
    for f in fps:
        if not f["addr"]:
            print(f"  ★atopile_address が無い: {f['ref']} (ロック対象外)")
            continue
        m[f["addr"]] = f["ref"]
        sp = SPLIT.match(f["ref"])
        if sp:
            p, n = sp.group(1), int(sp.group(2))
            hw[p] = max(hw.get(p, 0), n)
    LOCK.write_text(json.dumps(
        {"frozen_at": tag, "map": m, "high_water": hw},
        ensure_ascii=False, indent=1, sort_keys=True) + "\n")
    print(f"凍結: {len(m)}部品 / 接頭辞{len(hw)}種を {LOCK.name} に記録 (tag={tag})")
    print("  最大値:", ", ".join(f"{k}{v}" for k, v in sorted(hw.items())))
    return 0


def apply():
    if not LOCK.exists():
        print(f"{LOCK} が無い。まず --freeze <タグ> を実行すること")
        return 1
    lock = json.loads(LOCK.read_text())
    m, hw = lock["map"], lock["high_water"]
    s, fps = read_pcb()

    edits, restored, added = [], [], []
    for f in fps:
        addr = f["addr"]
        if not addr:
            continue
        if addr in m:
            want = m[addr]
            if want != f["ref"]:
                restored.append((f["ref"], want, addr))
        else:
            sp = SPLIT.match(f["ref"])
            if not sp:
                continue
            p = sp.group(1)
            hw[p] = hw.get(p, 0) + 1
            want = f"{p}{hw[p]}"
            m[addr] = want
            added.append((f["ref"], want, addr))
        if want != f["ref"]:
            edits.append((f["ref_span"], want))

    if edits:
        edits.sort()
        out, prev = [], 0
        for (a, b), want in edits:
            out.append(s[prev:a])
            out.append(want)
            prev = b
        out.append(s[prev:])
        PCB.write_text("".join(out))
    LOCK.write_text(json.dumps(lock | {"map": m, "high_water": hw},
                               ensure_ascii=False, indent=1, sort_keys=True) + "\n")

    print(f"ロック適用(tag={lock['frozen_at']}): 書き換え {len(edits)}件")
    for old, new, addr in restored:
        print(f"  戻した   {old:>6s} → {new:<6s} {addr}")
    for old, new, addr in added:
        print(f"  新規採番 {old:>6s} → {new:<6s} {addr}  (欠番は使わない)")
    if not edits:
        print("  変更なし(すべてロックどおり)")
    return 0


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--freeze":
        if len(sys.argv) < 3:
            print("使い方: lock_designators.py --freeze <タグ (例 v0.9.0)>")
            return 1
        return freeze(sys.argv[2])
    return apply()


if __name__ == "__main__":
    sys.exit(main())
