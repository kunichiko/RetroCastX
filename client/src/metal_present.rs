//! フルスクリーンの present を自前で行い、**実際に画面に出た時刻**を取る(macOS)。
//!
//! wgpu の `SurfaceTexture::present()` は MTLDrawable を隠していて、
//! `presentedTime` を読む口が無い。そこで present の部分だけを wgpu-hal と
//! 同じ手順で自前に持つ:
//!
//! 1. CAMetalLayer から `nextDrawable` を取る(wgpu の get_current_texture 相当)
//! 2. その MTLTexture を wgpu のテクスチャとして包み、いつもどおり wgpu で描く
//! 3. 同じコマンドキューで `presentDrawable` を commit する(wgpu-hal の present と同じ。
//!    同じキューへの commit 順で、描画が済んでから出る)
//! 4. `addPresentedHandler` で presentedTime(= 画面に出た時刻)を受け取る
//!
//! ★**presentedTime は CADisplayLink 等と違い、本当に出た時刻。** ProMotion の
//!   検証(client/README.md)で、CADisplayLink や CGDisplayModeGetRefreshRate が
//!   実態と違う値を返し続けたのに対し、presentedTime だけが正しかった。
//!
//! レイヤーの設定(画素形式・大きさ・displaySyncEnabled)は従来どおり
//! `surface.configure` に任せる。ここで触るのは drawable の取得と present だけ。

use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use eframe::wgpu;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::Message;
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDrawable, MTLTexture, MTLTextureType};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

type Hal = wgpu::hal::api::Metal;

pub struct MetalPresenter {
    layer: Retained<CAMetalLayer>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// presented handler から届いた (id, presentedTime[秒, ホスト時刻])。0.0 は出なかった
    done: Arc<Mutex<Vec<(u64, f64)>>>,
    next_id: u64,
}

// CAMetalLayer / MTLCommandQueue はスレッドをまたいで使ってよい(Metal の規約)。
// 使うのは present スレッドだけ。
unsafe impl Send for MetalPresenter {}

pub struct Acquired {
    pub texture: wgpu::Texture,
    drawable: Retained<ProtocolObject<dyn CAMetalDrawable>>,
}

impl MetalPresenter {
    /// Metal バックエンドでなければ None(従来の present を使う)。
    pub fn new(surface: &wgpu::Surface, queue: &wgpu::Queue) -> Option<Self> {
        let layer = unsafe { surface.as_hal::<Hal>() }?
            .render_layer()
            .lock()
            .clone();
        let queue = unsafe { queue.as_hal::<Hal>() }?.as_raw().retain();
        Some(Self { layer, queue, done: Default::default(), next_id: 0 })
    }

    /// 次の drawable を取り、wgpu のテクスチャとして包む。
    pub fn acquire(&self, device: &wgpu::Device, format: wgpu::TextureFormat)
        -> Option<Acquired>
    {
        let drawable = objc2::rc::autoreleasepool(|_| self.layer.nextDrawable())?;
        let raw = drawable.texture();
        let (w, h) = (raw.width() as u32, raw.height() as u32);
        let hal_tex = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width: w, height: h, depth: 1 },
            )
        };
        let texture = unsafe {
            device.create_texture_from_hal::<Hal>(hal_tex, &wgpu::TextureDescriptor {
                label: Some("drawable"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        Some(Acquired { texture, drawable })
    }

    /// 描画を submit した後に呼ぶ。返す id で `drain` の結果と突き合わせる。
    pub fn present(&mut self, a: Acquired) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let done = self.done.clone();
        let handler = RcBlock::new(move |d: NonNull<ProtocolObject<dyn MTLDrawable>>| {
            let t = unsafe { d.as_ref() }.presentedTime();
            if let Ok(mut v) = done.lock() {
                v.push((id, t));
            }
        });
        let drawable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(&*a.drawable);
        objc2::rc::autoreleasepool(|_| unsafe {
            drawable.addPresentedHandler(RcBlock::as_ptr(&handler));
            if let Some(cb) = self.queue.commandBuffer() {
                cb.presentDrawable(drawable);
                cb.commit();
            }
        });
        drop(a.texture);
        id
    }

    /// 画面に出た(または出なかった)drawable を取り出す。出なかったものは None。
    pub fn drain(&self) -> Vec<(u64, Option<Instant>)> {
        let v = std::mem::take(&mut *self.done.lock().unwrap());
        v.into_iter().map(|(id, t)| (id, host_to_instant(t))).collect()
    }
}

/// presentedTime(CACurrentMediaTime と同じ基準 = mach_absolute_time の秒)を
/// Instant に直す。Rust の Instant も macOS では同じ時計(CLOCK_UPTIME_RAW)だが、
/// 中身を直接は作れないので「今」を両方で取って差で換算する。
fn host_to_instant(t: f64) -> Option<Instant> {
    if t <= 0.0 {
        return None; // 出なかった(差し替えられて捨てられた)
    }
    let now_i = Instant::now();
    let now_h = unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) } as f64 * 1e-9;
    let dt = now_h - t;
    if dt >= 0.0 {
        now_i.checked_sub(Duration::from_secs_f64(dt))
    } else {
        Some(now_i + Duration::from_secs_f64(-dt))
    }
}

unsafe extern "C" {
    /// macOS の libSystem。libc クレートには無いので自前で宣言する
    fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
}
