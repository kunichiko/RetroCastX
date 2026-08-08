//! Viewerの設定を保存/復元する(音量など、次回起動時に引き継ぎたいもの)。
//!
//! eframeのpersistence機能はserde+ronを引き込むので、数個のスカラのために
//! 依存を増やさず自前の `key = value` テキストにする。壊れていても既定値で
//! 起動できるよう、読めない行は黙って無視する。
//!
//! 置き場所は `$XDG_CONFIG_HOME/retrocastx/viewer.conf`(未設定なら
//! `~/.config/retrocastx/viewer.conf`)。起動時にパスを1行表示して、
//! 「どこかに勝手に作られた忘れられる設定」にならないようにしている。

use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Settings {
    pub volume: f32,
    pub muted: bool,
    /// 再生する音声source。None は再生しない
    pub audio_source: Option<u8>,
    /// 出力デバイス名。None はシステム既定
    pub audio_device: Option<String>,
    pub integer_scale: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            audio_source: Some(0),
            audio_device: None,
            integer_scale: true,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("retrocastx").join("viewer.conf")
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        let Ok(text) = std::fs::read_to_string(Self::path()) else { return s };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "volume" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.volume = x.clamp(0.0, 1.5);
                    }
                }
                "muted" => s.muted = v == "true",
                "integer_scale" => s.integer_scale = v == "true",
                "audio_source" => {
                    s.audio_source = if v == "off" { None } else { v.parse().ok() };
                }
                "audio_device" => {
                    s.audio_device = if v.is_empty() { None } else { Some(v.to_string()) };
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let body = format!(
            "# RetroCastX Viewer settings (自動生成。消しても既定値で起動する)\n\
             volume = {:.3}\n\
             muted = {}\n\
             audio_source = {}\n\
             audio_device = {}\n\
             integer_scale = {}\n",
            self.volume,
            self.muted,
            self.audio_source.map(|v| v.to_string()).unwrap_or_else(|| "off".into()),
            self.audio_device.clone().unwrap_or_default(),
            self.integer_scale,
        );
        // 書けなくても致命的ではないので黙って諦める(次回は既定値で起動する)
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(body.as_bytes());
        }
    }
}
