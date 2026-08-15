//! 表示の共通描画(GPU)。
//!
//! 通常モードとフルスクリーンで見え方が食い違わないよう、切り出し・管面への
//! 引き伸ばし・回転・補間を1つのシェーダにまとめる。両モードともこれを使う。
//!
//! 考え方は実際のCRTと同じ:
//!   1. 受け取った絵から有効映像を切り出す
//!   2. それを「管面」(縦横比だけが決まった長方形)いっぱいに引き伸ばす
//!      → 512x256 でも 768x512 でも同じ管面に映る。CRTの偏向がやっていること
//!   3. 管面をウィンドウ/画面に収める(余った側は黒)
//!
//! 補間は sharp-bilinear を既定にしている。非整数倍で拡大すると、ニアレストでは
//! ドットの幅が不揃いになり(512→3840なら7.5倍で7pxと8pxが混ざる)、バイリニア
//! では全体がぼやける。sharp-bilinear はドットの内部をそのまま保ち、境目だけを
//! 出力1画素分で繋ぐので、どちらの欠点も出ない。

use eframe::wgpu;

/// 補間方式
pub const FILTER_NEAREST: u32 = 0;
pub const FILTER_LINEAR: u32 = 1;
pub const FILTER_SHARP: u32 = 2;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// 回転 0/1/2/3 = 時計回りに 0/90/180/270 度
    pub rotate: u32,
    /// 管面の縦横比 (幅/高さ)。0 なら表示する時間窓の比をそのまま使う
    pub tube: f32,
    pub filter: u32,
    /// 1ライン当たりのサンプル数(= pll_divide)。0 なら幾何計算をやめて全体表示
    pub htotal: u32,
    /// バッファ先頭サンプルが、HSYNCから何サンプル後か
    pub hs_offset: u32,
    /// 1フレーム当たりのライン数
    /// 1VSYNC周期に入る半ラインスロット数。**整数ではなく実数で持つ。**
    ///
    /// ★インターレースは 262.5 ライン/フィールドなので、ボードが測る vtotal は
    ///   262 と 263 を**交互に**返すのが正常。その瞬間値を幾何に使うと縦の
    ///   スケールがフレームごとに 0.38% 変わり、絵が上下に震える(実機で
    ///   「横線の縦位置がフレームごとに動いて二重線に見える」形で出た)。
    ///   呼び出し側で平滑した値を入れること。
    pub vtotal: f32,
    /// バッファ先頭行が、VSYNCから何行後か
    pub vbp: u32,
    /// 管面が映す時間窓。ラインとフレームに対する割合 [h0, h1, v0, v1]
    /// time_based=false のときだけ使う(旧方式)。
    pub window: [f32; 4],
    /// 水平同期周波数[Hz]。0 なら時間ベースの計算をやめる
    pub fh_hz: u32,
    /// 有効映像のサンプル数/ライン数。管面の中心を決めるのに使う
    pub hactive: u32,
    pub vactive: u32,
    /// 管面プロファイル(モニタの帯域ごとのプリセット)。
    ///
    /// 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど掃引する」
    /// ように周波数ごとに速度が切り替わる。だから管面の横幅は 1/fH そのもので、
    /// 掃引時間は自由パラメータではない。縦も同様に 1VSYNC周期が管面の高さになる。
    ///
    /// プロファイルが持つのは「その周期のうち実際に管面へ出る割合」と「位置」だけ。
    /// 1.0 なら周期全体(帰線期間まで)が見える。実際のCRTは帰線の間ビームが戻るので
    /// 少し小さい値(0.85〜0.95)が現実に近いが、既定は何も切れない 1.0 にしてある。
    pub h_size: f32,
    pub h_pos: f32,
    pub v_size: f32,
    pub v_pos: f32,
    /// 実測した有効映像の外接矩形[サンプル/ライン]。管面の中心と、縦の連動に使う。
    /// MODEの hactive は送出フレームの幅(常に1024)で有効映像の幅ではないため、
    /// 実測値が要る。0 のときは中心を0.5とみなす
    pub act_x: u32,
    pub act_y: u32,
    pub act_w: u32,
    pub act_h: u32,
    /// 時間ベースで管面を決める(false なら window の割合をそのまま使う)
    pub time_based: bool,
}

impl Params {
    /// 管面が映す時間窓を、ラインとフレームに対する割合で返す。
    ///
    /// 3モードディスプレイの偏向は「1HSYNC周期でブラウン管の左右をちょうど掃引する」
    /// ように周波数ごとに速度が切り替わる。よって管面の横幅は 1/fH そのもので、
    /// 縦は 1VSYNC周期。ここで返す割合は「その周期のどこからどこまでが管面に
    /// 出るか」で、モニタ固有かつモードに依らない。
    ///
    /// pll_divide を変えても絵が動かないのが要点。サンプル k の位置は
    /// (hs_offset + k)/htotal で、htotal = pll_divide、k も pll_divide に比例して
    /// 増えるので時間としては同じ場所を指す。ただし hs_offset はサンプル単位
    /// なので、pll_divide を変えたら同じ比で動かす必要がある(Viewer側で行う)。
    pub fn effective_window(&self) -> [f32; 4] {
        if !self.time_based || self.htotal == 0 || self.vtotal <= 0.0 {
            return self.window;
        }
        // 管面は「1周期のうち h_size の割合」を映す。位置は周期の中心からのずれ。
        // pll_divide を変えても htotal と k が同じ比で動くので絵は動かない
        // (hs_offset もサンプル単位なので、pll_divide に追従させる必要がある。
        //  それはViewer側で送るときに行う)。
        let hs = self.h_size.clamp(0.05, 4.0);
        let vs = self.v_size.clamp(0.05, 4.0);
        // 位置の値は「絵をどちらへ動かすか」を表す。管面の中心を動かすと絵は逆へ
        // 動くので符号を反転する(スライダーを右へ→絵が右へ、下へ→絵が下へ)。
        let ch = 0.5 - self.h_pos;
        let cv = 0.5 - self.v_pos;
        [ch - hs * 0.5, ch + hs * 0.5, cv - vs * 0.5, cv + vs * 0.5]
    }
}

/// 測定した vtotal(スロット数)を幾何用に平滑する。
///
/// ★**瞬間値をそのまま使ってはいけない。** インターレースは 262.5 ライン/
///   フィールドなので、ボードが測る vtotal は 262 と 263 を**交互に**返すのが
///   正常。これを幾何に入れると縦のスケールがフレームごとに 0.38% 変わり、
///   絵が上下に震える。実機では「横線の縦位置がフレームごとに動いて二重線に
///   見える」形で出た。**片フィールド表示にしても残った**ので、織り込みでは
///   なく幾何側だと切り分けられた。
///
/// `reset` はモードが変わったとき。追従を待つと一瞬絵が伸びるので即座に飛ばす。
/// 大きく外れた値(8スロット超)も同様に飛ばす。
pub fn smooth_vtotal(cur: f32, raw: f32, reset: bool) -> f32 {
    if !(raw > 0.0) {
        return cur;
    }
    if reset || (raw - cur).abs() > 8.0 {
        raw
    } else {
        cur + (raw - cur) * 0.1
    }
}

impl Default for Params {
    fn default() -> Self {
        Self {
            rotate: 0, tube: 0.0, filter: FILTER_SHARP,
            htotal: 0, hs_offset: 0, vtotal: 0.0, vbp: 0,
            window: [0.22, 0.94, 0.07, 0.98],
            fh_hz: 0, hactive: 0, vactive: 0,
            h_size: 1.0, h_pos: 0.0, v_size: 1.0, v_pos: 0.0,
            act_x: 0, act_y: 0, act_w: 0, act_h: 0,
            time_based: true,
        }
    }
}

/// シェーダへ渡す値。16バイト境界に合わせて4つのvec4にまとめる。
///
/// 幾何は時間で決まる。バッファのサンプルkはラインの (hs_offset + k)/htotal の
/// 位置にあり、これは映像の内容に一切依存しない。管面はラインの中の時間窓を
/// 映すので、その窓とこの対応から、どのサンプルが画面のどこに来るかが決まる。
fn uniforms(p: &Params, tex_w: u32, tex_h: u32, dst_w: f32, dst_h: f32) -> [u8; 64] {
    let (tw, th) = (tex_w.max(1) as f32, tex_h.max(1) as f32);
    // htotal/vtotal が無い(MODE未受信など)ときはバッファ全体をそのまま出す
    let geom = p.htotal > 0 && p.vtotal > 0.0;
    let (ht, vt) = (p.htotal.max(1) as f32, p.vtotal.max(1.0));
    let w = p.effective_window();
    let (h0, h1, v0, v1) = if geom {
        (w[0], w[1], w[2], w[3])
    } else {
        // 幾何が使えないときは、バッファ全体がちょうど窓になるようにする
        (0.0, 1.0, 0.0, 1.0)
    };
    let (hoff, voff) = if geom { (p.hs_offset as f32, p.vbp as f32) } else { (0.0, 0.0) };
    let (span_x, span_y) = if geom {
        ((h1 - h0) * ht, (v1 - v0) * vt)
    } else {
        (tw, th)
    };
    // 管面の縦横比。管面は物理的な形なので、時間ベースのときは指定が無ければ 4:3
    // にする(時間窓の比を使うとモードごとに管面の形が変わってしまう)。
    let aspect = if p.tube > 0.0 {
        p.tube
    } else if p.time_based && geom {
        4.0 / 3.0
    } else {
        span_x / span_y.max(1.0)
    };
    let (fw, fh) = if p.rotate % 2 == 1 { (1.0, aspect) } else { (aspect, 1.0) };
    let sc = (dst_w / fw).min(dst_h / fh);
    let (draw_w, draw_h) = (fw * sc, fh * sc);
    let (kx, ky) = (dst_w / draw_w.max(1e-6), dst_h / draw_h.max(1e-6));
    // sharp-bilinear 用: 1サンプルが出力何画素に拡大されるか
    let (su, sv) = if p.rotate % 2 == 1 {
        (draw_h / span_x.max(1e-6), draw_w / span_y.max(1e-6))
    } else {
        (draw_w / span_x.max(1e-6), draw_h / span_y.max(1e-6))
    };
    let vals: [f32; 16] = [
        kx, ky, p.rotate as f32, p.filter as f32,
        h0 * ht - hoff, (h1 - h0) * ht, v0 * vt - voff, (v1 - v0) * vt,
        su.max(1e-6), sv.max(1e-6), tw, th,
        0.0, 0.0, 0.0, 0.0,
    ];
    let mut buf = [0u8; 64];
    for (i, v) in vals.iter().enumerate() {
        buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
    }
    buf
}

pub const SHADER: &str = r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// a: kx, ky, rotate, filter
// b: 管面の左端に来るサンプル番号 x, その幅[サンプル] y, 同 上端の行 z, 高さ w
// c: 1サンプルあたりの出力画素数 u, v / テクスチャ寸法 w, h
struct U { a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32> };
@group(0) @binding(2) var<uniform> u: U;

struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VOut;
    out.pos = vec4<f32>(p[i], 0.0, 1.0);
    out.uv = vec2<f32>((p[i].x + 1.0) * 0.5, 1.0 - (p[i].y + 1.0) * 0.5);
    return out;
}

@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    // 描画先の中で、管面が占める部分だけを使う。外側は黒(letterbox/pillarbox)
    let e = vec2<f32>(0.5, 0.5) + (in.uv - vec2<f32>(0.5, 0.5)) * u.a.xy;
    if (e.x < 0.0 || e.x > 1.0 || e.y < 0.0 || e.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // 回転(管面座標 → 切り出した絵の座標)
    var q: vec2<f32>;
    let r = u.a.z;
    if (r < 0.5) {
        q = e;
    } else if (r < 1.5) {
        q = vec2<f32>(e.y, 1.0 - e.x);
    } else if (r < 2.5) {
        q = vec2<f32>(1.0 - e.x, 1.0 - e.y);
    } else {
        q = vec2<f32>(1.0 - e.y, e.x);
    }

    let tex = u.c.zw;
    // 管面の座標 → バッファ内のサンプル座標(時間で決まる。内容に依存しない)
    let texel0 = vec2<f32>(u.b.x + q.x * u.b.y, u.b.z + q.y * u.b.w);
    // 取り込み窓の外は情報が無いので黒
    if (texel0.x < 0.0 || texel0.x > tex.x || texel0.y < 0.0 || texel0.y > tex.y) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let uv0 = texel0 / tex;
    let mode = u.a.w;

    if (mode < 0.5) {
        // ニアレスト: テクセル中心へ吸着させてから線形サンプラで拾う
        let c = (floor(uv0 * tex) + vec2<f32>(0.5, 0.5)) / tex;
        return textureSample(t, samp, c);
    }
    if (mode < 1.5) {
        return textureSample(t, samp, uv0);
    }
    // sharp-bilinear: テクセルの内部はそのまま、境目だけ出力1画素分で繋ぐ。
    // 非整数倍でもドット幅が不揃いにならず、かつ全体がぼやけない。
    let texel = uv0 * tex;
    let base = floor(texel);
    let frac = texel - base;
    let scale = max(u.c.xy, vec2<f32>(1.0, 1.0));
    let range = vec2<f32>(0.5, 0.5) - vec2<f32>(0.5, 0.5) / scale;
    let dist = frac - vec2<f32>(0.5, 0.5);
    let f = (dist - clamp(dist, -range, range)) * scale + vec2<f32>(0.5, 0.5);
    return textureSample(t, samp, (base + f) / tex);
}
"#;

/// パイプラインとサンプラ。テクスチャは呼び出し側が持つ。
pub struct Pipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
}

impl Pipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("retrocastx-blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("retrocastx-blit"),
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
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("retrocastx-blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // 補間はシェーダ側で決めるので、サンプラは線形で固定する
        // (ニアレストもテクセル中心へ吸着させて実現している)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self { pipeline, layout, sampler }
    }

    pub fn write_uniforms(
        &self, queue: &wgpu::Queue, buf: &wgpu::Buffer,
        p: &Params, tex_w: u32, tex_h: u32, dst_w: f32, dst_h: f32,
    ) {
        queue.write_buffer(buf, 0, &uniforms(p, tex_w, tex_h, dst_w, dst_h));
    }

    pub fn make_uniform_buffer(&self, device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retrocastx-blit-u"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn bind_group(
        &self, device: &wgpu::Device, view: &wgpu::TextureView, uniform: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        })
    }
}

// ---- egui(通常モード)から同じシェーダを使うための paint callback ----

/// egui の描画中に GPU リソースへ触れるための入れ物。
/// eframe の RenderState に1つだけ登録し、フレームごとに更新して使う。
pub struct EguiBlit {
    pub pipeline: Pipeline,
    pub uniform: wgpu::Buffer,
    /// (テクスチャ, ビュー, バインド, 幅, 高さ)
    pub tex: Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup, u32, u32)>,
}

impl EguiBlit {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let pipeline = Pipeline::new(device, format);
        let uniform = pipeline.make_uniform_buffer(device);
        Self { pipeline, uniform, tex: None }
    }

    /// 受信した RGBA をテクスチャへ載せる(寸法が変わったら作り直す)
    pub fn upload(
        &mut self, device: &wgpu::Device, queue: &wgpu::Queue,
        rgba: &[u8], w: u32, h: u32,
    ) {
        let need = self.tex.as_ref().map(|t| (t.3, t.4)) != Some((w, h));
        if need {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // 映像データはsRGB符号化済み。リニア扱いにすると出力段で二重に
                // 符号化され中間調が持ち上がる(フルスクリーンで実際に起きた)
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = self.pipeline.bind_group(device, &view, &self.uniform);
            self.tex = Some((tex, view, bind, w, h));
        }
        let (tex, ..) = self.tex.as_ref().unwrap();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }
}

/// egui の Shape として積む描画命令。矩形は egui 側で決め、その中を
/// フルスクリーンと同じシェーダで塗る。
pub struct Callback {
    pub params: Params,
    /// 描画先の画素数(論理座標ではなく実画素。Retinaでは2倍になる)
    pub dst: (f32, f32),
}

impl eframe::egui_wgpu::CallbackTrait for Callback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &eframe::egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        res: &mut eframe::egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(b) = res.get::<EguiBlit>() {
            if let Some((_, _, _, w, h)) = b.tex.as_ref() {
                b.pipeline.write_uniforms(
                    queue, &b.uniform, &self.params, *w, *h, self.dst.0, self.dst.1);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        rp: &mut wgpu::RenderPass<'static>,
        res: &eframe::egui_wgpu::CallbackResources,
    ) {
        if let Some(b) = res.get::<EguiBlit>() {
            if let Some((_, _, bind, _, _)) = b.tex.as_ref() {
                rp.set_pipeline(&b.pipeline.pipeline);
                rp.set_bind_group(0, bind, &[]);
                rp.draw(0..3, 0..1);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// **交互する vtotal で幾何が動かないこと。** これが崩れると絵が上下に震える。
    #[test]
    fn smooth_vtotal_averages_the_alternation() {
        // インターレースの 262.5 ライン/フィールド → スロットでは 524/526 が交互
        let mut v = 524.0f32;
        for i in 0..200 {
            v = smooth_vtotal(v, if i % 2 == 0 { 524.0 } else { 526.0 }, false);
        }
        assert!((v - 525.0).abs() < 0.2, "525付近へ収束していない: {v:.2}");

        // 収束後、1サンプルで動く量が「見える」量を大きく下回ること。
        // 瞬間値をそのまま使うと 2スロット(=0.38%)動いていた。
        let a = smooth_vtotal(v, 524.0, false);
        let b = smooth_vtotal(v, 526.0, false);
        assert!((a - b).abs() < 0.4,
                "平滑後もフレーム間で {:.2} スロット動く(生の値なら2.0)", (a - b).abs());
    }

    /// モード切替では待たずに飛ぶこと(追従を待つと一瞬絵が伸びる)
    #[test]
    fn smooth_vtotal_jumps_on_mode_change() {
        assert_eq!(smooth_vtotal(525.0, 1050.0, true), 1050.0);
        // reset が来なくても、大きく外れたら飛ぶ
        assert_eq!(smooth_vtotal(525.0, 626.0, false), 626.0);
        // 0以下は無視(MODE未受信)
        assert_eq!(smooth_vtotal(525.0, 0.0, false), 525.0);
    }
}
