#!/usr/bin/env python3
"""発注リストから部品代を集計する。**価格が無い品目は推測で埋めない。**

## なぜこれがあるか

2026-08-19、手半田版の部品代を口頭で見積もったとき、Colorlight i5 の価格を
「$15〜25(2,300〜3,900円)」と述べた。これは調べずに書いた記憶ベースの数字で、
実勢(利用者実測 約5,000円)の半分以下だった。しかも実測した DigiKey/LCSC の
価格と同じ表に並べたため、同じ確度の情報に見えてしまった。

→ 価格の出所を機械可読にし、**根拠(URL)と取得日を持たない金額は合計に入れない**。

## 出るもの

    orders/<tag>/cost-estimate.md

  実測済み        単価・数量・小計・根拠URL・取得日
  価格未取得      品目と数量だけ。金額は入れない
  合計            実測分のみの**下限**。未取得が残る限り「以上」と表示する

## 使い方

    python3 tools/cost_estimate.py --tag v0.9.0 --boards 5

価格を足すときは、実際に販売ページを見て tools/prices.json に
source と fetched を付けて追記すること。

## 為替について

tools/prices.json の fx で換算する。DigiKey Japan の円建て価格は USD x 約170
だった(利用者実測: ASEM1 $2.77 -> 471円)。あくまで概算なので、確定額は
digikey-bom.csv を DigiKey の BOM ツールに投げて出すのが確実。
"""
import argparse
import csv
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PRICES = ROOT / "tools/prices.json"
SOURCING = ROOT / "tools/order_sourcing.json"


def load_rows(tag):
    """発注CSVから (キー, 表示名, 発注数, 員数/枚, 仕入先) を集める。"""
    out = []
    d = ROOT / "orders" / tag
    for fn, keycol, refcol, src in (
        ("digikey-bom.csv", "Manufacturer Part Number", "Customer Reference", "DK"),
        ("lcsc-bom.csv", "LCSC Part Number", "Designator", "LCSC"),
    ):
        f = d / fn
        if not f.exists():
            continue
        for x in csv.DictReader(f.open()):
            key = x[keycol].strip()
            refs = [r.strip() for r in x[refcol].split(",") if r.strip()]
            name = x.get("Manufacturer Part Number") or x.get("Description", "")
            out.append({
                "key": key or "(品番なし)",
                "name": name if src == "DK" else f'{key} {x.get("Manufacturer Part Number","")}'.strip(),
                "order_qty": int(x["Quantity"]),
                "per_board": len(refs),
                "refs": x[refcol],
                "src": src,
            })
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--tag", default="v0.9.0")
    p.add_argument("--boards", type=int, default=5)
    a = p.parse_args()

    pj = json.loads(PRICES.read_text())
    prices, fx = pj["prices"], pj["fx"]
    rate = {"DK": fx["USD_JPY_digikey"], "LCSC": fx["USD_JPY_lcsc"]}
    rows = load_rows(a.tag)
    if not rows:
        print(f"orders/{a.tag}/ に発注CSVが無い。先に gen_order_bom.py を実行すること",
              file=sys.stderr)
        return 1

    priced, unpriced = [], []
    for r in rows:
        info = prices.get(r["key"])
        if info is None:
            unpriced.append(r)
            continue
        assert info["currency"] == "USD", f'{r["key"]}: USD 以外は未対応'
        yen = rate[r["src"]]
        r["unit_usd"] = info["unit"]
        r["order_jpy"] = info["unit"] * r["order_qty"] * yen
        r["board_jpy"] = info["unit"] * r["per_board"] * yen
        r["info"] = info
        # その数量では単価が届いていない場合の注意
        r["below_break"] = r["order_qty"] < info.get("qty_break", 1)
        priced.append(r)

    misc = [e for e in json.loads(SOURCING.read_text())["extra"]
            if e["status"] == "misc"]

    tot_order = sum(r["order_jpy"] for r in priced)
    tot_board = sum(r["board_jpy"] for r in priced)

    L = []
    A = L.append
    A(f"# RetroCast X 部品代の集計 ({a.tag}, {a.boards}枚分)\n")
    A("`tools/cost_estimate.py` が生成。**手で編集しない**(再生成で消えます)。")
    A("元データは `orders/<tag>/*.csv` と `tools/prices.json`。\n")
    A("★**価格の根拠(URL)と取得日を持つ品目だけ**を合計しています。")
    A("価格未取得の品目は金額に入れていないので、合計は常に**下限**です。\n")

    A(f"## 合計(実測分のみ / 未取得 {len(unpriced)}品目を含まず)\n")
    A("```")
    A(f"{a.boards}枚分の発注総額   {tot_order:>10,.0f}円 以上")
    A(f"1枚あたり(発注額÷{a.boards})  {tot_order/a.boards:>10,.0f}円 以上   ← 予備・端数込みの実支出")
    A(f"1枚あたり(正味BOM)     {tot_board:>10,.0f}円 以上   ← 員数ぶんだけの原価")
    A("```\n")
    A(f"為替: DigiKey は USD x {fx['USD_JPY_digikey']}、LCSC/AliExpress は USD x {fx['USD_JPY_lcsc']}。")
    A(f"{fx['note']}\n")

    A(f"## 価格を確認済みの品目 ({len(priced)})\n")
    A("| 部品 | 発注数 | 単価 | 小計 | 取得日 | 根拠 |")
    A("|---|---:|---:|---:|---|---|")
    for r in sorted(priced, key=lambda r: -r["order_jpy"]):
        i = r["info"]
        warn = " ★数量不足" if r["below_break"] else ""
        A(f'| {r["refs"][:28]} {r["key"]} | {r["order_qty"]} | '
          f'${i["unit"]:.4f}@{i.get("qty_break",1)}{warn} | {r["order_jpy"]:,.0f}円 | '
          f'{i["fetched"]} | [{i["basis"]}]({i["source"]}) |')
    A("")
    A("★数量不足 = その単価が適用される数量に発注数が届いていない。実際はもっと高くなります。\n")

    A(f"## ★価格未取得 ({len(unpriced)}品目) — 上の合計に**含まれていません**\n")
    A("| 部品 | 発注数 | 仕入先 |")
    A("|---|---:|---|")
    for r in sorted(unpriced, key=lambda r: r["refs"]):
        A(f'| {r["refs"][:34]} {r["key"]} | {r["order_qty"]} | {r["src"]} |')
    A("")
    A("価格を足すときは、実際に販売ページを見て `tools/prices.json` に")
    A("`source` と `fetched` を付けて追記してください。")
    A("DigiKey 分は `digikey-bom.csv` を BOM ツールに投げれば一括で確定します。\n")

    A(f"## 汎用品 ({len(misc)}品目) — 金額未計上\n")
    A("ピンヘッダ・ネジ等。秋月・千石等でまとめ買いする前提で、単価を持っていません。\n")
    A("| 部品 | 数/枚 | 品名 |")
    A("|---|---:|---|")
    for e in misc:
        A(f'| {e["ref"]} | {e["qty"]} | {e["mpn"]} |')
    A("")

    if pj.get("unpriced_notes"):
        A("## 価格を確定できていないものの覚え書き\n")
        for k, v in pj["unpriced_notes"].items():
            A(f"- **{k}**: {v}")
        A("")

    out = ROOT / "orders" / a.tag / "cost-estimate.md"
    out.write_text("\n".join(L))
    print(f"{out.relative_to(ROOT)} に出力しました")
    print(f"  価格確認済み {len(priced)}品目 / 未取得 {len(unpriced)}品目 / 汎用品 {len(misc)}品目")
    print(f"  {a.boards}枚分 {tot_order:,.0f}円以上  (1枚 {tot_order/a.boards:,.0f}円以上)")
    if unpriced:
        print(f"  ★未取得が {len(unpriced)}品目あるので、この合計は下限です")
    return 0


if __name__ == "__main__":
    sys.exit(main())
