# RetroCast X 手半田実装 発注リスト (v0.9.0, 1枚分)

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

| 部品 | 品番 | 注意 |
|---|---|---|
| C42 | CL05B102KB5NNNC | FH(風華)は DK/Mouser 扱い無し。同容量・同耐圧の Samsung へ |
| C45 | CL05B472KB5NNNC | 同上 |
| C82 | CC1812KKX7RDBB472 | ★耐圧が要点。GND とシャーシ GND を跨ぐので 250VAC 級の安全規格品を選ぶこと |
| D1 | EMZT6.8ET2R | 2026-08-18 データシート確認済で回路は正しい。実物は**5ピン**(上段中央にリード無し)なので、6パッドのランドのうち pad5 は裸パッドになる。GND に落としてあるので無害だが、★手半田で pad5 に半田を盛ると隣の映像信号 pad4/pad6 へブリッジするので盛らないこと |
| D7 | 1N4148W-7-F | 1N4148W は各社互換。Diodes 版が DK/Mouser とも潤沢 |
| J1 | DS1037-15FNAKT76-0CC | ★X68000 用 DA-15。DK/Mouser の DA-15 は取付穴・シェル寸法が異なりフットプリントに合わない |
| J2 | TYPE-C-31-M-12 | いわゆる TYPE-C-31-M-12。DK/Mouser の USB-C は各社独自フットプリント |
| J11 | HR911130A | HANRUN HR911130A。マグジャックはピン配置が製品固有で互換品が無い |
| LED1 | WS2812B-2020 | 2020 サイズは DK/Mouser では稀。5050 は入るがフットプリントが合わない |
| R4 | RC0402FR-0775RL | ★映像の終端抵抗。1% 品を使うこと(75Ω 整合がずれると映像レベルが狂う) |
| R18 | RC0402FR-071K5L | 音声ミキシング用。4本は誤差が揃っている方が左右バランスが良い |
| U1 | TVP7002PZP | ★TI は生産終了方向。DigiKey に在庫あり(2026-08 時点 184個 $12.04)、Mouser は取扱なし。入手できるうちに確保する |
| U5 | TLV75718PDBVR | RICHTEK は DK/Mouser 扱い無し。TLV757P は SOT-23-5 で 1=IN/2=GND/3=EN/4=NC/5=OUT、RT9013 とピン配置一致。1.8V は TVP7002 AVDD 推奨 1.8〜2.0V 内 |
| U7 | CH347F | WCH は DK/Mouser 扱い無し。LCSC / AliExpress / 秋月 で入手 |
| X1 | ASEM1-27.000MHZ-LC-T | 2026-08 DigiKey 在庫4,466 $2.77。3225 4pin XO は 1=OE/2=GND/3=OUT/4=VDD が業界標準で基板と一致 |
| X2 | ABM8AIG-8.000MHZ-1Z-T | ★負荷容量が元選定と違う(元 CL=20pF → これは CL=18pF)。基板の 22pF x2 では実効負荷 ≒ 22/2+浮遊4 ≒ 15pF なので、CL=18pF の方がむしろ近い。ずれは +30ppm 程度で USB 2.0 の許容 ±500ppm に対して十分小さい。厳密に合わせたいなら C9/C10 を 27pF にする |
| X3 | SG3225CAN 12.2880M-TJGA3 | 2026-08 DigiKey 在庫120 $2.62。48kHz x 256fs。Abracon ASFL1/ASA は 5.0x3.2mm で入らない |

## 読み替えた部品 (13品目)

元の選定は LCSC 前提なので、DigiKey/Mouser で買える同等品に差し替えています。
**値・パッケージ・耐圧は等価**ですが、発注前に一度確認してください。

| 部品 | 元(LCSC) | 読み替え先 | 内容 |
|---|---|---|---|
| C42 | FH 0402B102K500NT | Samsung Electro-Mechanics CL05B102KB5NNNC | 1nF 0402 X7R 50V MLCC |
| C45 | FH 0402B472K500NT | Samsung Electro-Mechanics CL05B472KB5NNNC | 4.7nF 0402 X7R 50V MLCC |
| D7 | ST(Semtech) 1N4148W | Diodes Incorporated 1N4148W-7-F | 汎用スイッチングダイオード SOD-123 |
| R1 | UNI-ROYAL 0402WGF5101TCE | YAGEO RC0402FR-075K1L | 5.1k 0402 1% 1/16W |
| R3 | UNI-ROYAL 0402WGF1002TCE | YAGEO RC0402FR-0710KL | 10k 0402 1% 1/16W |
| R4 | UNI-ROYAL 0402WGF750JTCE | YAGEO RC0402FR-0775RL | 75R 0402 1% 1/16W |
| R5 | UNI-ROYAL 0402WGF2200TCE | YAGEO RC0402FR-07220RL | 220R 0402 1% 1/16W |
| R18 | UNI-ROYAL 0402WGF1501TCE | YAGEO RC0402FR-071K5L | 1.5k 0402 1% 1/16W |
| R28 | UNI-ROYAL 0402WGF2201TCE | YAGEO RC0402FR-072K2L | 2.2k 0402 1% 1/16W |
| U5 | RICHTEK RT9013-18GB | Texas Instruments TLV75718PDBVR | 1.8V LDO SOT-23-5 |
| X1 | YXC OT322527MJBA4SL | Abracon LLC ASEM1-27.000MHZ-LC-T | 27MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| X2 | YXC X32258MSB4SI | Abracon LLC ABM8AIG-8.000MHZ-1Z-T | 8MHz 水晶 3.2x2.5mm 4pad CL=18pF |
| X3 | YXC OT2JI-111-12.288M | EPSON SG3225CAN 12.2880M-TJGA3 | 12.288MHz アクティブ発振器 3.2x2.5mm 4pad CMOS |

## DigiKey / Mouser では買えない部品 (7品目)

**フットプリントが製品固有**のため代替が効きません。LCSC / AliExpress 等で
別途手配してください。

| 部品 | LCSC | 品番 | 内容 |
|---|---|---|---|
| J1 | C77836 | DS1037-15FNAKT76-0CC | D-SUB 15pin(DA-15 2列)メス スルーホール |
| J2 | C165948 | TYPE-C-31-M-12 | USB Type-C レセプタクル 16pin SMD |
| J6 | C7436577 | YTC-A1251-08ABW | 1.25mm ピッチ 8pin SMD コネクタ |
| J11, J12 | C54408 | HR911130A | RJ45 パルストランス内蔵 10/100/1000 スルーホール |
| LED1 | C965555 | WS2812B-2020 | RGB LED 2020(WS2812B 互換) |
| U7 | C18221627 | CH347F | USB-HS → JTAG+UART ブリッジ QFN-28 |
| (module) |  | Colorlight i5 (ECP5) | SO-DIMM FPGA モジュール本体。AliExpress 等 |

## どこでも買える汎用品 (8品目)

品番を指定する意味が無いのでアップロード用 CSV には**入れていません**
(説明文を混ぜると照合結果が「該当なし」だらけになるため)。秋月・千石・
マルツ等でまとめて買う方が早いです。

| 部品 | 数 | 品名 | 内容 |
|---|---|---|---|
| J3 | 1 | ピンヘッダ 1x6 2.54mm | JTAG。秋月・千石等で可 |
| J4 | 1 | ボックスヘッダ 2x5 2.54mm | 第2映像入力(AUX)。シュラウド付 |
| J5 | 1 | ボックスヘッダ 2x4 2.54mm | S-Video + 音声。シュラウド付 |
| J7 | 1 | ピンヘッダ 2x15 2.54mm | デバッグ |
| J9 | 1 | ピンヘッダ 1x4 2.54mm | Argus |
| J10 | 1 | ピンヘッダ 1x4 2.54mm | OLED |
| PG1-PG4 | 4 | ポゴピン D1.0mm | 任意。未実装でも動作する |
| H1-H5 | 5 | M2.6 ネジ + スペーサ | 取付穴。基板発注には含まれない |

## 全品目

| 部品 | 基板1枚 | 発注数 | 入手先 | 品番 | 内容 |
|---|---|---|---|---|---|
| C1, C2, C3, C4, C5, C7, C60, C63, C67, C68, C71, C75, C76 | 13 | 23 | DK/Mouser | CL21A106KAYNNNE | 10uF 0805 X5R MLCC |
| C6, C8, C11, C12, C13, C14, C15, C16, C17, C18, C19, C20, C21, C22, C23, C24, C25, C27, C29, C31, C33, C35, C37, C39, C41, C43, C44, C46, C47, C49, C51, C55, C56, C57, C58, C59, C64, C65, C66, C72, C73, C74, C77, C78, C79, C80, C81 | 47 | 57 | DK/Mouser | CL05B104KO5NNNC | 100nF 0402 X7R 16V MLCC |
| C9, C10 | 2 | 12 | DK/Mouser | CL05C220JB5NNNC | 22pF 0402 C0G 50V MLCC |
| C26, C28, C30, C34, C36, C38, C48, C50 | 8 | 18 | DK/Mouser | CL05C330JB5NNNC | 33pF 0402 C0G 50V MLCC |
| C42 | 1 | 11 | DK/Mouser(読替) | CL05B102KB5NNNC | 1nF 0402 X7R 50V MLCC |
| C45 | 1 | 11 | DK/Mouser(読替) | CL05B472KB5NNNC | 4.7nF 0402 X7R 50V MLCC |
| C53, C54 | 2 | 12 | DK/Mouser | CL05B103KB5NNNC | 10nF 0402 X7R 50V MLCC |
| C61, C62, C69, C70 | 4 | 14 | DK/Mouser | CL10A105KB8NNNC | 1uF 0603 X5R MLCC |
| C82, C83, C84, C85 | 4 | 6 | DK/Mouser | CC1812KKX7RDBB472 | 4.7nF 1812 X7R 高耐圧(GND-シャーシ間ブリッジ) |
| D1, D2, D3, D4, D5, D6, D8 | 7 | 9 | DK/Mouser | EMZT6.8ET2R | ROHM 4ch コモンアノード ESD アレイ EMD5(SC-75A) |
| D7 | 1 | 3 | DK/Mouser(読替) | 1N4148W-7-F | 汎用スイッチングダイオード SOD-123 |
| D9 | 1 | 3 | DK/Mouser | USBLC6-2SC6 | USB 2ライン ESD 保護アレイ SOT-23-6 |
| FB1, FB2, FB3 | 3 | 5 | DK/Mouser | MPZ1608S221ATA00 | フェライトビーズ 220R@100MHz 0603 |
| J1 | 1 | 1 | LCSC | DS1037-15FNAKT76-0CC | D-SUB 15pin(DA-15 2列)メス スルーホール |
| J2 | 1 | 3 | LCSC | TYPE-C-31-M-12 | USB Type-C レセプタクル 16pin SMD |
| J6 | 1 | 3 | LCSC | YTC-A1251-08ABW | 1.25mm ピッチ 8pin SMD コネクタ |
| J11, J12 | 2 | 2 | LCSC | HR911130A | RJ45 パルストランス内蔵 10/100/1000 スルーホール |
| LED1 | 1 | 3 | LCSC | WS2812B-2020 | RGB LED 2020(WS2812B 互換) |
| R1, R2 | 2 | 12 | DK/Mouser(読替) | RC0402FR-075K1L | 5.1k 0402 1% 1/16W |
| R3, R32, R33, R34, R35, R36, R37 | 7 | 17 | DK/Mouser(読替) | RC0402FR-0710KL | 10k 0402 1% 1/16W |
| R4, R6, R8, R11, R13, R15, R17, R23, R25 | 9 | 19 | DK/Mouser(読替) | RC0402FR-0775RL | 75R 0402 1% 1/16W |
| R5, R7, R9, R10, R12, R14, R16, R20, R21, R24, R26, R27, R38, R43, R44, R45, R46 | 17 | 27 | DK/Mouser(読替) | RC0402FR-07220RL | 220R 0402 1% 1/16W |
| R18, R19, R22, R39, R40, R41, R42 | 7 | 17 | DK/Mouser(読替) | RC0402FR-071K5L | 1.5k 0402 1% 1/16W |
| R28, R29, R30, R31 | 4 | 14 | DK/Mouser(読替) | RC0402FR-072K2L | 2.2k 0402 1% 1/16W |
| U1 | 1 | 1 | DK/Mouser | TVP7002PZP | ビデオデジタイザ HTQFP-100 |
| U2, U8, U10, U11, U12 | 5 | 7 | DK/Mouser | SN74LVC2G17DBVR | デュアルシュミットバッファ SOT-23-6 |
| U3, U4 | 2 | 4 | DK/Mouser | AZ1117H-3.3TRE1 | 3.3V LDO SOT-223 |
| U5 | 1 | 3 | DK/Mouser(読替) | TLV75718PDBVR | 1.8V LDO SOT-23-5 |
| U6 | 1 | 3 | DK/Mouser | TLV70019DDCR | 1.9V 200mA LDO SOT-23-5 |
| U7 | 1 | 3 | LCSC | CH347F | USB-HS → JTAG+UART ブリッジ QFN-28 |
| U9 | 1 | 1 | DK/Mouser | 1473005-4 | DDR3 SO-DIMM 204pin ソケット |
| U13, U14 | 2 | 3 | DK/Mouser | PCM1808PWR | ステレオ ADC TSSOP-14 |
| U15, U16 | 2 | 3 | DK/Mouser | 24AA025E48-I/SN | 2kbit EEPROM + EUI-48 MAC SOIC-8 |
| X1 | 1 | 3 | DK/Mouser(読替) | ASEM1-27.000MHZ-LC-T | 27MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| X2 | 1 | 3 | DK/Mouser(読替) | ABM8AIG-8.000MHZ-1Z-T | 8MHz 水晶 3.2x2.5mm 4pad CL=18pF |
| X3 | 1 | 3 | DK/Mouser(読替) | SG3225CAN 12.2880M-TJGA3 | 12.288MHz アクティブ発振器 3.2x2.5mm 4pad CMOS |
| J3 | 1 | 1 | 汎用品 | ピンヘッダ 1x6 2.54mm | JTAG。秋月・千石等で可 |
| J4 | 1 | 1 | 汎用品 | ボックスヘッダ 2x5 2.54mm | 第2映像入力(AUX)。シュラウド付 |
| J5 | 1 | 1 | 汎用品 | ボックスヘッダ 2x4 2.54mm | S-Video + 音声。シュラウド付 |
| J7 | 1 | 1 | 汎用品 | ピンヘッダ 2x15 2.54mm | デバッグ |
| J9 | 1 | 1 | 汎用品 | ピンヘッダ 1x4 2.54mm | Argus |
| J10 | 1 | 1 | 汎用品 | ピンヘッダ 1x4 2.54mm | OLED |
| J8 | 1 | 1 | DK/Mouser | PLR135/T10 | S/PDIF 光受信モジュール |
| PG1-PG4 | 4 | 4 | 汎用品 | ポゴピン D1.0mm | 任意。未実装でも動作する |
| H1-H5 | 5 | 5 | 汎用品 | M2.6 ネジ + スペーサ | 取付穴。基板発注には含まれない |
| (module) | 1 | 1 | LCSC | Colorlight i5 (ECP5) | SO-DIMM FPGA モジュール本体。AliExpress 等 |
