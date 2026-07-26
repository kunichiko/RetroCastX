# RetroCastX ゲートウェア(Colorlight i5 / LiteX)

**ステータス: step2(テストパターン・ストリーマ)実装済み・シミュレーション検証済み**(実機は入手待ち)。
ローカル環境: `~/opt/oss-cad-suite` + `gateware/.venv`(LiteX一式、gitignore済み)。

```sh
export PATH="$HOME/opt/oss-cad-suite/bin:$PATH"
.venv/bin/python retrocastx_stream.py --build   # -> build/colorlight_i5/gateware/colorlight_i5.bit
.venv/bin/python sim_stream.py                  # 実機不要のプロトコル検証(Migenシミュレーション)
```

step2 の内容(全てハードウェア実装、CPU不介在):

- **SUBSCRIBE受信**: 映像の送り先をSUBSCRIBE送信元(IP/ポート)に動的切替。
  最後のSUBSCRIBEから10秒で購読失効(PC側が2秒ごとに再送)
- **ANNOUNCE**: 毎秒送出 + SUBSCRIBE受信時に送信元へ即時ユニキャスト返信(発見用)
- **MODE**: 購読開始時に即時送出 + 毎秒再送
- **LINE**: テストパターン(`host/python` の `pattern.make_frame` と同一)を
  RGB555・512×512・30fps で 1ライン=1パケット送出(~126 Mbps)

`sim_stream.py` は UDPポート直結のシミュレーションで、生成パケットを
PC側実装(`protocol.py`/`receiver.FrameAssembler`)にそのまま食わせ、
再構成フレームが `pattern.make_frame` と(RGB555量子化を除き)**ビット一致**する
こと、MODEがLINEに先行すること、購読タイムアウトで停止することを確認する。

実機到着後の疎通手順:

```sh
# PC側(同一L2セグメント、192.168.10.x を推奨。発見だけなら食い違っていても可)
python3 -m retrocastx.discover                        # ANNOUNCEが見えること
python3 -m retrocastx.receiver --subscribe --dump out # SUBSCRIBE送出+パターン受信
```

タイミング収束済み(step2, seed=3): **eth_rx 130.8MHz(制約125)/ sys 52.9MHz(制約50)**。
ポイントは `LiteEthUDPIPCore(..., with_sys_datapath=True)`(CRC等の広幅処理をsysドメインへ
移し、125MHzのethドメインは8bit幅の軽い経路のみにする)。これ無しではeth_rxが93MHz止まり。
sysは50MHz×32bit=200MB/sでGbE線速(125MB/s)に対し十分。

## 構成

- ターゲット: Colorlight i5(ECP5 LFE5U-25F-6BG381C + GbE PHY ×2)+ 拡張基板
- フレームワーク: [LiteX](https://github.com/enjoy-digital/litex) + [LiteEth](https://github.com/enjoy-digital/liteeth)
  - LiteEth の UDP/IP コアはハードウェア実装(データパスにCPU不介在)なので、
    「ADC→ラインFIFO→UDP送出」を全てゲートウェアで完結できる
- 参考実装: [enjoy-digital/colorlite](https://github.com/enjoy-digital/colorlite)
  (5A-75B 上の LiteEth UDP/Etherbone デモ。CRG・PHY 配線はこれに倣う)

## ネットワーク設計(2026-07-26 決定)

- **MAC**: ローカル管理アドレス `02:52:43:58:00:01`(ボードに不揮発IDが無いため。
  複数台対応はSPIフラッシュ設定ページで将来対応)
- **IP**: 既定は静的 192.168.10.50。**DHCPはLiteEth同梱のハードウェアクライアント
  (`liteeth/core/dhcp.py`)でフェーズ2導入予定**
- **発見**: アプリがSUBSCRIBE(ANNOUNCE_ONLY)をブロードキャスト → ボードがANNOUNCEを
  ユニキャスト返信(受信は `with_broadcast=True` で対応済み。LiteEthの通常UDP送信パスに
  ブロードキャストの特別扱いが無いため、ボード発ブロードキャストは使わない設計)
- **ストリーム送り先**: SUBSCRIBE送信元に動的設定(step2で実装)。PC側のIP固定は不要になる

## ブリングアップ計画

1. **step0**: LED点滅 + JTAG書き込み確認(`--build --load`)
2. **step1**: 固定ペイロード(ANNOUNCE)のUDP連続送出、PC側で受信確認
   (step2のコードに置き換え済み。ANNOUNCEビーコンとしてstep2にも含まれる)
3. **step2**: テストパターン生成器 + プロトコルv0のLINE/MODEパケタイザ + SUBSCRIBE購読
   (**コード実装済み・シミュレーションでPC側実装とビット一致を検証済み**。
   実機での `receiver --subscribe` 表示確認が残タスク)
4. **step3**: SODIMM I/O にTVP7002ブレークアウトを接続、実信号キャプチャへ
   (DATACLKのクロック対応ピン割当は hardware/adc-frontend の J4=F2/PCLKT7_0 で確定済み)。
   **ライン断片化の実装が必要**: step2のパケタイザは1ライン=1パケット前提
   (768px RGB888 = 2324B > MTU1500。クライアント側は断片化受信を
   768×512@55.46/533Mbpsで検証済みなので、ゲートウェア側のみ)
5. **step4(音声+CONFIG)**: I2Sキャプチャ×2(BCK/LRCKはMCLK 12.288MHz
   =F1/PCLKC6_1 から分周生成、DOUT→F3/J4取り込み)+ S/PDIFデコーダ(E19、
   sysクロックでオーバーサンプリング)→ AUDIOパケット送出。
   CONFIGパケット受信 → 音声ソース選択 / I2Cマスター経由でTVP7002・ArgusX設定
   (プロトコルは docs/protocol-v0.md の AUDIO / CONFIG で定義済み)

## ツールチェーン(macOS)

```sh
# オープンソースFPGAツール一式(yosys / nextpnr-ecp5 / prjtrellis / openFPGALoader)
brew install yosyshq/tap/oss-cad-suite   # または GitHub Release のバイナリを展開してPATHへ

# LiteX(専用venv推奨)
python3 -m venv ~/litex-env && source ~/litex-env/bin/activate
pip install meson ninja
wget https://raw.githubusercontent.com/enjoy-digital/litex/master/litex_setup.py
python3 litex_setup.py --init --install
```

## ビルド・書き込み(ボード入手後)

```sh
python3 retrocastx_stream.py --build            # bitstream生成
openFPGALoader -b colorlight-i5 build/colorlight_i5/gateware/colorlight_i5.bit   # SRAMへロード(揮発)
openFPGALoader -b colorlight-i5 -f build/... # SPIフラッシュへ書き込み(元のLED受信カードFWは事前に --dump-flash で退避推奨)
```

## メモ(調査結果より)

- i5 の SODIMM エッジは約100本のI/OがバッファなしでFPGA直結(5A-75Bと違い改造不要)
- ピクセルクロック(TVP7002のDATACLK、最大~80MHz)は**クロック対応ピン(GPLL入力)**に
  割り当てること。拡張基板のピン引き出しとBG381ピンアウトの照合が必要(未実施)
- GbE PHY は RGMII。LiteEth の `LiteEthPHYRGMII` を使用(litex-boards の colorlight_i5 ターゲット参照)
