//! RetroCastX viewer (scaffold).
//!
//! egui/eframe(wgpuバックエンド)でフレームを表示する。v0.1はegui標準の
//! テクスチャAPIで毎フレーム全面アップロード(512x512なら余裕)。
//! パレット吸着・CRTシェーダ・VRR present制御は、後段でwgpuの
//! paint callback(egui_wgpu::Callback)へ移行して実装する。
//!
//! Usage:
//!   cargo run --release -- [--board IP] [--mac AA:BB:..] [--port 34600] [--no-subscribe]
//!
//! --mac は購読対象ボードのMACを指名する(複数ボードLANで必須。省略時は
//! ワイルドカード=全ボード、単一ボードLAN専用)。
//!
//! 既定ではSUBSCRIBEを255.255.255.255にブロードキャストし、ボードの
//! ストリームを自分に向ける。sender_sim相手なら --no-subscribe でよい
//! (sender_simはSUBSCRIBEを無視して--dest宛に送るため)。

mod assembler;
mod audio;
mod fullscreen;
mod protocol;
mod receiver;
mod settings;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result {
    let mut port = protocol::DEFAULT_PORT;
    let mut subscribe_to = Some("255.255.255.255".to_string());
    let mut headless_secs: Option<u64> = None;
    let mut no_vsync = false;
    let mut fullscreen_mode = false;
    let mut target_mac: Option<[u8; 6]> = None;
    let mut decay = 0.8f32;
    // 保存済み設定を読み、CLI引数があればそれで上書きする(その回だけ有効)
    let mut cfg = settings::Settings::load();
    eprintln!("settings: {}", settings::Settings::path().display());
    let mut audio = receiver::AudioOpts::default();
    audio.source = cfg.audio_source;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--board" => subscribe_to = Some(args.next().expect("--board needs an IP")),
            "--mac" => target_mac = Some(parse_mac(&args.next().expect("--mac needs AA:BB:.."))),
            "--port" => port = args.next().expect("--port needs a value").parse().unwrap(),
            "--no-subscribe" => subscribe_to = None,
            "--headless" => {
                headless_secs = Some(args.next().expect("--headless needs seconds").parse().unwrap())
            }
            // VRR実験用: presentをvsyncから切り離す(wgpu AutoNoVsync)。
            // VRR対応パネルなら「フレームが来たときに出す」に近づく第一歩
            "--no-vsync" => no_vsync = true,
            // 低遅延フルスクリーン(専用presentスレッド+Immediate、ソース駆動present)
            "--fullscreen" => fullscreen_mode = true,
            // 欠損ライン減衰率(1.0=前フレーム保持のまま, 0.8=毎フレーム80%へ暗転して消える)
            "--decay" => {
                decay = args.next().expect("--decay needs a value (e.g. 0.8)").parse().unwrap()
            }
            // 音声: 再生するsource(0=RGB端子音声, 1=LINE入力, 2=S/PDIF)
            "--audio" => {
                audio.source =
                    Some(args.next().expect("--audio needs 0|1|2").parse().unwrap());
                cfg.audio_source = audio.source;
            }
            "--no-audio" => {
                audio.source = None;
                cfg.audio_source = None;
            }
            // 音量[%]。保存値より優先(その回だけ)
            "--volume" => {
                let pct: f32 = args.next().expect("--volume needs 0..150").parse().unwrap();
                cfg.volume = (pct / 100.0).clamp(0.0, 1.5);
            }
            // 音声バッファ[ms]。小さいほど低遅延だが枯渇(音切れ)しやすい
            "--audio-buffer" => {
                audio.prebuffer_ms =
                    args.next().expect("--audio-buffer needs ms").parse().unwrap();
                audio.max_ms = audio.prebuffer_ms * 3;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    if let Some(secs) = headless_secs {
        return run_headless(port, subscribe_to, target_mac, secs, decay, audio);
    }
    if fullscreen_mode {
        fullscreen::run(port, subscribe_to, target_mac, decay, audio); // 戻らない
    }

    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if no_vsync {
        wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
        wgpu_options.surface.desired_maximum_frame_latency = Some(1); // 低遅延優先
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([cfg.window_w, cfg.window_h])
            .with_title("RetroCastX Viewer"),
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        "RetroCastX Viewer",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ViewerApp::new(cc, port, subscribe_to, target_mac, no_vsync, decay,
                                       audio, cfg.clone())))
        }),
    )
}


/// CJKを含むシステムフォントをeguiへ追加する。
/// 出力デバイス名はOS由来で日本語を含むことがあり、egui既定フォントにはCJKの
/// グリフが無いため豆腐(□)になる。フォントは同梱せず、実行環境のものを使う。
fn install_cjk_font(ctx: &egui::Context) {
    // 単一フォント(.ttf/.otf)のみ。.ttc はコレクションでab_glyphが扱えない
    const CANDIDATES: &[&str] = &[
        // macOS
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        // Linux (Noto/VLGothic)
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttf",
        "/usr/share/fonts/truetype/vlgothic/VL-Gothic-Regular.ttf",
        // Windows
        "C:/Windows/Fonts/meiryo.ttc",
        "C:/Windows/Fonts/msgothic.ttc",
        "C:/Windows/Fonts/YuGothR.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        // 既定フォントの後ろに足す(ASCIIは既定の見た目を保ち、CJKだけ補完)
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
    eprintln!("font: CJK対応フォントが見つからないため日本語が豆腐になります");
}

/// "AA:BB:CC:DD:EE:FF" → [u8; 6](区切りは ':' or '-')。
fn parse_mac(s: &str) -> [u8; 6] {
    let parts: Vec<&str> = s.split([':', '-']).collect();
    assert_eq!(parts.len(), 6, "MAC must have 6 octets: {s}");
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).expect("bad MAC octet");
    }
    mac
}

/// GUIなしで受信パイプラインだけ動かし、毎秒統計を出力する(検証・CI用)。
fn run_headless(
    port: u16,
    subscribe_to: Option<String>,
    target_mac: Option<[u8; 6]>,
    secs: u64,
    decay: f32,
    audio: receiver::AudioOpts,
) -> eframe::Result {
    let shared = Arc::new(receiver::Shared::default());
    receiver::spawn(
        receiver::Config { port, subscribe_to, target_mac, decay, audio },
        shared.clone(),
        || {},
    )
    .expect("UDP bind failed");
    for _ in 0..secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let s = shared.stats.lock().unwrap().clone();
        let mode = shared.mode.lock().unwrap().clone();
        let dims = mode
            .map(|m| format!("{}x{}", m.hactive, m.vactive))
            .unwrap_or_else(|| "no mode".into());
        println!(
            "{dims}  {:.1} fps  {:.1} Mbps  frames={} pkts={} lost={} orphan={}",
            s.fps, s.mbps, s.frames, s.packets, s.lost_packets, s.orphan_lines
        );
    }
    let s = shared.stats.lock().unwrap().clone();
    shared.stop.store(true, Ordering::Relaxed);
    if s.frames == 0 {
        eprintln!("FAIL: no frames received");
        std::process::exit(1);
    }
    println!("OK: {} frames, {} lost packets", s.frames, s.lost_packets);
    Ok(())
}

/// 新フレームpaint間隔の計測(vsync量子化の検出用)。
/// present完了時刻そのものはwgpu経由では取れないため、paint cadenceを代理指標にする
/// (FIFOではスワップチェーンのバックプレッシャでvsync周期に量子化される)。
struct PaceMeter {
    last_paint: Option<std::time::Instant>,
    intervals_ms: std::collections::VecDeque<f32>,
    last_log: std::time::Instant,
    summary: String,
}

impl PaceMeter {
    fn new() -> Self {
        Self {
            last_paint: None,
            intervals_ms: std::collections::VecDeque::with_capacity(256),
            last_log: std::time::Instant::now(),
            summary: String::new(),
        }
    }

    fn on_new_frame(&mut self) {
        let now = std::time::Instant::now();
        if let Some(prev) = self.last_paint {
            let ms = prev.elapsed().as_secs_f32() * 1000.0;
            if self.intervals_ms.len() >= 256 {
                self.intervals_ms.pop_front();
            }
            self.intervals_ms.push_back(ms);
        }
        self.last_paint = Some(now);
        if self.last_log.elapsed().as_secs_f32() >= 1.0 && !self.intervals_ms.is_empty() {
            let n = self.intervals_ms.len() as f32;
            let mean = self.intervals_ms.iter().sum::<f32>() / n;
            let var = self.intervals_ms.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
            self.summary = format!(
                "paint {:.2}ms σ{:.2} → {:.2}Hz",
                mean,
                var.sqrt(),
                1000.0 / mean
            );
            eprintln!("pace: {}", self.summary);
            self.last_log = std::time::Instant::now();
        }
    }
}

/// 実測タイミングから「まともな映像が来ているか」を判定する。
/// 無信号だとTVPのH-PLLがロックを失い、DATACLKが低速で自走してADCが浮いた入力の
/// ノイズを拾うため、砂嵐が表示される。実測値が現実的な範囲を外れたら無信号とみなす。
fn signal_is_valid(m: &protocol::Mode) -> bool {
    let fh_hz = m.hfreq_mhz_x1000 as f64 / 1000.0;
    // 15kHz系(≒15.98k)〜31kHz系(≒31.5k)を含む余裕のある範囲
    (12_000.0..80_000.0).contains(&fh_hz)
        && (100..1500).contains(&m.vtotal)
        && m.dotclk_hz > 5_000_000
}

/// キャプチャ範囲の外側に塗る色。取り込んだ絵が黒いと画枠が分からないため。
/// インターレースの織り込み方を自動判定している最中の状態
struct WeaveAuto {
    /// 試す (f2_row, swap) の並び
    /// (方式, f2_row, swap, 極性の取得元)
    cands: Vec<(u8, i32, bool, u8)>,
    idx: usize,
    /// 現在の候補を送った時刻。None ならまだ送っていない
    started: Option<std::time::Instant>,
    sum: f64,
    n: u32,
    /// 取り込んだ最後の測定番号(同じ値を二重に足さないため)
    last_seen: u64,
    results: Vec<(f64, (u8, i32, bool, u8))>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backdrop {
    Black,
    DarkGray,
    Magenta,
}

impl Backdrop {
    fn color(self) -> egui::Color32 {
        match self {
            Backdrop::Black => egui::Color32::BLACK,
            Backdrop::DarkGray => egui::Color32::from_gray(40),
            // 映像には出にくい色。画枠を厳密に見たいとき用
            Backdrop::Magenta => egui::Color32::from_rgb(120, 0, 120),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Backdrop::Black => "black",
            Backdrop::DarkGray => "dark gray",
            Backdrop::Magenta => "magenta",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "black" => Backdrop::Black,
            "magenta" => Backdrop::Magenta,
            _ => Backdrop::DarkGray,
        }
    }
}

struct ViewerApp {
    shared: Arc<receiver::Shared>,
    /// 再生中の音声source(UI表示用)
    audio_source: Option<u8>,
    /// 出力デバイス一覧(起動時に列挙。再列挙ボタンで更新)
    audio_devices: Vec<String>,
    /// UIで選択中のデバイス。None は「システム既定」
    audio_device_sel: Option<String>,
    /// 音量(0.0..1.5、1.0=原音)。ミュート時も値は保持してトグルで戻せるようにする
    audio_volume: f32,
    audio_muted: bool,
    /// 設定の保存(スライダー操作中に毎フレーム書かないよう、変更後に少し待って書く)
    settings_dirty: Option<std::time::Instant>,
    /// キャプチャ範囲の外側の色と、境界の枠線表示
    backdrop: Backdrop,
    show_border: bool,
    /// 画枠パラメータ(ボードへCONFIGで送る値)。モードごとに最適値が違うので、
    /// ビルドし直さずにここで追い込む。
    tune_vbp: i32,
    tune_hs_offset: i32,
    tune_pll_divide: i32,
    /// 目標の有効幅[ドット](そのモードの水平解像度)。pll_div の推奨値算出に使う
    tune_target_w: i32,
    /// 推奨値を8の倍数に丸める(X68000のhtotalは8ドット単位)
    tune_snap8: bool,
    /// インターレース方式。0=なし / 1=1VSYNCに2フィールド / 2=フィールド毎VSYNC
    tune_interlace: u8,
    /// 第2フィールドが始まる row(0=vtotal/2)
    tune_f2_row: i32,
    /// フィールドの偶奇を入れ替える
    tune_field_swap: bool,
    /// 方式2の極性の取得元。0=位相 / 1=FIDOUT
    tune_field_src: u8,
    /// 送信したがまだボードの応答で確認できていない値(key → (値, 送信時刻))。
    /// 応答が返るまでは自分の値を優先し、押した瞬間に元へ戻って見えるのを防ぐ。
    tune_pending: std::collections::HashMap<u16, (u32, std::time::Instant)>,
    /// 次に現在値を問い合わせる時刻。全キーが揃うまで繰り返す
    tune_get_at: Option<std::time::Instant>,
    /// インターレースの織り込み方を自動判定している最中の状態
    weave_auto: Option<WeaveAuto>,
    /// 現在のウィンドウ内寸(保存用)
    window_size: egui::Vec2,
    /// 中央の描画領域と画のサイズ。ウィンドウを等倍に合わせるのに使う
    last_avail: egui::Vec2,
    last_tex: egui::Vec2,
    /// 起動後に一度だけ自動で等倍に合わせたか
    did_autofit: bool,
    /// 次のフレームでウィンドウを等倍に合わせる
    want_fit: bool,
    texture: Option<egui::TextureHandle>,
    seen_gen: u64,
    integer_scale: bool,
    /// 表示時の縦倍率(ドットが正方形でないモードの縦つぶれを直す)
    vscale: f32,
    rx_error: Option<String>,
    subscribe_to: Option<String>,
    no_vsync: bool,
    pace: PaceMeter,
}

impl ViewerApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        port: u16,
        subscribe_to: Option<String>,
        target_mac: Option<[u8; 6]>,
        no_vsync: bool,
        decay: f32,
        audio: receiver::AudioOpts,
        cfg: settings::Settings,
    ) -> Self {
        let audio_source = audio.source;
        install_cjk_font(&cc.egui_ctx);
        let shared = Arc::new(receiver::Shared::default());
        let ctx = cc.egui_ctx.clone();
        let rx_error = receiver::spawn(
            receiver::Config { port, subscribe_to: subscribe_to.clone(), target_mac, decay, audio },
            shared.clone(),
            move || ctx.request_repaint(),
        )
        .err()
        .map(|e| format!("UDP {port} bind failed: {e}"));
        Self {
            shared,
            audio_source,
            audio_devices: audio::output_devices(),
            audio_device_sel: cfg.audio_device.clone(),
            audio_volume: cfg.volume,
            audio_muted: cfg.muted,
            settings_dirty: None,
            backdrop: Backdrop::from_str(&cfg.backdrop),
            show_border: cfg.show_border,
            tune_vbp: cfg.tune_vbp,
            tune_hs_offset: cfg.tune_hs_offset,
            tune_pll_divide: cfg.tune_pll_divide,
            tune_target_w: cfg.tune_target_w,
            tune_snap8: cfg.tune_snap8,
            tune_interlace: cfg.tune_interlace,
            tune_f2_row: cfg.tune_f2_row,
            tune_field_swap: cfg.tune_field_swap,
            tune_field_src: cfg.tune_field_src,
            tune_pending: Default::default(),
            tune_get_at: None,
            weave_auto: None,
            window_size: egui::vec2(cfg.window_w, cfg.window_h),
            last_avail: egui::Vec2::ZERO,
            last_tex: egui::Vec2::ZERO,
            did_autofit: false,
            want_fit: false,
            texture: None,
            seen_gen: 0,
            integer_scale: cfg.integer_scale,
            vscale: cfg.vscale,
            rx_error,
            subscribe_to,
            no_vsync,
            pace: PaceMeter::new(),
        }
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let generation = self.shared.frame_gen.load(Ordering::Acquire);
        if generation == self.seen_gen {
            return;
        }
        self.seen_gen = generation;
        self.pace.on_new_frame();
        let guard = self.shared.frame.lock().unwrap();
        let Some(frame) = guard.as_ref() else { return };
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width, frame.height],
            &frame.rgba,
        );
        // レトロ画面はニアレスト(整数拡大でドットがくっきり)
        let opts = egui::TextureOptions::NEAREST;
        match &mut self.texture {
            Some(t) => t.set(image, opts),
            None => self.texture = Some(ctx.load_texture("video", image, opts)),
        }
    }
}

impl ViewerApp {
    /// 現在のUI状態を設定として書き出す。スライダー操作中に毎フレーム書くのを
    /// 避けるため、変更を記録して少し待ってから1回だけ保存する。
    fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(std::time::Instant::now());
    }

    fn flush_settings(&mut self) {
        let due = self
            .settings_dirty
            .map_or(false, |t| t.elapsed() >= std::time::Duration::from_millis(700));
        if !due {
            return;
        }
        self.settings_dirty = None;
        settings::Settings {
            volume: self.audio_volume,
            muted: self.audio_muted,
            audio_source: self.audio_source,
            audio_device: self.audio_device_sel.clone(),
            integer_scale: self.integer_scale,
            vscale: self.vscale,
            window_w: self.window_size.x,
            window_h: self.window_size.y,
            backdrop: self.backdrop.label().to_string(),
            show_border: self.show_border,
            tune_vbp: self.tune_vbp,
            tune_hs_offset: self.tune_hs_offset,
            tune_pll_divide: self.tune_pll_divide,
            tune_target_w: self.tune_target_w,
            tune_snap8: self.tune_snap8,
            tune_interlace: self.tune_interlace,
            tune_f2_row: self.tune_f2_row,
            tune_field_swap: self.tune_field_swap,
            tune_field_src: self.tune_field_src,
        }
        .save();
    }

    /// 音声の状態表示 + 出力デバイス/source の選択UI。
    /// cpalのStreamは生成スレッドから動かせないので、選択は「要求」として置き、
    /// 受信スレッドが再生器を作り直す。
    fn audio_ui(&mut self, ui: &mut egui::Ui) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = self.shared.audio_now.lock().unwrap().clone();
        let astats = self.shared.audio.lock().unwrap().clone();

        let mut request: Option<receiver::AudioRequest> = None;

        // 音声source(0=RGB端子, 1=LINE入力, 2=S/PDIF)。OFFで停止。
        let mut src = self.audio_source;
        ui.horizontal(|ui| {
            ui.label("src");
            let label = match src {
                None => "off".to_string(),
                Some(0) => "0 RGB".to_string(),
                Some(1) => "1 LINE".to_string(),
                Some(2) => "2 S/PDIF".to_string(),
                Some(n) => n.to_string(),
            };
            egui::ComboBox::from_id_salt("audio_src")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut src, None, "off");
                    ui.selectable_value(&mut src, Some(0), "0 RGB (D-SUB15)");
                    ui.selectable_value(&mut src, Some(1), "1 LINE in");
                    ui.selectable_value(&mut src, Some(2), "2 S/PDIF");
                });
        });
        if src != self.audio_source {
            self.audio_source = src;
            self.mark_settings_dirty();
            request = Some(receiver::AudioRequest {
                device: self.audio_device_sel.clone(),
                source: src,
            });
        }

        // 出力デバイス
        let mut dev = self.audio_device_sel.clone();
        ui.horizontal(|ui| {
            ui.label("out");
            let sel_text = dev.clone().unwrap_or_else(|| "(system default)".to_string());
            egui::ComboBox::from_id_salt("audio_dev")
                .selected_text(sel_text)
                .width(180.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut dev, None, "(system default)");
                    for d in &self.audio_devices {
                        ui.selectable_value(&mut dev, Some(d.clone()), d);
                    }
                });
            if ui.small_button("⟳").on_hover_text("rescan devices").clicked() {
                self.audio_devices = audio::output_devices();
            }
        });
        if dev != self.audio_device_sel {
            self.audio_device_sel = dev.clone();
            self.mark_settings_dirty();
            request = Some(receiver::AudioRequest { device: dev, source: self.audio_source });
        }

        if let Some(req) = request {
            *self.shared.audio_request.lock().unwrap() = Some(req);
        }

        // 音量。ミュートは値を保持したままゲインだけ0にする
        let (vol0, mute0) = (self.audio_volume, self.audio_muted);
        ui.horizontal(|ui| {
            let icon = if self.audio_muted { "🔇" } else { "🔊" };
            if ui.button(icon).on_hover_text("mute").clicked() {
                self.audio_muted = !self.audio_muted;
            }
            ui.spacing_mut().slider_width = 150.0;
            ui.add(
                egui::Slider::new(&mut self.audio_volume, 0.0..=1.5)
                    .show_value(false),
            );
            ui.monospace(format!("{:3.0}%", self.audio_volume * 100.0));
        });
        if self.audio_volume != vol0 || self.audio_muted != mute0 {
            self.mark_settings_dirty();
        }
        if let Some(a) = &astats {
            let g = if self.audio_muted { 0.0 } else { self.audio_volume };
            audio::AudioPlayer::set_gain(a, g);
        }

        match astats {
            Some(a) => {
                let rate = a.device_rate.load(Relaxed).max(1);
                ui.monospace(format!(
                    "{}  {} Hz",
                    if a.playing.load(Relaxed) { "playing" } else { "buffering" },
                    rate
                ));
                if !now.device.is_empty() {
                    ui.monospace(format!("dev {}", now.device));
                }
                ui.monospace(format!(
                    "buffered {:.0} ms  pkts {}",
                    a.buffered.load(Relaxed) as f64 * 1000.0 / rate as f64,
                    a.packets.load(Relaxed)
                ));
                ui.monospace(format!(
                    "underruns {}  dropped {}",
                    a.underruns.load(Relaxed),
                    a.dropped.load(Relaxed)
                ));
            }
            None => {
                ui.monospace("stopped");
            }
        }
    }
}

impl ViewerApp {
    /// 画枠パラメータをボードへ即時反映するUI。モードごとに最適値が違い、
    /// ビルドし直していては追い込めないのでCONFIGパケットで実行時に送る。
    /// 織り込み方の自動判定を開始する。
    ///
    /// f2_row を1増やすのと swap を反転するのはフィールドの相対位置に対して
    /// 打ち消し合うので、4通りのうち (half,swap=1) と (half+1,swap=0) は同じ
    /// 配置になる。したがって試すべきは3通りで足りる。
    /// f2_row は half の代わりに0(=vtotal/2を自動で使う)を送る。モードが
    /// 変わっても追従させるため。
    fn weave_auto_start(&mut self, vtotal: u16) {
        let half = (vtotal / 2) as i32;
        // (方式, f2_row, swap, 極性の取得元)
        // 方式1: f2_row を1増やすのと swap の反転は打ち消し合うので3通りで足りる。
        // 方式2: 折り返し点は無く、極性の取得元(位相/FIDOUT)と swap の4通り。
        let cands = vec![
            (1u8, 0, false, 0u8),
            (1, 0, true, 0),
            (1, half + 1, true, 0),
            (2, 0, false, 0),
            (2, 0, true, 0),
            (2, 0, false, 1),
            (2, 0, true, 1),
        ];
        self.weave_auto = Some(WeaveAuto {
            cands,
            idx: 0,
            started: None,
            sum: 0.0,
            n: 0,
            last_seen: 0,
            results: Vec::new(),
        });
    }

    /// CONFIGを1件送る(送信済みとして pending に記録する)
    fn push_cfg(&mut self, key: u16, value: u32) {
        self.tune_pending
            .insert(key, (value, std::time::Instant::now()));
        self.shared.config_queue.lock().unwrap().push((key, value));
    }

    /// 自動判定を1フレーム分進める。候補ごとに「落ち着き待ち」→「測定」と進む。
    fn weave_auto_step(&mut self) {
        let Some(a) = self.weave_auto.as_mut() else { return };
        let now = std::time::Instant::now();
        let Some(started) = a.started else {
            // この候補を送って計測開始
            let (il, f2, sw, src) = a.cands[a.idx];
            a.started = Some(now);
            a.sum = 0.0;
            a.n = 0;
            a.last_seen = 0;
            self.push_cfg(protocol::CFG_KEY_INTERLACE, il as u32);
            self.push_cfg(protocol::CFG_KEY_F2_ROW, f2 as u32);
            self.push_cfg(protocol::CFG_KEY_FIELD_SWAP, sw as u32);
            self.push_cfg(protocol::CFG_KEY_FIELD_SRC, src as u32);
            return;
        };
        let el = now.duration_since(started);
        // 設定が効くまで待ってから測る。1つの候補に約2.2秒かける
        if el >= std::time::Duration::from_millis(900) {
            let st = self.shared.stats.lock().unwrap().clone();
            let a = self.weave_auto.as_mut().unwrap();
            if st.weave_n != a.last_seen && st.weave_err > 0.0 {
                a.last_seen = st.weave_n;
                a.sum += st.weave_err as f64;
                a.n += 1;
            }
        }
        let a = self.weave_auto.as_mut().unwrap();
        if el < std::time::Duration::from_millis(2200) {
            return;
        }
        let c = a.cands[a.idx];
        if a.n > 0 {
            a.results.push((a.sum / a.n as f64, c));
        }
        a.idx += 1;
        a.started = None;
        if a.idx < a.cands.len() {
            return;
        }
        // 全候補おわり: 最良を適用する
        let mut res = std::mem::take(&mut a.results);
        self.weave_auto = None;
        if res.is_empty() {
            eprintln!("weave auto: 測定できませんでした");
            return;
        }
        res.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
        for (e, (il, f2, sw, src)) in &res {
            eprintln!(
                "weave auto: 方式{il} f2_row={f2} swap={} 極性={} ずれ {e:.3}",
                *sw as u8,
                if *src == 1 { "FID" } else { "位相" }
            );
        }
        let (best, (il, f2, sw, src)) = res[0];
        let ratio = res[res.len() - 1].0 / best.max(1e-6);
        eprintln!("weave auto: 採用 方式{il} (最大との比 {ratio:.2}倍)");
        self.tune_interlace = il;
        self.tune_f2_row = f2;
        self.tune_field_swap = sw;
        self.tune_field_src = src;
        self.push_cfg(protocol::CFG_KEY_INTERLACE, il as u32);
        self.push_cfg(protocol::CFG_KEY_F2_ROW, f2 as u32);
        self.push_cfg(protocol::CFG_KEY_FIELD_SWAP, sw as u32);
        self.push_cfg(protocol::CFG_KEY_FIELD_SRC, src as u32);
    }

    /// 実行時に調整できるキーの一覧。読み戻しと表示の同期に使う
    const TUNE_KEYS: [u16; 7] = [
        protocol::CFG_KEY_VBP,
        protocol::CFG_KEY_HS_OFFSET,
        protocol::CFG_KEY_PLL_DIVIDE,
        protocol::CFG_KEY_INTERLACE,
        protocol::CFG_KEY_F2_ROW,
        protocol::CFG_KEY_FIELD_SWAP,
        protocol::CFG_KEY_FIELD_SRC,
    ];

    /// ボードの現在値を表示へ反映する。
    ///
    /// 読み戻さないと表示と実体が食い違う。実際、ボード側で swap が有効なのに
    /// チェックボックスは外れたまま、という状態が起きた(設定はボードの電源で
    /// 消え、Viewerの保存値とは別々に動くため)。送信パケットが落ちたときも
    /// ずれるので、応答で確認できた値を正としてUIを合わせる。
    fn sync_tune_from_board(&mut self) {
        let now = std::time::Instant::now();
        let state = self.shared.config_state.lock().unwrap().clone();
        // 未取得のキーがあるうちは定期的に問い合わせる
        if Self::TUNE_KEYS.iter().any(|k| !state.contains_key(k))
            && self.tune_get_at.map_or(true, |t| now >= t)
        {
            self.tune_get_at = Some(now + std::time::Duration::from_secs(2));
            self.shared
                .config_get_queue
                .lock()
                .unwrap()
                .extend(Self::TUNE_KEYS);
        }
        for (key, val) in state {
            // 送信直後は応答が返るまで自分の値を優先する。応答が一致したら解除、
            // 一定時間返らなければ諦めてボードの値に合わせる(送信が落ちた場合)
            if let Some((want, at)) = self.tune_pending.get(&key) {
                if *want == val {
                    self.tune_pending.remove(&key);
                } else if at.elapsed() < std::time::Duration::from_millis(1500) {
                    continue;
                } else {
                    self.tune_pending.remove(&key);
                }
            }
            match key {
                protocol::CFG_KEY_VBP => self.tune_vbp = val as i32,
                protocol::CFG_KEY_HS_OFFSET => self.tune_hs_offset = val as i32,
                protocol::CFG_KEY_PLL_DIVIDE => self.tune_pll_divide = val as i32,
                protocol::CFG_KEY_F2_ROW => self.tune_f2_row = val as i32,
                protocol::CFG_KEY_INTERLACE => self.tune_interlace = val as u8,
                protocol::CFG_KEY_FIELD_SWAP => self.tune_field_swap = val != 0,
                protocol::CFG_KEY_FIELD_SRC => self.tune_field_src = val as u8,
                _ => {}
            }
        }
    }

    fn tune_ui(&mut self, ui: &mut egui::Ui) {
        self.sync_tune_from_board();
        self.weave_auto_step();
        let vtotal = self.shared.mode.lock().unwrap().as_ref().map(|m| m.vtotal);
        let mut send: Vec<(u16, u32)> = Vec::new();
        let mut row = |ui: &mut egui::Ui, label: &str, val: &mut i32,
                       lo: i32, hi: i32, key: u16, send: &mut Vec<(u16, u32)>| {
            ui.horizontal(|ui| {
                ui.monospace(format!("{label:<10}"));
                if ui.small_button("-").clicked() {
                    *val = (*val - 1).max(lo);
                    send.push((key, *val as u32));
                }
                ui.add(
                    egui::DragValue::new(val)
                        .range(lo..=hi)
                        .speed(1.0),
                );
                if ui.small_button("+").clicked() {
                    *val = (*val + 1).min(hi);
                    send.push((key, *val as u32));
                }
                if ui.small_button("send").clicked() {
                    send.push((key, *val as u32));
                }
            });
        };
        row(ui, "vbp", &mut self.tune_vbp, 0, 400,
            protocol::CFG_KEY_VBP, &mut send);
        // 上限は pll_div の半分。これを超えるとキャプチャ窓がラインの後半から
        // 始まり、有効映像を取り逃がす(等号だと1画素も取り込めない)
        let hs_max = (self.tune_pll_divide / 2).max(1);
        row(ui, "hs_offset", &mut self.tune_hs_offset, 0, hs_max,
            protocol::CFG_KEY_HS_OFFSET, &mut send);
        // 上限はgateware側の制限と同じ。これを超えるとDATACLKがpixドメインの
        // タイミング制約を超え、実機ではボードごとハングした(4095で14秒)
        row(ui, "pll_div", &mut self.tune_pll_divide, 200, 2304,
            protocol::CFG_KEY_PLL_DIVIDE, &mut send);
        // 実測した有効映像の大きさと、そこから求まる pll_div の推奨値。
        // pll_div は「1ラインを何サンプルで取るか」なので、有効幅は pll_div に比例する。
        // 目標のドット数になるよう比例計算すれば一発で決まる(勘で動かす必要はない)。
        let st = self.shared.stats.lock().unwrap().clone();
        ui.monospace(format!(
            "active {}x{} at ({},{})",
            st.active_w, st.active_h, st.active_x, st.active_y
        ));
        if st.active_w > 0 {
            let r = self.tune_target_w as f32 / st.active_w as f32;
            if !(0.75..=1.35).contains(&r) {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 170, 60),
                    format!("target w が active({}) とかけ離れています", st.active_w),
                );
            }
        }
        ui.horizontal(|ui| {
            ui.monospace("target w  ");
            ui.add(egui::DragValue::new(&mut self.tune_target_w).range(64..=2048));
            // X68000のCRTCは水平トータルを8ドット単位で持つので、正解は必ず
            // 8の倍数になる。丸めておくと実測の端数に引きずられない
            // (24kHz 1024x424 で 1407.7 → 1408 = 176×8)。他機種で外したい
            // ことがあるかもしれないので切れるようにしておく。
            ui.checkbox(&mut self.tune_snap8, "×8");
            if st.active_w > 0 {
                let raw = self.tune_pll_divide as f64 * self.tune_target_w as f64
                    / st.active_w as f64;
                let want = if self.tune_snap8 {
                    ((raw / 8.0).round() * 8.0) as i32
                } else {
                    raw.round() as i32
                }
                // 測定が壊れていると推奨値が青天井に走る。実際、連打してDATACLKが
                // 100MHzを超え、ボードがハングした。1回の変化は2倍までに抑え、
                // 上限もgateware側と揃える。
                .clamp(
                    (self.tune_pll_divide / 2).max(200),
                    (self.tune_pll_divide * 2).min(2304),
                );
                if ui.button(format!("→ pll {want}")).clicked() {
                    let old = self.tune_pll_divide.max(1);
                    self.tune_pll_divide = want.clamp(200, 4095);
                    // pll_divに比例するのはHSYNCからの絶対位置、つまり
                    // (画の左端x + hs_offset)。xを動かさずに幅だけ変えるには、
                    // この和を比例させてからxを引き戻す。hs_offset単体を比例させる
                    // のでは足りない(hs_offsetが小さいほど補正不足になる)。
                    let x = st.active_x as f64;
                    // hs_offset が pll_div に近付くとキャプチャ窓がライン終端から
                    // 始まり、1画素も取り込めなくなる(実際に hs_offset=pll_div=1560
                    // になって映像が壊れた)。窓の先頭は必ずラインの手前側に置く。
                    let limit = (self.tune_pll_divide / 2).max(1);
                    self.tune_hs_offset = (((x + self.tune_hs_offset as f64)
                        * self.tune_pll_divide as f64 / old as f64)
                        - x)
                        .round()
                        .clamp(0.0, limit as f64) as i32;
                    send.push((protocol::CFG_KEY_PLL_DIVIDE, self.tune_pll_divide as u32));
                    send.push((protocol::CFG_KEY_HS_OFFSET, self.tune_hs_offset as u32));
                }
            }
        });
        // インターレース(ウィーブ)。X68000の 24kHz 1024x848 のように、VSYNCが
        // フレームに1回しか来ずその中にフィールドが2枚入る信号で使う。
        // 見分け方: 同じfHのままvtotalが約2倍かつ奇数になり、fVが半分になる。
        ui.horizontal(|ui| {
            // 0=なし / 1=1つのVSYNCにフィールドが2枚(24kHz 1024x848) /
            // 2=フィールドごとにVSYNC(15kHz 512x512。標準的なインターレース)
            let mut il = self.tune_interlace;
            ui.monospace("il");
            egui::ComboBox::from_id_salt("interlace")
                .width(96.0)
                .selected_text(match il {
                    1 => "1VS/2枚",
                    2 => "VS/枚",
                    _ => "なし",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut il, 0, "なし");
                    ui.selectable_value(&mut il, 1, "1VS/2枚");
                    ui.selectable_value(&mut il, 2, "VS/枚");
                });
            if il != self.tune_interlace {
                self.tune_interlace = il;
                send.push((protocol::CFG_KEY_INTERLACE, il as u32));
                // 折り返し点は既定(vtotal/2)に戻す。モードを変えると前のモードの
                // 値が残って絵が割れるため。
                self.tune_f2_row = 0;
                send.push((protocol::CFG_KEY_F2_ROW, 0));
            }
            if self.tune_interlace != 0 {
                // どちらのフィールドが偶数ラインを描いているかは信号から判別
                // できない。取り違えると1行ずつ食い違い、斜め線や円がギザギザ
                // に見えるので、見ながら切り替えられるようにしておく。
                if ui.checkbox(&mut self.tune_field_swap, "swap").changed() {
                    send.push((protocol::CFG_KEY_FIELD_SWAP,
                               self.tune_field_swap as u32));
                }
                if self.weave_auto.is_some() {
                    let a = self.weave_auto.as_ref().unwrap();
                    ui.monospace(format!("auto {}/{}", a.idx + 1, a.cands.len()));
                } else if ui.button("auto").clicked() {
                    if let Some(vt) = vtotal {
                        self.weave_auto_start(vt);
                    }
                }
                if self.tune_interlace == 2 {
                    // 方式2の極性の取得元。位相判定とFIDOUTのどちらが正しく出るかは
                    // 実機で確かめる
                    let mut src = self.tune_field_src;
                    egui::ComboBox::from_id_salt("field_src")
                        .width(72.0)
                        .selected_text(if src == 1 { "FID" } else { "位相" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut src, 0, "位相");
                            ui.selectable_value(&mut src, 1, "FID");
                        });
                    if src != self.tune_field_src {
                        self.tune_field_src = src;
                        send.push((protocol::CFG_KEY_FIELD_SRC, src as u32));
                    }
                    return;
                }
                ui.monospace("f2row");
                // 半ライン分のずれで1行ぶん食い違うことがあるので手で詰められる。
                // 0 = vtotal/2 を自動で使う
                if ui.add(egui::DragValue::new(&mut self.tune_f2_row).range(0..=4095))
                    .changed()
                {
                    send.push((protocol::CFG_KEY_F2_ROW, self.tune_f2_row as u32));
                }
            }
        });
        if ui.button("send all").clicked() {
            send.push((protocol::CFG_KEY_VBP, self.tune_vbp as u32));
            send.push((protocol::CFG_KEY_HS_OFFSET, self.tune_hs_offset as u32));
            send.push((protocol::CFG_KEY_PLL_DIVIDE, self.tune_pll_divide as u32));
        }
        if !send.is_empty() {
            let now = std::time::Instant::now();
            for (k, v) in &send {
                self.tune_pending.insert(*k, (*v, now));
            }
            self.shared.config_queue.lock().unwrap().extend(send);
            self.mark_settings_dirty();
        }
    }
}

impl eframe::App for ViewerApp {
    /// 終了時にも保存する(遅延書込の待ち時間中に閉じても取りこぼさない)
    fn on_exit(&mut self) {
        // 遅延を無視して即座に書く
        self.settings_dirty = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        self.flush_settings();
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.refresh_texture(&ctx);
        // ウィンドウ内寸を覚えておき、次回起動時に復元する
        if let Some(r) = ctx.input(|i| i.viewport().inner_rect) {
            let sz = r.size();
            if (sz - self.window_size).length() > 1.0 {
                self.window_size = sz;
                self.mark_settings_dirty();
            }
        }
        // 変更があれば少し待って1回だけ保存する(スライダー操作中の連続書込を避ける)
        self.flush_settings();

        egui::Panel::right(egui::Id::new("info")).show(root, |ui| {
            ui.heading("RetroCastX");
            if let Some(err) = &self.rx_error {
                ui.colored_label(egui::Color32::RED, err);
            }
            match &self.subscribe_to {
                Some(d) => ui.label(format!("SUBSCRIBE → {d}")),
                None => ui.label("subscribe: off (listen only)"),
            };
            ui.separator();

            ui.strong("Mode");
            if let Some(m) = self.shared.mode.lock().unwrap().clone() {
                ui.monospace(format!("{}x{} (id {})", m.hactive, m.vactive, m.mode_id));
                ui.monospace(format!("pixfmt {}", m.pixfmt));
                // htotal×vtotal もボードの実測値。モード表を作る調査で必要なので出す
                ui.monospace(format!("total {}x{}", m.htotal, m.vtotal));
                ui.monospace(format!("dotclk {:.4} MHz", m.dotclk_hz as f64 / 1e6));
                ui.monospace(format!("h {:.3} kHz", m.hfreq_mhz_x1000 as f64 / 1e6));
                ui.monospace(format!("v {:.3} Hz", m.vfreq_mhz_x1000 as f64 / 1e3));
            } else {
                ui.weak("no mode yet");
            }
            ui.separator();

            ui.strong("Stats");
            ui.label(if self.no_vsync { "present: no-vsync" } else { "present: vsync (FIFO)" });
            if !self.pace.summary.is_empty() {
                ui.monospace(&self.pace.summary);
            }
            let s = self.shared.stats.lock().unwrap().clone();
            ui.monospace(format!("{:.1} fps  {:.1} Mbps", s.fps, s.mbps));
            ui.monospace(format!("frames {}", s.frames));
            ui.monospace(format!("pkts {}  lost {}", s.packets, s.lost_packets));
            ui.monospace(format!("orphan lines {}", s.orphan_lines));
            // 暗部のフレーム間差分=点状ノイズの量。配線やパスコンの効果を数値で見る
            ui.monospace(format!(
                "noise {:.1}%  lvl {:.1}",
                s.noise_flicker, s.noise_level
            ));
            ui.separator();

            ui.strong("Audio");
            self.audio_ui(ui);
            ui.separator();

            ui.strong("Tune");
            self.tune_ui(ui);
            ui.separator();

            ui.strong("Boards");
            let boards = self.shared.boards.lock().unwrap();
            if boards.is_empty() {
                ui.weak("none discovered");
            }
            for b in boards.values() {
                let mac = b.mac.map(|x| format!("{x:02x}")).join(":");
                ui.monospace(format!("{} {}", b.addr, b.name));
                ui.weak(format!("  {mac} fw {:04x}", b.fw_version));
            }
            drop(boards);
            ui.separator();

            ui.horizontal(|ui| {
                ui.monospace("縦倍率");
                if ui
                    .add(egui::DragValue::new(&mut self.vscale).speed(0.01).range(0.25..=4.0))
                    .changed()
                {
                    self.mark_settings_dirty();
                }
                // 実測した有効映像の縦横比が4:3になる倍率にする。CRTCHKのように
                // 画面いっぱいの絵で押すこと(外接矩形は内容の大きさなので)。
                let st = self.shared.stats.lock().unwrap().clone();
                if st.active_w > 0 && st.active_h > 0 && ui.button("4:3").clicked() {
                    self.vscale =
                        (3.0 * st.active_w as f32 / (4.0 * st.active_h as f32)).clamp(0.25, 4.0);
                    self.mark_settings_dirty();
                    self.want_fit = true;
                }
                if ui.button("1.0").clicked() {
                    self.vscale = 1.0;
                    self.mark_settings_dirty();
                    self.want_fit = true;
                }
            });
            if ui.button("画に合わせる").clicked() {
                self.want_fit = true;
            }
            if ui.checkbox(&mut self.integer_scale, "integer scaling").changed() {
                self.mark_settings_dirty();
            }
            if ui.checkbox(&mut self.show_border, "capture border").changed() {
                self.mark_settings_dirty();
            }
            ui.horizontal(|ui| {
                ui.label("outside");
                let mut bd = self.backdrop;
                egui::ComboBox::from_id_salt("backdrop")
                    .selected_text(bd.label())
                    .show_ui(ui, |ui| {
                        for v in [Backdrop::Black, Backdrop::DarkGray, Backdrop::Magenta] {
                            ui.selectable_value(&mut bd, v, v.label());
                        }
                    });
                if bd != self.backdrop {
                    self.backdrop = bd;
                    self.mark_settings_dirty();
                }
            });
        });

        // キャプチャ範囲の外側は背景色で塗る。取り込んだ絵が真っ黒だと画枠の
        // 位置が分からないので、黒以外を選べるようにしてある(既定は暗い灰)。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.backdrop.color()))
            .show(root, |ui| {
                let Some(tex) = &self.texture else {
                    ui.centered_and_justified(|ui| {
                        ui.weak("waiting for stream...");
                    });
                    return;
                };
                // 無信号(同期喪失)は砂嵐になるので、絵を出さずに状態を示す
                let mode = self.shared.mode.lock().unwrap().clone();
                let valid = mode.as_ref().map_or(false, signal_is_valid);
                if !valid {
                    ui.centered_and_justified(|ui| {
                        let txt = match &mode {
                            Some(m) => format!(
                                "NO SIGNAL\n\nh {:.3} kHz   v {:.3} Hz\nvtotal {}",
                                m.hfreq_mhz_x1000 as f64 / 1e6,
                                m.vfreq_mhz_x1000 as f64 / 1e3,
                                m.vtotal
                            ),
                            None => "NO SIGNAL".to_string(),
                        };
                        ui.label(egui::RichText::new(txt).size(20.0).weak());
                    });
                    return;
                }
                let tex_size = tex.size_vec2();
                let avail = ui.available_size();
                // 縦倍率を掛けた見かけの大きさで画面に収める。X68000の
                // 15kHz 512x256 のようにドットが正方形でないモードは、等倍だと
                // 縦につぶれて見える。
                let disp = egui::vec2(tex_size.x, tex_size.y * self.vscale);
                // ウィンドウを画にぴったり合わせるのに使う(余白 = 内寸 - この領域)
                self.last_avail = avail;
                self.last_tex = disp;
                let fit = (avail.x / disp.x).min(avail.y / disp.y);
                // 起動直後に画が入り切らない場合だけ、一度だけ等倍に合わせる。
                // 縮小されるとラインが間引かれて欠けたように見えるため。
                // 以後はユーザーの操作したサイズを尊重する(一度きり)。
                if !self.did_autofit {
                    self.did_autofit = true;
                    if fit < 1.0 {
                        self.want_fit = true;
                    }
                }
                let scale = if self.integer_scale && fit >= 1.0 { fit.floor() } else { fit };
                let size = disp * scale;
                let resp = ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new((tex.id(), size)))
                });
                // キャプチャ範囲の境界に枠線。画の内容に埋もれないよう、外周の
                // すぐ外側(1px外)に引く
                if self.show_border {
                    let r = resp.inner.rect.expand(1.0);
                    ui.painter().rect_stroke(
                        r,
                        0.0,
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 64, 64)),
                        egui::StrokeKind::Outside,
                    );
                }
            });

        // ウィンドウを画にぴったり合わせる。中央パネルを描いた後に行う
        // (今フレームの描画領域が分かってから計算するため)。
        // 画の外側の余白 = ウィンドウ内寸 - 中央の描画領域。右パネルとフレームの
        // 分がここに入るので、内寸をこれだけ変えれば画がちょうど等倍で収まる。
        if self.want_fit && self.last_tex.x > 0.0 && self.last_avail.x > 0.0 {
            self.want_fit = false;
            let chrome = self.window_size - self.last_avail;
            let mut want = self.last_tex + chrome;
            // 画面より大きくしない(はみ出すと操作できなくなる)
            let mon = ctx.input(|i| i.viewport().monitor_size);
            if let Some(mon) = mon {
                want = want.min(mon * 0.95);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
        }

        // ストリーム停止中でもUI(統計・発見リスト)を更新し続ける
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl Drop for ViewerApp {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}
