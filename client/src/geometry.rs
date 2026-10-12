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

/// 管面の縦横比。NTSC を時間ベースで出すときは、見せている時間窓から求める
pub fn tube_aspect_eff(tube_aspect: f32, ntsc: bool, time_based: bool, mon: [f32; 4]) -> f32 {
    if !ntsc || !time_based {
        return tube_aspect;
    }
    let h_us = mon[0].max(0.01) * NTSC_LINE_US;
    let lines = mon[2].max(0.01) * NTSC_LINES;
    4.0 / 3.0 * (h_us / 53.333) * (480.0 / lines)
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
