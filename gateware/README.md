# WiTranX ゲートウェア(Colorlight i5 / LiteX)

**ステータス: step1(ANNOUNCEビーコン)のビットストリーム生成に成功**(実機は入手待ち)。
ローカル環境: `~/opt/oss-cad-suite` + `gateware/.venv`(LiteX一式、gitignore済み)。

```sh
export PATH="$HOME/opt/oss-cad-suite/bin:$PATH"
.venv/bin/python witranx_stream.py --build   # -> build/colorlight_i5/gateware/colorlight_i5.bit
```

タイミング収束済み: **eth_rx 133.1MHz(制約125)/ sys 52.3MHz(制約50)**。
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

- **MAC**: ローカル管理アドレス `02:57:54:58:00:01`(ボードに不揮発IDが無いため。
  複数台対応はSPIフラッシュ設定ページで将来対応)
- **IP**: 既定は静的 192.168.10.50。**DHCPはLiteEth同梱のハードウェアクライアント
  (`liteeth/core/dhcp.py`)でフェーズ2導入予定**
- **発見**: アプリがSUBSCRIBE(ANNOUNCE_ONLY)をブロードキャスト → ボードがANNOUNCEを
  ユニキャスト返信(受信は `with_broadcast=True` で対応済み。LiteEthの通常UDP送信パスに
  ブロードキャストの特別扱いが無いため、ボード発ブロードキャストは使わない設計)
- **ストリーム送り先**: SUBSCRIBE送信元に動的設定(step2で実装)。PC側のIP固定は不要になる

## ブリングアップ計画

1. **step0**: LED点滅 + JTAG書き込み確認(`--build --load`)
2. **step1**: `witranx_stream.py` — 固定ペイロードのUDPパケットをPCへ連続送出、
   PC側 `tcpdump` / receiver で受信確認(いまここのコードがある)
3. **step2**: テストパターン生成器 + プロトコルv0のLINE/MODEパケタイザを実装、
   `host/python/witranx/receiver.py` でパターンが表示されることを確認
4. **step3**: SODIMM I/O にTVP7002ブレークアウトを接続、実信号キャプチャへ

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
python3 witranx_stream.py --build            # bitstream生成
openFPGALoader -b colorlight-i5 build/colorlight_i5/gateware/colorlight_i5.bit   # SRAMへロード(揮発)
openFPGALoader -b colorlight-i5 -f build/... # SPIフラッシュへ書き込み(元のLED受信カードFWは事前に --dump-flash で退避推奨)
```

## メモ(調査結果より)

- i5 の SODIMM エッジは約100本のI/OがバッファなしでFPGA直結(5A-75Bと違い改造不要)
- ピクセルクロック(TVP7002のDATACLK、最大~80MHz)は**クロック対応ピン(GPLL入力)**に
  割り当てること。拡張基板のピン引き出しとBG381ピンアウトの照合が必要(未実施)
- GbE PHY は RGMII。LiteEth の `LiteEthPHYRGMII` を使用(litex-boards の colorlight_i5 ターゲット参照)
