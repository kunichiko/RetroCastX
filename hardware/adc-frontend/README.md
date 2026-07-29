# RetroCastX ADC フロントエンド(TVP7002 ブレークアウト)

レトロPCのアナログRGB(VGA HD-15経由)を TI TVP7002 でデジタル化し、
24bitパラレル + DATACLK/HSOUT/VSOUT/SOGOUT を Colorlight i5 EXTボードへ渡す
**HAT基板**(EXTのP1/P2/P4に直接スタック)。GbE MagJack・**音声入力3系統**・
ArgusX制御コネクタ搭載。回路は `main.ato`(Atopile)。

## 音声入力(3系統)

| 系統 | 経路 | 変換 |
|---|---|---|
| RGB端子音声 | J1(D-SUB15)ピン4=L/11=R → SJ2/SJ3 → U11 PCM1808 | 16bit/48kHz I2S |
| LINE入力 | J12(3.5mmステレオ) → U12 PCM1808 | 16bit/48kHz I2S |
| 光デジタル | J13(TOSLINKモジュール) → FPGA直結 | S/PDIFをゲートウェアでデコード |

- **クロック**: X2(12.288MHz XO =256fs@48kHz)→ 両ADCのSCKIとFPGA(F1=PCLKC6_1)。
  BCK/LRCKはFPGAがMCLKから分周して共通供給。**DOUTのみ個別**なので
  アナログ2系統は同時キャプチャ可能。S/PDIFのDIRチップは不要(FPGAでデコード)
- **SJ2/SJ3(既定: 閉)**: D-SUB15のピン4/11はVGA規格ではID/DDC。音声結線のある
  レトロPC用ケーブルでは閉、一般VGAケーブル(DDC結線あり)を挿す運用では開ける
- どの系統を転送するかはアプリからCONFIGパケットで選択(複数同時も可)。
  詳細は `docs/protocol-v0.md` の AUDIO / CONFIG 参照

## ArgusX(RGBセレクタ、別プロジェクト)連携

J11(1×4: 3V3/SDA/SCL/GND)にTVP7002と共有のI2Cバスを引き出し。アプリからの
CONFIGパケット(target=ArgusX)をゲートウェアがI2C書き込みに中継し、セレクタの
入力切替をアプリから行える。ArgusXのI2Cアドレス/レジスタマップは同プロジェクト側で
確定後に反映。**I2Cアドレスの割当: TVP7002=0x5C(7bit)、MAC EEPROM=0x50/0x51
→ ArgusXは0x50-0x53と0x5Cを避けること**。

## MACアドレス(EUI-48 EEPROM)

U13/U14(24AA025E48)に工場書込みのグローバル一意MACを搭載。
ゲートウェアが起動時にI2Cで読み出してEthernetに設定する
(EEPROM未実装/読出し失敗時はローカル管理アドレス02:52:43:58:xx:xxへフォールバック)。
複数台運用・2ポートストライピング(ETH1追加時)でMAC衝突管理が不要になる。
EEPROMの空き領域(~250B)は個体設定(IP・ボード名等)の保存に使用
(旧計画のSPIフラッシュ設定ページを置き換え)。

## 回路の構成(根拠: TVP7002データシート SLES206C / OSSC rev1.8 実機回路)

- **アナログ入力**: VGA各色 → ESDアレイ → 75Ω終端 → 100nF ACカップリング → RIN_3/GIN_3/BIN_3。
  緑は追加で 1nF → SOGIN_3(Sync-on-Green対応)
- **同期入力**: HS/VS → ESD → SN74LVC2G17(シュミット、5Vトレラント)→ HSYNC_A/VSYNC_A。
  VSYNC側は 220Ω+1nF のRC遅延(データシート推奨: VSYNCエッジとHSYNCの完全一致を回避)。
  複合同期(csync)はHSYNCピンに入れれば内部セパレータが分離する(4線モード)
- **H-PLLループフィルタ**(唯一の必須外付け): FILT1→1.5kΩ+100nF→PLL_F、FILT2→4.7nF→PLL_F
- **27MHz発振器** → EXT_CLK(モード検出の安定化用。動作自体は同期駆動で可)
- **電源**: **本基板が電源マスター(既定)**。J10 USB-C(CCにRd 5.1kΩ実装 → PD充電器+
  C-Cケーブルで5V/3A受電可)から給電し、SJ1(閉=既定)でP1/P2/P4の5Vピン経由で
  **EXTボードへ逆給電**する。この運用ではEXTのスライドスイッチS1をOFFにし、EXTのUSB-Cは
  JTAG/UARTデータ専用とする。代替入力: J7 DCジャック(J10と排他)。合計消費 ≈1.2A。
  ※EXTボードのUSB-CはCC未接続(Rdなし)のためC-Cケーブルでは受電できない(回路図で確認済み)。
  5Vレールから →
  - AZ1117H-3.3 → 3.3V IO系(IOVDD、発振器、バッファ、プルアップ)
  - AZ1117H-3.3(別個)→ 3.3Vアナログ(A33VDD)
  - AZ1117H-ADJ + 120Ω/62Ω分圧 ≈1.896V → AVDD・PLL_AVDD(各フェライトビーズ経由)
  - TLV70019(1.9V)→ DVDD
- **未使用入力**: video系は10nF、SOG系は1nFでGNDへ(データシート指定)
- **ストラップ**: PWDN/TMS/CLAMP/COAST/HSYNC_B/VSYNC_B→GND、I2CA→GND(I2Cアドレス0xB8)、
  RESETB は2.2kプルダウン(FPGAがHighで解除)、SDA/SCL 2.2kプルアップ

## 主要部品(2026-07 時点の在庫確認済み)

| Ref | 部品 | LCSC | 備考 |
|---|---|---|---|
| U1 | TVP7002PZP | C3824085 | DigiKey: TVP7002PZPR ($10.20)。LCSCは5個のみ |
| U2 | SN74LVC2G17DBVR | C10429 | 同期バッファ |
| U3,U4 | AZ1117H-3.3TRE1 | C92517 | IO系 / アナログ3.3V |
| U5 | AZ1117H-ADJTRE1 | C92103 | 1.9V生成(分圧) |
| U6 | TLV70019DDCR | C2862411 | DVDD 1.9V |
| FB1,FB2 | MPZ1608S221ATA00 | C76815 | AVDD/PLL分離 |
| D1,D2 | PESD5V0U4BW | C5182054 | 映像/同期ESD |
| X1 | X322527MSB4SI | C9008 | 27MHz 3.3V CMOS |
| J1 | VGA-002 | C138387 | HD-15メス |
| J2, J4 | 2×15 ピンヘッダ/ソケット 2.54mm | 汎用 | EXT P2/P4ミラー(直挿しハット時はソケット) |
| J7 | DC005 | C431533 | 5V入力(EXT給電運用時は未実装可) |
| SJ1 | はんだジャンパ | — | 閉(既定)=ヘッダ5VピンでEXTへ逆給電 / 開=本基板単独給電 |
| J10 | TYPE-C-31-M-12 | C165948 | USB-C電源入力(Rd 5.1kΩ×2実装、C-C/PD充電器対応) |
| J8 | 2×15 ピンヘッダ 2.54mm | 汎用 | EXT P1対応(GbE差動ペア受け) |
| J9 | HanRun HR911130A | C54408 | 1000BASE-T MagJack(トランス内蔵RJ45) |
| U11,U12 | PCM1808PWR | 要選定 | 音声ADC(RGB端子音声 / LINE入力) |
| X2 | 12.288MHz XO 3225 | 要選定 | 音声マスタークロック(256fs@48kHz) |
| J11 | 1×4 ピンヘッダ 2.54mm | 汎用 | ArgusX制御(I2C引き出し) |
| J12 | 3.5mmステレオジャック | 要選定(PJ-320A系) | LINE入力 |
| J13 | TOSLINK受信モジュール | 要選定(PLR135/T8等) | 光デジタル入力 |
| SJ2,SJ3 | はんだジャンパ | — | D-SUB15ピン4/11の音声結線(既定: 閉) |
| U13,U14 | 24AA025E48 | 要選定 | MACアドレスEEPROM(EUI-48)。0x50=ETH0 / 0x51=ETH1(将来) |

抵抗・コンデンサは値・パッケージ指定済み(Atopileのピッカーが実部品を自動選定)。

## ビルド状態

`ato build` は **Verify electrical design(電気設計検証)まで全ステージ通過済み**。
最終段の部品選定(Picking parts)は atopile の部品API
(components.atopileapi.com)が必要だが、**2026-07-18頃からDNSレコード消失により
全世界的に停止中**([atopile#1829](https://github.com/atopile/atopile/issues/1829)、
0.15.7/main 両方で再現、2026-07-28時点でも未復旧)。

### オフライン部品取り込みの回避策(2026-07-28確立)

atopileの**検索・パラメトリック選定API**(atopileapi、死亡)とは別に、
**フットプリント実体はEasyEDA**(easyeda.com、生存)から取得できることを利用する。
`tools/ingest_parts.py` が、既知のLCSC番号を渡すと `ato create part` 相当を
オフラインで実行し `parts/<MFR_PN>/{.ato,.kicad_mod,.kicad_sym}` を生成する
(取得後のメーカー名enrich呼び出し=atopileapiのみスタブ化)。

```sh
/Users/ohnaka/.local/share/uv/tools/atopile/bin/python tools/ingest_parts.py C10429 C3824085 ...
```

生成された部品は `has_part_picked` traitを持つため、`ato build` の選定処理で
**スキップされ、atopileapiを叩かない**。よって**全部品をatomic part化(=各部品に
LCSC番号を割当てて取り込み)すれば、`ato build` が完全オフラインで通り、
ネットリスト・PCB・BOM・JLCfabデータまで生成できる**。

取り込み済み(2026-07-28、`parts/`): TVP7002, SN74LVC2G17, AZ1117H-3.3,
AZ1117H-ADJ, TLV70019, MPZ1608S221A, 27MHz OSC(X322527), VGA-002, DC005,
TYPE-C-31-M-12, HR911130A の11点。
未取り込み: PESD5V0U4BW(C5182054はEasyEDAにデータ無し→代替番号要選定)、
受動部品(各値のLCSC番号割当が必要)、音声系"要選定"部品(PCM1808/XO/TOSLINK/
3.5mmジャック/24AA025E48)。

## 発注前コネクタQA(2026-07-30、要対応)

atopile取り込み部品のフットプリント実パッドと、main.atoの(元・仮割り当て)ピン番号の
照合結果。`ato build` の「Could not match lead to pad」警告＝不一致のサイン。

| コネクタ | 実パッド | 状態 |
|---|---|---|
| D-SUB15 (DS1037) | 数値 1-17 | ✓ 整合(X68000配置はD-subピン番号通り)。実部品で最終目視のみ |
| DCジャック (DC005) | 1-3 | ✓ 整合(center/sleeve/switch)。極性のみ要確認 |
| PCM1808 / EEPROM / TVP等IC | 数値=データシート順 | ✓ 整合 |
| **USB-C (TYPE-C-31-M-12)** | A1B12/B1A12=GND, A4B9/B4A9=VBUS, A5=CC1, B5=CC2, A6/A7/B6/B7=D±, A8/B8=SBU, 1-4=シェル | ✓ **修正済**(標準USB-C配置で実パッド名へ再マッピング) |
| **MagJack (HR911130A)** | P1-P10(磁気部), 11-14(LED), SHIELD0/1 | ✗ **要修正**。スタブは数値1-18で実パッドと不一致。P1-P10→MDI0-3±/中央タップの対応とCTトポロジ(現設計は4CT想定だが実部品は最大2ピン)はHR911130Aデータシート必須。LCSC/HanRunのPDFが機械取得不可のため未解決。★1000BASE-TはAuto-MDIXでペア/極性入替を吸収するが、ペアのグルーピングとCT接続は要確定 |
| **3.5mmジャック (PJ-327C-4A)** | 1-4 (実4ピン) | ✗ **要修正**。スタブは5ピン(tip/ring/sleeve/tip_sw/ring_sw)想定。実4ピンのtip/ring/sleeve/switch割当はPJ-327Cデータシート要確認 |

→ 残: MagJackと3.5mmジャックの実ピン割当をデータシート(またはKiCadフットプリント
ビューア+実部品)で確定して main.ato を修正。それ以外のコネクタは整合確認済み。

## 残作業(発注前に必須)

1. **フットプリント紐付け**: VS Code の atopile 拡張で各ICを `ato create part`
   (上記LCSC ID)で取り込み、`main.ato` のスタブcomponentを置き換える
2. **ピン配置の照合**: VGA-002・DC005・PZ254V のピン番号は代表的な配列を仮定している。
   取り込んだフットプリントの実ピン番号と必ず照合すること
3. **レイアウト注意**:
   - DATACLK は i5 側で**クロック対応ピン(PCLK/GPLL入力)**に接続(上記対応表参照)
   - AZ1117H-ADJ は約0.9W発熱: SOT-223のタブに銅箔ベタ+サーマルビア
   - TVP7002 の露出パッドはGNDベタにはんだ付け(熱・電気両方の要件)
   - アナロググラウンドは入力コネクタ周りでベタを分離しすぎない(1点で結合)
4. **色深度**: 配線は各色上位8bit(R/G/B[9:2])。J4の空き6ピンは音声系で
   使い切ったため、10bit化する場合はP6ミラーヘッダの追加が必要
5. **音声まわり**: PCM1808のVCC(5Vアナログ)はFB経由で分離済み。レイアウトでは
   ADC・XOをGbE/映像系から離し、D-SUB音声の引き回しは映像RGBと並走させない。
   PCM1808/TOSLINK/3.5mmジャック/12.288MHz XOのスタブはピン番号仮割り当てなので
   フットプリント取り込み時にデータシート照合(main.ato内の注意書き参照)

## Colorlight i5 EXTボードとの接続(2026-07-25 調査確定)

- **EXTボードにRJ45は無い**。i5モジュール上のGbE PHY×2の差動ペア(MDI)はヘッダ**P1**
  (2×15)に出ているだけ。→ **本基板にMagJack(J8+J9)を搭載済み**。回路は
  [kazkojima氏 i5ether](https://github.com/kazkojima/colorlight-i5-tips)準拠
  (1000BASE-T netboot/nfsroot動作実績あり):MDIはDC結合のまま、PHY側CTは各0.1µF→GND
  (B50612Dは内部バイアスで無給電可)、Bob-SmithはMagJack内蔵、GND-シャーシ間4.7nF/2kV×2。
  **ETH2側ペア=LiteXのphy0** を使用(PHY番号とコネクタ名の対応は逆転に注意)
- EXTボードのGPIOヘッダは P2–P6(各2×15メス、GPIOは1ヘッダあたり18〜20本)。USB-C は
  DAPLink(JTAG書き込み+UARTコンソール)+VBUS。HDMIは出力のみ。SDスロット無し
- **EXTのUSB-C電源には要注意(回路図 schematic/i5-i9-extboard.pdf で確認)**: PDコントローラなし・
  **CC端子未接続(Rd 5.1kΩなし)**のため、規格準拠のC-Cケーブルでは受電不可(A→Cケーブル専用)。
  VBUS→ヒューズF1(定格不明)→スライドスイッチS1→5Vレール直結。
  → 本基板をJ10から給電しS1をOFFにする「逆給電」運用でこの問題を回避する
- **ECP5のクロック対応(PCLK)ボールが出ているヘッダ**(prjtrellis DB照合結果):
  - P2: J20(PCLKT2_0), K20(PCLKC2_0), L20(PCLKT3_1), L18(GR_PCLK3_1)
  - P3: E2(PCLKC7_0)
  - **P4: F2(PCLKT7_0), F1(PCLKC6_1), F3(PCLKC7_1), G3(PCLKT7_1), H4(GR_PCLK7_1)** ← 最多
  - P6: J18(GR_PCLK2_0)
- **HAT形式(スタック)を正式採用**: 本基板は裏面の2×15ピン(J2/J4/J8)でEXTボードの
  P2/P4/P1メスヘッダに直接嵌合する。ピン配置は1対1ミラーのため、寸法不一致時は
  2×15リボンケーブルでフォールバック可。**製作前にEXTボード実寸(ヘッダ間距離・
  i5モジュールの高さ・ソケット嵌合高さ≈11mm)の採寸が必須**
- 信号は P2/P4 の2ヘッダに集約(EXT側1ヘッダのGPIOは18〜20本でRGB24bitは1本に収まらない):
  - **J2 = P2ミラー**: R9-R2→J20,G20,L18,K20,M18,L20,N17,N18 / G9-G2→U17,P18,T17,U18,P17,M17,R18,R17 / B9-B6→C18,T18,U16,K18
  - **J4 = P4ミラー**: DATACLK→**F2(PCLKT7_0)**、B5-B2→E1,E4,H3,H5、HSOUT→J5、VSOUT→A2、
    SOGOUT→K4、FIDOUT→B2、SDA→K3、SCL→K5、RESETB→B3、
    音声: MCLK→**F1(PCLKC6_1)**、BCK→G3、LRCK→H4、DOUT(DSUB音声)→F3、
    DOUT(LINE)→J4、S/PDIF→E19(空きなし)

