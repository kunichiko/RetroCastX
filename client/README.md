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

**アプリが自分で気付いて知らせる**(`src/netcheck.rs`)。README を読まなくても
分かるように、右パネルの Stats に警告とコピーできるコマンドを出す。二段構え:

1. **設定を読む。** ボードのIPから `GetBestInterfaceEx` で経路上のNICを引き
   (全アダプタを見ると Wi-Fi と有線が両方生きている機械で誤判定する)、
   LUID → GUID → レジストリの `*ReceiveBuffers` を読む。**読むだけなので管理者
   権限は不要**(実機の ACL は `BUILTIN\Users: ReadKey`)
2. **実測のロスを見る。** `*ReceiveBuffers` は NDIS の advanced property で、
   **ドライバが公開していなければ存在しない**。実機の15アダプタのうち持っていたのは
   Intel の有線だけで、TP-Link の Wi-Fi は持っていなかった。**無いことを「小さい」と
   扱うと嘘の警告になる**ので、その場合は黙って実測のロス率だけで判断する

`--netcheck <ボードのIP>` で GUI なしに結果だけ見られる。実機(Windows 11 26200)
で4通りとも確認済み: Intel有線 → `Known{2048}`「足りています」、同じNICを一時的に
256 にすると「小さすぎます」+ アダプタ名を埋めたコマンド、TP-Link Wi-Fi →
`Unsupported`、Bluetooth PAN → `Unknown`。値は **REG_SZ の文字列**で入る
(DWORD 決め打ちでは読めない)。

一時変更の検証は `Set-NetAdapterAdvancedProperty ... -NoRestart` で行う。
レジストリだけ書いてドライバを再起動しないので、**リンクが落ちず**(SSH越しでも
切れない)実際の受信性能も変わらないまま、読み取り側の判定だけ確かめられる。

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

    RetroCastX_macos-<version>.zip          「RetroCast X.app」universal(arm64+x86_64)、署名済み
    RetroCastX_windows-<version>.zip        RetroCastX.exe + README-first.txt
    RetroCastX_windows-setup-<version>.exe  同じ中身のインストーラ(Inno Setup、未署名)

macOSの `.app` は `packaging/macos/bundle.sh` が組み立てる。**CIのYAMLにロジックを
置かず**スクリプトにしてあるので、手元で同じものを作って試せる:

```sh
cargo build --release
packaging/macos/bundle.sh target/release/retrocastx-viewer 0.1.0 0 /tmp/out
open "/tmp/out/RetroCast X.app"
```

**Windowsはインストーラ版も作る**(`client/packaging/windows/retrocastx.iss`、
Inno Setup 6。`windows-latest` に preinstall されている)。中身は zip と同じで、
違いは**固定パスへ入る**ことだけだが、そこに意味がある:

> Windowsのファイアウォール/ローカルネットワークの許可は**実行ファイルのパス
> 単位**なので、zip を展開する場所を変えたり exe を置き換えたりするたびに
> 許可を聞かれ、**許可するまで映像が来ない**(数秒間まったく反応が無いように
> 見える)。インストーラなら次回以降その入れ替わりが起きない。

zip とインストーラで中身が食い違わないよう、**組み立ては1か所**(ワークフローの
`Package zip` が作る `dist/RetroCastX`)にして、ISCC はそれをそのまま入れる。
`AppId` の GUID は**一度決めたら変えない**(アップグレード判定とアンインストール
の識別が壊れる)。手元で作るときは .iss 冒頭のコメントの手順で。

**macOS はアイコンに角丸マスクをかけない。** 丸みも余白も素材側で作る決まりなので、
`make-icons.sh` は Apple の配置規則(1024の画布に本体824×824、角丸半径185.4、周囲は
透明)に合わせて `.icns` を作る。全面ベタのまま入れると **Dock で四角いアイコンに
なる**(Finderは小さく表示するので黒い角に気づけない。実際これで一度作り直した)。

**Windows も同じ絵を使う**(見た目を揃える方針)。Windowsのアイコンは全面ベタが
普通なので、他のアプリと並ぶと1割ほど小さく見える。全面ベタに戻したくなったら
`make-icons.sh` の ico / PNG の生成元を master に変えるだけでよい。

アイコンは `packaging/AppIcon.png`(1024×1024)が master で、そこから
`packaging/make-icons.sh` が3つを作る。**生成物もコミットしてある**(CIに画像変換
ツールを入れない方針。作り直したときは一緒にコミットする):

    packaging/macos/AppIcon.icns    .app に入る(bundle.sh が拾う)
    packaging/windows/AppIcon.ico   exe に埋め込む(build.rs が拾う)

Windows版はコンソール窓を出さずに起動する(`#![windows_subsystem = "windows"]`)。
コンソールから起動したときだけ `AttachConsole(ATTACH_PARENT_PROCESS)` で親の
コンソールへ出力を戻すので、`--headless` の表示は失われない。

**ただしスクリプトから呼ぶときは待たせること。** GUIサブシステムの実行ファイルは
シェルが終了を待たないので、`& exe --netcheck ...` や `cmd /c exe > log` では
**出力が間に合わず空になる**(実機でこれに引っかかった。exit code は 0 なので
気付きにくい)。PowerShell なら:

```powershell
Start-Process -FilePath .\RetroCastX.exe -ArgumentList "--netcheck","192.168.11.50" `
              -Wait -NoNewWindow -RedirectStandardOutput out.log
```

**exeへの埋め込みだけではタスクバーに出ない。** winit はウィンドウクラスを
`hIcon: 0` で登録し、`window_icon` が `None` なら明示的にアイコンを外すので、

    Explorer のファイルのアイコン → exe に埋め込んだリソース(build.rs)
    タイトルバー / タスクバー     → 実行時に設定する(src/appicon.rs)
    macOS の Dock                → .app の CFBundleIconFile(bundle.sh)

の3つが別経路になる。実行時用に `packaging/AppIcon-256.png` を埋め込んでいる
(1024pxを毎起動デコードするのは無駄なので256px)。

macOS で Dock が既定アイコンのままなら、**アイコン無しの版を一度起動したときの
キャッシュ**が残っている:

```sh
touch "/path/to/RetroCast X.app" && killall Dock
```

未対応(必要になったら):

- **Windowsの署名**: 証明書が無いので未署名。SmartScreenの警告は同梱の
  README-first.txt で案内している
- **ゲートウェア/Python側のCI**: このワークフローはViewerだけを見る

## 構成

- `src/protocol.rs` — プロトコルv0のパース(リファレンスは `host/python/retrocastx/protocol.py`)
- `src/assembler.rs` — LINE→フレーム再構成(RGBA8出力、ロス/迷子ライン統計)
- `src/receiver.rs` — UDP受信スレッド + SUBSCRIBEキープアライブ(2秒ごと)
- `src/main.rs` — eframe UI(映像表示 + モード/統計/発見ボードのサイドパネル)
- `src/remote_input.rs` — MimicX へのキー転送(CoreMIDI SysEx)
- `src/keytap.rs` — 物理キーの取り出し(macOS の AppKit イベント監視)
- `src/netcheck.rs` — NIC受信バッファーの確認(Windows、レジストリを読む)

## MimicX と組み合わせてリモート操作する(macOS)

映像は RetroCastX、キー入力は MimicX(HIDアダプタのホストアプリ)が実機へ送る。
macOS はフォーカスの無いアプリにキーイベントを配送しないので、RetroCastX の画面を
見ながら操作するには**RetroCastX が受けたキーを MimicX へ転送する**必要がある。
プロトコルの仕様は `docs/mimicx-remote-input.md`。

使い方:

1. MimicX を起動し、アダプタに接続して**キーボード操作画面に入る**
   (リモート入力のハンドラはこの画面が生きている間だけ動く)
2. RetroCastX の右パネル「Remote input」で転送を入れる、または **⌘+Shift+ESC**
3. 転送中は画面左上に赤いバッジが出る。**⌘+Shift+ESC かバッジのクリック**で解除

- 切り替えは **⌘+Shift+ESC**(F12 も受け付ける)。⌘ は X68000 のキーボードに無いので、
  組み合わせにすれば実機へ送りたい打鍵と衝突しない。**ESC 単独は実機の ESC**
  として送られる。
  Shift まで要るのは **⌘+ESC が macOS のシステムショートカットに取られていて
  アプリに届かない**ため(`AppleSymbolicHotKeys` id 73)。別の組み合わせに
  変えるときは、先に取られていないかを確かめること:

  ```sh
  defaults export com.apple.symbolichotkeys - | plutil -convert json -o - - \
    | python3 -c "import sys,json;[print(k, v['value']['parameters']) for k,v in json.load(sys.stdin)['AppleSymbolicHotKeys'].items() if v.get('enabled') and 'parameters' in v.get('value',{})]"
  ```

  出力の `parameters` は `(文字, キーコード, 修飾)`。ESC は 53、⌘ は 0x100000。
- 転送中は Tab / B などの RetroCastX 自身のキー操作もすべて実機へ行く。
  マウス操作(パネルやバッジ)は効いたままなので、キーが届かない環境でも戻れる。
- **⌘ 付きの打鍵は Mac の操作**として扱い、実機へは送らない。転送中でも
  ⌘Q で終了、⌘H で隠す、などがそのまま効く(終了時は全解放が走るので、
  キーが押しっぱなしで実機に残らない)。
- 起動時は必ずOFF(保存しない)。キーが黙って実機へ流れる状態で始まらないように。
- 数値欄(pll_div など)へ打ち込んでいる間は転送を止めて UI に譲る。欄から
  外れれば転送に戻る。
- ゲームパッドは転送しない。MimicX がフォーカス無しでも直接受け取るので、
  転送すると二重入力になる。
- Shift / Control / Option の左右、テンキー、JIS 配列のキー(¥ / ろ / かな /
  英数)も区別して送る。egui のキーイベントはこれらを落とし、修飾キーは macOS
  では keyDown を出さないので、AppKit のイベントを直接見ている(`src/keytap.rs`)。
- `--fullscreen` でも ⌘+Shift+ESC で転送できる。転送中の素の ESC は実機の ESC として
  送るので、終了するには先に ⌘+Shift+ESC で転送を切る。
- 経路の切り分けに `--mimicx-probe`。CoreMIDI の宛先一覧を出し、A を
  押して離すところまでを GUI 無しで試す。
- 「このキーが効かない」ときは `--log-keys`。受け取った物理キーと修飾の状態、
  送る usage を stderr へ出すので、**そもそもアプリに届いていない**のか、
  届いているが転送していないのかが分かる(OS が横取りする組み合わせを疑うとき)。

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
