//! present経路の検証プローブ(VRR対応の前段調査)。
//!
//! winitの再描画スケジューリング(macOSでは60Hzのディスプレイリンクに量子化される)を
//! 迂回し、専用スレッドからwgpuサーフェスへ直接presentした場合に、任意のレート
//! (既定90Hz)でpresentできるかを測る。eframe本体の測定(--no-vsync でも60Hz張り付き)
//! との比較用。
//!
//! Usage: cargo run --release --example pace_probe -- [target_hz] [--fifo]
//!   出力: 毎秒 "probe: present XX.XXms σX.XX → XX.XXHz (mode)" をstderrへ

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::wgpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

struct Probe {
    target_hz: f64,
    fifo: bool,
    latency: u32,
    fullscreen: bool,
    window: Option<Arc<Window>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("pace_probe")
            .with_inner_size(winit::dpi::LogicalSize::new(320.0, 240.0));
        if self.fullscreen {
            attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        self.window = Some(window.clone());
        let target_hz = self.target_hz;
        let fifo = self.fifo;
        let latency = self.latency;
        // サーフェス作成はmacOSではメインスレッド限定。初期化一式をここで行い、
        // 描画・presentだけをwinitのイベントループの外(専用スレッド)へ移す
        let gpu = setup_gpu(window, fifo, latency);
        std::thread::spawn(move || render_thread(gpu, target_hz));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    mode: wgpu::PresentMode,
}

fn setup_gpu(window: Arc<Window>, fifo: bool, latency: u32) -> Gpu {
    let instance = wgpu::Instance::default();
    let surface = instance.create_surface(window.clone()).unwrap();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .unwrap();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

    let caps = surface.get_capabilities(&adapter);
    eprintln!("probe: supported present modes: {:?}", caps.present_modes);
    let mode = if fifo {
        wgpu::PresentMode::Fifo
    } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
        wgpu::PresentMode::Immediate
    } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        eprintln!("probe: no unsynced mode available; falling back to Fifo");
        wgpu::PresentMode::Fifo
    };
    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .unwrap();
    config.present_mode = mode;
    config.desired_maximum_frame_latency = latency;
    surface.configure(&device, &config);
    Gpu { surface, device, queue, config, mode }
}

fn render_thread(gpu: Gpu, target_hz: f64) {
    let Gpu { surface, device, queue, config, mode } = gpu;
    eprintln!("probe: using {mode:?}, target {target_hz}Hz");

    let period = Duration::from_secs_f64(1.0 / target_hz);
    let mut next = Instant::now();
    let mut last_present: Option<Instant> = None;
    let mut intervals: Vec<f64> = Vec::new();
    let mut last_log = Instant::now();
    let mut hue = 0.0f64;

    loop {
        // ターゲットレートで刻む(ソース55.46Hz/90Hz相当のフレーム到着を模擬)
        next += period;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        } else {
            next = Instant::now();
        }

        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                surface.configure(&device, &config);
                continue;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = device.create_command_encoder(&Default::default());
        {
            hue = (hue + 0.02) % 1.0;
            let _rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: hue,
                            g: 0.3,
                            b: 1.0 - hue,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        queue.submit([enc.finish()]);
        frame.present();

        let now = Instant::now();
        if let Some(prev) = last_present {
            intervals.push((now - prev).as_secs_f64() * 1000.0);
        }
        last_present = Some(now);
        if last_log.elapsed().as_secs_f64() >= 1.0 && !intervals.is_empty() {
            let n = intervals.len() as f64;
            let mean = intervals.iter().sum::<f64>() / n;
            let var = intervals.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
            eprintln!(
                "probe: present {:.2}ms σ{:.2} → {:.2}Hz ({mode:?})",
                mean,
                var.sqrt(),
                1000.0 / mean
            );
            intervals.clear();
            last_log = now;
        }
    }
}

fn main() {
    let mut target_hz = 90.0f64;
    let mut fifo = false;
    let mut latency = 1u32;
    let mut fullscreen = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--fifo" => fifo = true,
            "--fullscreen" => fullscreen = true,
            "--latency" => latency = args.next().unwrap().parse().unwrap(),
            v => target_hz = v.parse().expect("target_hz must be a number"),
        }
    }
    let event_loop = EventLoop::new().unwrap();
    let mut probe = Probe { target_hz, fifo, latency, fullscreen, window: None };
    event_loop.run_app(&mut probe).unwrap();
}
