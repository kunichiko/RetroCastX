//! 一時的な通知(スナックバー)。
//!
//! 「起きたこと」を数秒だけ知らせて消える。状態ではなく**出来事**を伝えるので、
//! パネルに常設するのは合わない。
//!
//! ★**パネルの外に出す理由。** 右パネルは Tab で閉じられるし、フルスクリーン
//!   モードにはそもそも UI が無い。パネル内に出すと「閉じている間に起きたこと」を
//!   伝えられない。映像の上に重ねれば、パネルの開閉に関係なく目に入る。
//!
//! ★**配信出力ウィンドウには出ない。** あちらは `paint_tube` しか描かないので、
//!   ここで足す Area は本体ウィンドウにしか乗らない。通知が配信に写る心配はない。

use std::time::{Duration, Instant};

use eframe::egui;

use crate::theme;

/// 表示し続ける時間(この後 FADE をかけて消える)。
const LIFE: Duration = Duration::from_secs(7);
/// 消え際にかける時間。
const FADE: Duration = Duration::from_millis(600);
/// 積み上げる上限。これを超えたら古いものから捨てる。
const MAX: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// ふつうの報告。
    Info,
    /// **こちらが勝手にやったこと。** ユーザーが頼んでいない副作用を伝える。
    Notice,
}

pub struct Toast {
    text: String,
    level: Level,
    born: Instant,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        self.items.push(Toast { text: text.into(), level, born: Instant::now() });
        while self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.push(Level::Notice, text);
    }

    /// 1フレーム分描いて、寿命の切れたものを捨てる。
    ///
    /// ★**再描画を予約する。** 映像が来ていないと Viewer は 250ms 間隔まで
    ///   落ちるので、そのままだと通知が消えるのが遅れる(最悪 250ms ずれる)。
    ///   出ている間だけ細かく回す。
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|t| now.duration_since(t.born) < LIFE + FADE);
        if self.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(50));

        // 左下から上へ積む。左上は MimicX 転送中バッジが使っている
        let base = ctx.content_rect().left_bottom() + egui::vec2(8.0, -8.0);
        let mut dismissed = None;
        for (i, t) in self.items.iter().enumerate().rev() {
            let age = now.duration_since(t.born);
            let alpha = if age <= LIFE {
                1.0
            } else {
                1.0 - (age - LIFE).as_secs_f32() / FADE.as_secs_f32()
            }
            .clamp(0.0, 1.0);
            // 下から i 段目。高さは実測せず一定(行数が増えても重ならない程度に取る)
            let pos = base - egui::vec2(0.0, 26.0 * (self.items.len() - 1 - i) as f32);
            let id = egui::Id::new(("toast", i, t.born));
            egui::Area::new(id)
                .order(egui::Order::Foreground)
                .fixed_pos(pos - egui::vec2(0.0, 22.0))
                .show(ctx, |ui| {
                    let (fg, bg) = match t.level {
                        Level::Info => (theme::ACCENT, theme::ACCENT_BG),
                        Level::Notice => (theme::AMBER, egui::Color32::from_rgb(0x2E, 0x1E, 0x08)),
                    };
                    let btn = egui::Button::new(
                        egui::RichText::new(&t.text)
                            .size(12.0)
                            .color(fg.gamma_multiply(alpha)),
                    )
                    .fill(bg.gamma_multiply(alpha))
                    .stroke(egui::Stroke::new(1.0, fg.gamma_multiply(alpha * 0.5)))
                    .corner_radius(4.0);
                    if ui.add(btn).on_hover_text("クリックで消す").clicked() {
                        dismissed = Some(i);
                    }
                });
        }
        if let Some(i) = dismissed {
            self.items.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_latest() {
        let mut t = Toasts::default();
        for i in 0..MAX + 3 {
            t.info(format!("{i}"));
        }
        assert_eq!(t.items.len(), MAX, "上限を超えて溜まっている");
        // 古い方から捨てるので、残るのは新しい MAX 件
        assert_eq!(t.items[0].text, (3).to_string());
    }

    /// ★**消えることを確かめる。** ここが壊れると「消えない通知」に逆戻りして、
    ///   常設表示をやめた意味が無くなる。
    #[test]
    fn expires_after_life_and_fade() {
        let mut t = Toasts::default();
        t.info("x");
        t.items[0].born = Instant::now() - (LIFE + FADE + Duration::from_millis(1));
        let now = Instant::now();
        t.items.retain(|x| now.duration_since(x.born) < LIFE + FADE);
        assert!(t.items.is_empty(), "寿命を過ぎても残っている");
    }
}
