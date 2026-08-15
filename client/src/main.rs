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
//! --mimicx-probe は MimicX へのキー転送経路(CoreMIDI)だけを確かめて終了する。
//!
//! --mac は購読対象ボードのMACを指名する(複数ボードLANで必須。省略時は
//! ワイルドカード=全ボード、単一ボードLAN専用)。
//!
//! 既定ではSUBSCRIBEを255.255.255.255にブロードキャストし、ボードの
//! ストリームを自分に向ける。sender_sim相手なら --no-subscribe でよい
//! (sender_simはSUBSCRIBEを無視して--dest宛に送るため)。

// Windows: コンソール窓を出さずに起動する。これが無いと、Explorerから起動しても
// 先に真っ黒なコンソールが開いてからUIが出る(通常のGUIアプリの見た目にならない)。
// 代わりにコンソールから起動したときは main の先頭で親のコンソールへ繋ぎ直すので、
// --headless の出力はこれまでどおり見える。
#![windows_subsystem = "windows"]

mod appicon;
mod assembler;
mod audio;
mod bezel;
mod fullscreen;
mod keytap;
mod netcheck;
mod ntsc;
mod profiles;
mod remote_input;
mod render;
mod protocol;
mod receiver;
mod settings;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;

/// コンソールから起動されたときだけ、その親コンソールへ標準出力を戻す。
/// Explorer から起動したときは親が無いので何も起きない(コンソールは出ない)。
/// **最初の出力より前に呼ぶこと** — 標準ハンドルは初回使用時に決まるため。
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() -> eframe::Result {
    attach_parent_console();
    let mut port = protocol::DEFAULT_PORT;
    let mut subscribe_to = Some("255.255.255.255".to_string());
    let mut headless_secs: Option<u64> = None;
    // --headless のときに完成フレームをPPMへ落とす。絵を目で見られない環境で
    // 「組立・復調までは合っているのか」を切り分けるため
    let mut dump_frame: Option<String> = None;
    // 連続Nフレームを番号付きで落とす(フレーム間で変わる現象の追跡用)
    let mut dump_seq: Option<usize> = None;
    let mut no_vsync = false;
    let mut fullscreen_mode = false;
    let mut rotate: u32 = u32::MAX;   // 未指定なら設定ファイルの値を使う
    let mut target_mac: Option<[u8; 6]> = None;
    let mut decay = 0.8f32;
    // インターレース時の減衰率。既定1.0=減衰しない。
    // インターレースは「毎フレーム半分の行が来ない」のが正常なので、パケットロス用の
    // 減衰をそのまま掛けると面全体がフィールドレートでちらつく。CRTの残光を模すなら
    // 少し減衰させたい人もいるので設定にしてある。
    let mut interlace_decay = 1.0f32;
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
            "--dump-seq" => {
                dump_seq = Some(args.next().expect("--dump-seq needs a count")
                                .parse().unwrap())
            }
            "--dump-frame" => {
                dump_frame = Some(args.next().expect("--dump-frame needs a path"))
            }
            "--headless" => {
                headless_secs = Some(args.next().expect("--headless needs seconds").parse().unwrap())
            }
            // VRR実験用: presentをvsyncから切り離す(wgpu AutoNoVsync)。
            // VRR対応パネルなら「フレームが来たときに出す」に近づく第一歩
            "--no-vsync" => no_vsync = true,
            // 低遅延フルスクリーン(専用presentスレッド+Immediate、ソース駆動present)
            "--fullscreen" => fullscreen_mode = true,
            // 画面回転(縦画面のゲーム用)。0/90/180/270 度、時計回り
            "--rotate" => {
                let v: u32 = args.next().expect("--rotate needs 0|90|180|270").parse().unwrap();
                rotate = match v {
                    0 => 0, 90 => 1, 180 => 2, 270 => 3,
                    _ => panic!("--rotate は 0/90/180/270 のいずれか"),
                };
            }
            // 欠損ライン減衰率(1.0=前フレーム保持のまま, 0.8=毎フレーム80%へ暗転して消える)
            "--interlace-decay" => {
                interlace_decay = args.next()
                    .expect("--interlace-decay needs a value (e.g. 1.0)").parse().unwrap()
            }
            "--decay" => {
                decay = args.next().expect("--decay needs a value (e.g. 0.8)").parse().unwrap()
            }
            // 音声: 再生するsource(0=RGB端子音声, 1=LINE入力, 2=S/PDIF)
            "--audio" => {
                audio.source =
                    Some(args.next().expect("--audio needs 0|1|2").parse().unwrap());
                cfg.audio_source = audio.source;
            }
            // MimicX への経路だけを確かめて終了する(GUIは起動しない)。
            // 「キーが効かない」ときに、宛先が無いのか送れているのかを切り分ける
            "--mimicx-probe" => std::process::exit(!remote_input::probe() as i32),
            // 受け取った物理キーを stderr へ出す。「そのキーがアプリに届いて
            // いないのか、届いているが転送していないのか」の切り分け用
            // NIC受信バッファーの確認だけして終了する(GUIは起動しない)。
            // 実機での切り分け用。IPはボードのアドレス(経路の判定に使う)
            "--netcheck" => {
                let ip = args.next().expect("--netcheck needs the board IP");
                let b = netcheck::probe(&ip);
                println!("board {ip} への経路の NIC: {b:?}");
                match &b {
                    netcheck::Buffers::Known { adapter, value } => {
                        println!("  アダプタ      : {adapter}");
                        println!("  ReceiveBuffers: {value} (推奨 {})", netcheck::RECOMMENDED);
                        if b.should_warn() {
                            println!("  → 小さすぎます:");
                            println!("     {}", netcheck::fix_command(Some(adapter)));
                        } else {
                            println!("  → 足りています");
                        }
                    }
                    netcheck::Buffers::Unsupported => {
                        println!("  このドライバは *ReceiveBuffers を公開していません(判定不能)");
                    }
                    netcheck::Buffers::Unknown => {
                        println!("  調べられませんでした(経路が引けない/レジストリが読めない)");
                    }
                }
                std::process::exit(0);
            }
            "--log-keys" => {
                remote_input::LOG_KEYS.store(true, Ordering::Relaxed);
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
        return run_headless(port, subscribe_to, target_mac, secs, decay, interlace_decay,
                            audio, dump_frame, dump_seq);
    }
    if fullscreen_mode {
        let rot = if rotate == u32::MAX { cfg.rotate } else { rotate };
        let p = render::Params {
            rotate: rot,
            tube: cfg.tube_aspect,
            filter: cfg.filter,
            window: cfg.window,
            time_based: cfg.tube_time_based,
            h_size: 1.0, h_pos: 0.0, v_size: 1.0, v_pos: 0.0,
            ..Default::default()
        };
        fullscreen::run(port, subscribe_to, target_mac, decay, interlace_decay, audio, p); // 戻らない
    }

    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if no_vsync {
        wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
        wgpu_options.surface.desired_maximum_frame_latency = Some(1); // 低遅延優先
    }
    let options = eframe::NativeOptions {
        viewport: {
            // アイコンは実行時に設定しないとタスクバー/タイトルバーに出ない
            // (exeへの埋め込みはExplorerのファイル用。appicon.rs 参照)
            let mut vp = egui::ViewportBuilder::default()
                .with_inner_size([cfg.window_w, cfg.window_h])
                .with_title("RetroCast X");
            if let Some(icon) = appicon::egui_icon() {
                vp = vp.with_icon(icon);
            }
            vp
        },
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        "RetroCast X",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ViewerApp::new(cc, port, subscribe_to, target_mac, no_vsync, decay, interlace_decay,
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
    interlace_decay: f32,
    audio: receiver::AudioOpts,
    dump_frame: Option<String>,
    dump_seq: Option<usize>,
) -> eframe::Result {
    let shared = Arc::new(receiver::Shared::default());
    receiver::spawn(
        receiver::Config { port, subscribe_to, target_mac, decay, interlace_decay, audio },
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
            "{dims}  {:.1} fps  {:.1} Mbps  frames={} pkts={} lost={} qdrop={} orphan={}",
            s.fps, s.mbps, s.frames, s.packets, s.lost_packets, s.queue_drops,
            s.orphan_lines
        );
        // 連続フレームを番号付きで落とす。**フレームごとに入れ替わる縞**のような
        // 「1枚では分からない」現象を追うのに要る(実際に必要になった)。
        if let (Some(path), Some(n)) = (dump_frame.as_ref(), dump_seq) {
            let mut last = 0u64;
            let mut got = 0usize;
            let t0 = std::time::Instant::now();
            while got < n && t0.elapsed() < std::time::Duration::from_secs(10) {
                let g = shared.frame_gen.load(Ordering::Acquire);
                if g == last {
                    std::thread::sleep(std::time::Duration::from_micros(500));
                    continue;
                }
                last = g;
                if let Some(f) = shared.frame.lock().unwrap().as_ref() {
                    let mut out = format!("P6\n{} {}\n255\n", f.width, f.height)
                        .into_bytes();
                    for px in f.rgba.chunks_exact(4) {
                        out.extend_from_slice(&px[..3]);
                    }
                    let _ = std::fs::write(format!("{path}.{got:04}.ppm"), &out);
                    got += 1;
                }
            }
            println!("   dump: {path}.0000..{:04}.ppm ({}枚)", got.saturating_sub(1), got);
            return Ok(());
        }
        // 完成フレームをそのままPPMへ落とす。**GUIを開かずに絵を確かめる唯一の手段。**
        // 「絵が出ない」を追うとき、組立・復調までは正しいのか、描画側なのかを
        // ここで切り分けられる(実際にこれで切り分けた)。
        if let Some(path) = dump_frame.as_ref() {
            if let Some(f) = shared.frame.lock().unwrap().as_ref() {
                let mut out = format!("P6\n{} {}\n255\n", f.width, f.height).into_bytes();
                for px in f.rgba.chunks_exact(4) {
                    out.extend_from_slice(&px[..3]);
                }
                if std::fs::write(path, &out).is_ok() {
                    let n = f.rgba.chunks_exact(4).filter(|p| p[0] | p[1] | p[2] != 0).count();
                    println!("   dump: {path} ({}x{}) 非黒画素 {}/{} = {:.1}%",
                             f.width, f.height, n, f.width * f.height,
                             n as f32 * 100.0 / (f.width * f.height) as f32);
                }
            }
        }
        // NTSC復調の状態。**GUIを開かずに確認できるようにしておく。**
        // この環境は画面収録の権限が無く、絵で確かめられないので数値で出す
        if s.ntsc_comb_step > 0 {
            println!("   NTSC復調: {}行ロック  コム間隔{}  位相差{:.0}°(180°が正常)",
                     s.ntsc_locked, s.ntsc_comb_step, s.ntsc_phase_deg);
            if s.ntsc_svideo {
                println!("   S端子(赤chのバーストで判定): コムは使わない/輝度は素通し");
            } else {
                println!("   3次元コム: {}行  動きと判定 {:.1}%  1フレーム前との位相ズレ {:.1}°",
                         s.ntsc_lines_3d, 100.0 * s.ntsc_motion_frac,
                         s.ntsc_phase_drift_deg);
            }
        }
        // 生産側がUIを待った時間。**GUIのときだけパケットが落ちる**を切り分ける
        if s.publish_wait_max_ms > 0.05 {
            println!("   フレーム差し替えのUI待ち: 合計{:.1}ms 最大{:.1}ms /区間",
                     s.publish_wait_ms, s.publish_wait_max_ms);
        }
        println!("   インタレース判定(測定): {}  未充填の行 {}",
                 if s.interlace_measured { "1フィールドずつ来ている(太らせ停止・減衰なし)" }
                 else { "プログレッシブ扱い(太らせ有効)" },
                 s.unfilled_rows);
        // 何も来ないときに、どの種別が届いていないかを出す。
        // ブロードキャストだけ届いてユニキャストが届かない、などが分かる
        if s.frames == 0 {
            let k: Vec<String> = ["LINE", "AUDIO", "MODE", "INFO", "CONFIG", "other"]
                .iter()
                .zip(shared.kind_counts.iter())
                .map(|(n, c)| format!("{n}={}", c.load(Ordering::Relaxed)))
                .collect();
            println!("   受信種別: {}", k.join(" "));
        }
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

/// **新しいフレームが画面に出る間隔**の計測(vsync量子化の検出用)。
///
/// ★**描画にかかった時間ではない。** `refresh_texture` は新フレームが無ければ
///   即 return するので、ここに来るのは「中身が変わったpaint」だけ。だから
///   ボードが59.8fps送っているのにここが54.9Hzなら、**8%のフレームが表示されずに
///   置き換わっている**という意味になる。以前は "paint" と表示していて
///   「描画に18msかかっている」と読めてしまった。
///
/// present完了時刻そのものはwgpu経由では取れないため、この間隔を代理指標にする
/// (FIFOではスワップチェーンのバックプレッシャでvsync周期に量子化される)。
/// CPU側で実際にかかった時間は `cpu_summary` に別に出す。
struct PaceMeter {
    last_paint: Option<std::time::Instant>,
    intervals_ms: std::collections::VecDeque<f32>,
    last_log: std::time::Instant,
    summary: String,
    /// ★**間隔と処理時間は別物。** 上の intervals_ms は「新フレームが画面に出る
    ///   間隔」で、FIFOのvsync待ちを含む。こちらはCPU側で実際にかかった時間で、
    ///   間隔が伸びたときに「我々が遅いのか、待たされているのか」を分ける。
    ui_ms: std::collections::VecDeque<f32>,
    upload_ms: std::collections::VecDeque<f32>,
    cpu_summary: String,
    /// ★**描画そのものの回数。** 新フレームの有無に関わらず数える。
    ///   これが60Hz出ているのに新フレーム間隔が54Hzなら「描画は回っているが
    ///   フレームが置き換わっている」、こちらも54Hzなら「描画が回っていない」。
    ui_calls: u32,
    ui_rate_hz: f32,
    ui_since: std::time::Instant,
}

impl PaceMeter {
    fn new() -> Self {
        Self {
            last_paint: None,
            intervals_ms: std::collections::VecDeque::with_capacity(256),
            last_log: std::time::Instant::now(),
            summary: String::new(),
            ui_ms: std::collections::VecDeque::with_capacity(256),
            upload_ms: std::collections::VecDeque::with_capacity(256),
            cpu_summary: String::new(),
            ui_calls: 0,
            ui_rate_hz: 0.0,
            ui_since: std::time::Instant::now(),
        }
    }

    fn note_upload(&mut self, d: std::time::Duration) {
        if self.upload_ms.len() >= 256 { self.upload_ms.pop_front(); }
        self.upload_ms.push_back(d.as_secs_f32() * 1000.0);
    }

    fn note_ui(&mut self, d: std::time::Duration) {
        if self.ui_ms.len() >= 256 { self.ui_ms.pop_front(); }
        self.ui_ms.push_back(d.as_secs_f32() * 1000.0);
        self.ui_calls += 1;
        let el = self.ui_since.elapsed().as_secs_f32();
        if el >= 1.0 {
            self.ui_rate_hz = self.ui_calls as f32 / el;
            self.ui_calls = 0;
            self.ui_since = std::time::Instant::now();
        }
        let f = |v: &std::collections::VecDeque<f32>| -> (f32, f32) {
            if v.is_empty() { return (0.0, 0.0); }
            let n = v.len() as f32;
            let mean = v.iter().sum::<f32>() / n;
            let max = v.iter().cloned().fold(0.0f32, f32::max);
            (mean, max)
        };
        let (um, ux) = f(&self.ui_ms);
        let (tm, tx) = f(&self.upload_ms);
        self.cpu_summary = format!(
            "描画 {:.1}Hz / CPU: UI {um:.2}ms(最大{ux:.2}) 転送 {tm:.2}ms(最大{tx:.2})",
            self.ui_rate_hz);
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
                "新フレーム間隔 {:.2}ms σ{:.2} → {:.2}Hz",
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

/// pll_divide と位相を自動で決める処理の進行状態。
///
/// 原理は host/python/retrocastx/pll_tune.py の autotune と同じ。要点だけ:
/// 「近傍で鋭さが最大の点」を探す方法は有理倍に騙される(1.5倍は位相ロックする
/// 有理比なので本物の局所ピークに見える)。そこで、まず上限まで意図的に過剰
/// サンプルしてスペクトル占有率から倍率を割り出し、そこから8の倍数で±7%だけ
/// 局所探索して詰める。最後に位相を振る。
enum AutoPhase {
    /// 上限まで過剰サンプルして占有率を測る
    Oversample,
    /// 8の倍数で局所探索(pll候補, 結果)
    Sweep { cands: Vec<u32>, i: usize, best: (u32, f32), med: Vec<f32> },
    /// 位相を粗く振る
    PhaseCoarse { i: u8, best: (u8, f32), all: Vec<f32> },
    /// 位相を1刻みで詰める
    PhaseFine { list: Vec<u8>, i: usize, best: (u8, f32) },
}

struct AutoTune {
    phase: AutoPhase,
    /// この時刻まではボードの反映待ち
    wait_until: std::time::Instant,
    /// 待ち開始時点の測定回数。これより進んだ測定だけを採用する
    base_n: u64,
    /// 経過表示
    note: String,
    /// 最悪の位相(参考表示用)
    worst: f32,
    /// 半分にして測り直した回数
    attempt: u8,
}

const AUTO_PLL_MIN: u32 = 200;
const AUTO_PLL_MAX: u32 = 2304;

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
    /// 画枠パラメータ(ボードへCONFIGで送る値)。モードごとに最適値が違うので、
    /// ビルドし直さずにここで追い込む。
    tune_vbp: i32,
    tune_hs_offset: i32,
    tune_pll_divide: i32,
    /// TVPのアナログ映像帯域。0=最大 / 15=最小(約95MHz)
    tune_video_bw: u8,
    /// 1ラインまるごと送る(非黒範囲の最適化を切る)。
    /// 全黒行の直後の数行で範囲判定が壊れて行が欠ける不具合の回避策。
    tune_full_line: bool,
    /// フレーム間引き。0=毎フレーム / 1=2フレームに1回 …
    /// 受信が追いつかない機械での保険。音声は間引かない
    tune_frame_skip: u8,
    /// 復調を止めて生のYを見る(切り分け用。設定には保存しない)
    raw_view: bool,
    /// 表示するフィールド 0=織り込み / 1=偶数 / 2=奇数(同上)
    field_view: u8,
    /// 見た目の調整(彩度・明るさ・コントラスト・色相)。設定に保存する
    adjust: ntsc::Adjust,
    /// 管面の幾何に使う vtotal(スロット数)を平滑した値。**整数のまま使わない。**
    ///
    /// ★インターレースは 262.5 ライン/フィールドなので、ボードが測る vtotal は
    ///   262 と 263 を交互に返すのが正常。瞬間値を幾何に使うと縦のスケールが
    ///   フレームごとに 0.38% 変わり、絵が上下に震える(実機で「横線の縦位置が
    ///   フレームごとに動いて二重線に見える」形で出た。片フィールド表示でも
    ///   残ったので、織り込みではなく幾何側だと切り分けられた)。
    /// (描画中に更新するので Cell。paint_tube を &mut self にすると波及が大きい)
    vtotal_smooth: std::cell::Cell<f32>,
    /// 平滑をリセットする判定用。モードが変わったら追従を待たずに飛ばす
    vtotal_mode_id: std::cell::Cell<u16>,
    /// 直近で入力設定を書き込んだソース(ラベル, 件数)。パネルの確認表示用。
    input_regs_sent: Option<(&'static str, usize)>,
    /// 映像ソースのプロファイル名(profiles::PROFILES の key)。空文字は「自動」。
    /// pll_divide を絵の内容ではなく fH とドットクロック候補から決めるのに使う
    source_profile: String,
    tune_phase: u8,
    /// 送信したがまだボードの応答で確認できていない値(key → (値, 送信時刻))。
    /// 応答が返るまでは自分の値を優先し、押した瞬間に元へ戻って見えるのを防ぐ。
    tune_pending: std::collections::HashMap<u16, (u32, std::time::Instant)>,
    /// 次に現在値を問い合わせる時刻。全キーが揃うまで繰り返す
    tune_get_at: Option<std::time::Instant>,
    /// ボードの現在値を取り込み済みか。起動時に1回だけ合わせる
    tune_synced: bool,
    /// 現在のウィンドウ内寸(保存用)
    window_size: egui::Vec2,
    /// 中央の描画領域と画のサイズ。ウィンドウを等倍に合わせるのに使う
    last_avail: egui::Vec2,
    last_tex: egui::Vec2,
    /// 起動後に一度だけ自動で等倍に合わせたか
    did_autofit: bool,
    /// 次のフレームでウィンドウを等倍に合わせる
    want_fit: bool,
    /// 受信中フレームの寸法(GPUテクスチャは callback_resources 側が持つ)
    frame_size: (u32, u32),
    render_state: Option<eframe::egui_wgpu::RenderState>,
    seen_gen: u64,
    /// 画面回転 0/1/2/3 = 時計回りに 0/90/180/270 度
    rotate: u32,
    /// 表示する切り出し範囲[画素]。w か h が 0 なら全体を表示
    crop: [u32; 4],
    /// 管面の縦横比(幅/高さ)。0 なら有効映像の比のまま
    tube_aspect: f32,
    /// 右の操作パネルを表示するか。隠すと映像(と枠)だけになる。
    /// 隠すと戻すUIが無くなるので、Tabキーで必ず戻せるようにしてある。
    show_panel: bool,
    /// モニタの枠(ベゼル)の名前。空文字は枠なし
    bezel: String,
    /// 枠を一時的に隠す。プルダウンの選択(bezel)は保ったまま切り替えたいので、
    /// 「なし」を選ぶのとは別に持つ。B キーで切り替える。
    bezel_off: bool,
    /// 枠のテクスチャ。ラスタライズした幅と一緒に持ち、幅が変わったら作り直す
    bezel_tex: Option<(egui::TextureHandle, u32)>,
    /// 補間 0=ニアレスト 1=バイリニア 2=sharp-bilinear
    filter: u32,
    /// インターレース時の残光(1.0=減衰しない)。設定に保存する
    interlace_decay: f32,
    /// 管面が映す時間窓 [h0,h1,v0,v1]。CRTのH位置/H幅・V位置/V幅に相当
    window: [f32; 4],
    tube_time_based: bool,
    /// いまの帯域のモニタプロファイル [H幅, H位置, V幅, V位置]。
    /// 1周期のうち管面に出る割合と、周期の中心からのずれ
    mon: [f32; 4],
    /// 帯域ごとのモニタプロファイル(CZ-612Dのような3モードディスプレイに相当)
    mon_bands: std::collections::BTreeMap<u32, [f32; 4]>,
    /// 帯域ごとの [pll_divide, 位相]
    band_pll: std::collections::BTreeMap<u32, [u32; 2]>,
    /// いまの周波数帯[kHz]。0 は未確定
    band_khz: u32,
    auto: Option<AutoTune>,
    /// 自動調整の結果表示(完了後に残す)
    auto_done: Option<String>,
    /// モードごとの切り出し・回転。キーは fH_vtotal_htotal
    modes: std::collections::BTreeMap<String, [u32; 6]>,
    /// いま適用しているモードのキー(変化したら保存・復元する)
    mode_key: String,
    rx_error: Option<String>,
    subscribe_to: Option<String>,
    no_vsync: bool,
    pace: PaceMeter,
    /// MimicX へのキー転送。起動時は常にOFF(保存しない)。ONのまま起動すると
    /// キーが黙って実機へ流れるので、毎回明示的に入るようにしている
    remote: remote_input::RemoteInput,
    /// 転送ON/OFF操作(⌘+Shift+ESC / F12)の検出。押下の瞬間だけを拾う
    remote_toggle: remote_input::ToggleDetect,
    /// 物理キーの取り出し口。egui の Key では JIS の ¥/_/かな が落ちるので、
    /// AppKit のイベントを直接見る(keytap.rs)
    keytap: keytap::KeyTap,
    /// NIC受信バッファーの確認結果。ボードが見つかってから1回だけ調べる
    /// (レジストリを読むだけなので同期でよい)
    netcheck: Option<netcheck::Buffers>,
    /// 起動時のダイアログを出すか。**確度の高い設定チェックのときだけ**立てる
    /// (ロス率は推定なうえセッション中に発火するので、割り込ませない)
    netcheck_modal: bool,
    /// 「今後表示しない」。設定に保存する
    netcheck_muted: bool,
}

impl ViewerApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        port: u16,
        subscribe_to: Option<String>,
        target_mac: Option<[u8; 6]>,
        no_vsync: bool,
        decay: f32,
    interlace_decay: f32,
        audio: receiver::AudioOpts,
        cfg: settings::Settings,
    ) -> Self {
        let audio_source = audio.source;
        install_cjk_font(&cc.egui_ctx);
        // 通常モードもフルスクリーンと同じシェーダで描く。見え方が食い違わないように。
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            // ★描画先の色空間を出す。**ここが sRGB でないと中間調が暗くなる。**
            //   テクスチャは Rgba8UnormSrgb なのでサンプラがリニアへ復号する。
            //   出力先が非sRGBだとリニアのまま書かれ、ガンマ2.2ぶん暗く出る。
            eprintln!("render target format: {:?}", rs.target_format);
            let blit = render::EguiBlit::new(&rs.device, rs.target_format);
            rs.renderer.write().callback_resources.insert(blit);
        }
        let shared = Arc::new(receiver::Shared::default());
        let ctx = cc.egui_ctx.clone();
        let rx_error = receiver::spawn(
            receiver::Config { port, subscribe_to: subscribe_to.clone(), target_mac, decay, interlace_decay, audio },
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
            tune_vbp: cfg.tune_vbp,
            tune_hs_offset: cfg.tune_hs_offset,
            tune_pll_divide: cfg.tune_pll_divide,
            tune_video_bw: cfg.tune_video_bw,
            tune_full_line: cfg.tune_full_line,
            tune_frame_skip: cfg.tune_frame_skip,
            tune_phase: cfg.tune_phase,
            raw_view: false,
            field_view: 0,
            adjust: ntsc::Adjust {
                hue_deg: cfg.adj_hue_deg,
                saturation: cfg.adj_saturation,
                brightness: cfg.adj_brightness,
                contrast: cfg.adj_contrast,
                gamma: if cfg.adj_gamma > 0.0 { cfg.adj_gamma } else { 1.0 },
            },
            vtotal_smooth: std::cell::Cell::new(0.0),
            vtotal_mode_id: std::cell::Cell::new(u16::MAX),
            input_regs_sent: None,
            source_profile: cfg.source_profile.clone(),
            tune_pending: Default::default(),
            tune_get_at: None,
            tune_synced: false,
            window_size: egui::vec2(cfg.window_w, cfg.window_h),
            last_avail: egui::Vec2::ZERO,
            last_tex: egui::Vec2::ZERO,
            did_autofit: false,
            want_fit: false,
            frame_size: (0, 0),
            render_state: cc.wgpu_render_state.clone(),
            seen_gen: 0,
            rotate: cfg.rotate,
            crop: [cfg.crop_x, cfg.crop_y, cfg.crop_w, cfg.crop_h],
            tube_aspect: cfg.tube_aspect,
            show_panel: cfg.show_panel,
            bezel: cfg.bezel.clone(),
            bezel_off: cfg.bezel_off,
            bezel_tex: None,
            filter: cfg.filter,
            interlace_decay: cfg.interlace_decay,
            window: cfg.window,
            tube_time_based: cfg.tube_time_based,
            mon: cfg.mon,
            mon_bands: cfg.mon_bands.clone(),
            band_pll: cfg.band_pll.clone(),
            band_khz: 0,
            auto: None,
            auto_done: None,
            modes: cfg.modes.clone(),
            mode_key: String::new(),
            rx_error,
            subscribe_to,
            no_vsync,
            pace: PaceMeter::new(),
            remote: remote_input::RemoteInput::default(),
            remote_toggle: Default::default(),
            keytap: keytap::KeyTap::install(&cc.egui_ctx),
            netcheck: None,
            netcheck_modal: false,
            netcheck_muted: cfg.netcheck_muted,
        }
    }

    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let generation = self.shared.frame_gen.load(Ordering::Acquire);
        if generation == self.seen_gen {
            return;
        }
        self.seen_gen = generation;
        self.pace.on_new_frame();
        // ★**ロックの順序が逆だった。** 以前はフレームのロックを持ったまま
        //   `rs.renderer.write()` を取りに行っていた。描画中はレンダラのロックが
        //   取れないので、paint が 19ms かかる間ずっとフレームのロックを握り続け、
        //   受信側の `shared.frame.lock()` がその間ブロックされていた。
        //   レンダラを先に取れば、フレームのロックはGPU転送の間だけで済む。
        //   (両方を取るのはここだけなので、順序を入れ替えても行き詰まらない)
        if let Some(rs) = self.render_state.as_ref() {
            let mut w = rs.renderer.write();
            let guard = self.shared.frame.lock().unwrap();
            let Some(frame) = guard.as_ref() else { return };
            self.frame_size = (frame.width as u32, frame.height as u32);
            if let Some(b) = w.callback_resources.get_mut::<render::EguiBlit>() {
                b.upload(&rs.device, &rs.queue, &frame.rgba,
                         frame.width as u32, frame.height as u32);
            }
        } else {
            let guard = self.shared.frame.lock().unwrap();
            if let Some(frame) = guard.as_ref() {
                self.frame_size = (frame.width as u32, frame.height as u32);
            }
        }
        let _ = ctx;
    }
}

impl ViewerApp {
    /// いま使う枠。bezel_off なら枠なしとして扱う(選択は保つ)。
    fn active_bezel(&self) -> Option<&'static bezel::Bezel> {
        if self.bezel_off {
            None
        } else {
            bezel::by_key(&self.bezel)
        }
    }

    /// 管面(GPUで描く映像)を指定した矩形へ描く。
    ///
    /// Retina では論理座標と実画素が違う。sharp-bilinear は出力画素数を基準に
    /// 境目の幅を決めるので、実画素で渡す。
    fn paint_tube(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let ppp = ui.ctx().pixels_per_point();
        ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
            rect,
            render::Callback {
                params: self.render_params(),
                dst: (rect.width() * ppp, rect.height() * ppp),
            },
        ));
    }

    /// 現在のUI状態を設定として書き出す。スライダー操作中に毎フレーム書くのを
    /// 避けるため、変更を記録して少し待ってから1回だけ保存する。
    fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(std::time::Instant::now());
    }

    fn flush_settings(&mut self) {
        if self.band_khz > 0 {
            let k = self.band_khz;
            self.mon_bands.insert(k, self.mon);
            self.band_pll.insert(k, [self.tune_pll_divide.max(0) as u32,
                                     self.tune_phase as u32]);
        }
        if !self.mode_key.is_empty() {
            let k = self.mode_key.clone();
            self.modes.insert(k, [self.crop[0], self.crop[1], self.crop[2],
                                  self.crop[3], self.rotate, self.tune_phase as u32]);
        }
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
            rotate: self.rotate,
            tube_aspect: self.tube_aspect,
            show_panel: self.show_panel,
            netcheck_muted: self.netcheck_muted,
            bezel: self.bezel.clone(),
            bezel_off: self.bezel_off,
            filter: self.filter,
            interlace_decay: self.interlace_decay,
            adj_hue_deg: self.adjust.hue_deg,
            adj_saturation: self.adjust.saturation,
            adj_brightness: self.adjust.brightness,
            adj_contrast: self.adjust.contrast,
            adj_gamma: self.adjust.gamma,
            window: self.window,
            tube_time_based: self.tube_time_based,
            mon: self.mon,
            mon_bands: self.mon_bands.clone(),
            band_pll: self.band_pll.clone(),
            modes: self.modes.clone(),
            crop_x: self.crop[0],
            crop_y: self.crop[1],
            crop_w: self.crop[2],
            crop_h: self.crop[3],
            window_w: self.window_size.x,
            window_h: self.window_size.y,
            tune_vbp: self.tune_vbp,
            tune_hs_offset: self.tune_hs_offset,
            tune_pll_divide: self.tune_pll_divide,
            tune_video_bw: self.tune_video_bw,
            tune_full_line: self.tune_full_line,
            tune_frame_skip: self.tune_frame_skip,
            tune_phase: self.tune_phase,
            source_profile: self.source_profile.clone(),
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
    fn render_params(&self) -> render::Params {
        let m = self.shared.mode.lock().unwrap().clone();
        // 実測した有効映像の外接矩形。MODEの hactive は送出フレームの幅(常に1024)で
        // 有効映像の幅ではないため、管面の中心と縦の連動にはこの実測値を使う
        // 1ラインが何スロットを占めるか(MODEの mflags bit0 = インターレース)
        let slot_k: u32 = match m.as_ref() {
            Some(m) if m.mflags & 0x0001 != 0 => 1,
            _ => 2,
        };
        // vtotal を平滑する。**262/263 の交互は正常なので、その平均(262.5)を使う。**
        // モードが変わったときだけ即座に飛ばす(追従を待つと一瞬絵が伸びる)。
        {
            let raw = m.as_ref().map_or(0.0, |m| (m.vtotal as u32 * slot_k) as f32);
            let id = m.as_ref().map_or(u16::MAX, |m| m.mode_id as u16);
            let reset = id != self.vtotal_mode_id.get();
            self.vtotal_smooth
                .set(render::smooth_vtotal(self.vtotal_smooth.get(), raw, reset));
            if raw > 0.0 {
                self.vtotal_mode_id.set(id);
            }
        }
        let act = {
            let st = self.shared.stats.lock().unwrap();
            // span_* は「明るい画素が1つでもある行/列」の外接矩形。active_* は
            // 「20%以上の行が明るい列」なので、細い線ばかりの画面ではベタ塗りの
            // ブロックだけに縮む(実機で幅125になった)。管面の幾何は絵の広がりを
            // 知りたいので span_* を使い、無ければ active_* に落とす
            if st.span_w > 64 {
                (st.span_x as u32, st.span_y as u32, st.span_w as u32, st.span_h as u32)
            } else {
                (st.active_x as u32, st.active_y as u32,
                 st.active_w as u32, st.active_h as u32)
            }
        };
        render::Params {
            rotate: self.rotate,
            tube: self.tube_aspect,
            filter: self.filter,
            htotal: m.as_ref().map_or(0, |m| m.htotal as u32),
            // 行位置は半ライン単位のスロット。1VSYNC周期に何スロット入るかは
            // 織り込みの有無で変わる:
            //   プログレッシブ  1ラインが2スロット      → 2*vtotal
            //   インターレース  折り返して2フィールドが  → vtotal
            //                   交互に入るので1ラインが1スロット
            // これを間違えると縦が2倍に引き伸ばされる(実機で絵が上半分に収まった)。
            vtotal: self.vtotal_smooth.get(),
            // offset_px はライン内の絶対位置で来るので、ここでずらす必要はない。
            // pll_divide を変えても絵が動かないのはこのため(hs_offset は取り込み
            // 窓の設定であって、描画位置とは無関係になった)
            hs_offset: 0,
            vbp: self.tune_vbp.max(0) as u32 * slot_k,
            window: self.window,
            fh_hz: m.as_ref().map_or(0, |m| (m.hfreq_mhz_x1000 / 1000) as u32),
            hactive: m.as_ref().map_or(0, |m| m.hactive as u32),
            vactive: m.as_ref().map_or(0, |m| m.vactive as u32),
            time_based: self.tube_time_based,
            h_size: self.mon[0],
            h_pos: self.mon[1],
            v_size: self.mon[2],
            v_pos: self.mon[3],
            act_x: act.0, act_y: act.1, act_w: act.2, act_h: act.3,
        }
    }

    /// 実行時に調整できるキーの一覧。読み戻しと表示の同期に使う
    const TUNE_KEYS: [u16; 7] = [
        protocol::CFG_KEY_VBP,
        protocol::CFG_KEY_HS_OFFSET,
        protocol::CFG_KEY_PLL_DIVIDE,
        protocol::CFG_KEY_VIDEO_BW,
        protocol::CFG_KEY_PHASE,
        protocol::CFG_KEY_FULL_LINE,
        protocol::CFG_KEY_FRAME_SKIP,
    ];

    /// ボードの現在値を表示へ反映する。
    ///
    /// 読み戻さないと表示と実体が食い違う。設定はボードの電源で
    /// 消え、Viewerの保存値とは別々に動くため)。送信パケットが落ちたときも
    /// ずれるので、応答で確認できた値を正としてUIを合わせる。
    /// いまの入力モードのキー。同じ31kHzでも 768x512 と 256x256 は同期信号が
    /// 同一なので、htotal(= pll_divide)まで含めないと区別できない。
    fn current_mode_key(&self) -> Option<String> {
        let m = self.shared.mode.lock().unwrap().clone()?;
        if m.htotal == 0 || m.vtotal == 0 || m.hfreq_mhz_x1000 == 0 {
            return None;
        }
        let fh = (m.hfreq_mhz_x1000 as f64 / 1000.0 / 100.0).round() as u32;
        Some(format!("{fh}_{}_{}", m.vtotal, m.htotal))
    }

    /// モードが変わったら、いまの切り出し・回転を旧モードへ保存し、
    /// 新モードの値を復元する。未知のモードでは切り出しを解除する
    /// (別モードの座標で切り出すと絵が切れるため)。
    fn follow_mode(&mut self) {
        let Some(key) = self.current_mode_key() else { return };
        if key == self.mode_key {
            return;
        }
        if !self.mode_key.is_empty() {
            let old = self.mode_key.clone();
            self.modes.insert(old, [self.crop[0], self.crop[1], self.crop[2],
                                    self.crop[3], self.rotate, self.tune_phase as u32]);
        }
        match self.modes.get(&key) {
            Some(v) => {
                self.crop = [v[0], v[1], v[2], v[3]];
                self.rotate = v[4].min(3);
                // 位相はモードごとに最適値が違うので、復元したらボードへ送り直す
                let ph = (v[5].min(31)) as u8;
                if ph != self.tune_phase {
                    self.tune_phase = ph;
                    self.send_cfg(protocol::CFG_KEY_PHASE, ph as u32);
                }
            }
            None => {
                self.crop = [0; 4];
                self.rotate = 0;
            }
        }
        self.mode_key = key;
        self.mark_settings_dirty();
        self.want_fit = true;
    }

    /// pll_divide を変える。hs_offset を同じ比で追従させて絵が動かないようにする。
    ///
    /// サンプル k の水平位置は (hs_offset + k)/htotal で htotal = pll_divide。
    /// k は pll_divide に比例して増えるので式としては時間ベースだが、hs_offset は
    /// サンプル単位なので固定のままだと取り込み開始「時刻」が変わり、絵が横へ跳ぶ。
    /// pll_divide はドットクロック再生のためのもので描画位置とは無関係でなければ
    /// ならない。いまは hs_offset を常に0で運用しているので実質的な効果は無いが、
    /// 0以外にしたときに壊れないようにしておく。
    /// pll_divide を Viewer 側が動かしてよいか。
    ///
    /// **YC8(生8bit)のときは動かしてはいけない。** コンポジット/S端子の
    /// サンプルレートは規格で決まっていて(NTSC 8fsc = 227.5×8 = 1820
    /// サンプル/ライン)、実測で探すものではない。8サンプル/周期という
    /// 前提が崩れると副搬送波の直交復調が成立しなくなる。
    ///
    /// 実際に踏んだ: コンポジットで 1820 を入れた直後に Viewer が
    /// **帯域ごとの保存値と自動調整の初期値(2304)で上書き**し、
    /// 10.13サンプル/周期になっていた(絵は出るので気づきにくい)。
    ///
    /// 入力方式ごとの値は `retrocastx.videoin` が入れる。Viewer は触らない。
    fn pll_locked_by_format(&self) -> bool {
        matches!(self.shared.mode.lock().unwrap().as_ref(),
                 Some(m) if m.pixfmt == protocol::PIXFMT_YC8)
    }

    fn set_pll(&mut self, new: u32) {
        // 自動でここへ来る経路(帯域切替の復元など)を1か所で止める。
        // 手動の pll_div 欄は別経路なので、逃げ道としては残る。
        if self.pll_locked_by_format() {
            return;
        }
        let old = self.tune_pll_divide.max(1) as f32;
        let hs = ((self.tune_hs_offset as f32 * (new as f32 / old)).round() as i32)
            .clamp(0, (new / 2) as i32);
        self.tune_pll_divide = new as i32;
        self.send_cfg(protocol::CFG_KEY_PLL_DIVIDE, new);
        if hs != self.tune_hs_offset {
            self.tune_hs_offset = hs;
            self.send_cfg(protocol::CFG_KEY_HS_OFFSET, hs as u32);
        }
    }

    /// CONFIGを1つ送る。UIの外(モード追従など)から使う
    fn send_cfg(&mut self, key: u16, val: u32) {
        self.tune_pending.insert(key, (val, std::time::Instant::now()));
        self.shared.config_queue.lock().unwrap().push((key, val));
        self.mark_settings_dirty();
    }

    /// いまの有効映像が何µsか。管面の掃引時間を決める基準になる
    fn active_us(&self) -> Option<f32> {
        let aw = {
            let st = self.shared.stats.lock().unwrap();
            if st.span_w > 64 { st.span_w as f32 } else { st.active_w as f32 }
        };
        let m = self.shared.mode.lock().unwrap().clone()?;
        let fh_khz = m.hfreq_mhz_x1000 as f32 / 1_000_000.0;
        if aw <= 0.0 || fh_khz <= 1.0 || m.htotal == 0 {
            return None;
        }
        Some(aw / m.htotal as f32 * (1000.0 / fh_khz))
    }

    /// H-PLL制御(TVPレジスタ03h)の値をモードから計算する。
    ///
    /// データシート 03h の定義:
    ///   VCO 00 = Ultra low (KVCO  75)  PCLK < 36 MHz
    ///       01 = Low       (KVCO  85)  36 ≤ PCLK < 70
    ///       10 = Medium    (KVCO 150)  70 ≤ PCLK < 135   ← 既定(A8h)
    ///       11 = High      (KVCO 200)  135 ≤ PCLK ≤ 165
    ///   ICP = 40 × KVCO / (pixels per line)
    ///
    /// データシートは「VCOレンジ制御の目的はノイズ性能の改善」と書いている。
    /// レンジ外で動かすと位相ノイズが増え、ラインごとにサンプリング位相が揺れて
    /// 行単位の横ずれになる(既定のA8h=Mediumを全モードに使っていて実機で発生)。
    /// X68000は全モード PCLK < 36MHz なので Ultra low が正しい。
    ///
    /// 計算式が Table 4 と一致することは確認済み(720×480p: 27MHz/858 → ICP 3.50
    /// → 18h)。
    /// 想定される最大のライン周波数。VCOレンジを決めるときの最悪ケースに使う。
    /// レトロ機の水平周波数は 15〜32kHz の範囲なので、上端を取れば
    /// 「PLLが作る必要のある最大周波数」を fH の測定値に依存せず見積もれる。
    const FH_MAX_HZ: f64 = 32_000.0;

    fn pll_ctl_for(pclk_hz: f64, pix_per_line: u32) -> u8 {
        let (vco, kvco) = if pclk_hz < 36.0e6 {
            (0u8, 75.0)
        } else if pclk_hz < 70.0e6 {
            (1, 85.0)
        } else if pclk_hz < 135.0e6 {
            (2, 150.0)
        } else {
            (3, 200.0)
        };
        let icp = (40.0 * kvco / pix_per_line.max(1) as f64).round().clamp(0.0, 7.0) as u8;
        (vco << 6) | (icp << 3)
    }

    /// fH[kHz] を呼称の帯域へ分類する。
    ///
    /// 丸めると 24.698kHz が 25 になって呼称とずれる。マルチスキャンモニタの
    /// インジケータと同じ 15 / 24 / 31 に寄せる。範囲外は丸めた値をそのまま使う。
    fn band_of(fh_khz: f32) -> u32 {
        if fh_khz < 20.0 {
            15
        } else if fh_khz < 28.0 {
            24
        } else if fh_khz < 40.0 {
            31
        } else {
            fh_khz.round() as u32
        }
    }

    /// 周波数帯が変わったら、いまの管面設定を旧帯域へ保存し、新帯域の値を復元する。
    ///
    /// 実機のマルチスキャンモニタは周波数帯ごとに走査速度を切り替えて管面いっぱいに
    /// 走らせる。掃引時間を絶対時間で共通にすると、15kHz(有効52.7µs)に合わせた
    /// 管面では31kHz(同22.1µs)が42%の大きさになってしまう。帯域ごとに持てば、
    /// 同じ帯域内のモードは同じ大きさで表示される(31kHzの512x512と768x512は
    /// 有効時間がどちらも22.1µsで実際に一致する)。
    fn follow_band(&mut self) {
        let khz = {
            let m = self.shared.mode.lock().unwrap().clone();
            match m {
                Some(m) if m.hfreq_mhz_x1000 > 1_000_000 => {
                    Self::band_of(m.hfreq_mhz_x1000 as f32 / 1_000_000.0)
                }
                _ => return,
            }
        };
        if khz == self.band_khz {
            return;
        }
        if self.band_khz > 0 {
            let old = self.band_khz;
            self.mon_bands.insert(old, self.mon);
            self.band_pll.insert(old, [self.tune_pll_divide.max(0) as u32,
                                       self.tune_phase as u32]);
        }
        // 帯域ごとの pll_divide / 位相を復元してボードへ送る。
        // pll_divide はモードごとに正解が違い、位相も帯域ごとに最適値が変わるので、
        // 1つだけ持つと帯域を切り替えるたびに合わせ直しになる。
        if let Some(v) = self.band_pll.get(&khz).copied() {
            let pll = (v[0] as i32).clamp(200, 2304);
            if pll != self.tune_pll_divide {
                self.set_pll(pll as u32);
            }
            let ph = (v[1].min(31)) as u8;
            if ph != self.tune_phase {
                self.tune_phase = ph;
                self.send_cfg(protocol::CFG_KEY_PHASE, ph as u32);
            }
        }
        // 未知の帯域は「1周期まるごとが管面に出る」から始める。管面の横幅は 1/fH
        // そのものなので、これで必ず全体が収まる。あとは実機のモニタに合わせて
        // H幅/V幅を1未満へ詰めていけばよい(帰線の間ビームは戻っているので、
        // 実際のCRTは 0.85〜0.95 あたりが近い)。
        self.mon = self.mon_bands.get(&khz).copied().unwrap_or([1.0, 0.0, 1.0, 0.0]);
        self.band_khz = khz;
        // H-PLLのVCOレンジ/チャージポンプはピクセルクロックとpixels per lineで
        // 決まるので、モードが変わったら計算して送り直す
        // VCOレンジは「報告されたドットクロック」ではなく pll_divide から決める。
        //
        // 報告値は誤ったロックの結果でもあり得るので、それを根拠にすると誤りが
        // 固定される。実機で発生: pll_div 1182 に対しレンジを Ultra low(<36MHz)に
        // 設定したため、必要な 37.2MHz を出せずPLLが半分のライン周期にロックし、
        // その状態が自己維持した。pll_divide × 想定最大fH で見積もれば、レンジが
        // 足りなくなることはない(少し高めのレンジになりノイズ性能を僅かに譲るが、
        // 誤ロックより害が小さい)。
        let ht = self.tune_pll_divide.max(1) as u32;
        let v = Self::pll_ctl_for(ht as f64 * Self::FH_MAX_HZ, ht);
        self.send_cfg(protocol::CFG_KEY_PLL_CTL, v as u32);
        self.mark_settings_dirty();
        self.want_fit = true;
    }

    fn sync_tune_from_board(&mut self) {
        // 取り込みは起動時の1回だけにする。毎フレーム反映すると、手で入力した値が
        // send を押す前にボードの値へ戻されてしまう(未送信の変更は pending に
        // 入らないため)。ずれ直したときは「読む」で取り直せる。
        if self.tune_synced {
            return;
        }
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
        for (key, val) in &state {
            let (key, val) = (*key, *val);
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
                protocol::CFG_KEY_VIDEO_BW => self.tune_video_bw = val as u8,
                protocol::CFG_KEY_FULL_LINE => self.tune_full_line = val != 0,
                protocol::CFG_KEY_FRAME_SKIP => self.tune_frame_skip = val as u8,
                protocol::CFG_KEY_PHASE => self.tune_phase = (val as u8).min(31),
                _ => {}
            }
        }
        // 完了判定は「いま適用したのと同じスナップショット」で行う。config_state を
        // 取り直すと、スナップショット後に届いた応答を適用しないまま完了扱いになり、
        // 以後この関数が早期returnして二度と読まれない(hs_offset だけ古い値のまま
        // 残る、という形で実機で発覚した)。
        if Self::TUNE_KEYS.iter().all(|k| state.contains_key(k)) {
            self.tune_synced = true;
        }
    }

    /// マルチスキャンモニタの周波数インジケータ。いま同期している帯域が点灯する。
    ///
    /// 実機のモニタは 15/24/31kHz のどれで同期しているかをLEDで示していた。
    /// 帯域は管面のプリセットを選ぶ単位でもあるので、いまどれなのかが常に
    /// 見えている方が分かりやすい。
    fn band_leds(&self, ui: &mut egui::Ui) {
        const BANDS: [u32; 3] = [15, 24, 31];
        ui.horizontal(|ui| {
            for b in BANDS {
                let on = self.band_khz == b;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(52.0, 18.0), egui::Sense::hover());
                let p = ui.painter();
                // 消灯時も枠は見えるようにして、何段あるかが分かるようにする
                let (fill, text) = if on {
                    (egui::Color32::from_rgb(60, 220, 120), egui::Color32::BLACK)
                } else {
                    (egui::Color32::from_rgb(38, 40, 44), egui::Color32::from_gray(110))
                };
                p.rect_filled(rect, 3.0, fill);
                p.rect_stroke(rect, 3.0, egui::Stroke::new(1.0,
                              egui::Color32::from_gray(70)), egui::StrokeKind::Inside);
                p.text(rect.center(), egui::Align2::CENTER_CENTER,
                       format!("{b}kHz"), egui::FontId::monospace(11.0), text);
            }
            // 呼称の帯域に当てはまらない周波数(他機種など)はそのまま出す
            if self.band_khz > 0 && !BANDS.contains(&self.band_khz) {
                ui.monospace(format!("{}kHz", self.band_khz));
            }
            if self.band_khz == 0 {
                ui.weak("同期なし");
            }
        });
    }

    /// TVPが許す pll_divide の下限(8の倍数に切り上げ)。
    ///
    /// TVP7002のピクセルクロックは 12MHz 未満だと動作が保証されない(データシート)。
    /// 実測でも 11.25MHz から崩れ始め、9.72MHz では白の青/赤比が上下で0.43違った。
    /// X68000の15kHzモードは本来の htotal が 608(9.72MHz)なので、**1:1で
    /// サンプリングできない**。下限を満たす最小の整数倍(2倍=1216)を使う。
    /// 整数倍なら位相ロックするのでドット間の混ざりは起きず、各ドットが2サンプルに
    /// なるだけで済む。
    fn pll_min_hw(&self) -> u32 {
        match self.shared.mode.lock().unwrap().as_ref() {
            Some(m) if m.hfreq_mhz_x1000 > 0 => {
                let fh = m.hfreq_mhz_x1000 as f64 / 1000.0;
                let v = (12.0e6 / fh).ceil() as u32;
                ((v + 7) / 8 * 8).clamp(AUTO_PLL_MIN, AUTO_PLL_MAX)
            }
            _ => AUTO_PLL_MIN,
        }
    }

    /// 自動調整を開始する。上限まで過剰サンプルするところから始める
    fn auto_start(&mut self) {
        // YC8 は pll_divide が規格で決まっているので探索してはいけない
        if self.pll_locked_by_format() {
            self.auto_done = Some("YC8では pll_divide は規格値。自動調整しません".into());
            return;
        }
        self.send_cfg(protocol::CFG_KEY_PLL_DIVIDE, AUTO_PLL_MAX);
        self.tune_pll_divide = AUTO_PLL_MAX as i32;
        self.auto = Some(AutoTune {
            phase: AutoPhase::Oversample,
            wait_until: std::time::Instant::now() + std::time::Duration::from_millis(1800),
            base_n: self.shared.stats.lock().unwrap().tune_n,
            note: "過剰サンプル中...".into(),
            worst: 0.0,
            attempt: 0,
        });
    }

    /// 新しい測定が来ていれば (鋭さ, 占有率) を返す。まだなら None
    fn auto_measure(&self) -> Option<(f32, f32)> {
        let a = self.auto.as_ref()?;
        if std::time::Instant::now() < a.wait_until {
            return None;
        }
        let st = self.shared.stats.lock().unwrap();
        // 待ち終了後の測定を2回以上見てから採る(境界の1枚は前の設定の絵かもしれない)
        if st.tune_n < a.base_n + 2 || st.sharp_h <= 0.0 {
            return None;
        }
        Some((st.sharp_h, st.occ_h))
    }

    /// 次の測定点へ進む
    fn auto_next(&mut self, key: u16, val: u32, note: String, settle_ms: u64) {
        self.send_cfg(key, val);
        match key {
            protocol::CFG_KEY_PLL_DIVIDE => self.tune_pll_divide = val as i32,
            protocol::CFG_KEY_PHASE => self.tune_phase = val as u8,
            _ => {}
        }
        let base = self.shared.stats.lock().unwrap().tune_n;
        if let Some(a) = self.auto.as_mut() {
            a.wait_until = std::time::Instant::now()
                + std::time::Duration::from_millis(settle_ms);
            a.base_n = base;
            a.note = note;
        }
    }

    /// 推定値の周りを8の倍数で局所探索する段へ入る
    fn auto_enter_sweep(&mut self, est: u32, note: String) {
        let lo = ((est as f32 * 0.93 / 8.0) as u32 * 8).max(self.pll_min_hw());
        let hi = ((est as f32 * 1.07 / 8.0) as u32 * 8).min(AUTO_PLL_MAX);
        let cands: Vec<u32> = (lo..=hi).step_by(8).collect();
        if cands.is_empty() {
            self.auto = None;
            return;
        }
        let first = cands[0];
        let n = cands.len();
        if let Some(a) = self.auto.as_mut() {
            a.phase = AutoPhase::Sweep { cands, i: 0, best: (first, 0.0), med: Vec::new() };
        }
        self.auto_next(protocol::CFG_KEY_PLL_DIVIDE, first,
                       format!("{note} 局所探索 1/{n}"), 700);
    }

    fn auto_step(&mut self) {
        let Some((sharp, occ)) = self.auto_measure() else { return };
        let phase = match self.auto.as_mut() {
            Some(a) => std::mem::replace(&mut a.phase, AutoPhase::Oversample),
            None => return,
        };
        match phase {
            AutoPhase::Oversample => {
                // 1:1の pll_divide ≒ 過剰サンプル時の pll_divide x 占有率。
                // 実測では真値に対して -3% の系統バイアスが残るので、局所探索で吸収する
                let est0 = (AUTO_PLL_MAX as f32 * occ).round() as u32;
                // TVPの下限を下回る場合は、下限を満たす最小の整数倍にする。
                // 整数倍なら位相ロックするのでドット間の混ざりは起きない。
                let pmin = self.pll_min_hw();
                let mut est = est0;
                let mut mult = 1u32;
                while est < pmin && mult < 8 {
                    mult += 1;
                    est = est0 * mult;
                }
                if occ > 0.9 || !(AUTO_PLL_MIN..=AUTO_PLL_MAX).contains(&est) {
                    if let Some(a) = self.auto.as_mut() {
                        a.note = format!("推定できません(占有率 {occ:.2})。\
                                          細かい模様が出ている画面で試してください");
                    }
                    // 元の値へ戻さず、ここで諦める(絵が出ない値に置き去りにしない)
                    let back = est.clamp(AUTO_PLL_MIN, AUTO_PLL_MAX);
                    self.send_cfg(protocol::CFG_KEY_PLL_DIVIDE, back);
                    self.tune_pll_divide = back as i32;
                    self.auto = None;
                    return;
                }
                let note = if mult > 1 {
                    format!("推定 {est0}(TVPの下限で{mult}倍= {est})→")
                } else {
                    format!("推定 {est} →")
                };
                self.auto_enter_sweep(est, note);
            }
            AutoPhase::Sweep { cands, i, mut best, mut med } => {
                if sharp > best.1 {
                    best = (cands[i], sharp);
                }
                med.push(sharp);
                let next = i + 1;
                if next < cands.len() {
                    let pll = cands[next];
                    let n = cands.len();
                    if let Some(a) = self.auto.as_mut() {
                        a.phase = AutoPhase::Sweep { cands, i: next, best, med };
                    }
                    self.auto_next(protocol::CFG_KEY_PLL_DIVIDE, pll,
                                   format!("局所探索 {}/{n}", next + 1), 700);
                } else {
                    // 突出度(中央値との比)が低いときは測定が信用できない
                    med.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let m = med[med.len() / 2];
                    let ratio = if m > 0.0 { best.1 / m } else { 0.0 };
                    if let Some(a) = self.auto.as_mut() {
                        a.phase = AutoPhase::PhaseCoarse { i: 0, best: (0, 0.0), all: Vec::new() };
                        a.note = format!("pll_div {} (突出度 {ratio:.2})→位相", best.0);
                    }
                    self.send_cfg(protocol::CFG_KEY_PLL_DIVIDE, best.0);
                    self.tune_pll_divide = best.0 as i32;
                    self.auto_next(protocol::CFG_KEY_PHASE, 0,
                                   format!("pll_div {} 突出度 {ratio:.2} → 位相 1/16", best.0),
                                   900);
                }
            }
            AutoPhase::PhaseCoarse { i, mut best, mut all } => {
                let cur = i * 2;
                if sharp > best.1 {
                    best = (cur, sharp);
                }
                all.push(sharp);
                if i + 1 < 16 {
                    let nxt = (i + 1) * 2;
                    if let Some(a) = self.auto.as_mut() {
                        a.phase = AutoPhase::PhaseCoarse { i: i + 1, best, all };
                    }
                    self.auto_next(protocol::CFG_KEY_PHASE, nxt as u32,
                                   format!("位相 {}/16", i + 2), 600);
                } else {
                    let worst = all.iter().cloned().fold(f32::MAX, f32::min);
                    // 粗い最良点の周辺を1刻みで詰める
                    let list: Vec<u8> = (0..5)
                        .map(|k| ((best.0 as i32 + k - 2).rem_euclid(32)) as u8)
                        .collect();
                    let first = list[0];
                    if let Some(a) = self.auto.as_mut() {
                        a.worst = worst;
                        a.phase = AutoPhase::PhaseFine { list, i: 0, best };
                    }
                    self.auto_next(protocol::CFG_KEY_PHASE, first as u32,
                                   "位相を詰めています".into(), 600);
                }
            }
            AutoPhase::PhaseFine { list, i, mut best } => {
                if sharp > best.1 {
                    best = (list[i], sharp);
                }
                let next = i + 1;
                if next < list.len() {
                    let ph = list[next];
                    if let Some(a) = self.auto.as_mut() {
                        a.phase = AutoPhase::PhaseFine { list, i: next, best };
                    }
                    self.auto_next(protocol::CFG_KEY_PHASE, ph as u32,
                                   "位相を詰めています".into(), 600);
                } else {
                    let worst = self.auto.as_ref().map_or(0.0, |a| a.worst);
                    let rel = if best.1 > 0.0 { worst / best.1 } else { 1.0 };
                    let attempt = self.auto.as_ref().map_or(0, |a| a.attempt);
                    let pll = self.tune_pll_divide as u32;
                    // 位相感度が1:1の指標になる。1ドット=1サンプルなら、位相を半ドット
                    // 動かせば全サンプルが中央から縁へ移るので鮮鋭度が大きく変わる。
                    // 変わらないなら「半分のサンプルが既に縁にいる」= 過剰サンプル。
                    // 実機の15kHzモードで最悪95%(=変わらない)のまま2.3倍の値に
                    // 着地したので、その場合は半分にして測り直す。
                    // 半分にできるのは、それがTVPの下限以上のときだけ。下限で
                    // 過剰サンプルを強いられている帯域(15kHz)では位相感度が弱いのが
                    // 正常なので、ここで下げてしまうとPLLが範囲外の無効な領域へ入る
                    // (実機で432まで落ちて絵が壊れた)。
                    if rel > 0.85 && attempt < 2 && pll / 2 >= self.pll_min_hw() {
                        if let Some(a) = self.auto.as_mut() {
                            a.attempt = attempt + 1;
                        }
                        self.send_cfg(protocol::CFG_KEY_PHASE, 16);
                        self.tune_phase = 16;
                        let half = pll / 2;
                        self.auto_enter_sweep(
                            half,
                            format!("位相感度が弱い({:.0}%)ので {half} で再測定", rel * 100.0));
                        return;
                    }
                    self.send_cfg(protocol::CFG_KEY_PHASE, best.0 as u32);
                    self.tune_phase = best.0;
                    self.auto = None;
                    let at_floor = pll <= self.pll_min_hw() + 8;
                    let judge = if at_floor {
                        "TVPの下限(12MHz)。このモードは1:1不可"
                    } else if rel <= 0.85 {
                        "1:1"
                    } else {
                        "要確認"
                    };
                    self.auto_done = Some(format!(
                        "pll_div {} / 位相 {} で完了({judge}: 最悪の位相は {:.0}%)",
                        pll, best.0, rel * 100.0));
                    self.want_fit = true;
                }
            }
        }
    }

    fn tune_ui(&mut self, ui: &mut egui::Ui) {
        self.auto_step();
        self.follow_band();
        self.follow_mode();
        self.sync_tune_from_board();
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
        // 入力欄の範囲は固定にする。DragValue は範囲外の値を黙って書き換えて
        // 保持するので、上限を pll_div から動的に決めると、読み戻しや pll_div の
        // 変更のたびに hs_offset が勝手に動き、それが送信されてしまった
        // (実機で hs_offset が 160→607 に化けた)。
        // 実際の上限(pll_div/2)は gateware 側で守られるので、ここでは警告に留める。
        row(ui, "hs_offset", &mut self.tune_hs_offset, 0, 2304,
            protocol::CFG_KEY_HS_OFFSET, &mut send);
        let hs_max = (self.tune_pll_divide / 2).max(1);
        if self.tune_hs_offset >= hs_max {
            ui.colored_label(
                egui::Color32::from_rgb(220, 170, 60),
                format!("hs_offset は {hs_max} 未満に(ボード側で頭打ちになります)"),
            );
        }
        // 下限は「ピクセルクロック12MHz」から決まる。TVP7002のPLLは12〜165MHzしか
        // 保証されておらず(データシート)、下回るとクランプが効かなくなって画面の
        // 下ほど色がずれる。実測でも 12.02MHz は正常、11.25MHz から崩れ始め、
        // 9.72MHz では白の青/赤比が上下で0.43も違った。
        // 上限はgateware側の制限と同じ。超えるとDATACLKがpixドメインのタイミング
        // 制約を超え、実機ではボードごとハングした(4095で14秒)。
        let pll_min = match self.shared.mode.lock().unwrap().as_ref() {
            Some(m) if m.hfreq_mhz_x1000 > 0 => {
                // fH[Hz] = hfreq_mhz_x1000 / 1000。pll_div >= 12e6 / fH
                let fh = m.hfreq_mhz_x1000 as f64 / 1000.0;
                ((12.0e6 / fh).ceil() as i32).clamp(200, 2304)
            }
            _ => 200,
        };
        // YC8(コンポジット/S端子)は pll_divide が規格で決まる。Viewerの
        // 自動経路(帯域ごとの復元・自動調整・send all)を止めていることを示す。
        let pll_locked = self.pll_locked_by_format();
        // ★送るのを止めたら**表示もボードの実値に合わせる**。片方だけ直すと、
        //   欄に古い値(自動調整が入れた2304など)が残ってボードの実値(1820)と
        //   食い違い、「効いていない」ように見える(実際そう見えた)。
        //   MODE の htotal はボードの pll_divide そのものなので、それを映す。
        if pll_locked {
            if let Some(ht) = self.shared.mode.lock().unwrap().as_ref()
                .map(|m| m.htotal as i32).filter(|h| *h > 0)
            {
                self.tune_pll_divide = ht;
            }
        }
        row(ui, "pll_div", &mut self.tune_pll_divide, pll_min, 2304,
            protocol::CFG_KEY_PLL_DIVIDE, &mut send);
        if pll_locked {
            ui.colored_label(
                egui::Color32::from_rgb(120, 200, 120),
                "YC8: pll_div は規格値(NTSC 8fsc=1820)。\n\
                 自動調整・プロファイル・send all では変更しません",
            );
        }
        if self.tune_pll_divide < pll_min {
            ui.colored_label(
                egui::Color32::from_rgb(220, 170, 60),
                format!("pll_div は {pll_min} 以上に(12MHz未満はTVPの範囲外)"),
            );
        }
        // 映像ソースのプロファイルから pll_div を決める。
        //
        // レトロPCのドットクロックは水晶を分周した有限個の値しか取らないので、
        // htotal = f_dot / fH が整数になる f_dot を選べば一意に決まる。fH は
        // pll_divide に依存しない絶対値で、MODEが持っている。絵の内容を一切
        // 見ないので、真っ黒な画面でも模様が無くても当たる(スペクトル探索の
        // 「自動調整」が変な値に着地するのはここが理由)。
        let fh_hz = self
            .shared
            .mode
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.hfreq_mhz_x1000 as f64 / 1000.0)
            .unwrap_or(0.0);
        // 映像ソースは「配線の方式」でもある。選んだらその入力設定をボードへ書く。
        //
        // ★**TVPのレジスタは電源で消える。** ビットストリームをSPIフラッシュに
        //   焼いても、pixfmt・入力MUX・同期の取り方・クランプ・ゲインは
        //   CONFIG で入れ直す必要がある。以前は毎回
        //   `python3 -m retrocastx.videoin apply composite` を叩いていた。
        //   同じ選択を選び直してもコンボは反応しないので、書き直しボタンも要る。
        ui.horizontal(|ui| {
            ui.monospace("映像ソース");
            let cur = self.source_profile.clone();
            let mut sel = cur.clone();
            let name = |k: &str| -> String {
                if k.is_empty() {
                    "自動".into()
                } else {
                    profiles::by_key(k).map_or_else(|| k.to_string(), |p| p.label.to_string())
                }
            };
            egui::ComboBox::from_id_salt("srcprof")
                .width(190.0)
                .selected_text(name(&cur))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, String::new(), "自動");
                    for p in profiles::PROFILES {
                        ui.selectable_value(&mut sel, p.key.to_string(), p.label);
                    }
                });
            let changed = sel != cur;
            if changed {
                self.source_profile = sel;
                self.mark_settings_dirty();
            }
            let prof = profiles::by_key(&self.source_profile);
            let has_regs = prof.is_some_and(|p| !p.input_regs.is_empty());
            let write = ui
                .add_enabled(has_regs, egui::Button::new("入力設定を書く"))
                .on_hover_text(
                    "TVPの入力MUX・同期の取り方・クランプ・ゲイン・伝送形式を書く。\n\
                     ボードの電源を入れ直すと消えるので、そのときはここを押す\n\
                     (焼き直しは不要。全部CONFIGで済む)",
                )
                .clicked();
            if let Some(p) = prof {
                if (changed || write) && !p.input_regs.is_empty() {
                    for (k, v, _) in p.input_regs {
                        send.push((*k, *v));
                    }
                    self.input_regs_sent = Some((p.label, p.input_regs.len()));
                }
            }
        });
        // ボードの伝送形式が選択と食い違っていたら言う。**電源断でTVPのレジスタが
        // 消えた状態がこれ**で、絵は出るが復調できない(コンポジットなのにRGB555)。
        if let Some(p) = profiles::by_key(&self.source_profile) {
            if let (Some(want), Some(have)) = (
                p.pixfmt(),
                self.shared.mode.lock().unwrap().as_ref().map(|m| m.pixfmt),
            ) {
                if want != have {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 170, 60),
                        format!(
                            "ボードの伝送形式が {have}(このソースは {want})。\
                             電源を入れ直して設定が消えた可能性 → 「入力設定を書く」"
                        ),
                    );
                }
            }
        }
        if let Some((label, n)) = self.input_regs_sent {
            ui.monospace(format!("入力設定を書いた: {label} ({n}件)"));
        }
        // 見た目の調整。**復調の校正とは別に持つ。**
        //
        // 信号内の基準(同期40 IRE / バースト40 IRE p-p)に合わせた結果が「正しい」絵で、
        // 既定値はそこを指す。ここはその上に載せる好みの調整。校正の方を歪めると
        // 「どこまでが信号でどこからが好みか」が分からなくなり、後で数値で追えない。
        ui.horizontal(|ui| {
            ui.monospace("画調");
            if ui.small_button("既定へ").on_hover_text(
                "信号どおり(彩度1.00 明るさ0 コントラスト1.00 色相0°)へ戻す")
                .clicked()
            {
                self.adjust = ntsc::Adjust::default();
                *self.shared.adjust.lock().unwrap() = Some(self.adjust);
                self.mark_settings_dirty();
            }
            if self.adjust != ntsc::Adjust::default() {
                ui.colored_label(egui::Color32::from_rgb(220, 170, 60), "信号どおりではない");
            }
        });
        {
            let mut a = self.adjust;
            let mut ch = false;
            let mut row = |ui: &mut egui::Ui, label: &str, v: &mut f32,
                           lo: f32, hi: f32, dec: usize, tip: &str, ch: &mut bool| {
                ui.horizontal(|ui| {
                    ui.monospace(format!("{label:<6}"));
                    *ch |= ui.add(egui::Slider::new(v, lo..=hi).fixed_decimals(dec))
                        .on_hover_text(tip)
                        .changed();
                });
            };
            row(ui, "彩度", &mut a.saturation, 0.0, 2.0, 2,
                "色の濃さ。1.00 = バースト(40 IRE p-p)基準の信号どおり", &mut ch);
            row(ui, "明るさ", &mut a.brightness, -20.0, 20.0, 1,
                "黒レベルを上下する[IRE]。0 = 信号どおり", &mut ch);
            row(ui, "コントラスト", &mut a.contrast, 0.5, 1.5, 2,
                "1.00 = 信号どおり。色差にも掛かるので色が薄くならない", &mut ch);
            row(ui, "色相", &mut a.hue_deg, -30.0, 30.0, 0,
                "NTSCのtint。復調の位相基準をずらす[度]。0 = バーストどおり", &mut ch);
            row(ui, "ガンマ", &mut a.gamma, 0.6, 3.0, 2,
                "1.00 = 信号どおり。**これが正しい。**\n\
                 信号は既にガンマ符号化済みで、それはブラウン管のガンマを見越した\n\
                 ものなので、受け側で補正するものではない。sRGB液晶も同じ復号を\n\
                 するので、そのまま出せばブラウン管と同じ絵になる。\n\
                 \n\
                 上げると中間調が持ち上がる(端点は動かない)。明るい部屋で暗部が\n\
                 潰れて見えるときの対処。1.2〜1.6 あたりが実用的。\n\
                 2.20 は同じ画面のYouTube版と分位9点が平均誤差1.8コードで一致する値\n\
                 (あちらは符号化済みの値を二重符号化した絵。正しいわけではない)",
                &mut ch);
            if ch {
                self.adjust = a;
                *self.shared.adjust.lock().unwrap() = Some(a);
                self.mark_settings_dirty();
            }
        }
        // 復調前の生Yをそのまま見る。**artefactの出所を分ける道具。**
        //
        // 「線が二重に見える」「縞が出る」が復調由来なのか、それより前(信号そのもの、
        // 送出側のフィルタ、アナログ経路)なのかは、絵を見比べれば一発で分かる。
        // 生では副搬送波が8サンプル周期の細かい市松模様として見えるのが正常。
        // 設定には保存しない(次に開いたとき灰色で驚くので)。
        {
            let mut v = self.raw_view;
            if ui.checkbox(&mut v, "復調しない(生のYを見る)")
                .on_hover_text(
                    "NTSC復調を止めて、緑ch(CVBSそのもの)をグレースケールで出す。\n\
                     色は出ないが、二重像や縞が復調より前にあるかを目で分けられる。\n\
                     細かい市松模様は副搬送波(8サンプル周期)で、これは正常")
                .changed()
            {
                self.raw_view = v;
                *self.shared.raw_view.lock().unwrap() = Some(v);
            }
        }
        // 片方のフィールドだけ見る。**織り込みの影響を外すための道具。**
        // 縦線が二重に見えるとき、2枚のフィールドがずれているのか、1枚の中で
        // 既に二重なのかを分けられる。選んだ側を隣のスロットへ複製して全高で出す
        // (黒で間引くとスキャンラインが乗って、かえって見分けにくい)。
        ui.horizontal(|ui| {
            ui.monospace("フィールド");
            let mut v = self.field_view;
            let mut ch = false;
            // ★片フィールド表示は**ラインダブラ**。横線が2行ぶんの太さになるのは
            //   仕様。これを知らないと「横線が二重になった」と読めてしまうので、
            //   ホバーで明示する(実機で実際に紛らわしかった)。
            let tip = "偶数/奇数は片方のフィールドだけを見る切り分け用。\n\
                       ★選んだ側の行を隣へ複製するので、**横線は2行ぶんの太さになる**\n\
                       (縦線の二重化を見るための道具。横方向の判断は「織り込み」で)";
            ch |= ui.selectable_value(&mut v, 0u8, "織り込み")
                .on_hover_text(tip).changed();
            ch |= ui.selectable_value(&mut v, 1u8, "偶数のみ")
                .on_hover_text(tip).changed();
            ch |= ui.selectable_value(&mut v, 2u8, "奇数のみ")
                .on_hover_text(tip).changed();
            if ch {
                self.field_view = v;
                *self.shared.field_view.lock().unwrap() = Some(v);
            }
        });
        // 選んだプロファイル(空なら全部試す)での答え
        let pick = if self.source_profile.is_empty() {
            profiles::best_over_all(fh_hz).map(|(p, c)| (p.label, c))
        } else {
            profiles::by_key(&self.source_profile)
                .and_then(|p| profiles::best(p, fh_hz).map(|c| (p.label, c)))
        };
        match pick {
            Some((label, c)) => {
                ui.horizontal(|ui| {
                    let over = if c.oversample > 1 {
                        format!(" ×{}", c.oversample)
                    } else {
                        String::new()
                    };
                    ui.monospace(format!(
                        "→ {} ({:.4}MHz{})",
                        c.pll_divide,
                        c.f_dot / 1e6,
                        over
                    ));
                    let same = c.pll_divide == self.tune_pll_divide;
                    // YC8 では規格値が正なので、プロファイルの推定値で上書きしない
                    let btn = ui.add_enabled(!same && !pll_locked,
                                             egui::Button::new("適用"));
                    if pll_locked {
                        btn.clone().on_hover_text(
                            "YC8(コンポジット/S端子)では pll_div は規格値なので\n\
                             プロファイルからは変更しません");
                    }
                    if btn
                        .on_hover_text(format!(
                            "{label}: fH {:.3}kHz と {} から htotal {}\n\
                             (整数からのずれ {:.3}カウント)",
                            fh_hz / 1000.0,
                            c.label,
                            c.htotal,
                            c.residual
                        ))
                        .clicked()
                    {
                        self.tune_pll_divide = c.pll_divide;
                        send.push((
                            protocol::CFG_KEY_PLL_DIVIDE,
                            self.tune_pll_divide as u32,
                        ));
                        self.mark_settings_dirty();
                    }
                    if same {
                        ui.monospace("一致");
                    }
                });
            }
            None if fh_hz > 0.0 => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 170, 60),
                    "このプロファイルでは説明できない fH です",
                );
            }
            None => {
                ui.monospace("MODE 未受信");
            }
        }
        // 実測した有効映像の大きさ。pll_div が妥当かの目安として出す。
        //
        // ここには「目標の有効幅から pll_div を比例計算する」UI(target w / ×8 /
        // → pll)があったが撤去した。比例計算は有効映像の実測が信用できないと
        // 上限まで走り、実機で pll_div が 2304 に達して絵が崩れ、DATACLKが
        // 100MHzを超えてボードごとハングした。いまは下の「自動調整」が絵の
        // スペクトルから倍率を割り出すので、初期値がどれだけ外れていても収束する。
        let st = self.shared.stats.lock().unwrap().clone();
        ui.monospace(format!(
            "active {}x{} at ({},{})",
            st.active_w, st.active_h, st.active_x, st.active_y
        ));
        // 1ラインがバッファに入り切っていない状態。過剰サンプルの明確な兆候で、
        // このとき外接矩形は有効映像の幅ではなくバッファ幅を表すので、そこから
        // 計算した値は全部おかしくなる(管面の「絵に合わせる」も小さく出る)。
        if st.span_w > 0 && st.span_w as u32 >= self.frame_size.0.max(1) {
            ui.colored_label(
                egui::Color32::from_rgb(220, 170, 60),
                "1ラインがバッファに入り切っていません(過剰サンプル)",
            );
        }
        // インターレースの設定は撤去した。すべて測定から決まる:
        //   TVPの検出ビット(38h bit5)      インターレースかどうか
        //   生VSYNCの半ライン位相          どちらのフィールドが下か
        //   TVP vtotal / 生ライン数の比    フィールド単位かフレーム単位か
        // 以前は il(方式) / f2_row(折り返し点) / swap(偶奇) / field_src(極性の
        // 取得元) / auto(内容から判定)を人手で設定していたが、どれも信号から
        // 測れる量だった。手動の上書きを残すと誤った値で壊せる経路になる。
        // 実機でX68000のモード0〜18すべてが無調整で表示されることを確認した。
        // TVPのアナログ映像帯域。折り返しの元になる高周波を削る。
        // 15(最小=約95MHz)で、エッジ後の残留エコーが消える(実測 +2.55→+0.01)。
        // 立ち上がりは変わらず最細部が6%落ちるだけなので既定を15にしている。
        // 1ラインまるごと送る。全黒行が続いた直後の数行で非黒範囲の判定が壊れ、
        // 内容があるのに count_px=0 で送られて行が欠ける不具合がある。範囲を
        // 使わなくなるので、これを入れると完全に消える。帯域は htotal 画素ぶんに
        // 増える(31kHz 1104px で約555Mbps。GbEには収まる)。
        if ui.checkbox(&mut self.tune_full_line, "全ライン送信(行欠けの回避)")
            .on_hover_text(
                "非黒範囲だけを送る最適化を切り、1ラインまるごと送ります。\n\
                 全黒行の直後で行が欠ける不具合を回避できます。\n\
                 帯域は増えます(31kHzで約555Mbps)。")
            .changed()
        {
            send.push((protocol::CFG_KEY_FULL_LINE,
                       if self.tune_full_line { 1 } else { 0 }));
            self.mark_settings_dirty();
        }
        // フレーム間引き。受信が追いつかない機械での保険。
        // 映像だけを間引き、音声は間引かないので、音の途切れが減る。
        ui.horizontal(|ui| {
            ui.monospace("間引き");
            let mut n = self.tune_frame_skip as i32;
            if ui.add(egui::DragValue::new(&mut n).range(0..=7))
                .on_hover_text("映像を何フレームに1回にするか。0=毎フレーム、\n\
                                1=2フレームに1回(帯域とfpsが半分)。\n\
                                音声は間引かないので、映像を減らすと\n\
                                音の途切れも減ります")
                .changed()
            {
                self.tune_frame_skip = n as u8;
                send.push((protocol::CFG_KEY_FRAME_SKIP, n as u32));
            }
            ui.monospace(if self.tune_frame_skip == 0 {
                "毎フレーム".to_string()
            } else {
                format!("1/{} ({}fps相当)", self.tune_frame_skip + 1,
                        55 / (self.tune_frame_skip as u32 + 1))
            });
        });
        ui.horizontal(|ui| {
            ui.monospace("帯域制限");
            let mut bw = self.tune_video_bw as i32;
            if ui.add(egui::DragValue::new(&mut bw).range(0..=15)).changed() {
                self.tune_video_bw = bw as u8;
                send.push((protocol::CFG_KEY_VIDEO_BW, bw as u32));
            }
            ui.monospace(if self.tune_video_bw == 0 { "最大" } else if self.tune_video_bw == 15 { "最小≈95MHz" } else { "" });
        });
        // サンプリング位相。1ドット周期の1/32刻みで、ADCがドットのどこを掴むか。
        // 実測で鮮鋭度が位相だけで2.5倍変わった(最良22 / 既定16は-12% / 最悪4は-60%)。
        // 最適値はモードごとに違うので、モード別設定として保存している。
        ui.horizontal(|ui| {
            ui.monospace("位相");
            let mut ph = self.tune_phase as i32;
            if ui.add(egui::DragValue::new(&mut ph).range(0..=31)).changed() {
                self.tune_phase = ph as u8;
                send.push((protocol::CFG_KEY_PHASE, ph as u32));
            }
            if ui.button("-").clicked() {
                self.tune_phase = (self.tune_phase + 31) % 32;
                send.push((protocol::CFG_KEY_PHASE, self.tune_phase as u32));
            }
            if ui.button("+").clicked() {
                self.tune_phase = (self.tune_phase + 1) % 32;
                send.push((protocol::CFG_KEY_PHASE, self.tune_phase as u32));
            }
            ui.monospace(format!("{:.0}度", self.tune_phase as f32 * 360.0 / 32.0));
        });
        ui.horizontal(|ui| {
            if ui.button("読む").on_hover_text(
                "ボードの現在値を取り込んで表示を合わせる").clicked()
            {
                self.tune_synced = false;
                self.tune_get_at = None;
                // 未確認の送信が残っていると読み戻しがその分だけ塞がれるので消す
                self.tune_pending.clear();
                self.shared.config_state.lock().unwrap().clear();
            }
        });
        // pll_divide と位相の自動調整。
        //
        // 「→ pll」の比例計算は、有効映像の実測が信用できないと上限まで走る
        // (実機で pll_div が 2304 まで行って絵が崩れた)。こちらは絵の
        // スペクトルから倍率を割り出すので、いまの値がどれだけ外れていても
        // 1回で正しい範囲に入る。
        ui.horizontal(|ui| {
            if self.auto.is_some() {
                if ui.button("中止").clicked() {
                    self.auto = None;
                    self.auto_done = Some("中止しました".into());
                }
                if let Some(a) = self.auto.as_ref() {
                    ui.monospace(a.note.clone());
                }
            } else {
                if ui.button("自動調整")
                    .on_hover_text("pll_divide と位相を実測で決める(40秒ほど)。\n\
                                    文字など細かい模様が出ている画面で実行してください")
                    .clicked()
                {
                    self.auto_done = None;
                    self.auto_start();
                }
                if let Some(d) = self.auto_done.clone() {
                    ui.monospace(d);
                }
            }
        });
        if ui.button("send all").clicked() {
            send.push((protocol::CFG_KEY_VBP, self.tune_vbp as u32));
            send.push((protocol::CFG_KEY_HS_OFFSET, self.tune_hs_offset as u32));
            // YC8 では pll_divide は規格値。保存してある古い値で上書きしない
            if !pll_locked {
                send.push((protocol::CFG_KEY_PLL_DIVIDE, self.tune_pll_divide as u32));
            }
            send.push((protocol::CFG_KEY_PHASE, self.tune_phase as u32));
            send.push((protocol::CFG_KEY_FULL_LINE,
                       if self.tune_full_line { 1 } else { 0 }));
            send.push((protocol::CFG_KEY_FRAME_SKIP, self.tune_frame_skip as u32));
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

/// --- NIC受信バッファーの警告 ---
impl ViewerApp {
    /// 受信バッファーが小さすぎないかを知らせる。
    ///
    /// **二段構えにしてある。** 設定を読む方は「壊れる前に」警告できて直し方も
    /// 名指しできるが、`*ReceiveBuffers` を公開しないドライバでは空振りする。
    /// 実測のロスはどんなNICでも「いま実際に落ちている」ことを言えるが、
    /// 起きてからしか分からない。どちらかが引っかかれば取りこぼさない。
    fn netcheck_ui(&mut self, ui: &mut egui::Ui, s: &receiver::StatsSnapshot) {
        // ボードのアドレスが分かってから1回だけ調べる。経路から NIC を決めるので、
        // 相手のIPが要る(Wi-Fiと有線が両方生きている機械で誤判定しないため)
        if self.netcheck.is_none() {
            let addr = self
                .shared
                .boards
                .lock()
                .unwrap()
                .values()
                .next()
                .map(|b| b.addr.clone())
                // ボードが見つかる前でも判定できるように、SUBSCRIBE の宛先で
                // 代替する。**ボード発見を待つと、新規の機械でいちばん警告が
                // 要る場面(まだ何も映っていない状態)で黙ってしまう。**
                // ブロードキャスト宛でも経路は引けるので、送り先の NIC が分かる。
                // 取り違えても Unsupported / Unknown に落ちるだけで、嘘の警告には
                // ならない(実機のWi-Fiで確認済み)
                .or_else(|| {
                    let d = self.shared.sub_dest.lock().unwrap().clone();
                    (!d.is_empty()).then_some(d)
                });
            if let Some(addr) = addr {
                // ポート番号が付いていることがあるので落とす
                let ip = addr.split(':').next().unwrap_or(&addr).to_string();
                let b = netcheck::probe(&ip);
                // 起動ごとに1回だけ。パネルは Tab で隠せるうえ --fullscreen には
                // そもそも無いので、パネル内の表示だけでは「遊ぶときのモード」で
                // 気付けない
                self.netcheck_modal = b.should_warn() && !self.netcheck_muted;
                self.netcheck = Some(b);
            }
        }

        let warn = egui::Color32::from_rgb(220, 170, 60);
        let cfg = self.netcheck.clone().unwrap_or(netcheck::Buffers::Unknown);
        // 実測のロス率。少しだけ受けた段階では判断しない(起動直後の取りこぼしで
        // 毎回警告が出てしまう)
        let total = s.packets + s.lost_packets;
        let ratio = if total > 0 { s.lost_packets as f64 / total as f64 } else { 0.0 };
        let losing = total > 200_000 && ratio > 0.001;

        if !cfg.should_warn() && !losing {
            return;
        }
        if cfg.should_warn() {
            ui.colored_label(
                warn,
                format!(
                    "受信バッファーが {} です(推奨 {})",
                    cfg.value().unwrap_or(0),
                    netcheck::RECOMMENDED
                ),
            )
            .on_hover_text(
                "NICドライバの受信リングが小さいと、ソケットに届く前に\n                 パケットが捨てられます。映像に穴が開き音が途切れます",
            );
        } else {
            ui.colored_label(warn, format!("パケットを {:.2}% 落としています", ratio * 100.0))
                .on_hover_text(
                    "NICの受信バッファーが小さい可能性があります\n                     (このドライバは設定値を公開していないので確認できません)",
                );
        }
        // 直すには管理者権限が要るので、こちらでは適用せずコマンドを渡す
        let cmd = netcheck::fix_command(cfg.adapter());
        ui.horizontal(|ui| {
            if ui.small_button("コマンドをコピー").clicked() {
                ui.ctx().copy_text(cmd.clone());
            }
            ui.weak("管理者のPowerShellで実行");
        });
        ui.add(egui::Label::new(egui::RichText::new(&cmd).monospace().size(10.0)).wrap());
    }
}

/// 受信バッファーが小さいときに起動時へ1回だけ出すダイアログ。
///
/// **modal にしてよいのは設定チェックだけ。** 実際に設定を読んでいて、
/// `Unsupported` / `Unknown` では絶対に出さないので誤警告の経路が無い。しかも
/// 直せば二度と出ない一度きりの修正で、放置すると確実に映像に穴が開く。
/// ロス率ベースの警告は推定なうえセッション中に発火するので、割り込ませない
/// (パネル内の表示だけにしてある)。
///
/// 管理者権限が無くて直せない人に毎回出すのは敵対的なので、「今後表示しない」を
/// 用意して設定に保存する(パネル内の表示は消さない)。
impl ViewerApp {
    fn netcheck_modal(&mut self, ctx: &egui::Context) {
        if !self.netcheck_modal {
            return;
        }
        let cfg = self.netcheck.clone().unwrap_or(netcheck::Buffers::Unknown);
        let cmd = netcheck::fix_command(cfg.adapter());
        let mut close = false;
        let mut mute = false;
        egui::Modal::new(egui::Id::new("netcheck_modal")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("NIC の受信バッファーが小さすぎます");
            ui.add_space(6.0);
            ui.label(format!(
                "このまま使うとパケットを取りこぼし、映像に穴が開いて音が途切れます。\n\
                 アダプタ「{}」の受信バッファーが {} です(推奨 {})。",
                cfg.adapter().unwrap_or("(不明)"),
                cfg.value().unwrap_or(0),
                netcheck::RECOMMENDED,
            ));
            ui.add_space(6.0);
            ui.label("管理者の PowerShell で次を実行してください:");
            ui.add(
                egui::Label::new(egui::RichText::new(&cmd).monospace().size(11.0)).wrap(),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("コマンドをコピー").clicked() {
                    ui.ctx().copy_text(cmd.clone());
                }
                if ui.button("あとで").clicked() {
                    close = true;
                }
                if ui
                    .button("今後表示しない")
                    .on_hover_text("このダイアログだけ出さなくなります。\n                                    右パネルの Stats には表示され続けます")
                    .clicked()
                {
                    mute = true;
                    close = true;
                }
            });
        });
        if mute {
            self.netcheck_muted = true;
            self.mark_settings_dirty();
        }
        if close {
            self.netcheck_modal = false;
        }
    }
}

/// --- MimicX リモート入力 ---
impl ViewerApp {
    /// 物理キーを MimicX へ転送する。毎フレーム、UIを組む前に呼ぶ
    /// (`raw_input_hook`)。
    ///
    /// **キーの出どころは egui ではなく `keytap`(AppKit のイベント監視)。**
    /// egui が変換した `Key` には JIS の ¥ / _ / かな / 英数 が無く、テンキーも
    /// 最上段の数字に潰れる。転送中に egui へイベントを渡さないのも keytap 側で
    /// 捨てている(渡すと egui が Tab / 矢印 / Escape をフォーカス移動として
    /// 解釈し、パネルのウィジェットにフォーカスが移って転送が中断される)。
    ///
    /// 転送してよいのは「このウィンドウにフォーカスがあり、UIへ文字を打ち込んで
    /// いないとき」だけ。MimicX にフォーカスがあるときは MimicX 自身がキーを
    /// 受け取るので、そもそもここへイベントは来ない(macOS はフォーカスの無い
    /// アプリに配送しない)。だから二重入力にはならない。
    fn remote_step(&mut self, ctx: &egui::Context, raw: &egui::RawInput) {
        if !remote_input::AVAILABLE {
            return;
        }
        if !raw.focused {
            // 以後の解放イベントが届かないので押下状態を忘れる
            self.remote_toggle.reset();
        } else if !self.keytap.mods().is_toggle_combo() {
            // 修飾が揃っていない間は組み合わせのラッチを戻す。macOS は ⌘ を
            // 押しながらの keyUp を配送しないことがあり、これが無いと2回目が
            // 効かない。修飾は keytap 側(キーと同じ経路)を見る
            self.remote_toggle.rearm_chord();
        }
        // 転送のON/OFF(⌘+Shift+ESC / F12)を先に判定し、そのキーは転送しない。
        // **転送中でも必ず効く**(これが抜け道になる)
        let mut edge = false;
        let mut forward: Vec<(u32, bool)> = Vec::new();
        for ev in self.keytap.drain() {
            let Some(code) = remote_input::keycode_from_mac_vk(ev.vk) else {
                remote_input::log_unknown(ev.vk, ev.pressed);
                continue;
            };
            remote_input::log_key(code, Some(ev.vk), ev.pressed, ev.mods);
            let (toggle, eat) = self.remote_toggle.feed(code, ev.pressed, ev.mods);
            edge |= toggle;
            if eat {
                continue;
            }
            // ⌘ を押している間は Mac の操作(⌘Q で終了、など)。押下は実機へ
            // 送らない。X68000 のキーボードに ⌘ は無いので、⌘ 付きの打鍵が
            // 実機向けであることはない。
            //
            // **解放は ⌘ の有無に関わらず送る。** 押している途中で ⌘ を足すと、
            // 解放だけ落ちて実機にキーが押しっぱなしで残る。
            if ev.pressed && ev.mods.command {
                continue;
            }
            // ⌘ 自体も送らない(実機に無いキーで、MimicX も捨てる)
            if matches!(code, winit::keyboard::KeyCode::SuperLeft
                            | winit::keyboard::KeyCode::SuperRight)
            {
                continue;
            }
            // 表に無いキー(メディアキー等)は送らない。MimicX 側も黙って捨てる
            if let Some(usage) = remote_input::usage_from_keycode(code) {
                forward.push((usage, ev.pressed));
            }
        }
        if edge {
            self.remote.toggle();
            if self.remote.enabled() {
                // IME に食われるとキーイベントが届かない。egui/winit は
                // 「テキスト入力中のウィジェットがある」ときだけ IME を許可する
                // ので、フォーカスを外せば IME は無効になる
                ctx.memory_mut(|m| m.stop_text_input());
            }
        }

        // フォーカスを失った瞬間・UIが文字入力を始めた瞬間に全解放される
        // (update の中で送られる)。これを怠るとキーが押しっぱなしで実機に残る。
        // フォーカスは raw の値を見る(ctx 側は1フレーム前の状態)。
        //
        // 譲るのは「文字を打ち込んでいるウィジェットがあるとき」だけにする。
        // 「フォーカスがあるウィジェット」で判定すると、転送を入れるチェック
        // ボックス自身がフォーカスを持ったまま残って転送が始まらない。
        // pll_div などの数値欄へ打ち込む間だけ UI に渡し、外せば転送に戻る。
        //
        // 転送の可否を決めてから流す(順序が逆だと、ONにした最初のフレームの
        // 打鍵が落ちる)。
        self.remote.update(raw.focused && !ctx.text_edit_focused());
        for (usage, pressed) in forward {
            self.remote.key(usage, pressed);
        }
        // 次のイベントを egui へ渡すかどうかを keytap へ伝える
        self.keytap.set_capturing(self.remote.active());
    }

    /// 転送中であることを画面に出す。キーが実機へ流れている状態は見ただけでは
    /// 分からない(映像は転送していなくても同じ)ので、必ず示す。
    /// パネルを隠していても見えるように、最前面レイヤへ置く。
    ///
    /// これ自体がボタンで、押すと転送をやめる。**キーに頼らない抜け道**を必ず
    /// 用意するため(キーボードによっては打ちにくい・そのキーが無いことがあり、
    /// パネルを隠しているとキー以外の手段が無くなる)。
    fn remote_badge(&mut self, ctx: &egui::Context) {
        if !self.remote.active() {
            return;
        }
        let mut off = false;
        egui::Area::new(egui::Id::new("remote_input_badge"))
            .order(egui::Order::Foreground)
            .fixed_pos(ctx.content_rect().left_top() + egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                let label = egui::RichText::new(format!(
                    "⌨ MimicX 転送中 — {} かクリックで解除",
                    remote_input::TOGGLE_LABEL
                ))
                .size(12.0)
                .color(egui::Color32::WHITE);
                let btn = egui::Button::new(label)
                    .fill(egui::Color32::from_rgba_unmultiplied(170, 40, 40, 215))
                    .corner_radius(4.0);
                off = ui
                    .add(btn)
                    .on_hover_text("キー転送をやめて RetroCastX の操作に戻ります")
                    .clicked();
            });
        if off {
            self.remote.set_enabled(false);
        }
    }

    fn remote_ui(&mut self, ui: &mut egui::Ui) {
        if !remote_input::AVAILABLE {
            ui.weak("リモート入力は macOS 専用です");
            return;
        }
        let mut on = self.remote.enabled();
        if ui
            .checkbox(&mut on, format!("キーを MimicX へ転送 ({})",
                                      remote_input::TOGGLE_LABEL))
            .on_hover_text(format!(
                "このウィンドウで受けた物理キーを MimicX へ流し、\n\
                 実機のキーボード入力にします。\n\
                 転送中は Tab / B などもすべて実機へ行きます。\n\
                 {} か、画面左上に出るバッジのクリックで解除できます。\n\
                 ESC 単独は実機の ESC として送ります。\n\
                 MimicX を起動し、アダプタに接続して\n\
                 キーボード操作画面に入っている必要があります。",
                remote_input::TOGGLE_LABEL_FULL
            ))
            .changed()
        {
            self.remote.set_enabled(on);
            if on {
                ui.ctx().memory_mut(|m| m.stop_text_input());
            }
        }
        if !self.remote.enabled() {
            return;
        }
        if self.remote.connected() {
            ui.monospace(format!(
                "{}  押下 {}",
                if self.remote.active() { "転送中" } else { "待機(未フォーカス/入力中)" },
                self.remote.held()
            ));
        } else {
            ui.colored_label(egui::Color32::from_rgb(220, 170, 60), self.remote.status())
                .on_hover_text(format!(
                    "CoreMIDI の宛先 \"{}\" を探しています。\n\
                     MimicX が起動すれば自動で繋がります",
                    remote_input::PORT_NAME
                ));
        }
        // ゲームパッドは MimicX 側が背景でも直接受け取るので転送しない
        // (両方送ると二重入力になる)
        ui.weak("ゲームパッドは MimicX が直接受け取ります");
    }
}

impl eframe::App for ViewerApp {
    /// MimicX へのキー転送。キーそのものは keytap(AppKit のイベント監視)から
    /// 取るので、ここでは RawInput を読むだけ(フォーカスと修飾の状態)
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.remote_step(ctx, raw_input);
    }

    /// 終了時にも保存する(遅延書込の待ち時間中に閉じても取りこぼさない)
    fn on_exit(&mut self) {
        // 押しっぱなしのキーを実機に残さない。ここで送らないと操作不能になる
        self.remote.set_enabled(false);
        // 遅延を無視して即座に書く
        self.settings_dirty = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        self.flush_settings();
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t_ui = std::time::Instant::now();
        let ctx = root.ctx().clone();
        let t_tex = std::time::Instant::now();
        self.refresh_texture(&ctx);
        self.pace.note_upload(t_tex.elapsed());
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

        // Tab で操作パネルを出し入れする。隠すと戻すUIが無くなるので、
        // キーだけは常に効くようにしておく(テキスト入力中は除く)。
        // トグルでウィンドウサイズは変えない。切り替えた直後の last_avail /
        // last_tex は「切り替わる前のレイアウト」の値なので、それで合わせると
        // 毎回ずれ、出し入れを繰り返すとウィンドウが縮み続ける(実際そうなった)。
        // 大きさを合わせたいときは「画に合わせる」を押す。
        //
        // MimicX への転送中はここへ来ない。raw_input_hook が先にイベントを
        // 取り除いているので、Tab も B も X68000 のキーとして送られる
        // (⌘+Shift+ESC で転送を切れば元に戻る)。
        if root.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            self.show_panel = !self.show_panel;
            self.mark_settings_dirty();
        }
        // B で筐体の枠を出し入れする。プルダウンの選択は保つので、機種を選び直さずに
        // 「枠あり/なし」を見比べられる。
        if root.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::B)) {
            self.bezel_off = !self.bezel_off;
            self.mark_settings_dirty();
        }
        if self.show_panel {
        egui::Panel::right(egui::Id::new("info")).show(root, |ui| {
            // 項目が増えて下が操作できなくなるので縦スクロールにする。
            // auto_shrink=false でパネル幅いっぱいを使い、内容が短いときも
            // 幅が縮まないようにする。
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                ui.heading("RetroCastX");
                if let Some(err) = &self.rx_error {
                    ui.colored_label(egui::Color32::RED, err);
                }
                // 指定値ではなく「実際に送っている宛先」を出す。ユニキャスト指定で
                // 応答が無いとブロードキャストへ落ちるので、ここが変わることがある。
                {
                    let dest = self.shared.sub_dest.lock().unwrap().clone();
                    if dest.is_empty() {
                        ui.label("subscribe: off (listen only)");
                    } else if dest == "255.255.255.255" {
                        ui.label("SUBSCRIBE → ブロードキャスト")
                            .on_hover_text(
                                "宛先を指定せずに探しています。ボードは SUBSCRIBE の\n\
                                 送信元へ映像を返すので、サブネットが違っても\n\
                                 同じL2セグメントにいれば届きます。\n\
                                 ブロードキャストになるのはこの2秒ごとの要求だけで、\n\
                                 映像はユニキャストです");
                    } else {
                        ui.label(format!("SUBSCRIBE → {dest}"));
                    }
                }
                ui.separator();

                ui.strong("Mode");
                // 周波数インジケータ(実機モニタのLEDに相当)。同期している帯域が点灯する
                self.band_leds(ui);
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
                    if !self.pace.cpu_summary.is_empty() {
                        ui.monospace(&self.pace.cpu_summary);
                    }
                }
                let s = self.shared.stats.lock().unwrap().clone();
                ui.monospace(format!("{:.1} fps  {:.1} Mbps", s.fps, s.mbps));
                ui.monospace(format!("frames {}", s.frames));
                // lost はOSのUDPバッファ溢れ、qdrop は組立が追いつかず受信スレッドの
                // キューが満杯で捨てた分。分けて出さないとどちらが詰まっているのか
                // 判断できない
                ui.monospace(format!("pkts {}  lost {}", s.packets, s.lost_packets));
                self.netcheck_ui(ui, &s);
                if s.queue_drops > 0 {
                    ui.monospace(format!("queue drops {}", s.queue_drops))
                        .on_hover_text("組立が追いつかず、受信スレッドのキューが\n\
                                        満杯で捨てた数。ここが増えるなら組立側が、\n\
                                        lost だけ増えるなら受信側が詰まっています");
                }
                ui.monospace(format!("orphan lines {}", s.orphan_lines));
                // 太らせても埋まらなかった行数。0でないと前フレームの残りが減衰して
                // 薄い影として見える。プログレッシブでは0になるべき
                // インタレースかどうかは mflags では決まらないので測定で判定している。
                // ここが「プログレッシブ扱い」のまま実際はインタレースだと、
                // 太らせが別フィールドの行を複製して縞が交互にちらつく
                if s.interlace_measured {
                    // **インタレースでは半分が未充填なのが正常**(残りは前フィールドの
                    // 内容 = 織り込み)。ここで警告色にすると毎回不具合に見える
                    ui.colored_label(
                        egui::Color32::from_rgb(120, 200, 120),
                        format!("インタレース(測定): 1フィールドずつ / 太らせ停止・減衰なし\n\
                                 前フィールド保持 {}行(半分が正常)", s.unfilled_rows),
                    );
                } else if s.unfilled_rows > 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 170, 60),
                        format!("未充填の行 {}", s.unfilled_rows),
                    );
                }
                // NTSC復調(YC8のときだけ)。位相差が180°から離れたらコムが
                // 効いていない=色が出ない。コム間隔は測って決めているので、
                // 織り込み設定が変わっても追従する
                if s.ntsc_comb_step > 0 {
                    if s.publish_wait_max_ms > 1.0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 170, 60),
                            format!("フレーム差し替えのUI待ち 合計{:.0}ms 最大{:.0}ms",
                                    s.publish_wait_ms, s.publish_wait_max_ms));
                    }
                    ui.colored_label(
                        egui::Color32::from_rgb(120, 200, 120),
                        if s.ntsc_svideo {
                            format!("NTSC復調 {}行 コム間隔{} 位相差{:.0}°\n\
                                     S端子(赤chのバースト検出) コム未使用",
                                    s.ntsc_locked, s.ntsc_comb_step, s.ntsc_phase_deg)
                        } else {
                            format!("NTSC復調 {}行 コム間隔{} 位相差{:.0}°\n\
                                     3次元コム {}行 動き {:.1}% 位相ズレ{:.1}°",
                                    s.ntsc_locked, s.ntsc_comb_step, s.ntsc_phase_deg,
                                    s.ntsc_lines_3d, 100.0 * s.ntsc_motion_frac,
                                    s.ntsc_phase_drift_deg)
                        },
                    );
                } else if matches!(self.shared.mode.lock().unwrap().as_ref(),
                                   Some(m) if m.pixfmt == protocol::PIXFMT_YC8) {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 170, 60),
                        "NTSC復調 未ロック(Yのグレースケール表示)",
                    );
                }
                // 暗部のフレーム間差分=点状ノイズの量。配線やパスコンの効果を数値で見る
                ui.monospace(format!(
                    "noise {:.1}%  lvl {:.1}",
                    s.noise_flicker, s.noise_level
                ));
                ui.separator();

                ui.strong("Audio");
                self.audio_ui(ui);
                ui.separator();

                ui.strong("Remote input");
                self.remote_ui(ui);
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

                // 管面(ブラウン管の物理的な表示領域)。
                //
                // 管面(ブラウン管の物理的な表示領域)。
                //
                // 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど
                // 掃引する」ように周波数ごとに速度が切り替わる。だから管面の横幅は
                // 1/fH そのもので、掃引時間は設定項目ではない。縦も 1VSYNC周期が
                // 管面の高さになる。
                //
                // ここで決めるのは「1周期のうち実際に管面へ出る割合」と「位置」だけ。
                // 1.0 なら周期全体が見えるので何も切れない。実際のCRTは帰線の間ビームが
                // 戻っているので 0.85〜0.95 あたりが現実に近い。
                //
                // 掃引時間を絶対時間で持つ方式も試したが実機と違った。横のサイズは
                // 1/fH に比例するのに、X68000はどのモードも fV が53〜61Hzで1フレームの
                // 時間がほぼ一定なので縦は変わらない。実測で24kHzが横51%・縦95%になり
                // 縦長に潰れた。
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.tube_time_based, "時間ベース")
                        .on_hover_text("1HSYNC周期を管面の横幅、1VSYNC周期を高さとする\n\
                                        (3モードディスプレイの偏向と同じ)。\n\
                                        切ると旧方式(割合を直接指定)になる")
                        .changed()
                    {
                        self.mark_settings_dirty();
                    }
                    if let Some(m) = self.shared.mode.lock().unwrap().as_ref() {
                        let fh_khz = m.hfreq_mhz_x1000 as f32 / 1_000_000.0;
                        if fh_khz > 1.0 {
                            ui.weak(format!("1ライン {:.1}µs = 管面の横幅", 1000.0 / fh_khz));
                        }
                    }
                });
                if self.tube_time_based {
                    // 実CRTのH幅/H位置/V幅/V位置つまみに相当する。スライダーにして
                    // 「回して合わせる」操作にしている。
                    //
                    // 絵の内容から自動で中央へ寄せる方法は採らない。幾何が内容に依存して
                    // しまい、「幾何は同期信号だけで決まる」という原則が崩れる。実CRTの
                    // H位置もモードごとに一度合わせる固定値であって、絵に追従はしない。
                    // 帯域ごとに保存されるので一度合わせれば以後は再現される。
                    let mut ch = false;
                    ui.horizontal(|ui| {
                        ui.monospace("H幅");
                        ch |= ui.add(egui::Slider::new(&mut self.mon[0], 0.2..=2.0)
                                     .fixed_decimals(3).show_value(true))
                            .on_hover_text("1HSYNC周期のうち管面に出る割合。\n\
                                            1.0で帰線期間まで全部見える")
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.monospace("H位置");
                        ch |= ui.add(egui::Slider::new(&mut self.mon[1], -0.5..=0.5)
                                     .fixed_decimals(3).show_value(true))
                            .on_hover_text("右へ動かすと絵が右へ動く。\n\
                                            信号は左右非対称(バックポーチが長い)なので、\n\
                                            0だと絵が右に寄る(負の値で左へ戻す)")
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.monospace("V幅");
                        ch |= ui.add(egui::Slider::new(&mut self.mon[2], 0.2..=2.0)
                                     .fixed_decimals(3).show_value(true))
                            .on_hover_text("1VSYNC周期のうち管面に出る割合")
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.monospace("V位置");
                        ch |= ui.add(egui::Slider::new(&mut self.mon[3], -0.5..=0.5)
                                     .fixed_decimals(3).show_value(true))
                            .on_hover_text("右へ動かすと絵が下へ動く")
                            .changed();
                    });
                    if ch {
                        self.mark_settings_dirty();
                    }
                    // 有効映像が管面のどれだけを占めるか。モードごとに違って当然で、
                    // 15kHzはラインの0.842を占めるので大きく、31kHzは0.696なので小さい
                    let (aw, ah) = {
                        let st = self.shared.stats.lock().unwrap();
                        (st.span_w as f32, st.span_h as f32)
                    };
                    if let Some(m) = self.shared.mode.lock().unwrap().as_ref() {
                        if m.htotal > 0 && m.vtotal > 0 && aw > 0.0 {
                            let fw = aw / m.htotal as f32 / self.mon[0].max(0.01) * 100.0;
                            // 縦はスロット目盛り。1ラインが何スロットを占めるかは
                            // 織り込みの有無で変わる(mflags bit0)
                            let k = if m.mflags & 0x0001 != 0 { 1.0 } else { 2.0 };
                            let vt = m.vtotal as f32 * k;
                            let fh = ah / vt / self.mon[2].max(0.01) * 100.0;
                            ui.weak(format!("有効映像は管面の {fw:.0}% x {fh:.0}%"));
                        }
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.monospace("H位置");
                        let mut c = (self.window[0] + self.window[1]) * 0.5;
                        let mut w = self.window[1] - self.window[0];
                        let a = ui.add(egui::DragValue::new(&mut c).speed(0.002).range(0.0..=1.0)
                                       .fixed_decimals(3));
                        ui.monospace("H幅");
                        let b = ui.add(egui::DragValue::new(&mut w).speed(0.002).range(0.05..=1.0)
                                       .fixed_decimals(3));
                        if a.changed() || b.changed() {
                            self.window[0] = (c - w * 0.5).clamp(-0.5, 1.5);
                            self.window[1] = self.window[0] + w;
                            self.mark_settings_dirty();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.monospace("V位置");
                        let mut c = (self.window[2] + self.window[3]) * 0.5;
                        let mut h = self.window[3] - self.window[2];
                        let a = ui.add(egui::DragValue::new(&mut c).speed(0.002).range(0.0..=1.0)
                                       .fixed_decimals(3));
                        ui.monospace("V幅");
                        let b = ui.add(egui::DragValue::new(&mut h).speed(0.002).range(0.05..=1.0)
                                       .fixed_decimals(3));
                        if a.changed() || b.changed() {
                            self.window[2] = (c - h * 0.5).clamp(-0.5, 1.5);
                            self.window[3] = self.window[2] + h;
                            self.mark_settings_dirty();
                        }
                    });
                }
                ui.horizontal(|ui| {
                    // 縦画面のゲーム(ドラゴンスピリット等)向け
                    ui.monospace("回転");
                    let mut rot = self.rotate;
                    egui::ComboBox::from_id_salt("rotate")
                        .width(72.0)
                        .selected_text(["0°", "90°", "180°", "270°"][rot as usize])
                        .show_ui(ui, |ui| {
                            for (i, t) in ["0°", "90°", "180°", "270°"].iter().enumerate() {
                                ui.selectable_value(&mut rot, i as u32, *t);
                            }
                        });
                    if rot != self.rotate {
                        self.rotate = rot;
                        self.mark_settings_dirty();
                        self.want_fit = true;
                    }
                });
                ui.horizontal(|ui| {
                    // 管面(表示領域)の縦横比。実際のCRTと同じで、ドット数ではなく
                    // 管面の形が表示を決める。4:3 にすれば 512x256 でも 768x512 でも
                    // 同じ形に映る。
                    ui.monospace("管面");
                    let cur = self.tube_aspect;
                    let mut sel = cur;
                    let name = |a: f32| -> &'static str {
                        if a <= 0.0 { "そのまま" }
                        else if (a - 4.0 / 3.0).abs() < 0.01 { "4:3" }
                        else if (a - 1.0).abs() < 0.01 { "1:1" }
                        else if (a - 16.0 / 9.0).abs() < 0.01 { "16:9" }
                        else { "任意" }
                    };
                    egui::ComboBox::from_id_salt("tube")
                        .width(96.0)
                        .selected_text(name(cur))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, 0.0, "そのまま");
                            ui.selectable_value(&mut sel, 4.0 / 3.0, "4:3");
                            ui.selectable_value(&mut sel, 1.0, "1:1");
                            ui.selectable_value(&mut sel, 16.0 / 9.0, "16:9");
                        });
                    if sel != cur {
                        self.tube_aspect = sel;
                        self.mark_settings_dirty();
                        self.want_fit = true;
                    }
                });
                ui.horizontal(|ui| {
                    // モニタの枠。実在モニタのイラストの開口部に管面をはめる。
                    // 実機のCRTと同じで「管面の形が表示を決める」ので、管面モデルの
                    // 映像はそのまま開口部に入る。
                    ui.monospace("枠");
                    let cur = self.bezel.clone();
                    let mut sel = cur.clone();
                    let name = |k: &str| -> String {
                        if k.is_empty() { "なし".into() }
                        else { bezel::by_key(k).map_or_else(|| k.to_string(),
                                                           |b| b.label.to_string()) }
                    };
                    egui::ComboBox::from_id_salt("bezel")
                        .width(150.0)
                        .selected_text(name(&cur))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, String::new(), "なし");
                            for b in bezel::BEZELS {
                                ui.selectable_value(&mut sel, b.key.to_string(), b.label);
                            }
                        });
                    if sel != cur {
                        self.bezel = sel;
                        self.bezel_tex = None;   // 幅が同じでも作り直す
                        self.mark_settings_dirty();
                        self.want_fit = true;
                    }
                    // 選択は保ったまま出し入れする。機種を選び直さずに見比べられる
                    let mut on = !self.bezel_off;
                    if ui.checkbox(&mut on, "表示")
                        .on_hover_text("筐体の枠を出し入れします (B キー)")
                        .changed()
                    {
                        self.bezel_off = !on;
                        self.mark_settings_dirty();
                    }
                });
                ui.horizontal(|ui| {
                    // 拡大時の補間。sharp-bilinear は非整数倍でもドット幅が不揃いに
                    // ならず、かつ全体がぼやけない。高解像度モニタ向けの既定。
                    ui.monospace("補間");
                    let mut f = self.filter;
                    egui::ComboBox::from_id_salt("filter")
                        .width(120.0)
                        .selected_text(["ニアレスト", "バイリニア", "sharp-bilinear"][f as usize])
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut f, 0, "ニアレスト");
                            ui.selectable_value(&mut f, 1, "バイリニア");
                            ui.selectable_value(&mut f, 2, "sharp-bilinear");
                        });
                    if f != self.filter {
                        self.filter = f;
                        self.mark_settings_dirty();
                    }
                });
                ui.horizontal(|ui| {
                    // インターレースの残光。
                    //
                    // インターレースでは毎フレーム半分の行しか来ないのが正常なので、
                    // パケットロス用の減衰をそのまま掛けると全行が 100%⇄80% を
                    // フィールドレートで往復して面全体がちらつく。既定は 1.00 =
                    // 減衰なし(前フィールドの行をそのまま残す = 素直な weave)。
                    // 下げるとCRTの残光に近い見え方になる。好みで選べるようにしてある。
                    ui.monospace("残光");
                    let mut d = self.interlace_decay;
                    if ui.add(egui::Slider::new(&mut d, 0.5..=1.0).fixed_decimals(2))
                        .on_hover_text("インターレース時に、前のフィールドの行を\n\
                                        次のフィールドでどれだけ残すか。\n\
                                        1.00 = そのまま残す(チラつき無し)\n\
                                        下げるとCRTの残光に近づくがチラつく")
                        .changed()
                    {
                        self.interlace_decay = d;
                        *self.shared.interlace_decay.lock().unwrap() = Some(d);
                        self.mark_settings_dirty();
                    }
                });
                // 縦倍率(と 4:3 / 1.0 / integer scaling)は撤去した。表示の形は
                // 管面が決めるので、ドット数に対する倍率という概念が要らない。
                // 512x256 も 768x512 も、管面を 4:3 にすれば同じ形で映る。
                if ui.button("画に合わせる").clicked() {
                    self.want_fit = true;
                }
                ui.separator();
                if ui.button("パネルを隠す (Tab)")
                    .on_hover_text("操作パネルを隠して映像だけにします。\n\
                                    Tab キーで戻せます")
                    .clicked()
                {
                    self.show_panel = false;
                    self.mark_settings_dirty();
                }
                });
        });
        }   // if self.show_panel

        // 管面の外側は黒。実CRTと同じで、掃引が届かない場所には何も出ない。
        // 以前は「capture border(赤い枠)」と「outside(外側の色)」で画枠の位置を
        // 見えるようにしていたが、幾何がGPU側へ移ってどちらもキャプチャ範囲では
        // なく管面の外周を指すようになったので撤去した。管面の位置は H位置/H幅
        // のスライダーで直接合わせる。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(root, |ui| {
                // 映像が無くても枠は描く。実機のCRTは映像が無くても筐体があるので、
                // 待機中に枠だけ消えるのは不自然。開口部は黒にして待機表示を中に出す。
                let has_video = self.frame_size.0 > 0;
                if !has_video && self.active_bezel().is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.weak("waiting for stream...");
                    });
                    return;
                }
                let tex_size = egui::vec2(self.frame_size.0 as f32, self.frame_size.1 as f32);
                let avail = ui.available_size();
                // 有効映像(切り出し)を「管面」いっぱいに引き伸ばして表示する。
                // 実際のCRTと同じ考え方で、ドット数ではなく管面の形が表示を決める。
                let (cw, ch) = if self.crop[2] > 0 && self.crop[3] > 0 {
                    (self.crop[2] as f32, self.crop[3] as f32)
                } else {
                    (tex_size.x, tex_size.y)
                };
                let bez = self.active_bezel();
                // 枠ありなら管面の形は開口部が決める。枠は実在モニタの寸法なので、
                // tube_aspect(4:3など)ではなく開口部の比に従うのが筋。
                let aspect = match bez {
                    Some(b) => b.screen_aspect(),
                    None if self.tube_aspect > 0.0 => self.tube_aspect,
                    None => cw / ch.max(1.0),
                };
                let disp = if self.rotate % 2 == 1 {
                    egui::vec2(1.0, aspect)
                } else {
                    egui::vec2(aspect, 1.0)
                };
                self.last_avail = avail;
                if !self.did_autofit {
                    self.did_autofit = true;
                }
                match bez {
                    // --- 枠なし: 管面を領域いっぱいに ---
                    None => {
                        let fit = (avail.x / disp.x).min(avail.y / disp.y);
                        let size = disp * fit;
                        self.last_tex = size;
                        ui.centered_and_justified(|ui| {
                            let (rect, _r) =
                                ui.allocate_exact_size(size, egui::Sense::hover());
                            self.paint_tube(ui, rect);
                        });
                    }
                    // --- 枠あり: 枠全体を領域に収め、その中の開口部に管面を置く ---
                    Some(b) => {
                        let outer = egui::vec2(b.outer_aspect(), 1.0);
                        let fit = (avail.x / outer.x).min(avail.y / outer.y);
                        let bsize = outer * fit;
                        self.last_tex = bsize;
                        // 枠のテクスチャを必要な幅で用意する。幅が変わったら作り直す
                        // (拡大されたSVGがぼけないように)。ラスタライズは重いので
                        // 幅は64px刻みに丸めて、リサイズ中に作り直し続けないようにする。
                        let ppp = ui.ctx().pixels_per_point();
                        let want_w = (((bsize.x * ppp) as u32).max(64) + 63) / 64 * 64;
                        let stale = self
                            .bezel_tex
                            .as_ref()
                            .map_or(true, |(_, w)| *w != want_w);
                        if stale {
                            if let Some((rgba, w, h)) = b.rasterize(want_w) {
                                let img = egui::ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize], &rgba);
                                let tex = ui.ctx().load_texture(
                                    "bezel", img, egui::TextureOptions::LINEAR);
                                self.bezel_tex = Some((tex, want_w));
                            }
                        }
                        let (rect, _r) = ui.allocate_exact_size(avail, egui::Sense::hover());
                        // 枠を領域の中央に置く
                        let brect = egui::Rect::from_center_size(rect.center(), bsize);
                        // 開口部を枠の座標から実座標へ写す
                        let sx = bsize.x / b.view.0;
                        let sy = bsize.y / b.view.1;
                        let srect = egui::Rect::from_min_size(
                            brect.min + egui::vec2(b.screen[0] * sx, b.screen[1] * sy),
                            egui::vec2(b.screen[2] * sx, b.screen[3] * sy),
                        );
                        // 管面 → 枠。枠は開口部が抜けているので後から重ねる
                        if has_video {
                            self.paint_tube(ui, srect);
                        } else {
                            ui.painter().rect_filled(srect, 0.0, egui::Color32::BLACK);
                        }
                        if let Some((tex, _)) = self.bezel_tex.as_ref() {
                            ui.painter().image(
                                tex.id(), brect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0),
                                                         egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                        if !has_video {
                            ui.painter().text(
                                srect.center(), egui::Align2::CENTER_CENTER,
                                "waiting for stream...",
                                egui::FontId::proportional(14.0),
                                egui::Color32::from_gray(90));
                        }
                    }
                }
            });

        // ウィンドウを画にぴったり合わせる。中央パネルを描いた後に行う
        // (今フレームの描画領域が分かってから計算するため)。
        // 画の外側の余白 = ウィンドウ内寸 - 中央の描画領域。右パネルとフレームの
        // 分がここに入るので、内寸をこれだけ変えれば画がちょうど等倍で収まる。
        // フルスクリーン中に InnerSize を送ると、要求が部分的にしか効かずレイアウトが
        // 中途半端な大きさで残る(実機で画が画面中央ではなく左に寄った)。
        // フルスクリーンでは「画面いっぱいに収める」のが既に正しい状態なので何もしない。
        let is_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if self.want_fit && is_fullscreen {
            self.want_fit = false;
        }
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

        self.remote_badge(&ctx);
        self.netcheck_modal(&ctx);

        // ストリーム停止中でもUI(統計・発見リスト)を更新し続ける
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        self.pace.note_ui(t_ui.elapsed());
    }
}

impl Drop for ViewerApp {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}
