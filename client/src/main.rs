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
    let mut audio = receiver::AudioOpts::default();
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
                    Some(args.next().expect("--audio needs 0|1|2").parse().unwrap())
            }
            "--no-audio" => audio.source = None,
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
            .with_inner_size([1160.0, 820.0])
            .with_title("RetroCastX Viewer"),
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        "RetroCastX Viewer",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ViewerApp::new(cc, port, subscribe_to, target_mac, no_vsync, decay, audio)))
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

struct ViewerApp {
    shared: Arc<receiver::Shared>,
    /// 再生中の音声source(UI表示用)
    audio_source: Option<u8>,
    /// 出力デバイス一覧(起動時に列挙。再列挙ボタンで更新)
    audio_devices: Vec<String>,
    /// UIで選択中のデバイス。None は「システム既定」
    audio_device_sel: Option<String>,
    texture: Option<egui::TextureHandle>,
    seen_gen: u64,
    integer_scale: bool,
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
            audio_device_sel: None,
            texture: None,
            seen_gen: 0,
            integer_scale: true,
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
            request = Some(receiver::AudioRequest { device: dev, source: self.audio_source });
        }

        if let Some(req) = request {
            *self.shared.audio_request.lock().unwrap() = Some(req);
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

impl eframe::App for ViewerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.refresh_texture(&ctx);

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
            ui.separator();

            ui.strong("Audio");
            self.audio_ui(ui);
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

            ui.checkbox(&mut self.integer_scale, "integer scaling");
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(root, |ui| {
                let Some(tex) = &self.texture else {
                    ui.centered_and_justified(|ui| {
                        ui.weak("waiting for stream...");
                    });
                    return;
                };
                let tex_size = tex.size_vec2();
                let avail = ui.available_size();
                let fit = (avail.x / tex_size.x).min(avail.y / tex_size.y);
                let scale = if self.integer_scale && fit >= 1.0 { fit.floor() } else { fit };
                let size = tex_size * scale;
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new((tex.id(), size)));
                });
            });

        // ストリーム停止中でもUI(統計・発見リスト)を更新し続ける
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl Drop for ViewerApp {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}
