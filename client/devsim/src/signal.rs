//! 機種の映像信号を、TVP7002 がサンプルした値として作る。
//!
//! TVP は1ラインを `pll_divide` 個のサンプルに切る(H-PLL の帰還分周比)。
//! サンプル i の時刻は HSYNC の立ち上がりから `(i + 位相/32) / pll_divide` ライン。
//! これを機種のドット位置に直すと
//!
//! ```text
//!     d = (i + phase/32) × htotal / pll_divide + 遅延
//! ```
//!
//! になる。pll_divide が機種の htotal と合っていなければ絵が伸び縮みし、
//! 1:1 でも位相がドットの境目に掛かれば隣のドットと混ざる。**Viewer の PLL と
//! 位相の自動調整は、実機と同じ理屈でここに反応する。**
//!
//! ドットの境目は一瞬では切り替わらない(ケーブルとアンプの帯域)。立ち上がりに
//! `RISE` ドットかかるとして直線でつなぐ。遅延は固定の ns で持つので、最適な位相は
//! ドットクロックごとに違う値になる(実機もそう)。

use crate::machines::{Kind, Scan, Timing};
use crate::pattern::Picture;

/// ドットの立ち上がりにかかる時間[ドット]
const RISE: f64 = 0.35;
/// 信号の遅延[秒]。最適な位相を既定の 16 からずらすため(実機でもずれている)
const DELAY_S: f64 = 13e-9;

/// NTSC 色副搬送波
pub const FSC: f64 = 3_579_545.0;

// コンポジットの符号値(緑ch、ミッドレベルクランプ + 粗ゲイン0.5倍)。
// host/python/retrocastx/tests/test_ntsc.py の合成と同じ値(実機相当)
const CV_BLANK: f64 = 158.0;
const CV_CPI: f64 = 0.78;
// S端子の Y(ボトムクランプで同期チップを 16 に、1 IRE=1.45コード)と C(ミッド)。
// client/src/profiles.rs の REGS_SVIDEO のコメントの実測値
const SV_TIP: f64 = 16.0;
const SV_YPI: f64 = 1.45;
const SV_CPI: f64 = 2.21;

/// 1台の機種の信号
#[derive(Clone, Debug)]
pub struct Source {
    pub kind: Kind,
    pub timing: Timing,
    pub color_bits: u32,
    /// デジタルRGB の色の表(`Mode::palette`)
    pub palette: Option<crate::palette::Palette>,
}

/// 1ラインが映像のどこに当たるか
#[derive(Clone, Copy, Debug)]
pub struct LineRef {
    /// VSYNC(各フィールドの)から数えたライン
    pub field_line: u32,
    /// インターレースのどちらのフィールドか(0=上 / 1=半ライン下)
    pub field: u32,
    /// 通しのライン番号の偶奇(NTSC の副搬送波はラインごとに 180° 回る)
    pub abs_odd: bool,
}

impl Source {
    /// その行が絵の何行目か。None はブランキング
    fn picture_row(&self, l: LineRef, pic_h: u32) -> Option<u32> {
        let t = &self.timing;
        let fields = if t.scan == Scan::Progressive { 1 } else { 2 };
        let field_active = t.vactive / fields;
        if l.field_line < t.vstart || l.field_line >= t.vstart + field_active {
            return None;
        }
        let y = (l.field_line - t.vstart) * fields + l.field.min(fields - 1);
        let row = y / t.line_repeat.max(1);
        (row < pic_h).then_some(row)
    }

    fn in_vsync(&self, l: LineRef) -> bool {
        l.field_line < self.timing.vsync
    }
}

/// TVP の取り込み設定(Viewer が CONFIG で書く値)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tvp {
    pub pll_divide: u32,
    /// 0..31
    pub phase: u32,
}

/// 1ラインをサンプルする。`out` の長さがサンプル数(普通は pll_divide、
/// ラインバッファの幅で切る)。値は 8bit の生値 [R, G, B]
pub fn sample_line(
    src: &Source, pic: &Picture, l: LineRef, tvp: Tvp, noise: u8, seed: &mut u32,
    out: &mut [[u8; 3]],
) {
    let t = &src.timing;
    let pll = tvp.pll_divide.max(1) as f64;
    let dots_per_sample = t.htotal as f64 / pll;
    let d0 = tvp.phase as f64 / 32.0 * dots_per_sample + DELAY_S * t.dotclk_hz;
    let row = src.picture_row(l, pic.h);
    let vsync = src.in_vsync(l);
    match src.kind {
        Kind::Rgb => {
            for (i, o) in out.iter_mut().enumerate() {
                let d = i as f64 * dots_per_sample + d0;
                *o = match row {
                    Some(r) => rgb_at(t, pic, r, d),
                    None => [0; 3],
                };
            }
        }
        Kind::Composite | Kind::SVideo | Kind::Component => {
            let line_s = t.htotal as f64 / t.dotclk_hz;
            let act0 = t.hstart as f64 / t.dotclk_hz;
            let act_w = t.hactive as f64 / t.dotclk_hz;
            let sync_s = t.hsync as f64 / t.dotclk_hz;
            let flip = if l.abs_odd { std::f64::consts::PI } else { 0.0 };
            for (i, o) in out.iter_mut().enumerate() {
                let d = i as f64 * dots_per_sample + d0;
                let ts = d / t.dotclk_hz; // HSYNC からの時刻[秒]
                // 垂直同期のラインは、ほぼ全部が同期レベル(ブロードパルス)
                let in_sync = if vsync { ts < line_s - sync_s } else { ts < sync_s };
                let rgb = row.and_then(|r| {
                    let x = (ts - act0) / act_w * pic.w as f64;
                    (x >= 0.0 && x < pic.w as f64).then(|| rgb_f(pic, r, x))
                });
                let psi = 2.0 * std::f64::consts::PI * FSC * ts + flip;
                let burst = !vsync && (5.4e-6..7.7e-6).contains(&ts);
                *o = analog(src.kind, in_sync, burst, rgb, psi);
            }
        }
    }
    if noise > 0 {
        for o in out.iter_mut() {
            for v in o.iter_mut() {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                let n = (*seed % (2 * noise as u32 + 1)) as i32 - noise as i32;
                *v = (*v as i32 + n).clamp(0, 255) as u8;
            }
        }
    }
}

/// ドット位置 d [ドット、HSYNC から] の RGB の値
#[inline]
fn rgb_at(t: &Timing, pic: &Picture, row: u32, d: f64) -> [u8; 3] {
    let x = d - t.hstart as f64;
    if x < -1.0 || x >= pic.w as f64 + 1.0 {
        return [0; 3];
    }
    let c = rgb_f(pic, row, x);
    [c[0] as u8, c[1] as u8, c[2] as u8]
}

/// 絵の横位置 x(小数)の値。ドットの頭の RISE の間だけ前のドットから直線で移る
#[inline]
fn rgb_f(pic: &Picture, row: u32, x: f64) -> [f64; 3] {
    let k = x.floor();
    let f = x - k;
    let px = |k: f64| -> [f64; 3] {
        if k < 0.0 || k >= pic.w as f64 {
            [0.0; 3]
        } else {
            let c = pic.get(k as u32, row);
            [c[0] as f64, c[1] as f64, c[2] as f64]
        }
    };
    let cur = px(k);
    if f >= RISE {
        return cur;
    }
    let prev = px(k - 1.0);
    let w = f / RISE;
    [
        prev[0] + (cur[0] - prev[0]) * w,
        prev[1] + (cur[1] - prev[1]) * w,
        prev[2] + (cur[2] - prev[2]) * w,
    ]
}

/// コンポジット/S端子/コンポーネントの1サンプル
fn analog(kind: Kind, sync: bool, burst: bool, rgb: Option<[f64; 3]>, psi: f64) -> [u8; 3] {
    let [r, g, b] = rgb.map_or([0.0; 3], |c| [c[0] / 255.0, c[1] / 255.0, c[2] / 255.0]);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    match kind {
        Kind::Composite | Kind::SVideo => {
            // ntsc.decode の逆: U = -2C·cos(ψ), V = +2C·sin(ψ)
            let u = 0.493 * (b - y);
            let v = 0.877 * (r - y);
            let c = (-u * psi.cos() + v * psi.sin()) / 2.0;
            // [IRE]
            let (luma, chroma) = if sync {
                (-40.0, 0.0)
            } else if burst {
                (0.0, 20.0 * psi.cos())
            } else {
                (y * 100.0, c * 100.0)
            };
            if kind == Kind::Composite {
                [2, q(CV_BLANK + (luma + chroma) * CV_CPI), 0]
            } else {
                let yy = SV_TIP + (luma + 40.0) * SV_YPI;
                let cc = 128.0 + chroma * SV_CPI;
                [q(cc), q(yy), 0]
            }
        }
        _ => {
            // コンポーネント: Y は黒 16〜白 235(同期は 0 まで落ちて切れる)、
            // 色差はミッドレベル 128 ± 112
            if sync {
                return [128, 0, 128];
            }
            let pb = 0.564 * (b - y);
            let pr = 0.713 * (r - y);
            [q(128.0 + pr * 224.0), q(16.0 + y * 219.0), q(128.0 + pb * 224.0)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::machines;
    use crate::pattern::{self, Options, Pattern};

    fn src(key: &str, mode: usize) -> Source {
        let ms = machines();
        let mc = ms.iter().find(|m| m.key == key).unwrap();
        let md = &mc.modes[mode];
        Source { kind: mc.kind, timing: md.timing.clone(), color_bits: md.color_bits, palette: md.palette }
    }

    fn stripes(s: &Source) -> Picture {
        let (w, h) = s.timing.picture_size();
        let o = Options { pattern: Pattern::DotStripes, moving: false, caption: String::new() };
        pattern::render(w, h, s.color_bits, s.palette, &o, 0)
    }

    /// 1:1 で位相が合っていれば縦縞が 0/255 のまま取れる。合っていない位相では
    /// 混ざる。Viewer の位相探索が手掛かりにする「位相感度」がここで出ること
    #[test]
    fn phase_matters_only_at_one_to_one() {
        let s = src("x68000", 0);
        let pic = stripes(&s);
        let l = LineRef { field_line: s.timing.vstart + 10, field: 0, abs_odd: false };
        let contrast = |pll: u32, phase: u32| {
            let mut out = vec![[0u8; 3]; pll as usize];
            let mut seed = 1;
            sample_line(&s, &pic, l, Tvp { pll_divide: pll, phase }, 0, &mut seed, &mut out);
            let a = (s.timing.hstart + 100) as usize * pll as usize / s.timing.htotal as usize;
            let seg = &out[a..a + 64];
            let mx = seg.iter().map(|c| c[1]).max().unwrap() as i32;
            let mn = seg.iter().map(|c| c[1]).min().unwrap() as i32;
            mx - mn
        };
        let best = (0..32).map(|p| contrast(1104, p)).max().unwrap();
        let worst = (0..32).map(|p| contrast(1104, p)).min().unwrap();
        assert!(best >= 250, "best {best}");
        assert!(worst < best, "worst {worst}");
    }

    #[test]
    fn blanking_is_black_and_active_is_where_timing_says() {
        let s = src("x68000", 0);
        let o = Options { pattern: Pattern::White, moving: false, caption: String::new() };
        let (w, h) = s.timing.picture_size();
        let pic = pattern::render(w, h, 5, None, &o, 0);
        let mut out = vec![[0u8; 3]; 1104];
        let mut seed = 1;
        let l = LineRef { field_line: s.timing.vstart, field: 0, abs_odd: false };
        sample_line(&s, &pic, l, Tvp { pll_divide: 1104, phase: 16 }, 0, &mut seed, &mut out);
        let lit: Vec<usize> = (0..1104).filter(|&i| out[i][0] > 128).collect();
        assert_eq!(*lit.first().unwrap(), 264);
        assert_eq!(lit.len(), 768);
        let l = LineRef { field_line: 0, field: 0, abs_odd: false };
        sample_line(&s, &pic, l, Tvp { pll_divide: 1104, phase: 16 }, 0, &mut seed, &mut out);
        assert!(out.iter().all(|c| *c == [0; 3]));
    }

    /// コンポジットの同期チップとバックポーチ(Viewer の ntsc.rs がレベル校正に使う)
    #[test]
    fn composite_levels() {
        let s = src("composite", 0);
        let o = Options { pattern: Pattern::Black, moving: false, caption: String::new() };
        let (w, h) = s.timing.picture_size();
        let pic = pattern::render(w, h, 8, None, &o, 0);
        let mut out = vec![[0u8; 3]; 1820];
        let mut seed = 1;
        let l = LineRef { field_line: 40, field: 0, abs_odd: false };
        sample_line(&s, &pic, l, Tvp { pll_divide: 1820, phase: 0 }, 0, &mut seed, &mut out);
        let tip = (CV_BLANK - 40.0 * CV_CPI).round() as u8;
        assert_eq!(out[50][1], tip);
        assert_eq!(out[1000][1], CV_BLANK as u8);
        // バーストは ±20 IRE 振れる
        let burst: Vec<u8> = out[170..210].iter().map(|c| c[1]).collect();
        let (mx, mn) = (*burst.iter().max().unwrap(), *burst.iter().min().unwrap());
        assert!(mx as f64 > CV_BLANK + 14.0 && (mn as f64) < CV_BLANK - 14.0, "{mn}..{mx}");
    }
}
