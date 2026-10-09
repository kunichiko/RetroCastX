//! 遊んでいる間に OS がスリープしないようにする。
//!
//! 動画プレイヤーと同じ流儀で、**Viewer が前面にあり、映像を受けている間だけ**
//! 画面のアイドルスリープ(とそれに続くシステムスリープ)を止める。
//! ゲームはパッドで操作するので Mac/PC 側には入力が無く、放っておくと
//! アイドル扱いで画面が消える。
//!
//! 「前面にある」だけを条件にしないのは、ボードが繋がっていないまま
//! 窓を置きっぱなしにしたときに PC が永久に眠らなくなるため。
//!
//! - macOS: IOKit の電源アサーション(PreventUserIdleDisplaySleep)。
//!   `pmset -g assertions` に "RetroCast X" の名前で出る
//! - Windows: `SetThreadExecutionState`。**呼んだスレッドに紐づく**ので、
//!   UIスレッド(update の中)からだけ呼ぶこと
//! - その他: 何もしない

pub struct KeepAwake {
    on: bool,
    #[cfg(target_os = "macos")]
    id: u32,
}

impl KeepAwake {
    pub fn new() -> Self {
        Self {
            on: false,
            #[cfg(target_os = "macos")]
            id: 0,
        }
    }

    /// 毎フレーム呼んでよい。状態が変わったときだけ OS へ伝える
    pub fn set(&mut self, want: bool) {
        if want == self.on {
            return;
        }
        if want {
            if self.acquire() {
                self.on = true;
            }
        } else {
            self.release();
            self.on = false;
        }
    }

    #[cfg(target_os = "macos")]
    fn acquire(&mut self) -> bool {
        // 名前は ASCII にする。日本語だと `pmset -g assertions` に空で出て、
        // 誰が止めているのか分からなくなる(実機で確認)
        match mac::create("RetroCast X (showing video)") {
            Some(id) => {
                self.id = id;
                true
            }
            None => false,
        }
    }

    #[cfg(target_os = "macos")]
    fn release(&mut self) {
        mac::release(self.id);
        self.id = 0;
    }

    #[cfg(windows)]
    fn acquire(&mut self) -> bool {
        use windows_sys::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        // SAFETY: 引数はフラグだけ。戻り値0が失敗
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) != 0 }
    }

    #[cfg(windows)]
    fn release(&mut self) {
        use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
        // ES_CONTINUOUS だけを渡すと、以前に立てた要求が解除される
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn acquire(&mut self) -> bool {
        true
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn release(&mut self) {}
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.set(false);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void, CString};

    type CFStringRef = *const c_void;
    type IOReturn = i32;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: IOReturn = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> IOReturn;
        fn IOPMAssertionRelease(id: u32) -> IOReturn;
    }

    fn cfstr(s: &str) -> Option<CFStringRef> {
        let c = CString::new(s).ok()?;
        // SAFETY: c は NUL 終端で、呼び出し中は生存している
        let r = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
        (!r.is_null()).then_some(r)
    }

    pub fn create(reason: &str) -> Option<u32> {
        // kIOPMAssertionTypePreventUserIdleDisplaySleep の実体。画面が消えなければ
        // システムのアイドルスリープも起きない(QuickTime などと同じ種類)
        let ty = cfstr("PreventUserIdleDisplaySleep")?;
        let Some(name) = cfstr(reason) else {
            unsafe { CFRelease(ty) };
            return None;
        };
        let mut id = 0u32;
        // SAFETY: 2つの CFString は有効で、id は書き込み先として有効
        let rc = unsafe { IOPMAssertionCreateWithName(ty, K_IOPM_ASSERTION_LEVEL_ON, name, &mut id) };
        unsafe {
            CFRelease(ty);
            CFRelease(name);
        }
        (rc == K_IO_RETURN_SUCCESS).then_some(id)
    }

    pub fn release(id: u32) {
        if id != 0 {
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}
