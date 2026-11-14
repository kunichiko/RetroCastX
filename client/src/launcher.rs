//! ランチャー。**見つかったボードを並べ、選んだボードの Viewer を開く。**
//!
//! ★**窓 = ボード にする。** 以前は主ウィンドウの中でボードを切り替え、別の
//!   ボードは「別窓」で開いていた。主ウィンドウだけが特別扱いで、閉じるとアプリ
//!   ごと終わり、どの窓がどのボードかも窓の中を見ないと分からなかった。ボードを
//!   2枚持つのは別の機械を繋ぐためなので、ボードごとに窓を開き、窓の中では
//!   替えない。一覧の窓(このランチャー)を閉じたときだけアプリが終わる。
//! ★**モバイルは窓が1枚なので、一覧と Viewer を切り替えて見せる。** Viewer の
//!   「一覧へ」で戻る。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::discovery::Discovery;
use crate::profiles;
use crate::receiver::BoardInfo;
use crate::settings::{self, BoardInput, BoardSel, Settings};
use crate::{claim, theme, MOBILE};

const INK: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xEB, 0xEA);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x9A, 0x99);
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(0x15, 0x1B, 0x1D);
const CARD_EDGE: egui::Color32 = egui::Color32::from_rgb(0x2B, 0x37, 0x39);

/// 設定ファイルと claim のロックを覗き直す間隔。毎フレーム開くほどのものではない
const PEEK_EVERY: Duration = Duration::from_millis(1500);

/// ボードの様子(一覧の札)
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// このアプリの窓で開いている
    Here,
    /// 別の Viewer(別プロセス・別の機械ではない)が掴んでいる
    Elsewhere,
    Free,
}

pub struct Launcher {
    pub disc: Arc<Discovery>,
    /// 手入力のボードIP(モバイル)。設定の 0 番に保存する
    pub board_ip: String,
    /// ボードごとの「前に選んでいた入力」(設定ファイルから)
    hints: HashMap<[u8; 6], String>,
    /// 他の Viewer が掴んでいるボード
    busy: HashSet<[u8; 6]>,
    peeked: Option<Instant>,
}

/// ランチャーで起きたこと
#[derive(Default)]
pub struct Out {
    /// このボードの Viewer を開く(開いていれば前に出す)
    pub open: Option<[u8; 6]>,
    /// ボードIP の欄が変わった(App が保存して発見へ渡す)
    pub ip_changed: bool,
}

impl Launcher {
    pub fn new(disc: Arc<Discovery>, board_ip: String) -> Self {
        Self { disc, board_ip, hints: HashMap::new(), busy: HashSet::new(), peeked: None }
    }

    /// 前に選んでいた入力と、他の Viewer の使用状況を覗き直す
    fn peek(&mut self, list: &[BoardInfo], here: &HashSet<[u8; 6]>) {
        if self.peeked.is_some_and(|t| t.elapsed() < PEEK_EVERY) {
            return;
        }
        self.peeked = Some(Instant::now());
        self.busy = list
            .iter()
            .map(|b| b.mac)
            .filter(|m| !here.contains(m) && claim::taken_by_other(m, None))
            .collect();
        // ★**そのボードを開いていた窓の値を優先する。** 入力の選択はどの窓の
        //   設定にも board_inputs として残るが、最後に使ったのはその窓
        self.hints.clear();
        let slots: Vec<Settings> = (0..claim::MAX_SLOTS).map(Settings::peek_slot).collect();
        for b in list {
            let bound = slots.iter().find(|s| s.board_sel == BoardSel::Mac(b.mac));
            let bi = bound
                .and_then(|s| s.board_inputs.get(&b.mac))
                .or_else(|| slots.iter().find_map(|s| s.board_inputs.get(&b.mac)));
            if let Some(bi) = bi {
                self.hints.insert(b.mac, input_label(bi));
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, here: &HashSet<[u8; 6]>) -> Out {
        let mut out = Out::default();
        let list = self.disc.list();
        self.peek(&list, here);
        // 一覧は ANNOUNCE が来たときに描き直されるが、使用状況はファイルなので
        // 時々は見に行く
        ui.ctx().request_repaint_after(PEEK_EVERY);

        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("RetroCast X").size(20.0).color(theme::ACCENT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(env!("CARGO_PKG_VERSION"))
                            .size(10.5).color(MUTED));
                    });
                });
                ui.label(egui::RichText::new(if list.is_empty() {
                    "LAN 上のボードを探しています…".to_string()
                } else {
                    format!("ボードを選ぶと Viewer が開きます({}枚)", list.len())
                }).size(12.0).color(MUTED));
                ui.add_space(10.0);

                let foot_h = if MOBILE { 96.0 } else { 40.0 };
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height((ui.available_height() - foot_h).max(0.0))
                    .show(ui, |ui| {
                        if list.is_empty() {
                            self.empty_ui(ui);
                        }
                        for b in &list {
                            let state = if here.contains(&b.mac) {
                                Use::Here
                            } else if self.busy.contains(&b.mac) {
                                Use::Elsewhere
                            } else {
                                Use::Free
                            };
                            if self.card(ui, b, state).clicked() {
                                out.open = Some(b.mac);
                            }
                            ui.add_space(8.0);
                        }
                    });
                self.foot(ui, &mut out);
            });
        out
    }

    /// 見つからないあいだ。**黙って空にしない** ─ 探している最中なのか、送れて
    /// いないのかを分ける
    fn empty_ui(&self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.add(egui::Spinner::new().size(22.0).color(theme::ACCENT));
            ui.add_space(8.0);
            if let Some(e) = self.disc.net_error.lock().unwrap().clone() {
                ui.label(egui::RichText::new(e).size(11.5).color(theme::AMBER));
            } else {
                ui.label(egui::RichText::new(
                    "ボードの電源と LAN ケーブルを確かめてください。\n\
                     見つかると、ここに並びます")
                    .size(11.5).color(MUTED));
            }
        });
    }

    /// ボード1枚の札。札全体が押せる
    fn card(&self, ui: &mut egui::Ui, b: &BoardInfo, state: Use) -> egui::Response {
        let w = ui.available_width();
        let h = if MOBILE { 84.0 } else { 72.0 };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        let edge = if resp.hovered() || state == Use::Here { theme::ACCENT_EDGE } else { CARD_EDGE };
        let fill = if state == Use::Here { theme::ACCENT_BG } else { CARD_BG };
        p.rect(rect, 8.0, fill, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);

        // 左の「画面」。開いている窓があれば点いている
        let scr = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(14.0, (h - 34.0) / 2.0), egui::vec2(46.0, 34.0));
        let lit = state == Use::Here;
        p.rect(scr, 4.0,
               if lit { egui::Color32::from_rgb(0x10, 0x3C, 0x3E) } else { egui::Color32::BLACK },
               egui::Stroke::new(1.5, if lit { theme::ACCENT } else { CARD_EDGE }),
               egui::StrokeKind::Inside);
        let dot = match state {
            Use::Here => theme::ACCENT,
            Use::Elsewhere => theme::AMBER,
            Use::Free => theme::OK,
        };
        p.circle_filled(scr.right_bottom() + egui::vec2(-6.0, -6.0), 3.0, dot);

        let x = scr.right() + 14.0;
        p.text(egui::pos2(x, rect.top() + 20.0), egui::Align2::LEFT_CENTER, &b.name,
               egui::FontId::proportional(16.0), INK);
        let input = self.hints.get(&b.mac).cloned().unwrap_or_else(|| "初めてのボード".into());
        p.text(egui::pos2(x, rect.top() + 40.0), egui::Align2::LEFT_CENTER,
               format!("{input} · {}", b.addr),
               egui::FontId::proportional(12.0), MUTED);
        p.text(egui::pos2(x, rect.top() + 57.0), egui::Align2::LEFT_CENTER,
               format!("{}  fw {}.{}.{}", claim::mac_string(&b.mac),
                       (b.fw_version >> 12) & 0xF, (b.fw_version >> 6) & 0x3F,
                       b.fw_version & 0x3F),
               egui::FontId::monospace(10.0), egui::Color32::from_gray(100));

        let (tag, col) = match state {
            Use::Here => ("表示中", theme::ACCENT),
            Use::Elsewhere => ("他の Viewer が使用中", theme::AMBER),
            Use::Free => ("開く ›", INK),
        };
        p.text(egui::pos2(rect.right() - 14.0, rect.center().y), egui::Align2::RIGHT_CENTER,
               tag, egui::FontId::proportional(12.5), col);
        let tip = match state {
            Use::Here => "開いている窓を前に出します",
            Use::Elsewhere => "開くと、その Viewer が手放したときに繋がります\n\
                              (1枚のボードは同時に1つの Viewer でしか見られません)",
            Use::Free => "このボードの Viewer を開きます",
        };
        // ★**MACがEEPROM由来でない基板は出荷できない**(同じLANに2枚で両方通信不能)
        let tip = if b.caps & 0x0001 == 0 {
            format!("{tip}\n★MACをEEPROMから読めていません(出荷不可)")
        } else {
            tip.to_string()
        };
        resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    fn foot(&mut self, ui: &mut egui::Ui, out: &mut Out) {
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(),
                           egui::Stroke::new(1.0, egui::Color32::from_rgb(0x1D, 0x26, 0x28)));
        ui.add_space(6.0);
        let dest = self.disc.sub_dest.lock().unwrap().clone();
        ui.label(egui::RichText::new(format!("探索 → {}", if dest.is_empty() { "…" } else { &dest }))
            .size(10.5).color(MUTED))
            .on_hover_text(
                "各NICのサブネット宛ブロードキャストへ、発見だけの問いかけを2秒ごとに\n\
                 送っています。映像の送り先は動かないので、他の Viewer の邪魔はしません");
        // ★**iOS はブロードキャストを送れないことがある。** そのときの逃げ道
        if MOBILE {
            ui.horizontal(|ui| {
                theme::label_col(ui, "ボードのIP");
                let r = ui.add(egui::TextEdit::singleline(&mut self.board_ip)
                    .desired_width(160.0)
                    .hint_text("見つからないときだけ"));
                if r.changed() {
                    out.ip_changed = true;
                }
            });
        }
    }
}

/// 「入力2 X68000」の形
fn input_label(bi: &BoardInput) -> String {
    let key = bi
        .profiles
        .get(&bi.port)
        .cloned()
        .unwrap_or_else(|| bi.port.default_profile().to_string());
    let name = if key.is_empty() {
        "汎用".to_string()
    } else if let Some(p) = profiles::by_key(&key) {
        p.label.to_string()
    } else {
        // ユーザーの機種(`u:…`)。名前はフォルダのファイルにあるので、キーで出す
        key.trim_start_matches("u:").to_string()
    };
    format!("入力{} {}", bi.port.number(), name)
}

/// ボードを開く窓の番号を選ぶ。
///
/// ★**前にそのボードを開いていた番号を使う。** 管面・PLL・音量などは番号ごとの
///   設定ファイルにあるので、同じボードは同じ番号で開けば前回の続きになる。
/// ★**次に、どのボードにも付いていない番号。** これまで1枚で使っていた人の 0 番
///   (接続先が「自動」)は、最初に開いたボードが受け継ぐ。
pub fn slot_for(mac: [u8; 6], in_use: &[u32]) -> Option<u32> {
    let free: Vec<u32> = (0..claim::MAX_SLOTS).filter(|i| !in_use.contains(i)).collect();
    let sel: Vec<(u32, BoardSel)> =
        free.iter().map(|&i| (i, Settings::peek_slot(i).board_sel)).collect();
    pick_slot(mac, &sel).or_else(|| free.first().copied())
}

/// `slot_for` の選び方だけ(空いている番号と、その接続先)
fn pick_slot(mac: [u8; 6], free: &[(u32, BoardSel)]) -> Option<u32> {
    free.iter()
        .find(|(_, s)| *s == BoardSel::Mac(mac))
        .or_else(|| free.iter().find(|(_, s)| !matches!(s, BoardSel::Mac(_))))
        .map(|(i, _)| *i)
}

/// 起動時に開き直すボード(前回いっしょに開いていた窓のうち、ボードの決まったもの)
pub fn boards_to_restore() -> Vec<(u32, [u8; 6])> {
    Settings::slots_to_restore(u32::MAX, claim::MAX_SLOTS)
        .into_iter()
        .filter_map(|id| match Settings::peek_slot(id).board_sel {
            settings::BoardSel::Mac(m) => Some((id, m)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_prefers_the_boards_own_then_unbound() {
        let (a, b) = ([2, 0, 0, 0, 0, 0xa], [2, 0, 0, 0, 0, 0xb]);
        let free = [(0, BoardSel::Mac(b)), (1, BoardSel::Auto), (2, BoardSel::Mac(a))];
        assert_eq!(pick_slot(a, &free), Some(2), "前にそのボードを開いた番号");
        // 初めてのボードは、どのボードにも付いていない番号(従来の 0 番 = 自動)
        let c = [2, 0, 0, 0, 0, 0xc];
        assert_eq!(pick_slot(c, &free), Some(1));
        // 全部が他のボードのものなら決めない(呼び側が空きの先頭を使う)
        assert_eq!(pick_slot(c, &[(0, BoardSel::Mac(a))]), None);
    }
}
