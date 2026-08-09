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
    /// キャプチャ範囲の外側の色: black|dark gray|magenta
    pub backdrop: String,
    /// キャプチャ範囲の境界に枠線を描く
    pub show_border: bool,
    /// 画枠パラメータ(ボードへCONFIGで送る値)
    pub tune_vbp: i32,
    pub tune_hs_offset: i32,
    pub tune_pll_divide: i32,
    /// 目標の有効幅[ドット]
    pub tune_target_w: i32,
    /// 推奨値を8の倍数に丸める
    pub tune_snap8: bool,
    /// インターレース方式。0=なし / 1=1VSYNCに2フィールド / 2=フィールド毎VSYNC
    pub tune_interlace: u8,
    /// 第2フィールドが始まる row(0=vtotal/2)
    pub tune_f2_row: i32,
    /// フィールドの偶奇を入れ替える
    pub tune_field_swap: bool,
    /// 方式2の極性の取得元。0=位相 / 1=FIDOUT
    pub tune_field_src: u8,
    /// 前回のウィンドウ内寸。次回起動時に復元する
    pub window_w: f32,
    pub window_h: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            audio_source: Some(0),
            audio_device: None,
            integer_scale: true,
            backdrop: "dark gray".into(),
            show_border: true,
            tune_vbp: 43,
            tune_hs_offset: 152,
            tune_pll_divide: 1104,
            tune_target_w: 768,
            tune_snap8: true,
            tune_interlace: 0,
            tune_f2_row: 0,
            tune_field_swap: false,
            tune_field_src: 0,
            window_w: 1160.0,
            window_h: 820.0,
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
                "show_border" => s.show_border = v == "true",
                "backdrop" => s.backdrop = v.to_string(),
                "tune_vbp" => { if let Ok(x) = v.parse() { s.tune_vbp = x } }
                "tune_hs_offset" => { if let Ok(x) = v.parse() { s.tune_hs_offset = x } }
                "tune_pll_divide" => { if let Ok(x) = v.parse() { s.tune_pll_divide = x } }
                "tune_target_w" => { if let Ok(x) = v.parse() { s.tune_target_w = x } }
                "tune_snap8" => s.tune_snap8 = v == "true",
                "tune_interlace" => {
                    // 以前は真偽値だった。古い設定ファイルも読めるようにする
                    s.tune_interlace = match v {
                        "true" => 1,
                        "false" => 0,
                        _ => v.parse().unwrap_or(0),
                    };
                }
                "tune_f2_row" => { if let Ok(x) = v.parse() { s.tune_f2_row = x } }
                "tune_field_swap" => s.tune_field_swap = v == "true",
                "tune_field_src" => { if let Ok(x) = v.parse() { s.tune_field_src = x } }
                "window_w" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.window_w = x.clamp(480.0, 8192.0);
                    }
                }
                "window_h" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.window_h = x.clamp(360.0, 8192.0);
                    }
                }
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
             integer_scale = {}\n\
             backdrop = {}\n\
             show_border = {}\n\
             tune_vbp = {}\n\
             tune_hs_offset = {}\n\
             tune_pll_divide = {}\n\
             tune_target_w = {}\n\
             tune_snap8 = {}\n\
             tune_interlace = {}\n\
             tune_f2_row = {}\n\
             tune_field_swap = {}\n\
             tune_field_src = {}\n\
             window_w = {:.0}\n\
             window_h = {:.0}\n",
            self.volume,
            self.muted,
            self.audio_source.map(|v| v.to_string()).unwrap_or_else(|| "off".into()),
            self.audio_device.clone().unwrap_or_default(),
            self.integer_scale,
            self.backdrop,
            self.show_border,
            self.tune_vbp,
            self.tune_hs_offset,
            self.tune_pll_divide,
            self.tune_target_w,
            self.tune_snap8,
            self.tune_interlace,
            self.tune_f2_row,
            self.tune_field_swap,
            self.tune_field_src,
            self.window_w,
            self.window_h,
        );
        // 書けなくても致命的ではないので黙って諦める(次回は既定値で起動する)
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(body.as_bytes());
        }
    }
}
