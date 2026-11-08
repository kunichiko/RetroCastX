//! 管面の外(窓と管面の縦横比が違うときに余る領域)の描き方。
//!
//! ★**管面の黒と見分けられるようにする。** 実機のCRTと同じで、信号が無い管面は
//!   真っ黒になる。余白まで黒だと「映像の黒」と「そもそも映像ではない場所」の
//!   区別がつかず、H位置・H幅を合わせるときに管面の端が分からない。
//!
//! ★**余白は暗く保つ。** 明るいと目が順応して管面の黒が浮いて見える。
//!   線も文字も地の上でうっすら読める程度に留める。
//!
//! 余白は窓の形しだいで左右にも上下にもできる(スマホの縦画面のような縦長の
//! 窓なら上下)。どちら向きでも同じ規則で描けるよう、穴(管面や枠)を除いた
//! 4本の帯として扱う。

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::theme;

/// 余白の描き方。設定ファイルには `key()` で書く。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// 斜線。「ここは映像ではない」をいちばんはっきり示す
    Hatch,
    /// 計測帯。管面の辺にドット座標の目盛りを打ち、空いた帯にモードを出す
    Ruler,
}

impl Style {
    // 既定(計測帯)を先に並べる
    pub const ALL: [Style; 2] = [Style::Ruler, Style::Hatch];

    pub fn key(self) -> &'static str {
        match self {
            Style::Hatch => "hatch",
            Style::Ruler => "ruler",
        }
    }

    pub fn from_key(k: &str) -> Option<Style> {
        Style::ALL.into_iter().find(|s| s.key() == k)
    }

    pub fn label(self) -> &'static str {
        match self {
            Style::Hatch => "斜線",
            Style::Ruler => "計測帯",
        }
    }
}

/// 目盛りを打つ1つの軸。
///
/// ★**目盛りは絵の座標に打つ。** 0 と最終ドット(`len`)に目盛りを置き、その間を
///   16分割して2つおき(1/8ごと)に数値を出す。管面の辺を等分するのではないので、
///   H幅/H位置/V幅/V位置を回すと目盛りが絵と一緒に動き、管面の外に出た
///   目盛りは打たない。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    /// 管面の始端(左/上)と終端(右/下)に来ているバッファ座標。逆向きもある(回転)
    pub a: f32,
    pub b: f32,
    /// 絵の長さ(最終ドットの座標)
    pub len: f32,
}

impl Axis {
    /// 目盛り n(0..=TICK_DIV)の、管面の辺の上での位置 0..1。辺の外なら None
    pub fn place(&self, n: u32) -> Option<f32> {
        let v = self.len * n as f32 / TICK_DIV as f32;
        let span = self.b - self.a;
        if span.abs() < 1e-6 {
            return None;
        }
        let f = (v - self.a) / span;
        (-1e-4..=1.0 + 1e-4).contains(&f).then_some(f.clamp(0.0, 1.0))
    }

    pub fn value(&self, n: u32) -> i32 {
        (self.len * n as f32 / TICK_DIV as f32).round() as i32
    }
}

/// 計測帯に出す情報。
pub struct Info {
    /// 管面の画面上の 横・縦 の目盛り(回転済み)。映像が無ければ `None`
    pub ruler: Option<[Axis; 2]>,
    /// 1行目(モード)。映像が無ければ `None`
    pub mode: Option<String>,
    /// 2行目以降
    pub lines: Vec<String>,
}

const GROUND_HATCH: Color32 = Color32::from_rgb(0x0C, 0x10, 0x12);
const GROUND_RULER: Color32 = Color32::from_rgb(0x10, 0x14, 0x16);
/// 斜線の間隔[point]
const HATCH_STEP: f32 = 14.0;
/// 計測帯の点の間隔[point]
const GRID_STEP: f32 = 24.0;
/// 目盛りの長さ[point](小 / 大)
const TICK_MINOR: f32 = 7.0;
const TICK_MAJOR: f32 = 11.0;
/// 目盛りの分割数。2つおき(1/8ごと)に大目盛りと数値を置く
const TICK_DIV: u32 = 16;
/// 情報の文字の大きさと、帯の端からの余白[point]
const INFO_FONT: f32 = 11.0;
const INFO_MARGIN: f32 = 14.0;

/// `area` から `hole` を除いた4本の帯(上・下・左・右)。
/// 左右の帯は穴の高さぶんだけなので、4本は重ならない。
pub fn bars(area: Rect, hole: Rect) -> [Rect; 4] {
    let hole = hole.intersect(area);
    [
        Rect::from_min_max(area.min, Pos2::new(area.max.x, hole.min.y)),
        Rect::from_min_max(Pos2::new(area.min.x, hole.max.y), area.max),
        Rect::from_min_max(Pos2::new(area.min.x, hole.min.y), Pos2::new(hole.min.x, hole.max.y)),
        Rect::from_min_max(Pos2::new(hole.max.x, hole.min.y), Pos2::new(area.max.x, hole.max.y)),
    ]
}

/// 情報ブロック(大きさ `need`)を置く場所。入らなければ `None`。
///
/// ★**左を先に見る。** 右には操作パネルが重なるので、左右に帯があるなら左へ。
///   縦長の窓(上下に帯)なら下、だめなら上。目盛りの数値ぶんは避ける。
pub fn info_slot(area: Rect, hole: Rect, need: Vec2) -> Option<Rect> {
    let [top, bottom, left, _right] = bars(area, hole);
    let pad = Vec2::splat(INFO_MARGIN);
    let fits = |r: Rect| r.width() >= need.x + 2.0 * pad.x && r.height() >= need.y + 2.0 * pad.y;
    // 左の帯は目盛りの数値が右端に来るので、その幅(~5文字)を空ける
    let left_room = Rect::from_min_max(left.min, Pos2::new(left.max.x - 48.0, left.max.y));
    if fits(left_room) {
        return Some(Rect::from_min_size(left_room.min + pad, need));
    }
    // 下の帯は大目盛りの長さぶん下げる
    let below = Rect::from_min_max(Pos2::new(bottom.min.x, bottom.min.y + TICK_MAJOR), bottom.max);
    if fits(below) {
        return Some(Rect::from_min_size(below.min + pad, need));
    }
    // 上の帯は数値が下端に並ぶので、その高さぶん削る
    let above = Rect::from_min_max(top.min, Pos2::new(top.max.x, top.max.y - TICK_MAJOR - 16.0));
    if fits(above) {
        return Some(Rect::from_min_size(above.min + pad, need));
    }
    None
}

/// 余白を描く。`hole` は管面(枠ありなら枠全体)で、ここには描かない。
/// `tube` は目盛りを打つ管面の矩形。枠ありのときは枠に重なるので `None` を渡す。
pub fn paint(
    painter: &egui::Painter,
    area: Rect,
    hole: Rect,
    tube: Option<Rect>,
    style: Style,
    info: &Info,
) {
    let bars = bars(area, hole);
    match style {
        Style::Hatch => {
            for b in bars.iter().filter(|b| b.is_positive()) {
                paint_hatch(&painter.with_clip_rect(*b), area, *b);
            }
        }
        Style::Ruler => {
            for b in bars.iter().filter(|b| b.is_positive()) {
                paint_grid(&painter.with_clip_rect(*b), area, *b);
            }
            if let (Some(t), Some(d)) = (tube, info.ruler) {
                paint_ticks(painter, &bars, t, d);
                paint_corners(painter, t);
            }
            paint_info(painter, area, hole, info);
        }
    }
}

fn paint_hatch(p: &egui::Painter, area: Rect, b: Rect) {
    p.rect_filled(b, 0.0, GROUND_HATCH);
    let stroke = Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.13));
    // x + y = c の線(右上がり)を窓の原点から等間隔に引く。帯ごとに描いても
    // 縞が帯の境目で揃うよう、位相は帯ではなく窓で決める
    let base = area.min.x + area.min.y;
    let c0 = b.min.x + b.min.y;
    let c1 = b.max.x + b.max.y;
    let mut c = base + ((c0 - base) / HATCH_STEP).floor() * HATCH_STEP;
    while c <= c1 {
        p.line_segment(
            [Pos2::new(c - b.max.y, b.max.y), Pos2::new(c - b.min.y, b.min.y)],
            stroke,
        );
        c += HATCH_STEP;
    }
}

fn paint_grid(p: &egui::Painter, area: Rect, b: Rect) {
    p.rect_filled(b, 0.0, GROUND_RULER);
    let col = Color32::from_white_alpha(14);
    let snap = |v: f32, o: f32| o + ((v - o) / GRID_STEP).ceil() * GRID_STEP + GRID_STEP * 0.5;
    let mut y = snap(b.min.y, area.min.y);
    while y < b.max.y {
        let mut x = snap(b.min.x, area.min.x);
        while x < b.max.x {
            p.rect_filled(Rect::from_center_size(Pos2::new(x, y), Vec2::splat(1.5)), 0.0, col);
            x += GRID_STEP;
        }
        y += GRID_STEP;
    }
}

/// 管面の辺に沿った目盛り。帯がある辺にだけ打つ(帯が無ければ外に出しようがない)。
fn paint_ticks(p: &egui::Painter, bars: &[Rect; 4], t: Rect, ruler: [Axis; 2]) {
    let [top, bottom, left, right] = *bars;
    let major = Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.55));
    let minor = Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.28));
    let font = egui::FontId::monospace(10.0);
    let text = Color32::from_rgb(0x8A, 0x97, 0x9A);
    // 辺の端に来た数値は管面の外へはみ出さないよう、内側へ寄せる
    let near = |f: f32, lo, mid, hi| if f < 0.03 { lo } else if f > 0.97 { hi } else { mid };
    for n in 0..=TICK_DIV {
        let is_major = n % 2 == 0;
        let (len, stroke) = if is_major { (TICK_MAJOR, major) } else { (TICK_MINOR, minor) };
        // 横の目盛り(上下の辺)。縦長の窓のとき
        if let Some(f) = ruler[0].place(n) {
            let x = t.min.x + t.width() * f;
            if top.height() > TICK_MINOR {
                p.line_segment([Pos2::new(x, t.min.y - len.min(top.height())), Pos2::new(x, t.min.y)], stroke);
                if is_major && top.height() >= TICK_MAJOR + 16.0 {
                    let align = near(f, egui::Align2::LEFT_BOTTOM, egui::Align2::CENTER_BOTTOM,
                                     egui::Align2::RIGHT_BOTTOM);
                    p.text(Pos2::new(x, t.min.y - TICK_MAJOR - 3.0), align,
                           ruler[0].value(n).to_string(), font.clone(), text);
                }
            }
            if bottom.height() > TICK_MINOR {
                p.line_segment([Pos2::new(x, t.max.y), Pos2::new(x, t.max.y + len.min(bottom.height()))], stroke);
            }
        }
        // 縦の目盛り(左右の辺)。横長の窓のとき
        if let Some(f) = ruler[1].place(n) {
            let y = t.min.y + t.height() * f;
            if left.width() > TICK_MINOR {
                p.line_segment([Pos2::new(t.min.x - len.min(left.width()), y), Pos2::new(t.min.x, y)], stroke);
                if is_major && left.width() >= 48.0 {
                    let align = near(f, egui::Align2::RIGHT_TOP, egui::Align2::RIGHT_CENTER,
                                     egui::Align2::RIGHT_BOTTOM);
                    p.text(Pos2::new(t.min.x - TICK_MAJOR - 4.0, y), align,
                           ruler[1].value(n).to_string(), font.clone(), text);
                }
            }
            if right.width() > TICK_MINOR {
                p.line_segment([Pos2::new(t.max.x, y), Pos2::new(t.max.x + len.min(right.width()), y)], stroke);
            }
        }
    }
}

fn paint_corners(p: &egui::Painter, t: Rect) {
    let stroke = Stroke::new(1.5, theme::ACCENT);
    let (arm, gap) = (14.0, 3.0);
    for (c, sx, sy) in [
        (t.left_top(), 1.0, 1.0),
        (t.right_top(), -1.0, 1.0),
        (t.left_bottom(), 1.0, -1.0),
        (t.right_bottom(), -1.0, -1.0),
    ] {
        let k = Pos2::new(c.x - sx * gap, c.y - sy * gap);
        p.line(
            vec![Pos2::new(k.x, c.y + sy * arm), k, Pos2::new(c.x + sx * arm, k.y)],
            stroke,
        );
    }
}

fn paint_info(p: &egui::Painter, area: Rect, hole: Rect, info: &Info) {
    let font = egui::FontId::monospace(INFO_FONT);
    let mut job = egui::text::LayoutJob::default();
    let head = info.mode.clone().unwrap_or_else(|| "NO SIGNAL".into());
    let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
    job.append(&head, 0.0, fmt(theme::ACCENT.gamma_multiply(0.85)));
    for l in &info.lines {
        job.append(&format!("\n{l}"), 0.0, fmt(Color32::from_rgb(0x8A, 0x97, 0x9A)));
    }
    job.wrap.max_width = f32::INFINITY;
    let galley = p.layout_job(job);
    if let Some(slot) = info_slot(area, hole, galley.size()) {
        p.galley(slot.min, galley, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**目盛りは絵の座標(0..最終ドット)に打ち、管面の外は打たない。**
    #[test]
    fn ticks_sit_on_picture_coordinates() {
        // 管面の左端に -100、右端に 900 が来ている。絵は 0..640
        let ax = Axis { a: -100.0, b: 900.0, len: 640.0 };
        assert!((ax.place(0).unwrap() - 0.1).abs() < 1e-6, "0 の目盛りの位置");
        assert!((ax.place(TICK_DIV).unwrap() - 0.74).abs() < 1e-6, "最終ドットの位置");
        assert_eq!(ax.value(4), 160);
        // 管面が絵の内側しか映していなければ、外の目盛りは打たない
        let ax = Axis { a: 100.0, b: 500.0, len: 640.0 };
        assert_eq!(ax.place(0), None);
        assert_eq!(ax.place(TICK_DIV), None);
        assert!(ax.place(4).is_some());
        // 回転で逆向きに並ぶ軸
        let ax = Axis { a: 640.0, b: 0.0, len: 640.0 };
        assert!((ax.place(0).unwrap() - 1.0).abs() < 1e-6);
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
    }

    #[test]
    fn key_roundtrips() {
        for s in Style::ALL {
            assert_eq!(Style::from_key(s.key()), Some(s));
        }
        assert_eq!(Style::from_key("nope"), None);
    }

    /// 帯は穴を除いた残りをちょうど覆い、互いに重ならない。
    #[test]
    fn bars_tile_the_rest() {
        let area = r(0.0, 0.0, 1280.0, 720.0);
        let hole = r(160.0, 0.0, 960.0, 720.0);
        let [top, bottom, left, right] = bars(area, hole);
        assert!(!top.is_positive() && !bottom.is_positive());
        assert_eq!(left, r(0.0, 0.0, 160.0, 720.0));
        assert_eq!(right, r(1120.0, 0.0, 160.0, 720.0));
        let sum: f32 = bars(area, hole).iter().map(|b| b.area().max(0.0)).sum();
        assert_eq!(sum + hole.area(), area.area());
    }

    /// 横長の窓では左の帯に置く(右は操作パネルが重なる)。
    #[test]
    fn info_goes_left_on_wide_window() {
        let area = r(0.0, 0.0, 1280.0, 720.0);
        let hole = r(160.0, 0.0, 960.0, 720.0);
        let slot = info_slot(area, hole, Vec2::new(80.0, 40.0)).expect("入るはず");
        assert!(slot.max.x <= hole.min.x, "左の帯からはみ出した: {slot:?}");
    }

    /// ★**スマホの縦画面のような縦長の窓では、上下の帯に置く。** 左右に帯が無いので、
    ///   左だけ見ていると情報が出ない。
    #[test]
    fn info_goes_below_on_tall_window() {
        let area = r(0.0, 0.0, 390.0, 844.0);
        let tube_h = 390.0 * 3.0 / 4.0;
        let hole = r(0.0, (844.0 - tube_h) / 2.0, 390.0, tube_h);
        let slot = info_slot(area, hole, Vec2::new(300.0, 48.0)).expect("入るはず");
        assert!(slot.min.y >= hole.max.y + TICK_MAJOR, "目盛りに重なる: {slot:?}");
        assert!(slot.max.y <= area.max.y && slot.max.x <= area.max.x);
    }

    /// 帯が細ければ出さない(無理に重ねて管面へ掛けない)。
    #[test]
    fn info_skips_when_bars_are_thin() {
        let area = r(0.0, 0.0, 1000.0, 760.0);
        let hole = r(0.0, 5.0, 1000.0, 750.0);
        assert_eq!(info_slot(area, hole, Vec2::new(200.0, 40.0)), None);
    }
}
