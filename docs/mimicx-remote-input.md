# MimicX リモート入力 — RetroCastX 側 実装指示書

**対象**: RetroCastX client (`client/`, Rust + eframe/winit)
**相手**: MimicX macOS 版 (実装済み・動作確認済み)
**日付**: 2026-08-11

---

## 1. 何を作るか

RetroCastX のウィンドウにフォーカスがある状態で受け取った**物理キー入力を MimicX へ転送**し、
MimicX 経由で X68000 実機にキー入力を届ける。

### 背景

RetroCastX は実機のアナログ RGB 映像を Mac に表示する。MimicX は実機に HID (キーボード /
ジョイスティック) を送るアダプタのホストアプリ。この 2 つを組み合わせると実機をリモート操作
できるが、**macOS はフォーカスの無いアプリにキーイベントを配送しない**ため、RetroCastX の
画面を見ながら操作しようとすると、キーを打つたびにウィンドウを切り替える必要がある。

そこで RetroCastX が受けたキーをそのまま MimicX へ流す。

### ゲームパッドは対象外 (対応済み)

ゲームパッドは `GCController.shouldMonitorBackgroundEvents = true` を MimicX 側の
`gamepads_darwin` フォークで有効化済みで、**RetroCastX 側の実装は不要**。フォーカスが
無くても MimicX が直接パッド入力を受け取る (macOS 実機で確認済み)。

RetroCastX はゲームパッドを使わない前提なので競合しない。もし将来 RetroCastX が
ゲームパッドを使うようになると、両方のアプリが同時に反応する点に注意。

---

## 2. 経路: CoreMIDI の仮想宛先

MimicX は起動時に **`MimicX Remote Input`** という名前の CoreMIDI 仮想宛先
(virtual destination) を公開している。RetroCastX はここへ MIDI SysEx を送るだけでよい。

### なぜ MIDI なのか (Unix domain socket や TCP ではなく)

MimicX は **App Sandbox が有効**で、以下の制約がある:

- 書き込めるのはコンテナ内だけ → UDS はパス決め打ちが必要で脆い
- コンテナ外のソケットへ connect できない → 逆向き (MimicX がクライアント) も不可
- Release の entitlements に `com.apple.security.network.server` が無い → **TCP の listen ができない**

CoreMIDI なら MimicX が既に使っており、名前で発見でき、追加の権限も要らない。

---

## 3. ワイヤ形式

MimicX のデバイス向けプロトコル (`F0 7D 01 ...`) と混ざらないよう、**sub-id は `0x02`** を使う。
これは「アプリ ⇄ アプリ」の取り決めであり、アダプタには一切送られない。

### 3.1. キー押下 / 解放

```
F0 7D 02 01 <u27-21> <u20-14> <u13-7> <u6-0> <pressed> F7
            └───────── USB HID usage を 7bit×4 (ビッグエンディアン) ─────────┘
                                                          pressed: 1=押下 / 0=解放
```

`usage` は **USB HID usage コード** (W3C の `code` に 1:1 対応)。ページ番号を含む 32bit 値で、
標準キーボードは `0x0007xxxx`。

エンコード:

```
b0 = (usage >> 21) & 0x7F
b1 = (usage >> 14) & 0x7F
b2 = (usage >>  7) & 0x7F
b3 =  usage        & 0x7F
```

MIDI SysEx のデータバイトは 7bit しか使えないためこの形にしている。

**具体例**:

| キー | usage | SysEx (押下) |
|---|---|---|
| `A` | `0x00070004` | `F0 7D 02 01 00 1C 00 04 01 F7` |
| `Escape` | `0x00070029` | `F0 7D 02 01 00 1C 00 29 01 F7` |
| `ArrowLeft` | `0x00070050` | `F0 7D 02 01 00 1C 00 50 01 F7` |

標準キーボードページ (`0x07`) かつ usage 下位が `0x7F` 以下なら先頭 3 バイトは常に
`00 1C 00` になる。ただし**汎用の式で実装すること** (テンキーや将来の拡張で崩れる)。

### 3.2. 全解放

```
F0 7D 02 02 F7
```

押下中のキーをすべて解放させる。**フォーカス喪失時・転送 OFF 時・アプリ終了時に必ず送る**。
送らないとキーが押しっぱなしのまま実機に残る。

---

## 4. 実装の要点

### 4.1. 宛先の発見と自動接続

- CoreMIDI の **destination** から名前が `MimicX Remote Input` のものを探す
- 見つからなければ MimicX が起動していない。**定期的にリトライ**する (数秒間隔で十分)
- MimicX を再起動すると仮想宛先も作り直される。**送信失敗したら探し直す**

Rust では [`midir`](https://crates.io/crates/midir) が使える (CoreMIDI バックエンド)。
追加依存はこれ 1 つで済む。

```rust
use midir::{MidiOutput, MidiOutputConnection};

const PORT_NAME: &str = "MimicX Remote Input";

fn connect() -> Option<MidiOutputConnection> {
    let out = MidiOutput::new("RetroCastX").ok()?;
    let port = out.ports().into_iter()
        .find(|p| out.port_name(p).map(|n| n.contains(PORT_NAME)).unwrap_or(false))?;
    out.connect(&port, "mimicx-remote-input").ok()
}

fn send_key(conn: &mut MidiOutputConnection, usage: u32, pressed: bool) {
    let msg = [
        0xF0, 0x7D, 0x02, 0x01,
        ((usage >> 21) & 0x7F) as u8,
        ((usage >> 14) & 0x7F) as u8,
        ((usage >>  7) & 0x7F) as u8,
        ( usage        & 0x7F) as u8,
        if pressed { 1 } else { 0 },
        0xF7,
    ];
    let _ = conn.send(&msg);
}

fn send_release_all(conn: &mut MidiOutputConnection) {
    let _ = conn.send(&[0xF0, 0x7D, 0x02, 0x02, 0xF7]);
}
```

### 4.2. winit の KeyCode → USB HID usage

**これが RetroCastX 側に必要な唯一の実装らしい実装** (約 120 エントリの変換表)。

winit の `KeyEvent.physical_key` が `PhysicalKey::Code(KeyCode)` を返す。`KeyCode` は
W3C の `code` 文字列と 1:1 対応しており、それがそのまま USB HID usage に対応する。

```rust
fn keycode_to_usage(code: winit::keyboard::KeyCode) -> Option<u32> {
    use winit::keyboard::KeyCode::*;
    let page7 = |n: u32| Some(0x0007_0000 | n);
    match code {
        KeyA => page7(0x04), KeyB => page7(0x05), KeyC => page7(0x06),
        // ... 以下、USB HID Usage Tables (Keyboard/Keypad Page 0x07) に従う
        Digit1 => page7(0x1E), Digit2 => page7(0x1F),
        Enter => page7(0x28), Escape => page7(0x29), Backspace => page7(0x2A),
        Tab => page7(0x2B), Space => page7(0x2C),
        ArrowRight => page7(0x4F), ArrowLeft => page7(0x50),
        ArrowDown => page7(0x51), ArrowUp => page7(0x52),
        ShiftLeft => page7(0xE1), ControlLeft => page7(0xE0),
        _ => None,
    }
}
```

正典は **USB HID Usage Tables の Keyboard/Keypad Page (0x07)**。W3C UI Events
KeyboardEvent code の仕様書にも対応表がある。

**マップに無いキーは送らなくてよい** (MimicX 側も未対応 usage は黙って捨てる)。

### 4.3. 必ず守ること

**リピートは絶対に転送しない。**
`KeyEvent.repeat == true` のイベントは捨てる。MimicX はファームから配られた delay/interval を
使って**アプリ側 Timer でリピートを生成する**設計になっているため、転送すると二重にかかる。
送るのは**押下と解放のエッジだけ**。

**IME を無効化する。**
有効なままだと日本語入力に食われてキーイベントが届かない。

**転送のオン/オフを用意する。**
常時転送だと RetroCastX 自身のショートカット (Cmd+Q など) が使えなくなる。トグルキーか
明示的な「入力キャプチャ中」モードを設け、**抜ける手段を必ず用意すること**。

**OFF / フォーカス喪失 / 終了時に全解放を送る。**
これを怠るとキーが押しっぱなしで実機に残り、操作不能になる。

**転送するのは非フォーカス時だけでよい。**
MimicX にフォーカスがあるときは MimicX 自身がキーを受け取るので、両方送ると二重入力になる。

---

## 5. 動作確認の方法

MimicX 側にはコマンドラインから SysEx を撃つツールがある。RetroCastX の実装前に、
この経路が生きていることを確認できる。

```sh
cd ~/work/github/MimicX/MimicX-firmware
swiftc -O tools/midiprobe.swift -o tools/midiprobe   # 初回のみ

# A キーを短く押して離す (第 2 引数は待ち秒数)
./tools/midiprobe "MimicX Remote Input" 0.05 F0 7D 02 01 00 1C 00 04 01 F7
./tools/midiprobe "MimicX Remote Input" 0.05 F0 7D 02 01 00 1C 00 04 00 F7
```

**前提**: MimicX が起動し、アダプタに接続して**キーボード操作画面に入っている**こと。
リモート入力のハンドラはキーボード画面の body が生存している間だけ動く
(Combined セッションではジョイスティック画面を表示中でも生きている)。

**注意**: 押下と解放の間隔を空けすぎるとリピートがかかる (実キーボードを押しっぱなしに
したのと同じ)。1 文字だけ入力したいときは 50ms 程度で解放すること。

---

## 6. MimicX 側の既知の制約

**PC キーボードモードの記号横取りは効かない。**
MimicX には macOS の刻印どおりに記号を打つモードがあるが、これは `event.character`
(OS が解決した文字) に依存している。転送されるのは物理キー位置だけなので、
**リモート入力は常に X68k 配列モード相当の解釈**になる。

記号の互換が必要になったら、転送プロトコルに「文字」を足す拡張は可能
(MimicX 側 `protocol.dart` の `RemoteInputSysEx` に cmd を追加する)。

---

## 7. RetroCastX 側の実装(2026-08-11 実装済み)

`client/src/remote_input.rs` に集約。依存は `midir` 1つで、macOS だけに閉じている
(他のOSでは何もしないスタブ)。使い方は `client/README.md` を参照。

**転送のON/OFF は ⌘+Shift+ESC**(F12 も受け付ける)。⌘ は X68000 のキーボードに
無いので、組み合わせにすれば実機へ送りたい打鍵と衝突しない。**ESC 単独は横取り
せず実機へ送る**。F12 は単独で効くが、載っていない/打ちにくいキーボードがある
ので主にはしない。

**Shift まで要るのは、⌘+ESC が macOS のシステムショートカットに取られていて
アプリに届かないため。** `AppleSymbolicHotKeys` の id 73 が keycode 53 + Command で
有効になっており、WindowServer がアプリより先に奪うので `addLocalMonitor` でも
何も観測できない(実機で確認)。**この手の「特定の組み合わせだけ無反応」は
アプリ側からは原因が見えない**ので、別の組み合わせに変えるときは先に
取られていないかを確かめること:

```sh
defaults export com.apple.symbolichotkeys - | plutil -convert json -o - -
```

`parameters` は `(文字, キーコード, 修飾)`。ESC は 53、⌘ は 0x100000、Shift は
0x20000。⌘+Shift+ESC は空きだが、**⌘+⌥+ESC は強制終了なので使えない**
(こちらはこの表に出ない)。

組み合わせの検出は `ToggleDetect` にまとめた。素直に書くと2つ壊れる:

- **押下の瞬間だけを拾う。** OS のキーリピートは押しっぱなしで押下イベントを
  撒き続けるので、そのまま数えると1回の打鍵で何度もトグルする。
- **横取りした押下に対応する解放も、修飾の状態に関係なく横取りする。**
  「⌘が押されているか」で解放を判定すると、⌘ を先に離したときに解放を
  取りこぼして押下ラッチが残る。さらに **macOS は ⌘ を押しながらの keyUp を
  配送しないことがある**ので、⌘ が離れている間はラッチを戻す
  (`rearm_chord`)。これが無いと2回目の ⌘+Shift+ESC が効かない。

**⌘ 付きの打鍵は Mac の操作として扱い、実機へ送らない**(⌘Q で終了、⌘H で隠す、
など)。X68000 のキーボードに ⌘ が無いので、⌘ 併用が実機向けであることはない。
ただし**解放は ⌘ の有無に関わらず送る** — 押している途中で ⌘ を足すと、解放だけ
落ちて実機にキーが押しっぱなしで残る。⌘ 自体も送らない。

⌘Q での終了でも全解放は走る。eframe は macOS の ⌘Q を `exiting()` で受けて
`on_exit()` を呼ぶので、そこで `set_enabled(false)` → 全解放が出る
(設定ファイルが書き直されることで到達を確認した)。

転送中は画面左上に赤いバッジが出て、**それ自体がボタン**になっている。
キーボードによっては切り替えの打鍵が難しく、パネルを隠しているとキー以外の
手段が無くなるので、キーに頼らない抜け道を用意した。

### 通常モードは AppKit のイベントを直接見る

**egui のキーイベントでは足りない。** eframe 経路で手に入るのは egui が変換した
`Key` までで、これは物理位置を表すと言いながら次を落とす:

- JIS の `¥`(International3)/ `_`(International1)/ かな / 英数 に対応する
  variant が無い → **イベントそのものが作られず、押しても何も起きない**
- テンキーを最上段の数字に潰す(`Numpad0` も `Digit0` も `Num0`)
- CapsLock も無い

egui 0.36 でも同じ。eframe には winit の生イベントに触れるフックが無い
(`raw_input_hook` は変換後の `RawInput`)ので、`src/keytap.rs` で
**`NSEvent.addLocalMonitorForEventsMatchingMask:`** を張って OS の層から取る。

- 見るのは**このアプリに配送されるイベントだけ**なので、CGEventTap と違って
  アクセシビリティ権限が要らない
- `NSApplication.sendEvent:` の中で通常の配送より先に呼ばれるので、
  **⌘ 付きの組み合わせを AppKit のキーイコーレント処理(メニュー)に
  横取りされる前に受け取れる**
- ハンドラが null を返すとイベントを捨てられる。転送中は捨てて egui へ渡さない。
  渡すと egui が Tab / 矢印 / Escape をパス開始時にフォーカス移動として解釈し、
  パネルのウィジェットにフォーカスが移って転送が中断される
- ただし **⌘ を押している間は捨てない**。winit は既定メニューに ⌘Q(`terminate:`)を
  入れているので、飲み込むとメニューに届かず**転送中はアプリを終了できなくなる**。
  X68000 のキーボードに ⌘ は無いので、⌘ 付きの打鍵が実機向けであることもない
- 捨てたイベントは winit に届かないので**再描画要求も出ない**。監視ハンドラから
  `request_repaint()` を呼ばないと、次の定期再描画(250ms)まで処理されず
  キー入力が目に見えて遅れる
- リピートは `NSEvent.isARepeat` で入口で落とす(MimicX が自前の Timer で
  リピートを作るので、送ると二重にかかる)

**修飾キーは `keyDown`/`keyUp` を出さない。** macOS は Shift / Control / Option /
Command / CapsLock を **`flagsChanged`** として送る。KeyDown/KeyUp だけ監視して
いたので、**Shift も Control も ⌘ も1つも届いていなかった**(⌘+Shift+ESC が効かない
のもこれが根)。監視を1本足して解決した。ここは「一族まるごと無反応」という
形で出るので、単体テスト(`modifiers_reach_a_usage`)で塞いである。

`flagsChanged` には押下/解放の区別が無いので、いまのフラグにそのキーのビットが
立っているかで判定する。**一般の `NSEventModifierFlagShift`(1<<17)は左右
どちらでも立つので使えない** — 両方押している最中に片方だけ離しても消えず、
解放を取りこぼす。macOS が別に持つデバイス依存ビット(IOKit の
`NX_DEVICE*KEYMASK`: 左Shift 0x02 / 右Shift 0x04 / 左Ctrl 0x01 / 右Ctrl 0x2000 …)
なら左右が独立して見える。

修飾キーのイベントは **egui へも渡す**(捨てると egui が ⌘ / Shift の状態を
見失う)。修飾キー単独で egui が何かすることは無いので渡して困らない。

CapsLock はロックするキーで、macOS は状態が変わったときだけ知らせる。押しっぱなし
として送ると MimicX の Timer がリピートを作って実機側で切り替わり続けるので、
**1打(押下→解放)として送る**。実機側も1回押すごとに切り替わるので釣り合う。
実機での確認は未了。

仮想キーコード → `KeyCode` の変換は winit の `PhysicalKey::from_scancode` を使う
(macOS で公開されている)。ただし **winit は macOS の JIS キーを未対応のまま
残していて**(`platform_impl/macos/event.rs` にコメントアウトで並んでいる)、
ろ(0x5e)・テンキーの `,`(0x5f)・英数(0x66)・かな(0x68)が `Unidentified` に
なるので、その4つだけ `keycode_from_mac_vk` で補う。かな/英数 は Apple の JIS
キーボードが USB で送るのと同じ **LANG1 / LANG2** に当てている(PC/AT の
「かなカナ」= International2 とは別の usage なので、MimicX 側がどちらを見て
いるかは合わせる必要がある)。

`--fullscreen` は winit を直接使うので監視は要らないが、上記の JIS 4キーは
winit 側で落ちるので `PhysicalKey::Unidentified(NativeKeyCode::MacOS(vk))` を
同じ関数で補う。usage の表(`usage_from_keycode`)は1つだけ。

### 検証

- ワイヤ形式は単体テストで固定した(`cargo test`、19件)。本書の具体例3件と、
  データバイトが7bitに収まること。
- 組み合わせの検出も単体テストで固定した。ESC 単独が横取りされないこと、
  リピートで再トグルしないこと、解放が来なくても2回目が効くこと。
- 修飾キーと左右の区別も単体テストで固定した(`modifiers_reach_a_usage` /
  `left_and_right_use_distinct_bits`)。
- JIS キーとテンキーの usage も単体テストで固定した(`mac_vk_covers_jis_keys`)。
  ここが None だと「押しても何も起きない」という形で出るので、表に穴が
  空いたら落ちるようにしてある。
- `--mimicx-probe` で GUI 無しに経路を確かめられる。CoreMIDI の宛先一覧を出し、
  接続して A を押して離す。「宛先が無い」のか「送れているのに MimicX 側で
  止まっている」のかの切り分け用。実機で `MimicX Remote Input` の発見と送信の
  成功を確認済み。
- `--log-keys` で受け取った物理キー(仮想キーコード / KeyCode / usage)を
  stderr に出す。**そもそもアプリに届いていない**のか、届いているが表に
  無いのかが分かる。表に無いキーは `(未知のキー)` として仮想キーコードだけ出る。

## 8. 参照

- MimicX 側の実装とプロトコル定義:
  `~/work/github/MimicX/MimicX-app/lib/protocol.dart` の
  `RemoteInputSysEx` / `RemoteInputEvent` / `kRemoteInputPortName`
- 受信と仮想宛先の公開: 同 `lib/midi_service.dart` の `startRemoteInput()`
- キーの流し込み先: 同 `lib/x68k_keyboard_page.dart` の `_handleRemoteInput()`
- 動作確認ツール: `~/work/github/MimicX/MimicX-firmware/tools/midiprobe.swift`

MimicX 側は macOS 実機で end-to-end 確認済み
(外部から SysEx 送信 → MimicX が受信・変換 → アダプタへ MIDI 送信 → X68000 実機に入力、
TARGET_RX の応答も確認)。
