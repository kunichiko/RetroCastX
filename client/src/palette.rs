//! デジタルRGB の色の方式。ボードが送る RGBI4 の4bit(bit0=R bit1=G bit2=B bit3=I)を
//! どの色で見せるか。
//!
//! ボードとゲートウェアは4本の線をそのまま送るだけで、色の意味は知らない。4本目(I)の
//! 意味が機種で違うので、Viewer が機種(プロファイル)ごとに選ぶ。
//!
//! devsim も同じ表で信号を作る(`#[path]` で読む)ので、このファイルは他のモジュールに
//! 依存しないこと。

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Palette {
    /// 8色。I を見ない(8色の機種には I の線が無く、ボードのプルダウンで 0)
    #[default]
    Rgb8,
    /// RGBI 16色(IBM CGA と同じ)。色の bit で 170、I で全チャンネル +85。
    /// 黒 + I は暗い灰色になる
    Rgbi16,
    /// RGBI 15色。I=0 で色を 170、I=1 で 255 にする。黒は I に関係なく黒
    /// (「輝度違いの黒を同一視して15色」のモニタ)
    Rgbi15,
    /// PC-6001mkII / mkIISR / PC-6601 系の15色。I の線に来る色相制御の信号(C)で、
    /// 専用ディスプレイの中の表を切り替える方式(RGBI ではない)。C=1 で普通の8色、
    /// C=0 で中間色(橙・青緑・黄緑・青紫・赤紫・空色・灰)
    Pc6001,
}

/// PC-6001 系の中間色。添字は RGB の3bit(bit0=R bit1=G bit2=B)。
///
/// 値は「Retro PC画像変換」(https://nazo.main.jp/prog/retropc/gcrpc.html)の
/// 「PC-6001系15色」の色見本から取った。そこでは機種の色コード(bit0=B bit1=R bit2=G)
/// 1〜7 が中間色、8〜15 が普通の8色(8 が黒)の並びで、0 と 8 がどちらも黒。
/// ★**C の極性(C=0 が中間色)は、この並びからの推測。** 実機で中間色と普通の色が
///   入れ替わって見えたら、ここではなく `table` の I の扱いを逆にする
const PC6001_HALF: [[u8; 3]; 8] = [
    [0x00, 0x00, 0x00], // 黒
    [0xFF, 0xAE, 0x00], // R   → 橙
    [0x00, 0xFF, 0xAC], // G   → 青緑
    [0xAC, 0xFF, 0x00], // RG  → 黄緑
    [0xAC, 0x00, 0xFF], // B   → 青紫
    [0xFF, 0x00, 0xAC], // RB  → 赤紫
    [0x00, 0xAE, 0xFF], // GB  → 空色
    [0xAC, 0xAE, 0xAC], // RGB → 灰色
];

impl Palette {
    pub const ALL: [Palette; 4] = [Palette::Rgb8, Palette::Rgbi16, Palette::Rgbi15, Palette::Pc6001];

    /// 設定ファイルとプロファイルに書く名前
    pub fn key(self) -> &'static str {
        match self {
            Palette::Rgb8 => "rgb8",
            Palette::Rgbi16 => "rgbi16",
            Palette::Rgbi15 => "rgbi15",
            Palette::Pc6001 => "pc6001",
        }
    }

    pub fn from_key(s: &str) -> Option<Palette> {
        Palette::ALL.into_iter().find(|p| p.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Palette::Rgb8 => "8色",
            Palette::Rgbi16 => "RGBI 16色",
            Palette::Rgbi15 => "RGBI 15色",
            Palette::Pc6001 => "PC-6001 15色",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Palette::Rgb8 => "RGB の3本だけを見る。I(4本目)の線が無い機種に",
            Palette::Rgbi16 => "I で明るくする16色(IBM CGA と同じ)。暗い色は 2/3 の明るさ、\n\
                                黒に I が付くと暗い灰色になる",
            Palette::Rgbi15 => "I で明るくする15色。I が無いと 2/3 の明るさ、黒は I に関係なく黒",
            Palette::Pc6001 => "PC-6001mkII / mkIISR / PC-6601 系の専用ディスプレイの15色。\n\
                                4本目の線(色相制御)で橙・青緑・黄緑・青紫・赤紫・空色・灰色の\n\
                                中間色に切り替える",
        }
    }

    pub fn to_u8(self) -> u8 {
        Palette::ALL.iter().position(|p| *p == self).unwrap_or(0) as u8
    }

    pub fn from_u8(v: u8) -> Palette {
        Palette::ALL.get(v as usize).copied().unwrap_or_default()
    }

    /// RGBI4 の値(0〜15)→ RGB888
    pub fn table(self) -> [[u8; 3]; 16] {
        let mut t = [[0u8; 3]; 16];
        for (v, c) in t.iter_mut().enumerate() {
            let rgb = v & 7;
            let i = v & 8 != 0;
            let bit = |ch: usize| rgb >> ch & 1 != 0;
            *c = match self {
                Palette::Rgb8 => [0, 1, 2].map(|ch| if bit(ch) { 255 } else { 0 }),
                Palette::Rgbi16 => [0, 1, 2].map(|ch| bit(ch) as u8 * 170 + i as u8 * 85),
                Palette::Rgbi15 => [0, 1, 2].map(|ch| if !bit(ch) { 0 } else if i { 255 } else { 170 }),
                Palette::Pc6001 if i => [0, 1, 2].map(|ch| if bit(ch) { 255 } else { 0 }),
                Palette::Pc6001 => PC6001_HALF[rgb],
            };
        }
        t
    }

    /// 色が最も近い RGBI4 の値(devsim が絵を信号にするときに使う)。
    /// 同じ色が2つある(黒など)ときは小さい方
    pub fn nearest(self, c: [u8; 3]) -> u8 {
        let t = self.table();
        let dist = |v: usize| -> u32 {
            (0..3).map(|ch| {
                let d = c[ch] as i32 - t[v][ch] as i32;
                (d * d) as u32
            }).sum()
        };
        let lim = if self == Palette::Rgb8 { 8 } else { 16 };
        (0..lim).min_by_key(|&v| dist(v)).unwrap() as u8
    }

    /// 出せる色の数(同じ色を除く)
    pub fn colors(self) -> usize {
        let mut t = self.table().to_vec();
        t.sort();
        t.dedup();
        t.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_counts() {
        assert_eq!(Palette::Rgb8.colors(), 8);
        assert_eq!(Palette::Rgbi16.colors(), 16);
        assert_eq!(Palette::Rgbi15.colors(), 15);
        assert_eq!(Palette::Pc6001.colors(), 15);
    }

    #[test]
    fn keys_round_trip() {
        for p in Palette::ALL {
            assert_eq!(Palette::from_key(p.key()), Some(p));
            assert_eq!(Palette::from_u8(p.to_u8()), p);
        }
    }

    /// 以前の 16色の展開(色の bit で 170、I で +85)と同じ
    #[test]
    fn rgbi16_matches_cga() {
        let t = Palette::Rgbi16.table();
        assert_eq!(t[0x7], [170, 170, 170]);
        assert_eq!(t[0x8], [85, 85, 85]);
        assert_eq!(t[0xF], [255, 255, 255]);
        assert_eq!(t[0x1], [170, 0, 0]);
    }

    #[test]
    fn pc6001_colors() {
        let t = Palette::Pc6001.table();
        assert_eq!(t[0x9], [255, 0, 0]); // 赤
        assert_eq!(t[0x1], [0xFF, 0xAE, 0x00]); // 橙
        assert_eq!(t[0xF], [255, 255, 255]);
        assert_eq!(t[0x0], t[0x8]); // 黒が2つ
    }

    #[test]
    fn nearest_is_exact_for_table() {
        for p in Palette::ALL {
            let t = p.table();
            for v in 0..16usize {
                if p == Palette::Rgb8 && v >= 8 {
                    continue;
                }
                assert_eq!(t[p.nearest(t[v]) as usize], t[v], "{p:?} {v}");
            }
        }
    }
}
