//! ユーザーが作る機種(プロファイル)のファイル。
//!
//! 買った人が自分の機種に合わせた管面と PLL を書き出して作者へ送り、作者が
//! 裏を取って次の版で正式採用する(`profiles::Preset` へ写す)、という流れのための
//! もの。有志が作ったファイルを置けば、その機種が端子の機種一覧に出る。
//!
//! 形式は設定ファイル(`settings`)と同じ `key = value` のテキスト。メールに
//! 貼っても読めるように、依存を増やして JSON にはしない。
//!
//! ```text
//! name = FM-TOWNS
//! port = rgb15            ← どの端子の機種か(video / rgb21 / rgb15 / digital)
//! base = x68000           ← 水晶の候補と入力設定を借りる組み込みの機種。空なら汎用
//! author = …
//! note = …
//! mon.31 = 0.9200,-0.0600,0.9500,0.0000      ← 帯域ごとの管面
//! bandpll.31 = 1104,12                       ← 帯域ごとの pll_divide, 位相
//! ```
//!
//! 置き場所は設定ファイルの隣の `profiles/`。パネルの「フォルダを開く」で
//! 開けるので、どこにあるか分からなくなる設定にはしない。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::profiles::{self, Port};
use crate::settings::{self, Settings};

/// ユーザーの機種のキーに付ける印。組み込みのキーと重ならないようにする
pub const KEY_PREFIX: &str = "u:";

#[derive(Clone, Debug, PartialEq)]
pub struct UserProfile {
    /// `u:` + ファイル名(拡張子なし)。設定ファイルと管面のキーに使う
    pub key: String,
    pub label: String,
    pub port: Port,
    /// 水晶の候補と入力設定を借りる組み込みの機種。空なら汎用
    pub base: String,
    pub author: String,
    pub note: String,
    pub mon: BTreeMap<u32, [f32; 4]>,
    pub pll: BTreeMap<u32, [u32; 2]>,
}

impl UserProfile {
    /// 借りている組み込みの機種(汎用なら None)
    pub fn base_profile(&self) -> Option<&'static profiles::Profile> {
        profiles::by_key(&self.base).filter(|p| p.port == self.port)
    }

    /// テキストから読む。`stem` はファイル名(拡張子なし)。端子が書かれていない
    /// ものはプロファイルではないとみなして捨てる
    pub fn parse(text: &str, stem: &str) -> Option<UserProfile> {
        let mut u = UserProfile {
            key: format!("{KEY_PREFIX}{}", sanitize(stem)),
            label: stem.to_string(),
            port: Port::Rgb15,
            base: String::new(),
            author: String::new(),
            note: String::new(),
            mon: BTreeMap::new(),
            pll: BTreeMap::new(),
        };
        let mut has_port = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "name" if !v.is_empty() => u.label = v.to_string(),
                "port" => {
                    if let Some(p) = Port::from_key(v) {
                        u.port = p;
                        has_port = true;
                    }
                }
                "base" => u.base = v.to_string(),
                "author" => u.author = v.to_string(),
                "note" => u.note = v.to_string(),
                k if k.starts_with("mon.") => {
                    if let (Ok(b), Some(m)) = (k[4..].parse(), settings::parse_mon(v)) {
                        u.mon.insert(b, m);
                    }
                }
                k if k.starts_with("bandpll.") => {
                    if let (Ok(b), Some(p)) = (k[8..].parse(), settings::parse_pll(v)) {
                        u.pll.insert(b, p);
                    }
                }
                _ => {}
            }
        }
        has_port.then_some(u)
    }

    pub fn format(&self) -> String {
        let mut s = String::new();
        s.push_str("# RetroCastX プロファイル\n");
        s.push_str("# Viewer の「プロファイルのフォルダ」に置くと、端子の機種一覧に出ます。\n");
        s.push_str(&format!("# 書き出した Viewer: {}\n", env!("CARGO_PKG_VERSION")));
        s.push_str(&format!("name = {}\n", self.label));
        s.push_str(&format!("port = {}\n", self.port.key()));
        s.push_str(&format!("base = {}\n", self.base));
        s.push_str(&format!("author = {}\n", self.author));
        s.push_str(&format!("note = {}\n", self.note));
        for (b, m) in &self.mon {
            s.push_str(&format!("mon.{b} = {:.4},{:.4},{:.4},{:.4}\n", m[0], m[1], m[2], m[3]));
        }
        for (b, p) in &self.pll {
            s.push_str(&format!("bandpll.{b} = {},{}\n", p[0], p[1]));
        }
        // 正式採用するときに profiles.rs へそのまま貼れる形。読み込みでは無視される
        s.push_str("#\n# 正式採用するときは client/src/profiles.rs の機種の presets へ:\n");
        s.push_str("#     presets: &[\n");
        let bands: std::collections::BTreeSet<u32> =
            self.mon.keys().chain(self.pll.keys()).copied().collect();
        for b in bands {
            let mon = self.mon.get(&b).map_or("None".into(), |m| {
                format!("Some([{:.4}, {:.4}, {:.4}, {:.4}])", m[0], m[1], m[2], m[3])
            });
            let pll = self.pll.get(&b).map_or("None".into(), |p| format!("Some([{}, {}])", p[0], p[1]));
            s.push_str(&format!("#         Preset {{ band: {b}, mon: {mon}, pll: {pll} }},\n"));
        }
        s.push_str("#     ],\n");
        s
    }
}

/// キーに使えない文字を `_` にする(設定ファイルの `mon@<キー>.<帯域>` に入るので、
/// `=` や空白が混ざると読めなくなる)
fn sanitize(stem: &str) -> String {
    stem.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// プロファイルを置くフォルダ(設定ファイルの隣)
pub fn dir() -> PathBuf {
    Settings::path().with_file_name("profiles")
}

/// フォルダの `*.txt` を全部読む。読めないものは黙って飛ばす。並びはファイル名順
pub fn load_all() -> Vec<UserProfile> {
    load_dir(&dir())
}

fn load_dir(dir: &Path) -> Vec<UserProfile> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt")))
        .collect();
    paths.sort();
    let mut out: Vec<UserProfile> = Vec::new();
    for p in paths {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("profile");
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        if let Some(u) = UserProfile::parse(&text, stem) {
            // 記号を `_` にした結果キーが重なったら、後のものは捨てる
            if !out.iter().any(|o| o.key == u.key) {
                out.push(u);
            }
        }
    }
    out
}

/// フォルダへ書き出す。名前が重なれば `-2` `-3` … を付ける。書いたパスを返す
pub fn export(u: &UserProfile, stem: &str) -> std::io::Result<PathBuf> {
    export_to(&dir(), u, stem)
}

fn export_to(dir: &Path, u: &UserProfile, stem: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stem = sanitize(stem);
    let mut path = dir.join(format!("{stem}.txt"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.txt"));
        n += 1;
    }
    std::fs::write(&path, u.format())?;
    Ok(path)
}

/// フォルダを OS のファイル管理で開く。モバイルには無いので false
pub fn reveal(dir: &Path) -> bool {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(target_os = "macos")]
    let cmd = Some("open");
    #[cfg(target_os = "windows")]
    let cmd = Some("explorer");
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    let cmd = Some("xdg-open");
    #[cfg(any(target_os = "ios", target_os = "android"))]
    let cmd: Option<&str> = None;
    cmd.is_some_and(|c| std::process::Command::new(c).arg(dir).spawn().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UserProfile {
        let mut u = UserProfile {
            key: "u:fm-towns".into(),
            label: "FM-TOWNS".into(),
            port: Port::Rgb15,
            base: String::new(),
            author: "tester".into(),
            note: "31kHz の 640x480 で合わせた".into(),
            mon: BTreeMap::new(),
            pll: BTreeMap::new(),
        };
        u.mon.insert(31, [0.92, -0.06, 0.95, 0.0]);
        u.pll.insert(31, [800, 12]);
        u
    }

    /// 書き出したものを読み直すと同じになる(送ってもらったファイルがそのまま使える)
    #[test]
    fn format_roundtrips() {
        let u = sample();
        let r = UserProfile::parse(&u.format(), "fm-towns").expect("読めない");
        assert_eq!(r, u);
        // 正式採用用の貼り付け形も出ている
        assert!(u.format().contains("Preset { band: 31, mon: Some([0.9200, -0.0600, 0.9500, 0.0000]), pll: Some([800, 12]) }"));
    }

    /// 端子の無いテキストはプロファイルとして読まない(フォルダに別の txt が
    /// 置かれていても機種一覧を汚さない)
    #[test]
    fn ignores_text_without_port() {
        assert!(UserProfile::parse("name = memo\n", "memo").is_none());
    }

    /// ファイル名の記号はキーに入らない
    #[test]
    fn key_is_sanitized() {
        let u = UserProfile::parse("port = rgb21\n", "アーケード 基板=1").unwrap();
        assert!(u.key.starts_with(KEY_PREFIX));
        assert!(!u.key.contains(' ') && !u.key.contains('='), "{}", u.key);
        assert_eq!(u.label, "アーケード 基板=1");
    }

    /// 書き出しは既存のファイルを上書きしない。フォルダから全部読める
    #[test]
    fn export_never_overwrites_and_loads_back() {
        let dir = std::env::temp_dir().join(format!("rcx-userprof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let u = sample();
        let a = export_to(&dir, &u, "fm-towns").unwrap();
        let b = export_to(&dir, &u, "fm-towns").unwrap();
        assert_ne!(a, b);
        let all = load_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "FM-TOWNS");
    }

    /// 借りる機種が別の端子のものなら汎用として扱う(15ピンの機種が MSX の
    /// 入力設定を書かないように)
    #[test]
    fn base_must_be_on_the_same_port() {
        let mut u = sample();
        u.base = "msx".into();
        assert!(u.base_profile().is_none());
        u.base = "x68000".into();
        assert_eq!(u.base_profile().map(|p| p.key), Some("x68000"));
    }
}
