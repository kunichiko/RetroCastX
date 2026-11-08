//! Android で標準出力/標準エラーを logcat へ流す。
//!
//! アプリの診断はすべて `eprintln!` で書いてあるが、Android のプロセスでは
//! fd 1/2 が /dev/null に繋がっていて何も残らない。パイプに差し替えて、
//! 読み出したものを1行ずつ `__android_log_write` へ渡す。
//!
//! `adb logcat -s RetroCastX` で読める。

use std::ffi::{c_char, c_int, CString};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::FromRawFd;

const TAG: &[u8] = b"RetroCastX\0";
const ANDROID_LOG_INFO: c_int = 4;

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// 最初の出力より前に1回だけ呼ぶ
pub fn redirect_stdio() {
    let mut fds = [0 as c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    unsafe {
        libc::dup2(fds[1], 1);
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);
    }
    let read = unsafe { File::from_raw_fd(fds[0]) };
    let _ = std::thread::Builder::new().name("logcat".into()).spawn(move || {
        for line in BufReader::new(read).lines().map_while(Result::ok) {
            let Ok(msg) = CString::new(line) else { continue };
            unsafe { __android_log_write(ANDROID_LOG_INFO, TAG.as_ptr().cast(), msg.as_ptr()) };
        }
    });
}
