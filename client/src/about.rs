//! 「このアプリについて」と「オープンソースライセンス」の画面(ランチャーから開く)。
//!
//! ★**ストアで配るアプリはここが要る。** 依存ライブラリの多くは MIT / Apache-2.0 で、
//!   バイナリを配るときも著作権表示とライセンス本文を添えることを求める。一覧は
//!   `tools/gen_licenses.py` が `assets/third-party-licenses.txt` に書き出したものを
//!   埋め込む(依存を変えたら作り直す。CI の checks が古さを見る)。
//! ★**ビルド番号は .app の Info.plist から読む。** 番号は CI が bundle.sh に渡して
//!   決めるので、コンパイル時には分からない。cargo で直接起動したときは版だけ出す。

use std::sync::OnceLock;

use eframe::egui;

use crate::theme;

const LICENSES: &str = include_str!("../assets/third-party-licenses.txt");
const REPO_URL: &str = "https://github.com/kunichiko/RetroCastX";
const COPYRIGHT: &str = "© 2026 Kunihiko Ohnaka";

const INK: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xEB, 0xEA);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x9A, 0x99);

/// 依存ライブラリ1つ
pub struct Pkg {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
    pub repo: Option<&'static str>,
    /// (本文の番号, 元のファイル名)
    pub texts: Vec<(usize, &'static str)>,
}

/// 埋め込んだ一覧を読む(形式は tools/gen_licenses.py の冒頭)
pub fn parse(src: &'static str) -> (Vec<Pkg>, Vec<&'static str>) {
    let mut pkgs: Vec<Pkg> = Vec::new();
    let mut texts: Vec<&'static str> = Vec::new();
    // 本文の区切りは "\n@text N\n"。本文の中の行頭の @ は生成時にずらしてある
    let (head, body) = match src.find("\n@text ") {
        Some(i) => (&src[..i], &src[i + 1..]),
        None => (src, ""),
    };
    for line in head.lines() {
        if let Some(rest) = line.strip_prefix("@pkg ") {
            let (name, version) = rest.split_once(' ').unwrap_or((rest, ""));
            pkgs.push(Pkg { name, version, license: "", repo: None, texts: Vec::new() });
        } else if let Some(p) = pkgs.last_mut() {
            if let Some(v) = line.strip_prefix("license ") {
                p.license = v;
            } else if let Some(v) = line.strip_prefix("repo ") {
                p.repo = Some(v);
            } else if let Some(v) = line.strip_prefix("text ") {
                let (n, file) = v.split_once(' ').unwrap_or((v, ""));
                if let Ok(n) = n.parse() {
                    p.texts.push((n, file));
                }
            }
        }
    }
    for chunk in body.split("@text ").filter(|c| !c.is_empty()) {
        let (num, text) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let n: usize = num.trim().parse().unwrap_or(texts.len());
        if texts.len() <= n {
            texts.resize(n + 1, "");
        }
        texts[n] = text.trim_end_matches('\n');
    }
    (pkgs, texts)
}

fn licenses() -> &'static (Vec<Pkg>, Vec<&'static str>) {
    static L: OnceLock<(Vec<Pkg>, Vec<&'static str>)> = OnceLock::new();
    L.get_or_init(|| parse(LICENSES))
}

/// 「バージョン 0.21.0 (123)」。ビルド番号が分からなければ版だけ
pub fn version_text() -> String {
    let v = env!("CARGO_PKG_VERSION");
    match bundle_build() {
        Some(b) if !b.is_empty() && b != v => format!("バージョン {v} ({b})"),
        _ => format!("バージョン {v}"),
    }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn bundle_build() -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};
    let key = NSString::from_str("CFBundleVersion");
    let v = NSBundle::mainBundle().objectForInfoDictionaryKey(&key)?;
    let s = v.downcast::<NSString>().ok()?;
    Some(s.to_string())
}

#[cfg(not(any(target_os = "ios", target_os = "macos")))]
fn bundle_build() -> Option<String> {
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    About,
    Licenses,
}

/// 画面の状態
#[derive(Default)]
pub struct About {
    pub page: Page,
    icon: Option<egui::TextureHandle>,
    filter: String,
}

impl About {
    /// 描く。戻るボタン(一覧へ)が押されたら true
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut back = false;
        let title = match self.page {
            Page::About => "このアプリについて",
            Page::Licenses => "オープンソースライセンス",
        };
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let b = ui.add(egui::Button::new(egui::RichText::new("‹ 戻る").size(14.0))
                    .frame(false));
                if b.clicked() {
                    match self.page {
                        Page::About => back = true,
                        Page::Licenses => self.page = Page::About,
                    }
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(title).size(16.0).color(INK));
            });
            ui.add_space(10.0);
            match self.page {
                Page::About => self.about_ui(ui),
                Page::Licenses => self.licenses_ui(ui),
            }
        });
        back
    }

    fn about_ui(&mut self, ui: &mut egui::Ui) {
        let icon = self.icon.get_or_insert_with(|| {
            let img = crate::appicon::rgba()
                .map(|(rgba, w, h)| egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba))
                .unwrap_or_else(|| egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]));
            ui.ctx().load_texture("about-icon", img, egui::TextureOptions::LINEAR)
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.add(egui::Image::new((icon.id(), egui::vec2(112.0, 112.0))));
                ui.add_space(12.0);
                ui.label(egui::RichText::new("RetroCast X").size(26.0).strong().color(INK));
                ui.add_space(2.0);
                ui.label(egui::RichText::new(version_text()).size(13.0).color(MUTED));
                ui.add_space(18.0);
                ui.add(egui::Label::new(egui::RichText::new(
                    "レトロPC やゲーム機の映像を RetroCast X ボードで取り込み、\
                     LAN 経由で受けて表示するビューアーです。")
                    .size(13.5).color(INK)).wrap());
                ui.add_space(24.0);
                let btn = egui::Button::new(egui::RichText::new("オープンソースライセンス")
                    .size(14.0).color(egui::Color32::from_rgb(0x06, 0x22, 0x24)))
                    .fill(theme::ACCENT)
                    .corner_radius(20.0)
                    .min_size(egui::vec2(240.0, 40.0));
                if ui.add(btn).clicked() {
                    self.page = Page::Licenses;
                }
                ui.add_space(24.0);
                ui.label(egui::RichText::new(
                    "RetroCast X はオープンソースです(Apache License 2.0)")
                    .size(11.5).color(MUTED));
                ui.hyperlink_to(egui::RichText::new(REPO_URL).size(11.5), REPO_URL);
                ui.add_space(16.0);
                ui.label(egui::RichText::new(COPYRIGHT).size(12.0).color(MUTED));
                ui.add_space(20.0);
            });
        });
    }

    fn licenses_ui(&mut self, ui: &mut egui::Ui) {
        let (pkgs, texts) = licenses();
        ui.add(egui::Label::new(egui::RichText::new(format!(
            "このアプリは次のオープンソースソフトウェア({}件)を使っています。\
             名前を押すとライセンスの本文を表示します。", pkgs.len()))
            .size(12.0).color(MUTED)).wrap());
        ui.add_space(6.0);
        ui.add(egui::TextEdit::singleline(&mut self.filter)
            .hint_text("名前で絞り込む")
            .desired_width(f32::INFINITY));
        ui.add_space(6.0);
        let q = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for p in pkgs.iter().filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q)) {
                egui::CollapsingHeader::new(
                    egui::RichText::new(format!("{} {}", p.name, p.version)).size(13.0).color(INK))
                    .id_salt((p.name, p.version))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(p.license).size(11.5).color(theme::ACCENT));
                        if let Some(r) = p.repo {
                            ui.hyperlink_to(egui::RichText::new(r).size(11.0), r);
                        }
                        for (n, file) in &p.texts {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(*file).size(10.5).color(MUTED));
                            let body = texts.get(*n).copied().unwrap_or("");
                            ui.add(egui::Label::new(
                                egui::RichText::new(body).monospace().size(10.0)
                                    .color(egui::Color32::from_gray(170))).wrap());
                        }
                    });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 埋め込んだ一覧が読めて、どのクレートの本文の番号も実在する
    #[test]
    fn embedded_list_parses() {
        let (pkgs, texts) = parse(LICENSES);
        assert!(pkgs.len() > 100, "クレートが少なすぎる ({})", pkgs.len());
        assert!(pkgs.iter().any(|p| p.name == "egui"));
        for p in &pkgs {
            assert!(!p.license.is_empty(), "{} の license が空", p.name);
            for (n, _) in &p.texts {
                assert!(texts.get(*n).is_some_and(|t| !t.is_empty()), "{} の本文 {n} が無い", p.name);
            }
        }
        // egui の欧文フォント(Ubuntu・Hack・Noto Emoji)のライセンスも入っている
        let fonts = pkgs.iter().find(|p| p.name == "epaint_default_fonts").expect("epaint_default_fonts");
        assert!(fonts.texts.iter().any(|(_, f)| f.contains("UFL")));
        assert!(fonts.texts.iter().any(|(_, f)| f.contains("OFL")));
    }

    #[test]
    fn parses_small_sample() {
        let src = "# c\n@pkg a 1.0\nlicense MIT\ntext 0 LICENSE\n@pkg b 2.0\nlicense Zlib\n@text 0\nhello\n @world\n";
        let (p, t) = parse(src);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].name, p[0].version, p[0].license), ("a", "1.0", "MIT"));
        assert_eq!(p[0].texts, vec![(0, "LICENSE")]);
        assert_eq!(t, vec!["hello\n @world"]);
    }
}
