# RetroCast X 手半田実装 発注リスト (v0.9.0, 2枚分)

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
| C82 | CC1812KKX7RDBB472 | ★2kV 耐圧は必須。LANケーブルはサージや建物間の電位差を拾うため Bob-Smith 終端の ブリッジには 2kV が標準で、耐圧は落とせない。LCSC なら同じ Yageo 品が $0.24@25。DigiKey で買うなら KYOCERA AVX 1812GC472KAT1A($0.49@25、4700pF/2000V/X7R/±10%/1812 で完全同スペック、在庫42,893)が Yageo(154円)より安い |
| D1 | EMZT6.8ET2R | 2026-08-18 データシート確認済。実物は EMD5 で**5ピン**。フットプリントは tools/fix_esd_land.py で ROHM 推奨の5パッド(外側0.4/中央0.3)に作り直したので、リードの載らない裸パッドは無くなった(pad番号=データシートのリード番号) |
| D7 | 1N4148W-7-F | 1N4148W は各社互換。Diodes 版が DK/Mouser とも潤沢 |
| J1 | DS1037-15FNAKT76-0CC | ★X68000 用 DA-15(メス/15ピン2列/ライトアングル/スルーホール)。DK/Mouser の DA-15 は取付穴・シェル寸法が異なりフットプリントに合わない。2026-08-18 時点 LCSC 自社在庫 112個 / $0.43 で問題なし |
| J2 | TYPE-C-31-M-12 | いわゆる TYPE-C-31-M-12。DK/Mouser の USB-C は各社独自フットプリント |
| J11 | HR911130A | HANRUN HR911130A。★2026-08-18 時点で調達先が分かれる:
  LCSC   **在庫切れ**(Notify Me のみ、$2.17@10 / $1.92@30)
  JLCPCB Assembly Parts Lib に **5,165個**(Extended、¥401.14@1+ / ¥307.76@31+、Full Reel 31個)
→ v1.0 の PCBA は JLCPCB 側で調達できるので問題なし。2個/枚なので16枚以上なら 31+ の価格帯に入る。
→ v0.9.0 の手半田分は AliExpress で調達する(定番品)。
フットプリントは HanRun HR911105A/HR911130A 系の事実上の業界標準配置(10ピン2列千鳥+LED4+シールド2)なので互換品が多い。公称互換品: LINK-PP LPJG0806FBNL(★使うならピン表の突き合わせが必要)。
★AliExpress で買う場合は内蔵トランス品質のばらつきに注意。10/100 は通っても 1000Base-T でリンクが不安定になる例がある。届いたら1個で先にリンク確認すること |
| LED1 | XL-2020RGBC-2812B | ★Worldsemi WS2812B-2020(C965555)から変更(2026-08-19)。理由は吸湿性ランク:
  WS2812B-2020 MSL 5a → JLCPCB が48時間の焼成(60℃)を要求し追加料金
  XL-2020RGBC  MSL 3  → 焼成不要
JLCPCB は MSL5以上で焼成を要求する。LCSC 在庫も 35,115個→175,745個 と5倍、単価差は $0.074→$0.093 の2セントのみ。WS2812B 互換の1本線プロトコルで 信号割り当ても DI=3/DO=1/GND=2/VDD=4 と同一。★ランドは違う(パッドが x 方向 ±0.92→±0.68 と内寄り)ので基板のフットプリントも差し替え済み。
2020 サイズは DK/Mouser では稀なので引き続き LCSC 調達 |
| R4 | RC0402FR-0775RL | ★映像の終端抵抗。1% 品を使うこと(75Ω 整合がずれると映像レベルが狂う) |
| R18 | RC0402FR-071K5L | 音声ミキシング用。4本は誤差が揃っている方が左右バランスが良い |
| U1 | TVP7002PZP | ★2026-08-18 調査。**廃番ではない**(TI 製品ページは ACTIVE)。ただし流通在庫が薄く、DigiKey 184個 $12.04 / 工場リードタイム26週 / **Mouser は取扱なし** / LCSC 自社在庫4個。以前『生産終了方向』と書いたが言い過ぎだった(根拠にした TI の PCN は本品種を特定したものではない)。LCSC の Other Suppliers に2,973個あるが EOL 品のブローカー在庫で再マーキングの懸念があるため勧めない。PCBA 用は DigiKey で確保して JLCPCB へ委託送付(または JLCPCB の Pre-Order / Global Sourcing)する。★TVP70025IPZP は後継品ではなく上位互換でもない: 90MSPS/10bit固定/WXGA止まりで $16.56 と高く、ピン配置もレジスタマップも互換の保証が無い |
| U5 | TLV75718PDBVR | RICHTEK は DK/Mouser 扱い無し。TLV757P は SOT-23-5 で 1=IN/2=GND/3=EN/4=NC/5=OUT、RT9013 とピン配置一致。1.8V は TVP7002 AVDD 推奨 1.8〜2.0V 内 |
| U7 | CH347F | WCH は DK/Mouser 扱い無し。LCSC / AliExpress / 秋月 で入手 |
| X1 | OT322527MJBA4SL | ★DigiKey は高い(Abracon ASEM1-27.000MHZ-LC-T が471円)。LCSC なら元の選定品が $0.43。DigiKey 内の安価品 ECS-3225MVLC-270-CN-TR($1.16)は**電源1.8〜3.3Vで使えない** (X1 は AZ1117H-3.3 ±2% = 最大3.37V から給電)。1.6〜3.6V の ECS-3225MV 系に 27MHz は無い |
| X2 | X32258MSB4SI | CH347F 用。LCSC で $0.19。DigiKey の 3225 8MHz 在庫品は ABM8AIG-8.000MHZ-1Z-T($1.17)しか無く CL=18pF になるので、元の CL=20pF 品を LCSC で買う方が安くて正確 |
| X3 | OT2JI-111-12.288M | 48kHz x 256fs。DigiKey は Epson SG3225CAN が445円、ECS-TXO-3225MV-122.8-TR でも $2.18。LCSC なら元の選定品が $0.30 |

## 読み替えた部品 (10品目)

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

## DigiKey / Mouser では買えない部品 (11品目)

**フットプリントが製品固有**のため代替が効きません。LCSC / AliExpress 等で
別途手配してください。

| 部品 | LCSC | 品番 | 内容 |
|---|---|---|---|
| C82, C83, C84, C85 | C309511 | CC1812KKX7RDBB472 | 4.7nF 2kV X7R 1812(RJ45 シャーシ-論理GND ブリッジ) |
| J1 | C77836 | DS1037-15FNAKT76-0CC | D-SUB 15pin(DA-15 2列)メス スルーホール |
| J2 | C165948 | TYPE-C-31-M-12 | USB Type-C レセプタクル 16pin SMD |
| J6 | C7436577 | YTC-A1251-08ABW | 1.25mm ピッチ 8pin SMD コネクタ |
| J11, J12 | C54408 | HR911130A | RJ45 パルストランス内蔵 10/100/1000 スルーホール |
| LED1 | C5349955 | XL-2020RGBC-2812B | フルカラーLED 2020(WS2812B互換, 制御IC内蔵) |
| U7 | C18221627 | CH347F | USB-HS → JTAG+UART ブリッジ QFN-28 |
| X1 | C725995 | OT322527MJBA4SL | 27MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| X2 | C2682774 | X32258MSB4SI | 8MHz 水晶 3.2x2.5mm 4pad CL=20pF |
| X3 | C20617595 | OT2JI-111-12.288M | 12.288MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| (module) |  | Colorlight i5 (ECP5) | SO-DIMM FPGA モジュール本体。AliExpress 等 |

## どこでも買える汎用品 (9品目)

品番を指定する意味が無いのでアップロード用 CSV には**入れていません**
(説明文を混ぜると照合結果が「該当なし」だらけになるため)。秋月・千石・
マルツ等でまとめて買う方が早いです。

| 部品 | 数 | 品名 | 内容 |
|---|---|---|---|
| J3 | 2 | ピンヘッダ 1x6 2.54mm | JTAG。秋月・千石等で可 |
| J4 | 2 | ボックスヘッダ 2x5 2.54mm | 第2映像入力(AUX)。シュラウド付 |
| J5 | 2 | ボックスヘッダ 2x4 2.54mm | S-Video + 音声。シュラウド付 |
| J7 | 2 | ピンヘッダ 2x15 2.54mm | デバッグ |
| J9 | 2 | ピンヘッダ 1x4 2.54mm | Argus |
| J10 | 2 | ピンヘッダ 1x4 2.54mm | OLED |
| J8 | 2 | PLR135/T | S/PDIF 光受信モジュール。秋月電子 109595(150円)。★PLR135/T10 は DigiKey 廃品種・在庫0、代替の PLR237/T10BK も在庫0だったため 形状違いの PLR135/T へ変更。ランドが違うのでフットプリントも差し替え済み |
| PG1-PG4 | 8 | ポゴピン D1.0mm | 任意。未実装でも動作する |
| H1-H5 | 10 | M2.6 ネジ + スペーサ | 取付穴。基板発注には含まれない |

## 全品目

| 部品 | 基板1枚 | 発注数 | 入手先 | 品番 | 内容 |
|---|---|---|---|---|---|
| C1, C2, C3, C4, C5, C7, C60, C63, C67, C68, C71, C75, C76 | 13 | 36 | DK/Mouser | CL21A106KAYNNNE | 10uF 0805 X5R MLCC |
| C6, C8, C11, C12, C13, C14, C15, C16, C17, C18, C19, C20, C21, C22, C23, C24, C25, C27, C29, C31, C33, C35, C37, C39, C41, C43, C44, C46, C47, C49, C51, C55, C56, C57, C58, C59, C64, C65, C66, C72, C73, C74, C77, C78, C79, C80, C81 | 47 | 113 | DK/Mouser | CL05B104KO5NNNC | 100nF 0402 X7R 16V MLCC |
| C9, C10 | 2 | 14 | DK/Mouser | CL05C220JB5NNNC | 22pF 0402 C0G 50V MLCC |
| C26, C28, C30, C34, C36, C38, C48, C50 | 8 | 26 | DK/Mouser | CL05C330JB5NNNC | 33pF 0402 C0G 50V MLCC |
| C42 | 1 | 12 | DK/Mouser(読替) | CL05B102KB5NNNC | 1nF 0402 X7R 50V MLCC |
| C45 | 1 | 12 | DK/Mouser(読替) | CL05B472KB5NNNC | 4.7nF 0402 X7R 50V MLCC |
| C53, C54 | 2 | 14 | DK/Mouser | CL05B103KB5NNNC | 10nF 0402 X7R 50V MLCC |
| C61, C62, C69, C70 | 4 | 18 | DK/Mouser | CL10A105KB8NNNC | 1uF 0603 X5R MLCC |
| C82, C83, C84, C85 | 4 | 10 | LCSC | CC1812KKX7RDBB472 | 4.7nF 2kV X7R 1812(RJ45 シャーシ-論理GND ブリッジ) |
| D1, D2, D3, D4, D5, D6, D8 | 7 | 16 | DK/Mouser | EMZT6.8ET2R | ROHM 4ch コモンアノード ESD アレイ EMD5(SC-75A) |
| D7 | 1 | 4 | DK/Mouser(読替) | 1N4148W-7-F | 汎用スイッチングダイオード SOD-123 |
| D9 | 1 | 4 | DK/Mouser | USBLC6-2SC6 | USB 2ライン ESD 保護アレイ SOT-23-6 |
| FB1, FB2, FB3 | 3 | 8 | DK/Mouser | MPZ1608S221ATA00 | フェライトビーズ 220R@100MHz 0603 |
| J1 | 1 | 2 | LCSC | DS1037-15FNAKT76-0CC | D-SUB 15pin(DA-15 2列)メス スルーホール |
| J2 | 1 | 4 | LCSC | TYPE-C-31-M-12 | USB Type-C レセプタクル 16pin SMD |
| J6 | 1 | 4 | LCSC | YTC-A1251-08ABW | 1.25mm ピッチ 8pin SMD コネクタ |
| J11, J12 | 2 | 6 | LCSC | HR911130A | RJ45 パルストランス内蔵 10/100/1000 スルーホール |
| LED1 | 1 | 4 | LCSC | XL-2020RGBC-2812B | フルカラーLED 2020(WS2812B互換, 制御IC内蔵) |
| R1, R2 | 2 | 14 | DK/Mouser(読替) | RC0402FR-075K1L | 5.1k 0402 1% 1/16W |
| R3, R32, R33, R34, R35, R36, R37 | 7 | 24 | DK/Mouser(読替) | RC0402FR-0710KL | 10k 0402 1% 1/16W |
| R4, R6, R8, R11, R13, R15, R17, R23, R25 | 9 | 28 | DK/Mouser(読替) | RC0402FR-0775RL | 75R 0402 1% 1/16W |
| R5, R7, R9, R10, R12, R14, R16, R20, R21, R24, R26, R27, R38, R43, R44, R45, R46 | 17 | 44 | DK/Mouser(読替) | RC0402FR-07220RL | 220R 0402 1% 1/16W |
| R18, R19, R22, R39, R40, R41, R42 | 7 | 24 | DK/Mouser(読替) | RC0402FR-071K5L | 1.5k 0402 1% 1/16W |
| R28, R29, R30, R31 | 4 | 18 | DK/Mouser(読替) | RC0402FR-072K2L | 2.2k 0402 1% 1/16W |
| U1 | 1 | 2 | DK/Mouser | TVP7002PZP | ビデオデジタイザ HTQFP-100 |
| U2, U8, U10, U11, U12 | 5 | 12 | DK/Mouser | SN74LVC2G17DBVR | デュアルシュミットバッファ SOT-23-6 |
| U3, U4 | 2 | 6 | DK/Mouser | AZ1117H-3.3TRE1 | 3.3V LDO SOT-223 |
| U5 | 1 | 4 | DK/Mouser(読替) | TLV75718PDBVR | 1.8V LDO SOT-23-5 |
| U6 | 1 | 4 | DK/Mouser | TLV70019DDCR | 1.9V 200mA LDO SOT-23-5 |
| U7 | 1 | 4 | LCSC | CH347F | USB-HS → JTAG+UART ブリッジ QFN-28 |
| U9 | 1 | 2 | DK/Mouser | 1473005-4 | DDR3 SO-DIMM 204pin ソケット |
| U13, U14 | 2 | 5 | DK/Mouser | PCM1808PWR | ステレオ ADC TSSOP-14 |
| U15, U16 | 2 | 5 | DK/Mouser | 24AA025E48-I/SN | 2kbit EEPROM + EUI-48 MAC SOIC-8 |
| X1 | 1 | 4 | LCSC | OT322527MJBA4SL | 27MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| X2 | 1 | 4 | LCSC | X32258MSB4SI | 8MHz 水晶 3.2x2.5mm 4pad CL=20pF |
| X3 | 1 | 4 | LCSC | OT2JI-111-12.288M | 12.288MHz アクティブ発振器 3.2x2.5mm 4pad 3.3V CMOS |
| J3 | 1 | 2 | 汎用品 | ピンヘッダ 1x6 2.54mm | JTAG。秋月・千石等で可 |
| J4 | 1 | 2 | 汎用品 | ボックスヘッダ 2x5 2.54mm | 第2映像入力(AUX)。シュラウド付 |
| J5 | 1 | 2 | 汎用品 | ボックスヘッダ 2x4 2.54mm | S-Video + 音声。シュラウド付 |
| J7 | 1 | 2 | 汎用品 | ピンヘッダ 2x15 2.54mm | デバッグ |
| J9 | 1 | 2 | 汎用品 | ピンヘッダ 1x4 2.54mm | Argus |
| J10 | 1 | 2 | 汎用品 | ピンヘッダ 1x4 2.54mm | OLED |
| J8 | 1 | 2 | 汎用品 | PLR135/T | S/PDIF 光受信モジュール。秋月電子 109595(150円)。★PLR135/T10 は DigiKey 廃品種・在庫0、代替の PLR237/T10BK も在庫0だったため 形状違いの PLR135/T へ変更。ランドが違うのでフットプリントも差し替え済み |
| PG1-PG4 | 4 | 8 | 汎用品 | ポゴピン D1.0mm | 任意。未実装でも動作する |
| H1-H5 | 5 | 10 | 汎用品 | M2.6 ネジ + スペーサ | 取付穴。基板発注には含まれない |
| (module) | 1 | 2 | LCSC | Colorlight i5 (ECP5) | SO-DIMM FPGA モジュール本体。AliExpress 等 |
