//! 窓とタスクバー(Dock以外)のアイコン。
//!
//! **実行ファイルへの埋め込みだけでは足りない。** winit はウィンドウクラスを
//! `hIcon: 0` で登録し、`window_icon` が `None` のときは明示的に
//! `unset_for_window` を呼ぶ(winit 0.30 `platform_impl/windows/window.rs`)。
//! そのため Windows では
//!
//!     Explorer のファイルのアイコン → exe に埋め込んだリソース(build.rs)
//!     タイトルバー / タスクバー     → **実行時に設定したウィンドウアイコン** ← ここ
//!
//! となり、両方やる必要がある。macOS の Dock は `.app` の `CFBundleIconFile` で
//! 決まる(winit の `set_window_icon` は macOS では何もしない)。
//!
//! 画像は `packaging/AppIcon-256.png`(`packaging/make-icons.sh` が master から作る)を
//! 埋め込む。1024pxを毎起動デコードするのは無駄なので256pxにしてある。
use eframe::egui;
use resvg::tiny_skia;

const PNG: &[u8] = include_bytes!("../packaging/AppIcon-256.png");

/// (RGBA, 幅, 高さ)。デコードできなければ None(アイコンが付かないだけで動作する)。
fn rgba() -> Option<(Vec<u8>, u32, u32)> {
    let pixmap = tiny_skia::Pixmap::decode_png(PNG).ok()?;
    let (w, h) = (pixmap.width(), pixmap.height());
    // tiny-skia は乗算済みアルファで持つ。アイコンは不透明なので実際は同じ値だが、
    // 素材を差し替えたときに崩れないよう戻しておく。
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some((out, w, h))
}

/// eframe(通常のウィンドウ)用。
pub fn egui_icon() -> Option<egui::IconData> {
    let (rgba, width, height) = rgba()?;
    Some(egui::IconData { rgba, width, height })
}

/// winit を直接使う経路(`--fullscreen`)用。
pub fn winit_icon() -> Option<winit::window::Icon> {
    let (rgba, width, height) = rgba()?;
    winit::window::Icon::from_rgba(rgba, width, height).ok()
}

#[cfg(test)]
mod tests {
    /// 埋め込んだPNGが壊れていない/差し替えで形が変わっていないことを見る。
    /// アイコンが出ないのは実機を起動しないと気づかないので、ここで止める。
    #[test]
    fn decodes_to_square_rgba() {
        let (rgba, w, h) = super::rgba().expect("アイコンPNGをデコードできない");
        assert_eq!((w, h), (256, 256), "packaging/AppIcon-256.png のサイズが違う");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.chunks(4).all(|p| p[3] == 255), "不透明であること");
    }
}
