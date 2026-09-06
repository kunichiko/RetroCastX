//! 配信用のクリーン出力ウィンドウ。
//!
//! 本体ウィンドウとは別に、**映像だけ**を描く OS ウィンドウをもう1枚出す。
//! UI もベゼルも枠線も出さず、背景は黒。OBS の「ウィンドウキャプチャ」で
//! これを掴めば、調整 UI を写り込ませずに配信ソースにできる。
//!
//! 本体と同じ `render::Callback`(同じシェーダ)で描くので、見えている絵は
//! 本体ウィンドウと同一。egui のマルチビューポートを使っていて wgpu の
//! デバイスとテクスチャは本体と共有なので、映像の転送は1回で済む。
//!
//! ★**サイズはモード切替で変えない。** 解像度が変わるたびにウィンドウが
//!   伸び縮みすると OBS 側のソースサイズが動いて配信が破綻する。中の映像は
//!   アスペクトを保って収め、余った所は黒で埋める(レターボックス)。
//!
//! ★**immediate viewport を使っている。** deferred だと描画クロージャが
//!   `'static` を要求するので `ViewerApp` を借りられない。immediate は本体の
//!   再描画に同期して描かれるが、本体は映像がある間 8ms 間隔で回している
//!   (main.rs の request_repaint_after 参照)ので、実用上の不足はない。

use eframe::egui;

/// プリセット。**物理画素での目安**を併記する(実際の画素数は表示倍率で変わる)。
pub const PRESETS: &[(&str, [f32; 2])] = &[
    ("1920x1080", [1920.0, 1080.0]),
    ("1280x720", [1280.0, 720.0]),
    ("1024x768", [1024.0, 768.0]),
    ("768x512", [768.0, 512.0]),
    ("640x480", [640.0, 480.0]),
];

pub const DEFAULT_SIZE: [f32; 2] = [1280.0, 720.0];

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("retrocastx-clean-output")
}

/// 映像の表示比を決める。**ベゼルは使わない**(クリーン出力の趣旨なので)。
///
/// 本体の「枠なし」表示と同じ考え方: 切り出しがあればその比、無ければ
/// テクスチャの比。`tube_aspect` が指定されていればそちらが優先される
/// (実機の管面の形はドット数では決まらないため)。
pub fn display_aspect(
    frame_size: (u32, u32),
    crop: [u32; 4],
    tube_aspect: f32,
    rotate: u32,
) -> egui::Vec2 {
    let (cw, ch) = if crop[2] > 0 && crop[3] > 0 {
        (crop[2] as f32, crop[3] as f32)
    } else {
        (frame_size.0 as f32, frame_size.1 as f32)
    };
    let aspect = if tube_aspect > 0.0 {
        tube_aspect
    } else {
        cw / ch.max(1.0)
    };
    if rotate % 2 == 1 {
        egui::vec2(1.0, aspect)
    } else {
        egui::vec2(aspect, 1.0)
    }
}

/// 与えられた領域に、比を保った最大の矩形を中央寄せで置く。
pub fn fit_centered(area: egui::Rect, aspect: egui::Vec2) -> egui::Rect {
    let scale = (area.width() / aspect.x).min(area.height() / aspect.y);
    egui::Rect::from_center_size(area.center(), aspect * scale)
}

/// クリーン出力ウィンドウを1フレーム分描く。
///
/// `paint` には「映像を描くべき矩形」が渡る。本体の `paint_tube` をそのまま
/// 渡せばよい。戻り値が `false` ならユーザーがウィンドウを閉じたので、
/// 呼び出し側は開閉フラグを下ろすこと。
pub fn show(
    ctx: &egui::Context,
    size: [f32; 2],
    resize: bool,
    aspect: egui::Vec2,
    has_video: bool,
    paint: impl FnOnce(&mut egui::Ui, egui::Rect),
) -> bool {
    let builder = egui::ViewportBuilder::default()
        .with_title("RetroCastX 配信出力")
        .with_inner_size(size);

    let mut keep_open = true;
    let mut paint = Some(paint);
    ctx.show_viewport_immediate(viewport_id(), builder, |ctx, _class| {
        // 既に開いているウィンドウにはビルダーの inner_size が効かないので、
        // プリセットを変えたときは明示的にリサイズを送る。
        if resize {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let area = ui.available_rect_before_wrap();
                if !has_video || area.width() < 1.0 || area.height() < 1.0 {
                    return;   // 黒のまま。映像が無いときに枠だけ描いても意味がない
                }
                let rect = fit_centered(area, aspect);
                if let Some(p) = paint.take() {
                    p(ui, rect);
                }
            });
        if ctx.input(|i| i.viewport().close_requested()) {
            keep_open = false;
        }
    });
    keep_open
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横長の領域に 4:3 を置いたら、高さいっぱいで左右が余ること。
    #[test]
    fn fit_letterboxes_on_wide_area() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let r = fit_centered(area, egui::vec2(4.0 / 3.0, 1.0));
        assert!((r.height() - 1080.0).abs() < 0.01, "高さが領域いっぱいでない: {}", r.height());
        assert!((r.width() - 1440.0).abs() < 0.01, "幅が 4:3 になっていない: {}", r.width());
        assert!((r.center() - area.center()).length() < 0.01, "中央寄せされていない");
    }

    /// 縦長の領域なら幅いっぱいで上下が余ること。
    #[test]
    fn fit_pillarboxes_on_tall_area() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 1000.0));
        let r = fit_centered(area, egui::vec2(4.0 / 3.0, 1.0));
        assert!((r.width() - 600.0).abs() < 0.01, "幅が領域いっぱいでない: {}", r.width());
        assert!((r.height() - 450.0).abs() < 0.01, "高さが 4:3 になっていない: {}", r.height());
    }

    /// ★**モード切替で表示比が動いても、置き場所の計算だけで完結すること。**
    ///   ウィンドウサイズは呼び出し側が固定するので、ここが解像度に依存して
    ///   はいけない。768x512 と 640x480 で「同じ領域」に収まるかを見る。
    #[test]
    fn window_area_is_independent_of_source_resolution() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 720.0));
        for (w, h) in [(768u32, 512u32), (640, 480), (320, 240)] {
            let a = display_aspect((w, h), [0; 4], 0.0, 0);
            let r = fit_centered(area, a);
            assert!(area.contains_rect(r), "{}x{} が領域からはみ出した: {r:?}", w, h);
        }
    }

    /// tube_aspect が指定されていればドット数より優先されること。
    /// (X68000 の 768x512 は 1.5 だが、管面は 4:3 で見せたい場合がある)
    #[test]
    fn tube_aspect_overrides_pixel_ratio() {
        let by_pixels = display_aspect((768, 512), [0; 4], 0.0, 0);
        assert!((by_pixels.x - 1.5).abs() < 0.001, "{by_pixels:?}");
        let forced = display_aspect((768, 512), [0; 4], 4.0 / 3.0, 0);
        assert!((forced.x - 4.0 / 3.0).abs() < 0.001, "{forced:?}");
    }

    /// 切り出しが有効なら、切り出し後の比で決まること。
    #[test]
    fn crop_decides_the_ratio() {
        let a = display_aspect((768, 512), [0, 0, 640, 480], 0.0, 0);
        assert!((a.x - 640.0 / 480.0).abs() < 0.001, "{a:?}");
    }

    /// 90度回転では縦横が入れ替わること。
    #[test]
    fn rotation_swaps_the_ratio() {
        let a = display_aspect((640, 480), [0; 4], 0.0, 1);
        assert!((a.x - 1.0).abs() < 0.001 && (a.y - 640.0 / 480.0).abs() < 0.001, "{a:?}");
    }
}
