//! アプリが前面にいないあいだ、映像の処理を止める。
//!
//! ★**スクリーンショットの編集画面やコントロールセンターでアプリが半分背後に
//!   回ると、iOS は CPU を絞る。** そのまま復調・描画を続けると処理が追いつかず、
//!   同じ受信ループで受けている音声まで途切れた。前面を離れているあいだは
//!   復調とフレームの受け渡しをやめ(受信は続けるので音声は鳴り続ける)、
//!   管面は黒にする。戻れば次のフィールドから映る。
//!
//! いまは iOS だけが立てる(ios_scene の sceneWillResignActive)。他の OS では
//! 常に false。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use eframe::egui;

static PAUSED: AtomicBool = AtomicBool::new(false);
/// 切り替わったときに描き直してもらう先。止まっている間は受信側が repaint を
/// 呼ばないので、黒にする・映像に戻すの1回分はこちらで頼む
static CTX: OnceLock<egui::Context> = OnceLock::new();

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub fn set_paused(p: bool) {
    if PAUSED.swap(p, Ordering::Relaxed) != p {
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }
}

pub fn set_ctx(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}
