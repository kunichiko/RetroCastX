//! Viewerの設定を保存/復元する(音量など、次回起動時に引き継ぎたいもの)。
//!
//! eframeのpersistence機能はserde+ronを引き込むので、数個のスカラのために
//! 依存を増やさず自前の `key = value` テキストにする。壊れていても既定値で
//! 起動できるよう、読めない行は黙って無視する。
//!
//! 置き場所は環境変数を順に見て決める:
//!
//! ```text
//! $XDG_CONFIG_HOME/retrocastx/viewer.conf
//! $HOME/.config/retrocastx/viewer.conf          Unix
//! %APPDATA%\retrocastx\viewer.conf              Windows
//! %USERPROFILE%\AppData\Roaming\retrocastx\...   同(APPDATA未設定時)
//! ```
//!
//! 起動時にパスを1行表示して、「どこかに勝手に作られた忘れられる設定」に
//! ならないようにしている。

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
    /// 画枠パラメータ(ボードへCONFIGで送る値)
    pub tune_vbp: i32,
    pub tune_hs_offset: i32,
    pub tune_pll_divide: i32,
    /// TVPのアナログ映像帯域。0=最大 / 15=最小
    pub tune_video_bw: u8,
    /// サンプリング位相 0..31。ドット周期の1/32刻み
    pub tune_phase: u8,
    /// 1ラインまるごと送る(非黒範囲の最適化を切る)
    pub tune_full_line: bool,
    /// フレーム間引き 0=毎フレーム / 1=2フレームに1回 …
    pub tune_frame_skip: u8,
    /// 映像ソースのプロファイル名。空文字は「自動」(全プロファイルを試す)
    pub source_profile: String,
    /// 画面回転 0/1/2/3 = 時計回りに 0/90/180/270 度(縦画面のゲーム用)
    pub rotate: u32,
    /// 管面(表示領域)の縦横比 幅/高さ。0 なら有効映像の比をそのまま使う。
    /// 実際のCRTは「何ドットか」を知らず、偏向で決まった管面いっぱいに描く。
    /// ここを 4/3 にすれば 512x256 でも 768x512 でも同じ形で表示される。
    pub tube_aspect: f32,
    /// 右の操作パネルを表示するか
    pub show_panel: bool,
    /// NIC受信バッファーの警告ダイアログを二度と出さない。
    /// 管理者権限が無くて直せない人に毎回出すのは敵対的なので、逃げ道を用意する
    /// (パネル内の表示は消さない)
    pub netcheck_muted: bool,
    /// モニタの枠(ベゼル)の名前。空文字は枠なし
    pub bezel: String,
    /// 枠を一時的に隠す(bezel の選択は保つ)
    pub bezel_off: bool,
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
    /// pll_divide と位相は帯域ごとに別途持つ(mon_pll)。
    ///
    /// 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど掃引する」
    /// ように周波数ごとに速度が切り替わる。だから管面の横幅は 1/fH そのもので、
    /// 掃引時間は自由パラメータではない。ここで持つのは「その周期のうち実際に
    /// 管面へ出る割合」と「位置」だけ。1.0なら周期全体が見えるので何も切れない。
    pub mon: [f32; 4],
    /// 帯域ごとのモニタプロファイル(CZ-612Dのような3モードディスプレイに相当)
    pub mon_bands: BTreeMap<u32, [f32; 4]>,
    /// 帯域ごとの [pll_divide, 位相]。
    ///
    /// pll_divide はモードごとに正解が違い(31kHz 768x512 は1104、15kHz 512x512 は
    /// 1216 など)、位相も帯域ごとに最適値が変わる(ケーブルとTVP内部の遅延は固定[ns]
    /// だが、位相レジスタはドット周期の1/32刻みなのでドットクロックが変われば
    /// 同じ遅延が別の目盛りになる)。1つだけ持つと帯域を切り替えるたびに合わせ直しに
    /// なるので、帯域ごとに覚える。
    pub band_pll: BTreeMap<u32, [u32; 2]>,
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
            tune_vbp: 0,
            tune_hs_offset: 152,
            tune_pll_divide: 1104,
            tune_video_bw: 15,
            tune_phase: 16,
            tune_full_line: false,
            tune_frame_skip: 0,
            source_profile: String::new(),
            rotate: 0,
            window: [0.22, 0.94, 0.07, 0.98],
            tube_time_based: true,

            mon: [1.0, 0.0, 1.0, 0.0],
            mon_bands: BTreeMap::new(),
            band_pll: BTreeMap::new(),
            modes: BTreeMap::new(),
            tube_aspect: 0.0,
            show_panel: true,
            netcheck_muted: false,
            bezel: String::new(),
            bezel_off: false,
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
        // Windows では XDG_CONFIG_HOME も HOME も無いことが多い。そのまま
        // フォールバックすると「.」= カレントディレクトリに作ってしまい、
        // 起動した場所ごとに設定が散らばる(実機のWindowsで確認)。
        // APPDATA(= C:\Users\<user>\AppData\Roaming)を先に見る。
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Roaming"))
            })
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
                "tune_vbp" => { if let Ok(x) = v.parse() { s.tune_vbp = x } }
                "tune_hs_offset" => { if let Ok(x) = v.parse() { s.tune_hs_offset = x } }
                "tune_pll_divide" => { if let Ok(x) = v.parse() { s.tune_pll_divide = x } }
                // 表示倍率まわりの設定は撤去した。表示の形は管面(tube_aspect と
                // モニタプロファイル)が決めるので、ドット数に対する倍率や整数倍の
                // 概念が要らない。枠線と外側の色も、幾何がGPU側へ移って
                // キャプチャ範囲ではなく管面の外周を指すようになったのでやめた。
                "integer_scale" | "show_border" | "backdrop" | "vscale" => {}
                // pll_divide の比例計算(target w)は撤去した。実測が信用できないと
                // 上限まで走るので、いまはスペクトルから求める「自動調整」を使う
                "tune_target_w" | "tune_snap8" => {}
                // インターレースの設定は撤去した(すべて測定から決まる)。
                // 古い設定ファイルにこれらの行が残っていても読み捨てる
                "tune_interlace" | "tune_f2_row" | "tune_field_swap"
                    | "tune_field_src" => {}
                "tune_video_bw" => { if let Ok(x) = v.parse() { s.tune_video_bw = x } }
                "tune_phase" => { if let Ok(x) = v.parse::<u8>() { s.tune_phase = x.min(31) } }
                "source_profile" => s.source_profile = v.to_string(),
                "tune_full_line" => s.tune_full_line = v == "true",
                "tune_frame_skip" => { if let Ok(x) = v.parse::<u8>() { s.tune_frame_skip = x.min(7) } }
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
                k if k.starts_with("bandpll.") => {
                    if let Ok(khz) = k[8..].parse::<u32>() {
                        let n: Vec<u32> =
                            v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        if n.len() >= 2 {
                            s.band_pll.insert(khz, [n[0], n[1].min(31)]);
                        }
                    }
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
                "bezel" => s.bezel = v.to_string(),
                "show_panel" => s.show_panel = v != "false",
                "netcheck_muted" => s.netcheck_muted = v == "true",
                "bezel_off" => s.bezel_off = v == "true",
                "filter" => { if let Ok(x) = v.parse::<u32>() { s.filter = x.min(2) } }
                "crop_x" => { if let Ok(x) = v.parse() { s.crop_x = x } }
                "crop_y" => { if let Ok(x) = v.parse() { s.crop_y = x } }
                "crop_w" => { if let Ok(x) = v.parse() { s.crop_w = x } }
                "crop_h" => { if let Ok(x) = v.parse() { s.crop_h = x } }
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
            "# RetroCast X settings (自動生成。消しても既定値で起動する)\n\
             volume = {:.3}\n\
             muted = {}\n\
             audio_source = {}\n\
             audio_device = {}\n\
             tune_vbp = {}\n\
             tune_hs_offset = {}\n\
             tune_pll_divide = {}\n\
             tune_video_bw = {}\n\
             tune_phase = {}\n\
             source_profile = {}\n\
             tune_full_line = {}\n\
             tune_frame_skip = {}\n\
             tube_time_based = {}\n\
             mon = {:.4},{:.4},{:.4},{:.4}\n\
             rotate = {}\n\
             window = {:.4},{:.4},{:.4},{:.4}\n\
             tube_aspect = {:.4}\n\
             bezel = {}\n\
             show_panel = {}\n\
             netcheck_muted = {}\n\
             bezel_off = {}\n\
             filter = {}\n\
             crop_x = {}\n\
             crop_y = {}\n\
             crop_w = {}\n\
             crop_h = {}\n\
             window_w = {:.0}\n\
             window_h = {:.0}\n",
            self.volume,
            self.muted,
            self.audio_source.map(|v| v.to_string()).unwrap_or_else(|| "off".into()),
            self.audio_device.clone().unwrap_or_default(),
            self.tune_vbp,
            self.tune_hs_offset,
            self.tune_pll_divide,
            self.tune_video_bw,
            self.tune_phase,
            self.source_profile,
            self.tune_full_line,
            self.tune_frame_skip,
            self.tube_time_based,
            self.mon[0], self.mon[1], self.mon[2], self.mon[3],
            self.rotate,
            self.window[0],
            self.window[1],
            self.window[2],
            self.window[3],
            self.tube_aspect,
            self.bezel,
            self.show_panel,
            self.netcheck_muted,
            self.bezel_off,
            self.filter,
            self.crop_x,
            self.crop_y,
            self.crop_w,
            self.crop_h,
            self.window_w,
            self.window_h,
        );
        let mut body = body;
        for (khz, v) in &self.band_pll {
            body.push_str(&format!("bandpll.{khz} = {},{}\n", v[0], v[1]));
        }
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
