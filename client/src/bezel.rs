//! モニタの枠(ベゼル)。実在モニタのイラストを映像のまわりに描く。
//!
//! 実機のCRTは「ドット数ではなく管面の形が表示を決める」ので、管面モデルで
//! 表示している映像は、そのまま実在モニタの開口部にはめられる。枠はそれを
//! 見た目として完成させるためのもの。
//!
//! 絵はSVG(`assets/`)で持つ。起動時に1回だけラスタライズしてテクスチャに
//! 保持する。SVGにしてあるのは、寸法を採寸値のまま座標で持てて後から直せる
//! ことと、将来 CZ-600D など機種を増やすときに絵だけ足せばよくするため。
//!
//! 開口部(映像を置く矩形)はSVGから読めないので、ここに定数として持つ。
//! SVGの `<desc>` にも同じ値が書いてある。片方だけ直すと絵と映像がずれる。

/// 枠1つぶんの定義。座標はSVGのviewBox座標系。
pub struct Bezel {
    pub key: &'static str,
    pub label: &'static str,
    /// SVGの中身(ビルド時に埋め込む)
    pub svg: &'static str,
    /// viewBox の大きさ
    pub view: (f32, f32),
    /// 映像を置く開口部 [x, y, w, h]
    pub screen: [f32; 4],
}

pub const BEZELS: &[Bezel] = &[Bezel {
    key: "cz612d",
    label: "SHARP CZ-612D",
    svg: include_str!("../assets/cz612d.svg"),
    view: (1013.0, 1057.0),
    // assets/cz612d.svg の <desc> と一致させること
    screen: [105.0, 127.0, 797.0, 598.0],
}];

pub fn by_key(key: &str) -> Option<&'static Bezel> {
    BEZELS.iter().find(|b| b.key == key)
}

impl Bezel {
    /// 開口部の縦横比(幅/高さ)。管面をここへ合わせる
    pub fn screen_aspect(&self) -> f32 {
        self.screen[2] / self.screen[3].max(1.0)
    }

    /// 枠全体の縦横比
    pub fn outer_aspect(&self) -> f32 {
        self.view.0 / self.view.1.max(1.0)
    }

    /// 指定した幅[px]でラスタライズして RGBA を返す。高さは比から決まる。
    ///
    /// 失敗しても致命的ではない(枠なしで表示すればよい)ので Option を返す。
    pub fn rasterize(&self, width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let width = width.clamp(64, 4096);
        let height = ((width as f32) / self.outer_aspect()).round().max(1.0) as u32;
        let mut opt = resvg::usvg::Options::default();
        // パネルの文字にシステムフォントを使う。読めなくても図形は出る
        opt.fontdb_mut().load_system_fonts();
        let tree = resvg::usvg::Tree::from_str(self.svg, &opt).ok()?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
        let sx = width as f32 / self.view.0;
        let sy = height as f32 / self.view.1;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(sx, sy),
            &mut pixmap.as_mut(),
        );
        Some((pixmap.take(), width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SVGがラスタライズできて、寸法が viewBox の比に合うこと。
    #[test]
    fn rasterizes() {
        for b in BEZELS {
            let (rgba, w, h) = b.rasterize(512).expect("ラスタライズ失敗");
            assert_eq!(w, 512);
            let want_h = (512.0 / b.outer_aspect()).round() as u32;
            assert_eq!(h, want_h, "{}", b.key);
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        }
    }

    /// 開口部が透明に抜けていること。
    ///
    /// ここが抜けていないと映像が枠に隠れる。作図中、開口部に半透明の帯が
    /// はみ出す不具合を2回出した(inner の even-odd の外周が開口部より小さい、
    /// ガラスの映り込みを上に重ねる)。どちらも暗い背景で合成すると見えず、
    /// 白背景で初めて露見した。テストなら背景に依らず捕まえられる。
    #[test]
    fn screen_area_is_transparent() {
        for b in BEZELS {
            let (rgba, w, h) = b.rasterize(512).unwrap();
            let sx = w as f32 / b.view.0;
            let sy = h as f32 / b.view.1;
            // 角丸を避けて開口部の内側だけを見る
            let inset = 12.0;
            let x0 = ((b.screen[0] + inset) * sx) as u32;
            let x1 = ((b.screen[0] + b.screen[2] - inset) * sx) as u32;
            let y0 = ((b.screen[1] + inset) * sy) as u32;
            let y1 = ((b.screen[1] + b.screen[3] - inset) * sy) as u32;
            let mut opaque = 0;
            let mut total = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let a = rgba[((y * w + x) * 4 + 3) as usize];
                    total += 1;
                    if a > 8 {
                        opaque += 1;
                    }
                }
            }
            assert!(total > 1000, "{} 検査領域が狭すぎる", b.key);
            assert_eq!(opaque, 0,
                       "{}: 開口部に不透明な画素が {}/{} ある", b.key, opaque, total);
        }
    }

    /// 枠の外側(キャビネットの中央付近)は不透明であること。
    /// 透明なら絵が描かれていない = SVGの読み込みに失敗している。
    #[test]
    fn cabinet_is_opaque() {
        for b in BEZELS {
            let (rgba, w, h) = b.rasterize(512).unwrap();
            let sx = w as f32 / b.view.0;
            let sy = h as f32 / b.view.1;
            // 開口部の左側のベゼル面を見る
            let x = ((b.screen[0] * 0.5) * sx) as u32;
            let y = ((b.screen[1] + b.screen[3] * 0.5) * sy) as u32;
            let a = rgba[((y * w + x) * 4 + 3) as usize];
            assert!(a > 200, "{}: 枠が描かれていない (alpha={a})", b.key);
        }
    }
}
