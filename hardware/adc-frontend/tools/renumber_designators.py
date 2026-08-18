#!/usr/bin/env python3
"""main.ato の宣言順にデジグネータを振り直す対応表を作る。

★★**発注後はこれを実行してはいけない。** 番号が大きく動くので、発注済み基板の
   資料・写真・オシロのメモと食い違う。発注後は tools/lock_designators.py を使うこと
   (一度使った番号を二度と使わせない。欠番は欠番のまま)。
   実際 2026-08-18 の CH347F 置換でこれを走らせ、135個の番号が動いた。

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


def _load_pcb(pcb):
    """pcbnew で .kicad_pcb を読み、フットプリントの Reference と atopile_address を返す。"""
    import pcbnew
    b = pcbnew.LoadBoard(str(pcb))
    out = []
    for f in b.GetFootprints():
        a = f.GetFieldByName("atopile_address").GetText() if f.HasFieldByName("atopile_address") else ""
        out.append({"ref": f.GetReference(), "addr": a, "val": f.GetValue()})
    return out


def apply_to_pcb(pcb, mapping):
    """.kicad_pcb の footprint の Reference プロパティだけを書き換える。
    配線・ビア・ゾーン・atopile_address には触らない。"""
    s = pcb.read_text()
    starts = [m.start() for m in re.finditer(r'^\t\(footprint ', s, re.M)]
    out, prev, n, seen = [], 0, 0, set()
    for a in starts:
        d, j = 0, a
        while True:
            if s[j] == '(':
                d += 1
            elif s[j] == ')':
                d -= 1
                if d == 0:
                    break
            j += 1
        blk = s[a:j + 1]
        m = re.search(r'\(property "Reference" "([^"]+)"', blk)
        assert m, "Reference の無いフットプリント"
        old = m.group(1)
        assert old in mapping, f"対応表に無い: {old}"
        assert old not in seen, f"Reference が重複: {old}"
        seen.add(old)
        out.append(s[prev:a])
        out.append(blk[:m.start(1)] + mapping[old] + blk[m.end(1):])
        prev, n = j + 1, n + 1
    out.append(s[prev:])
    pcb.write_text("".join(out))
    return n


def main():
    pcb = ROOT / "layouts/default/default.kicad_pcb"
    fps = _load_pcb(pcb)
    mapping, rows = build_mapping(fps)
    changed = [(a, b) for a, b in mapping.items() if a != b]
    n = apply_to_pcb(pcb, mapping)
    print(f"{n} 個の Reference を書き換えた(変更 {len(changed)} / 不変 {len(mapping) - len(changed)})")
    for r in rows:
        if r["ref"] != r["new"]:
            print(f"   {r['ref']:>5s} → {r['new']:<5s} {r['addr']}")
    return 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
