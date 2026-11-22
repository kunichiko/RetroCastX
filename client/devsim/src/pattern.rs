//! 機種の画面に出す絵(テストパターン)。
//!
//! 絵は機種の有効画素の大きさ(`Timing::picture_size`)で作り、色は機種の
//! 階調(`Mode::color_bits`)に丸める。Viewer の確かめたい所ごとに柄を変えられる:
//!
//! - 縦縞(1ドット): pll_divide と位相が合っているか(合っていないとモアレ/ぼけ)
//! - 階調: 32段などの階調が潰れていないか(パレット吸着の確認)
//! - 枠と格子: 有効映像の位置と大きさの実測(act_x/act_y/act_w/act_h)
//! - 動く物: フレームの欠け・ティアリング・インターレースの織り込み

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// カラーバー + 階調 + 縦縞 + 枠を1枚にまとめたもの
    Composite,
    ColorBars,
    /// 32段の階調(R/G/B/白)
    GrayRamp,
    /// 1ドットおきの縦縞(PLL と位相の確認)
    DotStripes,
    /// 1ドットの市松(インターレースの織り込みの確認)
    Checker,
    /// 格子と枠(有効映像の位置と大きさ)
    Grid,
    /// 真っ黒(信号はあるが絵が無い。範囲判定の確認)
    Black,
    /// 白一色
    White,
}

impl Pattern {
    pub const ALL: [Pattern; 8] = [
        Pattern::Composite, Pattern::ColorBars, Pattern::GrayRamp, Pattern::DotStripes,
        Pattern::Checker, Pattern::Grid, Pattern::Black, Pattern::White,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Pattern::Composite => "総合",
            Pattern::ColorBars => "カラーバー",
            Pattern::GrayRamp => "階調",
            Pattern::DotStripes => "1ドット縦縞",
            Pattern::Checker => "1ドット市松",
            Pattern::Grid => "格子と枠",
            Pattern::Black => "黒",
            Pattern::White => "白",
        }
    }
}

/// 絵。RGB888、行優先
#[derive(Clone)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<[u8; 3]>,
}

impl Picture {
    pub fn new(w: u32, h: u32) -> Picture {
        Picture { w, h, rgb: vec![[0; 3]; (w * h) as usize] }
    }

    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 3] {
        self.rgb[(y * self.w + x) as usize]
    }

    #[inline]
    fn set(&mut self, x: u32, y: u32, c: [u8; 3]) {
        if x < self.w && y < self.h {
            self.rgb[(y * self.w + x) as usize] = c;
        }
    }

    fn fill(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 3]) {
        for y in y0..y1.min(self.h) {
            for x in x0..x1.min(self.w) {
                self.rgb[(y * self.w + x) as usize] = c;
            }
        }
    }
}

/// 絵の付け足し
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub pattern: Pattern,
    /// 動く箱を重ねる(フレームの欠けや織り込みを見る)
    pub moving: bool,
    /// 左上に機種とモードの名前を書く
    pub caption: String,
}

const BARS: [[u8; 3]; 8] = [
    [255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0],
    [255, 0, 255], [255, 0, 0], [0, 0, 255], [0, 0, 0],
];

/// 階調を `bits` ビットに丸める(上位ビットの複製で 8bit に戻す)
fn quantize(v: u8, bits: u32) -> u8 {
    if bits >= 8 {
        return v;
    }
    let levels = (1u32 << bits) - 1;
    let q = (v as u32 * levels + 127) / 255;
    (q * 255 / levels) as u8
}

/// 絵を作る。`frame` は動く物の位置に使う
pub fn render(w: u32, h: u32, color_bits: u32, opt: &Options, frame: u32) -> Picture {
    let mut p = Picture::new(w.max(1), h.max(1));
    match opt.pattern {
        Pattern::Composite => composite(&mut p, color_bits),
        Pattern::ColorBars => bars(&mut p, 0, h),
        Pattern::GrayRamp => ramp(&mut p, 0, h, color_bits),
        Pattern::DotStripes => stripes(&mut p, 0, h),
        Pattern::Checker => {
            for y in 0..h {
                for x in 0..w {
                    let c = if (x + y) % 2 == 0 { 255 } else { 0 };
                    p.set(x, y, [c, c, c]);
                }
            }
        }
        Pattern::Grid => grid(&mut p),
        Pattern::Black => {}
        Pattern::White => p.fill(0, 0, w, h, [255, 255, 255]),
    }
    if opt.moving {
        let s = (h / 8).max(4);
        let span_x = w.saturating_sub(s).max(1);
        let span_y = h.saturating_sub(s).max(1);
        // 三角波で往復(端で止まって見えないよう x と y で周期を変える)
        let tri = |f: u32, span: u32| {
            let p = f % (2 * span);
            if p < span { p } else { 2 * span - p }
        };
        let x0 = tri(frame * 3, span_x);
        let y0 = tri(frame * 2, span_y);
        p.fill(x0, y0, x0 + s, y0 + s, [255, 128, 0]);
    }
    if !opt.caption.is_empty() {
        // ★**暗い下地の帯に白で書く。** 下の柄(白いカラーバーなど)にそのまま白で
        //   書くと読めない。帯は黒ではなく濃紺にして、絵の黒い部分とも区別できるようにする
        let scale = if w >= 600 { 2 } else { 1 };
        let n = opt.caption.chars().filter(|c| c.is_ascii()).count() as u32;
        let (x0, y0, pad) = (8, 8, 2 * scale);
        let bw = (n * 6 * scale).min(w.saturating_sub(x0 + 6 * scale));
        p.fill(x0 - pad, y0 - pad, x0 + bw + pad, y0 + 7 * scale + pad, [16, 24, 64]);
        text(&mut p, x0, y0, &opt.caption, scale, [255, 255, 255]);
    }
    for c in p.rgb.iter_mut() {
        for v in c.iter_mut() {
            *v = quantize(*v, color_bits);
        }
    }
    p
}

fn bars(p: &mut Picture, y0: u32, y1: u32) {
    for x in 0..p.w {
        let c = BARS[(x * 8 / p.w) as usize];
        for y in y0..y1 {
            p.set(x, y, c);
        }
    }
}

fn ramp(p: &mut Picture, y0: u32, y1: u32, bits: u32) {
    // 機種の段数(最大32段)に合わせて段を切る
    let steps = 1u32 << bits.min(5);
    let rows = [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 1]];
    let band = ((y1 - y0) / 4).max(1);
    for (i, k) in rows.iter().enumerate() {
        let ya = y0 + i as u32 * band;
        let yb = if i == 3 { y1 } else { ya + band };
        for x in 0..p.w {
            let s = x * steps / p.w;
            let v = (s * 255 / (steps - 1)) as u8;
            let c = [v * k[0], v * k[1], v * k[2]];
            for y in ya..yb {
                p.set(x, y, c);
            }
        }
    }
}

fn stripes(p: &mut Picture, y0: u32, y1: u32) {
    for y in y0..y1 {
        for x in 0..p.w {
            let c = if x % 2 == 0 { 255 } else { 0 };
            p.set(x, y, [c, c, c]);
        }
    }
}

fn grid(p: &mut Picture) {
    let (w, h) = (p.w, p.h);
    let step = (w.min(h) / 8).max(8);
    for y in 0..h {
        for x in 0..w {
            let on = x % step == 0 || y % step == 0;
            if on {
                p.set(x, y, [0, 160, 255]);
            }
        }
    }
    // 中心の十字
    p.fill(w / 2, 0, w / 2 + 1, h, [255, 255, 0]);
    p.fill(0, h / 2, w, h / 2 + 1, [255, 255, 0]);
    border(p);
}

/// 1ドットの白枠。有効映像の外接矩形がちょうど絵の大きさになる
fn border(p: &mut Picture) {
    let (w, h) = (p.w, p.h);
    p.fill(0, 0, w, 1, [255; 3]);
    p.fill(0, h - 1, w, h, [255; 3]);
    p.fill(0, 0, 1, h, [255; 3]);
    p.fill(w - 1, 0, w, h, [255; 3]);
}

fn composite(p: &mut Picture, bits: u32) {
    let h = p.h;
    // 上 3/8 カラーバー、次の 1/4 階調、次の 1/8 縦縞、残り格子
    let a = h * 3 / 8;
    let b = a + h / 4;
    let c = b + h / 8;
    bars(p, 0, a);
    ramp(p, a, b, bits);
    stripes(p, b, c);
    let step = (p.w.min(h) / 8).max(8);
    for y in c..h {
        for x in 0..p.w {
            if x % step == 0 || (y - c) % step == 0 {
                p.set(x, y, [0, 160, 255]);
            }
        }
    }
    border(p);
}

/// 5x7 の小さな字(ASCII の英数字と記号の一部)。名前を出すのに足りる分だけ
fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        _ => [0; 7],
    }
}

/// ASCII 以外(日本語)は飛ばす。機種名の英数字部分が読めれば足りる
fn text(p: &mut Picture, x0: u32, y0: u32, s: &str, scale: u32, c: [u8; 3]) {
    let mut x = x0;
    for ch in s.chars().filter(|c| c.is_ascii()) {
        let g = glyph(ch);
        for (gy, bits) in g.iter().enumerate() {
            for gx in 0..5 {
                if bits & (0x10 >> gx) != 0 {
                    let px = x + gx * scale;
                    let py = y0 + gy as u32 * scale;
                    p.fill(px, py, px + scale, py + scale, c);
                }
            }
        }
        x += 6 * scale;
        if x + 6 * scale >= p.w {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_keeps_extremes_and_levels() {
        assert_eq!(quantize(0, 5), 0);
        assert_eq!(quantize(255, 5), 255);
        let mut seen: Vec<u8> = (0..=255).map(|v| quantize(v, 5)).collect();
        seen.dedup();
        assert_eq!(seen.len(), 32);
        assert_eq!(quantize(200, 1), 255);
    }

    #[test]
    fn every_pattern_renders_at_odd_sizes() {
        for pat in Pattern::ALL {
            let o = Options { pattern: pat, moving: true, caption: "X68000 768x512".into() };
            for (w, h) in [(1, 1), (7, 3), (256, 192), (1490, 240)] {
                let p = render(w, h, 5, &o, 123);
                assert_eq!(p.rgb.len(), (w * h) as usize);
            }
        }
    }
}
