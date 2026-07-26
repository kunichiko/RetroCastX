# RetroCastX

レトロPC(X68000 / PC-98 など)のアナログRGB信号を、フレームバッファレスで PC/Mac に伝送・表示するプロジェクト。

```
Retro PC ──アナログRGB──▶ ADC(TVP7002) ──24bit+PCLK──▶ FPGA(ECP5) ──UDP/GbE──▶ PC/Mac(再構成・表示)
```

## 設計思想

一般的なアップスキャンコンバータは変換器内でフレームバッファを構築し標準的なHDMI信号に整形するため、
機種固有のタイミング情報が失われ、遅延・モード切替時のブラックアウト・価格の問題が生じる。

RetroCastX は「変換器は馬鹿に徹し、知性はPC側に置く」:

- ADC でドットクロックを再生してサンプリングした**生のピクセルデータを、タイミング情報ごと**
  「1パケット≒1ライン」の UDP で送出する(FPGA はラインFIFO程度しか持たない)
- フレームの再構成・スケーリング・表示は PC/Mac 側で行う

これにより:

- **生タイミング保存**: VRR対応モニタなら 55.46Hz(X68000)等の非標準リフレッシュをそのまま表示可能
- **モード切替の無切断追従**: ゲーム中の解像度切替もヘッダ通知で追従
- **真のロスレス**: 8bit で取得し PC 側でパレット吸着(X68000 は本来 RGB 各5bit)
- **低遅延**: サブフレーム(ライン単位)伝送
- **安価**: フレームバッファ用メモリも高度な画像処理も変換器側に不要

## リポジトリ構成

```
docs/
  protocol-v0.md    伝送プロトコル仕様 v0(ドラフト)
  research/         部品・ボード調査レポート
gateware/           FPGA側(LiteX / Colorlight i5)※骨格のみ、実機未検証
host/python/        PC側リファレンス実装(送信シミュレータ・受信・検証)
```

## クイックスタート(ハードウェア不要)

送信シミュレータと受信器をループバックで動かす:

```sh
cd host/python
python3 -m retrocastx.tests.test_loopback   # プロトコルのE2E検証
python3 -m retrocastx.sender_sim &          # テストパターンをUDP送出
python3 -m retrocastx.receiver --dump out   # 受信して out/frame_NNNN.ppm に保存
```

## ハードウェア(フェーズ1〜2 想定)

| 役割 | 部品 | 備考 |
|---|---|---|
| FPGAボード | Colorlight i5 + 拡張基板 | ECP5-25F、GbE PHY ×2、SODIMM I/O 直結 |
| ADC | TI TVP7002 | OSSC と同一。15/24/31kHz 実証済み。ブレークアウト自作 |
| 参考回路 | [marqs85/ossc_pcb](https://github.com/marqs85/ossc_pcb) | アナログ入力段・TVP7002 周辺 |

調査の詳細は `docs/research/2026-07-25-hardware-survey.md` を参照。

## ステータス

- [x] 構想・部品調査
- [x] プロトコル v0 ドラフト
- [x] PC側リファレンス実装(シミュレータ + 受信器)
- [ ] LiteX ゲートウェア(テストパターン送出)実機動作
- [ ] TVP7002 ブレークアウト設計
- [ ] 実機キャプチャ(X68000 / PC-98)
