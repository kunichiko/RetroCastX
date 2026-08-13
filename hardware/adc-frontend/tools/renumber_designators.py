#!/usr/bin/env python3
"""main.ato の宣言順にデジグネータを振り直す対応表を作る。

atopile は keep_designators=True(既定)のとき、.kicad_pcb の Reference プロパティを
正典としてデジグネータを読み込む(faebryk/libs/app/designators.py の
load_kicad_pcb_designators)。つまり PCB 側でリネームすれば ato build はそれを維持する。

並び順は「回路の宣言順」。各フットプリントの atopile_address(例 ft2232.c_osci,
dec_caps[6], ch_g2.r_term)のパスを辿り、各段の宣言行番号を並べたタプルをキーにする。
配列は添字を数値で見る。これで main.ato の上から下へ = ほぼ機能ブロック順に並ぶ。
"""
import re, sys, json, pathlib, collections

ROOT = pathlib.Path(__file__).resolve().parent.parent

# ---- .ato の module/component 定義を全部インデックスする ----
# defs[型名] = {"file":…, "line":…, "attrs":{属性名:(行, 型名)}}
def index_ato(root):
    defs = {}
    for f in sorted(root.rglob("*.ato")):
        if not f.is_file():
            continue
        lines = f.read_text().splitlines()
        cur, cur_indent = None, None
        for i, l in enumerate(lines, 1):
            m = re.match(r'^(\s*)(?:module|component)\s+([A-Za-z_]\w*)\s*:', l)
            if m:
                cur = m.group(2); cur_indent = len(m.group(1))
                defs[cur] = {"file": str(f.relative_to(root)), "line": i, "attrs": {}}
                continue
            if cur is None:
                continue
            if l.strip() and not l.startswith(" " * (cur_indent + 1)):
                cur = None; continue          # ブロックを抜けた
            m = re.match(r'\s+([A-Za-z_]\w*)\s*=\s*new\s+([A-Za-z_]\w*)', l)
            if m and m.group(1) not in defs[cur]["attrs"]:
                defs[cur]["attrs"][m.group(1)] = (i, m.group(2))
    return defs

def sort_key(addr, defs, root_type="App"):
    """atopile_address を (行, 添字, 行, 添字, …) のタプルに変換する。"""
    key, cur = [], root_type
    for seg in addr.split("."):
        m = re.match(r'([A-Za-z_]\w*)(?:\[(\d+)\])?$', seg)
        if not m:
            return (10**9,), f"パース不能: {seg}"
        name, idx = m.group(1), int(m.group(2) or 0)
        info = defs.get(cur)
        if info is None or name not in info["attrs"]:
            return (10**9,), f"{cur} に {name} の宣言が無い"
        line, typ = info["attrs"][name]
        key += [line, idx]
        cur = typ
    return tuple(key), None


def build_mapping(fps):
    """fps: [{"ref","addr",...}] → (mapping, ordered_rows)。ordered_rows は宣言順。"""
    defs = index_ato(ROOT)
    rows, errs = [], []
    for r in fps:
        k, err = sort_key(r["addr"], defs)
        if err:
            errs.append((r["ref"], r["addr"], err))
        rows.append({**r, "key": k})
    if errs:
        raise SystemExit("解決できないアドレス:\n" + "\n".join(map(str, errs)))
    dup = [k for k, n in collections.Counter(tuple(r["key"]) for r in rows).items() if n > 1]
    if dup:
        raise SystemExit(f"ソートキーが重複: {dup}")
    rows.sort(key=lambda r: r["key"])
    counter = collections.Counter()
    mapping = {}
    for r in rows:
        pfx = re.match(r'([A-Za-z]+)', r["ref"]).group(1)
        counter[pfx] += 1
        r["new"] = f"{pfx}{counter[pfx]}"
        mapping[r["ref"]] = r["new"]
    assert len(set(mapping.values())) == len(mapping), "新デジグネータが重複"
    assert len(mapping) == len(fps), "取りこぼし"
    return mapping, rows
