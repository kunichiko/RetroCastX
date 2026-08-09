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
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::receiver;

pub fn run(port: u16, subscribe_to: Option<String>, target_mac: Option<[u8; 6]>, decay: f32,
           audio: receiver::AudioOpts, rotate: u32, vscale: f32,
           crop: [u32; 4]) -> ! {
    let shared = Arc::new(receiver::Shared::default());
    let (tx, rx): (Sender<()>, Receiver<()>) = std::sync::mpsc::channel();
    receiver::spawn(
        receiver::Config { port, subscribe_to, target_mac, decay, audio },
        shared.clone(),
        move || {
            let _ = tx.send(());
        },
    )
    .expect("UDP bind failed");

    let event_loop = EventLoop::new().unwrap();
    let mut app = App { shared: Some((shared, rx)), window: None,
                        size: Arc::new(AtomicU64::new(0)), rotate, vscale, crop };
    event_loop.run_app(&mut app).unwrap();
    std::process::exit(0);
}

struct App {
    shared: Option<(Arc<receiver::Shared>, Receiver<()>)>,
    window: Option<Arc<Window>>,
    size: Arc<AtomicU64>, // (w<<32)|h — リサイズをrenderスレッドへ伝える
    rotate: u32,          // 0/1/2/3 = 時計回りに 0/90/180/270 度
    vscale: f32,          // 縦倍率(ドットが正方形でないモードの補正)
    crop: [u32; 4],       // 表示する切り出し範囲[画素]。w/hが0なら全体
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Borderless(None)は不可視のディスプレイ(TV/キャプチャ等)に開くことが
        // あるためプライマリモニタを明示する
        let monitor = event_loop.primary_monitor();
        if let Some(m) = &monitor {
            eprintln!("fullscreen: on monitor {:?}", m.name().unwrap_or_default());
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("RetroCastX (fullscreen)")
                        .with_fullscreen(Some(Fullscreen::Borderless(monitor))),
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
        let (rot, vs, cr) = (self.rotate, self.vscale, self.crop);
        std::thread::spawn(move || render_thread(gpu, shared, rx, size, rot, vs, cr));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(sz) => {
                self.size
                    .store(((sz.width as u64) << 32) | sz.height as u64, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
}

const SHADER: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VOut;
    out.pos = vec4<f32>(p[i], 0.0, 1.0);
    out.uv = vec2<f32>((p[i].x + 1.0) * 0.5, 1.0 - (p[i].y + 1.0) * 0.5);
    return out;
}
// p.xy = 画面の縦横比を吸収する倍率(1以上。余る側に黒帯が出る)
// p.z  = 回転 0/1/2/3 (時計回りに 0/90/180/270 度)
// c    = 切り出し範囲(テクスチャUV) xy=左上, zw=大きさ
struct U { p: vec4<f32>, c: vec4<f32> };
@group(0) @binding(2) var<uniform> u: U;

@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    // in.uv は画面内の位置(左上原点, 0..1)。中心を基準に拡げると、画の外側は
    // 0..1 の外に出るので、そこを黒く塗れば letterbox / pillarbox になる。
    let c = vec2<f32>(0.5, 0.5) + (in.uv - vec2<f32>(0.5, 0.5)) * u.p.xy;
    if (c.x < 0.0 || c.x > 1.0 || c.y < 0.0 || c.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    var q: vec2<f32>;
    let r = u.p.z;
    if (r < 0.5) {
        q = c;                                  // 0度
    } else if (r < 1.5) {
        q = vec2<f32>(c.y, 1.0 - c.x);          // 90度(時計回り)
    } else if (r < 2.5) {
        q = vec2<f32>(1.0 - c.x, 1.0 - c.y);    // 180度
    } else {
        q = vec2<f32>(1.0 - c.y, c.x);          // 270度
    }
    // q は切り出し範囲の中の位置。テクスチャ全体のUVへ写す
    return textureSample(t, s, u.c.xy + q * u.c.zw);
}
"#;

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
        let mode = if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
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
        config.desired_maximum_frame_latency = 3;
        surface.configure(&device, &config);
        eprintln!("fullscreen: present mode {mode:?}, {}x{}, surface {:?}",
                  config.width, config.height, config.format);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(config.format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // レトロ画面はニアレスト(整数拡大でドットがくっきり)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fit"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { surface, device, queue, config, pipeline, bind_layout, sampler, uniform }
    }
}

fn render_thread(
    mut gpu: Gpu,
    shared: Arc<receiver::Shared>,
    rx: Receiver<()>,
    size: Arc<AtomicU64>,
    rotate: u32,
    vscale: f32,
    crop: [u32; 4],
) {
    let mut texture: Option<(wgpu::Texture, wgpu::BindGroup, u32, u32)> = None;
    let mut fail_count = 0u64;
    let mut seen_gen = 0u64;
    let mut last_paint: Option<Instant> = None;
    let mut intervals: Vec<f64> = Vec::new();
    let mut last_log = Instant::now();

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

        if fresh {
            let guard = shared.frame.lock().unwrap();
            if let Some(frame) = guard.as_ref() {
                let (w, h) = (frame.width as u32, frame.height as u32);
                if texture.as_ref().map(|t| (t.2, t.3)) != Some((w, h)) {
                    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("video"),
                        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        // 映像データはsRGB符号化済みなので、テクスチャもsRGBで
                        // 作る。Rgba8Unorm(リニア扱い)にすると、サンプル時に
                        // 復号されないまま出力段でリニア→sRGBに符号化され、
                        // 二重にかかって中間調が持ち上がる(全体が明るく見える)。
                        // 通常モードのeguiは ColorImage をsRGBとして扱うので、
                        // こちらを合わせないと見え方が食い違う。
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let view = tex.create_view(&Default::default());
                    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &gpu.bind_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: gpu.uniform.as_entire_binding(),
                            },
                        ],
                    });
                    texture = Some((tex, bind, w, h));
                }
                // 画面と画の縦横比から letterbox 係数を決める。回転する場合は
                // 画面に占める向きが入れ替わるので、幅と高さを入れ替えて考える。
                {
                    // 切り出し(未設定なら全体)。取り込みバッファにはブランキング
                    // 由来の黒が入るので、回転すると余白として目立つ。
                    let ok = crop[2] > 0 && crop[3] > 0
                        && crop[0] + crop[2] <= w && crop[1] + crop[3] <= h;
                    let (cw, chh) = if ok { (crop[2], crop[3]) } else { (w, h) };
                    let (u0, v0) = if ok {
                        (crop[0] as f32 / w as f32, crop[1] as f32 / h as f32)
                    } else {
                        (0.0, 0.0)
                    };
                    let (du, dv) = (cw as f32 / w as f32, chh as f32 / h as f32);
                    let (iw, ih) = (cw as f32, chh as f32 * vscale.max(0.01));
                    let (fw, fh) = if rotate % 2 == 1 { (ih, iw) } else { (iw, ih) };
                    let (sw, sh) = (gpu.config.width.max(1) as f32,
                                    gpu.config.height.max(1) as f32);
                    let sc = (sw / fw).min(sh / fh);
                    let (kx, ky) = (sw / (fw * sc), sh / (fh * sc));
                    let mut buf = [0u8; 32];
                    for (i, v) in [kx, ky, rotate as f32, 0.0, u0, v0, du, dv]
                        .iter().enumerate()
                    {
                        buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                    }
                    gpu.queue.write_buffer(&gpu.uniform, 0, &buf);
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
            last_log = Instant::now();
        }

        let Some((_, bind, tw, th)) = texture.as_ref() else { continue };

        let frame = match gpu.surface.get_current_texture() {
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
        let view = frame.texture.create_view(&Default::default());
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
            rp.set_pipeline(&gpu.pipeline);
            rp.set_bind_group(0, bind, &[]);
            rp.draw(0..3, 0..1);
        }
        gpu.queue.submit([enc.finish()]);
        frame.present();

        if fresh {
            let now = Instant::now();
            if let Some(prev) = last_paint {
                intervals.push((now - prev).as_secs_f64() * 1000.0);
            }
            last_paint = Some(now);
        }
    }
}
