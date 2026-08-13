#!/usr/bin/env python3
"""docs/decoupling-placement.html を .kicad_pcb から生成する。

以前は344行の手書きデータだった。デジグネータを振り直すたび、部品を足し引きするたびに
嘘になるので生成に切り替えた(2026-08-13)。基板が正典。

出す情報は「置くべき場所の希望」ではなく**実際にどれだけ近く置けたかの実測**。
配線が終わった後のレビュー用。

  ref / 値 / パッケージ / 実装面 / 座標
  対象 = そのコンデンサとネットを共有する最も近いIC・コネクタ
  そのネットに繋がる対象側のピン番号
  最近ピアまでの距離[mm]  ← これが本題

注記(設計意図)はアドレス(atopile_address)をキーに tools/decoupling_notes.json から
引く。**デジグネータではなくアドレスをキーにしてあるので、番号を振り直しても壊れない。**

実行:
  /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3 \
      tools/gen_decoupling_html.py
"""
import collections
import html
import json
import math
import pathlib
import re
import sys

import pcbnew

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"
NOTES = ROOT / "tools/decoupling_notes.json"
OUT = ROOT.parent.parent / "docs/decoupling-placement.html"

# 「対象」候補から外す接頭辞(受動部品と機構部品)
PASSIVE = re.compile(r"^(C|R|FB|L|TP|JP|PG|H|LED)\d+$")
GND = {"gnd", "chassis"}


def collect():
    b = pcbnew.LoadBoard(str(PCB))
    net2pads = collections.defaultdict(list)
    for f in b.GetFootprints():
        for p in f.Pads():
            if p.GetNetCode():
                net2pads[p.GetNetname()].append((f, p))

    def addr(f):
        return f.GetFieldByName("atopile_address").GetText() if f.HasFieldByName("atopile_address") else ""

    rows = []
    for f in b.GetFootprints():
        ref = f.GetReference()
        if not ref.startswith("C"):
            continue
        c = f.GetPosition()
        nets = {p.GetNetname() for p in f.Pads() if p.GetNetCode()}
        sig = sorted(nets - GND)
        # 「対象ピンまでの距離」が意味を持つのはパスコン(片側GND・反対側が電源/信号)だけ。
        # 直列コンデンサは抵抗を挟んだ先のICが拾われて無意味な距離になるので分けて扱う。
        # GND-シャーシ間のブリッジも同様。
        if not sig:
            kind = "GND-シャーシ ブリッジ"
        elif nets & GND and len(sig) == 1:
            kind = "パスコン"
        else:
            kind = "直列/結合"
        best = None
        for n in (sig if kind == "パスコン" else []):
            for tf, tp in net2pads[n]:
                if tf.GetReference() == ref or PASSIVE.match(tf.GetReference()):
                    continue
                d = math.hypot((tp.GetPosition().x - c.x) / 1e6, (tp.GetPosition().y - c.y) / 1e6)
                if best is None or d < best[0]:
                    best = (d, tf, n)
        tgt_ref = tgt_pins = tgt_net = ""
        dist = None
        if best:
            dist, tf, tgt_net = best
            tgt_ref = tf.GetReference()
            tgt_pins = ",".join(
                sorted({p.GetNumber() for p in tf.Pads() if p.GetNetname() == tgt_net}, key=lambda x: (len(x), x))
            )
        rows.append(
            {
                "ref": ref,
                "addr": addr(f),
                "val": f.GetValue(),
                "pkg": f.GetFPIDAsString().split(":")[-1],
                "layer": "裏" if f.GetLayer() == pcbnew.B_Cu else "表",
                "pos": (round(c.x / 1e6, 2), round(c.y / 1e6, 2)),
                "net": tgt_net,
                "tgt": tgt_ref,
                "pins": tgt_pins,
                "dist": None if dist is None else round(dist, 2),
                "kind": kind,
                "nets": ", ".join(sorted(nets)),
            }
        )
    # 対象IC名(表示用)を集める
    names = {}
    for f in b.GetFootprints():
        names[f.GetReference()] = (addr(f), f.GetFPIDAsString().split(":")[-1])
    return rows, names


def main():
    rows, names = collect()
    notes = json.loads(NOTES.read_text()) if NOTES.exists() else {}
    groups = collections.defaultdict(list)
    for r in rows:
        n = notes.get(r["addr"], {})
        # 注記は「値とパッケージが基板と一致するものだけ」引き継ぐ。
        # 一致しないものは部品が差し替わっている可能性が高く、流用すると嘘になる。
        # 値は "33pF ±5%" と "33pF"、パッケージは "C0402" と "0402" のように表記が違うので
        # 先頭の値トークンとサイズの数字だけで緩く比べる
        def vtok(v):
            m = re.match(r"\s*([\d.]+\s*[a-zA-Zµ]*)", v or "")
            return (m.group(1).replace(" ", "") if m else "").lower()
        def ptok(v):
            m = re.search(r"(\d{4})", v or "")
            return m.group(1) if m else ""
        ok = vtok(n.get("val")) == vtok(r["val"]) and ptok(n.get("pkg")) == ptok(r["pkg"])
        r["pri"] = n.get("pri", 0) if ok else 0
        r["note"] = n.get("note", "") if ok else ""
        r["stale"] = bool(n) and not ok
        groups[r["tgt"]].append(r)

    # グループの並びは main.ato の宣言順(デジグネータの並びと同じ)にする。
    # 辞書順だと adc_aux が先頭に来て、上から読んでも回路の流れにならない。
    sys.path.insert(0, str(ROOT / "tools"))
    from renumber_designators import index_ato, sort_key
    defs = index_ato(ROOT)
    def gkey(t):
        a = names.get(t, ("", ""))[0]
        k, err = sort_key(a, defs) if a else ((10**9,), "no addr")
        return (k if not err else (10**9,), t)
    order = sorted(groups, key=gkey)
    esc = lambda s: html.escape(str(s), quote=True)
    G = []
    for t in order:
        rs = sorted(groups[t], key=lambda r: (r["dist"] is None, -(r["dist"] or 0)))
        a, fp = names.get(t, ("", ""))
        G.append(
            {
                "ref": t or "(対象不明)",
                "name": a,
                "meta": fp,
                "rows": [
                    [r["ref"], r["val"], r["pkg"], r["layer"], f'{r["pos"][0]}, {r["pos"][1]}',
                     r["net"] or r["nets"], r["pins"], r["dist"], r["pri"], r["note"], r["addr"],
                     r["stale"], r["kind"]]
                    for r in rs
                ],
            }
        )
    n_stale = sum(1 for r in rows if r["stale"])
    n_note = sum(1 for r in rows if r["note"])
    tmpl = (ROOT / "tools/decoupling_template.html").read_text()
    OUT.write_text(
        tmpl.replace("__DATA__", json.dumps(G, ensure_ascii=False))
        .replace("__NCAP__", str(len(rows)))
        .replace("__NNOTE__", str(n_note))
        .replace("__NSTALE__", str(n_stale))
    )
    print(f"{OUT} を生成: コンデンサ{len(rows)}個 / 対象{len(order)}グループ")
    print(f"  設計意図の注記を引き継げたもの: {n_note}件")
    print(f"  旧注記があるが値/パッケージが変わっていて引き継がなかったもの: {n_stale}件")
    kinds = collections.Counter(r["kind"] for r in rows)
    print(f"  種別: {dict(kinds)}")
    far = [r for r in rows if r["dist"] is not None and r["dist"] > 3.0]
    print(f"  ★パスコンで対象ピンから3mm超のもの: {len(far)}件")
    for r in sorted(far, key=lambda r: -r["dist"])[:12]:
        print(f"     {r['ref']:5s} {r['dist']:5.2f}mm  {r['net']:14s} → {r['tgt']:5s} pin {r['pins'][:30]}  ({r['addr']})")


if __name__ == "__main__":
    sys.exit(main())
