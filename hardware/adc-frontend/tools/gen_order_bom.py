#!/usr/bin/env python3
"""手半田実装用の発注リストを DigiKey / Mouser / LCSC 向けに書き出す。

## なぜ要るか

`ato build` が出す `build/builds/default/default.bom.csv` は **JLCPCB 発注用**で、
LCSC 品番が主役になっている。この基板は中国国内ブランド(UNI-ROYAL の抵抗、FH の
コンデンサ、HANRUN の RJ45、YXC の水晶など)を多用しているので、そのまま DigiKey や
Mouser の BOM ツールに投げても**半分近くが「該当なし」で落ちる**。

このツールは `tools/order_sourcing.json` の読み替え表を当てて、

    orders/<tag>/digikey-bom.csv   DigiKey myLists にアップロードする用
    orders/<tag>/mouser-bom.csv    Mouser BOM ツールにアップロードする用
    orders/<tag>/lcsc-bom.csv      LCSC でしか買えない分
    orders/<tag>/README.md         人間が読む用(注意点つき)

を出す。DigiKey / Mouser とも、アップロード時に**列の対応を画面で選べる**ので
ヘッダ名は厳密でなくてよい。照合は製造元品番(MPN)で行われる。

## 使い方

    python3 tools/gen_order_bom.py --tag v0.9.0            # 1枚分
    python3 tools/gen_order_bom.py --tag v0.9.0 --boards 5 # 5枚分

## 予備の考え方

手半田なので 0402 は飛ばす・焦がすことを前提に多めに積む(`spare` 区分は
order_sourcing.json 参照)。高価な部品(TVP7002, SO-DIMM ソケット, RJ45)は
予備なしにしてあるので、必要なら手で足すこと。

★読み替え表に無い LCSC 品番があるとエラーで止まる。回路を変えたら
  order_sourcing.json も更新すること(黙って落とさないための仕掛け)。
"""
import argparse
import csv
import json
import math
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOM = ROOT / "build/builds/default/default.bom.csv"
TABLE = ROOT / "tools/order_sourcing.json"


def clean(text, limit=90):
    """CSV の1セルへ入れるために改行を潰して短く切る。

    ★注記に改行入りの長文を書いたら CSV が壊れた(2026-08-19)。
      order_sourcing.json の note は人間向けに長く詳しく書くので、
      CSV へはそのまま流さない。詳細は README 側に出る。
    """
    t = " ".join(str(text or "").split())
    return t[:limit - 1] + "…" if len(t) > limit else t


def with_spares(qty, kind):
    """手半田用の予備を足す。"""
    if kind == "passive":
        return qty + max(10, math.ceil(qty * 0.2))
    return qty + {"cheap": 2, "normal": 1, "exact": 0}.get(kind, 1)


def collect(boards):
    tbl = json.loads(TABLE.read_text())
    parts = tbl["parts"]
    rows, unknown = [], []

    for x in csv.DictReader(BOM.open()):
        lcsc = x["LCSC Part #"].strip()
        info = parts.get(lcsc)
        if info is None:
            unknown.append((lcsc, x["Manufacturer"], x["Partnumber"]))
            continue
        base = int(x["Quantity"]) * boards
        rows.append({
            "refs": x["Designator"],
            "qty_board": int(x["Quantity"]),
            "qty": with_spares(base, info.get("spare", "normal")),
            "status": info["status"],
            # sub なら読み替え先、そうでなければ元の製造元/品番
            "mfr": info.get("alt_mfr") or x["Manufacturer"],
            "mpn": info.get("alt_mpn") or x["Partnumber"],
            "orig_mfr": x["Manufacturer"],
            "orig_mpn": x["Partnumber"],
            "lcsc": lcsc,
            "desc": info.get("desc", x["Value"]),
            "note": info.get("note", ""),
        })

    if unknown:
        print("★読み替え表(tools/order_sourcing.json)に無い部品があります:", file=sys.stderr)
        for lcsc, mfr, mpn in unknown:
            print(f"    {lcsc:<12} {mfr} {mpn}", file=sys.stderr)
        print("  追記してから再実行してください。", file=sys.stderr)
        sys.exit(1)

    for e in tbl["extra"]:
        rows.append({
            "refs": e["ref"], "qty_board": e["qty"], "qty": e["qty"] * boards,
            "status": e["status"], "mfr": e.get("mfr", ""), "mpn": e["mpn"],
            "orig_mfr": e.get("mfr", ""), "orig_mpn": e["mpn"], "lcsc": "",
            "desc": e["desc"], "note": e.get("note", ""),
        })
    return rows


def write_csvs(rows, out):
    # ★アップロード用 CSV には実在する製造元品番だけを載せる。汎用ヘッダやネジを
    #   「ピンヘッダ 1x6」のような説明文で混ぜると、その行が「該当なし」になって
    #   BOM ツールの照合結果が汚れるので misc は別扱いにする。
    dist = [r for r in rows if r["status"] in ("ok", "sub")]
    # ★LCSC 品番を持たない行は CSV に載せない。BOM ツールは品番で照合するので
    #   照合できない行が混ざると結果が汚れる(ポゴピン・i5モジュールが該当)。
    #   README の一覧には出るので、買い忘れる心配はない。
    lcsc = [r for r in rows if r["status"] == "lcsc" and r["lcsc"]]
    lcsc_nokey = [r for r in rows if r["status"] == "lcsc" and not r["lcsc"]]
    misc = [r for r in rows if r["status"] == "misc"]

    with (out / "digikey-bom.csv").open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["Quantity", "Manufacturer Part Number", "Manufacturer",
                    "Customer Reference", "Description"])
        for r in dist:
            w.writerow([r["qty"], r["mpn"], r["mfr"], r["refs"], clean(r["desc"])])

    with (out / "mouser-bom.csv").open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["Quantity 1", "Mfr Part Number", "Manufacturer Name",
                    "Customer Part Number", "Description"])
        for r in dist:
            w.writerow([r["qty"], r["mpn"], r["mfr"], r["refs"], clean(r["desc"])])

    with (out / "lcsc-bom.csv").open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["Quantity", "LCSC Part Number", "Manufacturer Part Number",
                    "Designator", "Description"])
        for r in lcsc:
            w.writerow([r["qty"], r["lcsc"], r["mpn"], r["refs"], clean(r["desc"])])
    return dist, lcsc, lcsc_nokey, misc


def write_readme(rows, dist, lcsc, lcsc_nokey, misc, out, tag, boards):
    subs = [r for r in dist if r["status"] == "sub"]
    notes = [r for r in rows if r["note"]]

    def table(rs, cols):
        head = "| " + " | ".join(c[0] for c in cols) + " |"
        sep = "|" + "|".join("---" for _ in cols) + "|"
        body = ["| " + " | ".join(str(c[1](r)) for c in cols) + " |" for r in rs]
        return "\n".join([head, sep] + body)

    md = f"""# RetroCast X 手半田実装 発注リスト ({tag}, {boards}枚分)

`tools/gen_order_bom.py` が生成。**手で編集しない**(再生成で消えます)。
元データは `build/builds/default/default.bom.csv` と `tools/order_sourcing.json`。

## アップロード方法

| 販売店 | ファイル | 手順 |
|---|---|---|
| DigiKey | `digikey-bom.csv` | myLists → BOM Manager → Create New BOM → アップロード → 列を対応付け → 一括カート投入 |
| Mouser | `mouser-bom.csv` | BOM ツール → 新規 BOM → アップロード → 列を対応付け → 一括カート投入 |
| LCSC | `lcsc-bom.csv` | BOM Tool にアップロード(LCSC 品番で直接照合) |

どちらも**アップロード時に列の対応を画面で選べる**ので、ヘッダ名が違っても通ります。
照合は製造元品番(MPN)で行われます。

## 数量について

`Quantity` には**手半田用の予備を含めて**あります(0402 は +20%/最低+10個、
安価な IC は +2個、高価な部品は予備なし)。素の必要数は下表の「基板1枚」列です。

## ★発注前に必ず確認すること

{table(notes, [("部品", lambda r: r["refs"].split(",")[0]),
               ("品番", lambda r: r["mpn"]),
               ("注意", lambda r: r["note"])])}

## 読み替えた部品 ({len(subs)}品目)

元の選定は LCSC 前提なので、DigiKey/Mouser で買える同等品に差し替えています。
**値・パッケージ・耐圧は等価**ですが、発注前に一度確認してください。

{table(subs, [("部品", lambda r: r["refs"].split(",")[0]),
              ("元(LCSC)", lambda r: f'{r["orig_mfr"]} {r["orig_mpn"]}'),
              ("読み替え先", lambda r: f'{r["mfr"]} {r["mpn"]}'),
              ("内容", lambda r: r["desc"])])}

## DigiKey / Mouser では買えない部品 ({len(lcsc)}品目)

**フットプリントが製品固有**のため代替が効きません。LCSC / AliExpress 等で
別途手配してください。

{table(lcsc, [("部品", lambda r: r["refs"]),
              ("LCSC", lambda r: r["lcsc"]),
              ("品番", lambda r: r["mpn"]),
              ("内容", lambda r: r["desc"])])}

## ★品番指定のない調達品 ({len(lcsc_nokey)}品目) — CSV には入れていません

LCSC 品番を持たないので BOM ツールでは照合できません。**AliExpress 等で手配**します。

{table(lcsc_nokey, [("部品", lambda r: r["refs"]),
                    ("数", lambda r: r["qty"]),
                    ("品名", lambda r: r["mpn"]),
                    ("内容", lambda r: r["desc"][:200])])}

## どこでも買える汎用品 ({len(misc)}品目)

品番を指定する意味が無いのでアップロード用 CSV には**入れていません**
(説明文を混ぜると照合結果が「該当なし」だらけになるため)。秋月・千石・
マルツ等でまとめて買う方が早いです。

{table(misc, [("部品", lambda r: r["refs"]),
              ("数", lambda r: r["qty"]),
              ("品名", lambda r: r["mpn"]),
              ("内容", lambda r: r["desc"])])}

## 全品目

{table(rows, [("部品", lambda r: r["refs"]),
              ("基板1枚", lambda r: r["qty_board"]),
              ("発注数", lambda r: r["qty"]),
              ("入手先", lambda r: {"ok": "DK/Mouser", "sub": "DK/Mouser(読替)",
                                    "lcsc": "LCSC", "misc": "汎用品"}[r["status"]]),
              ("品番", lambda r: r["mpn"]),
              ("内容", lambda r: r["desc"])])}
"""
    (out / "README.md").write_text(md)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--tag", default="v0.9.0")
    p.add_argument("--boards", type=int, default=1)
    a = p.parse_args()

    out = ROOT / "orders" / a.tag
    out.mkdir(parents=True, exist_ok=True)
    rows = collect(a.boards)
    dist, lcsc, lcsc_nokey, misc = write_csvs(rows, out)
    write_readme(rows, dist, lcsc, lcsc_nokey, misc, out, a.tag, a.boards)

    n_sub = sum(1 for r in dist if r["status"] == "sub")
    print(f"{out.relative_to(ROOT)} に出力しました ({a.boards}枚分)")
    print(f"  DigiKey/Mouser  {len(dist):>3}品目 (うち読み替え {n_sub}品目) "
          f"合計 {sum(r['qty'] for r in dist)}個")
    print(f"  LCSC のみ       {len(lcsc):>3}品目 "
          f"合計 {sum(r['qty'] for r in lcsc)}個")
    print(f"  汎用品(CSV外)   {len(misc):>3}品目 "
          f"合計 {sum(r['qty'] for r in misc)}個")
    return 0


if __name__ == "__main__":
    sys.exit(main())
