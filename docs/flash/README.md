# ブラウザからのゲートウェア書き込み (WebUSB)

RetroCastX v0.9.0 基板の FPGA (Colorlight i5 / ECP5 LFE5U-25F) の SPI フラッシュを、
Chrome / Edge から書き換えるためのページ。

公開 URL: <https://kunichiko.github.io/RetroCastX/flash/>

```
ブラウザ ──WebUSB──▶ CH347F (IF4, vendor class 0xff)
                       └─JTAG─▶ ポゴピン ─▶ Colorlight i5 の TCK/TMS/TDI/TDO
                                              └─ ECP5 background SPI ─▶ SPIフラッシュ
```

## なぜ WebUSB で書けるのか

CH347F は複合デバイスで、UART0/UART1 は CDC-ACM (OS のシリアルドライバが掴む) だが、
**JTAG は interface 4 の vendor class 0xff** に分かれている。ここにはどの OS の
カーネルドライバも割り当たらないので、ブラウザが `claimInterface(4)` できる。
UART を使いながら同時に JTAG を叩けるのはこのため。

MimicX の CH32 書き込みと違い、**BOOT ボタンや時間制限のあるモード遷移は不要**。
USB を挿せばいつでも書ける。

## 動作環境

- **ブラウザ**: Chrome, Edge (WebUSB 対応が必要。Safari / Firefox は非対応)
- **OS**: macOS / Linux / ChromeOS / Android は追加作業なし
- **Windows のみ準備が必要**: CH347F は BOS ディスクリプタも MS OS ディスクリプタも
  持たないため、Windows が interface 4 に WinUSB を自動割り当てしない。Zadig を
  「Options → List All Devices」で開き、`UART+SPI+I2C+JTAG (Interface 4)` を選んで
  WinUSB をインストールする。WCH 純正の CH347 ドライバが入っていると競合する

## 配布ビットストリームの追加

通常は **`gw-vX.Y.Z` タグを push すれば CI が自動でやる**
(`.github/workflows/gateware-release.yml`)。ビルド → タイミング検査 →
Release 作成 → `bitstreams/` への配置と `manifest.json` の更新まで行う。
手順は `gateware/README.md` の「リリース」を参照。

★**実体をここに置くのは GitHub Release のアセットが
`access-control-allow-origin` を返さないため**(2026-09-04 実測)。Pages 上の
このページから fetch するには同一オリジンに無ければならない。

手で足す場合は `bitstreams/manifest.json` に追記し、同じディレクトリに `.bit` を置く。

```json
{
  "bitstreams": [
    { "name": "v0.9.0", "file": "retrocastx_v0.9.0.bit",
      "date": "2026-09-04", "note": "TVP7002 キャプチャ + 音声3系統" }
  ]
}
```

一覧に無いものはページ上で「ローカルファイルから選択」で書ける。開発中は
`gateware/build/colorlight_i5/gateware/colorlight_i5.bit` を直接選べばよい。

## 実測

v0.9.0 基板 + macOS / Chrome で、350KB のビットストリームを TCK 3.75MHz で
**消去 + 書き込み + ベリファイ + 再コンフィグ 合わせて約 7 秒**
(2026-09-04 実機確認)。基板のフラッシュは GigaDevice 2MB (JEDEC `0xc84015`) で、
ブロックプロテクトは出荷状態でかかっていない。

## ファイル

| ファイル | 内容 |
|---|---|
| `ch347.js` | CH347F の WebUSB トランスポートと JTAG TAP ステートマシン |
| `ecp5.js` | ECP5 の background SPI 移行、SPI NOR 操作、Lattice `.bit` パーサ |
| `index.html` | UI |
| `simtest.mjs` | 開発用。ECP5 + SPI フラッシュのシミュレータ相手に全経路を通す |

実機を止めずに検証できる。ビットストリームを渡さなければ直近のビルド結果を使う:

```sh
node docs/flash/simtest.mjs [path/to/foo.bit]
```

TAP 遷移・ビット順・SPI 手順・ページ書き込みループを、偽の CH347 パケット層越しに
検証し、最後に「フラッシュの中身がビットストリームと一致するか」まで確かめる。
`ch347.js` / `ecp5.js` を触ったら流すこと。ビットストリームを渡さず直近のビルド結果も
無ければテスト側が合成するので、FPGA ツールチェーンの無い環境でも走る。

`.github/workflows/flasher-test.yml` が `docs/flash/**` を触った PR で自動実行する。

## コマンドラインとの対応

同じことを openFPGALoader でやる場合:

```sh
./gateware/bitstore.sh flash-perm <名前>
#  = openFPGALoader -b colorlight-i5 -c ch347_jtag -f --unprotect-flash <file>.bit
```

`ch347.js` / `ecp5.js` は openFPGALoader (Apache-2.0) の `src/ch347jtag.cpp`,
`src/lattice.cpp`, `src/spiFlash.cpp` の ECP5 経路を JavaScript に移植したもの。
手順は次のとおり:

1. SRAM を消して FPGA を止める (PRELOAD → ISC_ENABLE → ISC_ERASE → ISC_DISABLE)
2. `IR=0x3A` + `DR={0xFE,0x68}` で background SPI に入る
3. SPI のバイトはビット反転して shiftDR。CS は SHIFT-DR に居る間だけアサートされる
4. WREN / RDSR / 64KB ブロック消去 / ページプログラム / ベリファイ
5. `LSC_REFRESH (0x79)` でフラッシュから再コンフィグ

## 実装上の落とし穴

- **`transferIn` / `transferOut` にはタイムアウトが無い**。libusb の
  `libusb_bulk_transfer(..., timeout)` に相当する引数が WebUSB には存在せず、
  デバイスが応答しないと Promise が永久に解決しない。`ch347.js` では全転送を
  `Promise.race` で包んでいる。libusb 版のコードを移植するときは、捨て読みや
  ステータス待ちをそのまま持ってこないこと
- **IN 転送は必ず 512 バイト要求する**。応答より短い長さを要求すると babble に
  なるため、短く読まずに実際の長さで検証する
- **1回の `transferIn` で応答が全部来るとは限らない**。CH347 はデータをクロック
  アウトしながら小分けに返してくるので、期待バイト数に達するまで読み継ぐこと
  (openFPGALoader の `usb_xfer` が `while (rlen) { ... rlen -= actual_length; }` と
  書いているのはこのため)。ここを 1 回読みにすると、短い応答では動くのに
  509 バイトのベリファイ読み出しだけが壊れる、という分かりにくい形で出る。
  `simtest.mjs` の偽デバイスは応答を意図的に分割して、この前提が入り込んだら
  落ちるようにしてある
- **応答の同期が一度ずれると以降の通信が全部化ける**。`ch347.js` は `desynced`
  フラグを立て、UI は復帰を試さず再接続を促す。再接続時の `setClock` は正しい
  4 バイト応答が出るまで読み捨てて同期を取り直す(WebUSB には「タイムアウト付きの
  空読み」が無いため、期待している通信そのものでリカバリしている)
- **JTAG は書き込み中のビットストリームと独立**。壊れたビットストリームを書いても
  同じ経路で書き直せるので、文鎮化はしない
