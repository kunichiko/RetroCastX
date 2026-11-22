//! RetroCastX ダミーデバイス(キャプチャボードの代役)。
//!
//! 持っていない機種の映像信号で Viewer を試すためのもの。LAN(または同じ機械)の
//! 上で実機のボードと同じに振る舞うので、Viewer は何も変えずに繋がる。
//!
//! ```text
//! cargo run --release -p retrocastx-devsim                 # GUI
//! cargo run --release -p retrocastx-devsim -- --headless   # GUI なし(試験用)
//!     --input rgb15=x68000:0     端子に機種とモード(番号)を繋ぐ。複数回指定可
//!     --cycle 5                  TVP が見ている端子のモードを5秒ごとに切り替える
//!     --port 34600 --name devsim
//! ```
//!
//! ★**同じ機械で Viewer と動かすときは、こちらを先に起動する。** 34600 を先に
//!   掴んだ側が残り、Viewer は塞がっていれば空きポートへ落ちて動く(ボードは
//!   送信元ポートへ返すので支障ない)。逆の順だとこちらが 34600 を取れない。

#![windows_subsystem = "windows"]

mod board;
mod capture;
mod engine;
mod machines;
mod pattern;
mod signal;

/// プロトコルの定数とパーサは Viewer のものをそのまま使う(版ずれを防ぐ)
#[allow(dead_code)]
#[path = "../../src/protocol.rs"]
mod protocol;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui;

use board::{Board, Input};
use engine::Shared;
use machines::{Kind, Machine, Mode, Port, Scan, Timing};
use pattern::Pattern;

struct Args {
    port: u16,
    headless: bool,
    inputs: Vec<(Port, String, usize)>,
    cycle: Option<f64>,
    name: Option<String>,
    mac: Option<[u8; 6]>,
}

fn parse_port(s: &str) -> Option<Port> {
    match s {
        "video" | "1" => Some(Port::Video),
        "rgb21" | "2" => Some(Port::Rgb21),
        "rgb15" | "3" => Some(Port::Rgb15),
        "digital" | "4" => Some(Port::Digital),
        _ => None,
    }
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { port: protocol::DEFAULT_PORT, headless: false, inputs: Vec::new(), cycle: None, name: None, mac: None };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        let mut val = || it.next().ok_or(format!("{s} に値がありません"));
        match s.as_str() {
            "--port" => a.port = val()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--headless" => a.headless = true,
            "--name" => a.name = Some(val()?),
            // 複数のダミーを同じ LAN に置くときは MAC を変える(Viewer は MAC でボードを区別する)
            "--mac" => {
                let v = val()?;
                let b: Vec<u8> = v.split([':', '-']).filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
                a.mac = Some(b.try_into().map_err(|_| format!("--mac {v} は AA:BB:CC:DD:EE:FF の形で"))?);
            }
            "--cycle" => a.cycle = Some(val()?.parse().map_err(|e| format!("--cycle: {e}"))?),
            "--input" => {
                // rgb15=x68000:0
                let v = val()?;
                let (p, rest) = v.split_once('=').ok_or("--input は 端子=機種[:モード番号]")?;
                let port = parse_port(p).ok_or(format!("端子 {p} は video/rgb21/rgb15/digital"))?;
                let (m, idx) = match rest.split_once(':') {
                    Some((m, i)) => (m, i.parse().map_err(|e| format!("モード番号: {e}"))?),
                    None => (rest, 0),
                };
                a.inputs.push((port, m.to_string(), idx));
            }
            "--list" => {
                for mc in machines::machines() {
                    println!("{} ({}) {}", mc.key, mc.label, mc.port.label());
                    for (i, md) in mc.modes.iter().enumerate() {
                        let t = &md.timing;
                        println!("  {i}: {:<28} fH {:7.3}kHz fV {:6.2}Hz{}", md.name, t.fh() / 1e3, t.fv(),
                                 if md.verified { "" } else { "  (推定)" });
                    }
                }
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("使い方: retrocastx-devsim [--headless] [--input 端子=機種[:番号]]... [--cycle 秒] [--port N] [--name 名前] [--mac AA:BB:..] [--list]");
                std::process::exit(0);
            }
            _ => return Err(format!("知らない引数: {s}")),
        }
    }
    Ok(a)
}

fn make_input(mc: &Machine, mode: &Mode) -> Input {
    Input {
        machine_key: mc.key.to_string(),
        kind: mc.kind,
        mode: mode.clone(),
    }
}

fn main() {
    #[cfg(windows)]
    attach_console();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    keep_awake();
    let ms = machines::machines();
    let mut b = Board::new();
    if let Some(n) = &args.name {
        b.set_name(n);
    } else {
        b.set_name("devsim");
    }
    b.picture.caption = "on".into();
    if let Some(mac) = args.mac {
        b.mac = mac;
    }
    let inputs = if args.inputs.is_empty() {
        vec![
            (Port::Rgb15, "x68000".to_string(), 0),
            (Port::Rgb21, "msx".to_string(), 0),
            (Port::Video, "composite".to_string(), 0),
        ]
    } else {
        args.inputs.clone()
    };
    for (port, key, idx) in &inputs {
        let Some(mc) = ms.iter().find(|m| m.key == key) else {
            eprintln!("機種 {key} はありません(--list で一覧)");
            std::process::exit(2);
        };
        let Some(md) = mc.modes.get(*idx) else {
            eprintln!("{key} にモード {idx} はありません");
            std::process::exit(2);
        };
        b.inputs[*port as usize] = Some(make_input(mc, md));
    }
    let sh = Shared::new(b);
    let (sock, net_note) = match engine::open(args.port) {
        Ok((s, false)) => (Some(s), None),
        Ok((s, true)) => {
            let msg = format!(
                "UDP {p} は他のアプリ(Viewer など)が使っているので 127.0.0.1:{p} だけで待ち受けます。\n\
                 同じ機械の別の Viewer から `--board 127.0.0.1` で繋いでください(ボード一覧には出ません)。\n\
                 34600 を掴んでいる Viewer 自身からは繋がりません(返事が自分に戻るため)",
                p = args.port);
            eprintln!("{msg}");
            (Some(s), Some(msg))
        }
        Err(e) => {
            let msg = format!("UDP {} を開けません: {e}", args.port);
            eprintln!("{msg}");
            if args.headless {
                std::process::exit(1);
            }
            (None, Some(msg))
        }
    };
    if let Some(s) = sock {
        let sh2 = sh.clone();
        std::thread::Builder::new().name("engine".into()).spawn(move || engine::run(s, sh2)).unwrap();
    }

    if args.headless {
        headless(&sh, &ms, args.cycle);
        return;
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RetroCastX ダミーデバイス")
            .with_inner_size([1280.0, 860.0]),
        ..Default::default()
    };
    let app = App::new(sh.clone(), ms, net_note, args.cycle);
    if let Err(e) = eframe::run_native("retrocastx-devsim", opts, Box::new(move |cc| {
        install_cjk_font(&cc.egui_ctx);
        Ok(Box::new(app))
    })) {
        eprintln!("eframe: {e}");
    }
    sh.stop.store(true, Ordering::Relaxed);
}

/// GUI なしで動かし、1秒ごとに状態を出す
fn headless(sh: &Arc<Shared>, ms: &[Machine], cycle: Option<f64>) {
    let mut next_cycle = cycle.map(|c| Instant::now() + Duration::from_secs_f64(c));
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = Instant::now();
        if let (Some(t), Some(c)) = (next_cycle, cycle) {
            if now >= t {
                next_cycle = Some(now + Duration::from_secs_f64(c));
                let mut b = sh.board.lock().unwrap();
                let p = b.tvp_port();
                if let Some(inp) = b.inputs[p as usize].clone() {
                    if let Some(mc) = ms.iter().find(|m| m.key == inp.machine_key) {
                        let i = mc.modes.iter().position(|m| m.name == inp.mode.name).unwrap_or(0);
                        let md = &mc.modes[(i + 1) % mc.modes.len()];
                        b.inputs[p as usize] = Some(make_input(mc, md));
                        b.pic_gen += 1;
                        println!("モード切替: {} {}", mc.label, md.name);
                    }
                }
            }
        }
        let b = sh.board.lock().unwrap();
        let s = sh.stats.lock().unwrap().clone();
        for e in b.log.iter().filter(|e| now.duration_since(e.at) < Duration::from_secs(1)) {
            println!("  {}", e.text);
        }
        println!(
            "購読 {:<22} TVP {:<18} pll {:4} 位相 {:2} 形式 {} | {:6} pkt/s {:7.1} Mbps 負荷 {:.2} 周回 {}/s 最長 {:.1}ms 遅れ {:.0}ms 詰まり {}",
            match (b.sub, b.subscribed(now)) {
                (Some(a), true) => a.to_string(),
                _ => "-".into(),
            },
            b.tvp_input().map_or("信号なし".into(), |i| i.mode.name.to_string()),
            b.reg(protocol::CFG_KEY_PLL_DIVIDE),
            b.reg(protocol::CFG_KEY_PHASE),
            pixfmt_name(b.reg(protocol::CFG_KEY_PIXFMT) as u8),
            s.pkts_per_s, s.mbps, s.load, s.iters, s.worst_ms, s.lag_ms, s.would_block,
        );
    }
}

fn pixfmt_name(f: u8) -> &'static str {
    match f {
        protocol::PIXFMT_RGB888 => "RGB888",
        protocol::PIXFMT_RGB555 => "RGB555",
        protocol::PIXFMT_RGB565 => "RGB565",
        protocol::PIXFMT_YC8 => "YC8",
        protocol::PIXFMT_YCC422 => "YCC422",
        protocol::PIXFMT_CVBS8 => "CVBS8",
        _ => "?",
    }
}

/// macOS の App Nap を止める。
///
/// ★**ダミーは普段、Viewer の裏に回る。** 裏の GUI アプリは App Nap で間引かれ、
///   1ms の sleep が大きく引き延ばされる。目覚めたときに溜まったラインを一気に
///   送るので受信側のソケットバッファが溢れ、Viewer が大量に取りこぼして自動の
///   間引きまで入った(コンポーネント 240p、lost 189025)。同じ組み合わせでも
///   ダミーを端末から(GUI なしで)動かすと取りこぼしは 0 だった。
///   送出スレッドの QoS(engine.rs)だけではプロセスごとの間引きは止まらない
fn keep_awake() {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        let opts = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::LatencyCritical;
        let token = NSProcessInfo::processInfo()
            .beginActivityWithOptions_reason(opts, &NSString::from_str("映像を実時間で送る"));
        // 終了まで持ち続ける(手放すと活動が終わる)
        std::mem::forget(token);
    }
}

#[cfg(windows)]
fn attach_console() {
    // コンソールから起動されたときだけ、そこへ出力を戻す(Viewer と同じ)
    unsafe extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
}

/// CJK を含むシステムフォントを足す(Viewer の install_cjk_font と同じ候補)
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Core/HiraginoKakuGothic.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttf",
        "/usr/share/fonts/truetype/vlgothic/VL-Gothic-Regular.ttf",
        "C:/Windows/Fonts/meiryo.ttc",
        "C:/Windows/Fonts/msgothic.ttc",
        "C:/Windows/Fonts/YuGothR.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
    eprintln!("font: CJK対応フォントが見つからないため日本語が豆腐になります");
}

// ---------------------------------------------------------------------------
// GUI

/// 端子ごとの画面の状態
struct PortUi {
    /// None = 未接続 / Some(i) = machines[i] / Some(CUSTOM) = カスタム
    machine: Option<usize>,
    mode: usize,
    custom: CustomUi,
    /// モードを自動で切り替える間隔[秒](0 = しない)
    cycle_s: f32,
    cycle_at: Option<Instant>,
}

const CUSTOM: usize = usize::MAX;

#[derive(Clone)]
struct CustomUi {
    t: Timing,
    kind: Kind,
    color_bits: u32,
    error: Option<String>,
}

impl Default for CustomUi {
    fn default() -> Self {
        CustomUi {
            t: Timing {
                dotclk_hz: 25.175e6, htotal: 800, hsync: 96, hstart: 144, hactive: 640,
                vtotal: 525, vsync: 2, vstart: 35, vactive: 480, scan: Scan::Progressive, line_repeat: 1,
            },
            kind: Kind::Rgb,
            color_bits: 8,
            error: None,
        }
    }
}

struct App {
    sh: Arc<Shared>,
    ms: Vec<Machine>,
    ports: Vec<PortUi>,
    net_err: Option<String>,
    preview_tex: Option<egui::TextureHandle>,
    preview_gen: u64,
    preview_at: Instant,
    name_edit: String,
}

impl App {
    fn new(sh: Arc<Shared>, ms: Vec<Machine>, net_err: Option<String>, cycle: Option<f64>) -> App {
        let ports = {
            let b = sh.board.lock().unwrap();
            Port::ALL
                .iter()
                .map(|&p| {
                    let (machine, mode) = match b.input(p) {
                        Some(inp) => {
                            let mi = ms.iter().position(|m| m.key == inp.machine_key);
                            let md = mi.and_then(|i| ms[i].modes.iter().position(|m| m.name == inp.mode.name));
                            (mi, md.unwrap_or(0))
                        }
                        None => (None, 0),
                    };
                    let cycle_s = if p == b.tvp_port() { cycle.unwrap_or(0.0) as f32 } else { 0.0 };
                    PortUi { machine, mode, custom: CustomUi::default(), cycle_s, cycle_at: None }
                })
                .collect()
        };
        let name_edit = sh.board.lock().unwrap().name.clone();
        App {
            sh, ms, ports, net_err, preview_tex: None, preview_gen: 0,
            preview_at: Instant::now(), name_edit,
        }
    }

    /// 画面の選択をボードへ反映する
    fn apply_port(&mut self, pi: usize) {
        let p = &self.ports[pi];
        let input = match p.machine {
            None => None,
            Some(CUSTOM) => {
                let c = &p.custom;
                Some(Input {
                    machine_key: "custom".into(),
                    kind: c.kind,
                    mode: Mode { name: "custom", timing: c.t.clone(), color_bits: c.color_bits,
                                 verified: false, note: "" },
                })
            }
            Some(i) => {
                let mc = &self.ms[i];
                mc.modes.get(p.mode).map(|md| make_input(mc, md))
            }
        };
        let mut b = self.sh.board.lock().unwrap();
        b.inputs[pi] = input;
        b.pic_gen += 1;
    }

    fn tick_cycles(&mut self) {
        let now = Instant::now();
        for pi in 0..self.ports.len() {
            let p = &mut self.ports[pi];
            let Some(mi) = p.machine.filter(|&m| m != CUSTOM) else {
                p.cycle_at = None;
                continue;
            };
            if p.cycle_s <= 0.0 {
                p.cycle_at = None;
                continue;
            }
            let at = *p.cycle_at.get_or_insert(now + Duration::from_secs_f32(p.cycle_s));
            if now >= at {
                p.cycle_at = Some(now + Duration::from_secs_f32(p.cycle_s));
                p.mode = (p.mode + 1) % self.ms[mi].modes.len();
                self.apply_port(pi);
            }
        }
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        if let Some(e) = &self.net_err {
            ui.colored_label(egui::Color32::YELLOW, e);
        }
        let local = *self.sh.local.lock().unwrap();
        let s = self.sh.stats.lock().unwrap().clone();
        let b = self.sh.board.lock().unwrap();
        egui::Grid::new("status").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("待ち受け");
            ui.monospace(local.map_or("-".into(), |a| a.to_string()));
            ui.end_row();
            ui.label("購読");
            match (b.sub, b.sub_until) {
                (Some(a), Some(t)) if now < t => {
                    ui.monospace(format!("{a}  (残り {:.0} 秒)", (t - now).as_secs_f32()))
                }
                (Some(a), _) => ui.weak(format!("{a}(切れた)")),
                _ => ui.weak("Viewer を待っています"),
            };
            ui.end_row();
            ui.label("TVP の入力");
            let port = b.tvp_port();
            ui.monospace(format!("{}  (19h=0x{:02X})", port.label(), b.reg(protocol::CFG_KEY_IN_MUX1)));
            ui.end_row();
            ui.label("取り込み");
            ui.monospace(format!(
                "pll {}  位相 {}  {}  間引き {}  vbp {}{}",
                b.reg(protocol::CFG_KEY_PLL_DIVIDE), b.reg(protocol::CFG_KEY_PHASE),
                pixfmt_name(b.reg(protocol::CFG_KEY_PIXFMT) as u8), b.reg(protocol::CFG_KEY_FRAME_SKIP),
                b.reg(protocol::CFG_KEY_VBP),
                if b.reg(protocol::CFG_KEY_FULL_LINE) != 0 { "  全ライン" } else { "" }));
            ui.end_row();
            ui.label("MODE");
            let m = &b.meas;
            ui.monospace(format!(
                "fH {:.3}kHz  vtotal {}  fV {:.3}Hz  組 {}",
                m.hfreq as f64 / 1e3, m.vtotal, m.vfreq_mhz as f64 / 1e3,
                if m.frame_clocks != 0 { format!("{} ライン", m.frame_lines) } else { "-".into() }));
            ui.end_row();
            ui.label("送信");
            ui.monospace(format!("{} pkt/s  {:.1} Mbps  {} ライン/s  生成負荷 {:.0}%",
                                 s.pkts_per_s, s.mbps, s.lines_per_s, s.load * 100.0));
            ui.end_row();
            ui.label("送出ループ");
            // 周回が数十/s に落ちていたら、OS に間引かれて送出が塊になっている
            let t = format!("{} 周/s  1周の最長 {:.1}ms  遅れ {:.1}ms", s.iters, s.worst_ms, s.lag_ms);
            if s.pkts_per_s > 0 && s.iters < 300 {
                ui.colored_label(egui::Color32::YELLOW, t + "(間引かれています)");
            } else if s.lag_ms > 30.0 {
                // ★**信号どおりの速さで送れていない。** Viewer に出るフレームレートが
                //   信号より低くなる。ループバックの送信は1パケット 15〜20µs かかり、
                //   毎秒 4〜6 万パケットあたりが上限(機械の混み具合で変わる)
                ui.colored_label(egui::Color32::YELLOW, t)
                    .on_hover_text("送信が信号のパケットレートに追いついていません。\n\
                                    Viewer に出るフレームレートが信号より低くなります。\n\
                                    ダミーを別の機械で動かすか、Viewer の間引きを使ってください");
            } else {
                ui.monospace(t);
            }
            ui.end_row();
            if s.send_errors > 0 || s.dropped > 0 {
                ui.label("");
                ui.colored_label(egui::Color32::YELLOW, format!(
                    "送信エラー {}  {}  わざと捨てた {}", s.send_errors, s.last_error, s.dropped));
                ui.end_row();
            }
        });
    }

    fn ports_ui(&mut self, ui: &mut egui::Ui) {
        let tvp_port = self.sh.board.lock().unwrap().tvp_port();
        for pi in 0..self.ports.len() {
            let port = Port::ALL[pi];
            let mut changed = false;
            let title = format!("{}{}", port.label(), if port == tvp_port { "  ◀ TVP が見ている" } else { "" });
            egui::CollapsingHeader::new(egui::RichText::new(title).strong())
                .id_salt(("port", pi))
                .default_open(self.ports[pi].machine.is_some() || port == tvp_port)
                .show(ui, |ui| {
                    let p = &mut self.ports[pi];
                    let cur = match p.machine {
                        None => "未接続".to_string(),
                        Some(CUSTOM) => "カスタム".to_string(),
                        Some(i) => self.ms[i].label.to_string(),
                    };
                    egui::ComboBox::from_id_salt(("machine", pi)).selected_text(cur).width(240.0).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut p.machine, None, "未接続").changed();
                        for (i, mc) in self.ms.iter().enumerate() {
                            let label = if mc.port == port { mc.label.to_string() } else {
                                format!("{}(本来は{})", mc.label, mc.port.label())
                            };
                            if ui.selectable_value(&mut p.machine, Some(i), label).changed() {
                                p.mode = 0;
                                changed = true;
                            }
                        }
                        changed |= ui.selectable_value(&mut p.machine, Some(CUSTOM), "カスタム").changed();
                    });
                    match p.machine {
                        Some(CUSTOM) => {
                            if custom_ui(ui, &mut p.custom, pi) {
                                changed = true;
                            }
                        }
                        Some(mi) => {
                            let mc = &self.ms[mi];
                            ui.horizontal_wrapped(|ui| {
                                for (i, md) in mc.modes.iter().enumerate() {
                                    let r = ui.selectable_label(p.mode == i, md.name);
                                    if r.clicked() && p.mode != i {
                                        p.mode = i;
                                        changed = true;
                                    }
                                }
                            });
                            if let Some(md) = mc.modes.get(p.mode) {
                                let t = &md.timing;
                                ui.weak(format!(
                                    "{:.4}MHz  {}x{}  total {}x{}  fH {:.3}kHz  fV {:.2}Hz{}",
                                    t.dotclk_hz / 1e6, t.hactive, t.vactive, t.htotal, t.vtotal,
                                    t.fh() / 1e3, t.fv(),
                                    match t.scan { Scan::Progressive => "", Scan::Interlace => "  インターレース",
                                                   Scan::FieldPair => "  VSYNC1回にフィールド2枚" }));
                                let note = if md.verified { md.note.to_string() } else { format!("【推定】{}", md.note) };
                                if !note.is_empty() {
                                    ui.weak(note);
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.label("モードを自動で切り替える");
                                ui.add(egui::DragValue::new(&mut p.cycle_s).range(0.0..=60.0).speed(0.1).suffix(" 秒"))
                                    .on_hover_text("0 で止める。ゲーム中の解像度切替の追従を試す");
                            });
                        }
                        None => {}
                    }
                });
            if changed {
                self.apply_port(pi);
            }
        }
    }

    fn picture_ui(&mut self, ui: &mut egui::Ui) {
        let mut b = self.sh.board.lock().unwrap();
        let before = (b.picture.clone(), b.noise);
        ui.horizontal(|ui| {
            ui.label("柄");
            egui::ComboBox::from_id_salt("pattern").selected_text(b.picture.pattern.label()).show_ui(ui, |ui| {
                for p in Pattern::ALL {
                    ui.selectable_value(&mut b.picture.pattern, p, p.label());
                }
            });
            ui.checkbox(&mut b.picture.moving, "動く箱");
            let mut cap = !b.picture.caption.is_empty();
            if ui.checkbox(&mut cap, "名前を書く").changed() {
                b.picture.caption = if cap { "on".into() } else { String::new() };
            }
        });
        ui.horizontal(|ui| {
            ui.label("ノイズ");
            ui.add(egui::Slider::new(&mut b.noise, 0..=16).suffix(" コード"))
                .on_hover_text("全サンプルに ± この幅の雑音を足す。黒の判定(black_th)や吸着の試験に");
        });
        if (b.picture.clone(), b.noise) != before {
            b.pic_gen += 1;
        }
    }

    fn fault_ui(&mut self, ui: &mut egui::Ui) {
        let mut b = self.sh.board.lock().unwrap();
        ui.horizontal(|ui| {
            ui.label("パケットロス");
            ui.add(egui::Slider::new(&mut b.loss_pct, 0.0..=20.0).suffix(" %").logarithmic(true))
                .on_hover_text("LINE をラインごと(断片ごと全部)捨てる");
        });
        ui.checkbox(&mut b.tone, "音声にテスト音を乗せる(機種が繋がった端子の系統だけ)");
        ui.label("ANNOUNCE の caps(古いゲートウェアを装う)");
        ui.horizontal_wrapped(|ui| {
            for (bit, name) in [
                (protocol::CAP_FRAME_CLOCKS, "FRAME_CLOCKS (gw-v1.5.0〜)"),
                (protocol::CAP_COMPACT, "COMPACT"),
                (protocol::CAP_LED_IDLE, "LED_IDLE"),
            ] {
                let mut on = b.caps & bit != 0;
                if ui.checkbox(&mut on, name).changed() {
                    b.caps ^= bit;
                }
            }
        });
    }

    fn ident_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("ボード名");
            let r = ui.add(egui::TextEdit::singleline(&mut self.name_edit).desired_width(160.0));
            if r.lost_focus() {
                self.sh.board.lock().unwrap().set_name(&self.name_edit.clone());
            }
        });
        let b = self.sh.board.lock().unwrap();
        let mac: Vec<String> = b.mac.iter().map(|v| format!("{v:02X}")).collect();
        ui.weak(format!("MAC {}  fw 0x{:04X}  caps 0x{:04X}", mac.join(":"), b.fw_version, b.caps));
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        // 取り込んだ絵は数百ms ごとに作り直せば足りる(31kHz で 2048x1136)
        if now.duration_since(self.preview_at) >= Duration::from_millis(250) {
            let pv = self.sh.preview.lock().unwrap();
            if pv.serial != self.preview_gen && pv.width > 0 && !pv.slots.is_empty() {
                self.preview_gen = pv.serial;
                self.preview_at = now;
                let (w, h) = (pv.width as usize, pv.slots.len());
                let mut rgb = vec![0u8; w * h * 3];
                for (y, s) in pv.slots.iter().enumerate() {
                    if let Some(s) = s {
                        for (x, c) in s.iter().take(w).enumerate() {
                            rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(c);
                        }
                    }
                }
                let img = egui::ColorImage::from_rgb([w, h], &rgb);
                match &mut self.preview_tex {
                    Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                    None => self.preview_tex =
                        Some(ui.ctx().load_texture("preview", img, egui::TextureOptions::NEAREST)),
                }
            }
        }
        ui.label(egui::RichText::new("取り込んだ絵(Viewer に送っている中身。横は pll_divide、縦は半ライン単位のスロット)").strong());
        if let Some(t) = &self.preview_tex {
            let size = t.size_vec2();
            let avail = ui.available_width();
            let h = (avail * size.y / size.x).min(ui.available_height() * 0.7).max(50.0);
            let w = h * size.x / size.y;
            ui.image((t.id(), egui::vec2(w, h)));
        } else {
            ui.weak("(まだ何も取り込んでいません。TVP が見ている端子に機種を繋いでください)");
        }
    }

    fn regs_ui(&mut self, ui: &mut egui::Ui) {
        let b = self.sh.board.lock().unwrap();
        let mut keys: Vec<(&u16, &u32)> = b.regs.iter().filter(|(k, _)| **k < 0x0400 && **k != 0xFFFF).collect();
        keys.sort();
        egui::Grid::new("regs").num_columns(6).striped(true).show(ui, |ui| {
            for (i, (k, v)) in keys.iter().enumerate() {
                ui.monospace(format!("0x{k:04X} {}", board::key_name(**k)));
                ui.monospace(format!("{v} (0x{v:X})"));
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        let b = self.sh.board.lock().unwrap();
        let start = b.log.front().map(|e| e.at);
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for e in &b.log {
                let t = start.map_or(0.0, |s| e.at.duration_since(s).as_secs_f32());
                ui.monospace(format!("{t:8.2}  {}", e.text));
            }
        });
    }
}

/// カスタムのタイミング入力。適用したら true
fn custom_ui(ui: &mut egui::Ui, c: &mut CustomUi, pi: usize) -> bool {
    let mut apply = false;
    let mut mhz = c.t.dotclk_hz / 1e6;
    egui::Grid::new(("custom", pi)).num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
        ui.label("ドットクロック");
        ui.add(egui::DragValue::new(&mut mhz).range(1.0..=200.0).speed(0.001).suffix(" MHz").max_decimals(5));
        ui.label("信号");
        egui::ComboBox::from_id_salt(("ckind", pi)).selected_text(format!("{:?}", c.kind)).show_ui(ui, |ui| {
            for k in [Kind::Rgb, Kind::Composite, Kind::SVideo, Kind::Component] {
                ui.selectable_value(&mut c.kind, k, format!("{k:?}"));
            }
        });
        ui.end_row();
        let row = |ui: &mut egui::Ui, a: &str, va: &mut u32, b: &str, vb: &mut u32| {
            ui.label(a);
            ui.add(egui::DragValue::new(va).range(0..=4095));
            ui.label(b);
            ui.add(egui::DragValue::new(vb).range(0..=4095));
            ui.end_row();
        };
        row(ui, "htotal [ドット]", &mut c.t.htotal, "vtotal [ライン]", &mut c.t.vtotal);
        row(ui, "HSYNC 幅", &mut c.t.hsync, "VSYNC 幅", &mut c.t.vsync);
        row(ui, "有効の開始 (H)", &mut c.t.hstart, "有効の開始 (V)", &mut c.t.vstart);
        row(ui, "有効の幅", &mut c.t.hactive, "有効の高さ", &mut c.t.vactive);
        row(ui, "階調 [bit]", &mut c.color_bits, "同じ行の繰り返し", &mut c.t.line_repeat);
        ui.label("走査");
        egui::ComboBox::from_id_salt(("cscan", pi)).selected_text(format!("{:?}", c.t.scan)).show_ui(ui, |ui| {
            for s in [Scan::Progressive, Scan::Interlace, Scan::FieldPair] {
                ui.selectable_value(&mut c.t.scan, s, format!("{s:?}"));
            }
        });
        ui.end_row();
    });
    c.t.dotclk_hz = mhz * 1e6;
    c.color_bits = c.color_bits.clamp(1, 8);
    ui.horizontal(|ui| {
        ui.weak(format!("fH {:.3}kHz  fV {:.2}Hz", c.t.fh() / 1e3, c.t.fv()));
        if ui.button("適用").clicked() {
            match c.t.validate() {
                Ok(()) => {
                    c.error = None;
                    apply = true;
                }
                Err(e) => c.error = Some(e),
            }
        }
    });
    if let Some(e) = &c.error {
        ui.colored_label(egui::Color32::LIGHT_RED, e);
    }
    apply
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tick_cycles();
        egui::Panel::left("controls").resizable(true).default_size(470.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("ボード");
                self.status_ui(ui);
                self.ident_ui(ui);
                ui.separator();
                ui.heading("端子と機種");
                self.ports_ui(ui);
                ui.separator();
                ui.heading("絵");
                self.picture_ui(ui);
                ui.separator();
                ui.heading("障害");
                self.fault_ui(ui);
            });
        });
        egui::Panel::bottom("log").resizable(true).default_size(220.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("記録(SUBSCRIBE / CONFIG)");
                if ui.small_button("消す").clicked() {
                    self.sh.board.lock().unwrap().log.clear();
                }
            });
            self.log_ui(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.preview_ui(ui);
            egui::CollapsingHeader::new("CONFIG レジスタ").show(ui, |ui| self.regs_ui(ui));
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}
