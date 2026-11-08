//! Android の Performance Hint(ADPF)。スレッドの処理時間の目標と実績を
//! OS へ伝え、CPU のクロックとコアの割り当てを上げてもらう。
//!
//! ★**これが無いと Pixel 10 ではクロックが上がらない。** 8fsc のコンポジットを
//!   復調するスレッドが 96% 動いていても、最速のコアは 883MHz のままだった。
//!   Android のスケジューラは負荷の平均でクロックを決めるので、短い周期で
//!   締め切りのある処理は、こうして伝えないと遅れる。
//!
//! API 33 からの関数なので、minSdk(26)では直接リンクせず dlsym で引く。
//! 無い端末では None になり、何もしない。

use std::time::Duration;

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::{c_int, c_void};
    use std::time::Duration;

    type GetManager = unsafe extern "C" fn() -> *mut c_void;
    type CreateSession = unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void;
    type Report = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
    type Close = unsafe extern "C" fn(*mut c_void);

    pub struct Session {
        ptr: *mut c_void,
        report: Report,
        close: Close,
    }

    impl Session {
        pub fn for_current_thread(target: Duration) -> Option<Self> {
            unsafe {
                let lib = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
                if lib.is_null() {
                    return None;
                }
                let sym = |name: &std::ffi::CStr| {
                    let p = libc::dlsym(lib, name.as_ptr());
                    (!p.is_null()).then_some(p)
                };
                let get: GetManager = std::mem::transmute(sym(c"APerformanceHint_getManager")?);
                let create: CreateSession =
                    std::mem::transmute(sym(c"APerformanceHint_createSession")?);
                let report: Report =
                    std::mem::transmute(sym(c"APerformanceHint_reportActualWorkDuration")?);
                let close: Close = std::mem::transmute(sym(c"APerformanceHint_closeSession")?);
                let mgr = get();
                if mgr.is_null() {
                    return None;
                }
                let tid = libc::gettid();
                let ptr = create(mgr, &tid, 1, target.as_nanos() as i64);
                (!ptr.is_null()).then_some(Self { ptr, report, close })
            }
        }

        pub fn report(&self, actual: Duration) {
            unsafe {
                (self.report)(self.ptr, actual.as_nanos().max(1) as i64);
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe { (self.close)(self.ptr) }
        }
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use std::time::Duration;

    pub struct Session;

    impl Session {
        pub fn for_current_thread(_target: Duration) -> Option<Self> {
            None
        }

        pub fn report(&self, _actual: Duration) {}
    }
}

/// 呼んだスレッドの処理時間を OS へ伝えるセッション。Android 13 未満と
/// Android 以外では作れない(None)。
pub struct Hint(imp::Session);

impl Hint {
    /// `target` は1回の処理に許す時間(締め切り)
    pub fn for_current_thread(target: Duration) -> Option<Self> {
        imp::Session::for_current_thread(target).map(Hint)
    }

    /// 1回分の実際にかかった時間
    pub fn report(&self, actual: Duration) {
        self.0.report(actual);
    }
}
