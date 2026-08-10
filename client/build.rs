//! Windows の実行ファイルにアプリアイコンを埋め込む。
//!
//! Explorer やタスクバーに出る絵はこれになる(winit は明示指定が無ければ実行ファイルの
//! アイコンを使うので、これだけで窓のアイコンにもなる)。
//! macOS 側は .app の `CFBundleIconFile` で決まるので `packaging/macos/bundle.sh` の担当。
//! アイコンの作り直しは `packaging/make-icons.sh`。
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/windows/AppIcon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/windows/AppIcon.ico");
        // 失敗したら黙って進めずに止める。「配布物にアイコンが付いていない」と
        // 後から気づく方が高くつく(rc.exe はMSVCツールチェーンに含まれる)。
        res.compile().expect("アイコンの埋め込みに失敗した (rc.exe が必要)");
    }
}
