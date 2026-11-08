# iOS / Android 版 Viewer

Viewer(`client/`)を iPhone / iPad / Android でも動かすための検討と、2026-11-08
時点の状態。ビルド手順は [client/README.md](../client/README.md) の「iOS / Android」。

## 0. 結論の要約

- **同じコードベースのまま動く。** eframe/egui + wgpu は iOS と Android の両方に
  対応しているので、Swift や Kotlin で作り直さずに済む。入口と OS ごとの差分を
  数か所 `cfg` で分けただけで、iOS シミュレータと Android エミュレータの両方で
  **sender_sim の映像を受けて表示するところまで確認した**
- **いちばん大きい壁はネットワーク。** 帯域(Wi-Fi で 130〜390Mbps の UDP)と、
  iOS のブロードキャスト制限。どちらも実機でしか確かめられず、**まだ確かめていない**
- 配布(App Store / Google Play)には、署名・審査・ストア用の形式という別の手間がある。
  今回は手元でビルドして入れるところまで

## 1. 構成

| | iOS / iPadOS | Android |
|---|---|---|
| 入口 | デスクトップと同じ `main()`。winit が中で `UIApplicationMain` を呼ぶ | `android_main`(`src/main.rs`)。OS 標準の NativeActivity が `.so` を読む |
| ビルド | 本体の bin をそのまま `aarch64-apple-ios(-sim)` 向けにビルド | `client/android/` の別パッケージ(cdylib) |
| 梱包 | `packaging/ios/bundle.sh`(Xcode プロジェクトなし) | `packaging/android/build-apk.sh`(Gradle なし) |
| 描画 | Metal | **GLES**(下記 3.3) |
| 音声 | cpal → CoreAudio(RemoteIO) | cpal → AAudio |
| 設定ファイル | `<コンテナ>/Library/Application Support/retrocastx/` | `<内部データ領域>/retrocastx/` |
| ログ | stderr(`simctl launch --console-pty`) | logcat(`adb logcat -s RetroCastX`) |

Android を別パッケージにしたのは、本体の `Cargo.toml` に cdylib の `[lib]` を足すと
**デスクトップの `cargo build` が bin と lib で本体を2回コンパイルする**から。
`client/android/Cargo.toml` は `../src/main.rs` を lib のルートにしているだけで、
ソースは共有している。依存の版は workspace(`client/Cargo.toml` の
`[workspace.dependencies]`)で1か所にまとめた。

## 2. モバイル向けに変えたところ

- **UI**: Tab キーの代わりに映像のタップでパネルを出し入れする。狭い画面
  (幅 720pt 未満 = スマホの縦持ち)ではパネルを全幅にして映像と切り替える。
  ボタン類は指で押せる大きさに広げた(`theme.rs`)。ノッチ・Dynamic Island・
  ホームインジケータの分は余白を空ける
- **出さないもの**: 低遅延全画面(別プロセスで起動し直す仕組み)、新規ウィンドウ、
  配信用の別窓、画に合わせる(窓の大きさを変えられない)。MimicX へのキー転送と
  NIC受信バッファーの確認はもともと macOS / Windows だけ
- **ボードのIP欄**: 手入力した IP へも SUBSCRIBE を送る(ブロードキャストに足す。
  置き換えると他のボードの発見が止まる)。iOS の逃げ道(4.2)
- **スリープ抑止**: iOS は `idleTimerDisabled`。Android は `FLAG_KEEP_SCREEN_ON` を
  起動時に1回だけ立てる(3.4)
- **iOS の音声セッション**: 「再生」カテゴリにする。既定のままだと消音スイッチで
  音が出なくなる

## 3. やってみて分かったこと(解決済み)

### 3.1 iOS シミュレータで起動直後に落ちる

eframe は wgpu のデバイスを `wgpu::Limits::default()` の決め打ちで要求する。
iOS シミュレータの GPU は `max_inter_stage_shader_variables` が 15 で既定の 16 に
届かず、`Limit 'max_inter_stage_shader_variables' value 16 is better than allowed 15`
で終了した(winit は iOS でイベントループを抜けると `exit(0)` するので、
クラッシュログも残らない)。**モバイルでは GPU が申告する上限をそのまま要求する**
ようにした。

### 3.2 Android でどのボードも掴めない

Rust の std は Android で `File::try_lock` を実装していない(常に `Unsupported`)。
ボードの占有(`claim.rs`)がすべて失敗するので、**SUBSCRIBE が一度も出ず、実機の
映像が来ない**。受信スレッドもロックの取り直しで空回りしていた。Android だけ
`libc::flock` を直接呼ぶ。iOS の std は実装済み(シミュレータで確認)。

sender_sim は SUBSCRIBE を無視して送ってくるので、これに気づかないまま
「映った」と判断しかねなかった。

### 3.3 Android エミュレータで Vulkan のスワップチェーンを作れない

`Surface::configure` が `Invalid surface` で panic した(エミュレータの gfxstream)。
映像を板に貼るだけの描画なので、**Android は GLES に固定**した。実機の Vulkan が
使えるなら戻してもよいが、ドライバの当たり外れが大きいので急ぐ理由は無い。

### 3.4 Android で映像を受け始めると操作が効かなくなる(ANR)

スリープ抑止のつもりで、映像を受け始めた時点で `set_window_flags(KEEP_SCREEN_ON)`
を呼んでいた。これを境にイベントループが寝たまま起きなくなり、描画も入力も止まって
5秒で「応答していません」になった。simpleperf の off-CPU 計測で、UI スレッドが
`ALooper_pollOnce` で待ち続けていることまで確認した。**フラグは起動時に1回だけ
立てる**ようにして解消。前面にいる間だけ効くフラグなので、映像の有無で切り替える
意味もあまり無い。

## 4. 残っているハードル(未検証・未対応)

### 4.1 帯域とパケットレート(最大の懸念)

| モード | RGB555 |
|---|---|
| X68000 31kHz(768×512) | 約 390 Mbps |
| PC-98 24kHz(640×400) | 約 230 Mbps |
| X68000 15kHz(512×512) | 約 130 Mbps |

パケットは31kHz等倍で約 35,000 pkt/s。Wi-Fi 6 の端末なら PHY の速度は足りても、
**UDP をこのレートで取りこぼさずに受けられるかは実機で測るまで分からない。**
Windows ですら NIC の受信リングが足りずに落ちた実績がある(client/README.md)。

- 手持ちの手段: **フレーム間引き**(パネルの Tune)と RGB555。どちらも既にある
- **有線がいちばん確実。** iPad と USB-C の iPhone(15 以降)は USB-C の
  Ethernet アダプタが使える。Android も USB Ethernet に対応した機種が多い
- 受信バッファーは Android で 0.5MB しか取れなかった(`rmem_max` の上限、
  エミュレータで確認)。デスクトップは 64MB 要求している。iOS は 8MB

### 4.2 iOS のブロードキャスト

iOS 14 以降、**ブロードキャスト/マルチキャストの送信には Apple の許可
(`com.apple.developer.networking.multicast` entitlement)が要る**。申請して
審査を通る必要がある(用途を説明するフォーム)。無いと発見用の SUBSCRIBE が
送れない。

- 暫定策として「ボードのIP」欄を付けた。IP が分かればユニキャストで繋がる
- ただしボードの既定 IP はリンクローカル(`169.254.x.x`)。**iOS が Wi-Fi の
  上でリンクローカルへユニキャストを出せるか(on-link の経路があるか)は未確認。**
  出せないなら、ボードに静的 IP を焼くか、ゲートウェアに DHCP を入れる必要がある
  (protocol-v0.md の「DHCP」)
- entitlement が下りたら、プロビジョニングプロファイルに入る。`bundle.sh` は
  プロファイルの entitlements をそのまま使うので、スクリプトの変更は要らない
- 「ローカルネットワーク」の許可ダイアログで拒否されると同じ症状になる。
  そのときの案内は送信失敗のメッセージに入れた

Android はブロードキャストの送信に制限が無い(INTERNET 権限だけ)。ボードからの
返信はユニキャストなので、マルチキャストロックも要らない。

### 4.3 アプリが裏へ回ったとき

- iOS は裏へ回るとプロセスが止まる(受信も止まる)。戻ったときに描画面が作り直され、
  購読が復帰するかは未確認。購読は10秒で失効するので、戻れば再送で繋がるはず
- Android は回転や裏へ回るたびに描画面(Surface)が作り直される。`configChanges` で
  回転による Activity の作り直しは止めてあるが、裏から戻ったときの動作は未確認

### 4.4 実機で確かめていないもの

- 音声(iOS の AVAudioSession、Android の AAudio)。エミュレータでは AAudio の
  ストリームが開くところまで確認
- ソフトキーボードでの IP 入力(winit は両 OS で対応しているはず)
- NTSC 復調(コンポジット/S端子)を CPU でやったときの負荷と発熱
- iOS の日本語フォントは Hiragino(41MB の .ttc)を読み込む。メモリに乗せるので、
  古い端末で重いかもしれない

### 4.5 配布

- **iOS**: Apple Developer Program(macOS 版の署名に使っているものと同じ)。
  実機に入れるには開発用のプロビジョニングプロファイル、配るには TestFlight か
  App Store の審査。バンドルIDは今のところ macOS 版と同じ `jp.ohnaka.RetroCastX`
  (`IOS_BUNDLE_ID` で変えられる)。分けるかどうかは決めていない
- **Android**: いまの成果物は APK(デバッグ鍵で署名)。**Google Play への提出は
  AAB 形式が必須**なので、出すなら bundletool で AAB を作る段を足す。対象 API
  レベルの引き上げ要件(毎年)にも追従が要る
- CI(`viewer-release.yml`)にはまだ組み込んでいない。Android の NDK と iOS の
  ターゲットを足すとビルド時間が延びるので、配る段になってから入れる

## 5. 試し方(実機ボードなしで)

sender_sim を相手にする。**同じ LAN に実機ボードとデスクトップ版 Viewer が
居るときは、ポートを変えて `--no-subscribe` にする**(モバイル版が自動でボードを
掴むと、デスクトップ版の映像を奪う)。

iOS シミュレータ(ホストとネットワークを共有する):

```sh
xcrun simctl launch --console-pty booted jp.ohnaka.RetroCastX --port 34700 --no-subscribe
(cd host/python && python3 -m retrocastx.sender_sim --port 34700 --pixfmt rgb555)
```

Android エミュレータ(NAT の内側なので、UDP をリダイレクトする):

```sh
adb emu redir add udp:34601:34600
(cd host/python && python3 -m retrocastx.sender_sim --port 34601 --pixfmt rgb555)
```

エミュレータのブロードキャストは NAT の外へ出ないので、実機ボードを掴むことはない。
