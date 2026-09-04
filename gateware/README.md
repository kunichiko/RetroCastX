# RetroCastX ゲートウェア(Colorlight i5 / LiteX)

**ステータス: step2(テストパターン・ストリーマ)実装済み・シミュレーション検証済み**(実機は入手待ち)。
ローカル環境: `~/opt/oss-cad-suite` + `gateware/.venv`(LiteX一式、gitignore済み)。

```sh
export PATH="$HOME/opt/oss-cad-suite/bin:$PATH"
.venv/bin/python retrocastx_stream.py --build   # -> build/colorlight_i5/gateware/colorlight_i5.bit
.venv/bin/python sim_stream.py                  # 実機不要のプロトコル検証(Migenシミュレーション)
.venv/bin/python sim_arp.py                     # 受信からのARP学習(retrocastx_net.py)の検証
```

`retrocastx_net.py` は UDP/IPコアに「受信パケットから相手のMACを学習する層」を足した
もの(`LiteEthUDPIPCore` の代わり)。ARP要求に応答しない相手 ─ 別サブネットの
Windows ─ へも返せるようにするため。理由と挟む位置はファイル冒頭と
[docs/design-notes.md](../docs/design-notes.md) を参照。

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

タイミング収束済み(step2+音声, seed=3): **eth_rx 135.4MHz(制約125)/
sys 49.9MHz(制約45)/ aud 182.8MHz(制約12.29)**。
ポイントは `LiteEthUDPIPCore(..., with_sys_datapath=True)`(CRC等の広幅処理をsysドメインへ
移し、125MHzのethドメインは8bit幅の軽い経路のみにする)。これ無しではeth_rxが93MHz止まり。
sysは音声パス追加時に50→**45MHz**へ変更(50MHzでは収束せず。45MHz×32bit=180MB/sで
GbE線速125MB/sに対しなお十分)。

## 構成

- ターゲット: Colorlight i5(ECP5 LFE5U-25F-6BG381C + GbE PHY ×2)+ 拡張基板
- フレームワーク: [LiteX](https://github.com/enjoy-digital/litex) + [LiteEth](https://github.com/enjoy-digital/liteeth)
  - LiteEth の UDP/IP コアはハードウェア実装(データパスにCPU不介在)なので、
    「ADC→ラインFIFO→UDP送出」を全てゲートウェアで完結できる
- 参考実装: [enjoy-digital/colorlite](https://github.com/enjoy-digital/colorlite)
  (5A-75B 上の LiteEth UDP/Etherbone デモ。CRG・PHY 配線はこれに倣う)

## ネットワーク設計(2026-07-26 決定)

- **MAC**: 基板のEUI-48 EEPROM(24AA025E48 ×2、0x50=ETH0/0x51=ETH1)から起動時に
  読み出し(グローバル一意)。フォールバックはローカル管理アドレス `02:52:43:58:00:01`。
  現行ビットストリームはフォールバック値を直書き(EEPROM読み出しは実機step4で実装)
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
   768×512@55.46/533Mbpsで検証済みなので、ゲートウェア側のみ)。
   **インターレース対応**: TVP7002のFIDOUT(フィールド識別)はJ4のB2に配線済み。
   規約は protocol-v0.md で確定(line=フルフレーム行・frame=フィールドごと+1、
   受信側は無修正でweave合成になることを検証済み)。ゲートウェアは
   FIDOUTから行番号 2n+field を計算してFIELD_ODDフラグを立てるだけ
5. **step4(音声+CONFIG)**: **コード実装済み・sim検証済み**(retrocastx_audio.py)。
   - I2Sキャプチャ×2: MCLK 12.288MHz(F1/PCLKC6_1)="aud"ドメイン、BCK/LRCK分周
     生成、DOUT(F3/J4)から上位16bit取得、AsyncFIFOでsysへ。PCM1808波形モデルで
     ビット一致検証(sim_audio.py)
   - S/PDIFデコーダ(E19): sysクロックでBMC復号(UI長EWMA追従、プリアンブル
     B/M/W判別、レート実測)。非整数UI(8.14サイクル)の波形でビット一致検証
   - AUDIOパケット送出(ソース別FIFO、複数同時可)+ CONFIG受信/応答
     (音声マスク・ArgusX仮レジスタ)を統合sim(sim_stream.py シナリオC)で検証
   - 実機残タスク: TVP7002のI2C初期化、ArgusX宛CONFIGのI2C中継(仮レジスタを置換)、
     MAC EEPROM(24AA025E48@0x50)の起動時読み出し→MAC設定(読めなければLAAへ
     フォールバック。Ethernetの有効化をEEPROM読了まで保留する)

## ツールチェーン(macOS)

```sh
# オープンソースFPGAツール一式(yosys / nextpnr-ecp5 / prjtrellis / openFPGALoader)
brew install yosyshq/tap/oss-cad-suite   # または GitHub Release のバイナリを展開してPATHへ

# LiteX(専用venv推奨)
python3 -m venv ~/litex-env && source ~/litex-env/bin/activate
pip install meson ninja
wget -O litex_setup.py https://raw.githubusercontent.com/enjoy-digital/litex/master/litex_setup.py
mkdir -p litex-src && cd litex-src
python3 ../litex_setup.py --dev --init --install    # ★--dev を必ず付ける
```

### ★`--dev` を必ず付けること

`litex_setup.py` は起動時に**自分自身と `litex_repos.py` を upstream の master で
無条件に上書きする**。`litex_repos.py` は各リポジトリを sha1 で固定している
このリポジトリの管理対象ファイルなので、`--dev` を付けずに走らせると**固定が
丸ごと消えて、次のビルドが別バージョンの LiteX で行われる**。

`--dev` は自動更新を止めるフラグで、`GITHUB_ACTIONS=true` のときは clone URL を
SSH に書き換えない作りになっているため、CI でもそのまま使える。

固定を今の環境で取り直したいときは:

```sh
cd litex-src && python3 ../litex_setup.py --dev --freeze --freeze-output=../litex_repos.py
```

### ★タイミングはビルドの成否で分からない

LiteX は nextpnr に `--timing-allow-fail` を渡すので、**タイミング違反でもビルドは
成功する**。しかも eth_rx(125MHz要求)は配置シード次第で通ったり落ちたりする
(`retrocastx_stream.py` の `--seed` のコメント参照)。ビルド後は必ずログの
`Max frequency` を全ドメイン確認し、`FAIL at` が無いことを見ること。
CI(`.github/workflows/gateware-release.yml`)はこれを自動で検査して落とす。

## リリース

`gw-vX.Y.Z` の注釈付きタグを push すると、`gateware-release.yml` が

1. 固定した LiteX + oss-cad-suite でビルド
2. タイミング検査(`FAIL at` があれば落とす)
3. WebUSB フラッシャのシミュレータに通す
4. GitHub Release を作成して `.bit` を添付
5. 同じ `.bit` を main の `docs/flash/bitstreams/` に commit し `manifest.json` を更新

まで行う。5 まで済むと <https://kunichiko.github.io/RetroCastX/flash/> の一覧に出て、
ブラウザから焼けるようになる。

```sh
git tag -a gw-v0.9.0 -m "gw-v0.9.0: タイトル

タグの注釈メッセージが Release の本文と、フラッシャの一覧の説明文になる。"
git push origin gw-v0.9.0
```

★**前置きの `gw-` は必須。** Viewer のリリース(`viewer-release.yml`)が `v*.*.*`
で走るので、素の `v0.9.0` を打つと両方動いてしまう。

タグを打たずに CI のビルドだけ試したいときは、Actions から
`Gateware Release` を手動実行する(既定では Release も docs/ への配置もせず、
成果物は artifact から取れる。`seed` も指定できる)。

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
