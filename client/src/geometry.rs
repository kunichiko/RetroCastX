//! 管面の幾何(シェーダへ渡す Params と、画面上の管面の縦横比)。
//!
//! ★**ウィンドウ表示と低遅延全画面で同じものを使う。** 以前は全画面
//!   (fullscreen.rs)が自前で簡略版を作っていて、プログレッシブで vtotal を
//!   2倍しない(縦が2倍に伸びる)、vbp の単位が違う、帯域ごとのモニタ設定
//!   (H幅/V幅)や管面の縦横比を使わない、テクスチャの画素数で枠を決める、と
//!   ずれていた。パネルから全画面に切り替えたら縦横比が崩れた(2026-10-13)。

use crate::protocol::Mode;
use crate::render;

/// MODE の pixfmt のうちコンポジット/S端子(YC8)。これのときは管面を
/// 帯域と別に持つ(NTSC の窓は規格で決まるため)
pub const MON_KEY_NTSC: u32 = 1000;

/// 15/24/31kHz の帯域番号
pub fn band_of(fh_khz: f32) -> u32 {
    if fh_khz < 20.0 {
        15
    } else if fh_khz < 28.0 {
        24
    } else if fh_khz < 40.0 {
        31
    } else {
        fh_khz.round() as u32
    }
}

/// NTSC の1ライン[µs]と1フレームのライン数(管面を時間で決めるとき)
pub const NTSC_LINE_US: f32 = 63.556;
pub const NTSC_LINES: f32 = 525.0;

/// コンポジット/S端子の管面の初期値 [H幅, H位置, V幅, V位置]。
///
/// ★**ビデオデッキの HDMI 出力と同じ範囲を映す**(2026-10-09 実測、白黒の格子
///   パターン。中央の点と格子を目印に、こちらの受信データと同じ物差しで比べた)。
///     横: 9.95〜60.87µs(= ntsc::ACTIVE_US)。中心 35.41µs が中央の点に一致
///     縦: 480ライン。フレームの 35.6〜515.6 行目(中央の点は 272.5 行目)
///   以前は15kHz帯の管面(RGB用)を共用していて、帰線区間の黒い余白まで映り、
///   中心も右へずれていた。
///   位置は「周期の中心からのずれ」で、絵を動かす向きに符号を取る(render.rs)。
pub const MON_NTSC: [f32; 4] = {
    let (h0, h1) = crate::ntsc::ACTIVE_US;
    let (v0, v1) = (35.6, 515.6);
    [
        (h1 - h0) / NTSC_LINE_US,
        0.5 - (h0 + h1) * 0.5 / NTSC_LINE_US,
        (v1 - v0) / NTSC_LINES,
        0.5 - (v0 + v1) * 0.5 / NTSC_LINES,
    ]
};

/// コンポジット/S端子の管面の縦横比。初期の窓(`MON_NTSC`)を規格の画素比
/// (720サンプル×480ライン = 4:3、有効 53.333µs)で映した形で、約 1.274
/// (ビデオデッキと同じ)。
///
/// ★**いまの H幅/V幅からは求めない。** 以前は映している窓から毎回求めていた
///   ので、H幅を 1.1 倍にすると管面の横幅も 1.1 倍になり、1µs あたりの幅が
///   変わらず**絵が動かなかった**(管面の幅だけが動く)。実機の振幅つまみは
///   ガラスの形を変えずに絵を伸び縮みさせるものなので、形は初期の窓で固定し、
///   つまみは RGB と同じく絵の大きさに効かせる。
pub const NTSC_TUBE_ASPECT: f32 = {
    let h_us = MON_NTSC[0] * NTSC_LINE_US;
    let lines = MON_NTSC[2] * NTSC_LINES;
    4.0 / 3.0 * (h_us / 53.333) * (480.0 / lines)
};

/// 管面の縦横比。NTSC を時間ベースで出すときは、規格で決まる形に固定する
pub fn tube_aspect_eff(tube_aspect: f32, ntsc: bool, time_based: bool) -> f32 {
    if ntsc && time_based { NTSC_TUBE_ASPECT } else { tube_aspect }
}

/// 利用者の設定のうち幾何に効くもの
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub rotate: u32,
    /// tube_aspect_eff を通した後の値(0 なら時間窓の比そのまま)
    pub tube: f32,
    pub filter: u32,
    pub window: [f32; 4],
    pub time_based: bool,
    /// いまの帯域のモニタ設定 [H幅, H位置, V幅, V位置]
    pub mon: [f32; 4],
    /// ボードの vbp(ライン単位)
    pub vbp: u32,
}

/// 1ラインが何スロットを占めるか。インターレースは2フィールドが交互に
/// 入るので1、プログレッシブは2(MODE の mflags bit0)
pub fn slot_k(m: Option<&Mode>) -> u32 {
    match m {
        Some(m) if m.mflags & 0x0001 != 0 => 1,
        _ => 2,
    }
}

/// vtotal の平滑。262/263 の交互は正常なので平均を使う。モードが変わったら即座に飛ばす
#[derive(Default)]
pub struct VtotalSmoother {
    value: f32,
    mode_id: Option<u16>,
    last: Option<std::time::Instant>,
}

impl VtotalSmoother {
    pub fn update(&mut self, m: Option<&Mode>) -> f32 {
        let now = std::time::Instant::now();
        let dt = self.last.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32());
        self.last = Some(now);
        let raw = m.map_or(0.0, |m| (m.vtotal as u32 * slot_k(Some(m))) as f32);
        let id = m.map(|m| m.mode_id as u16);
        let reset = id != self.mode_id;
        self.value = render::smooth_vtotal(self.value, raw, reset, dt);
        if raw > 0.0 {
            self.mode_id = id;
        }
        self.value
    }
}

/// シェーダへ渡す Params を作る。`act` は実測した絵の外接矩形 (x, y, w, h)。
pub fn params(m: Option<&Mode>, vtotal_smooth: f32, act: (u32, u32, u32, u32), i: &Inputs)
    -> render::Params
{
    render::Params {
        rotate: i.rotate,
        tube: i.tube,
        filter: i.filter,
        htotal: m.map_or(0, |m| m.htotal as u32),
        // 行位置は半ライン単位のスロット。1VSYNC周期に何スロット入るかは
        // 織り込みの有無で変わる(slot_k)。間違えると縦が2倍に引き伸ばされる。
        vtotal: vtotal_smooth,
        // offset_px はライン内の絶対位置で来るので、ここでずらす必要はない
        hs_offset: 0,
        vbp: i.vbp * slot_k(m),
        window: i.window,
        fh_hz: m.map_or(0, |m| (m.hfreq_mhz_x1000 / 1000) as u32),
        hactive: m.map_or(0, |m| m.hactive as u32),
        vactive: m.map_or(0, |m| m.vactive as u32),
        time_based: i.time_based,
        h_size: i.mon[0],
        h_pos: i.mon[1],
        v_size: i.mon[2],
        v_pos: i.mon[3],
        act_x: act.0, act_y: act.1, act_w: act.2, act_h: act.3,
    }
}

/// 「画に合わせる」: 実測した絵の外接矩形(`act_*`)がちょうど管面に収まる
/// [H幅, H位置, V幅, V位置]。測れていなければ None。
///
/// サンプル x はラインの (hs_offset + x)/htotal、行 y は周期の (vbp + y)/vtotal に
/// ある(render.rs の uniforms と同じ対応)ので、矩形の両端を周期に対する割合へ直し、
/// その幅と中心をつまみの値にする。位置は「周期の中心からのずれ」で、絵を動かす
/// 向きに符号を取る(render.rs の effective_window)。
///
/// ★**押したときに一度だけ。** 絵に追従させると幾何が内容に依存し、暗い場面で
///   管面が縮む。実CRTのつまみと同じく、合わせた値は帯域ごとに残す。
pub fn fit_mon(p: &render::Params) -> Option<[f32; 4]> {
    // 小さすぎる矩形は絵の一部(暗い場面の明るい塊)なので信用しない
    if p.htotal == 0 || p.vtotal <= 0.0 || p.act_w < 64 || p.act_h < 16 {
        return None;
    }
    let (ht, vt) = (p.htotal as f32, p.vtotal);
    let x0 = (p.hs_offset + p.act_x) as f32 / ht;
    let x1 = (p.hs_offset + p.act_x + p.act_w) as f32 / ht;
    let y0 = (p.vbp + p.act_y) as f32 / vt;
    let y1 = (p.vbp + p.act_y + p.act_h) as f32 / vt;
    Some([x1 - x0, 0.5 - (x0 + x1) * 0.5, y1 - y0, 0.5 - (y0 + y1) * 0.5])
}

/// 管面の縦横比(幅/高さ。回転込み)。`frame` は切り出し後の画素数
pub fn display_aspect(frame: (f32, f32), tube: f32, rotate: u32) -> (f32, f32) {
    let aspect = if tube > 0.0 { tube } else { frame.0 / frame.1.max(1.0) };
    if rotate % 2 == 1 { (1.0, aspect) } else { (aspect, 1.0) }
}

/// 領域に比を保った最大の矩形を中央寄せで置く → (x, y, w, h)
pub fn fit_centered(area_w: f32, area_h: f32, aspect: (f32, f32)) -> (f32, f32, f32, f32) {
    let scale = (area_w / aspect.0).min(area_h / aspect.1);
    let (w, h) = (aspect.0 * scale, aspect.1 * scale);
    ((area_w - w) * 0.5, (area_h - h) * 0.5, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★**合わせた後、絵の外接矩形の四辺が管面の四辺にちょうど来ること。**
    ///   描画と同じ effective_window で答え合わせする(式を写しただけの試験に
    ///   しない)。vbp を入れて、縦の起点のずれも確かめる。
    #[test]
    fn fit_mon_puts_picture_edges_on_tube_edges() {
        let mut p = render::Params {
            htotal: 1104, vtotal: 1136.0, vbp: 70, time_based: true,
            act_x: 210, act_y: 12, act_w: 768, act_h: 1024,
            h_size: 1.0, v_size: 1.0,
            ..Default::default()
        };
        let m = fit_mon(&p).expect("測れているのに合わせない");
        (p.h_size, p.h_pos, p.v_size, p.v_pos) = (m[0], m[1], m[2], m[3]);
        let w = p.effective_window();
        let (ht, vt) = (1104.0, 1136.0);
        let left = w[0] * ht - p.hs_offset as f32;
        let right = w[1] * ht - p.hs_offset as f32;
        let top = w[2] * vt - p.vbp as f32;
        let bottom = w[3] * vt - p.vbp as f32;
        assert!((left - 210.0).abs() < 0.01 && (right - 978.0).abs() < 0.01,
                "横 {left}..{right}");
        assert!((top - 12.0).abs() < 0.01 && (bottom - 1036.0).abs() < 0.01,
                "縦 {top}..{bottom}");
    }

    /// 測れていない(モード未受信・暗い場面の小さな塊)なら動かさない。
    #[test]
    fn fit_mon_refuses_without_measurement() {
        let p = render::Params {
            htotal: 1104, vtotal: 1136.0,
            act_x: 500, act_y: 500, act_w: 20, act_h: 8,
            ..Default::default()
        };
        assert_eq!(fit_mon(&p), None);
        let p = render::Params { act_w: 768, act_h: 1024, ..Default::default() };
        assert_eq!(fit_mon(&p), None);
    }

    fn mode(mflags: u16, vtotal: u16) -> Mode {
        Mode {
            mode_id: 1, pixfmt: 0, mflags, hactive: 2048, htotal: 1104,
            vactive: 1136, vtotal, dotclk_hz: 34_775_400,
            hfreq_mhz_x1000: 31_500_000, vfreq_mhz_x1000: 55_500, seq: 0,
        }
    }

    /// ★**プログレッシブは vtotal と vbp を2倍(半ラインのスロット単位)。**
    ///   全画面が自前で作っていたときはここが1倍で、縦が崩れた
    #[test]
    fn progressive_doubles_slots() {
        let i = Inputs { rotate: 0, tube: 4.0 / 3.0, filter: 0, window: [0.0; 4],
                         time_based: false, mon: [1.0, 0.0, 1.0, 0.0], vbp: 30 };
        let p = mode(0, 568);
        assert_eq!(slot_k(Some(&p)), 2);
        assert_eq!(params(Some(&p), 1136.0, (0, 0, 0, 0), &i).vbp, 60);
        let il = mode(1, 525);
        assert_eq!(slot_k(Some(&il)), 1);
        assert_eq!(params(Some(&il), 525.0, (0, 0, 0, 0), &i).vbp, 30);
        let mut s = VtotalSmoother::default();
        assert_eq!(s.update(Some(&p)), 1136.0);
    }

    /// コンポジットの管面の形は、初期の窓をビデオデッキと同じ 1.274 で映す形。
    #[test]
    fn ntsc_tube_aspect_matches_vcr() {
        assert!((NTSC_TUBE_ASPECT - 1.274).abs() < 0.005, "縦横比 {NTSC_TUBE_ASPECT}");
        assert_eq!(tube_aspect_eff(4.0 / 3.0, true, true), NTSC_TUBE_ASPECT);
        // 時間ベースでなければ、RGB と同じく管面の設定に従う
        assert_eq!(tube_aspect_eff(4.0 / 3.0, true, false), 4.0 / 3.0);
        assert_eq!(tube_aspect_eff(4.0 / 3.0, false, true), 4.0 / 3.0);
    }

    /// ★**H幅/V幅は振幅つまみとして絵の大きさに効くこと。** 管面の形は固定で、
    ///   窓(映す時間)だけが変わる。1µs あたりの管面上の幅は窓に反比例する。
    ///   以前は管面の形も窓と一緒に変わり、この比が 1 のままで絵が動かなかった。
    #[test]
    fn ntsc_amplitude_scales_the_picture() {
        let tube_w = |h_size: f32| {
            let a = display_aspect((720.0, 480.0), tube_aspect_eff(0.0, true, true), 0);
            let (_, _, w, _) = fit_centered(1280.0, 960.0, a);
            // 管面の幅 ÷ 映している時間 = 1µs あたりの幅
            w / (h_size * NTSC_LINE_US)
        };
        let base = tube_w(MON_NTSC[0]);
        let shrunk = tube_w(MON_NTSC[0] * 1.1);
        assert!((shrunk / base - 1.0 / 1.1).abs() < 1e-4,
                "H幅を広げても絵が縮まない: {base} → {shrunk}");
    }

    /// 管面は画面いっぱいに比を保って置く(テクスチャの画素数では決めない)
    #[test]
    fn fullscreen_tube_rect_keeps_aspect() {
        let a = display_aspect((1104.0, 1136.0), 4.0 / 3.0, 0);
        let (x, y, w, h) = fit_centered(1920.0, 1080.0, a);
        assert!((w / h - 4.0 / 3.0).abs() < 1e-3);
        assert_eq!((y, h), (0.0, 1080.0));
        assert!((x - 240.0).abs() < 0.5);
        // 縦画面(回転)
        let a = display_aspect((1104.0, 1136.0), 4.0 / 3.0, 1);
        let (_, _, w, h) = fit_centered(1920.0, 1080.0, a);
        assert!((h / w - 4.0 / 3.0).abs() < 1e-3);
    }
}
