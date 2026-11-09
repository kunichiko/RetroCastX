//! 操作パネル: ホーム + 詳細の2階層。
//!
//! ★**基準は「パネルを開いた理由の大半がホームだけで済むこと」。** 入力を
//!   切り替える、音量を変える、スキャンし直す、の3つがそれに当たる。管面や
//!   同期の調整は「映像を見ながら細かく合わせる」もので、ホームに並べると
//!   縦に長くなって常用の操作が埋もれる(以前のパネルがそうだった: 入力・
//!   LED・Mode・Stats・Audio・Tune・管面が1本の縦スクロールに並び、
//!   調査用の数値は「詳細」チェックで出し入れしていた)。
//!
//! ★**カテゴリの行には、いまの設定の要約を出す。** 開かなくても状態が分かる
//!   (「表示 · 4:3 · H幅 0.70 · ニアレスト」)。開くのは変えたいときだけ。
//!
//! ★**状態カードはどのページでも出す。** 詳細を開いていても、どのボードの
//!   何が映っているかが見えていないと、調整の結果が読めない。
//!
//! 中身(各ページの部品)は main.rs の `*_ui` と、ここへ移した旧パネルの
//! ブロックをそのまま使う。並べ方だけをここで決める。

use super::*;

/// パネルのページ
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Page {
    #[default]
    Home,
    Video,
    Display,
    Audio,
    Board,
    Output,
    Sync,
    Diag,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Home => "ホーム",
            Page::Video => "映像",
            Page::Display => "表示",
            Page::Audio => "音声",
            Page::Board => "ボード",
            Page::Output => "出力・連携",
            Page::Sync => "同期の調整",
            Page::Diag => "診断",
        }
    }

    /// 調子が悪いときにだけ触るページ(見出しに「上級」を付ける)
    fn advanced(self) -> bool {
        self == Page::Sync
    }
}

// 案(docs のモック)の配色。theme.rs の基調色から作ってある
const GROUP_BG: egui::Color32 = egui::Color32::from_rgb(0x12, 0x19, 0x1B);
const GROUP_EDGE: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x29, 0x2B);
const TILE_BG: egui::Color32 = egui::Color32::from_rgb(0x18, 0x20, 0x23);
const TILE_EDGE: egui::Color32 = egui::Color32::from_rgb(0x2B, 0x37, 0x39);
const TILE_HOVER: egui::Color32 = egui::Color32::from_rgb(0x3E, 0x52, 0x54);
const INK: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xEB, 0xEA);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x9A, 0x99);
const LABEL: egui::Color32 = egui::Color32::from_rgb(0x7F, 0x90, 0x8F);
const RULE: egui::Color32 = egui::Color32::from_rgb(0x1D, 0x26, 0x28);

/// 小見出し(「入力」「音量」)。区切りは字の大きさと色だけ。帯は使わない
fn label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text).size(11.0).color(LABEL));
}

/// まとまり1つを角丸の面で囲む。`dim` のときは薄くする(管面を合わせている間)
fn group<R>(ui: &mut egui::Ui, title: &str, dim: bool,
            add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    ui.scope(|ui| {
        if dim {
            ui.multiply_opacity(0.25);
        }
        egui::Frame::new()
            .fill(GROUP_BG)
            .stroke(egui::Stroke::new(1.0, GROUP_EDGE))
            .corner_radius(7.0)
            .inner_margin(egui::Margin { left: 10, right: 10, top: 8, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if !title.is_empty() {
                    ui.label(egui::RichText::new(title).size(11.5).color(egui::Color32::from_rgb(0x9F, 0xB0, 0xAF)));
                    ui.add_space(2.0);
                }
                add(ui)
            })
            .inner
    })
}

/// 案の `.qbtn`。`on` は押し込まれている(ベゼル表示中など)
fn tile_button(ui: &mut egui::Ui, w: f32, text: &str, on: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 30.0), sense);
    if ui.is_rect_visible(rect) {
        let edge = if on { theme::ACCENT } else if r.hovered() && enabled { TILE_HOVER } else { TILE_EDGE };
        let ink = if !enabled { MUTED.gamma_multiply(0.6) } else if on { theme::ACCENT } else { INK };
        ui.painter().rect(rect, 6.0, TILE_BG, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text,
                          egui::FontId::proportional(12.5), ink);
    }
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// 機種の選択肢1つ(案の `.seg`)。`note` は右肩に小さく琥珀で添える注記(未検証)。
///
/// ★**選んでいないものもボタンに見せる。** egui の `selectable` は未選択だと地も枠も
///   無い素の文字になり、押せるものに見えなかった。暗い地と枠を必ず描く
pub(crate) fn seg_button(ui: &mut egui::Ui, text: &str, note: &str, on: bool) -> egui::Response {
    let font = egui::FontId::proportional(12.5);
    let note_font = egui::FontId::proportional(9.5);
    let ink = if on { theme::ACCENT } else { egui::Color32::from_rgb(0xBF, 0xCA, 0xCA) };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), ink));
    let gn = (!note.is_empty())
        .then(|| ui.fonts_mut(|f| f.layout_no_wrap(note.to_string(), note_font.clone(), theme::AMBER)));
    let pad = egui::vec2(9.0, 5.0);
    let nw = gn.as_ref().map_or(0.0, |n| n.size().x + 3.0);
    let size = egui::vec2(g.size().x + nw + 2.0 * pad.x, g.size().y + 2.0 * pad.y);
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, edge) = if on {
            (theme::ACCENT_BG, theme::ACCENT)
        } else if r.hovered() {
            (egui::Color32::from_rgb(0x15, 0x1C, 0x1E), TILE_HOVER)
        } else {
            (egui::Color32::from_rgb(0x15, 0x1C, 0x1E), TILE_EDGE)
        };
        let p = ui.painter();
        p.rect(rect, 5.0, fill, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        let tl = rect.min + pad;
        p.galley(tl, g, ink);
        if let Some(n) = gn {
            p.galley(egui::pos2(rect.max.x - pad.x - n.size().x, rect.min.y + pad.y - 2.0), n, theme::AMBER);
        }
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 帯域の札1枚の大きさ
const BAND_LED: egui::Vec2 = egui::vec2(46.0, 20.0);

/// マルチスキャンモニタの周波数インジケータの札1枚。いま同期している帯域が点灯する。
///
/// ★**旧パネルの「Mode」欄と同じ見た目にする。** 状態カードへ移したとき、地の色を
///   薄く変えるだけの小さな札にしたら、どれが点いているか読み取れなくなった。
///   点灯はアクセントの塗りに黒文字、消灯は暗い地に枠(何段あるかは見える)
fn band_led(ui: &mut egui::Ui, khz: u32, on: bool) {
    let (rect, _) = ui.allocate_exact_size(BAND_LED, egui::Sense::hover());
    let p = ui.painter();
    let (fill, text) = if on {
        (theme::ACCENT, egui::Color32::BLACK)
    } else {
        (egui::Color32::from_rgb(38, 40, 44), egui::Color32::from_gray(110))
    };
    p.rect_filled(rect, 3.0, fill);
    p.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
                  egui::StrokeKind::Inside);
    p.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{khz}kHz"),
           egui::FontId::monospace(11.0), text);
}

impl Session {
    /// パネルの中身すべて。スクロール領域の中から呼ばれる
    /// パネル全体。上(ナビゲーション・状態・警告)と下(キーの案内)は固定し、
    /// 間のページ本体だけをスクロールする。
    ///
    /// ★**ナビゲーションと状態は流さない。** 1本のスクロールに全部入れていた
    ///   ときは、下へ送ると「‹ ホーム」も状態カードも画面の外へ出て、戻る手段と
    ///   「いま何が映っているか」が同時に消えた
    pub(crate) fn panel_ui(&mut self, ui: &mut egui::Ui) {
        // Esc でホームへ戻る(入力欄に打っている最中は欄の方に渡す)
        if self.page != Page::Home
            && ui.memory(|m| m.focused().is_none())
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.page = Page::Home;
        }
        // 管面を合わせている間は、前のフレームの判定でパネルを薄くする
        self.peek = std::mem::take(&mut self.peek_next);

        self.panel_head(ui);
        self.status_card(ui);
        self.banners(ui);
        ui.add_space(6.0);
        let foot_h = if MOBILE { 0.0 } else { 22.0 };
        // ページごとにスクロール位置を持つ(開いたページは上から始まる)
        egui::ScrollArea::vertical()
            .id_salt(("panel_page", self.page))
            .auto_shrink([false, false])
            .max_height((ui.available_height() - foot_h).max(0.0))
            .show(ui, |ui| {
                match self.page {
                    Page::Home => self.home_page(ui),
                    Page::Video => self.video_page(ui),
                    Page::Display => self.display_page(ui),
                    Page::Audio => self.audio_page(ui),
                    Page::Board => self.board_page(ui),
                    Page::Output => self.output_page(ui),
                    Page::Sync => self.sync_page(ui),
                    Page::Diag => self.diag_page(ui),
                }
                ui.add_space(8.0);
            });
        if !MOBILE {
            self.panel_foot(ui);
        }
    }

    fn panel_foot(&mut self, ui: &mut egui::Ui) {
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, RULE));
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let keys = if self.page == Page::Home {
                "Tab: パネル  S: スキャン  B: ベゼル"
            } else {
                "Esc: ホームへ戻る"
            };
            ui.label(egui::RichText::new(keys).size(10.5).color(LABEL));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(env!("CARGO_PKG_VERSION")).size(10.5).color(LABEL));
            });
        });
    }

    fn panel_head(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.page == Page::Home {
                ui.label(egui::RichText::new("RetroCastX").size(17.0).color(theme::ACCENT));
            } else {
                let back = ui.add(egui::Button::new(
                    egui::RichText::new("‹ ホーム").size(14.0).color(theme::ACCENT)).frame(false));
                if back.clicked() {
                    self.page = Page::Home;
                }
                ui.label(egui::RichText::new(self.page.title()).size(15.0).color(INK));
                if self.page.advanced() {
                    ui.label(egui::RichText::new("上級").size(10.0).color(theme::AMBER));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Tab キーの無いモバイル向けの出口。パネルが全幅だと映像が
                // 見えずタップできないので、いちばん上に置く
                let (text, tip) = if MOBILE {
                    ("映像に戻る", "操作パネルを隠して映像だけにします。\n映像をタップすると戻ります")
                } else {
                    ("隠す Tab", "操作パネルを隠して映像だけにします。\nTab キーで戻せます")
                };
                if ui.button(text).on_hover_text(tip).clicked() {
                    self.show_panel = false;
                    self.mark_settings_dirty();
                }
            });
        });
        ui.add_space(4.0);
    }

    /// いま掴んでいるボードの名前と IP
    fn board_name_addr(&self) -> Option<(String, String)> {
        let mac = self.claimed()?;
        let boards = self.shared.boards.lock().unwrap();
        let b = boards.values().find(|b| b.mac == mac)?;
        Some((b.name.clone(), b.addr.to_string()))
    }

    /// 「どのボードの何が映っているか」。押すと診断へ
    fn status_card(&mut self, ui: &mut egui::Ui) {
        let mode = self.shared.mode.lock().unwrap().clone();
        let fps = self.shared.stats.lock().unwrap().fps;
        let board = self.board_name_addr();
        let (dot, who) = match (&board, &mode) {
            (Some((n, _)), Some(_)) => (theme::OK, n.clone()),
            (Some((n, _)), None) => (theme::AMBER, n.clone()),
            (None, _) => (egui::Color32::from_gray(90), "ボード未接続".to_string()),
        };
        // ★**解像度と周波数は2行に分ける。** 1行にすると、帯域の札と並べたときに
        //   パネル幅(360pt)に収まらず、末尾の fV が切れた(「60.0…」)
        let (line2, line3) = match &mode {
            Some(m) => {
                let il = m.mflags & 0x0001 != 0
                    || self.shared.stats.lock().unwrap().interlace_measured;
                let v = if il { m.vactive } else { m.vactive / 2 };
                (format!("{}×{}{}", m.hactive, v, if il { "i" } else { "" }),
                 Some(format!("{:.2}kHz / {:.2}Hz",
                              m.hfreq_mhz_x1000 as f64 / 1e6, m.vfreq_mhz_x1000 as f64 / 1e3)))
            }
            None if board.is_some() => ("信号を待っています".to_string(), None),
            None => ("LAN にボードが見つかるのを待っています".to_string(), None),
        };
        let r = egui::Frame::new()
            .fill(egui::Color32::from_rgb(0x0B, 0x0F, 0x10))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x22, 0x2C, 0x2E)))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, dot);
                    ui.label(egui::RichText::new(who).size(14.0).color(INK));
                    if mode.is_some() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(format!("{fps:.1} fps")).monospace().color(MUTED));
                        });
                    }
                });
                // 解像度・周波数の2行と、その右に帯域の札(2行の高さの中央)
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    let leds_w = 3.0 * BAND_LED.x + 2.0 * 4.0;
                    let left_w = (ui.available_width() - leds_w - 8.0).max(60.0);
                    ui.allocate_ui_with_layout(egui::vec2(left_w, 0.0),
                                               egui::Layout::top_down(egui::Align::Min), |ui| {
                        let ink = egui::Color32::from_rgb(0xB8, 0xC4, 0xC3);
                        ui.add(egui::Label::new(egui::RichText::new(line2).monospace().size(12.0)
                            .color(ink)).truncate());
                        if let Some(l3) = line3 {
                            ui.add(egui::Label::new(egui::RichText::new(l3).monospace().size(12.0)
                                .color(ink)).truncate());
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for b in [31u32, 24, 15] {
                            band_led(ui, b, self.band_khz == b);
                        }
                    });
                });
            })
            .response;
        // 診断を開いているときは押しても何も起きないので、押せる見た目にしない
        if self.page == Page::Diag {
            return;
        }
        let r = ui.interact(r.rect, ui.id().with("status_card"), egui::Sense::click())
            .on_hover_text("診断を開く");
        if r.clicked() {
            self.page = Page::Diag;
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// 気づかないと困ること。どのページにも出す
    fn banners(&mut self, ui: &mut egui::Ui) {
        let banner = |ui: &mut egui::Ui, text: &str, action: Option<&str>| -> bool {
            let mut hit = false;
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::AMBER.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0, theme::AMBER.gamma_multiply(0.45)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - if action.is_some() { 52.0 } else { 0.0 };
                        ui.allocate_ui(egui::vec2(w, 0.0), |ui| {
                            ui.label(egui::RichText::new(text).size(12.5)
                                .color(egui::Color32::from_rgb(0xF6, 0xB8, 0x8E)));
                        });
                        if let Some(a) = action {
                            hit = ui.button(a).clicked();
                        }
                    });
                });
            hit
        };
        if let Some(err) = self.rx_error.clone() {
            banner(ui, &err, None);
        }
        // ★**送れていないことを明示する。** 「何も映らない」だけだと
        //   受信側の問題と区別できず、原因の切り分けに時間がかかる。
        if let Some(err) = self.shared.net_error.lock().unwrap().clone() {
            banner(ui, &err, None);
        }
        // ★自動では戻さないので、落としていることは常に見えるようにする
        if self.auto_skipped {
            let text = format!("自動で間引き中 {}。パケットの取りこぼしが続いたため",
                               self.frame_skip_label());
            if banner(ui, &text, Some("戻す")) {
                self.auto_skipped = false;
                self.tune_frame_skip = 0;
                self.send_cfg(protocol::CFG_KEY_FRAME_SKIP, self.frame_skip_cfg());
            }
        }
    }

    // ---------------------------------------------------------------- ホーム

    fn home_page(&mut self, ui: &mut egui::Ui) {
        label(ui, "入力");
        self.source_ui(ui);

        label(ui, "音量");
        self.volume_home(ui);

        ui.add_space(8.0);
        self.quick_buttons(ui);

        ui.add_space(8.0);
        let mut cats: Vec<(Page, String)> = vec![
            (Page::Video, self.video_summary()),
            (Page::Display, self.display_summary()),
            (Page::Audio, self.audio_summary()),
            (Page::Board, self.board_summary()),
        ];
        // 配信用の別窓・低遅延全画面・MimicX はデスクトップにしか無い
        if !MOBILE {
            cats.push((Page::Output, self.output_summary()));
        }
        cats.push((Page::Sync, format!("PLL {} · 位相 {}", self.tune_pll_divide, self.tune_phase)));
        cats.push((Page::Diag, self.diag_summary()));
        let rule = |ui: &mut egui::Ui| {
            let r = ui.available_rect_before_wrap();
            ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, RULE));
        };
        rule(ui);
        for (page, sum) in cats {
            if self.category_row(ui, page, &sum) {
                self.page = page;
            }
            rule(ui);
        }
    }

    /// カテゴリの行。題・要約・› を1つの押せる行にする
    fn category_row(&mut self, ui: &mut egui::Ui, page: Page, sum: &str) -> bool {
        let w = ui.available_width();
        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 44.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            if r.hovered() {
                p.rect_filled(rect, 0.0, theme::ACCENT.gamma_multiply(0.05));
            }
            let x = rect.left() + 2.0;
            p.text(egui::pos2(x, rect.top() + 13.0), egui::Align2::LEFT_CENTER, page.title(),
                   egui::FontId::proportional(13.5), INK);
            if page.advanced() {
                let tw = ui.fonts_mut(|f| f.layout_no_wrap(page.title().into(),
                    egui::FontId::proportional(13.5), INK).size().x);
                p.text(egui::pos2(x + tw + 6.0, rect.top() + 13.0), egui::Align2::LEFT_CENTER,
                       "上級", egui::FontId::proportional(10.0), theme::AMBER);
            }
            // 要約は1行に収める(パネルの幅で切る)
            let galley = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(
                    sum.to_string(), egui::FontId::monospace(11.0), MUTED);
                job.wrap = egui::text::TextWrapping::truncate_at_width(w - 26.0);
                f.layout_job(job)
            });
            p.galley(egui::pos2(x, rect.top() + 24.0), galley, MUTED);
            p.text(egui::pos2(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, "›",
                   egui::FontId::proportional(17.0), egui::Color32::from_rgb(0x5F, 0x70, 0x72));
        }
        r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    }

    /// 音量を幅いっぱいに。ミュートは値を保ったままゲインだけ0にする
    fn volume_home(&mut self, ui: &mut egui::Ui) {
        let (vol0, mute0) = (self.audio_volume, self.audio_muted);
        let row = egui::vec2(ui.available_width(), 30.0);
        ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let mute_w = 64.0;
            let num_w = 40.0;
            ui.spacing_mut().slider_width =
                (ui.available_width() - mute_w - num_w - 2.0 * ui.spacing().item_spacing.x).max(60.0);
            ui.add(egui::Slider::new(&mut self.audio_volume, 0.0..=1.5).show_value(false));
            ui.add_sized([num_w, 20.0], egui::Label::new(
                egui::RichText::new(format!("{:.0}", self.audio_volume * 100.0)).monospace()));
            if tile_button(ui, mute_w, "ミュート", self.audio_muted, true).clicked() {
                self.audio_muted = !self.audio_muted;
            }
        });
        if self.audio_volume != vol0 || self.audio_muted != mute0 {
            self.mark_settings_dirty();
        }
        self.apply_gain();
    }

    /// 簡易スキャン・ベゼル・低遅延全画面(モバイルは「画に合わせる」)
    fn quick_buttons(&mut self, ui: &mut egui::Ui) {
        let gap = ui.spacing().item_spacing.x;
        let w = ((ui.available_width() - 2.0 * gap) / 3.0).max(60.0);
        ui.horizontal(|ui| {
            // 簡易スキャン。モードが変わるたびに押すもの(S キーでも)
            if self.auto.is_some() {
                if tile_button(ui, w, "スキャン中… 中止", true, true).clicked() {
                    self.auto = None;
                    self.auto_done = Some("中止しました".into());
                }
            } else {
                let n = self.scan_candidates().len();
                let r = tile_button(ui, w, "簡易スキャン", false, n > 0).on_hover_text(if n > 0 {
                    format!("候補 {n} 点の位相感度を測って、いちばん感度が強い点を採ります\n\
                             (候補1つにつき8秒ほど。位相も最適値に合わせます)。S キーでも実行できます")
                } else {
                    "この fH を説明できる候補がありません\n\
                     (MODE未受信か、機種に無いドットクロック)".to_string()
                });
                if r.clicked() {
                    self.auto_start_quick();
                }
            }
            // ベゼル。枠を選んでいなければ「表示」ページで選ぶよう促す
            let has_bezel = !self.bezel.is_empty();
            let r = tile_button(ui, w, "ベゼル", has_bezel && !self.bezel_off, true)
                .on_hover_text(if has_bezel {
                    "筐体の枠を出し入れします (B キー)"
                } else {
                    "枠をまだ選んでいません。押すと「表示」で選べます"
                });
            if r.clicked() {
                if has_bezel {
                    self.bezel_off = !self.bezel_off;
                    self.mark_settings_dirty();
                } else {
                    self.page = Page::Display;
                }
            }
            if MOBILE {
                if tile_button(ui, w, "画に合わせる", false, true)
                    .on_hover_text("H幅/H位置/V幅/V位置を一度だけ合わせます")
                    .clicked()
                {
                    self.fit_geometry();
                }
            } else {
                let r = tile_button(ui, w, "低遅延全画面", false, self.only_window);
                let r = if self.only_window {
                    r.on_hover_text("映像だけを全画面に出し、遅延を最小にする(画面中央で約1フレーム)。\n\
                                     ESC でこのウィンドウに戻る。vsync は「出力・連携」で")
                } else {
                    r.on_hover_text("ウィンドウが複数あるときは使えない(全部を閉じて起動し直すため)")
                };
                if r.clicked() {
                    self.want_lowlat_fs = true;
                }
            }
        });
        // スキャンの進み具合と直前の結果。押した場所の近くに出ないと見落とす
        if let Some(a) = self.auto.as_ref() {
            ui.label(egui::RichText::new(a.note.clone()).monospace().size(11.0).color(MUTED));
        } else if let Some(d) = self.auto_done.clone() {
            ui.label(egui::RichText::new(d).size(11.0).color(MUTED));
        }
    }

    // ---------------------------------------------------------------- 要約

    fn video_summary(&self) -> String {
        let fmt = self.shared.mode.lock().unwrap().as_ref()
            .map_or_else(|| pixfmt_name(self.tune_pixfmt as u8), |m| pixfmt_name(m.pixfmt));
        let dot = if self.dot_a_milli == 0 {
            "ドット復元 切".to_string()
        } else {
            format!("ドット復元 {:.2}", self.dot_a_milli as f32 / 1000.0)
        };
        format!("{} · {fmt} · {dot}", self.profile_label(&self.source_profile))
    }

    fn display_summary(&self) -> String {
        let a = self.tube_aspect;
        let aspect = if a <= 0.0 { "そのまま" }
            else if (a - 4.0 / 3.0).abs() < 0.01 { "4:3" }
            else if (a - 1.0).abs() < 0.01 { "1:1" }
            else if (a - 16.0 / 9.0).abs() < 0.01 { "16:9" }
            else { "任意" };
        let filter = ["ニアレスト", "バイリニア", "sharp-bilinear"][self.filter.min(2) as usize];
        let mut s = format!("{aspect} · H幅 {:.2} · {filter}", self.mon[0]);
        if self.rotate != 0 {
            s.push_str(&format!(" · {}°", self.rotate * 90));
        }
        if !self.bezel.is_empty() && !self.bezel_off {
            s.push_str(" · 枠");
        }
        s
    }

    fn audio_summary(&self) -> String {
        let src = Self::audio_src_name(self.audio_source);
        let mut s = if self.audio_auto { format!("自動 ({src})") } else { src };
        s.push_str(&format!(" · {}ms", self.audio_buffer_ms));
        if self.spdif_mix {
            s.push_str(" · S/PDIF");
        }
        s
    }

    fn board_summary(&self) -> String {
        let led = if self.led_control { "LED Viewer" } else { "LED ボード既定" };
        match self.board_name_addr() {
            Some((n, a)) => format!("{n} · {a} · {led}"),
            None => format!("未接続 · {led}"),
        }
    }

    fn output_summary(&self) -> String {
        format!("配信窓 {} · vsync {}",
                if self.clean_open { "オン" } else { "オフ" },
                if self.fs_vsync { "あり" } else { "なし" })
    }

    fn diag_summary(&self) -> String {
        let s = self.shared.stats.lock().unwrap().clone();
        format!("受信 {:.1}fps · {:.0}Mbps · lost {}", s.fps, s.mbps, s.lost_packets)
    }

    // ---------------------------------------------------------------- 詳細

    fn video_page(&mut self, ui: &mut egui::Ui) {
        group(ui, "入力", false, |ui| {
            theme::kv(ui, "端子", format!("入力{} {}", self.input_port.number(), self.input_port.jack()));
            theme::kv(ui, "機種", self.profile_label(&self.source_profile));
            ui.label(egui::RichText::new(
                "管面の位置・幅と PLL は「機種 × 周波数帯」ごとに覚えます。").size(11.0).color(MUTED));
            self.input_write_ui(ui);
            self.profile_files_ui(ui);
        });
        ui.add_space(6.0);
        group(ui, "画の仕上げ", false, |ui| {
            self.finish_ui(ui);
        });
    }

    fn display_page(&mut self, ui: &mut egui::Ui) {
        // ★**つまみを動かしている間はパネルを薄くする。** 管面は映像を見ながら
        //   合わせるもので、パネルが重なっていると結果が見えない。つまみの面
        //   だけは残す(何を動かしているかは見えている必要がある)
        let dim = self.peek;
        let r = group(ui, "管面(CRT のつまみ)", false, |ui| {
            self.geometry_ui(ui);
            ui.add_space(2.0);
            self.fit_button_ui(ui);
        });
        if ui.ctx().dragged_id().is_some()
            && r.response.rect.contains(ui.ctx().pointer_interact_pos().unwrap_or_default())
        {
            self.peek_next = true;
        }
        ui.add_space(6.0);
        group(ui, "形と見せ方", dim, |ui| {
            self.shape_ui(ui);
        });
    }

    fn audio_page(&mut self, ui: &mut egui::Ui) {
        group(ui, "音源", false, |ui| {
            self.audio_src_ui(ui);
            self.volume_ui(ui);
            self.spdif_ui(ui);
        });
        ui.add_space(6.0);
        group(ui, "出力", false, |ui| {
            self.audio_ui(ui);
        });
    }

    fn board_page(&mut self, ui: &mut egui::Ui) {
        group(ui, "接続", false, |ui| {
            self.boards_ui(ui);
        });
        ui.add_space(6.0);
        group(ui, "LED", false, |ui| {
            self.led_ui(ui);
        });
    }

    fn output_page(&mut self, ui: &mut egui::Ui) {
        group(ui, "ウィンドウ", false, |ui| {
            self.window_bar(ui);
            self.lowlat_fs_ui(ui);
        });
        ui.add_space(6.0);
        group(ui, "配信(OBS 用)", false, |ui| {
            self.clean_out_ui(ui);
        });
        ui.add_space(6.0);
        // MimicX へのキー転送は macOS だけ(CoreMIDI)
        group(ui, "リモート入力", false, |ui| {
            self.remote_ui(ui);
        });
    }

    fn sync_page(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(
            "映らない・ずれるときの調整です。通常はホームの「簡易スキャン」で合います。")
            .size(11.5).color(MUTED));
        ui.add_space(4.0);
        self.advanced_toggle(ui);
        ui.add_space(4.0);
        group(ui, "", false, |ui| {
            self.tune_ui(ui);
        });
    }

    fn diag_page(&mut self, ui: &mut egui::Ui) {
        self.advanced_toggle(ui);
        ui.add_space(4.0);
        group(ui, "映像", false, |ui| {
            self.diag_mode_ui(ui);
        });
        ui.add_space(6.0);
        group(ui, "受信と表示", false, |ui| {
            self.diag_stats_ui(ui);
        });
    }

    /// ★**調査用の数値と細かい設定の出し入れ。** 以前はパネル最上部にあり、
    ///   常用時には意味の無いチェックがいちばん目立つ場所を取っていた。
    ///   使うのは診断と同期の調整だけなので、その2ページに置く
    fn advanced_toggle(&mut self, ui: &mut egui::Ui) {
        let mut adv = self.show_advanced;
        if ui.checkbox(&mut adv, "調査用の数値と細かい設定を出す")
            .on_hover_text(
                "調査用の数値(present方式・CPU時間・パケット統計など)と、\n\
                 細かい設定(vbp / hs_offset / pll_div / 間引き / 帯域制限 / 位相)を\n\
                 出し入れします。問題を追うときだけ開けば十分です")
            .changed()
        {
            self.show_advanced = adv;
            self.mark_settings_dirty();
        }
    }

    // ------------------------------------------- 旧パネルから移したブロック

    /// 受けているモード(旧パネルの Mode 節)
    fn diag_mode_ui(&mut self, ui: &mut egui::Ui) {
        // 帯域(15/24/31kHz)は上の状態カードに出ている。ここに重ねて出さない
        if let Some(m) = self.shared.mode.lock().unwrap().clone() {
            // ★vactive は**スロット数**であって映像源の行数ではない。
            //
            //   行位置は常に半ライン単位のスロットに置かれる(捕捉側が
            //   インタレースかどうかで分岐しないための設計)。だから
            //   プログレッシブでは1つ飛びに並び、スロットの範囲は
            //   実際の行数の2倍になる。768x512 の X68000 が 768x1024 と
            //   表示されていたのはこれで、誤判定ではない。
            //
            //   インタレースのときは vactive がそのままフレーム行数に
            //   なる(フィールド行数×2 = フレーム行数)ので割らない。
            //   判定は assembler と同じ「mflags か、測定した交互性」。
            //   mflags bit0 だけでは決まらない (フィールドごとに VSYNC が
            //   来る本物のインタレースでは 0 になる) ことに注意。
            let il = m.mflags & 0x0001 != 0
                || self.shared.stats.lock().unwrap().interlace_measured;
            let vsrc = if il { m.vactive } else { m.vactive / 2 };
            ui.monospace(format!("{}x{} (id {})", m.hactive, vsrc, m.mode_id));
            if vsrc != m.vactive && self.show_advanced {
                ui.weak(format!("スロット {}", m.vactive));
            }
            // 実際に届いている LINE の形式。UIの設定値ではなく**電線上の値**
            ui.monospace(pixfmt_name(m.pixfmt));
            // htotal×vtotal もボードの実測値。モード表を作る調査で必要なので出す
            if self.show_advanced {
                theme::kv(ui, "total", format!("{}x{}", m.htotal, m.vtotal));
                theme::kv(ui, "dotclk", format!("{:.4} MHz", m.dotclk_hz as f64 / 1e6));
            }
            theme::kv(ui, "h", format!("{:.3} kHz", m.hfreq_mhz_x1000 as f64 / 1e6));
            theme::kv(ui, "v", format!("{:.3} Hz", m.vfreq_mhz_x1000 as f64 / 1e3));
        } else {
            ui.weak("no mode yet");
        }
    }

    /// 受信・表示・復調の統計(旧パネルの Stats 節)
    fn diag_stats_ui(&mut self, ui: &mut egui::Ui) {
        if self.show_advanced {
            ui.label(if self.no_vsync { "present: no-vsync" } else { "present: vsync (FIFO)" });
        }
        let s = self.shared.stats.lock().unwrap().clone();
        // 描画遅延。基準は各ラインの受信時刻(≒直結の液晶が受け取る時刻)
        let lat = *self.shared.latency.lock().unwrap();
        if lat.frames > 0 {
            ui.monospace(lat.describe(self.frame_ms())).on_hover_text(
                "信号が届いてからの遅れ。上/中/下は画面のその位置のライン。\n\
                 液晶は届いたラインをすぐ出すが、こちらはフレームが揃ってから\n\
                 出すので、上ほど遅れる。\n\
                 走査: 最初〜最後のライン / 待ち: 次フレームの先頭が来て確定するまで /\n\
                 組立: 復調など / UI: 描画側が拾うまで(ウィンドウは60Hzで回る)\n\
                 ウィンドウ表示はここまで。この先に macOS のコンポジタ(通常1フレーム\n\
                 前後)と液晶自身の遅延が乗る。--fullscreen なら画面に出た時刻まで測る");
        }
        if !self.pace.summary.is_empty() {
            // ★**「53Hz」だけ出すと「12%描画できていない」と読める。**
            //   実際には publish を取りこぼしても**絵の情報は失われない**
            //   (フレームバッファは累積で、各publishはその全体のコピー。
            //    次のpublishが上位互換)。効くのは動きの滑らかさだけ。
            //   受信レートと並べ、意味はホバーで説明する。
            let disp = self.pace.new_frame_hz;
            let pct = if s.fps > 1.0 { 100.0 * disp / s.fps } else { 0.0 };
            ui.monospace(format!("表示 {disp:.1}Hz / 受信 {:.1}Hz ({pct:.0}%)",
                                 s.fps))
                .on_hover_text(
                    "画面に出せた新フレームの割合。\n\
                     ★**取りこぼしても絵の情報は失われない。** フレーム\n\
                     バッファは累積で、各フレームはその全体のコピーなので、\n\
                     次のフレームが前のものを含んでいる。効くのは動きの\n\
                     滑らかさだけ。\n\
                     \n\
                     そもそもソース59.75Hzに対しディスプレイは60Hzなので、\n\
                     完璧に拾っても4秒に1回は重複か欠落が出る(原理的)。");
            if !self.pace.cpu_summary.is_empty() {
                ui.monospace(&self.pace.cpu_summary)
                    .on_hover_text(
                        "描画=ui()の呼び出し回数(=新フレームを見に行った\n\
                         回数)。**画面の更新回数ではない。** 画面は\n\
                         ディスプレイのrefresh(60Hz)で更新される。\n\
                         到着の揺らぎより細かく見に行くために多めに回す。");
            }
        }
        theme::kv(ui, "rate", format!("{:.1} fps  {:.1} Mbps", s.fps, s.mbps));
        // 自動の間引きはどのページでも上の帯に出す(`banners`)
        if self.show_advanced {
            theme::kv(ui, "frames", format!("{}", s.frames));
            // 復調(コンポジット)が追いつかず、飛ばしたフィールドの数
            if s.decode_skipped > 0 {
                theme::kv(ui, "skipped", format!("{}", s.decode_skipped));
            }
        }
        // lost はOSのUDPバッファ溢れ、qdrop は組立が追いつかず受信スレッドの
        // キューが満杯で捨てた分。分けて出さないとどちらが詰まっているのか
        // 判断できない
        theme::kv(ui, "pkts", format!("{}  lost {}", s.packets, s.lost_packets));
        self.netcheck_ui(ui, &s);
        if s.queue_drops > 0 {
            ui.monospace(format!("queue drops {}", s.queue_drops))
                .on_hover_text("組立が追いつかず、受信スレッドのキューが\n\
                                満杯で捨てた数。ここが増えるなら組立側が、\n\
                                lost だけ増えるなら受信側が詰まっています");
        }
        if self.show_advanced {
            theme::kv(ui, "orphan", format!("{}", s.orphan_lines));
        }
        // 太らせても埋まらなかった行数。0でないと前フレームの残りが減衰して
        // 薄い影として見える。プログレッシブでは0になるべき
        // インタレースかどうかは mflags では決まらないので測定で判定している。
        // ここが「プログレッシブ扱い」のまま実際はインタレースだと、
        // 太らせが別フィールドの行を複製して縞が交互にちらつく
        if s.interlace_measured {
            // **インタレースでは半分が未充填なのが正常**(残りは前フィールドの
            // 内容 = 織り込み)。ここで警告色にすると毎回不具合に見える
            ui.colored_label(
                theme::OK,
                format!("インタレース(測定): 1フィールドずつ / 太らせ停止・減衰なし\n\
                         前フィールド保持 {}行(半分が正常)", s.unfilled_rows),
            );
        } else if s.unfilled_rows > 0 {
            ui.colored_label(
                theme::AMBER,
                format!("未充填の行 {}", s.unfilled_rows),
            );
        }
        // NTSC復調(YC8のときだけ)。位相差が180°から離れたらコムが
        // 効いていない=色が出ない。コム間隔は測って決めているので、
        // 織り込み設定が変わっても追従する
        if s.ntsc_comb_step > 0 {
            if s.publish_wait_max_ms > 1.0 {
                ui.colored_label(
                    theme::AMBER,
                    format!("フレーム差し替えのUI待ち 合計{:.0}ms 最大{:.0}ms",
                            s.publish_wait_ms, s.publish_wait_max_ms));
            }
            ui.colored_label(
                theme::OK,
                if s.ntsc_svideo {
                    format!("NTSC復調 {}行 コム間隔{} 位相差{:.0}°\n\
                             S端子(赤chのバースト検出) コム未使用",
                            s.ntsc_locked, s.ntsc_comb_step, s.ntsc_phase_deg)
                } else {
                    format!("NTSC復調 {}行 コム間隔{} 位相差{:.0}°\n\
                             3次元コム {}行 動き {:.1}% 位相ズレ{:.1}°\n\
                             TBC {:.3}サンプル",
                            s.ntsc_locked, s.ntsc_comb_step, s.ntsc_phase_deg,
                            s.ntsc_lines_3d, 100.0 * s.ntsc_motion_frac,
                            s.ntsc_phase_drift_deg, s.ntsc_tbc_rms)
                },
            );
        } else if s.ntsc_mono {
            ui.colored_label(
                theme::OK,
                "NTSC 白黒(バーストなし) 黒レベルのみ校正",
            );
        } else if matches!(self.shared.mode.lock().unwrap().as_ref(),
                           Some(m) if protocol::is_raw8(m.pixfmt)) {
            ui.colored_label(
                theme::AMBER,
                "NTSC復調 未ロック(Yのグレースケール表示)",
            );
        }
        // 暗部のフレーム間差分=点状ノイズの量。配線やパスコンの効果を数値で見る
        ui.monospace(format!(
            "noise {:.1}%  lvl {:.1}",
            s.noise_flicker, s.noise_level
        ));
    }

    /// 管面の幾何(H幅/H位置/V幅/V位置)
    fn geometry_ui(&mut self, ui: &mut egui::Ui) {

        // 管面(ブラウン管の物理的な表示領域)。
        //
        // 管面(ブラウン管の物理的な表示領域)。
        //
        // 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど
        // 掃引する」ように周波数ごとに速度が切り替わる。だから管面の横幅は
        // 1/fH そのもので、掃引時間は設定項目ではない。縦も 1VSYNC周期が
        // 管面の高さになる。
        //
        // ここで決めるのは「1周期のうち実際に管面へ出る割合」と「位置」だけ。
        // 1.0 なら周期全体が見えるので何も切れない。実際のCRTは帰線の間ビームが
        // 戻っているので 0.85〜0.95 あたりが現実に近い。
        //
        // 掃引時間を絶対時間で持つ方式も試したが実機と違った。横のサイズは
        // 1/fH に比例するのに、X68000はどのモードも fV が53〜61Hzで1フレームの
        // 時間がほぼ一定なので縦は変わらない。実測で24kHzが横51%・縦95%になり
        // 縦長に潰れた。
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.tube_time_based, "時間ベース")
                .on_hover_text("1HSYNC周期を管面の横幅、1VSYNC周期を高さとする\n\
                                (3モードディスプレイの偏向と同じ)。\n\
                                切ると旧方式(割合を直接指定)になる")
                .changed()
            {
                self.mark_settings_dirty();
            }
            if let Some(m) = self.shared.mode.lock().unwrap().as_ref() {
                let fh_khz = m.hfreq_mhz_x1000 as f32 / 1_000_000.0;
                if fh_khz > 1.0 {
                    ui.weak(format!("1ライン {:.1}µs = 管面の横幅", 1000.0 / fh_khz));
                }
            }
        });
        if self.tube_time_based {
            // 実CRTのH幅/H位置/V幅/V位置つまみに相当する。スライダーにして
            // 「回して合わせる」操作にしている。
            //
            // 絵の内容から自動で中央へ寄せる方法は採らない。幾何が内容に依存して
            // しまい、「幾何は同期信号だけで決まる」という原則が崩れる。実CRTの
            // H位置もモードごとに一度合わせる固定値であって、絵に追従はしない。
            // 帯域ごとに保存されるので一度合わせれば以後は再現される。
            let mut ch = false;
            theme::align_sliders(ui);
            ui.horizontal(|ui| {
                theme::label_col(ui, "H幅");
                ch |= ui.add(egui::Slider::new(&mut self.mon[0], 0.2..=2.0)
                             .fixed_decimals(3).show_value(true))
                    .on_hover_text("1HSYNC周期のうち管面に出る割合。\n\
                                    1.0で帰線期間まで全部見える")
                    .changed();
            });
            ui.horizontal(|ui| {
                theme::label_col(ui, "H位置");
                ch |= ui.add(egui::Slider::new(&mut self.mon[1], -0.5..=0.5)
                             .fixed_decimals(3).show_value(true))
                    .on_hover_text("右へ動かすと絵が右へ動く。\n\
                                    信号は左右非対称(バックポーチが長い)なので、\n\
                                    0だと絵が右に寄る(負の値で左へ戻す)")
                    .changed();
            });
            ui.horizontal(|ui| {
                theme::label_col(ui, "V幅");
                ch |= ui.add(egui::Slider::new(&mut self.mon[2], 0.2..=2.0)
                             .fixed_decimals(3).show_value(true))
                    .on_hover_text("1VSYNC周期のうち管面に出る割合")
                    .changed();
            });
            ui.horizontal(|ui| {
                theme::label_col(ui, "V位置");
                ch |= ui.add(egui::Slider::new(&mut self.mon[3], -0.5..=0.5)
                             .fixed_decimals(3).show_value(true))
                    .on_hover_text("右へ動かすと絵が下へ動く")
                    .changed();
            });
            if ch {
                self.mark_settings_dirty();
            }
            // 有効映像が管面のどれだけを占めるか。モードごとに違って当然で、
            // 15kHzはラインの0.842を占めるので大きく、31kHzは0.696なので小さい
            let (aw, ah) = {
                let st = self.shared.stats.lock().unwrap();
                (st.span_w as f32, st.span_h as f32)
            };
            if let Some(m) = self.shared.mode.lock().unwrap().as_ref() {
                if m.htotal > 0 && m.vtotal > 0 && aw > 0.0 {
                    let fw = aw / m.htotal as f32 / self.mon[0].max(0.01) * 100.0;
                    // 縦はスロット目盛り。1ラインが何スロットを占めるかは
                    // 織り込みの有無で変わる(mflags bit0)
                    let k = if m.mflags & 0x0001 != 0 { 1.0 } else { 2.0 };
                    let vt = m.vtotal as f32 * k;
                    let fh = ah / vt / self.mon[2].max(0.01) * 100.0;
                    ui.weak(format!("有効映像は管面の {fw:.0}% x {fh:.0}%"));
                }
            }
        } else {
            ui.horizontal(|ui| {
                ui.monospace("H位置");
                let mut c = (self.window[0] + self.window[1]) * 0.5;
                let mut w = self.window[1] - self.window[0];
                let a = ui.add(egui::DragValue::new(&mut c).speed(0.002).range(0.0..=1.0)
                               .fixed_decimals(3));
                ui.monospace("H幅");
                let b = ui.add(egui::DragValue::new(&mut w).speed(0.002).range(0.05..=1.0)
                               .fixed_decimals(3));
                if a.changed() || b.changed() {
                    self.window[0] = (c - w * 0.5).clamp(-0.5, 1.5);
                    self.window[1] = self.window[0] + w;
                    self.mark_settings_dirty();
                }
            });
            ui.horizontal(|ui| {
                ui.monospace("V位置");
                let mut c = (self.window[2] + self.window[3]) * 0.5;
                let mut h = self.window[3] - self.window[2];
                let a = ui.add(egui::DragValue::new(&mut c).speed(0.002).range(0.0..=1.0)
                               .fixed_decimals(3));
                ui.monospace("V幅");
                let b = ui.add(egui::DragValue::new(&mut h).speed(0.002).range(0.05..=1.0)
                               .fixed_decimals(3));
                if a.changed() || b.changed() {
                    self.window[2] = (c - h * 0.5).clamp(-0.5, 1.5);
                    self.window[3] = self.window[2] + h;
                    self.mark_settings_dirty();
                }
            });
        }
    }

    /// 回転・管面比・枠・余白・補間
    fn shape_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // 縦画面のゲーム(ドラゴンスピリット等)向け
            ui.monospace("回転");
            let mut rot = self.rotate;
            egui::ComboBox::from_id_salt("rotate")
                .width(72.0)
                .selected_text(["0°", "90°", "180°", "270°"][rot as usize])
                .show_ui(ui, |ui| {
                    for (i, t) in ["0°", "90°", "180°", "270°"].iter().enumerate() {
                        ui.selectable_value(&mut rot, i as u32, *t);
                    }
                });
            if rot != self.rotate {
                self.rotate = rot;
                self.mark_settings_dirty();
            }
        });
        ui.horizontal(|ui| {
            // 管面(表示領域)の縦横比。実際のCRTと同じで、ドット数ではなく
            // 管面の形が表示を決める。4:3 にすれば 512x256 でも 768x512 でも
            // 同じ形に映る。
            ui.monospace("管面");
            let cur = self.tube_aspect;
            let mut sel = cur;
            let name = |a: f32| -> &'static str {
                if a <= 0.0 { "そのまま" }
                else if (a - 4.0 / 3.0).abs() < 0.01 { "4:3" }
                else if (a - 1.0).abs() < 0.01 { "1:1" }
                else if (a - 16.0 / 9.0).abs() < 0.01 { "16:9" }
                else { "任意" }
            };
            egui::ComboBox::from_id_salt("tube")
                .width(96.0)
                .selected_text(name(cur))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, 0.0, "そのまま");
                    ui.selectable_value(&mut sel, 4.0 / 3.0, "4:3");
                    ui.selectable_value(&mut sel, 1.0, "1:1");
                    ui.selectable_value(&mut sel, 16.0 / 9.0, "16:9");
                });
            if sel != cur {
                self.tube_aspect = sel;
                self.mark_settings_dirty();
            }
            if self.mon_key == MON_KEY_NTSC && self.tube_time_based {
                ui.weak(format!("NTSCは規格の画素比 {:.3}", self.tube_aspect_eff()))
                    .on_hover_text("コンポジット/S端子は画素の形が規格で決まるので、\n\
                                    ここの設定ではなく、初期の窓(ビデオデッキと\n\
                                    同じ範囲)を 720サンプル×480ライン = 4:3 で\n\
                                    映した形に固定する。H幅/V幅は実機の振幅\n\
                                    つまみと同じく、絵を伸び縮みさせる");
            }
        });
        ui.horizontal(|ui| {
            // モニタの枠。実在モニタのイラストの開口部に管面をはめる。
            // 実機のCRTと同じで「管面の形が表示を決める」ので、管面モデルの
            // 映像はそのまま開口部に入る。
            ui.monospace("枠");
            let cur = self.bezel.clone();
            let mut sel = cur.clone();
            let name = |k: &str| -> String {
                if k.is_empty() { "なし".into() }
                else { bezel::by_key(k).map_or_else(|| k.to_string(),
                                                   |b| b.label.to_string()) }
            };
            egui::ComboBox::from_id_salt("bezel")
                .width(150.0)
                .selected_text(name(&cur))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, String::new(), "なし");
                    for b in bezel::BEZELS {
                        ui.selectable_value(&mut sel, b.key.to_string(), b.label);
                    }
                });
            if sel != cur {
                self.bezel = sel;
                self.bezel_tex = None;   // 幅が同じでも作り直す
                self.mark_settings_dirty();
            }
            // 選択は保ったまま出し入れする。機種を選び直さずに見比べられる
            let mut on = !self.bezel_off;
            if ui.checkbox(&mut on, "表示")
                .on_hover_text("筐体の枠を出し入れします (B キー)")
                .changed()
            {
                self.bezel_off = !on;
                self.mark_settings_dirty();
            }
        });
        ui.horizontal(|ui| {
            // 管面の外(窓と管面の縦横比が違うと余る領域)の描き方
            ui.monospace("余白");
            let mut sel = self.letterbox;
            egui::ComboBox::from_id_salt("letterbox")
                .width(96.0)
                .selected_text(sel.label())
                .show_ui(ui, |ui| {
                    for s in letterbox::Style::ALL {
                        ui.selectable_value(&mut sel, s, s.label());
                    }
                })
                .response
                .on_hover_text("管面の外に描くもの。\n\
                                斜線: 映像ではない場所であることだけを示す\n\
                                計測帯: 管面の辺にドット座標の目盛りを打ち、\n\
                                空いた帯にモードと表示倍率を出す");
            if sel != self.letterbox {
                self.letterbox = sel;
                self.mark_settings_dirty();
            }
        });
        ui.horizontal(|ui| {
            // 拡大時の補間。sharp-bilinear は非整数倍でもドット幅が不揃いに
            // ならず、かつ全体がぼやけない。高解像度モニタ向けの既定。
            ui.monospace("補間");
            let mut f = self.filter;
            egui::ComboBox::from_id_salt("filter")
                .width(120.0)
                .selected_text(["ニアレスト", "バイリニア", "sharp-bilinear"][f as usize])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f, 0, "ニアレスト");
                    ui.selectable_value(&mut f, 1, "バイリニア");
                    ui.selectable_value(&mut f, 2, "sharp-bilinear");
                });
            if f != self.filter {
                self.filter = f;
                self.mark_settings_dirty();
            }
        });
    }

    /// 伝送形式・ドット復元・残光
    fn finish_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // 伝送形式。RGB888 は 3B/px で帯域は 1.5 倍だが、
            // X68000 の 65536色モードのように元が5bitを超える階調を
            // 持つ場合に丸めが乗らない。RGB555 は 2B/px で帯域が軽い。
            // ゲートウェアは実行時に切り替えるので再ビルドは要らない。
            ui.monospace("伝送形式");
            let mut f = self.tune_pixfmt;
            // 帯域を減らす形式(YCC422 / CVBS8)は対応しているボードにだけ出す
            let compact = self.board_compact();
            egui::ComboBox::from_id_salt("pixfmt")
                .selected_text(pixfmt_name(f as u8))
                .show_ui(ui, |ui| {
                    for v in [protocol::PIXFMT_RGB888, protocol::PIXFMT_RGB555,
                              protocol::PIXFMT_YC8] {
                        ui.selectable_value(&mut f, v as i32, pixfmt_name(v));
                    }
                    if compact {
                        for v in [protocol::PIXFMT_YCC422, protocol::PIXFMT_CVBS8] {
                            ui.selectable_value(&mut f, v as i32, pixfmt_name(v));
                        }
                    }
                });
            if f != self.tune_pixfmt {
                self.tune_pixfmt = f;
                self.send_cfg(protocol::CFG_KEY_PIXFMT, f as u32);
            }
        });
        ui.horizontal_wrapped(|ui| {
            // 狭い画面では右の説明を次の行へ折り返す(縦持ちのスマホで切れていた)
            // ★**ドット復元(1タップ逆フィルタ)。**
            //   X68000 768x512 では本体の出力段で既に1ドットのパルスが
            //   立ち切っておらず、実測で 1ドット=白の64% / 2ドット=82% /
            //   4ドット以上=100%(2026-09-02, v0.9.0)。オシロでコネクタ
            //   直後と基板フィルタ後に差が無かったので、基板ではなく
            //   信号源側が原因と確定している。
            //
            //   一次ローパスの逆演算なので、係数が合っていれば
            //   オーバーシュート(輪郭のリンギング)は出ない。
            //   孤立1ドットの到達率が (1-a) なので a = 1 - 0.64 = 0.36 が理論値。
            //   ただし★**この式は「1サンプル=1ドット」でしか成立しない**
            //   (一次ローパスの逆演算なので、オーバーサンプリングだと前提が崩れる)。
            //   pll_divide を合わせた状態(X68000の512ドット系なら736)で
            //   実測すると、飽和ゼロで1pxが白に届くのは a=0.20 だった。既定はこちら。
            ui.monospace("ドット復元");
            let mut a = self.dot_a_milli as f32 / 1000.0;
            if ui.add(egui::Slider::new(&mut a, 0.0..=0.9).fixed_decimals(2)).changed() {
                self.dot_a_milli = (a * 1000.0).round() as u32;
                self.mark_settings_dirty();
            }
            ui.monospace(if self.dot_a_milli == 0 {
                "切".to_string()
            } else {
                format!("1px を {:.0}% → 100% へ", 100.0 * (1.0 - a))
            });
        });
        ui.horizontal(|ui| {
            // インターレースの残光。
            //
            // インターレースでは毎フレーム半分の行しか来ないのが正常なので、
            // パケットロス用の減衰をそのまま掛けると全行が 100%⇄80% を
            // フィールドレートで往復して面全体がちらつく。既定は 1.00 =
            // 減衰なし(前フィールドの行をそのまま残す = 素直な weave)。
            // 下げるとCRTの残光に近い見え方になる。好みで選べるようにしてある。
            ui.monospace("残光");
            let mut d = self.interlace_decay;
            if ui.add(egui::Slider::new(&mut d, 0.5..=1.0).fixed_decimals(2))
                .on_hover_text("インターレース時に、前のフィールドの行を\n\
                                次のフィールドでどれだけ残すか。\n\
                                1.00 = そのまま残す(チラつき無し)\n\
                                下げるとCRTの残光に近づくがチラつく")
                .changed()
            {
                self.interlace_decay = d;
                *self.shared.interlace_decay.lock().unwrap() = Some(d);
                self.mark_settings_dirty();
            }
        });
    }

    /// 「画に合わせる」
    fn fit_button_ui(&mut self, ui: &mut egui::Ui) {
        // 縦倍率(と 4:3 / 1.0 / integer scaling)は撤去した。表示の形は
        // 管面が決めるので、ドット数に対する倍率という概念が要らない。
        // 512x256 も 768x512 も、管面を 4:3 にすれば同じ形で映る。
        // ★**ウィンドウの大きさは変えない。** 以前はウィンドウを画の等倍に
        //   合わせるボタンだったが、窓は利用者が決めるもの(#45)。いまは
        //   実CRTの技術者が一度だけつまみを回すのと同じく、管面の幾何を合わせる
        if ui.button("画に合わせる")
            .on_hover_text("H幅/H位置/V幅/V位置を一度だけ合わせます。\n\
                            コンポジット/S端子: ビデオデッキと同じ既定の窓に戻す\n\
                            RGB: いま映っている絵の範囲が管面にちょうど収まるようにする\n\
                            (ウィンドウの大きさは変えません)")
            .clicked()
        {
            self.fit_geometry();
        }
    }
}
