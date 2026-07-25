# WiTranX ゲートウェア(Colorlight i5 / LiteX)

**ステータス: 骨格のみ・実機未検証**(ボード入手待ち)。

## 構成

- ターゲット: Colorlight i5(ECP5 LFE5U-25F-6BG381C + GbE PHY ×2)+ 拡張基板
- フレームワーク: [LiteX](https://github.com/enjoy-digital/litex) + [LiteEth](https://github.com/enjoy-digital/liteeth)
  - LiteEth の UDP/IP コアはハードウェア実装(データパスにCPU不介在)なので、
    「ADC→ラインFIFO→UDP送出」を全てゲートウェアで完結できる
- 参考実装: [enjoy-digital/colorlite](https://github.com/enjoy-digital/colorlite)
  (5A-75B 上の LiteEth UDP/Etherbone デモ。CRG・PHY 配線はこれに倣う)

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
