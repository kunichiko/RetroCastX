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

/// アプリ内デモのボード(ダミーデバイスを端末内で動かす。retrocastx_devsim::embed)。
///
/// ★**ボードが無くても一通り試せるようにする。** モバイルは LAN のボードを見る
///   だけのアプリなので、ボードが無いと何も映らない(App Store の審査もこれでは
///   機能を確かめられない)。デモを選ぶと 127.0.0.1 の空きポートでダミーが動き、
///   Viewer はそこへ普通のボードとして繋ぐ。LAN には何も出さない。
/// MAC は実機のフォールバック(02:52:43:58:00:01)とも単体のダミー(…:D5:01)とも
/// 違うローカル管理アドレスにする
pub const DEMO_MAC: [u8; 6] = [0x02, 0x52, 0x43, 0x58, 0xDE, 0x30];
pub const DEMO_NAME: &str = "デモ";

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
    /// 「デバイスシミュレータを起動」の結果(一覧の下に出す)
    devsim_msg: Option<String>,
    /// 「このアプリについて」を開いている(src/about.rs)。None なら一覧
    about: Option<crate::about::About>,
}

/// ランチャーで起きたこと
#[derive(Default)]
pub struct Out {
    /// このボードの Viewer を開く(開いていれば前に出す)
    pub open: Option<[u8; 6]>,
    /// ボードIP の欄が変わった(App が保存して発見へ渡す)
    pub ip_changed: bool,
    /// デモを開く(App がダミーを動かしてから DEMO_MAC を開く)
    pub open_demo: bool,
}

impl Launcher {
    pub fn new(disc: Arc<Discovery>, board_ip: String) -> Self {
        Self { disc, board_ip, hints: HashMap::new(), busy: HashSet::new(), peeked: None,
               devsim_msg: None, about: about_at_start() }
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
        if let Some(a) = self.about.as_mut() {
            if a.ui(ui) {
                self.about = None;
            }
            return out;
        }
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
                        // 版を押すと「このアプリについて」(版・ライセンス)
                        let r = ui.add(egui::Button::new(
                            egui::RichText::new(format!("ⓘ {}", env!("CARGO_PKG_VERSION")))
                                .size(if MOBILE { 13.0 } else { 10.5 }).color(MUTED))
                            .frame(false))
                            .on_hover_text("このアプリについて(バージョン・オープンソースライセンス)");
                        if r.clicked() {
                            self.about = Some(crate::about::About::default());
                        }
                    });
                });
                ui.label(egui::RichText::new(if list.is_empty() {
                    "LAN 上のボードを探しています…".to_string()
                } else {
                    format!("ボードを選ぶと Viewer が開きます({}枚)", list.len())
                }).size(12.0).color(MUTED));
                ui.add_space(10.0);

                let foot_h = if MOBILE { 96.0 } else { 66.0 };
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
                        if MOBILE {
                            if list.is_empty() {
                                ui.add_space(16.0);
                            }
                            if demo_card(ui, here.contains(&DEMO_MAC)).clicked() {
                                out.open_demo = true;
                            }
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
        // 開発用のダミーデバイス(client/devsim)を別のプロセスで起動する。
        // デスクトップだけ(配布物では Viewer と同じフォルダ/.app に同梱する)
        if !MOBILE {
            ui.horizontal(|ui| {
                if ui.small_button("デバイスシミュレータを起動").on_hover_text(
                    "持っていない機種の映像信号で試すための、ボードの代役(devsim)を起動します。\n\
                     起動すると、この一覧に「devsim」として出ます").clicked()
                {
                    self.devsim_msg = Some(match launch_devsim() {
                        Ok(()) => "デバイスシミュレータを起動しました".into(),
                        Err(e) => e,
                    });
                }
                if let Some(m) = &self.devsim_msg {
                    ui.label(egui::RichText::new(m).size(10.5).color(MUTED));
                }
            });
        }
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

/// 起動したら「このアプリについて」を開く(環境変数 RCX_ABOUT=1、ライセンスの
/// 一覧なら RCX_ABOUT=licenses)。シミュレータでの確認とスクリーンショット用
fn about_at_start() -> Option<crate::about::About> {
    let v = std::env::var("RCX_ABOUT").ok()?;
    let mut a = crate::about::About::default();
    match v.as_str() {
        "1" => {}
        "licenses" => a.page = crate::about::Page::Licenses,
        _ => return None,
    }
    Some(a)
}

/// デモの札。ボードの札と同じ大きさで、点線の枠にして実機と見分ける
fn demo_card(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let w = ui.available_width();
    let h = if MOBILE { 84.0 } else { 72.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter();
    let edge = if resp.hovered() || open { theme::ACCENT_EDGE } else { CARD_EDGE };
    p.rect_filled(rect, 8.0, if open { theme::ACCENT_BG } else { CARD_BG });
    let r = rect.shrink(0.5);
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    for seg in corners.windows(2) {
        p.extend(egui::Shape::dashed_line(&[seg[0], seg[1]], egui::Stroke::new(1.0, edge), 6.0, 4.0));
    }

    // 左の「画面」に色の帯(テスト柄)を描く
    let scr = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(14.0, (h - 34.0) / 2.0), egui::vec2(46.0, 34.0));
    let bars = [
        egui::Color32::from_rgb(0xC0, 0xC0, 0xC0), egui::Color32::from_rgb(0xC0, 0xC0, 0x00),
        egui::Color32::from_rgb(0x00, 0xC0, 0xC0), egui::Color32::from_rgb(0x00, 0xC0, 0x00),
        egui::Color32::from_rgb(0xC0, 0x00, 0xC0), egui::Color32::from_rgb(0xC0, 0x00, 0x00),
        egui::Color32::from_rgb(0x00, 0x00, 0xC0),
    ];
    let bw = (scr.width() - 6.0) / bars.len() as f32;
    for (i, c) in bars.iter().enumerate() {
        let x0 = scr.left() + 3.0 + bw * i as f32;
        p.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, scr.top() + 3.0),
                                               egui::pos2(x0 + bw, scr.bottom() - 3.0)), 0.0, *c);
    }
    p.rect_stroke(scr, 4.0, egui::Stroke::new(1.5, if open { theme::ACCENT } else { CARD_EDGE }),
                  egui::StrokeKind::Inside);

    let x = scr.right() + 14.0;
    p.text(egui::pos2(x, rect.top() + 20.0), egui::Align2::LEFT_CENTER, DEMO_NAME,
           egui::FontId::proportional(16.0), INK);
    p.text(egui::pos2(x, rect.top() + 40.0), egui::Align2::LEFT_CENTER,
           "ボードが無くても試せます",
           egui::FontId::proportional(12.0), MUTED);
    p.text(egui::pos2(x, rect.top() + 57.0), egui::Align2::LEFT_CENTER,
           "端末の中で作ったレトロ機の映像",
           egui::FontId::proportional(10.5), egui::Color32::from_gray(100));
    let (tag, col) = if open { ("表示中", theme::ACCENT) } else { ("開く ›", INK) };
    p.text(egui::pos2(rect.right() - 14.0, rect.center().y), egui::Align2::RIGHT_CENTER,
           tag, egui::FontId::proportional(12.5), col);
    resp.on_hover_text("ボードの代わりに、端末の中で信号を作って映します(LAN には何も出しません)")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// ダミーデバイス(devsim)の実行ファイルを、Viewer と同じフォルダから探して起動する。
///
/// cargo でビルドしたとき(target/release)も、Mac の .app(Contents/MacOS)も、
/// Windows の配布物(RetroCastX.exe の隣)も、同じフォルダに置く
fn launch_devsim() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("起動できませんでした: {e}"))?;
    let dir = exe.parent().ok_or("起動できませんでした: 実行ファイルの場所が分かりません")?;
    let path = dir.join(format!("retrocastx-devsim{}", std::env::consts::EXE_SUFFIX));
    if !path.is_file() {
        return Err(format!("{} が見つかりません", path.display()));
    }
    std::process::Command::new(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("起動できませんでした: {e}"))
}

/// ランチャー窓の位置と大きさ(`launcher.conf`)。
///
/// ★**Viewer の番号ごとの設定には入れない。** 0 番はボードの窓の設定で、
///   ランチャーの窓と同じファイルに置くと、0 番の Viewer が開いている間に
///   両方が同じキーを書き合う。
/// ★**位置は外枠で覚える。** `with_position` が指すのは外枠なので、内枠を
///   渡すと毎回タイトルバーの高さだけ下へずれる(Viewer の窓で踏んだ)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub pos: Option<egui::Pos2>,
    pub size: egui::Vec2,
}

impl Default for Geom {
    fn default() -> Self {
        Self { pos: None, size: egui::vec2(440.0, 520.0) }
    }
}

impl Geom {
    fn path() -> std::path::PathBuf {
        Settings::path().with_file_name("launcher.conf")
    }

    pub fn load() -> Self {
        Self::parse(&std::fs::read_to_string(Self::path()).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut g = Self::default();
        let (mut x, mut y) = (None, None);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let Ok(v) = v.trim().parse::<f32>() else { continue };
            match k.trim() {
                "window_x" => x = Some(v),
                "window_y" => y = Some(v),
                "window_w" => g.size.x = v.clamp(320.0, 8192.0),
                "window_h" => g.size.y = v.clamp(280.0, 8192.0),
                _ => {}
            }
        }
        if let (Some(x), Some(y)) = (x, y) {
            g.pos = Some(egui::pos2(x, y));
        }
        g
    }

    fn format(&self) -> String {
        let mut s = String::from("# RetroCast X ランチャーの窓(自動生成。消しても既定値で起動する)\n");
        if let Some(p) = self.pos {
            s.push_str(&format!("window_x = {:.0}\nwindow_y = {:.0}\n", p.x, p.y));
        }
        s.push_str(&format!("window_w = {:.0}\nwindow_h = {:.0}\n", self.size.x, self.size.y));
        s
    }

    /// 書けなくても致命的ではないので黙って諦める(次回は既定値で起動する)
    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, self.format());
    }
}

/// ランチャー窓の動きを追い、止まってから書く(ドラッグ中に書き続けない)
#[derive(Default)]
pub struct GeomTracker {
    pub geom: Geom,
    dirty: Option<Instant>,
}

impl GeomTracker {
    pub fn new(geom: Geom) -> Self {
        Self { geom, dirty: None }
    }

    /// 毎フレーム、主ウィンドウの内枠と外枠を渡す
    pub fn observe(&mut self, inner: Option<egui::Rect>, outer: Option<egui::Rect>) {
        if let Some(r) = inner {
            if (r.size() - self.geom.size).length() > 1.0 {
                self.geom.size = r.size();
                self.dirty = Some(Instant::now());
            }
        }
        if let Some(r) = outer.or(inner) {
            if self.geom.pos.is_none_or(|p| (r.min - p).length() > 1.0) {
                self.geom.pos = Some(r.min);
                self.dirty = Some(Instant::now());
            }
        }
        if self.dirty.is_some_and(|t| t.elapsed() >= Duration::from_millis(700)) {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        if self.dirty.take().is_some() {
            self.geom.save();
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

/// 新しくボードに割り当てた番号へ、見た目の好みを引き継ぐ。
///
/// ★**引き継ぐのは好みだけ。** 管面の縦横比・余白の描き方・補間・ベゼル。
///   H幅/V幅・PLL・位相は繋ぐ機械と線で決まる値なので、別のボードのものを
///   持ってくると合わない。
/// ★**前からこのボードの番号なら何もしない。** ユーザーがこの窓で決めた値を
///   上書きしない。
/// ★**元はいちばん最近使われた番号で、こちらより新しいときだけ。** 1枚だけで
///   使ってきた人の 0 番(接続先が「自動」)は自分がいちばん新しいので、古い
///   「別窓」の番号の値で上書きされない。逆に、以前の「別窓」で書いたきりの
///   番号(2026-11-08 以前の既定「斜線」が残っていた)は、いまの好みに揃う。
pub fn inherit_look(id: u32, mac: [u8; 6], cfg: &mut Settings) {
    if cfg.board_sel == BoardSel::Mac(mac) {
        return;
    }
    let others: Vec<(u32, Settings)> = (0..claim::MAX_SLOTS)
        .filter(|i| *i != id)
        .map(|i| (i, Settings::peek_slot(i)))
        .collect();
    let stamps: Vec<(u32, u64)> = others.iter().map(|(i, s)| (*i, s.last_used)).collect();
    let Some(src) = look_source(cfg.last_used, &stamps) else { return };
    if let Some((_, s)) = others.iter().find(|(i, _)| *i == src) {
        eprintln!("ウィンドウ #{id} に #{src} の見た目(縦横比・余白・補間・ベゼル)を引き継ぎます");
        copy_look(s, cfg);
    }
}

/// 見た目の好みを写す元の番号(`inherit_look`)。`stamps` は (番号, 最後に使った時刻)
fn look_source(target_last_used: u64, stamps: &[(u32, u64)]) -> Option<u32> {
    stamps
        .iter()
        .filter(|(_, t)| *t > target_last_used)
        .max_by_key(|(_, t)| *t)
        .map(|(i, _)| *i)
}

fn copy_look(src: &Settings, dst: &mut Settings) {
    dst.tube_aspect = src.tube_aspect;
    dst.letterbox = src.letterbox.clone();
    dst.filter = src.filter;
    dst.bezel = src.bezel.clone();
    dst.bezel_off = src.bezel_off;
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
    fn geom_roundtrips_and_keeps_default_when_unknown() {
        let g = Geom { pos: Some(egui::pos2(120.0, -40.0)), size: egui::vec2(500.0, 610.0) };
        assert_eq!(Geom::parse(&g.format()), g);
        // 初回(ファイル無し)は位置を決めない(OS に任せる)
        assert_eq!(Geom::parse(""), Geom::default());
        assert_eq!(Geom::parse("").pos, None);
    }

    /// ★**新しい側にだけ写す。** 1枚で使ってきた 0 番(いちばん新しい)を、古い
    ///   「別窓」の番号の値で上書きしない
    #[test]
    fn look_comes_from_the_newest_slot_only_if_newer() {
        let stamps = [(0, 1_000), (2, 500), (3, 0)];
        // 書いたことの無い番号(0)・古い番号は、いちばん新しい 0 番から
        assert_eq!(look_source(0, &[(0, 1_000), (2, 500)]), Some(0));
        assert_eq!(look_source(300, &stamps), Some(0));
        // 自分がいちばん新しければ写さない
        assert_eq!(look_source(2_000, &stamps), None);
        // 写すのは好みだけで、機械ごとの値は持ってこない
        let mut src = Settings::default();
        src.tube_aspect = 1.25;
        src.letterbox = "hatch".into();
        src.filter = 0;
        src.bezel = "cz612d".into();
        src.mon_bands.insert(("x68000".into(), 31), [0.8, 0.0, 0.9, 0.0]);
        let mut dst = Settings::default();
        copy_look(&src, &mut dst);
        assert_eq!((dst.tube_aspect, dst.letterbox.as_str(), dst.filter, dst.bezel.as_str()),
                   (1.25, "hatch", 0, "cz612d"));
        assert!(dst.mon_bands.is_empty(), "管面(機械ごとの値)まで写した");
    }

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
