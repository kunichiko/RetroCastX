//! Viewerの設定を保存/復元する(音量など、次回起動時に引き継ぎたいもの)。
//!
//! eframeのpersistence機能はserde+ronを引き込むので、数個のスカラのために
//! 依存を増やさず自前の `key = value` テキストにする。壊れていても既定値で
//! 起動できるよう、読めない行は黙って無視する。
//!
//! 置き場所は `$XDG_CONFIG_HOME/retrocastx/viewer.conf`(未設定なら
//! `~/.config/retrocastx/viewer.conf`)。起動時にパスを1行表示して、
//! 「どこかに勝手に作られた忘れられる設定」にならないようにしている。

use std::collections::BTreeMap;
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
    /// TVPのアナログ映像帯域。0=最大 / 15=最小
    pub tune_video_bw: u8,
    /// サンプリング位相 0..31。ドット周期の1/32刻み
    pub tune_phase: u8,
    /// 表示時の縦倍率。ドットが正方形でないモードの縦つぶれを直す
    pub vscale: f32,
    /// 画面回転 0/1/2/3 = 時計回りに 0/90/180/270 度(縦画面のゲーム用)
    pub rotate: u32,
    /// 管面(表示領域)の縦横比 幅/高さ。0 なら有効映像の比をそのまま使う。
    /// 実際のCRTは「何ドットか」を知らず、偏向で決まった管面いっぱいに描く。
    /// ここを 4/3 にすれば 512x256 でも 768x512 でも同じ形で表示される。
    pub tube_aspect: f32,
    /// 補間 0=ニアレスト 1=バイリニア 2=sharp-bilinear
    pub filter: u32,
    /// 表示する切り出し範囲[画素]。w か h が 0 なら切り出さない(全体を表示)
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_w: u32,
    pub crop_h: u32,
    /// 管面が映す時間窓 [h0,h1,v0,v1](ラインとフレームに対する割合)。
    /// tube_time_based=false のときだけ使う(旧方式)。
    pub window: [f32; 4],
    /// 管面を時間ベースで決める(実CRTと同じ挙動)
    pub tube_time_based: bool,
    /// いまの帯域のモニタプロファイル [H幅, H位置, V幅, V位置]。
    ///
    /// 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど掃引する」
    /// ように周波数ごとに速度が切り替わる。だから管面の横幅は 1/fH そのもので、
    /// 掃引時間は自由パラメータではない。ここで持つのは「その周期のうち実際に
    /// 管面へ出る割合」と「位置」だけ。1.0なら周期全体が見えるので何も切れない。
    pub mon: [f32; 4],
    /// 帯域ごとのモニタプロファイル(CZ-612Dのような3モードディスプレイに相当)
    pub mon_bands: BTreeMap<u32, [f32; 4]>,
    /// 管面の位置合わせ(実CRTのH位置/V位置つまみ)

    /// モードごとの設定。キーは "fH[100Hz単位]_vtotal_htotal"。
    /// 同じ31kHzでも 768x512 と 256x256 は同期信号が同一なので、htotal
    /// (= pll_divide)まで含めないと区別できない。pll_divide を合わせた時点で
    /// モードが確定するので、それ以降は自動で復元される。
    /// 値は crop_x,crop_y,crop_w,crop_h,rotate,phase
    /// (古い設定ファイルは phase を持たないので、その場合は既定16を入れる)
    pub modes: BTreeMap<String, [u32; 6]>,
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
            tune_video_bw: 15,
            tune_phase: 16,
            vscale: 1.0,
            rotate: 0,
            window: [0.22, 0.94, 0.07, 0.98],
            tube_time_based: true,

            mon: [1.0, 0.0, 1.0, 0.0],
            mon_bands: BTreeMap::new(),
            modes: BTreeMap::new(),
            tube_aspect: 0.0,
            filter: 2,
            crop_x: 0,
            crop_y: 0,
            crop_w: 0,
            crop_h: 0,
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
                "tune_video_bw" => { if let Ok(x) = v.parse() { s.tune_video_bw = x } }
                "tune_phase" => { if let Ok(x) = v.parse::<u8>() { s.tune_phase = x.min(31) } }
                "rotate" => {
                    if let Ok(x) = v.parse::<u32>() {
                        s.rotate = if x < 4 { x } else { 0 };
                    }
                }
                k if k.starts_with("mode.") => {
                    let nums: Vec<u32> =
                        v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if nums.len() >= 5 {
                        let ph = if nums.len() >= 6 { nums[5].min(31) } else { 16 };
                        s.modes.insert(k[5..].to_string(),
                                       [nums[0], nums[1], nums[2], nums[3], nums[4], ph]);
                    }
                }
                "tube_time_based" => { s.tube_time_based = v == "true" }
                // 旧方式(掃引時間を絶対時間で持つ)のキーは読み捨てる。
                // 管面の横幅は 1/fH そのものなので、掃引時間は設定項目ではない。
                "tube_span_us" | "tube_span_ms" | "tube_link_v" | "tube_v_trim"
                    | "tube_fit_margin" | "tube_off_us" | "tube_off_ms" => {}
                k if k.starts_with("band.") => {}
                "mon" => {
                    let n: Vec<f32> =
                        v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if n.len() >= 4 { s.mon = [n[0], n[1], n[2], n[3]] }
                }
                k if k.starts_with("mon.") => {
                    if let Ok(khz) = k[4..].parse::<u32>() {
                        let n: Vec<f32> =
                            v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        if n.len() >= 4 {
                            s.mon_bands.insert(khz, [n[0], n[1], n[2], n[3]]);
                        }
                    }
                }
                "window" => {
                    let v: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() == 4 { s.window = [v[0], v[1], v[2], v[3]] }
                }
                "tube_aspect" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.tube_aspect = if (0.0..=10.0).contains(&x) { x } else { 0.0 };
                    }
                }
                "filter" => { if let Ok(x) = v.parse::<u32>() { s.filter = x.min(2) } }
                "crop_x" => { if let Ok(x) = v.parse() { s.crop_x = x } }
                "crop_y" => { if let Ok(x) = v.parse() { s.crop_y = x } }
                "crop_w" => { if let Ok(x) = v.parse() { s.crop_w = x } }
                "crop_h" => { if let Ok(x) = v.parse() { s.crop_h = x } }
                "vscale" => {
                    if let Ok(x) = v.parse::<f32>() {
                        s.vscale = x.clamp(0.25, 4.0);
                    }
                }
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
             tune_video_bw = {}\n\
             tune_phase = {}\n\
             tube_time_based = {}\n\
             mon = {:.4},{:.4},{:.4},{:.4}\n\
             rotate = {}\n\
             window = {:.4},{:.4},{:.4},{:.4}\n\
             tube_aspect = {:.4}\n\
             filter = {}\n\
             crop_x = {}\n\
             crop_y = {}\n\
             crop_w = {}\n\
             crop_h = {}\n\
             vscale = {:.3}\n\
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
            self.tune_video_bw,
            self.tune_phase,
            self.tube_time_based,
            self.mon[0], self.mon[1], self.mon[2], self.mon[3],
            self.rotate,
            self.window[0],
            self.window[1],
            self.window[2],
            self.window[3],
            self.tube_aspect,
            self.filter,
            self.crop_x,
            self.crop_y,
            self.crop_w,
            self.crop_h,
            self.vscale,
            self.window_w,
            self.window_h,
        );
        let mut body = body;
        for (khz, v) in &self.mon_bands {
            body.push_str(&format!(
                "mon.{khz} = {},{},{},{}\n", v[0], v[1], v[2], v[3]));
        }
        for (k, v) in &self.modes {
            body.push_str(&format!(
                "mode.{k} = {},{},{},{},{},{}\n",
                v[0], v[1], v[2], v[3], v[4], v[5]));
        }
        // 書けなくても致命的ではないので黙って諦める(次回は既定値で起動する)
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(body.as_bytes());
        }
    }
}
