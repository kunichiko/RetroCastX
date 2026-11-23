//! 端子に繋いだ機種とモードを保存し、次に起動したときに戻す。
//!
//! 起動のたびに既定(入力4 は未接続)へ戻ると、デジタルRGB などを試すたびに
//! 選び直すことになる。Viewer と同じく `key = value` のテキストで、読めない行は
//! 黙って無視する(壊れていても既定で起動できる)。
//!
//! 置き場所は Viewer の設定(`viewer.conf`)と同じフォルダの `devsim.conf`。
//! 起動時にパスを1行表示する。
//!
//! ★**GUI のときだけ使う。** `--headless` と `--input` を付けたときは、試験の
//!   再現のため保存した値を読まない。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::machines::Port;

pub fn path() -> Option<PathBuf> {
    let base = if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(x)
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE")
                .map(|h| PathBuf::from(h).join("AppData").join("Roaming")))?
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(base.join("retrocastx").join("devsim.conf"))
}

pub fn port_key(p: Port) -> &'static str {
    match p {
        Port::Video => "video",
        Port::Rgb21 => "rgb21",
        Port::Rgb15 => "rgb15",
        Port::Digital => "digital",
    }
}

/// 1つの端子の保存値。machine が None なら未接続
#[derive(Clone, Debug, PartialEq)]
pub struct PortSave {
    pub machine: Option<String>,
    pub mode: String,
    pub cycle_s: f32,
}

pub fn load() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let Some(p) = path() else { return m };
    let Ok(text) = std::fs::read_to_string(&p) else { return m };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    m
}

/// 保存した端子の値。その端子の行が無ければ None(= 既定のまま)
pub fn port_of(m: &BTreeMap<String, String>, p: Port) -> Option<PortSave> {
    let k = port_key(p);
    let v = m.get(&format!("input.{k}"))?;
    let machine = (v != "none").then(|| v.clone());
    let mode = m.get(&format!("mode.{k}")).cloned().unwrap_or_default();
    let cycle_s = m.get(&format!("cycle.{k}")).and_then(|c| c.parse().ok()).unwrap_or(0.0);
    Some(PortSave { machine, mode, cycle_s })
}

pub fn save(ports: &[(Port, PortSave)]) {
    let Some(p) = path() else { return };
    let mut s = String::from("# RetroCastX ダミーデバイスの設定(起動時に読む。消せば既定に戻る)\n");
    for (port, v) in ports {
        let k = port_key(*port);
        s += &format!("input.{k} = {}\n", v.machine.as_deref().unwrap_or("none"));
        s += &format!("mode.{k} = {}\n", v.mode);
        s += &format!("cycle.{k} = {}\n", v.cycle_s);
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, s) {
        eprintln!("設定を書けません {}: {e}", p.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_round_trip() {
        let mut m = BTreeMap::new();
        m.insert("input.digital".into(), "pc6001mk2sr".into());
        m.insert("mode.digital".into(), "320x200 15kHz".into());
        m.insert("cycle.digital".into(), "2.5".into());
        m.insert("input.video".into(), "none".into());
        let d = port_of(&m, Port::Digital).unwrap();
        assert_eq!(d.machine.as_deref(), Some("pc6001mk2sr"));
        assert_eq!(d.mode, "320x200 15kHz");
        assert_eq!(d.cycle_s, 2.5);
        assert_eq!(port_of(&m, Port::Video).unwrap().machine, None);
        assert!(port_of(&m, Port::Rgb15).is_none());
    }
}
