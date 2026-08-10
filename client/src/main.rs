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
mod render;
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
    let mut rotate: u32 = u32::MAX;   // 未指定なら設定ファイルの値を使う
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
            // 画面回転(縦画面のゲーム用)。0/90/180/270 度、時計回り
            "--rotate" => {
                let v: u32 = args.next().expect("--rotate needs 0|90|180|270").parse().unwrap();
                rotate = match v {
                    0 => 0, 90 => 1, 180 => 2, 270 => 3,
                    _ => panic!("--rotate は 0/90/180/270 のいずれか"),
                };
            }
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
        fullscreen::run(port, subscribe_to, target_mac, decay, audio, p); // 戻らない
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
    /// TVPのアナログ映像帯域。0=最大 / 15=最小(約95MHz)
    tune_video_bw: u8,
    tune_phase: u8,
    /// 送信したがまだボードの応答で確認できていない値(key → (値, 送信時刻))。
    /// 応答が返るまでは自分の値を優先し、押した瞬間に元へ戻って見えるのを防ぐ。
    tune_pending: std::collections::HashMap<u16, (u32, std::time::Instant)>,
    /// 次に現在値を問い合わせる時刻。全キーが揃うまで繰り返す
    tune_get_at: Option<std::time::Instant>,
    /// ボードの現在値を取り込み済みか。起動時に1回だけ合わせる
    tune_synced: bool,
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
    /// 受信中フレームの寸法(GPUテクスチャは callback_resources 側が持つ)
    frame_size: (u32, u32),
    render_state: Option<eframe::egui_wgpu::RenderState>,
    seen_gen: u64,
    integer_scale: bool,
    /// 表示時の縦倍率(ドットが正方形でないモードの縦つぶれを直す)
    vscale: f32,
    /// 画面回転 0/1/2/3 = 時計回りに 0/90/180/270 度
    rotate: u32,
    /// 表示する切り出し範囲[画素]。w か h が 0 なら全体を表示
    crop: [u32; 4],
    /// 管面の縦横比(幅/高さ)。0 なら有効映像の比のまま
    tube_aspect: f32,
    /// 補間 0=ニアレスト 1=バイリニア 2=sharp-bilinear
    filter: u32,
    /// 管面が映す時間窓 [h0,h1,v0,v1]。CRTのH位置/H幅・V位置/V幅に相当
    window: [f32; 4],
    tube_time_based: bool,
    /// いまの帯域のモニタプロファイル [H幅, H位置, V幅, V位置]。
    /// 1周期のうち管面に出る割合と、周期の中心からのずれ
    mon: [f32; 4],
    /// 帯域ごとのモニタプロファイル(CZ-612Dのような3モードディスプレイに相当)
    mon_bands: std::collections::BTreeMap<u32, [f32; 4]>,
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
        // 通常モードもフルスクリーンと同じシェーダで描く。見え方が食い違わないように。
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let blit = render::EguiBlit::new(&rs.device, rs.target_format);
            rs.renderer.write().callback_resources.insert(blit);
        }
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
            tune_video_bw: cfg.tune_video_bw,
            tune_phase: cfg.tune_phase,
            tune_pending: Default::default(),
            tune_get_at: None,
            tune_synced: false,
            weave_auto: None,
            window_size: egui::vec2(cfg.window_w, cfg.window_h),
            last_avail: egui::Vec2::ZERO,
            last_tex: egui::Vec2::ZERO,
            did_autofit: false,
            want_fit: false,
            frame_size: (0, 0),
            render_state: cc.wgpu_render_state.clone(),
            seen_gen: 0,
            integer_scale: cfg.integer_scale,
            vscale: cfg.vscale,
            rotate: cfg.rotate,
            crop: [cfg.crop_x, cfg.crop_y, cfg.crop_w, cfg.crop_h],
            tube_aspect: cfg.tube_aspect,
            filter: cfg.filter,
            window: cfg.window,
            tube_time_based: cfg.tube_time_based,
            mon: cfg.mon,
            mon_bands: cfg.mon_bands.clone(),
            band_khz: 0,
            auto: None,
            auto_done: None,
            modes: cfg.modes.clone(),
            mode_key: String::new(),
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
        self.frame_size = (frame.width as u32, frame.height as u32);
        if let Some(rs) = self.render_state.as_ref() {
            let mut w = rs.renderer.write();
            if let Some(b) = w.callback_resources.get_mut::<render::EguiBlit>() {
                b.upload(&rs.device, &rs.queue, &frame.rgba,
                         frame.width as u32, frame.height as u32);
            }
        }
        let _ = ctx;
    }
}

impl ViewerApp {
    /// 現在のUI状態を設定として書き出す。スライダー操作中に毎フレーム書くのを
    /// 避けるため、変更を記録して少し待ってから1回だけ保存する。
    fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(std::time::Instant::now());
    }

    fn flush_settings(&mut self) {
        if self.band_khz > 0 {
            let k = self.band_khz;
            self.mon_bands.insert(k, self.mon);
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
            integer_scale: self.integer_scale,
            vscale: self.vscale,
            rotate: self.rotate,
            tube_aspect: self.tube_aspect,
            filter: self.filter,
            window: self.window,
            tube_time_based: self.tube_time_based,
            mon: self.mon,
            mon_bands: self.mon_bands.clone(),
            modes: self.modes.clone(),
            crop_x: self.crop[0],
            crop_y: self.crop[1],
            crop_w: self.crop[2],
            crop_h: self.crop[3],
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
            tune_video_bw: self.tune_video_bw,
            tune_phase: self.tune_phase,
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

    /// いまの表示パラメータ。幾何は MODE(htotal/vtotal)と画枠設定
    /// (hs_offset/vbp)から決まる。映像の内容には依存しない。
    fn render_params(&self) -> render::Params {
        let m = self.shared.mode.lock().unwrap().clone();
        // 実測した有効映像の外接矩形。MODEの hactive は送出フレームの幅(常に1024)で
        // 有効映像の幅ではないため、管面の中心と縦の連動にはこの実測値を使う
        // 1ラインが何スロットを占めるか(MODEの mflags bit0 = インターレース)
        let slot_k: u32 = match m.as_ref() {
            Some(m) if m.mflags & 0x0001 != 0 => 1,
            _ => 2,
        };
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
            vtotal: m.as_ref().map_or(0, |m| m.vtotal as u32 * slot_k),
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
    const TUNE_KEYS: [u16; 9] = [
        protocol::CFG_KEY_VBP,
        protocol::CFG_KEY_HS_OFFSET,
        protocol::CFG_KEY_PLL_DIVIDE,
        protocol::CFG_KEY_INTERLACE,
        protocol::CFG_KEY_F2_ROW,
        protocol::CFG_KEY_FIELD_SWAP,
        protocol::CFG_KEY_FIELD_SRC,
        protocol::CFG_KEY_VIDEO_BW,
        protocol::CFG_KEY_PHASE,
    ];

    /// ボードの現在値を表示へ反映する。
    ///
    /// 読み戻さないと表示と実体が食い違う。実際、ボード側で swap が有効なのに
    /// チェックボックスは外れたまま、という状態が起きた(設定はボードの電源で
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
        }
        // 未知の帯域は「1周期まるごとが管面に出る」から始める。管面の横幅は 1/fH
        // そのものなので、これで必ず全体が収まる。あとは実機のモニタに合わせて
        // H幅/V幅を1未満へ詰めていけばよい(帰線の間ビームは戻っているので、
        // 実際のCRTは 0.85〜0.95 あたりが近い)。
        self.mon = self.mon_bands.get(&khz).copied().unwrap_or([1.0, 0.0, 1.0, 0.0]);
        self.band_khz = khz;
        // H-PLLのVCOレンジ/チャージポンプはピクセルクロックとpixels per lineで
        // 決まるので、モードが変わったら計算して送り直す
        if let Some((pclk, ht)) = {
            let m = self.shared.mode.lock().unwrap().clone();
            m.map(|m| (m.dotclk_hz as f64, m.htotal as u32))
        } {
            if pclk > 1.0e6 && ht > 0 {
                let v = Self::pll_ctl_for(pclk, ht);
                self.send_cfg(protocol::CFG_KEY_PLL_CTL, v as u32);
            }
        }
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
                protocol::CFG_KEY_F2_ROW => self.tune_f2_row = val as i32,
                protocol::CFG_KEY_INTERLACE => self.tune_interlace = val as u8,
                protocol::CFG_KEY_FIELD_SWAP => self.tune_field_swap = val != 0,
                protocol::CFG_KEY_FIELD_SRC => self.tune_field_src = val as u8,
                protocol::CFG_KEY_VIDEO_BW => self.tune_video_bw = val as u8,
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
        row(ui, "pll_div", &mut self.tune_pll_divide, pll_min, 2304,
            protocol::CFG_KEY_PLL_DIVIDE, &mut send);
        if self.tune_pll_divide < pll_min {
            ui.colored_label(
                egui::Color32::from_rgb(220, 170, 60),
                format!("pll_div は {pll_min} 以上に(12MHz未満はTVPの範囲外)"),
            );
        }
        // 実測した有効映像の大きさと、そこから求まる pll_div の推奨値。
        // pll_div は「1ラインを何サンプルで取るか」なので、有効幅は pll_div に比例する。
        // 目標のドット数になるよう比例計算すれば一発で決まる(勘で動かす必要はない)。
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
        // 測定が信用できるか。有効映像が窓に入り切っていないと外接矩形は
        // 有効映像の幅を表さず、そこから計算した推奨値は正しい方向へ行かない。
        // 実際この状態で押し続けて pll_div が上限まで走り、DATACLKが100MHzを
        // 超えてボードがハングした。信用できないときはボタンを押せなくする。
        let mut measure_ok = st.active_w > 0;
        if st.active_w > 0 {
            let r = self.tune_target_w as f32 / st.active_w as f32;
            if !(0.75..=1.35).contains(&r) {
                measure_ok = false;
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
                let base = if self.tune_snap8 {
                    ((raw / 8.0).round() * 8.0) as i32
                } else {
                    raw.round() as i32
                };
                // 1サンプル=1ドットにするとTVPの下限(12MHz)を割るモードがある
                // (15kHz 512x480 の 1:1 は 608 = 9.7MHz)。その場合は2倍、
                // 足りなければ4倍にして範囲内へ入れる。オーバーサンプリングなので
                // 情報は失われず、8の倍数のままでもある。
                let mut want = base.max(1);
                let mut over = 1;
                while want < pll_min && want * 2 <= 2304 {
                    want *= 2;
                    over *= 2;
                }
                let want = want
                // 測定が壊れていると推奨値が青天井に走る。実際、連打してDATACLKが
                // 100MHzを超え、ボードがハングした。1回の変化は2倍までに抑え、
                // 上限もgateware側と揃える。
                .clamp(
                    (self.tune_pll_divide / 2).max(pll_min),
                    (self.tune_pll_divide * 2).min(2304),
                );
                let label = if over > 1 {
                    format!("→ pll {want} (×{over})")
                } else {
                    format!("→ pll {want}")
                };
                let btn = ui.add_enabled(measure_ok, egui::Button::new(label));
                let btn = if measure_ok {
                    btn
                } else {
                    btn.on_disabled_hover_text(
                        "実測(active)が目標と合っていないので計算できません。\n\
                         絵が窓に入り切っていない可能性があります。\n\
                         pll_div を手で入れて絵全体が入る状態にしてから使ってください。",
                    )
                };
                if btn.clicked() {
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
        // TVPのアナログ映像帯域。折り返しの元になる高周波を削る。
        // 15(最小=約95MHz)で、エッジ後の残留エコーが消える(実測 +2.55→+0.01)。
        // 立ち上がりは変わらず最細部が6%落ちるだけなので既定を15にしている。
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
            send.push((protocol::CFG_KEY_PLL_DIVIDE, self.tune_pll_divide as u32));
            send.push((protocol::CFG_KEY_PHASE, self.tune_phase as u32));
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
            // 太らせても埋まらなかった行数。0でないと前フレームの残りが減衰して
            // 薄い影として見える。プログレッシブでは0になるべき
            if s.unfilled_rows > 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 170, 60),
                    format!("未充填の行 {}", s.unfilled_rows),
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
                self.band_leds(ui);
                ui.horizontal(|ui| {
                    ui.monospace("H幅");
                    if ui.add(egui::DragValue::new(&mut self.mon[0])
                              .speed(0.002).range(0.2..=2.0).fixed_decimals(3))
                        .on_hover_text("1HSYNC周期のうち管面に出る割合。\n\
                                        1.0で帰線期間まで全部見える")
                        .changed() { self.mark_settings_dirty(); }
                    ui.monospace("H位置");
                    if ui.add(egui::DragValue::new(&mut self.mon[1])
                              .speed(0.002).range(-0.5..=0.5).fixed_decimals(3))
                        .changed() { self.mark_settings_dirty(); }
                });
                ui.horizontal(|ui| {
                    ui.monospace("V幅");
                    if ui.add(egui::DragValue::new(&mut self.mon[2])
                              .speed(0.002).range(0.2..=2.0).fixed_decimals(3))
                        .on_hover_text("1VSYNC周期のうち管面に出る割合")
                        .changed() { self.mark_settings_dirty(); }
                    ui.monospace("V位置");
                    if ui.add(egui::DragValue::new(&mut self.mon[3])
                              .speed(0.002).range(-0.5..=0.5).fixed_decimals(3))
                        .changed() { self.mark_settings_dirty(); }
                });
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
                if self.frame_size.0 == 0 {
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
                let aspect = if self.tube_aspect > 0.0 { self.tube_aspect } else { cw / ch.max(1.0) };
                let disp = if self.rotate % 2 == 1 {
                    egui::vec2(1.0, aspect)
                } else {
                    egui::vec2(aspect, 1.0)
                };
                // ウィンドウを画にぴったり合わせるのに使う(余白 = 内寸 - この領域)
                self.last_avail = avail;
                let fit = (avail.x / disp.x).min(avail.y / disp.y);
                let size = disp * fit;
                self.last_tex = size;
                if !self.did_autofit {
                    self.did_autofit = true;
                }
                let resp = ui.centered_and_justified(|ui| {
                    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::hover());
                    // Retina では論理座標と実画素が違う。sharp-bilinear は出力
                    // 画素数を基準に境目の幅を決めるので、実画素で渡す。
                    let ppp = ui.ctx().pixels_per_point();
                    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
                        rect,
                        render::Callback {
                            params: self.render_params(),
                            dst: (rect.width() * ppp, rect.height() * ppp),
                        },
                    ));
                    r
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
