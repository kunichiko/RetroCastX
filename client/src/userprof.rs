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
//! mon.31v70 = 0.8500,-0.0500,0.9000,0.0000   ← 帯域 × 垂直周波数(Hz)の管面
//! bandpll.31 = 1104,12                       ← 帯域ごとの pll_divide, 位相
//! ```
//!
//! 置き場所は設定ファイルの隣の `profiles/`。ここは**自分用の機種の置き場**で、
//! パネルの「自分用の機種」で作る・直す・消す。人に渡すファイルはここに置かず、
//! 保存のダイアログで好きな場所へ書き出す(`write_file`)。もらったファイルは
//! 開くダイアログで読み込むと、ここへ取り込まれる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::geometry::{key_band, key_from_str, key_fv, key_to_str};
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
    /// デジタルRGB の色の方式(書いていなければ借りている機種の既定)
    pub palette: Option<crate::palette::Palette>,
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
            palette: None,
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
                "palette" => u.palette = crate::palette::Palette::from_key(v),
                k if k.starts_with("mon.") => {
                    if let (Some(b), Some(m)) = (key_from_str(&k[4..]), settings::parse_mon(v)) {
                        u.mon.insert(b, m);
                    }
                }
                k if k.starts_with("bandpll.") => {
                    if let (Some(b), Some(p)) = (key_from_str(&k[8..]), settings::parse_pll(v)) {
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
        if let Some(p) = self.palette {
            s.push_str(&format!("palette = {}\n", p.key()));
        }
        for (b, m) in &self.mon {
            s.push_str(&format!("mon.{} = {:.4},{:.4},{:.4},{:.4}\n", key_to_str(*b), m[0], m[1], m[2], m[3]));
        }
        for (b, p) in &self.pll {
            s.push_str(&format!("bandpll.{} = {},{}\n", key_to_str(*b), p[0], p[1]));
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
            let fv = key_fv(b).map_or("None".into(), |f| format!("Some({f})"));
            s.push_str(&format!("#         Preset {{ band: {}, fv: {fv}, mon: {mon}, pll: {pll} }},\n",
                                key_band(b)));
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

impl UserProfile {
    /// フォルダの中のファイル名(拡張子なし)。キーから `u:` を外したもの
    pub fn stem(&self) -> &str {
        self.key.trim_start_matches(KEY_PREFIX)
    }
}

/// 新しい機種のファイル名。表示名の英数字部分から作り、フォルダの既存と重なれば
/// `-2` `-3` … を付ける(キー = 設定ファイルの管面の持ち主なので、既存とは別にする)
pub fn unique_stem(label: &str, existing: &[UserProfile]) -> String {
    let base = sanitize(label);
    let base = base.trim_matches('_');
    let base = if base.is_empty() { "machine" } else { base };
    let taken = |s: &str| existing.iter().any(|u| u.stem() == s) || dir().join(format!("{s}.txt")).exists();
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap()
}

/// 自分用の機種をフォルダへ保存する(同じ機種は上書き)
pub fn save(u: &UserProfile) -> std::io::Result<PathBuf> {
    save_in(&dir(), u)
}

fn save_in(dir: &Path, u: &UserProfile) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.txt", u.stem()));
    std::fs::write(&path, u.format())?;
    Ok(path)
}

/// 自分用の機種をフォルダから消す
pub fn delete(u: &UserProfile) -> std::io::Result<()> {
    std::fs::remove_file(dir().join(format!("{}.txt", u.stem())))
}

/// 人に渡すファイルとして、好きな場所へ書き出す(フォルダには足さない)
pub fn write_file(u: &UserProfile, path: &Path) -> std::io::Result<()> {
    std::fs::write(path, u.format())
}

/// 人からもらったファイルを読む。キーはファイル名から作る(取り込むときに
/// `unique_stem` で付け直すかを決める)
pub fn read_file(path: &Path) -> Option<UserProfile> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("profile");
    UserProfile::parse(&std::fs::read_to_string(path).ok()?, stem)
}

/// 保存のダイアログ。Mac / Windows だけ(rfd)。他は None
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn pick_save_path(default_name: &str) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new()
        .set_title("プロファイルを書き出す")
        .set_file_name(format!("{default_name}.txt"))
        .add_filter("RetroCastX プロファイル", &["txt"]);
    if let Some(docs) = documents_dir() {
        d = d.set_directory(docs);
    }
    d.save_file()
}

/// 開くダイアログ。Mac / Windows だけ
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn pick_open_path() -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new()
        .set_title("プロファイルを読み込む")
        .add_filter("RetroCastX プロファイル", &["txt"]);
    if let Some(docs) = documents_dir() {
        d = d.set_directory(docs);
    }
    d.pick_file()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn documents_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let d = PathBuf::from(home).join("Documents");
    d.is_dir().then_some(d)
}

/// ダイアログの無い環境(Linux)は書類フォルダ(無ければホーム)へ書く。
/// 同じ名前があれば `-2` … を付ける。iOS / Android はボタンを出さないので呼ばれない
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn pick_save_path(default_name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let docs = home.join("Documents");
    let dir = if docs.is_dir() { docs } else { home };
    let base = sanitize(default_name);
    let mut path = dir.join(format!("{base}.txt"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.txt"));
        n += 1;
    }
    Some(path)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn pick_open_path() -> Option<PathBuf> {
    None
}

/// ダイアログが使えるか(パネルのボタンを出すか)
pub const HAS_FILE_DIALOG: bool = cfg!(any(target_os = "macos", target_os = "windows"));

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
            // 色の方式も書き出して読み直せること(端子とは関係なく読む)
            palette: Some(crate::palette::Palette::Pc6001),
        };
        u.mon.insert(31, [0.92, -0.06, 0.95, 0.0]);
        u.pll.insert(31, [800, 12]);
        // 垂直周波数でも分けた値(31kHz・70Hz)
        u.mon.insert(crate::geometry::band_fv_key(31, 70), [0.85, -0.05, 0.9, 0.0]);
        u
    }

    /// 書き出したものを読み直すと同じになる(送ってもらったファイルがそのまま使える)
    #[test]
    fn format_roundtrips() {
        let u = sample();
        let r = UserProfile::parse(&u.format(), "fm-towns").expect("読めない");
        assert_eq!(r, u);
        // 正式採用用の貼り付け形も出ている
        assert!(u.format().contains("Preset { band: 31, fv: None, mon: Some([0.9200, -0.0600, 0.9500, 0.0000]), pll: Some([800, 12]) }"));
        assert!(u.format().contains("mon.31v70 = "));
        assert!(u.format().contains("Preset { band: 31, fv: Some(70), mon: Some([0.8500, -0.0500, 0.9000, 0.0000]), pll: None }"));
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

    /// 保存は同じ機種を上書きし、フォルダから読み直せる。消せば一覧から消える
    #[test]
    fn save_overwrites_same_machine_and_loads_back() {
        let dir = std::env::temp_dir().join(format!("rcx-userprof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut u = sample();
        save_in(&dir, &u).unwrap();
        u.mon.insert(24, [0.8, 0.0, 0.9, 0.0]);
        save_in(&dir, &u).unwrap();
        let all = load_dir(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], u);
        std::fs::remove_file(dir.join(format!("{}.txt", u.stem()))).unwrap();
        assert!(load_dir(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 書き出したファイルを読み込むと同じ中身になる(人に渡したものがそのまま使える)
    #[test]
    fn written_file_reads_back() {
        let dir = std::env::temp_dir().join(format!("rcx-userprof-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let u = sample();
        let path = dir.join("fm-towns.txt");
        write_file(&u, &path).unwrap();
        let r = read_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r, u);
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
