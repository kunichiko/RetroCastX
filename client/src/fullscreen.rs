//! フルスクリーン低遅延presentモード。
//!
//! pace_probe での実測(client/README.md)に基づく構成:
//! - winitの再描画スケジューリング(macOSでは60Hz量子化)を使わず、
//!   専用スレッドがフレーム到着を待って即present(タイミングはソース駆動)
//! - フルスクリーン(direct-to-display) + PresentMode::Immediate
//! - VRR対応パネル(ProMotion / Adaptive-Sync)ならパネルがソースレートに追従する
//!
//! UIはなし(統計はstderrに毎秒出力)。ESCまたはウィンドウクローズで終了。
//! ⌘L(Windows/Linux は Ctrl+L)で表示遅延の計測を開始/停止する(photo_lat.rs)。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// `vsync`: true なら present をモニタのリフレッシュに合わせる(Fifo)。
/// 既定(false)は Immediate で、遅延は小さいが画面の途中に継ぎ目(ティアリング)が出る。
/// 60Hz で present→画面 は Immediate 中央値 4.7ms / vsync 27ms(実測)。
/// 高リフレッシュのモニタほど vsync の代償は小さい(1リフレッシュ周期に比例)。
pub fn run(port: u16, bind: String, subscribe_to: Option<String>, target_mac: Option<[u8; 6]>, decay: f32,
           interlace_decay: f32,
           audio: receiver::AudioOpts, params: FsGeom, vsync: bool,
           return_window: bool, photo_lat_high: bool) -> ! {
    // Metal の HUD(⌘I で出し入れ)。HUD は Metal を初期化するときに環境変数を見て
    // 読み込まれるので、ここで入れておき、出すかどうかは layer の設定で切り替える。
    // 自分で MTL_HUD_ENABLED を付けて起動したときは最初から出す。
    // ★**右下に置く。** 既定の右上は macOS のメニュー(上端にカーソルを持って
    //   いくと出る)と重なる。MTL_HUD_ALIGNMENT を自分で指定したときはそちらに従う
    let hud_given = std::env::var_os("MTL_HUD_ENABLED").is_some();
    let align_given = std::env::var_os("MTL_HUD_ALIGNMENT").is_some();
    // SAFETY: まだ他のスレッドを立てていない(受信スレッドはこの後で作る)
    unsafe {
        if !hud_given {
            std::env::set_var("MTL_HUD_ENABLED", "1");
        }
        if !align_given {
            std::env::set_var("MTL_HUD_ALIGNMENT", "bottomright");
        }
    }
    let hud = Arc::new(AtomicBool::new(hud_given));
    let shared = Arc::new(receiver::Shared::default());
    // 受信(=再生器を開く)より前に入れる。後からだと開いた直後に 100% で鳴る
    *shared.gain.lock().unwrap() = Some(params.gain);
    // 組み立ての設定も受信より前に(最初のフレームから効くように)
    shared.ycbcr.store(params.ycbcr, Ordering::Relaxed);
    shared.dot_a_milli.store(params.dot_a_milli, Ordering::Relaxed);
    shared.comb3d.store(params.comb3d, Ordering::Relaxed);
    shared.deint.store(params.deint.to_u8(), Ordering::Relaxed);
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

    // S/PDIF の混ぜ方は再生器より長生きする AuxCtl に置く(作り直しても消えない)
    shared.aux.set(params.spdif.0, params.spdif.1);
    let event_loop = EventLoop::new().unwrap();
    let shared_at_exit = shared.clone();
    let mut app = App { lat_shared: shared.clone(), lat_on: false, lat_high: photo_lat_high,
                        shared: Some((shared, rx)), window: None, hud, cursor_hidden: false,
                        size: Arc::new(AtomicU64::new(0)), params, vsync,
                        remote: remote_input::RemoteInput::default(),
                        remote_toggle: Default::default(), remote_mods: Default::default(),
                        awake: crate::awake::KeepAwake::new() };
    event_loop.run_app(&mut app).unwrap();
    // 計測したまま抜けたら止める。切り替えのあいだの点滅を測らない
    if app.lat_on {
        app.toggle_photo_lat();
        crate::photo_lat::flush(&shared_at_exit, Duration::from_millis(300));
    }
    // パネルの「低遅延全画面」から来たときは、ウィンドウ表示へ戻す。
    // 受信中のボードを指定して戻る(戻った先で別のボードを掴まないように)
    if return_window {
        // 入れた HUD の環境変数を戻ったウィンドウ表示へ持ち込まない
        // (ウィンドウ表示には HUD を消す手段が無く、出しっぱなしになる)
        // SAFETY: 描画スレッドは event loop と一緒に終わっている。受信スレッドは
        // 環境変数を読まない
        unsafe {
            if !hud_given {
                std::env::remove_var("MTL_HUD_ENABLED");
            }
            if !align_given {
                std::env::remove_var("MTL_HUD_ALIGNMENT");
            }
        }
        let mac = *shared_at_exit.claimed.lock().unwrap();
        let extra: Vec<String> = mac
            .map(|m| vec!["--mac".into(), crate::format_mac(&m)])
            .unwrap_or_default();
        crate::relaunch(&extra);
    }
    std::process::exit(0);
}

/// 全画面の幾何に要る設定。ウィンドウ表示と同じ geometry.rs で Params を作る。
/// 帯域ごとのモニタ設定はその場のモードで引く(全画面中にモードが変わっても追う)
#[derive(Clone)]
pub struct FsGeom {
    pub rotate: u32,
    pub tube_aspect: f32,
    pub time_based: bool,
    pub filter: u32,
    pub window: [f32; 4],
    pub mon_bands: std::collections::BTreeMap<u32, [f32; 4]>,
    /// コンポジット/S端子の管面の既定(帯域の保存値が無いとき)
    pub mon_ntsc: [f32; 4],
    /// 音量(ミュート込み。0.0 で無音)。★**全画面にはパネルが無いので、
    ///   保存した音量をここで掛ける。** 掛けないと再生器の既定 1.0(100%)で鳴る
    ///   (パネルから切り替えたら音量がいきなり上がった, 2026-10-13)
    pub gain: f32,
    /// S/PDIF を混ぜるか・その音量
    pub spdif: (bool, f32),
    /// 1フレームを何本の帯に分けて、届いたそばから出すか(0 = フレーム全体だけ)
    pub slices: u32,
    /// コンポーネント(Pr/Y/Pb を RGB に直す)か。★**ウィンドウ表示では UI が
    ///   毎フレーム `Shared` へ書いているが、全画面には UI が無いのでここで渡す。**
    ///   渡さないと Pr/Y/Pb がそのまま RGB として出て色がおかしくなった(2026-11-17)
    pub ycbcr: bool,
    /// ドット復元の係数(同じく UI が書いていたもの)
    pub dot_a_milli: u32,
    /// コンポジットの3次元コム(同上)
    pub comb3d: bool,
    /// インターレースの出し方(同上)
    pub deint: crate::assembler::Deint,
}

impl FsGeom {
    /// いまのモードでの Params と管面の縦横比
    fn params(&self, shared: &receiver::Shared, vt: &mut crate::geometry::VtotalSmoother,
              frame: (f32, f32)) -> (render::Params, (f32, f32))
    {
        use crate::geometry as g;
        let m = shared.mode.lock().unwrap().clone();
        let ntsc = m.as_ref().is_some_and(|m| crate::protocol::is_raw8(m.pixfmt));
        // ウィンドウ側(main.rs の follow_band)と同じく、帯域 × 垂直周波数 → 帯域の順に引く
        let key = match &m {
            _ if ntsc => g::MON_KEY_NTSC,
            Some(m) if m.hfreq_mhz_x1000 > 1_000_000 =>
                g::mode_key_of(g::band_of(m.hfreq_mhz_x1000 as f32 / 1_000_000.0), m),
            _ => 0,
        };
        let default = if ntsc { self.mon_ntsc } else { [1.0, 0.0, 1.0, 0.0] };
        let mon = self.mon_bands.get(&key)
            .or_else(|| self.mon_bands.get(&g::key_band(key)))
            .copied().unwrap_or(default);
        let tube = g::tube_aspect_eff(self.tube_aspect, ntsc, self.time_based);
        // 絵の広がりは span_*(明るい画素がある範囲)。無ければ active_*(main.rs と同じ)
        let act = {
            let st = shared.stats.lock().unwrap();
            if st.span_w > 64 {
                (st.span_x as u32, st.span_y as u32, st.span_w as u32, st.span_h as u32)
            } else {
                (st.active_x as u32, st.active_y as u32, st.active_w as u32, st.active_h as u32)
            }
        };
        let vbp = shared.config_state.lock().unwrap()
            .get(&crate::protocol::CFG_KEY_VBP).copied().unwrap_or(0);
        let vtotal = vt.update(m.as_ref());
        let p = g::params(m.as_ref(), vtotal, act, &g::Inputs {
            rotate: self.rotate, tube, filter: self.filter, window: self.window,
            time_based: self.time_based, mon, vbp,
        });
        (p, g::display_aspect(frame, tube, self.rotate))
    }
}

struct App {
    shared: Option<(Arc<receiver::Shared>, Receiver<()>)>,
    /// 表示遅延の計測(⌘L)。受信の Shared は render スレッドへ渡すので別に持つ
    lat_shared: Arc<receiver::Shared>,
    lat_on: bool,
    /// センサーが光ると High(`--photo-lat-high`)
    lat_high: bool,
    /// Metal の HUD を出すか(⌘I)。描画スレッドが layer に反映する
    hud: Arc<AtomicBool>,
    /// NSCursor.hide を呼んで、まだ unhide していないか(hide/unhide は回数で数える
    /// ので、前面に来たら1回隠し、裏へ回ったら1回戻す)
    cursor_hidden: bool,
    window: Option<Arc<Window>>,
    size: Arc<AtomicU64>, // (w<<32)|h — リサイズをrenderスレッドへ伝える
    params: FsGeom,
    /// present をリフレッシュに合わせるか(`--vsync`)
    vsync: bool,
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

impl App {
    /// 前面にいる間はカーソルを隠す(macOS)。hide/unhide は回数で数えるので
    /// 状態を持って釣り合わせる
    fn set_cursor(&mut self, hide: bool) {
        #[cfg(target_os = "macos")]
        if hide != self.cursor_hidden {
            use objc2_app_kit::NSCursor;
            if hide { NSCursor::hide() } else { NSCursor::unhide() }
            self.cursor_hidden = hide;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = hide;
    }

    /// ⌘L。止めたら少し待って集計を読み、stderr に出す
    /// (パネルが無いので。ウィンドウ表示に戻ればパネルでも読める)
    fn toggle_photo_lat(&mut self) {
        use crate::photo_lat;
        let sh = &self.lat_shared;
        self.lat_on = !self.lat_on;
        if self.lat_on {
            photo_lat::send_start(sh, self.lat_high);
            eprintln!("表示遅延: 計測を開始(⌘L で停止)");
        } else {
            photo_lat::send_stop(sh, self.lat_high);
            eprintln!("表示遅延: 計測を停止");
            let sh = sh.clone();
            // 応答が1つでも落ちると揃わないので、揃うまで何度か頼み直す
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                for _ in 0..10 {
                    photo_lat::request_stats(&sh);
                    std::thread::sleep(Duration::from_millis(300));
                    if let Some(t) = photo_lat::summary(&sh) {
                        eprintln!("表示遅延: {t}");
                        return;
                    }
                }
                eprintln!("表示遅延: 集計を読めませんでした");
            });
        }
    }
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
        // ★**カーソルは隠す。** 全画面にマウスで触るものは無い。カーソルが出て
        //   いると macOS は直接表示(Direct)をやめて合成に戻し、出るまでの間隔が
        //   延びる(Metal HUD の Frame Interval が 2.5ms → 20ms 前後、実測)。
        //   macOS は winit の set_cursor_visible では消えなかったので NSCursor で
        //   隠す(set_cursor)。他の OS 向けに winit の方も呼んでおく
        window.set_cursor_visible(false);
        self.set_cursor(true);
        self.window = Some(window.clone());
        let sz = window.inner_size();
        self.size
            .store(((sz.width as u64) << 32) | sz.height as u64, Ordering::Relaxed);
        // サーフェス作成はmacOSではメインスレッド限定なのでここで初期化し、
        // present一式を専用スレッドへ移す
        let gpu = Gpu::new(window, self.vsync);
        let (shared, rx) = self.shared.take().unwrap();
        let size = self.size.clone();
        let p = self.params.clone();
        let hud = self.hud.clone();
        std::thread::spawn(move || render_thread(gpu, shared, rx, size, p, hud));
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
                // ⌘L で表示遅延の計測を開始/停止。⌘ 付きの押下は実機へ送らないので、
                // キー転送中でも横取りしてよい
                if code == Some(KeyCode::KeyL) && self.remote_mods.command
                    && event.state.is_pressed() && !event.repeat
                {
                    self.toggle_photo_lat();
                    return;
                }
                // ⌘I で Metal の HUD を出し入れ、⌘F で全画面を抜ける(ウィンドウ表示の
                // ⌘F で入るのと対)。⌘ 付きなので、キー転送中でも横取りしてよい
                if code == Some(KeyCode::KeyI) && self.remote_mods.command
                    && event.state.is_pressed() && !event.repeat
                {
                    let on = !self.hud.load(Ordering::Relaxed);
                    self.hud.store(on, Ordering::Relaxed);
                    return;
                }
                if code == Some(KeyCode::KeyF) && self.remote_mods.command
                    && event.state.is_pressed() && !event.repeat
                {
                    event_loop.exit();
                    return;
                }
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
                // ⌘Tab などで裏へ回ったらカーソルを戻し、前面に戻ったらまた隠す
                self.set_cursor(focused);
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
    fn new(window: Arc<Window>, vsync: bool) -> Self {
        // 既定の API(Windows は DX12)で取れなければ、指定が無い限り wgpu の既定へ逃げる
        let open = |backends: Option<wgpu::Backends>| {
            let instance = match backends {
                Some(backends) => wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends,
                    ..wgpu::InstanceDescriptor::new_without_display_handle()
                }),
                None => wgpu::Instance::default(),
            };
            let surface = instance.create_surface(window.clone()).ok()?;
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: Some(&surface),
                    ..Default::default()
                }))
                .ok()?;
            Some((instance, surface, adapter))
        };
        let preferred = render::preferred_backends();
        let (_instance, surface, adapter) = match open(preferred) {
            Some(v) => v,
            None if preferred.is_some() && !render::backend_overridden() => {
                eprintln!("fullscreen: {preferred:?} が使えないので wgpu の既定で開きます");
                open(None).expect("fullscreen: 描画アダプタが見つからない")
            }
            None => panic!("fullscreen: {preferred:?} で描画アダプタが見つからない"),
        };
        eprintln!("fullscreen: adapter {}", render::describe_adapter(&adapter.get_info()));
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .unwrap();

        let caps = surface.get_capabilities(&adapter);
        // `--vsync` なら Fifo。継ぎ目(ティアリング)を出さない代わりに、次の
        // リフレッシュまで待つぶん遅れる。計測の対照にもなる(vsync なら画面に出る
        // 時刻はリフレッシュ格子に乗る)
        let mode = if vsync {
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
        // 調査用: RCX_FRAME_LATENCY で drawable の段数を変えられる。
        // ★Windows の既定は 1。present 間隔はソース(55.46Hz)に追従しているので
        //   段数を積んでも詰まりは減らず、積んだぶん遅れるだけ(render::preferred_backends
        //   の実測を参照)。macOS は 3 のまま
        let default_latency = if cfg!(windows) { 1 } else { 3 };
        config.desired_maximum_frame_latency = std::env::var("RCX_FRAME_LATENCY").ok()
            .and_then(|v| v.parse().ok()).unwrap_or(default_latency);
        surface.configure(&device, &config);
        eprintln!("fullscreen: present mode {mode:?}, {}x{}, surface {:?}, frame latency {}",
                  config.width, config.height, config.format,
                  config.desired_maximum_frame_latency);

        let blit = render::Pipeline::new(&device, config.format);
        let uniform = blit.make_uniform_buffer(&device);
        Self { surface, device, queue, config, blit, uniform }
    }
}

/// テクスチャの `y0` 行目から `rgba` の行数ぶんを書き換える
fn write_rows(queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, y0: u32, rgba: &[u8]) {
    let rows = (rgba.len() / (w as usize * 4)) as u32;
    if rows == 0 {
        return;
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &rgba[..(rows * w * 4) as usize],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * w),
            rows_per_image: Some(rows),
        },
        wgpu::Extent3d { width: w, height: rows, depth_or_array_layers: 1 },
    );
}

/// 調査用(white.csv): 暗→明を見る行。プログレッシブは1行おきにしか埋まらないので
/// この行と次の行の明るい方を見る
const WHITE_ROW: u32 = 16;

/// `rgba`(`y0` 行目から始まる幅 `w` の行)の `row` 行目・中央が明るいか
fn bright_at(rgba: &[u8], w: usize, y0: usize, row: usize) -> bool {
    [row, row + 1].iter().any(|&r| {
        let o = ((r - y0) * w + w / 2) * 4;
        rgba.get(o..o + 3).is_some_and(|p| p.iter().any(|&c| c > 128))
    })
}

fn render_thread(
    mut gpu: Gpu,
    shared: Arc<receiver::Shared>,
    rx: Receiver<()>,
    size: Arc<AtomicU64>,
    geom: FsGeom,
    hud: Arc<AtomicBool>,
) {
    // Windows は描画スレッドも上げる。帯ごとに present するので、帯が届いてから
    // 起きるまでの遅れがそのまま画面の遅れになる
    #[cfg(windows)]
    crate::receiver::raise_thread_qos();
    let mut texture: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)> = None;
    let mut fail_count = 0u64;
    let mut seen_gen = 0u64;
    let mut last_paint: Option<Instant> = None;
    let mut intervals: Vec<f64> = Vec::new();
    let mut last_log = Instant::now();
    // 幾何に使う vtotal の平滑(geometry.rs。ウィンドウ表示と同じ)
    let mut vt = crate::geometry::VtotalSmoother::default();
    // 画面上の管面の矩形 (x, y, w, h)。比を保って画面いっぱいに置く
    let mut tube_rect: Option<(f32, f32, f32, f32)> = None;
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
    // Metal の HUD を layer に反映した値(⌘I で変わったら設定し直す)
    #[cfg(target_os = "macos")]
    let mut hud_applied: Option<bool> = None;
    #[cfg(not(target_os = "macos"))]
    let _ = &hud;
    // 届いたラインから順に描く(帯)。受信側に分け方を伝える
    shared.slices.store(geom.slices, Ordering::Relaxed);
    // 帯を描いたフレームと、そのフレームで描き終えた行の末尾。
    // ★**フレーム全体は「次のフレームの先頭」が届いて初めて閉じるので、
    //   次のフレームの帯より後に来ることがある。** そのまま全部書くと、描いた
    //   ばかりの上の帯が1つ前のフレームに戻って一瞬ちらつく。帯が先へ進んで
    //   いる行は書かない
    let mut slice_at: Option<(u16, usize)> = None;
    // 最後に書いたフレーム全体。これより古い帯(同じフレームの帯)は捨てる
    let mut last_full: Option<u16> = None;
    // 前の1枚が画面に出るのを待って出しそびれたか、そのとき拾ったフレームの時刻
    let mut owed = false;
    let mut carry_pick: Option<(crate::latency::FrameStamps, Instant)> = None;
    // 調査用: RCX_PRESENT_CSV=パス で、フレームごとの present 呼び出しと画面に出た
    // 時刻を書き出す(ms, 起動からの経過)。present→画面 の分布や、出る時刻が
    // モニタのリフレッシュ格子に乗っているか(=vsync待ち/コンポジタ経由)を見る
    let t_origin = Instant::now();
    let mut present_csv = std::env::var("RCX_PRESENT_CSV").ok()
        .and_then(|p| std::fs::File::create(p).ok())
        .map(std::io::BufWriter::new);
    if let Some(w) = present_csv.as_mut() {
        use std::io::Write;
        let _ = writeln!(w, "id,fresh,acquire_ms,submit_ms,presented_ms,drained_ms,\
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
    // 調査用: present の CSV と並べて、センサーの1回ごとの値を <パス>.photo.csv に
    // 書く(同じ時刻の基準)。ボードは集計しか返さないので、回数が増えたら直近の値を拾う
    let mut photo_csv = std::env::var("RCX_PRESENT_CSV").ok()
        .and_then(|p| std::fs::File::create(format!("{p}.photo.csv")).ok())
        .map(std::io::BufWriter::new);
    if let Some(w) = photo_csv.as_mut() {
        use std::io::Write;
        let _ = writeln!(w, "seen_ms,count,last_us");
    }
    let mut photo_polled = Instant::now();
    let mut photo_count: Option<u32> = None;
    // 調査用: 画面上端の近く(WHITE_ROW 行目の中央)が暗→明に変わった瞬間を
    // <パス>.white.csv に書く。帯/フレーム全体のどちらで書いたか、帯が届いた時刻、
    // それを載せた present とその presentedTime。センサーの値と時刻で突き合わせる
    let mut white_csv = std::env::var("RCX_PRESENT_CSV").ok()
        .and_then(|p| std::fs::File::create(format!("{p}.white.csv")).ok())
        .map(std::io::BufWriter::new);
    if let Some(w) = white_csv.as_mut() {
        use std::io::Write;
        let _ = writeln!(w, "src,rx_ms,written_ms,submit_ms,presented_ms");
    }
    let mut top_bright = false;
    // 明るくなったがまだ present していないもの: (帯/全体, 届いた, 書いた)
    let mut white_pending: Option<(&'static str, Option<Instant>, Instant)> = None;
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut white_inflight: std::collections::HashMap<
        u64, (&'static str, Option<Instant>, Instant)> = Default::default();

    loop {
        // 音量。再生器はデバイスの開き直しで作り直され、そのたびに 1.0 に戻るので毎回掛ける
        if let Some(a) = shared.audio.lock().unwrap().as_ref() {
            crate::audio::AudioPlayer::set_gain(a, geom.gain);
        }
        #[cfg(target_os = "macos")]
        if let Some(p) = presenter.as_ref() {
            let want = hud.load(Ordering::Relaxed);
            if hud_applied != Some(want) {
                p.set_hud(want);
                hud_applied = Some(want);
            }
        }
        // 新フレーム到着まで待つ(タイムアウトでハートビート描画: 統計・黒画面維持)
        // 出しそびれた絵があるときは、前の1枚が画面に出たらすぐ出せるよう短く待つ
        let _ = rx.recv_timeout(Duration::from_millis(if owed { 1 } else { 250 }));
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
                    let (p, aspect) = geom.params(&shared, &mut vt, (w as f32, h as f32));
                    let (sw, sh) = (gpu.config.width.max(1) as f32,
                                    gpu.config.height.max(1) as f32);
                    let r = crate::geometry::fit_centered(sw, sh, aspect);
                    tube_rect = Some(r);
                    gpu.blit.write_uniforms(&gpu.queue, &gpu.uniform, &p, w, h, r.2, r.3);
                }
                let (tex, _, _, _) = texture.as_ref().unwrap();
                let y0 = match slice_at {
                    Some((f, end)) if f != frame.frame_idx => (end as u32).min(h),
                    _ => 0,
                };
                if slice_at.is_some_and(|(f, _)| f == frame.frame_idx) {
                    slice_at = None;
                }
                last_full = Some(frame.frame_idx);
                if y0 < h {
                    write_rows(&gpu.queue, tex, w, y0,
                               &frame.rgba[(y0 * w * 4) as usize..]);
                    if white_csv.is_some() && y0 <= WHITE_ROW {
                        let b = bright_at(&frame.rgba, w as usize, 0, WHITE_ROW as usize);
                        if b && !top_bright {
                            white_pending = Some(("full", None, Instant::now()));
                        }
                        top_bright = b;
                    }
                }
            }
        }
        // 届いている帯を描く。大きさが違う(モードが変わった直後)ものは捨てる
        let slices: Vec<crate::assembler::Slice> =
            shared.slices_q.lock().unwrap().drain(..).collect();
        if let Some((tex, _, tw, th)) = texture.as_ref() {
            for sl in slices {
                if (sl.width as u32, sl.height as u32) != (*tw, *th)
                    || last_full == Some(sl.frame_idx)
                {
                    continue;
                }
                let end = sl.y0 + sl.rgba.len() / (sl.width * 4);
                write_rows(&gpu.queue, tex, *tw, sl.y0 as u32, &sl.rgba);
                if white_csv.is_some() && (sl.y0..end).contains(&(WHITE_ROW as usize)) {
                    let b = bright_at(&sl.rgba, sl.width, sl.y0, WHITE_ROW as usize);
                    if b && !top_bright {
                        white_pending = Some(("slice", Some(sl.t), Instant::now()));
                    }
                    top_bright = b;
                }
                slice_at = Some((sl.frame_idx, match slice_at {
                    Some((f, e)) if f == sl.frame_idx => e.max(end),
                    _ => end,
                }));
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
            if let Some(line) = shared.rtt.lock().unwrap().describe() {
                eprintln!("{line}");
            }
            *shared.latency.lock().unwrap() = snap;
            last_log = Instant::now();
        }

        // 画面に出たものを計測へ回す。出なかった(捨てられた)ものは数えない
        #[cfg(target_os = "macos")]
        if let Some(p) = presenter.as_ref() {
            for (id, presented) in p.drain() {
                if let (Some((src, rx, wr)), Some(w)) =
                    (white_inflight.remove(&id), white_csv.as_mut())
                {
                    use std::io::Write;
                    let ms = |x: Instant| format!("{:.3}", x.saturating_duration_since(t_origin)
                        .as_secs_f64() * 1000.0);
                    let sub = in_flight.get(&id).map(|v| ms(v.2)).unwrap_or_default();
                    let _ = writeln!(w, "{src},{},{},{sub},{}",
                        rx.map(ms).unwrap_or_default(), ms(wr),
                        presented.map(ms).unwrap_or_else(|| "skipped".into()));
                    let _ = w.flush();
                }
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
                    // drained: presented handler の結果をこちらで受け取った時刻。
                    // 帯のときはこれを待って次を出すので、ハンドラの遅れがそのまま効く
                    let _ = writeln!(w, "{id},{},{:.3},{:.3},{},{:.3},{stages}",
                        fresh.is_some() as u8,
                        acq.as_secs_f64() * 1000.0, ms(t_sub),
                        presented.map(|t| format!("{:.3}", ms(t)))
                            .unwrap_or_else(|| "skipped".into()),
                        ms(Instant::now()));
                }
            }
            // 届かないまま溜まり続けないように(画面が隠れている等)
            // 1秒たっても届かないものは捨てる
            in_flight.retain(|_, v| v.2.elapsed() < Duration::from_secs(1));
        }

        if let Some(w) = photo_csv.as_mut() {
            let st = shared.config_state.lock().unwrap();
            let count = st.get(&crate::protocol::CFG_KEY_PLAT_COUNT).map(|c| c & 0xFFFF);
            let last = st.get(&crate::protocol::CFG_KEY_PLAT_LAST).copied();
            drop(st);
            if let (Some(c), Some(l)) = (count, last) {
                if photo_count.is_some_and(|p| p != c) {
                    use std::io::Write;
                    let ms = Instant::now().saturating_duration_since(t_origin)
                        .as_secs_f64() * 1000.0;
                    let _ = writeln!(w, "{ms:.3},{c},{l}");
                    // 全画面は exit で抜けるので BufWriter は捨てられる。毎回書き出す
                    let _ = w.flush();
                }
                photo_count = Some(c);
            }
            if photo_polled.elapsed() >= Duration::from_millis(50) {
                photo_polled = Instant::now();
                shared.config_get_queue.lock().unwrap().extend([
                    crate::protocol::CFG_KEY_PLAT_LAST,
                    crate::protocol::CFG_KEY_PLAT_COUNT,
                ]);
            }
        }

        // ★**帯を描くときは、前に出した1枚が画面に出るまで次を出さない。**
        //   macOS は Immediate でも実際にはリフレッシュごとにしか受け取らず、
        //   出しきれない present は drawable の列(最大3枚)に溜まる。16分割で
        //   毎秒 880 回出したら列が常に詰まり、1枚ごとに3リフレッシュ待って
        //   1フレームずつより遅くなった(平均 65 → 125ms、実測)。出さない間に
        //   届いた帯はテクスチャへ書くだけにしておけば、次のリフレッシュには
        //   その時点でいちばん新しい行まで入った1枚が渡る
        #[cfg(target_os = "macos")]
        if geom.slices > 0 && presenter.is_some()
            && in_flight.values().any(|v| v.2.elapsed() < Duration::from_millis(50))
        {
            if picked.is_some() {
                carry_pick = picked;
            }
            owed = true;
            continue;
        }
        if picked.is_none() {
            picked = carry_pick.take();
        }
        owed = false;

        let Some((_, bind, _, _)) = texture.as_ref() else { continue };

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
            // 管面の矩形(geometry.rs で比を保って画面いっぱいに置いたもの)。
            // シェーダへ渡した dst と同じ大きさにする
            let (x, y, vw, vh) = tube_rect.unwrap_or((0.0, 0.0,
                gpu.config.width as f32, gpu.config.height as f32));
            rp.set_viewport(x, y, vw, vh, 0.0, 1.0);
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
            if let Some(wp) = white_pending.take() {
                white_inflight.insert(id, wp);
            }
        }
        if let Some(f) = wgpu_frame {
            f.present();
            // 画面に出た時刻は測れないので present まで
            if let Some((st, t_pick)) = picked {
                meter.add(&st, t_pick, Some(Instant::now()), None);
            }
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
