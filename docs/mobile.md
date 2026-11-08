# iOS / Android 版 Viewer

Viewer(`client/`)を iPhone / iPad / Android でも動かすための検討と、2026-11-08
時点の状態。ビルド手順は [client/README.md](../client/README.md) の「iOS / Android」。

## 0. 結論の要約

- **同じコードベースのまま動く。** eframe/egui + wgpu は iOS と Android の両方に
  対応しているので、Swift や Kotlin で作り直さずに済む。入口と OS ごとの差分を
  数か所 `cfg` で分けただけで、iOS シミュレータと Android エミュレータの両方で
  **sender_sim の映像を受けて表示するところまで確認した**。Android は実機
  (Pixel 10)で実ボードの映像を Wi-Fi 越しに受けられた(4.1)
- **いちばん大きい壁はネットワークと処理能力。** Pixel 10 の Wi-Fi(11ac)で
  コンポジット(約450Mbps)は 7〜9% 取りこぼし、時間が経つと CPU も追いつかなく
  なった。iOS はブロードキャストに Apple の許可が要り、申請中(4.2)
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

### 3.5 パネルをスクロールしようとするとスライダーが動く

egui はタッチをマウスとして扱い、指を置いた瞬間に押下を出す。パネルを縦に
スワイプしたら指の下のスライダーがその位置へ飛び、**LED の設定がボードへ
送られた**(Pixel 10)。数値欄(DragValue)も同じ。egui に渡す前に身振りを決め、
最初に縦へ動いたものはホイールに変えて押下を渡さない(`touch.rs`)。横に動かした
ときだけスライダーが動く。

### 3.6 Android でナビゲーションバーがパネルに重なる

targetSdk 35 ではウィンドウがシステムバーの下まで広がる。egui-winit は Android の
余白を egui に渡さず、NativeActivity の content rect も窓全体を返すので、JNI で
`WindowInsets` を読んで余白を空ける。テーマで強制を外す方法
(`windowOptOutEdgeToEdgeEnforcement`)は Android 17 の Pixel では効かなかった。

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

**Pixel 10 での実測(2026-11-08)**: Wi-Fi 5(11ac、受信リンク 650Mbps、RSSI -50)、
入力はコンポジット(8fsc、2048×526、59.88Hz、YC8 で約450Mbps)。

- 発見と接続はできた。Pixel は 192.168.11.x、ボードはリンクローカルの
  169.254.247.111 で、**サブネットが違ってもブロードキャストで見つかり、映像が届く**
- 起動直後: 取りこぼし 7.5〜8.75%、表示 約54fps(元は約60fps)、音のアンダーラン 40回
- しばらく後: 取りこぼし 16.8%、アプリ内のキューでの破棄 約12万、組立の遅延 573ms。
  **Wi-Fi ではなく CPU(コンポジットの色復調)が追いつかなくなっている。**
  発熱による性能低下の可能性がある。RGB 入力やフレーム間引きと比べて、Wi-Fi の
  取りこぼしと CPU 不足を切り分ける必要がある

**Pixel 10 での再計測(受信スレッドの優先度を上げた後、コンポーネント入力 RGB888)**:

- 組立の遅延は 0.3〜0.5ms で、CPU は追いついている。表示は 56〜57fps
- それでも**有線(USB の GbE アダプタ、Realtek r8152)でも 17.7% 落ちる。**
  受け取れているのは Wi-Fi でも有線でも毎秒 約27,000 パケット(約270〜290Mbps)で、
  ボードの送信(約330Mbps、毎秒 約33,000 パケット)に届かない
- 落ちているのは Pixel のカーネルより手前。`/proc/net/snmp` の RcvbufErrors /
  InErrors は増えず、eth0 の rx_drop も 5秒で2個。受信バッファーは 16MB 取れている
  (エミュレータの 0.5MB とは違う)。NIC 側の統計は root が無いと読めない
- **伝送形式を RGB555 にしても変わらなかった**(受信 191.6Mbps に下がったが、
  毎秒 約27,000 パケットで頭打ち、取りこぼしも毎秒 約5,300 のまま)。上限は帯域では
  なく**1秒あたりのパケット数**。パケットを大きくする(ジャンボフレーム、有線のみ)か、
  フレーム間引きで数を減らすしかない。iPhone 15 Pro は同じ有線構成で取りこぼし無し

**iPhone 15 Pro での実測(2026-11-08、USB の GbE アダプタで有線)**: 入力は
コンポジット(8fsc、YC8)。

- 最初は表示 約43fps、組立の遅延 約540ms。Time Profiler で見ると、組立と受信の
  スレッドが時間の約7割を**省電力コア(E コア)**で走っていた。受信スレッドに
  QoS(USER_INTERACTIVE)を付ける処理が macOS 限定だったため
- QoS を iOS にも付けたら、**表示 59.99fps(ばらつき 0.07ms)、組立の遅延 0.1ms、
  受信〜UI 取得 中央 約19ms(1.2フレーム)**。音切れも消えた。Android にも同じ
  趣旨で nice -8 を付けた(Pixel では未計測)
- 8秒間の CPU 時間(修正後): 組立スレッド 3.1秒(1コアの約39%)、うち
  `FrameAssembler::feed`(YC8 の復調と組立)約1.8秒、`TuneMeter` / `WeaveMeter`
  (毎フレームの解析)約1.1秒。受信スレッド 2.4秒(ほぼカーネルの recvfrom)。
  メインスレッド 5.9秒(アプリの UI は1フレーム約0.6ms、テクスチャ転送 約0.6ms で、
  残りは Metal / CoreAnimation 側)

### 4.1.1 GPU へ逃がせるか

**iPhone 15 Pro では今のところ要らない**(QoS の修正だけで実時間に収まった)。
もっと遅い端末で足りなくなったときの候補を、効果の大きい順に:

1. **毎フレームの解析(TuneMeter / WeaveMeter / NoiseMeter / ActiveBox)を間引く。**
   組立スレッドの約3割を使っている。表示には要らず、モバイルでは数フレームに1回で
   足りる。GPU 化よりずっと簡単
2. **YC8 の復調をシェーダーへ移す。** いまは CPU で1ラインずつ復調して RGBA8 を
   作り、毎フレーム 2048×526×4 ≒ 4.3MB を GPU へ送っている。生のサンプル
   (1バイト/サンプル)を R8 テクスチャで送り、画素ごとの復調(副搬送波との掛け算、
   フィルタ)をフラグメントシェーダーでやれば、CPU の復調がほぼ消え、転送も 1/4 に
   なる。バーストの位相推定などライン単位の軽い処理は CPU に残し、結果を小さな
   テクスチャで渡す。Android は GLES で描いているので、コンピュートシェーダーでは
   なくフラグメントシェーダーで書く必要がある。`ntsc.rs`(約2300行)の作り直しに
   なるので大仕事だが、デスクトップでも CPU が軽くなる
3. **受信のシステムコールは GPU では減らせない。** Android(Linux)なら
   `recvmmsg` でまとめて受けて回数を減らせる。iOS には無い

### 4.2 iOS のブロードキャスト

iOS 14 以降、**ブロードキャスト/マルチキャストの送信には Apple の許可
(`com.apple.developer.networking.multicast` entitlement)が要る**。申請して
審査を通る必要がある(用途を説明するフォーム)。無いと発見用の SUBSCRIBE が
送れない。

- 申請は developer.apple.com/contact/request/networking-multicast から出す。
  Apple から「どのプロトコル(IP とポート)で何をやり取りするのか」を聞き返された
  例があるので、パケット形式まで書いた説明を付けた(`ios-multicast-request.pdf`)
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

- 音声。Android(Pixel 10)は AAudio で 48kHz の再生が回っている(アンダーランは
  出る)。実際に耳で聞いての確認と、iOS の AVAudioSession はまだ
- ソフトキーボードでの入力は Android で確認した(数値欄)。iOS は未確認
- NTSC 復調(コンポジット/S端子)の CPU 負荷。Pixel 10 で追いつかなくなる兆候が
  出た(4.1)。復調を GPU に移すか、モバイルでは軽い設定にする必要があるかもしれない
- iOS の日本語フォントは Hiragino(41MB の .ttc)を読み込む。メモリに乗せるので、
  古い端末で重いかもしれない

### 4.5 配布

- **iOS**: Apple Developer Program(macOS 版の署名に使っているものと同じ)。
  実機に入れるには開発用のプロビジョニングプロファイル、配るには TestFlight か
  App Store の審査。ID は全 OS 共通で、配布版 `jp.ohnaka.RetroCastX`、手元の
  開発版 `jp.ohnaka.RetroCastX.dev`(MimicX と同じ使い分け。`APP_IDENTITY=prod`
  を付けたときだけ配布版になる)
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
xcrun simctl launch --console-pty booted jp.ohnaka.RetroCastX.dev --port 34700 --no-subscribe
(cd host/python && python3 -m retrocastx.sender_sim --port 34700 --pixfmt rgb555)
```

Android エミュレータ(NAT の内側なので、UDP をリダイレクトする):

```sh
adb emu redir add udp:34601:34600
(cd host/python && python3 -m retrocastx.sender_sim --port 34601 --pixfmt rgb555)
```

エミュレータのブロードキャストは NAT の外へ出ないので、実機ボードを掴むことはない。
