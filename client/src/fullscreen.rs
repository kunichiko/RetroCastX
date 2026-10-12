//! フルスクリーン低遅延presentモード。
//!
//! pace_probe での実測(client/README.md)に基づく構成:
//! - winitの再描画スケジューリング(macOSでは60Hz量子化)を使わず、
//!   専用スレッドがフレーム到着を待って即present(タイミングはソース駆動)
//! - フルスクリーン(direct-to-display) + PresentMode::Immediate
//! - VRR対応パネル(ProMotion / Adaptive-Sync)ならパネルがソースレートに追従する
//!
//! UIはなし(統計はstderrに毎秒出力)。ESCまたはウィンドウクローズで終了。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::wgpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, NativeKeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::receiver;
use crate::remote_input;
use crate::render;

pub fn run(port: u16, bind: String, subscribe_to: Option<String>, target_mac: Option<[u8; 6]>, decay: f32,
           interlace_decay: f32,
           audio: receiver::AudioOpts, params: render::Params) -> ! {
    let shared = Arc::new(receiver::Shared::default());
    let subscribe_dest = subscribe_to.clone();
    let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
    receiver::spawn(
        receiver::Config { port, bind, subscribe_to, target_mac, decay, interlace_decay, audio },
        shared.clone(),
        move || {
            let _ = tx.send(());
        },
    )
    .expect("UDP bind failed");

    // このモードには UI 層が無いので、受信バッファーの警告は stderr にしか出せない。
    // 起動時に一度だけ。判定できないとき(Unsupported/Unknown)は黙る
    {
        let dest = subscribe_dest.unwrap_or_else(|| "255.255.255.255".to_string());
        let b = crate::netcheck::probe(&dest);
        if b.should_warn() {
            eprintln!(
                "警告: NICの受信バッファーが {} です(推奨 {})。\n\
                 　　  このままだとパケットを取りこぼし、映像に穴が開き音が途切れます。\n\
                 　　  管理者のPowerShellで: {}",
                b.value().unwrap_or(0),
                crate::netcheck::RECOMMENDED,
                crate::netcheck::fix_command(b.adapter()),
            );
        }
    }

    let event_loop = EventLoop::new().unwrap();
    let mut app = App { shared: Some((shared, rx)), window: None,
                        size: Arc::new(AtomicU64::new(0)), params,
                        remote: remote_input::RemoteInput::default(),
                        remote_toggle: Default::default(), remote_mods: Default::default(),
                        awake: crate::awake::KeepAwake::new() };
    event_loop.run_app(&mut app).unwrap();
    std::process::exit(0);
}

struct App {
    shared: Option<(Arc<receiver::Shared>, Receiver<()>)>,
    window: Option<Arc<Window>>,
    size: Arc<AtomicU64>, // (w<<32)|h — リサイズをrenderスレッドへ伝える
    params: render::Params,
    /// MimicX へのキー転送。この経路は winit の生イベントを直接見られるので、
    /// テンキーやJIS配列のキー(かな/変換/¥/ろ)も区別して送れる
    /// (通常モードは egui の変換を経るためテンキーが最上段と同じになる)
    remote: remote_input::RemoteInput,
    remote_toggle: remote_input::ToggleDetect,
    /// いまの修飾(⌘ / Shift)。転送ON/OFFの組み合わせ判定に使う
    remote_mods: remote_input::Mods,
    /// 前面にある間は OS のスリープを止める(awake.rs)。全画面は映像を見るための
    /// 起動形態なので、受信の有無は見ずにフォーカスだけで決める
    awake: crate::awake::KeepAwake,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Borderless(None)は不可視のディスプレイ(TV/キャプチャ等)に開くことが
        // あるためプライマリモニタを明示する
        let mut monitor = event_loop.primary_monitor();
        // 調査用: RCX_MONITOR=番号 で出すモニタを選ぶ(一覧は起動時に出す)。
        // スケーリング表示のモニタと等倍のモニタで present の経路を比べるため
        for (i, m) in event_loop.available_monitors().enumerate() {
            eprintln!("fullscreen: monitor #{i} {:?} {:?} {:?}mHz scale {}",
                      m.name().unwrap_or_default(), m.size(),
                      m.refresh_rate_millihertz(), m.scale_factor());
        }
        if let Some(i) = std::env::var("RCX_MONITOR").ok().and_then(|v| v.parse::<usize>().ok()) {
            monitor = event_loop.available_monitors().nth(i).or(monitor);
        }
        if let Some(m) = &monitor {
            eprintln!("fullscreen: on monitor {:?}", m.name().unwrap_or_default());
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    {
                        let attrs = Window::default_attributes()
                            .with_title("RetroCast X (fullscreen)")
                            .with_fullscreen(Some(Fullscreen::Borderless(monitor)));
                        match crate::appicon::winit_icon() {
                            Some(icon) => attrs.with_window_icon(Some(icon)),
                            None => attrs,
                        }
                    },
                )
                .unwrap(),
        );
        self.window = Some(window.clone());
        let sz = window.inner_size();
        self.size
            .store(((sz.width as u64) << 32) | sz.height as u64, Ordering::Relaxed);
        // サーフェス作成はmacOSではメインスレッド限定なのでここで初期化し、
        // present一式を専用スレッドへ移す
        let gpu = Gpu::new(window);
        let (shared, rx) = self.shared.take().unwrap();
        let size = self.size.clone();
        let p = self.params;
        std::thread::spawn(move || render_thread(gpu, shared, rx, size, p));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            // ⌘の状態。転送ON/OFFの組み合わせ(⌘+ESC)の判定に使う
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.remote_mods = remote_input::Mods {
                    command: if cfg!(target_os = "macos") {
                        s.super_key()
                    } else {
                        s.control_key()
                    },
                    shift: s.shift_key(),
                };
                if !self.remote_mods.is_toggle_combo() {
                    // 修飾が揃っていない間はラッチを戻す。macOS は ⌘ を押し
                    // ながらの keyUp を配送しないことがあり、これが無いと
                    // 2回目が効かない
                    self.remote_toggle.rearm_chord();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let (code, vk) = match event.physical_key {
                    PhysicalKey::Code(c) => (Some(c), None),
                    // winit は macOS の JIS キー(ろ/かな/英数、テンキーの ,)を
                    // 未対応のまま残しているので、生のキーコードから補う
                    PhysicalKey::Unidentified(NativeKeyCode::MacOS(vk)) => {
                        (remote_input::keycode_from_mac_vk(vk), Some(vk))
                    }
                    PhysicalKey::Unidentified(_) => (None, None),
                };
                // 転送のON/OFF(⌘+ESC / F12)。転送中でも必ず効く(唯一の抜け道)。
                // ESC 単独は横取りされないので、下の終了判定と実機への転送へ流れる
                if let Some(code) = code {
                    remote_input::log_key(code, vk, event.state.is_pressed(), self.remote_mods);
                    let (toggle, eat) =
                        self.remote_toggle
                            .feed(code, event.state.is_pressed(), self.remote_mods);
                    if toggle {
                        self.remote.toggle();
                        self.remote.update(true);
                        eprintln!(
                            "remote input: {}{}",
                            if self.remote.enabled() { "on" } else { "off" },
                            if self.remote.enabled() && !self.remote.connected() {
                                format!(" — {}", self.remote.status())
                            } else {
                                String::new()
                            }
                        );
                    }
                    if eat {
                        return;
                    }
                }
                // 転送中の ESC は X68000 の ESC として送る。抜けるには先に
                // 転送を切る(borderless フルスクリーンには閉じるボタンが無いので、
                // ESC が唯一の終了手段になる)
                if !self.remote.enabled() && event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                    return;
                }
                if !self.remote.active() {
                    return;
                }
                // 送るのは押下と解放のエッジだけ。リピートは MimicX が自前の
                // Timer で作るので転送すると二重にかかる
                if event.repeat {
                    return;
                }
                let Some(code) = code else { return };
                // ⌘ を押している間は Mac の操作(⌘Q で終了など)。押下は実機へ
                // 送らない。X68000 のキーボードに ⌘ は無いので、⌘ 付きの打鍵が
                // 実機向けであることはない。
                // **解放は ⌘ の有無に関わらず送る** — 押している途中で ⌘ を
                // 足すと、解放だけ落ちて実機にキーが残る。
                if event.state.is_pressed() && self.remote_mods.command {
                    return;
                }
                // ⌘ 自体も送らない(実機に無いキーで、MimicX も捨てる)
                if matches!(code, KeyCode::SuperLeft | KeyCode::SuperRight) {
                    return;
                }
                if let Some(usage) = remote_input::usage_from_keycode(code) {
                    self.remote.key(usage, event.state.is_pressed());
                }
            }
            // フォーカスを失うと以後のキー解放が届かない。全解放しないと
            // キーが押しっぱなしで実機に残る。押下状態も忘れる(⌘+ESC の
            // 解放が届かないまま残ると次のトグルが効かなくなる)
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.remote_toggle.reset();
                    self.remote_mods = Default::default();
                }
                self.remote.update(focused);
                self.awake.set(focused);
            }
            WindowEvent::Resized(sz) => {
                self.size
                    .store(((sz.width as u64) << 32) | sz.height as u64, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// 終了時にも全解放する。`run` は exit(0) するので Drop は走らない
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.remote.set_enabled(false);
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    blit: render::Pipeline,
    uniform: wgpu::Buffer,
}


impl Gpu {
    fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            }))
            .unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .unwrap();

        let caps = surface.get_capabilities(&adapter);
        // 調査用: RCX_PRESENT_MODE=fifo で vsync に固定する(計測の対照実験用。
        // vsync なら画面に出る時刻はリフレッシュ格子に乗るはず)
        let force_fifo = std::env::var("RCX_PRESENT_MODE").ok().as_deref() == Some("fifo");
        let mode = if force_fifo {
            wgpu::PresentMode::Fifo
        } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            eprintln!("fullscreen: no unsynced present mode; falling back to Fifo (vsync)");
            wgpu::PresentMode::Fifo
        };
        let sz = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, sz.width.max(1), sz.height.max(1))
            .unwrap();
        config.present_mode = mode;
        // 調査用: RCX_FRAME_LATENCY で drawable の段数(既定3)を変えられる
        config.desired_maximum_frame_latency = std::env::var("RCX_FRAME_LATENCY").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(3);
        surface.configure(&device, &config);
        eprintln!("fullscreen: present mode {mode:?}, {}x{}, surface {:?}, frame latency {}",
                  config.width, config.height, config.format,
                  config.desired_maximum_frame_latency);

        let blit = render::Pipeline::new(&device, config.format);
        let uniform = blit.make_uniform_buffer(&device);
        Self { surface, device, queue, config, blit, uniform }
    }
}

fn render_thread(
    mut gpu: Gpu,
    shared: Arc<receiver::Shared>,
    rx: Receiver<()>,
    size: Arc<AtomicU64>,
    params: render::Params,
) {
    let mut texture: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)> = None;
    let mut fail_count = 0u64;
    let mut seen_gen = 0u64;
    let mut last_paint: Option<Instant> = None;
    let mut intervals: Vec<f64> = Vec::new();
    let mut last_log = Instant::now();
    // 幾何に使う vtotal の平滑値(main.rs の vtotal_smooth と同じ理由)
    let mut vtotal_smooth = 0.0f32;
    // 平滑の時定数は**時間**で効かせる。フレーム数で減衰させると、MODEが1秒に
    // 1〜2回しか更新されないのに毎フレーム呼ぶせいで収束しきり、平均されない
    let mut vtotal_last_t: Option<std::time::Instant> = None;
    // 描画遅延の計測(latency.rs)。macOS では present を自前で行い、
    // 実際に画面に出た時刻(presentedTime)まで測る
    #[cfg(target_os = "macos")]
    let mut presenter = crate::metal_present::MetalPresenter::new(&gpu.surface, &gpu.queue);
    #[cfg(target_os = "macos")]
    eprintln!("present: {}", if presenter.is_some() {
        "自前(Metal, presentedTime で画面に出た時刻を測る)"
    } else {
        "wgpu(画面に出た時刻は測れない)"
    });
    let mut meter = crate::latency::LatencyMeter::default();
    // 調査用: RCX_PRESENT_CSV=パス で、フレームごとの present 呼び出しと画面に出た
    // 時刻を書き出す(ms, 起動からの経過)。present→画面 の分布や、出る時刻が
    // モニタのリフレッシュ格子に乗っているか(=vsync待ち/コンポジタ経由)を見る
    let t_origin = Instant::now();
    let mut present_csv = std::env::var("RCX_PRESENT_CSV").ok()
        .and_then(|p| std::fs::File::create(p).ok())
        .map(std::io::BufWriter::new);
    if let Some(w) = present_csv.as_mut() {
        use std::io::Write;
        let _ = writeln!(w, "id,fresh,acquire_ms,submit_ms,presented_ms,\
                             first_ms,last_ms,trigger_ms,published_ms,picked_ms");
    }
    // present 済みで画面に出るのを待っているもの:
    // (id, 新フレームなら(時刻, 拾った), drawable 取得に掛かった時間, present した)。
    // 新フレームでない present(ハートビート・溜まった起床)も追う: それが新フレームを
    // 差し替えて「捨てられた」に見せている可能性があるため
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut, unused_variables))]
    // ★**id で引く。** presented handler は id 順に来るとは限らない。以前は先頭から
    //   順に突き合わせ、来た id より前のものを黙って捨てていたので、順序が入れ替わる
    //   と記録が抜け、残った行だけを見て「フレームが届いていない」と誤読した。
    let mut in_flight: std::collections::HashMap<
        u64, (Option<(crate::latency::FrameStamps, Instant)>, Duration, Instant)> =
        Default::default();

    loop {
        // 新フレーム到着まで待つ(タイムアウトでハートビート描画: 統計・黒画面維持)
        let _ = rx.recv_timeout(Duration::from_millis(250));
        let generation = shared.frame_gen.load(Ordering::Acquire);
        let fresh = generation != seen_gen;
        seen_gen = generation;

        // リサイズ反映
        let packed = size.load(Ordering::Relaxed);
        let (sw, sh) = ((packed >> 32) as u32, packed as u32);
        if sw > 0 && sh > 0 && (sw != gpu.config.width || sh != gpu.config.height) {
            gpu.config.width = sw;
            gpu.config.height = sh;
            gpu.surface.configure(&gpu.device, &gpu.config);
        }

        // 今回拾ったフレームの時刻(present したら計測に回す)
        let mut picked: Option<(crate::latency::FrameStamps, Instant)> = None;
        if fresh {
            let guard = shared.frame.lock().unwrap();
            if let Some(frame) = guard.as_ref() {
                picked = frame.stamps.map(|st| (st, Instant::now()));
                let (w, h) = (frame.width as u32, frame.height as u32);
                if texture.as_ref().map(|t| (t.2, t.3)) != Some((w, h)) {
                    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("video"),
                        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        // 描画先(サーフェス)の sRGB 性に合わせる。
                        // 詳細は render::video_tex_format のコメント。
                        format: crate::render::video_tex_format(gpu.config.format),
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let view = tex.create_view(&Default::default());
                    let bind = gpu.blit.bind_group(&gpu.device, &view, &gpu.uniform);
                    texture = Some((tex, bind, w, h));
                }
                // 画面と画の縦横比から letterbox 係数を決める。回転する場合は
                // 画面に占める向きが入れ替わるので、幅と高さを入れ替えて考える。
                {
                    // 幾何は MODE と画枠設定から毎フレーム作り直す。UIが無いので
                    // 受信スレッドが集めた値(config_state)を使う。
                    let mut p = params;
                    if let Some(m) = shared.mode.lock().unwrap().as_ref() {
                        p.htotal = m.htotal as u32;
                        // vtotal は平滑した値を使う。**262/263 の交互は正常**
                        // (インターレースは262.5ライン/フィールド)なので、瞬間値を
                        // 幾何に使うと縦のスケールがフレームごとに0.38%変わり、
                        // 絵が上下に震える。詳細は main.rs の vtotal_smooth のコメント。
                        let now = std::time::Instant::now();
                        let dt = vtotal_last_t
                            .map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32());
                        vtotal_last_t = Some(now);
                        vtotal_smooth = render::smooth_vtotal(
                            vtotal_smooth, m.vtotal as f32, false, dt);
                        p.vtotal = vtotal_smooth;
                        // 管面を時間ベースで決めるのに必要(実CRTと同じ挙動)
                        p.fh_hz = (m.hfreq_mhz_x1000 / 1000) as u32;
                        p.hactive = m.hactive as u32;
                        p.vactive = m.vactive as u32;
                    }
                    {
                        let st = shared.stats.lock().unwrap();
                        p.act_x = st.active_x as u32;
                        p.act_y = st.active_y as u32;
                        p.act_w = st.active_w as u32;
                        p.act_h = st.active_h as u32;
                    }
                    {
                        let st = shared.config_state.lock().unwrap();
                        if let Some(v) = st.get(&crate::protocol::CFG_KEY_HS_OFFSET) {
                            p.hs_offset = *v;
                        }
                        if let Some(v) = st.get(&crate::protocol::CFG_KEY_VBP) {
                            p.vbp = *v;
                        }
                    }
                    gpu.blit.write_uniforms(&gpu.queue, &gpu.uniform, &p, w, h,
                                            gpu.config.width.max(1) as f32,
                                            gpu.config.height.max(1) as f32);
                }
                let (tex, _, _, _) = texture.as_ref().unwrap();
                gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &frame.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * w),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            }
        }

        if last_log.elapsed().as_secs_f64() >= 1.0 {
            let s = shared.stats.lock().unwrap().clone();
            if intervals.is_empty() {
                eprintln!("no presents | frames={} pkts={} {:.1} Mbps (window occluded?)",
                          s.frames, s.packets, s.mbps);
            } else {
                let n = intervals.len() as f64;
                let mean = intervals.iter().sum::<f64>() / n;
                let var =
                    intervals.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
                eprintln!(
                    "present {:.2}ms σ{:.2} → {:.2}Hz | {:.1} Mbps lost={}",
                    mean,
                    var.sqrt(),
                    1000.0 / mean,
                    s.mbps,
                    s.lost_packets
                );
                intervals.clear();
            }
            let snap = meter.take();
            if snap.frames > 0 {
                let frame_ms = shared.mode.lock().unwrap().as_ref()
                    .filter(|m| m.vfreq_mhz_x1000 > 0)
                    .map(|m| 1.0e6 / m.vfreq_mhz_x1000 as f32);
                eprintln!("{}", snap.describe(frame_ms));
            }
            *shared.latency.lock().unwrap() = snap;
            last_log = Instant::now();
        }

        let Some((_, bind, tw, th)) = texture.as_ref() else { continue };

        // 描画先。macOS は自前で drawable を取る(presentedTime を読むため)
        let t_acq = Instant::now();
        #[cfg(target_os = "macos")]
        let mut acquired = match presenter.as_ref() {
            Some(p) => match p.acquire(&gpu.device, gpu.config.format) {
                Some(a) => Some(a),
                None => {
                    fail_count += 1;
                    if fail_count <= 5 || fail_count % 256 == 0 {
                        eprintln!("nextDrawable #{fail_count}: 取れなかった");
                    }
                    continue;
                }
            },
            None => None,
        };
        #[cfg(not(target_os = "macos"))]
        let acquired: Option<()> = None;
        let mut wgpu_frame = None;
        if acquired.is_none() {
            let f = match gpu.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f)
                | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                other => {
                    fail_count += 1;
                    if fail_count <= 5 || fail_count % 256 == 0 {
                        eprintln!("get_current_texture #{fail_count}: {}",
                                  match other {
                                      wgpu::CurrentSurfaceTexture::Timeout => "Timeout",
                                      wgpu::CurrentSurfaceTexture::Occluded => "Occluded",
                                      wgpu::CurrentSurfaceTexture::Outdated => "Outdated",
                                      wgpu::CurrentSurfaceTexture::Lost => "Lost",
                                      _ => "other",
                                  });
                    }
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    continue;
                }
            };
            wgpu_frame = Some(f);
        }
        #[cfg(target_os = "macos")]
        let view = match (&acquired, &wgpu_frame) {
            (Some(a), _) => a.texture.create_view(&Default::default()),
            (None, Some(f)) => f.texture.create_view(&Default::default()),
            (None, None) => continue,
        };
        #[cfg(not(target_os = "macos"))]
        let view = match &wgpu_frame {
            Some(f) => f.texture.create_view(&Default::default()),
            None => continue,
        };
        let t_acq_end = Instant::now();
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            // レターボックス(可能なら整数拡大)
            let (sw, sh) = (gpu.config.width as f32, gpu.config.height as f32);
            let (tw, th) = (*tw as f32, *th as f32);
            let fit = (sw / tw).min(sh / th);
            let scale = if fit >= 1.0 { fit.floor() } else { fit };
            let (vw, vh) = (tw * scale, th * scale);
            rp.set_viewport((sw - vw) * 0.5, (sh - vh) * 0.5, vw, vh, 0.0, 1.0);
            rp.set_pipeline(&gpu.blit.pipeline);
            rp.set_bind_group(0, bind, &[]);
            rp.draw(0..3, 0..1);
        }
        gpu.queue.submit([enc.finish()]);
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
        let t_acq_dur = t_acq_end.saturating_duration_since(t_acq);
        #[cfg(target_os = "macos")]
        if let (Some(a), Some(p)) = (acquired.take(), presenter.as_mut()) {
            let id = p.present(a);
            in_flight.insert(id, (picked, t_acq_dur, Instant::now()));
        }
        if let Some(f) = wgpu_frame {
            f.present();
            // 画面に出た時刻は測れないので present まで
            if let Some((st, t_pick)) = picked {
                meter.add(&st, t_pick, Some(Instant::now()), None);
            }
        }
        // 画面に出たものを計測へ回す。出なかった(捨てられた)ものは数えない
        #[cfg(target_os = "macos")]
        if let Some(p) = presenter.as_ref() {
            for (id, presented) in p.drain() {
                let Some((fresh, acq, t_sub)) = in_flight.remove(&id) else { continue };
                if let (Some((st, t_pick)), Some(t)) = (fresh, presented) {
                    meter.add(&st, t_pick, Some(t_sub), Some(t));
                }
                if let Some(w) = present_csv.as_mut() {
                    use std::io::Write;
                    let ms = |x: Instant| x.saturating_duration_since(t_origin)
                        .as_secs_f64() * 1000.0;
                    let stages = fresh.map(|(st, tp)| format!(
                        "{:.3},{:.3},{:.3},{:.3},{:.3}",
                        ms(st.first), ms(st.last), ms(st.trigger),
                        ms(st.published), ms(tp)))
                        .unwrap_or_else(|| ",,,,".into());
                    let _ = writeln!(w, "{id},{},{:.3},{:.3},{},{stages}",
                        fresh.is_some() as u8,
                        acq.as_secs_f64() * 1000.0, ms(t_sub),
                        presented.map(|t| format!("{:.3}", ms(t)))
                            .unwrap_or_else(|| "skipped".into()));
                }
            }
            // 届かないまま溜まり続けないように(画面が隠れている等)
            // 1秒たっても届かないものは捨てる
            in_flight.retain(|_, v| v.2.elapsed() < Duration::from_secs(1));
        }

        if fresh {
            let now = Instant::now();
            if let Some(prev) = last_paint {
                intervals.push((now - prev).as_secs_f64() * 1000.0);
            }
            last_paint = Some(now);
        }
    }
}
