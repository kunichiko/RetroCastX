# RetroCast X 部品代の集計 (v0.9.0, 2枚分)

`tools/cost_estimate.py` が生成。**手で編集しない**(再生成で消えます)。
元データは `orders/<tag>/*.csv` と `tools/prices.json`。

★**価格の根拠(URL)と取得日を持つ品目だけ**を合計しています。
価格未取得の品目は金額に入れていないので、合計は常に**下限**です。

## 合計(実測分のみ / 未取得 19品目を含まず)

```
2枚分の発注総額       13,561円 以上
1枚あたり(発注額÷2)       6,780円 以上   ← 予備・端数込みの実支出
1枚あたり(正味BOM)          5,940円 以上   ← 員数ぶんだけの原価
```

為替: DigiKey は USD x 170、LCSC/AliExpress は USD x 155。
DigiKey Japan の円建て価格は USD x 約170 だった(利用者実測: ASEM1 $2.77 -> 471円)。LCSC/AliExpress は USD 建てなので実勢レート寄りの155で換算。いずれも概算なので、確定額は各社の BOM ツールで出すこと。

## 価格を確認済みの品目 (17)

| 部品 | 発注数 | 単価 | 小計 | 取得日 | 根拠 |
|---|---:|---:|---:|---|---|
| U1 TVP7002PZP | 2 | $12.0400@1 | 4,094円 | 2026-08-18 | [measured](https://www.digikey.com/en/products/detail/texas-instruments/TVP7002PZP/1765836) |
| U9 1473005-4 | 2 | $9.5900@1 | 3,261円 | 2026-08-19 | [measured](https://www.digikey.com/en/products/result?keywords=1473005-4) |
| J11, J12 C54408 | 6 | $2.1665@10 ★数量不足 | 2,015円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C54408.html) |
| U13, U14 PCM1808PWR | 5 | $1.5900@1 | 1,352円 | 2026-08-19 | [measured](https://www.digikey.com/en/products/result?keywords=PCM1808PWR) |
| D1, D2, D3, D4, D5, D6, D8 EMZT6.8ET2R | 16 | $0.1983@25 ★数量不足 | 539円 | 2026-08-19 | [measured](https://www.digikey.com/en/products/result?keywords=EMZT6.8ET2R) |
| U2, U8, U10, U11, U12 C10429 | 12 | $0.2403@30 ★数量不足 | 447円 | 2026-08-19 | [measured](https://www.lcsc.com/product-detail/C10429.html) |
| C1, C2, C3, C4, C5, C7, C60, C15850 | 36 | $0.0795@50 ★数量不足 | 444円 | 2026-08-19 | [measured](https://www.lcsc.com/product-detail/C15850.html) |
| C82, C83, C84, C85 C309511 | 10 | $0.2414@25 ★数量不足 | 374円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C309511.html) |
| X1 C725995 | 4 | $0.4311@10 ★数量不足 | 267円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C725995.html) |
| X3 C20617595 | 4 | $0.3033@10 ★数量不足 | 188円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C20617595.html) |
| J1 C77836 | 2 | $0.4268@5 ★数量不足 | 132円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C77836.html) |
| C6, C8, C11, C12, C13, C14,  CL05B104KO5NNNC | 113 | $0.0066@250 ★数量不足 | 127円 | 2026-08-19 | [measured](https://www.digikey.com/en/products/result?keywords=CL05B104KO5NNNC) |
| X2 C2682774 | 4 | $0.1942@5 ★数量不足 | 120円 | 2026-08-18 | [measured](https://www.lcsc.com/product-detail/C2682774.html) |
| D9 C7519 | 4 | $0.1432@10 ★数量不足 | 89円 | 2026-08-19 | [measured](https://www.lcsc.com/product-detail/C7519.html) |
| LED1 C5349955 | 4 | $0.0925@50 ★数量不足 | 57円 | 2026-08-19 | [measured](https://www.lcsc.com/product-detail/RGB-LEDs-Built-in-IC_XINGLIGHT-XL-2020RGBC-WS2812B_C5349955.html) |
| R5, R7, R9, R10, R12, R14, R RC0402FR-07220RL | 44 | $0.0061@200 ★数量不足 | 46円 | 2026-08-19 | [measured](https://www.digikey.com/en/products/result?keywords=RC0402FR-07220RL) |
| R1, R2 C25905 | 14 | $0.0043@100 ★数量不足 | 9円 | 2026-08-19 | [measured](https://www.lcsc.com/product-detail/C25905.html) |

★数量不足 = その単価が適用される数量に発注数が届いていない。実際はもっと高くなります。

## ★価格未取得 (19品目) — 上の合計に**含まれていません**

| 部品 | 発注数 | 仕入先 |
|---|---:|---|
| C26, C28, C30, C34, C36, C38, C48, CL05C330JB5NNNC | 26 | DK |
| C42 CL05B102KB5NNNC | 12 | DK |
| C45 CL05B472KB5NNNC | 12 | DK |
| C53, C54 CL05B103KB5NNNC | 14 | DK |
| C61, C62, C69, C70 CL10A105KB8NNNC | 18 | DK |
| C9, C10 CL05C220JB5NNNC | 14 | DK |
| D7 1N4148W-7-F | 4 | DK |
| FB1, FB2, FB3 MPZ1608S221ATA00 | 8 | DK |
| J2 C165948 | 4 | LCSC |
| J6 C7436577 | 4 | LCSC |
| R18, R19, R22, R39, R40, R41, R42 RC0402FR-071K5L | 24 | DK |
| R28, R29, R30, R31 RC0402FR-072K2L | 18 | DK |
| R3, R32, R33, R34, R35, R36, R37 RC0402FR-0710KL | 24 | DK |
| R4, R6, R8, R11, R13, R15, R17, R2 RC0402FR-0775RL | 28 | DK |
| U15, U16 24AA025E48-I/SN | 5 | DK |
| U3, U4 AZ1117H-3.3TRE1 | 6 | DK |
| U5 TLV75718PDBVR | 4 | DK |
| U6 TLV70019DDCR | 4 | DK |
| U7 C18221627 | 4 | LCSC |

価格を足すときは、実際に販売ページを見て `tools/prices.json` に
`source` と `fetched` を付けて追記してください。
DigiKey 分は `digikey-bom.csv` を BOM ツールに投げれば一括で確定します。

## 汎用品 (8品目) — 金額未計上

ピンヘッダ・ネジ等。秋月・千石等でまとめ買いする前提で、単価を持っていません。

| 部品 | 数/枚 | 品名 |
|---|---:|---|
| J3 | 1 | ピンヘッダ 1x6 2.54mm |
| J4 | 1 | ボックスヘッダ 2x5 2.54mm |
| J5 | 1 | ボックスヘッダ 2x4 2.54mm |
| J7 | 1 | ピンヘッダ 2x15 2.54mm |
| J9 | 1 | ピンヘッダ 1x4 2.54mm |
| J10 | 1 | ピンヘッダ 1x4 2.54mm |
| J8 | 1 | PLR135/T |
| H1-H5 | 5 | M2.6 ネジ + スペーサ |

## 価格を確定できていないものの覚え書き

- **Colorlight i5 (ECP5)**: ★2026-08-19 利用者が AliExpress で確認したところ **約5,000円**。私が会話中に『$15〜25(2,300〜3,900円)』と述べたのは根拠の無い記憶で誤り(2021年頃の$12.99が頭にあった)。出品者差が大きく、$14.96〜66.73 の幅がある(i9・キャリアボード付きを含む)。商品ページを直接取得できなかったため measured にはしない。
