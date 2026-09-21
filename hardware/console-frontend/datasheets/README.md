# データシート置き場(小亀基板)

メーカー配布のPDFは**再配布が許諾されていないためコミットしていません**(`.gitignore` 済み)。
必要なときは各自で入手して、このディレクトリに下表の名前で置いてください。
`main.ato` / `parts/_mech/mech.ato` のコメントはこの名前で参照しています。
扱いの方針はメイン基板の `hardware/adc-frontend/datasheets/README.md` と同じです。

| ファイル | 内容 | 入手元 |
|---|---|---|
| `MJ-373-4B_Marushin_MiniDIN4.pdf` | マル信無線電機 MJ-373/4B(ミニDIN 4ピンジャック = S端子、基板取付)。**このPDFのPCB穴図から `parts/_mech/MJ373_4B.kicad_mod` を自作した** | モモハラ電機部品 [図面PDF](https://www.mepc.jp/store/pdf/est/drawing/MJ-373-4B.pdf) / 千石電商・マルツの商品ページ |
| `ts5a23159.pdf` | TI TS5A23159(2ch SPDT アナログスイッチ)。U1。ピン番号と真理値表(SCDS201H Table 2)の根拠 | [TI 製品ページ](https://www.ti.com/product/TS5A23159) / LCSC [C42751](https://www.lcsc.com/product-detail/C42751.html) |
| `RCA-105.png` | RCA-105(RCAジャック、右アングル、2端子)。**この図面から `parts/_mech/RCA105.kicad_mod` を自作した**(嵌合面からpad1=4.80 / pad2=8.30、スロット2.30×1.50 と 1.00×2.30) | AliExpress の商品ページの画像(例: [1005006152724809](https://ja.aliexpress.com/item/1005006152724809.html))。出品元ごとに寸法が違うことがあるので**買った現物で実測する** |
| `emzt6.8e.pdf` | ROHM EMZT6.8ET2R(4ch コモンアノード ESDアレイ) | LCSC [C510333](https://www.lcsc.com/product-detail/C510333.html) |

ミニDIN4のランドは**メーカー図面から画像解析で実測**して確定しました
(φ1.2×4 が x=8.5/11.0・y=±3.25、シェル用スロット 2.8×0.8 が (4.7,0) と (5.5,±6.85)、
原点=嵌合面)。**メイン基板のミニDIN8(MJ-373/8B)と同じハウジング枠**です。
