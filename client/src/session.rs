//! 新しい Viewer(別セッション)をもう1つ開く。
//!
//! ★**macOS の `.app` は二重起動できない。** LaunchServices が単一インスタンスに
//!   するので、Finder や Dock から2つ目を開こうとすると既存のウィンドウが前面に
//!   来るだけになる。**LAN に2枚あるボードを同時に見られない**のはこれが理由で、
//!   受信ポートの問題を直しただけでは足りなかった。
//!
//!   `open -n` は LaunchServices に「新しいインスタンスを作れ」と明示する唯一の
//!   手段なので、バンドルの中から起動されたときはこれを使う。バンドルの外
//!   (`cargo run` や Linux/Windows)では実行ファイルをそのまま起動すればよい。

use std::path::{Path, PathBuf};

/// 起動時の**事前走査**で読む引数。本体の引数ループより先に見る必要がある
/// (設定ファイルの置き場所や復元の可否がこれで決まるため)。
///
/// ★**1か所にまとめる。** ここと引数ループが食い違うと、子プロセスが
///   `unknown arg` で即死する ─ 実際 `--slot` を渡した復元がそれで
///   丸ごと動いていなかった(症状は「1つしか開かない」)。
pub const PRESCAN_FLAGS: [&str; 2] = ["--slot", "--no-restore"];

/// `PRESCAN_FLAGS` のうち、値を1つ取るもの。
pub const PRESCAN_WITH_VALUE: [&str; 1] = ["--slot"];

/// 実行ファイルのパスから `.app` バンドルの根を求める。
///
/// `/Applications/RetroCastX.app/Contents/MacOS/retrocastx-viewer`
///   → `/Applications/RetroCastX.app`
///
/// バンドルの中でなければ None。
pub fn bundle_root(exe: &Path) -> Option<PathBuf> {
    // .../Contents/MacOS/<exe> の3つ上がバンドル
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let root = contents.parent()?;
    (root.extension()? == "app").then(|| root.to_path_buf())
}

/// いまのコマンドライン引数から `--mac <値>` を取り除く。
///
/// ★**他の引数は引き継ぐ。** `--bind` を指定して VPN 環境で使っている人が、
///   新しいセッションだけ送信できなくなるのは分かりにくい。
pub fn args_without<I: IntoIterator<Item = String>>(args: I, drop: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if drop.contains(&a.as_str()) {
            skip = true;
            continue;
        }
        out.push(a);
    }
    out
}

pub fn mac_to_string(mac: &[u8; 6]) -> String {
    mac.map(|x| format!("{x:02x}")).join(":")
}

/// 新しい Viewer を開く。
///
/// - `mac`: そのボードを指名した状態で立ち上げる(「別窓」から)
/// - `slot`: ウィンドウ番号を指定する(前回の並びを復元するとき)。
///   省略すると子が**空いているうち最小**を取るので、通常は渡さない。
pub fn spawn_new(mac: Option<[u8; 6]>, slot: Option<u32>) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut extra = args_without(std::env::args().skip(1), &["--mac", "--slot"]);
    debug_assert!(PRESCAN_FLAGS.contains(&"--slot"));
    if let Some(m) = &mac {
        extra.push("--mac".into());
        extra.push(mac_to_string(m));
    }
    if let Some(id) = slot {
        extra.push("--slot".into());
        extra.push(id.to_string());
    }

    if cfg!(target_os = "macos") {
        if let Some(root) = bundle_root(&exe) {
            let mut cmd = std::process::Command::new("open");
            // -n = 既存インスタンスを前面に出すのではなく新しく作る
            cmd.arg("-n").arg(&root);
            if !extra.is_empty() {
                cmd.arg("--args").args(&extra);
            }
            cmd.spawn()?;
            return Ok(());
        }
    }
    std::process::Command::new(exe).args(&extra).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_root() {
        let p = Path::new("/Applications/RetroCastX.app/Contents/MacOS/retrocastx-viewer");
        assert_eq!(
            bundle_root(p),
            Some(PathBuf::from("/Applications/RetroCastX.app"))
        );
    }

    /// ★**バンドルの外を誤検出しない。** ここで嘘をつくと `open -n` に
    ///   存在しないパスを渡して、新しいセッションが黙って開かなくなる。
    #[test]
    fn plain_binary_is_not_a_bundle() {
        for p in [
            "/Users/x/work/target/release/retrocastx-viewer",
            "/usr/local/bin/retrocastx-viewer",
            "/tmp/Contents/MacOS/retrocastx-viewer", // .app が無い
            "/tmp/Foo.app/MacOS/retrocastx-viewer",  // Contents が無い
        ] {
            assert_eq!(bundle_root(Path::new(p)), None, "{p}");
        }
    }

    /// ★**--mac は1つだけになること。** 引き継ぎで2つ並ぶと、後勝ちか先勝ちかが
    ///   引数解析の実装依存になる。
    #[test]
    fn replaces_the_mac_and_slot_arguments() {
        let args = [
            "--bind", "192.168.11.24", "--mac", "aa:bb:cc:dd:ee:ff", "--slot", "3", "--no-vsync",
        ]
        .map(String::from);
        let out = args_without(args, &["--mac", "--slot"]);
        assert_eq!(out, vec!["--bind", "192.168.11.24", "--no-vsync"]);
    }

    /// ★**渡す引数は必ず受け側が知っていること。** `--slot` を渡すのに
    ///   引数ループが知らなかったため、復元で起こした子が `unknown arg` で
    ///   即死していた(2026-09-09 に実機で発覚)。
    #[test]
    fn every_flag_we_pass_is_prescanned() {
        for f in ["--slot", "--no-restore"] {
            assert!(PRESCAN_FLAGS.contains(&f), "{f} が事前走査の一覧に無い");
        }
        assert!(PRESCAN_WITH_VALUE.iter().all(|f| PRESCAN_FLAGS.contains(f)));
    }

    #[test]
    fn keeps_other_args() {
        let args = ["--board", "10.0.0.42"].map(String::from);
        assert_eq!(args_without(args, &["--mac", "--slot"]), vec!["--board", "10.0.0.42"]);
    }
}
