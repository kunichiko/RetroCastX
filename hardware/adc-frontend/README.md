# WiTranX ADC フロントエンド(TVP7002 ブレークアウト)

レトロPCのアナログRGB(VGA HD-15経由)を TI TVP7002 でデジタル化し、
24bitパラレル + DATACLK/HSOUT/VSOUT/SOGOUT を PMOD配列ヘッダ×5 で
Colorlight i5 EXTボードに渡すブレークアウト基板。回路は `main.ato`(Atopile)。

## 回路の構成(根拠: TVP7002データシート SLES206C / OSSC rev1.8 実機回路)

- **アナログ入力**: VGA各色 → ESDアレイ → 75Ω終端 → 100nF ACカップリング → RIN_3/GIN_3/BIN_3。
  緑は追加で 1nF → SOGIN_3(Sync-on-Green対応)
- **同期入力**: HS/VS → ESD → SN74LVC2G17(シュミット、5Vトレラント)→ HSYNC_A/VSYNC_A。
  VSYNC側は 220Ω+1nF のRC遅延(データシート推奨: VSYNCエッジとHSYNCの完全一致を回避)。
  複合同期(csync)はHSYNCピンに入れれば内部セパレータが分離する(4線モード)
- **H-PLLループフィルタ**(唯一の必須外付け): FILT1→1.5kΩ+100nF→PLL_F、FILT2→4.7nF→PLL_F
- **27MHz発振器** → EXT_CLK(モード検出の安定化用。動作自体は同期駆動で可)
- **電源**: 5V入力(DCジャック)→
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
| J2–J6 | PZ254V-12-6P | C492420 | 2x6ヘッダ(PMOD配列) |
| J7 | DC005 | C431533 | 5V入力 |
| J8 | 2×15 ピンヘッダ 2.54mm | 汎用 | EXT P1対応(GbE差動ペア受け) |
| J9 | HanRun HR911130A | C54408 | 1000BASE-T MagJack(トランス内蔵RJ45) |

抵抗・コンデンサは値・パッケージ指定済み(Atopileのピッカーが実部品を自動選定)。

## ビルド状態

`ato build` は **Verify electrical design(電気設計検証)まで全ステージ通過済み**。
最終段の部品選定(Picking parts)は atopile の部品API
(components.atopileapi.com)が必要だが、**2026-07-18頃からDNSレコード消失により
全世界的に停止中**([atopile#1829](https://github.com/atopile/atopile/issues/1829)、
0.15.7/main 両方で再現、回避策なし)。復旧後に `ato build` を再実行すれば
BOM/ネットリストまで生成される。エンドポイントは `ATO_SERVICES_COMPONENTS_URL`
環境変数または ato.yaml の `services.components.url` で差し替え可能(代替URLが
案内された場合用)。

## 残作業(発注前に必須)

1. **フットプリント紐付け**: VS Code の atopile 拡張で各ICを `ato create part`
   (上記LCSC ID)で取り込み、`main.ato` のスタブcomponentを置き換える
2. **ピン配置の照合**: VGA-002・DC005・PZ254V のピン番号は代表的な配列を仮定している。
   取り込んだフットプリントの実ピン番号と必ず照合すること
## Colorlight i5 EXTボードとの接続(2026-07-25 調査確定)

- **EXTボードにRJ45は無い**。i5モジュール上のGbE PHY×2の差動ペア(MDI)はヘッダ**P1**
  (2×15)に出ているだけ。→ **本基板にMagJack(J8+J9)を搭載済み**。回路は
  [kazkojima氏 i5ether](https://github.com/kazkojima/colorlight-i5-tips)準拠
  (1000BASE-T netboot/nfsroot動作実績あり):MDIはDC結合のまま、PHY側CTは各0.1µF→GND
  (B50612Dは内部バイアスで無給電可)、Bob-SmithはMagJack内蔵、GND-シャーシ間4.7nF/2kV×2。
  **ETH2側ペア=LiteXのphy0** を使用(PHY番号とコネクタ名の対応は逆転に注意)
- EXTボードのGPIOヘッダは P2–P6(2×16メス、計~40 GPIO)。USB-C が電源+DAPLink
  (JTAG書き込み+UARTコンソール)。HDMIは出力のみ。SDスロット無し
- **ECP5のクロック対応(PCLK)ボールが出ているヘッダ**(prjtrellis DB照合結果):
  - P2: J20(PCLKT2_0), K20(PCLKC2_0), L20(PCLKT3_1), L18(GR_PCLK3_1)
  - P3: E2(PCLKC7_0)
  - **P4: F2(PCLKT7_0), F1(PCLKC6_1), F3(PCLKC7_1), G3(PCLKT7_1), H4(GR_PCLK7_1)** ← 最多
  - P6: J18(GR_PCLK2_0)
- 推奨マッピング: J2→P2, J3→P3, J4→P5, **J5(DATACLK含む)→P4のPCLKボール**, J6→P6

3. **レイアウト注意**:
   - DATACLK は i5 側で**クロック対応ピン(PCLK/GPLL入力)**に接続(上記対応表参照)
   - AZ1117H-ADJ は約0.9W発熱: SOT-223のタブに銅箔ベタ+サーマルビア
   - TVP7002 の露出パッドはGNDベタにはんだ付け(熱・電気両方の要件)
   - アナロググラウンドは入力コネクタ周りでベタを分離しすぎない(1点で結合)
4. **色深度**: 配線は各色上位8bit(R/G/B[9:2])。10bit化したい場合は
   hdr_sync/hdr_ctrl の空きピンに [1:0] を追加配線する
