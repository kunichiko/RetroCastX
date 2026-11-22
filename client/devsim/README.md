# ダミーデバイス(devsim)

キャプチャボードの代役。**持っていない機種の映像信号で Viewer を試す**ためのもの。
ネットワーク上では実機のボードと同じに振る舞うので、Viewer は何も変えずに繋がる。

```sh
cd client
cargo run --release -p retrocastx-devsim                      # GUI
cargo run --release -p retrocastx-devsim -- --list            # 機種とモードの一覧
cargo run --release -p retrocastx-devsim -- --headless \
    --input rgb15=x68000:0 --input video=composite:0 --cycle 8  # GUI なし(試験用)
```

Mac / Windows / Linux でビルドできる(Viewer と同じ eframe)。

## 何を真似ているか

`機種の信号 → TVP7002 のサンプリング → ゲートウェアのパケット化` を順にソフトで作る。

- **信号**(`src/machines.rs` `src/signal.rs`): 機種ごとのドットクロック・htotal・
  同期幅・有効映像の位置で1ラインを描く。コンポジット/S端子は 8fsc の CVBS 波形
  (同期チップ・カラーバースト・副搬送波)、コンポーネントは Y/Pb/Pr で出す
- **TVP**: Viewer が CONFIG で書いた `pll_divide` と位相でサンプルする。pll_divide が
  合っていなければ絵が伸び縮みし、位相がドットの境目に掛かれば隣と混ざる。
  **Viewer の PLL・位相の自動調整が実機と同じ理屈で効く**
- **ゲートウェア**(`src/capture.rs` `src/engine.rs`): `gateware/retrocastx_capture.py`
  と `retrocastx_stream.py` から起こした規則で送る。docs/protocol-v0.md に無い挙動も
  合わせてある:
  - `mode_id` は常に 1、`hactive` は 2048、`vactive` は半ライン単位のスロット数
  - 真っ黒なラインも `count_px = 0` で送る。FIELD_ODD はプログレッシブでも毎フレーム反転
  - 送る範囲は「黒でない範囲」を4画素単位に広げたもの(全ライン送信・YC8/CVBS8・
    送出範囲の固定はそれぞれの規則)
  - fH と DATACLK は**1秒窓**、vtotal は4周期、2周期分の組は3回続いてから変わる。
    モードを切り替えた直後は前後の混ざった値が出る(Viewer の modeswitch.rs が
    待っているのはこの遅れ)
  - SUBSCRIBE / RENEW / 10秒の失効、ANNOUNCE の STREAMING / YOURS、CONFIG の応答と
    読み取り専用の測定値(fH_TVP・生同期・stat_il・SOG など)
  - 音声は 48kHz・240サンプル/パケットで系統 0/1/3 を送る(繋いだ端子の系統に
    テスト音)

**端子は4つ持っていて、TVP が見る端子は Viewer が書く入力MUX(19h)で決まる。**
入力3(D-SUB15)の RGB 機は生同期として 0x2A でも fH が読める。

## GUI でできること

- 端子ごとに機種とモードを選ぶ。モードのボタンを押すとその場で切り替わる
  (ゲーム中の解像度切替の追従を試す)。「自動で切り替える」で一定間隔で巡回
- **カスタム**: ドットクロック・htotal・同期幅・有効範囲・走査方式を自由に入れる。
  持っていない機種のタイミングを資料から入れて試す
- 絵の柄(総合 / カラーバー / 32段階調 / 1ドット縦縞 / 市松 / 格子)、動く箱、雑音
- 障害の注入: パケットロス、古いゲートウェアを装う(caps の FRAME_CLOCKS などを外す)
- 「取り込んだ絵」: Viewer に送っている中身そのもの(横は pll_divide、縦はスロット)
- CONFIG レジスタの現在値と、SUBSCRIBE / CONFIG の記録

## 繋ぎ方

**別の機械で動かす**: 同じ LAN に置けば、Viewer のボード一覧に `devsim`
(MAC `02:52:43:58:D5:01`)として出る。

**Viewer と同じ機械で動かす**:

- ダミーを先に起動すれば 0.0.0.0:34600 を掴むので、Viewer は空きポートへ落ちて
  そのまま繋がる(ボード一覧にも出る)
- 実機を見ている Viewer が先に 34600 を掴んでいたら、ダミーは `127.0.0.1:34600`
  だけで待ち受ける。**別の Viewer を** `--board 127.0.0.1` で起動して繋ぐ
  (34600 を掴んでいる Viewer 自身からは、返事が自分に戻るので繋がらない)

★**実機と同じ LAN では MAC を指名する。** Viewer をワイルドカードで購読させると、
実機のボードも映像をこちらへ向けてくる。`--mac 02:52:43:58:D5:01` を付けるか、
ボード一覧で devsim を選ぶ。

## 実機との違い(分かっているもの)

- 機種のタイミングは `verified` のもの以外は資料からの**推定**(GUI に【推定】と出る)
- TVP のクランプ・ゲイン・同期スライスなどのレジスタは覚えるだけで、絵には効かない。
  コンポジット/S端子/コンポーネントの符号値は、Viewer のプロファイルが書く設定での
  実測値で固定してある
- 表示遅延の計測(caps bit4)、S/PDIF、デジタルRGB の取り込みは無い
  (デジタルRGB は fH の測定値 0x70 だけ返す)
- X68000 24kHz(VSYNC 1回にフィールド2枚)の後半フィールドは半ラインずらしていない
