# RetroCast X(Viewer / クライアントアプリ)

Rust + eframe/egui(wgpuバックエンド)のクロスプラットフォーム・ビューア。
Mac/Windows/Linux を単一コードベースでカバーする。

```sh
cargo run --release                     # SUBSCRIBEをブロードキャストして実機ボードを受信
cargo run --release -- --board 192.168.10.50   # ボードIP指定
cargo run --release -- --no-subscribe   # 受け専用(sender_sim相手はこれ)
cargo run --release -- --fullscreen     # 低遅延フルスクリーン(下記、ESCで終了)
cargo run --release -- --headless 5 --no-subscribe  # GUIなしで受信統計のみ(検証用)
cargo test                              # プロトコル/アセンブラのユニットテスト
```

## Windows で使うときの必須設定: NICの受信バッファー

**これをやらないとパケットを1〜6%落とす**(映像に穴、音が途切れる)。Intel NIC の
`受信バッファー`(受信記述子リング)の既定 **256** が RetroCastX のパケットレート
(31kHz等倍で約35,000 pkt/s)に足りない。管理者PowerShellで:

```powershell
Set-NetAdapterAdvancedProperty -Name 'イーサネット' -RegistryKeyword '*ReceiveBuffers' -RegistryValue 2048
```

実測(同じ機械・同じ線): **256 → lost 1.7%**(間引き2で帯域1/3にしても落ちる)、
**2048 → lost 0.0013%**(等倍316Mbps、audio underruns 0)。

`SO_RCVBUF`(Viewerは64MB確保する)を増やしても効かない。捨てているのは
ソケットバッファではなく**その手前のNICドライバのリング**だから。切り分けは
`Get-NetAdapterStatistics -Name 'イーサネット' | fl ReceivedDiscardedPackets,ReceivedPacketErrors`
で、**errors≒0 なのに discards が大きい**ならこれ。詳細は
[docs/design-notes.md](../docs/design-notes.md)。

sender_sim を相手にした動作確認:

```sh
(cd ../host/python && python3 -m retrocastx.sender_sim --pixfmt rgb555) &
cargo run --release -- --no-subscribe
```

## 配布(GitHub Actions)

`.github/workflows/viewer-release.yml` が Mac/Windows のビルドと配布をする。
構成は MimicX-app の `release-build.yml` に倣ってあり、**Secretsの名前も同じ**なので
同じ証明書とAPIキーをそのまま設定できる。

| きっかけ | すること |
|---|---|
| タグ `v1.2.3` を push | Releaseを作り zip を添付。macOSは署名+公証+staple |
| main へのPR | 両OSで `cargo test` + ビルドのみ(署名も公証もしない) |
| 手動実行(Actions画面) | プラットフォームを選んでビルド。Releaseは触らない |

**バージョンの正は `client/Cargo.toml`。** タグと食い違っていたら `prepare` で止まる
(`v0.2.0` を打つなら先に `version = "0.2.0"` にしてコミットする)。

必要な Secrets(**RetroCastXリポジトリにも登録が必要**。リポジトリのSecretsは
他リポジトリから見えないので、MimicX と同じ値をコピーする):

| Secret | 用途 |
|---|---|
| `MACOS_CERTIFICATE_BASE64` | Developer ID Application 証明書(.p12)のbase64 |
| `MACOS_CERTIFICATE_PASSWORD` | その .p12 のパスワード |
| `KEYCHAIN_PASSWORD` | CI上に作る一時キーチェーンのパスワード(任意の文字列) |
| `APPSTORE_ISSUER_ID` / `APPSTORE_KEY_ID` / `APPSTORE_PRIVATE_KEY` | 公証(notarytool)用のApp Store Connect APIキー |

Secretsが無くてもビルドは通り、**未署名**の `...-unsigned.zip` ができる(フォークや
PRのため)。ただし**タグからのリリースだけは署名必須**で、無ければエラーで止まる。

成果物:

    RetroCastX_macos-<version>.zip     「RetroCast X.app」universal(arm64+x86_64)、署名済み
    RetroCastX_windows-<version>.zip   RetroCastX.exe + README-first.txt

macOSの `.app` は `packaging/macos/bundle.sh` が組み立てる。**CIのYAMLにロジックを
置かず**スクリプトにしてあるので、手元で同じものを作って試せる:

```sh
cargo build --release
packaging/macos/bundle.sh target/release/retrocastx-viewer 0.1.0 0 /tmp/out
open "/tmp/out/RetroCast X.app"
```

アイコンは `packaging/AppIcon.png`(1024×1024)が master で、そこから
`packaging/make-icons.sh` が2つを作る。**生成物もコミットしてある**(CIに画像変換
ツールを入れない方針。作り直したときは一緒にコミットする):

    packaging/macos/AppIcon.icns    .app に入る(bundle.sh が拾う)
    packaging/windows/AppIcon.ico   exe に埋め込む(build.rs が拾う)

Windows の窓とタスクバーの絵も exe のアイコンで決まる(winit は明示指定が無ければ
実行ファイルのアイコンを使う)ので、埋め込みだけで両方に効く。

未対応(必要になったら):

- **Windowsの署名**: 証明書が無いので未署名。SmartScreenの警告は同梱の
  README-first.txt で案内している
- **Windowsのインストーラ**: いまはzipのみ。`windows-latest` には Inno Setup が
  入っているので、固定パスへ入れたくなったら追加できる(実行ファイルのパスが
  変わるとファイアウォールの許可を再度聞かれるので、その観点では利点がある)
- **ゲートウェア/Python側のCI**: このワークフローはViewerだけを見る

## 構成

- `src/protocol.rs` — プロトコルv0のパース(リファレンスは `host/python/retrocastx/protocol.py`)
- `src/assembler.rs` — LINE→フレーム再構成(RGBA8出力、ロス/迷子ライン統計)
- `src/receiver.rs` — UDP受信スレッド + SUBSCRIBEキープアライブ(2秒ごと)
- `src/main.rs` — eframe UI(映像表示 + モード/統計/発見ボードのサイドパネル)

## 設計メモ

- v0.1 は egui 標準のテクスチャAPIで毎フレーム全面アップロード(512×512なら余裕)。
  **パレット吸着・CRTシェーダ・整数拡大以外のフィルタは egui_wgpu の paint callback
  (自前WGSLシェーダ)へ移行して実装する**(次段)

### VRR / 非標準リフレッシュ(55.46Hz等)の追従 — 検証結果(2026-07-27, Mac mini M4 Pro + 60Hz外部モニタ)

`examples/pace_probe.rs`(専用スレッドからwgpuサーフェスへ直接present)による実測:

| 構成 | present間隔 |
|---|---|
| eframe(ウィンドウ、vsync/no-vsyncとも) | **60Hzに量子化**(winitの再描画スケジューリングがディスプレイリンク駆動) |
| 専用スレッド + Immediate、ウィンドウ表示 | **60Hzに量子化**(コンポジタ律速。latency増でも変わらず) |
| 専用スレッド + Immediate、**フルスクリーン** | **ターゲット追従**: 90Hz指定→11.1ms/89.8Hz、55.46Hz指定→18.03ms/55.4Hz(σ~2.5ms) |

結論: **フルスクリーン(direct-to-display)+専用presentスレッド+Immediate** が
55.46Hz追従の実装形。レトロゲーム全画面用途と合致する。ウィンドウ表示時は
60Hz vsyncのジャダーを許容(それでも実用上は十分)。
再現: `cargo run --release --example pace_probe -- 55.46 --latency 3 --fullscreen`

この構成は `--fullscreen` モード(`src/fullscreen.rs`)として本実装済み:
フレーム到着駆動でpresentし、sender_sim 512×512@55.46Hz RGB555 に対して
**present間隔 18.03ms σ0.7ms = 55.46Hz追従・ロス0** を実測確認(2026-07-27)。
プライマリモニタに全画面表示(不可視ディスプレイ対策でモニタ明示)、
ニアレスト+整数拡大レターボックス、ESC/クローズで終了、統計はstderr。
タイムスタンプ駆動のジッタ平滑化(到着ゆらぎをドットクロック時刻で吸収)は次段。

Windows側は wgpu Immediate が DXGI allow-tearing にマップされるので FreeSync で同様の構成が取れる見込み。

#### ProMotionパネルでの追従確認(2026-08-06, MacBook Pro M5 Pro + 内蔵XDR 120Hz, macOS 26.4)

上記で残っていた「**パネルが実際に追従するか**」をProMotion実機で確認した。

app側(`--fullscreen`, 3024×1898全画面, sender_sim 512×512@55.46Hz RGB555):
**present間隔 18.03ms σ0.7ms = 55.46Hz・ロス0・237Mbps** — 別マシン(M4 Pro)の結果を再現。

パネル側は `MTLDrawable.presentedTime`(実際に画面へ出た時刻)で計測した。
**CADisplayLink と `CGDisplayModeGetRefreshRate` は使えない** — presentedTimeが明らかに
120Hz格子から外れている最中も両者は8.33ms/120Hzを報告し続ける(交差検証済み)。

| 構成 | presentedTime間隔 | 格子 |
|---|---|---|
| vsync + 全力present(基準) | 8.335ms σ1.6 | **8.333ms格子に残差0.000ms** → 素の状態は120Hz(計測器の妥当性確認) |
| vsync + 55.46Hz | 平均18.02ms **σ5.3** | 12.5 / 16.7 / 20.8 / 25.0ms に分散。**4.165ms(240Hz)格子に残差0.007ms** |
| Immediate + 55.46Hz(本実装の経路) | 平均18.00ms σ2.8 | どの格子にも乗らない(8.333ms格子の残差1.74ms ≒ ランダム相当)。投入時刻をそのままトレース |

読み取れること:

- パネルは**固定120Hzではなく可変している**(vsync時の間隔が240Hz刻みの離散値を取る)。
  ただし55.46Hzに**ロックはしない** — 240/N Hz の離散ステップを混ぜて平均をソースレートに
  合わせる挙動で、瞬間間隔は σ5.3ms 揺れる。ProMotionは任意レート同期ではない。
- Immediate(= `displaySyncEnabled=false`, 本実装が使う経路)では present が
  **120Hzにゲートされない**。フレームはソースタイミングのまま出る。
  この経路でパネルが走査タイミングまで打ち直しているかは presentedTime からは分離できない
  (どちらの場合も投入時刻に一致する)。最終判断は目視 or ハイスピードカメラ。

再現手順とプローブ(Swift/Metal)は `tools/present_probe.swift` を参照。
- 音声(AUDIO パケット, protocol-v0.md 参照)の再生は未実装。cpal クレートで
  リングバッファ再生を追加予定
- CONFIG パケット(音声ソース選択・ArgusX入力切替)のUIも未実装(プロトコル定義済み)
- インターレース: 現状の実装で自然に **weave 合成**になる(プロトコルの規約により
  LINE.line がフルフレーム行のため。検証済み)。**bob 表示**(フィールド縦2倍・
  フィールドレート表示、コーミングなし)は将来の表示オプション
