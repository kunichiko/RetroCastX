# RetroCastX ハードウェア調査レポート(2026-07-25)

FPGA直接変換構成(ADC → FPGA → GbE → UDP → PC/Mac)に向けた部品・ボード調査の結果。
4テーマ:①Colorlight 5A-75B リビジョン事情、②I/O入力化改造、③ADCフロントエンド入手性、④代替FPGAボード。

## 結論(TL;DR)

推奨構成:**TVP7002(ADCフロントエンド、OSSC回路流用)+ Colorlight i5(ECP5 + デュアルGbE)+ LiteX/LiteEth(UDPハードウェアスタック)**

- ボードは 5A-75B ではなく **Colorlight i5/i9** を推奨。SODIMMエッジに約100本のI/OがバッファなしでFPGA直結しており、5A-75Bで必須となる74HC245改造が不要。
- ADCは **TI TVP7002** が第一候補。DigiKeyで $10.20/個・1,539個在庫(2026-07時点)、OSSCで15/24/31kHz実績あり、リファレンス回路(ossc_pcb)がオープンソース。
- Colorlightカードへの映像レート入力の先行事例は**存在しない**(実現すれば世界初の領域)。

## ① Colorlight 5A-75B リビジョン事情

情報源: [chubby75](https://github.com/q3k/chubby75)(リバースエンジニアリング資料)

| Rev | FPGA | パッケージ | GbE PHY ×2 | SDRAM |
|---|---|---|---|---|
| V6.1 | LFE5U-25F-6BG381C | CABGA381 | Broadcom B50612D | 2× 1M×16bit |
| V7.0 | LFE5U-25F-6BG256C | CABGA256 | Broadcom B50612D | 2× 1M×16bit |
| V8.0 | LFE5U-25F-6BG256C | CABGA256 | Realtek RTL8211FD | 1× 2M×32bit (8MB) |
| V8.2 | LFE5U-25F-**7**BG256I | CABGA256 | Realtek RTL8211F系 | 1× 2M×32bit (8MB) |

- **全リビジョンが Lattice ECP5 LFE5U-25F**。「新しいリビジョンは他社FPGAに変わった」という噂は誤情報(発端は openFPGALoader issue #513 の配線ミス報告と、Xilinx Artix-7 搭載の別製品 i9+ の混同)。ただし他社ブランドのLED受信カード(ZDEC等)にはAnlogic等が載っているので、購入時は Colorlight 純正を選ぶこと。
- 現行流通品はほぼ **V8.2**(RTL8211F系PHY、-7スピードグレード)。V6.1/V7.0 と V8.x でピン配置・PHYが異なるため、LiteX では `revision=8.2` 等の指定が必要。
- 価格:AliExpress $12〜18、Amazon $25〜40。基板シルクにリビジョン印字あり。
- JTAG:基板上の未実装パッド(V8.x: J27=TCK, J31=TMS, J32=TDI, J30=TDO, J33/J34=3.3V/GND)にピンを立てる。openFPGALoader が公式対応(FT232系、Pico+DirtyJTAG等で書き込み可)。IDCODE 0x41111043 = LFE5U-25。
- LiteX: [colorlight_5a_75b プラットフォーム](https://github.com/litex-hub/litex-boards/blob/master/litex_boards/platforms/colorlight_5a_75b.py)が 6.1/7.0/8.0/8.2 対応。LiteEth(RGMII)動作実績多数。[colorlite](https://github.com/enjoy-digital/colorlite) が 5A-75B 上の LiteEth UDP/Etherbone デモ。

## ② 5A-75B の I/O 入力化改造

- HUB75コネクタの全信号は **74HC245(5V駆動)を出力方向固定で経由**。DIR(1番ピン)とVCC(20番ピン)が基板パターンで固定されており、単純な方向切替は不可(5VがECP5に入るため破損する)。
- 実証済みの改造レシピ3種(主に LinuxCNC の ColorCNC/LitexCNC コミュニティ):
  1. **74LVC245A への載せ替え**(3.3V動作・5Vトレラント入力):パターンカット+DIR→GND、VCC→3.3V。定番。
  2. **SN74CBT3245A(FETバススイッチ)への載せ替え**:遅延ほぼゼロ・双方向。5V供給に1N4007を直列に入れてクランプレベルを調整([ZeroMips事例](https://zeromips.org/posts/2022-05-29-5a-75b/))。
  3. **バッファ撤去+A-Bパッド直結ブリッジ**(小基板または直接ハンダ):最速だがECP5は5V非トレラントなので信号源が3.3V限定。
- ピン収支:バッファ経由I/Oは計56本(各コネクタ個別6本×8 + 共有8本)。24bit+ピクセルクロックの25本なら**バッファ4個の改造で足りる**。
- 速度:30MHzは低リスク。65〜80MHzは電気的には可能だが、コネクタのGNDが16ピン中2本しかなく(リターンパス比14:2)、専用クロック入力ピンがHUB75側に無い点が課題。短配線・追加GND・直列終端(22〜47Ω)・内部PLLでの再同期が必要。
- **映像レートのパラレル入力実績は皆無**。ロジアナ・SDR・ビデオキャプチャいずれも公開事例なし。

## ③ ADCフロントエンド(トリプルADC + HSYNC PLL)

| チップ | 状態(2026-07) | 価格/在庫 | 要点 |
|---|---|---|---|
| **TI TVP7002** | TI上Active(ただし設計サポート終了扱い) | DigiKey **$10.20@1、1,539個在庫** | 8bit@165MSPS / 10bit@110MSPS、PLL 12〜165MHz、位相32step。**OSSCで15/24/31kHz実績**。100-HTQFP |
| **ADI AD9984A** | Active | DigiKey $21.89@1、189個+ADI工場在庫あり | 10bit@170MSPS、PLL 10〜170MHz(分周2〜4095)、位相32step、SOGスライサ。HSYNC最低周波数の規定なし=15/24kHzも計算上カバー。ただし15kHzでの実機実績は未確認 |
| AD9983A | Active(8bit版) | 正規流通在庫確認できず | AD9984Aより入手難 |
| Renesas ISL51002 | **Obsolete** | 正規在庫なし(ブローカーのみ) | OSSC Pro採用。技術的には最良(H-PLLが最も堅牢)だが新規設計不可 |
| ADI ADV7181D | Active | 流通あり | ADC最大75MHz。15/24kHzには足りるが31kHz高ドットクロックに余裕なし。デコーダ寄りで複雑 |
| ADI ADV7842 | 流通薄(LCSC等) | $8〜10 | 12bit/170MHz + HDMI RX。BGA・過剰統合 |

- **第一候補:TVP7002**。理由:最安・在庫潤沢・OSSCによるレトロ周波数(15/24/31kHz)の実証・[ossc_pcb](https://github.com/marqs85/ossc_pcb) のオープンな回路図(SCART/VGA入力の前段回路含む)をそのまま参考にできる。ファームウェア側も [ossc](https://github.com/marqs85/ossc) のTVP7002初期化コードが参考になる。
- 第二候補:AD9984A。オープンな採用例として [HDMI2USB-vmodvga](https://github.com/timvideos/HDMI2USB-vmodvga)(VGAキャプチャ拡張ボード)あり。ただしVESAレート向けで、15kHz同期での動作はベンチ検証が必要。
- 評価ボードは実質入手不可(EVAL-AD9984AEBZ は $1,071・在庫1、TVP7002EVM は絶版)。**ブレークアウトは自作前提**。
- 補足:Chip One Stop は2026年5月にストア閉鎖(arrow.comに統合)。日本からの調達は DigiKey / Mouser / Arrow JP。

## ④ 代替FPGAボード比較(GbE付き)

帯域の前提:24bit@30MHz = 720Mbps(GbEに収まる)、24bit@65MHz = 1.56Gbps(GbEには色深度削減が必要)。

| ボード | FPGA | GbE | 価格 | ツールチェーン | 評価 |
|---|---|---|---|---|---|
| **Colorlight i5** | ECP5-25F (BG381) | ×2 (B50612D) | 〜$25-40 + 拡張基板$15-25 | **オープン(Yosys/nextpnr)** | ◎ SODIMMエッジに約100 I/Oが**バッファなし直結**。LiteX対応([colorlight_i5](https://github.com/litex-hub/litex-boards/blob/master/litex_boards/targets/colorlight_i5.py)) |
| **Colorlight i9** | ECP5-45F (BG381) | ×2 | 〜$40-60 | オープン | ◎ i5の45k LUT版。同上 |
| Colorlight 5A-75B | ECP5-25F | ×2 | **$13-25** | オープン | ○ 最安だが入力化に要バッファ改造 |
| Colorlight i9+ | **Xilinx XC7A50T** | ×2 | 〜$60 | openXC7/Vivado | △ ECP5ではない点に注意 |
| Sipeed Tang Mega 60K Dock | GOWIN GW5AT-60 | ×1 + USB3 | $99.9 | **ベンダー専用**(Apiculaの GW5A 対応は未成熟) | ○ ファブリック・DDR3は魅力 |
| QMTECH Wukong XC7A100T | Artix-7 100T | ×1 | 〜$100-110 | Vivado無償版 | ○ 最大の余裕。LiteX対応 |
| ButterStick | ECP5-85F | ×1 + SYZYGY | $179.99 | オープン | △ 技術的に理想だが高価・在庫不安定 |
| ULX3S / OrangeCrab / iCESugar-Pro | ECP5 | **なし** | - | - | × Ethernet PHYなし |
| RP2350 (Pico 2) | - | 実効〜95Mbps(RMIIハック)が上限 | - | - | × 取り込みはPIOで可能だがEthernet出口が桁不足。HSTX→HDMI→USB3([hsdaoh-rp2350](https://github.com/steve-m/hsdaoh-rp2350)、175MB/s)なら別解 |

- LiteEth は**ハードウェアUDP/IPスタック**(データパスにCPU不介在)であり、「カメラ的にUDPを垂れ流す」本プロジェクトの要件にそのまま合致。
- Colorlight i5 の参考資料:[wuxx/Colorlight-FPGA-Projects](https://github.com/wuxx/Colorlight-FPGA-Projects)、[tomverbeure氏の解説](https://tomverbeure.github.io/2021/01/22/The-Colorlight-i5-as-FPGA-development-board.html)

## 推奨ロードマップ(改訂)

1. **フェーズ1(伝送実証)**:Colorlight i5 + 拡張基板を購入。LiteX/LiteEth でテストパターンを「1パケット=1ライン+タイミングヘッダ」のUDPで送出し、PC/Mac側受信アプリを開発。
2. **フェーズ2(アナログ)**:ossc_pcb を参考に TVP7002 ブレークアウト基板を設計(入力段:75Ω終端、ACカップリング、SOG/CSYNC分離)。i5 のSODIMM I/O に24bit+PCLKを接続。ピクセルクロックはクロック対応ピン(GPLL入力)に割り当てること。
3. **フェーズ3(統合)**:X68000 実機で 15/31kHz、PC-98 で 24kHz のロック・位相調整を検証。モード切替追従をプロトコルに実装。
4. **フェーズ4(基板化)**:TVP7002 + ECP5 + GbE PHY のワンボード化(5A-75B/i5 の回路がリファレンスになる)。

## リスク・未検証事項

- Colorlightカードへの映像レートパラレル入力は前例がない(タイミングクロージャ・SIは自力検証)。
- TVP7002 は TI の設計サポート終了品。長期供給リスクあり(在庫は当面潤沢)。まとめ買い推奨。
- AD9984A の15kHz同期動作は未実証(採用するならベンチ確認)。
- i5 SODIMM ピンのうちクロック入力(PCLK/GPLL)対応ピンの特定が必要(BG381 ピンアウトと拡張基板の引き出しを照合)。
