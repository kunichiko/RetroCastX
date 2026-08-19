#!/usr/bin/env python3
"""`ato build` が消す pcbnew の設定を復元する。

★**`ato build` を走らせたら必ずこれを実行すること。**
  (tools/restore_tuning_patterns.py と同じ扱い)

## 何が消えるか (2026-08-19 に実証)

    (title_block (rev "v0.9.0"))   リビジョン番号
    (setup (grid_origin 87.5 149)) 基板の原点(グリッド原点)
    (setup (aux_axis_origin ...))  補助原点。設定していれば同様に消えると思われる

`title` は残るのに `rev` だけ消える。atopile の**非テストコードには PCB の
title_block も grid_origin/aux_axis_origin も一切現れない**(title_block は回路図側の
変換にしか出てこない)。モデル化されていない要素が書き出し時に落ちる、という
等長配線オブジェクト(tools/restore_tuning_patterns.py 参照)とまったく同じ構図。

## 証拠

**実測**: 両方が入った状態で `ato build` を実行 → 両方消えた。再現性あり。

**git履歴**: 消失は必ず `ato build` を伴うコミット、復活は pcbnew 作業のコミット。

    47868cf  grid 復活   基板固定用M2.6ネジ穴を追加し…(pcbnew作業)
    ab25097  grid 消失   配線途中のスナップショット
    f916edc  grid 復活   配線途中のスナップショット
    7156ba7  grid 消失   J9の3Dモデルオフセットをライブラリ側へ反映(ato build)
    34391de  grid 復活   配線途中のスナップショット
    715e368  grid 消失   デジグネータを宣言順に振り直す(ato build)
    fa3fe14  grid+rev 復活  v0.9.0の製造データを出した時点を記録(pcbnew作業)
    ac85734  grid+rev 消失  FT2232HL を CH347F に置き換える(ato build)

利用者は4回設定し直しており、そのたびに次の build で失われていた。

## なぜ放置できないか

grid_origin は**ガーバー/ドリルの出力原点**になる。消えたまま製造データを出すと
基板の座標系が変わり、実装機のデータや過去の版と突き合わせできなくなる。
rev は基板シルクの版数表示に出る。

## 使い方

    python3 tools/restore_pcb_settings.py            # 復元(ato build の直後に)
    python3 tools/restore_pcb_settings.py --capture  # 今の値を pcb_settings.json に取り込む

pcbnew で原点やリビジョンを変えたら --capture で取り込み直すこと。
値は tools/pcb_settings.json に置いてgitで追跡する。
"""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PCB = ROOT / "layouts/default/default.kicad_pcb"
CONF = ROOT / "tools/pcb_settings.json"


def read_current(s):
    """PCB から現在値を読む。"""
    tb = re.search(r"\(title_block\b(.*?)\n\t\)", s, re.S)
    cur = {"title": None, "rev": None, "grid_origin": None, "aux_axis_origin": None}
    if tb:
        for k in ("title", "rev"):
            m = re.search(r'\(' + k + r' "([^"]*)"\)', tb.group(1))
            if m:
                cur[k] = m.group(1)
    for k in ("grid_origin", "aux_axis_origin"):
        m = re.search(r"\(" + k + r" ([-\d.]+) ([-\d.]+)\)", s)
        if m:
            cur[k] = [float(m.group(1)), float(m.group(2))]
    return cur


def apply(s, want):
    """欲しい値を PCB テキストへ書き戻す。既にあれば上書き、無ければ挿入。"""
    changed = []

    # --- title_block の title / rev ---
    tb = re.search(r"(\(title_block\b)(.*?)(\n\t\))", s, re.S)
    if tb and (want.get("title") or want.get("rev")):
        body = tb.group(2)
        for k in ("title", "rev"):
            v = want.get(k)
            if not v:
                continue
            pat = re.compile(r'\n\t\t\(' + k + r' "[^"]*"\)')
            new = f'\n\t\t({k} "{v}")'
            if pat.search(body):
                if pat.search(body).group(0) != new:
                    body = pat.sub(new, body, count=1)
                    changed.append(f"{k} を上書き -> {v}")
            else:
                body = body + new
                changed.append(f"{k} を追加 -> {v}")
        s = s[:tb.start()] + tb.group(1) + body + tb.group(3) + s[tb.end():]

    # --- setup の grid_origin / aux_axis_origin ---
    # ★(setup ... ) の直下、(pcbplotparams の直前に置く(KiCad の並びに合わせる)
    for k in ("grid_origin", "aux_axis_origin"):
        v = want.get(k)
        if not v:
            continue
        val = f"\t\t({k} {v[0]:g} {v[1]:g})\n"
        pat = re.compile(r"\t\t\(" + k + r" [-\d.]+ [-\d.]+\)\n")
        if pat.search(s):
            if pat.search(s).group(0) != val:
                s = pat.sub(val, s, count=1)
                changed.append(f"{k} を上書き -> {v}")
        else:
            m = re.search(r"\n(\t\t\(pcbplotparams\b)", s)
            if not m:
                print(f"★(pcbplotparams が見つからず {k} を挿入できない", file=sys.stderr)
                continue
            s = s[:m.start() + 1] + val + s[m.start() + 1:]
            changed.append(f"{k} を追加 -> {v}")
    return s, changed


def main():
    s = PCB.read_text()

    if "--capture" in sys.argv:
        cur = read_current(s)
        cur = {k: v for k, v in cur.items() if v is not None}
        if not cur:
            print("★取り込める設定が PCB に無い。先に pcbnew で設定すること", file=sys.stderr)
            return 1
        CONF.write_text(json.dumps(cur, ensure_ascii=False, indent=1) + "\n")
        print(f"{CONF.name} に取り込んだ:")
        for k, v in cur.items():
            print(f"  {k} = {v}")
        return 0

    if not CONF.exists():
        print(f"{CONF} が無い。まず --capture を実行すること", file=sys.stderr)
        return 1
    want = json.loads(CONF.read_text())
    before = read_current(s)
    s2, changed = apply(s, want)
    if not changed:
        print("変更なし(すべて設定どおり)")
        return 0
    PCB.write_text(s2)
    after = read_current(s2)
    print(f"復元した ({len(changed)}件):")
    for c in changed:
        print(f"  {c}")
    for k in ("title", "rev", "grid_origin", "aux_axis_origin"):
        if want.get(k):
            print(f"  {k:<16} {str(before.get(k)):<18} -> {after.get(k)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
