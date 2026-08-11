//! MimicX へのキー転送(リモート入力)。
//!
//! RetroCastX のウィンドウで受けた**物理キー**を MimicX へ流し、MimicX 経由で
//! 実機(X68000)へ HID キー入力として届ける。macOS はフォーカスの無いアプリに
//! キーイベントを配送しないので、映像を見ている RetroCastX 側で受けて転送する
//! しか方法がない(そうでないとキーを打つたびにウィンドウを切り替えることになる)。
//!
//! 経路は CoreMIDI の仮想宛先 `MimicX Remote Input`。MimicX は App Sandbox が
//! 有効で、コンテナ外のソケットへ connect できず、Release の entitlements に
//! `com.apple.security.network.server` が無いので TCP の listen もできない。
//! CoreMIDI なら MimicX が既に使っていて、名前で発見でき、追加の権限も要らない。
//!
//! ゲームパッドは対象外。MimicX 側が `GCController.shouldMonitorBackgroundEvents`
//! でフォーカス無しでも直接受け取るので、ここで転送すると二重入力になる。
//!
//! プロトコルの仕様は docs/mimicx-remote-input.md。

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use winit::keyboard::KeyCode;

/// MimicX が公開している CoreMIDI 仮想宛先の名前
pub const PORT_NAME: &str = "MimicX Remote Input";

/// 宛先が見つからないときの再探索間隔。MimicX を後から起動しても繋がるように、
/// 諦めずに繰り返す(再起動すると仮想宛先も作り直されるため)
const RETRY: Duration = Duration::from_secs(2);

// ワイヤ形式。MimicX のデバイス向けプロトコル(sub-id 0x01)と混ざらないよう
// sub-id は 0x02 を使う。これは「アプリ ⇄ アプリ」の取り決めで、アダプタには
// 一切送られない。
const SYSEX_START: u8 = 0xF0;
const SYSEX_END: u8 = 0xF7;
const MANUFACTURER: u8 = 0x7D; // 非商用・実験用に予約されたID
const SUB_ID_APP: u8 = 0x02;
const CMD_KEY: u8 = 0x01;
const CMD_RELEASE_ALL: u8 = 0x02;

/// `F0 7D 02 01 <u27-21> <u20-14> <u13-7> <u6-0> <pressed> F7`
///
/// SysEx のデータバイトは 7bit しか使えないので、USB HID usage(ページ番号を
/// 含む32bit値)を 7bit×4 のビッグエンディアンで送る。
fn key_message(usage: u32, pressed: bool) -> [u8; 10] {
    [
        SYSEX_START,
        MANUFACTURER,
        SUB_ID_APP,
        CMD_KEY,
        ((usage >> 21) & 0x7F) as u8,
        ((usage >> 14) & 0x7F) as u8,
        ((usage >> 7) & 0x7F) as u8,
        (usage & 0x7F) as u8,
        pressed as u8,
        SYSEX_END,
    ]
}

/// `F0 7D 02 02 F7` — 押下中のキーをすべて解放させる
const RELEASE_ALL: [u8; 5] = [SYSEX_START, MANUFACTURER, SUB_ID_APP, CMD_RELEASE_ALL, SYSEX_END];

// --- CoreMIDI 送信口 ------------------------------------------------------
//
// MimicX は macOS 版だけで、CoreMIDI も同一マシン内の経路なので、
// 依存(midir)ごと macOS に閉じる。他のOSでは何もしないスタブになる。

#[cfg(target_os = "macos")]
mod backend {
    use midir::{MidiOutput, MidiOutputConnection};

    pub const AVAILABLE: bool = true;

    pub struct Port(MidiOutputConnection);

    impl Port {
        /// 名前が一致する CoreMIDI の宛先(destination)を探して開く。
        /// MimicX が起動していなければ宛先そのものが存在しない。
        pub fn open(name: &str) -> Result<Self, String> {
            let out = MidiOutput::new("RetroCastX").map_err(|e| e.to_string())?;
            let port = out
                .ports()
                .into_iter()
                .find(|p| out.port_name(p).is_ok_and(|n| n.contains(name)))
                .ok_or_else(|| format!("{name} が見つかりません(MimicX 未起動)"))?;
            out.connect(&port, "mimicx-remote-input")
                .map(Port)
                .map_err(|e| e.to_string())
        }

        pub fn send(&mut self, msg: &[u8]) -> Result<(), String> {
            self.0.send(msg).map_err(|e| e.to_string())
        }
    }

    /// CoreMIDI の宛先の名前をすべて返す(--mimicx-probe の表示用)
    pub fn destinations() -> Vec<String> {
        let Ok(out) = MidiOutput::new("RetroCastX probe") else { return Vec::new() };
        out.ports().iter().filter_map(|p| out.port_name(p).ok()).collect()
    }
}

#[cfg(not(target_os = "macos"))]
mod backend {
    pub const AVAILABLE: bool = false;

    pub struct Port(());

    impl Port {
        pub fn open(_name: &str) -> Result<Self, String> {
            Err("リモート入力は macOS 専用です".into())
        }

        pub fn send(&mut self, _msg: &[u8]) -> Result<(), String> {
            Ok(())
        }
    }

    pub fn destinations() -> Vec<String> {
        Vec::new()
    }
}

/// この環境でリモート入力が使えるか(macOS のみ)
pub const AVAILABLE: bool = backend::AVAILABLE;

/// 経路が生きているか、GUIを起動せずに確かめる(`--mimicx-probe`)。
///
/// 「キーが効かない」ときに、CoreMIDI の宛先が無いのか、送れているのに
/// MimicX 側で止まっているのかを切り分けるために要る。MimicX を起動し、
/// アダプタに接続して**キーボード操作画面に入っている**こと(リモート入力の
/// ハンドラはキーボード画面の body が生きている間だけ動く)。
pub fn probe() -> bool {
    println!("CoreMIDI destinations:");
    let all = backend::destinations();
    if all.is_empty() {
        println!("  (なし)");
    }
    for name in &all {
        let hit = if name.contains(PORT_NAME) { " ←" } else { "" };
        println!("  {name}{hit}");
    }
    let mut ri = RemoteInput::default();
    ri.set_enabled(true);
    ri.update(true);
    if !ri.connected() {
        eprintln!("FAIL: {}", ri.status());
        return false;
    }
    println!("connected: {PORT_NAME}");
    // A を短く押して離す。間隔を空けすぎると MimicX 側のリピートがかかるので
    // 50ms 程度で解放する(実キーボードを押しっぱなしにしたのと同じになる)
    let a = usage_from_keycode(KeyCode::KeyA).expect("A は必ず表にある");
    ri.key(a, true);
    std::thread::sleep(Duration::from_millis(50));
    ri.key(a, false);
    if !ri.connected() {
        eprintln!("FAIL: {}", ri.status());
        return false;
    }
    println!("sent: A 押下→解放 (usage {a:#010x})");
    println!("OK: MimicX のキーボード画面に 'a' が入っていれば経路は生きています");
    true
}

/// キー転送の状態。押下中のキーを覚えていて、転送を止めるときに必ず解放する。
pub struct RemoteInput {
    /// ユーザーが選んだ転送ON/OFF
    enabled: bool,
    /// 実際に転送している状態。enabled でもフォーカスが無い間などは落ちる
    active: bool,
    port: Option<backend::Port>,
    /// 次に宛先を探し直す時刻
    next_try: Instant,
    /// 押下として送った usage。転送を止めるときにこれを解放する
    pressed: BTreeSet<u32>,
    /// UI に出す状態(接続の成否や理由)
    status: String,
}

impl Default for RemoteInput {
    fn default() -> Self {
        Self {
            enabled: false,
            active: false,
            port: None,
            next_try: Instant::now(),
            pressed: BTreeSet::new(),
            status: if AVAILABLE { "off".into() } else { "macOS 専用".into() },
        }
    }
}

impl RemoteInput {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 実際に転送している最中か。画面の表示に使う
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn connected(&self) -> bool {
        self.port.is_some()
    }

    pub fn held(&self) -> usize {
        self.pressed.len()
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_enabled(&mut self, on: bool) {
        if self.enabled == on {
            return;
        }
        self.enabled = on;
        if !on {
            // OFF にした時点で必ず全解放する。送らないとキーが押しっぱなしの
            // まま実機に残り、操作不能になる
            self.set_active(false);
            self.status = "off".into();
        }
    }

    pub fn toggle(&mut self) {
        self.set_enabled(!self.enabled);
    }

    /// 毎フレーム呼ぶ。`want` は「いま転送してよいか」(フォーカスがある、
    /// UIがキーボードを使っていない、など呼び出し側の条件)。
    ///
    /// 転送できない状態へ落ちるときに全解放を送るのはここ。フォーカスを失うと
    /// 以後のキー解放イベントが届かないので、ここで送らないと押しっぱなしになる。
    pub fn update(&mut self, want: bool) {
        let want = want && self.enabled;
        if want != self.active {
            self.set_active(want);
        }
        if !self.active || self.port.is_some() {
            return;
        }
        // MimicX は後から起動されることも、再起動されることもある。見つかるまで
        // 諦めずに探し直す
        if Instant::now() >= self.next_try {
            self.try_connect();
        }
    }

    fn set_active(&mut self, on: bool) {
        if on {
            self.active = true;
            self.next_try = Instant::now(); // 待たずに探す
            self.try_connect();
        } else {
            // 順序が大事: active を落とす前に解放を送る
            self.release_all();
            self.active = false;
        }
    }

    fn try_connect(&mut self) {
        match backend::Port::open(PORT_NAME) {
            Ok(p) => {
                self.port = Some(p);
                self.status = "接続".into();
            }
            Err(e) => {
                self.port = None;
                self.next_try = Instant::now() + RETRY;
                self.status = e;
            }
        }
    }

    /// 押下/解放を1つ送る。**リピートは呼ばないこと**(MimicX が
    /// ファーム由来の delay/interval でアプリ側 Timer からリピートを作るので、
    /// 転送すると二重にかかる)。送るのは押下と解放のエッジだけ。
    pub fn key(&mut self, usage: u32, pressed: bool) {
        if !self.active {
            return;
        }
        if pressed {
            if self.pressed.contains(&usage) {
                return; // 既に押下中(リピートを取りこぼしても二重に送らない)
            }
            // 送れた分だけ覚える。届いていない押下を覚えると、解放の帳尻が合わない
            if self.send(&key_message(usage, true)) {
                self.pressed.insert(usage);
            }
        } else {
            if !self.pressed.remove(&usage) {
                return; // 押下を送っていないキーの解放は送らない
            }
            self.send(&key_message(usage, false));
        }
    }

    /// 押下中のキーをすべて解放させる。フォーカス喪失・転送OFF・終了時に必ず送る
    pub fn release_all(&mut self) {
        self.pressed.clear();
        // 繋がっていなければ相手も押下状態を持っていない。繋がっているなら
        // 押下を覚えていなくても送る(解放が落ちて取りこぼしている可能性がある。
        // MimicX は自前の Timer でリピートを続けるので、残ると打ち続けになる)
        if self.port.is_some() {
            self.send(&RELEASE_ALL);
        }
    }

    /// 送れたら true。宛先が居ない・死んでいたら false
    fn send(&mut self, msg: &[u8]) -> bool {
        // 繋がっていなければここで張りに行く。フルスクリーン経路には毎フレームの
        // 呼び出しが無く(winit はイベントが来たときだけ起きる)、update() の
        // リトライだけに任せると MimicX を後から起動しても繋がらない
        if self.port.is_none() && Instant::now() >= self.next_try {
            self.try_connect();
        }
        let Some(port) = self.port.as_mut() else { return false };
        let Err(e) = port.send(msg) else { return true };
        // MimicX を再起動すると仮想宛先が作り直され、古い接続は死ぬ。
        // 張り直してこの1打を送り直す(落とすと押しっぱなしになりうる)。
        // 相手が入れ替わったので、こちらが覚えている押下状態も捨てる。
        self.port = None;
        self.pressed.clear();
        self.status = format!("送信失敗({e})→再接続");
        self.try_connect();
        match self.port.as_mut() {
            Some(port) => match port.send(msg) {
                Ok(()) => true,
                Err(e) => {
                    self.port = None;
                    self.next_try = Instant::now() + RETRY;
                    self.status = format!("送信失敗({e})");
                    false
                }
            },
            None => false,
        }
    }
}

impl Drop for RemoteInput {
    /// アプリ終了時にも全解放する。これを怠るとキーが押しっぱなしで実機に残る
    fn drop(&mut self) {
        self.release_all();
    }
}

// --- 転送のON/OFF操作 -----------------------------------------------------
//
// 常時転送だと RetroCastX 自身のキー操作(Tab でパネル、B で枠)が使えなく
// なるので、抜ける手段を必ず用意する。
//
// 主は **⌘+Shift+ESC**。⌘ は X68000 のキーボードに無いので、組み合わせにすれば
// 実機へ送りたい打鍵と衝突しない。**ESC 単独は実機の ESC として送る**。
//
// Shift まで要るのは、**⌘+ESC が macOS のシステムショートカットに取られていて
// アプリに届かない**ため(`AppleSymbolicHotKeys` id 73。WindowServer が
// アプリより先に奪うので、こちらでは何も観測できない)。実機で確認した。
// F12 も単独で受け付けるが、載っていない/打ちにくいキーボードがあるので
// 主にはしない。
//
// 別の組み合わせに変えるときは、まず取られていないかを確かめること:
//   defaults export com.apple.symbolichotkeys - | plutil -convert json -o - -

/// 組み合わせの相手。⌘ と Shift の両方と一緒に押されたときだけトグルになる
pub const TOGGLE_KEY: KeyCode = KeyCode::Escape;
/// 修飾なしで単独でトグルになるキー。X68000 に F11/F12 は無いので衝突しない
pub const TOGGLE_ALT: KeyCode = KeyCode::F12;

pub const TOGGLE_LABEL: &str =
    if cfg!(target_os = "macos") { "⌘+Shift+ESC" } else { "Ctrl+Shift+ESC" };
/// UI の説明文用。副のキーも含む表記
pub const TOGGLE_LABEL_FULL: &str = if cfg!(target_os = "macos") {
    "⌘+Shift+ESC(または F12)"
} else {
    "Ctrl+Shift+ESC(または F12)"
};

/// 転送ON/OFF操作の判定に使う修飾の状態。
/// `command` は macOS では ⌘、他では Ctrl。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub command: bool,
    pub shift: bool,
}

impl Mods {
    /// 組み合わせが成立する修飾が揃っているか
    pub fn is_toggle_combo(self) -> bool {
        self.command && self.shift
    }
}

/// `--log-keys` 用。受け取った物理キーを stderr へ出す。
///
/// 「このキーがそもそもアプリに届いていないのか、届いているが転送していないのか」
/// は外から見て区別できない。OS が横取りする組み合わせ(macOS のシステム
/// ショートカットなど)を疑うときに要る。
pub static LOG_KEYS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 1イベント分を出す。`usage` が `-` なら表に無いので転送しない
pub fn log_key(code: KeyCode, vk: Option<u16>, pressed: bool, mods: Mods) {
    if !LOG_KEYS.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "key: {} {code:?} {} cmd={} shift={} usage={}",
        match vk {
            Some(v) => format!("vk={v:#04x}"),
            None => "vk=?".into(),
        },
        if pressed { "down" } else { "up  " },
        mods.command,
        mods.shift,
        match usage_from_keycode(code) {
            Some(u) => format!("{u:#010x}"),
            None => "-".into(),
        }
    );
}

/// `KeyCode` にすら落ちなかったキー。ここに出るなら変換表を足す必要がある
pub fn log_unknown(vk: u16, pressed: bool) {
    if !LOG_KEYS.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    eprintln!("key: vk={vk:#04x} (未知のキー) {}", if pressed { "down" } else { "up" });
}

/// 転送ON/OFF操作の検出。
///
/// **押下の瞬間だけ**を拾う。OS のキーリピートは押しっぱなしで押下イベントを
/// 撒き続けるので、そのまま数えると1回の打鍵で何度もトグルする。
///
/// 横取りした押下に対応する解放も横取りする。「⌘が押されているか」で解放を
/// 判定すると、**⌘を先に離したときに解放を取りこぼして**押下状態が残り、
/// 次のトグルが効かなくなる。
#[derive(Default)]
pub struct ToggleDetect {
    /// キーが物理的に押されている(リピートを除くため)
    down: [bool; 2],
    /// 押下を横取りした。対応する解放も横取りする
    eaten: [bool; 2],
}

impl ToggleDetect {
    /// 1イベント食わせる。戻り値は (トグルするか, このイベントを横取りするか)。
    /// 横取りしなかったイベントは、いつもどおり UI と転送へ回す。
    pub fn feed(&mut self, code: KeyCode, pressed: bool, mods: Mods) -> (bool, bool) {
        let i = match code {
            TOGGLE_KEY => 0,
            TOGGLE_ALT => 1,
            _ => return (false, false),
        };
        if pressed {
            if self.down[i] {
                // リピート。押下のときの判定をそのまま引き継ぐ
                return (false, self.eaten[i]);
            }
            self.down[i] = true;
            // 修飾の揃っていない ESC は実機の ESC として送る(横取りしない)
            self.eaten[i] = i == 1 || mods.is_toggle_combo();
            (self.eaten[i], self.eaten[i])
        } else {
            self.down[i] = false;
            let eaten = std::mem::replace(&mut self.eaten[i], false);
            (false, eaten)
        }
    }

    /// 組み合わせの修飾が揃っていないときに毎回呼ぶ。押下ラッチを戻す。
    ///
    /// **macOS は ⌘ を押しながらの keyUp を配送しないことがある**(AppKit の
    /// 古くからの挙動)。押下ラッチを解放イベントだけで戻していると、1回目の
    /// ⌘+ESC のあとラッチが残って2回目が効かなくなる。⌘ が離れていれば
    /// 組み合わせは成立していないので、そこで戻せば取り残されない。
    pub fn rearm_chord(&mut self) {
        self.down[0] = false;
        self.eaten[0] = false;
    }

    /// フォーカスを失ったら押下状態を忘れる(以後の解放イベントが届かないため)
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// winit の `KeyCode` → USB HID usage。
///
/// `KeyCode` は W3C UI Events の `code` 文字列と 1:1 で、それがそのまま
/// USB HID usage に対応する。正典は **USB HID Usage Tables の
/// Keyboard/Keypad Page (0x07)**。
///
/// マップに無いキー(メディアキー等、キーボードページに無いもの)は None を
/// 返して送らない。MimicX 側も未対応 usage は黙って捨てる。
pub fn usage_from_keycode(code: KeyCode) -> Option<u32> {
    use KeyCode::*;
    // ページ番号を含む32bit値。標準キーボードは 0x0007xxxx
    let p = |n: u32| Some(0x0007_0000 | n);
    match code {
        // --- 英字 (0x04..=0x1D) ---
        KeyA => p(0x04), KeyB => p(0x05), KeyC => p(0x06), KeyD => p(0x07),
        KeyE => p(0x08), KeyF => p(0x09), KeyG => p(0x0A), KeyH => p(0x0B),
        KeyI => p(0x0C), KeyJ => p(0x0D), KeyK => p(0x0E), KeyL => p(0x0F),
        KeyM => p(0x10), KeyN => p(0x11), KeyO => p(0x12), KeyP => p(0x13),
        KeyQ => p(0x14), KeyR => p(0x15), KeyS => p(0x16), KeyT => p(0x17),
        KeyU => p(0x18), KeyV => p(0x19), KeyW => p(0x1A), KeyX => p(0x1B),
        KeyY => p(0x1C), KeyZ => p(0x1D),

        // --- 数字。HIDは 1 が先頭で 0 が最後 ---
        Digit1 => p(0x1E), Digit2 => p(0x1F), Digit3 => p(0x20), Digit4 => p(0x21),
        Digit5 => p(0x22), Digit6 => p(0x23), Digit7 => p(0x24), Digit8 => p(0x25),
        Digit9 => p(0x26), Digit0 => p(0x27),

        // --- 編集・空白 ---
        Enter => p(0x28), Escape => p(0x29), Backspace => p(0x2A),
        Tab => p(0x2B), Space => p(0x2C),

        // --- 記号 ---
        Minus => p(0x2D), Equal => p(0x2E),
        BracketLeft => p(0x2F), BracketRight => p(0x30), Backslash => p(0x31),
        Semicolon => p(0x33), Quote => p(0x34), Backquote => p(0x35),
        Comma => p(0x36), Period => p(0x37), Slash => p(0x38),
        CapsLock => p(0x39),

        // --- ファンクションキー ---
        F1 => p(0x3A), F2 => p(0x3B), F3 => p(0x3C), F4 => p(0x3D),
        F5 => p(0x3E), F6 => p(0x3F), F7 => p(0x40), F8 => p(0x41),
        F9 => p(0x42), F10 => p(0x43), F11 => p(0x44), F12 => p(0x45),
        F13 => p(0x68), F14 => p(0x69), F15 => p(0x6A), F16 => p(0x6B),
        F17 => p(0x6C), F18 => p(0x6D), F19 => p(0x6E), F20 => p(0x6F),
        F21 => p(0x70), F22 => p(0x71), F23 => p(0x72), F24 => p(0x73),

        // --- 制御・移動 ---
        PrintScreen => p(0x46), ScrollLock => p(0x47), Pause => p(0x48),
        Insert => p(0x49), Home => p(0x4A), PageUp => p(0x4B),
        Delete => p(0x4C), End => p(0x4D), PageDown => p(0x4E),
        ArrowRight => p(0x4F), ArrowLeft => p(0x50),
        ArrowDown => p(0x51), ArrowUp => p(0x52),

        // --- テンキー。X68000 のテンキーは独立したキーなので main row とは
        //     別の usage を送る必要がある ---
        NumLock => p(0x53),
        NumpadDivide => p(0x54), NumpadMultiply => p(0x55),
        NumpadSubtract => p(0x56), NumpadAdd => p(0x57), NumpadEnter => p(0x58),
        Numpad1 => p(0x59), Numpad2 => p(0x5A), Numpad3 => p(0x5B),
        Numpad4 => p(0x5C), Numpad5 => p(0x5D), Numpad6 => p(0x5E),
        Numpad7 => p(0x5F), Numpad8 => p(0x60), Numpad9 => p(0x61),
        Numpad0 => p(0x62), NumpadDecimal => p(0x63),
        NumpadEqual => p(0x67), NumpadComma => p(0x85),
        NumpadParenLeft => p(0xB6), NumpadParenRight => p(0xB7),
        NumpadBackspace => p(0xBB),
        NumpadClear => p(0xD8), NumpadClearEntry => p(0xD9),

        // --- ISO 102番目のキー(左Shift と Z の間。US ANSI には無い) ---
        IntlBackslash => p(0x64),
        ContextMenu => p(0x65), // Keyboard Application
        Power => p(0x66),

        // --- 編集系の専用キー ---
        Open => p(0x74), Help => p(0x75), Props => p(0x76), Select => p(0x77),
        Again => p(0x79), Undo => p(0x7A),
        Cut => p(0x7B), Copy => p(0x7C), Paste => p(0x7D), Find => p(0x7E),

        // --- 日本語配列のキー。JIS キーボードから X68000 の かな/変換 等を
        //     打つのに要る ---
        IntlRo => p(0x87),      // International1 (ろ / _)
        KanaMode => p(0x88),    // International2 (かな)
        IntlYen => p(0x89),     // International3 (¥)
        Convert => p(0x8A),     // International4 (変換)
        NonConvert => p(0x8B),  // International5 (無変換)
        Lang1 => p(0x90), Lang2 => p(0x91),
        Lang3 => p(0x92), Lang4 => p(0x93), Lang5 => p(0x94),
        // macOS が かな/英数 を Lang とは別に報告する経路。Lang3/Lang4 と同義
        Katakana => p(0x92), Hiragana => p(0x93),

        // --- 修飾キー (0xE0..=0xE7) ---
        ControlLeft => p(0xE0), ShiftLeft => p(0xE1),
        AltLeft => p(0xE2), SuperLeft => p(0xE3),
        ControlRight => p(0xE4), ShiftRight => p(0xE5),
        AltRight => p(0xE6), SuperRight => p(0xE7),

        // メディアキー・ブラウザキー等はキーボードページに無いので送らない
        _ => None,
    }
}

/// macOS の仮想キーコード → winit の `KeyCode`。
///
/// 表の大半は winit が持っている(`PhysicalKey::from_scancode`)ので、そこに
/// 無いものだけ足す。**winit は macOS の JIS キーを未対応のまま残していて**
/// (`platform_impl/macos/event.rs` にコメントアウトで並んでいる)、
/// ろ・かな・英数・テンキーの `,` が `Unidentified` になる。
///
/// かな/英数 は Apple の JIS キーボードが USB で送るのと同じ LANG1 / LANG2 に
/// 当てる(PC/AT の「かなカナ」= International2 とは別の usage)。
#[cfg(target_os = "macos")]
pub fn keycode_from_mac_vk(vk: u16) -> Option<KeyCode> {
    use winit::keyboard::PhysicalKey;
    use winit::platform::scancode::PhysicalKeyExtScancode;
    match vk {
        0x5e => Some(KeyCode::IntlRo),      // ろ / _ (International1)
        0x5f => Some(KeyCode::NumpadComma), // JIS テンキーの ,
        0x66 => Some(KeyCode::Lang2),       // 英数 (LANG2)
        0x68 => Some(KeyCode::Lang1),       // かな (LANG1)
        _ => match PhysicalKey::from_scancode(vk as u32) {
            PhysicalKey::Code(c) => Some(c),
            PhysicalKey::Unidentified(_) => None,
        },
    }
}

#[cfg(not(target_os = "macos"))]
pub fn keycode_from_mac_vk(_vk: u16) -> Option<KeyCode> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 指示書の具体例と一致することを確かめる。7bit×4の分解を間違えると
    /// MimicX 側は黙って捨てるので、気付きにくい
    #[test]
    fn wire_format_matches_spec() {
        let a = usage_from_keycode(KeyCode::KeyA).unwrap();
        assert_eq!(a, 0x0007_0004);
        assert_eq!(
            key_message(a, true),
            [0xF0, 0x7D, 0x02, 0x01, 0x00, 0x1C, 0x00, 0x04, 0x01, 0xF7]
        );
        assert_eq!(
            key_message(usage_from_keycode(KeyCode::Escape).unwrap(), true),
            [0xF0, 0x7D, 0x02, 0x01, 0x00, 0x1C, 0x00, 0x29, 0x01, 0xF7]
        );
        assert_eq!(
            key_message(usage_from_keycode(KeyCode::ArrowLeft).unwrap(), false),
            [0xF0, 0x7D, 0x02, 0x01, 0x00, 0x1C, 0x00, 0x50, 0x00, 0xF7]
        );
        assert_eq!(RELEASE_ALL, [0xF0, 0x7D, 0x02, 0x02, 0xF7]);
    }

    /// データバイトに 0x80 以上が混ざると SysEx が壊れる。usage の下位が
    /// 0x7F を超えるテンキー系(0x85, 0xB6, 0xD8...)で確かめる
    #[test]
    fn data_bytes_stay_7bit() {
        for code in [
            KeyCode::NumpadComma, KeyCode::NumpadParenLeft,
            KeyCode::NumpadClear, KeyCode::ShiftLeft, KeyCode::Lang5,
        ] {
            let m = key_message(usage_from_keycode(code).unwrap(), true);
            assert!(m[4..9].iter().all(|b| *b < 0x80), "{code:?} -> {m:02x?}");
        }
    }

    /// ESC 単独は実機へ送り、⌘+ESC だけがトグルになること
    #[test]
    fn plain_escape_is_forwarded() {
        let none = Mods::default();
        let cmd = Mods { command: true, shift: false };
        let combo = Mods { command: true, shift: true };
        let mut d = ToggleDetect::default();
        assert_eq!(d.feed(KeyCode::Escape, true, none), (false, false));
        assert_eq!(d.feed(KeyCode::Escape, false, none), (false, false));
        // ⌘だけでは足りない(⌘+ESC は macOS が奪うので使えない)
        assert_eq!(d.feed(KeyCode::Escape, true, cmd), (false, false));
        assert_eq!(d.feed(KeyCode::Escape, false, cmd), (false, false));
        // ⌘+Shift ならトグルし、そのイベントは横取りする
        assert_eq!(d.feed(KeyCode::Escape, true, combo), (true, true));
        // 解放は修飾が離れていても横取りする(押下を横取りした分の後始末)
        assert_eq!(d.feed(KeyCode::Escape, false, none), (false, true));
        // 副のキーは単独で効く
        assert_eq!(d.feed(KeyCode::F12, true, none), (true, true));
        assert_eq!(d.feed(KeyCode::F12, false, none), (false, true));
        // 関係ないキーは素通し
        assert_eq!(d.feed(KeyCode::KeyA, true, combo), (false, false));
    }

    /// OSのキーリピートで何度もトグルしないこと
    #[test]
    fn repeat_does_not_retoggle() {
        let combo = Mods { command: true, shift: true };
        let mut d = ToggleDetect::default();
        assert_eq!(d.feed(KeyCode::Escape, true, combo), (true, true));
        for _ in 0..5 {
            // リピートは横取りするだけ(トグルしない)
            assert_eq!(d.feed(KeyCode::Escape, true, combo), (false, true));
        }
    }

    /// macOS は ⌘ を押しながらの keyUp を配送しないことがある。解放が来なくても
    /// ⌘ が離れたところでラッチが戻り、2回目が効くこと
    #[test]
    fn survives_missing_key_release() {
        let combo = Mods { command: true, shift: true };
        let mut d = ToggleDetect::default();
        assert_eq!(d.feed(KeyCode::Escape, true, combo), (true, true));
        // ここで解放イベントが来ない
        d.rearm_chord();
        assert_eq!(d.feed(KeyCode::Escape, true, combo), (true, true));
    }

    /// macOS の仮想キーコードから、winit の表に無い JIS キーまで拾えること。
    /// ここが None だと「押しても何も起きない」という形で出る
    #[test]
    #[cfg(target_os = "macos")]
    fn mac_vk_covers_jis_keys() {
        // (仮想キーコード, 期待する usage)
        for (vk, usage) in [
            (0x5du16, 0x0007_0089u32), // ¥   International3
            (0x5e, 0x0007_0087),       // ろ/_ International1
            (0x68, 0x0007_0090),       // かな LANG1
            (0x66, 0x0007_0091),       // 英数 LANG2
            (0x5f, 0x0007_0085),       // テンキーの , Keypad Comma
            (0x53, 0x0007_0059),       // テンキー1(最上段の 1 = 0x1e とは別)
            (0x35, 0x0007_0029),       // Escape
            (0x00, 0x0007_0004),       // A
        ] {
            let code = keycode_from_mac_vk(vk)
                .unwrap_or_else(|| panic!("vk {vk:#04x} が KeyCode に落ちない"));
            assert_eq!(usage_from_keycode(code), Some(usage), "vk {vk:#04x} -> {code:?}");
        }
    }
}
