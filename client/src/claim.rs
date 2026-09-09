//! 「このボードはこのウィンドウが使っている」をプロセスをまたいで示す。
//!
//! ★**ボードの映像送り先はプロトコル上1つしか無い。** 2つの Viewer が同じボードを
//!   購読すると、2秒ごとの SUBSCRIBE で送り先が交互に奪い合われ、**どちらも
//!   まともに映らない**(実機で発生)。取り合いを起こしてから気付くのではなく、
//!   開く前に「空いていない」と分かるようにする。
//!
//! ★**ウィンドウは別プロセス。** macOS の `.app` は単一インスタンスなので、
//!   2つ目は `open -n` で別プロセスとして開く(`session.rs`)。同じプロセス内の
//!   変数では共有できないので、**ファイルロック**で示す。
//!
//! ★**プロセスが落ちても残らないこと。** PIDを書いたファイルだと、強制終了や
//!   クラッシュのあとに「誰も居ないのに使用中」が残る。`flock` 相当のロックは
//!   OS がプロセス終了時に必ず解放するので、後始末が要らない。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use crate::settings::Settings;

pub fn mac_tag(mac: &[u8; 6]) -> String {
    mac.map(|x| format!("{x:02x}")).concat()
}

/// ロックファイルの場所。設定ファイルと同じディレクトリに置く。
pub fn lock_path(mac: &[u8; 6]) -> PathBuf {
    Settings::path().with_file_name(format!("claim-{}.lock", mac_tag(mac)))
}

/// ウィンドウの数の上限。
///
/// ★**無制限にしない。** 押し間違いや復元の暴走で何十個も開くと、1つあたり
///   60Mbps を受けるので機械ごと沈む。3画面筐体(ダライアス等)に足りればよい。
pub const MAX_SLOTS: u32 = 10;

pub fn mac_string(mac: &[u8; 6]) -> String {
    mac.map(|x| format!("{x:02x}")).join(":")
}

fn open_lock(path: &std::path::Path) -> Option<File> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
}

/// 掴んでいるボード。**落とすとロックが外れる**ので、使っている間は持ち続ける。
pub struct Claim {
    pub mac: [u8; 6],
    _file: File,
}

impl Claim {
    /// 掴めたら Some。他のウィンドウが使っていれば None。
    pub fn try_take(mac: [u8; 6]) -> Option<Claim> {
        let file = open_lock(&lock_path(&mac))?;
        // ★**try_lock。** 待ってはいけない ─ UIスレッドから呼ぶので、
        //   掴めないなら即座に「使用中」と答える必要がある。
        file.try_lock().ok()?;
        Some(Claim { mac, _file: file })
    }
}

/// 他のウィンドウが使っているか。
///
/// ★**自分が持っているものは「使用中」ではない。** ロックは同じプロセスでも
///   別に open すれば衝突するので、持ち主を除外しないと自分の掴んでいる
///   ボードが選べなくなる。
pub fn taken_by_other(mac: &[u8; 6], own: Option<[u8; 6]>) -> bool {
    if own == Some(*mac) {
        return false;
    }
    // ロックファイルすら作れない環境では、掴めない前提にすると何も開けなくなる
    let Some(file) = open_lock(&lock_path(mac)) else { return false };
    match file.try_lock() {
        Ok(()) => false, // 掴めた = 誰も使っていない(ここで落として解放)
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**空いているうち最小が返ること。** 0,1,2を開いて1を閉じたら、次に開く
    ///   ウィンドウは1でなければならない(1の設定が復元される前提が崩れる)。
    /// ★**具体的な番号を期待しない。** Viewer が起動中だと0番は埋まっている。
    ///   確かめたいのは「空いているうち最小が返る」という性質そのもので、

    /// ★**古い印が次の起動を殺さないこと。** ここを間違えると、一度全終了した

    #[test]
    fn tag_is_stable_and_lowercase() {
        assert_eq!(mac_tag(&[0x74, 0xC9, 0x0F, 0x7C, 0x84, 0x29]), "74c90f7c8429");
    }

    /// ★**2つ目は掴めないこと。** これが成り立たないと「使用中」の表示が嘘になり、
    ///   ボードの取り合いを止められない。別 open なので同一プロセスでも衝突する
    ///   (= 別プロセスでも同じ結果になる)。
    #[test]
    fn second_take_fails_while_first_is_held() {
        let mac = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
        let first = Claim::try_take(mac).expect("1つ目が掴めない");
        assert!(Claim::try_take(mac).is_none(), "2つ目が掴めてしまった");
        assert!(taken_by_other(&mac, None), "使用中と見えていない");
        assert!(!taken_by_other(&mac, Some(mac)), "自分の分を使用中と答えている");
        drop(first);
        assert!(!taken_by_other(&mac, None), "解放されていない");
        let _ = std::fs::remove_file(lock_path(&mac));
    }
}
