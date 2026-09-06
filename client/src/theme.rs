//! RetroCast X の配色。
//!
//! アプリアイコン(`packaging/AppIcon.png`)から起こしている。実測した支配色は
//!
//! ```text
//!   teal    hue 180°  #30a0a0 / #008080   髪と瞳のハイライト、ヘッドホン
//!   orange  hue  20°  #f07030 / #e06020   ヘッドホンの X、差し色
//!   地      #000000 / #101010             背景
//! ```
//!
//! ★**アクセントは teal 1色に絞る。** orange は「注意」だけに使う。両方を
//!   同じ強さで使うと、どちらが操作対象なのか読み取れなくなる。
//!
//! ★**見出しの帯はボタンより濃くする。** 以前は `widgets.inactive.bg_fill`
//!   (= ボタンの灰色)をそのまま帯に使っていたので、押せるものと見分けが
//!   つかなかった。地に近い濃さ + 明るい teal の文字、という逆の組み合わせに
//!   することで「これは操作対象ではなく区切りだ」が一目で分かる。

use eframe::egui::{self, Color32};

/// 主アクセント。見出しの文字、選択、リンク。
pub const ACCENT: Color32 = Color32::from_rgb(0x45, 0xD9, 0xD3);
/// 見出しの帯の地。ボタンの灰色(38,40,44 付近)より確実に濃く、かつ teal 寄り。
pub const ACCENT_BG: Color32 = Color32::from_rgb(0x0C, 0x2B, 0x2D);
/// 帯の左端に入れる縦線。面だけでなく線でも段差を作る。
pub const ACCENT_EDGE: Color32 = Color32::from_rgb(0x2E, 0xB5, 0xB0);
/// 副アクセント。警告と「注意して見るべき値」だけに使う。
pub const AMBER: Color32 = Color32::from_rgb(0xF0, 0x82, 0x38);
/// 良好・正常を示す色。
pub const OK: Color32 = Color32::from_rgb(0x4C, 0xD9, 0x8A);

/// パネルの地。映像が主役なので、UI 側は黒に寄せて沈める。
const PANEL_BG: Color32 = Color32::from_rgb(0x0F, 0x12, 0x14);
const INPUT_BG: Color32 = Color32::from_rgb(0x07, 0x09, 0x0A);

/// 起動時に一度だけ適用する。
pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL_BG;
    v.window_fill = PANEL_BG;
    v.extreme_bg_color = INPUT_BG;
    // 選択・フォーカスを teal に寄せて、既定の青を消す
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    // 触れるものだけが teal で反応する。押せないものは灰色のまま
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT_EDGE);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    ctx.set_visuals(v);
}

/// パネルのセクション見出しを1本描く。
///
/// ★**太字にしても効かない。** egui の既定フォントには太字の字形が無く、
///   `RichText::strong()` は色が少し明るくなるだけ。本文も monospace で字面が
///   近いので、大きさを変えただけでは段差にならなかった。面(帯)と線(左の
///   縦棒)と色(teal)の3つで区切る。
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 20.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 3.0, ACCENT_BG);
    let edge = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    p.rect_filled(edge, 1.0, ACCENT_EDGE);
    p.text(
        rect.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.0),
        ACCENT,
    );
    ui.add_space(4.0);
}

/// ラベルと値を桁揃えして1行出す。
///
/// ★**monospace の空白詰めで揃える。** `ui.horizontal` で2つのラベルを並べると
///   項目間の spacing が入るうえ幅も文字数で決まるので、行ごとに値の開始位置が
///   ずれる。Mode / Stats はほぼ全部が「名前 数値 単位」なので、そのずれが
///   いちばん目に付く。1つの monospace ラベルにまとめれば確実に揃う。
pub fn kv(ui: &mut egui::Ui, key: &str, val: impl AsRef<str>) {
    ui.monospace(format!("{key:<7}{}", val.as_ref()));
}

/// スライダー行のラベル列の幅[pt]。
///
/// ★**文字数では揃わない。** monospace でも CJK はシステムフォントの
///   フォールバックで、ASCII のちょうど2倍幅とは限らない。実際「彩度」と
///   「コントラスト」で `format!("{label:<6}")` を使っていたが、スライダーの
///   開始位置がずれていた。文字数ではなく**領域の幅**で揃える。
pub const LABEL_W: f32 = 78.0;
/// スライダーの軌道の幅[pt]。ここを揃えないと右端の数値欄がずれる。
pub const SLIDER_W: f32 = 108.0;

/// 固定幅のラベル列を置く。`ui.horizontal` の先頭で呼ぶ。
pub fn label_col(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(LABEL_W, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // 中身が短くても列幅を保つ(これが無いと内容幅まで縮む)
            ui.set_min_width(LABEL_W);
            ui.monospace(text);
        },
    );
}

/// スライダーの軌道幅を揃える。行を並べるスコープの先頭で1回呼ぶ。
pub fn align_sliders(ui: &mut egui::Ui) {
    ui.spacing_mut().slider_width = SLIDER_W;
}
