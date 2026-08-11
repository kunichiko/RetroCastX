//! 通常モード(eframe)で物理キーを取り出す。macOS の AppKit イベントを直接見る。
//!
//! **なぜ egui のキーイベントでは足りないのか。** eframe 経路で手に入るのは egui が
//! 変換した `Key` までで、これは物理キーを表すと言いながら次を落とす:
//!
//! - JIS の `¥`(International3)/ `_`(International1)/ かな / 英数 に対応する
//!   variant が無い → **イベントそのものが作られず、押しても何も起きない**
//! - テンキーを最上段の数字に潰す(`Numpad0` も `Digit0` も `Num0`)
//!
//! egui 0.36 でも同じ。eframe には winit の生イベントに触れるフックが無いので
//! (`raw_input_hook` は変換後の `RawInput`)、OS の層から取るほかない。
//! `--fullscreen` は winit を直接使うので、こちらは関係ない。
//!
//! **`addLocalMonitorForEventsMatchingMask:` を使う理由。** 見るのは
//! **このアプリに配送されるイベントだけ**なので、CGEventTap と違って
//! アクセシビリティ権限が要らない。しかも `NSApplication.sendEvent:` の中で
//! 通常の配送より先に呼ばれるため、**⌘ 付きの組み合わせを AppKit のキー
//! イコーレント処理(メニュー)に横取りされる前に受け取れる**。
//!
//! ハンドラが null を返すとイベントを捨てられる。転送中は捨てて egui へ渡さない
//! (渡すと Tab でパネルのフォーカスが動き、そのせいで転送が中断される)。

/// 受け取った1打。`vk` は macOS の仮想キーコード
pub struct Ev {
    pub vk: u16,
    pub pressed: bool,
    /// そのときの修飾。転送ON/OFFの組み合わせ判定と、⌘ 付きを実機へ
    /// 送らない判定に使う
    pub mods: crate::remote_input::Mods,
}

/// CapsLock の仮想キーコード。ロックするキーなので別扱いにする
const VK_CAPS_LOCK: u16 = 0x39;

/// 修飾キーの仮想キーコード → 「そのキーが押されているか」を表すフラグのビット。
///
/// **macOS の修飾キーは `keyDown`/`keyUp` を出さず `flagsChanged` として来る。**
/// どのキーが動いたかは `keyCode` で分かるが、押されたのか離されたのかは
/// イベントに無いので、いまのフラグにそのキーのビットが立っているかで判定する。
///
/// 一般の `NSEventModifierFlagShift`(1<<17)は**左右どちらでも立つ**ので使えない
/// (両方押している最中に片方だけ離しても消えず、解放を取りこぼす)。macOS が
/// 別に持っているデバイス依存ビット(IOKit の `NX_DEVICE*KEYMASK`)を使えば
/// 左右が独立して見える。
fn modifier_side_bit(vk: u16) -> Option<u64> {
    Some(match vk {
        0x38 => 0x0000_0002, // 左Shift
        0x3C => 0x0000_0004, // 右Shift
        0x3B => 0x0000_0001, // 左Control
        0x3E => 0x0000_2000, // 右Control
        0x3A => 0x0000_0020, // 左Option
        0x3D => 0x0000_0040, // 右Option
        0x37 => 0x0000_0008, // 左Command
        0x36 => 0x0000_0010, // 右Command
        // CapsLock だけはロック状態がそのままフラグに出る(1<<16)
        VK_CAPS_LOCK => 0x0001_0000,
        // Fn は HID のキーボードページに無いので送らない
        _ => return None,
    })
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::VecDeque;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

    use super::Ev;
    use crate::remote_input::Mods;

    fn mods_of(ev: &NSEvent) -> Mods {
        let f = ev.modifierFlags();
        Mods {
            command: f.contains(NSEventModifierFlags::Command),
            shift: f.contains(NSEventModifierFlags::Shift),
        }
    }

    pub struct KeyTap {
        queue: Arc<Mutex<VecDeque<Ev>>>,
        capturing: Arc<AtomicBool>,
        /// いまの修飾。(command, shift) を別々の atomic で持つ
        mods: Arc<(AtomicBool, AtomicBool)>,
    }

    impl KeyTap {
        pub fn install(ctx: &eframe::egui::Context) -> Self {
            let queue: Arc<Mutex<VecDeque<Ev>>> = Default::default();
            let capturing = Arc::new(AtomicBool::new(false));
            let mods: Arc<(AtomicBool, AtomicBool)> = Default::default();
            // 押下と解放で別の監視を張る。1つにまとめると NSEvent の `type` を
            // 読む必要があり(Rustの予約語)、得るものが無い
            for (mask, pressed) in
                [(NSEventMask::KeyDown, true), (NSEventMask::KeyUp, false)]
            {
                install_key(mask, pressed, queue.clone(), capturing.clone(),
                            mods.clone(), ctx.clone());
            }
            // 修飾キー(Shift / Control / Option / Command / CapsLock)は
            // keyDown/keyUp を出さない。これを張らないと1つも届かない
            install_flags(queue.clone(), mods.clone(), ctx.clone());
            Self { queue, capturing, mods }
        }

        /// 転送中か。ハンドラはこれを見てイベントを egui へ渡さない
        pub fn set_capturing(&self, on: bool) {
            self.capturing.store(on, Ordering::Relaxed);
        }

        /// いまの修飾。転送ON/OFFの組み合わせ判定に使う。
        /// egui 側の modifiers ではなくこちらを見る(同じ経路の値で揃える)
        pub fn mods(&self) -> Mods {
            Mods {
                command: self.mods.0.load(Ordering::Relaxed),
                shift: self.mods.1.load(Ordering::Relaxed),
            }
        }

        pub fn drain(&self) -> Vec<Ev> {
            self.queue.lock().unwrap().drain(..).collect()
        }
    }

    /// 監視ハンドラはメインスレッドのイベント配送中に呼ばれる。UI を組む側とは
    /// 同時に走らないので、Mutex が競合することはない。
    fn install(mask: NSEventMask, block: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>) {
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block)
        };
        // アプリが終わるまで生かす。外す手段は用意しない(転送のON/OFFは
        // capturing フラグで足りる)
        std::mem::forget(monitor);
        std::mem::forget(block);
    }

    /// 捨てたイベントは winit に届かないので再描画要求も出ない。ここで起こさないと、
    /// 次の定期再描画(250ms)まで処理されずキー入力が目に見えて遅れる。
    fn wake(ctx: &eframe::egui::Context) {
        ctx.request_repaint();
    }

    fn install_key(
        mask: NSEventMask,
        pressed: bool,
        queue: Arc<Mutex<VecDeque<Ev>>>,
        capturing: Arc<AtomicBool>,
        mods: Arc<(AtomicBool, AtomicBool)>,
        ctx: eframe::egui::Context,
    ) {
        install(
            mask,
            RcBlock::new(move |ev: NonNull<NSEvent>| -> *mut NSEvent {
                let ev: &NSEvent = unsafe { ev.as_ref() };
                // リピートは転送しない。MimicX はファーム由来の delay/interval で
                // アプリ側 Timer からリピートを作るので、送ると二重にかかる
                if !ev.isARepeat() {
                    let m = mods_of(ev);
                    mods.0.store(m.command, Ordering::Relaxed);
                    mods.1.store(m.shift, Ordering::Relaxed);
                    queue.lock().unwrap().push_back(Ev { vk: ev.keyCode(), pressed, mods: m });
                    wake(&ctx);
                }
                // ⌘ を押している間は横取りしない。**⌘Q などのアプリの
                // ショートカットを効かせる**ため(winit は既定メニューに
                // ⌘Q = terminate: を入れている。飲み込むとメニューに届かず
                // 転送中は終了できなくなる)。X68000 のキーボードに ⌘ は
                // 無いので、⌘ 付きの打鍵が実機向けであることもない。
                let cmd = ev.modifierFlags().contains(NSEventModifierFlags::Command);
                if capturing.load(Ordering::Relaxed) && !cmd {
                    std::ptr::null_mut() // egui へ渡さない
                } else {
                    ev as *const NSEvent as *mut NSEvent
                }
            }),
        );
    }

    /// 修飾キーの監視。押下/解放はイベントに無いので、いまのフラグから読む。
    ///
    /// **こちらは egui へも渡す。** 捨てると egui が ⌘ / Shift の状態を見失い、
    /// 転送を切ったあとの操作(Shift+ドラッグ等)がおかしくなる。修飾キー単独の
    /// イベントで egui が何かすることは無いので、渡して困らない。
    fn install_flags(
        queue: Arc<Mutex<VecDeque<Ev>>>,
        mods: Arc<(AtomicBool, AtomicBool)>,
        ctx: eframe::egui::Context,
    ) {
        install(
            NSEventMask::FlagsChanged,
            RcBlock::new(move |ev: NonNull<NSEvent>| -> *mut NSEvent {
                let ev: &NSEvent = unsafe { ev.as_ref() };
                let flags = ev.modifierFlags();
                let m = mods_of(ev);
                mods.0.store(m.command, Ordering::Relaxed);
                mods.1.store(m.shift, Ordering::Relaxed);
                let vk = ev.keyCode();
                if let Some(bit) = super::modifier_side_bit(vk) {
                    let pressed = flags.bits() as u64 & bit != 0;
                    let mut q = queue.lock().unwrap();
                    if vk == super::VK_CAPS_LOCK {
                        // CapsLock はロックするキーで、macOS は状態が変わったとき
                        // だけ知らせる。押しっぱなしとして送ると MimicX の Timer が
                        // リピートを作って実機側で切り替わり続けるので、1打として
                        // 送る(実機側も1回押すごとに切り替わる)
                        q.push_back(Ev { vk, pressed: true, mods: m });
                        q.push_back(Ev { vk, pressed: false, mods: m });
                    } else {
                        q.push_back(Ev { vk, pressed, mods: m });
                    }
                    drop(q);
                    wake(&ctx);
                }
                ev as *const NSEvent as *mut NSEvent
            }),
        );
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Ev;

    pub struct KeyTap;

    impl KeyTap {
        pub fn install(_ctx: &eframe::egui::Context) -> Self {
            Self
        }
        pub fn set_capturing(&self, _on: bool) {}
        pub fn mods(&self) -> crate::remote_input::Mods {
            Default::default()
        }
        pub fn drain(&self) -> Vec<Ev> {
            Vec::new()
        }
    }
}

pub use imp::KeyTap;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_input::{keycode_from_mac_vk, usage_from_keycode};

    /// 修飾キーが1つも落ちないこと。
    ///
    /// **これが抜けていて Shift / Control / ⌘ がまったく届いていなかった。**
    /// macOS の修飾キーは keyDown/keyUp を出さないので、監視を張り忘れると
    /// 「一族まるごと無反応」という形で出る。usage まで確かめておく。
    #[test]
    #[cfg(target_os = "macos")]
    fn modifiers_reach_a_usage() {
        for (vk, usage) in [
            (0x38u16, 0x0007_00E1u32), // 左Shift
            (0x3C, 0x0007_00E5),       // 右Shift
            (0x3B, 0x0007_00E0),       // 左Control
            (0x3E, 0x0007_00E4),       // 右Control
            (0x3A, 0x0007_00E2),       // 左Option
            (0x3D, 0x0007_00E6),       // 右Option
            (0x37, 0x0007_00E3),       // 左Command
            (0x36, 0x0007_00E7),       // 右Command
            (0x39, 0x0007_0039),       // CapsLock
        ] {
            assert!(modifier_side_bit(vk).is_some(), "vk {vk:#04x} に押下ビットが無い");
            let code = keycode_from_mac_vk(vk)
                .unwrap_or_else(|| panic!("vk {vk:#04x} が KeyCode に落ちない"));
            assert_eq!(usage_from_keycode(code), Some(usage), "vk {vk:#04x} -> {code:?}");
        }
    }

    /// 左右で別のビットを見ていること。一般の `NSEventModifierFlagShift`(1<<17)は
    /// 左右どちらでも立つので、両方押している最中の片方の解放が取れない
    #[test]
    fn left_and_right_use_distinct_bits() {
        for (l, r) in [(0x38u16, 0x3Cu16), (0x3B, 0x3E), (0x3A, 0x3D), (0x37, 0x36)] {
            let (lb, rb) = (modifier_side_bit(l).unwrap(), modifier_side_bit(r).unwrap());
            assert_ne!(lb, rb, "vk {l:#04x} と {r:#04x} が同じビットを見ている");
            assert_eq!(lb & rb, 0);
        }
    }
}
