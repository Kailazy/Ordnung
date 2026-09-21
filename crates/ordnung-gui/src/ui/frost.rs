//! Live frosted backdrops for glass surfaces, made on the GPU every frame.
//!
//! egui paints every layer straight to one surface, so a see-through window
//! can only tint what's under it: at any alpha that lets the app show, it
//! shows in outline. What a frosted surface wants is the app *blurred*, and
//! there is no backdrop blur to ask the GPU for. So the frost is made the
//! long way round, once a frame, for every glass surface that is up:
//!
//! 1. the shapes egui painted this frame, up to the surface's own frost,
//!    are drawn a second time through egui's own wgpu renderer into a small
//!    offscreen texture (a quarter or so of the screen's pixels);
//! 2. that is halved twice and gaussian-blurred, all on the GPU, into the
//!    surface's own texture, which is registered with egui;
//! 3. the surface paints that texture under its tint, mapped so it sees the
//!    part of the screen it sits over.
//!
//! Nothing is read back from the GPU, so this costs the frame a fraction
//! of a millisecond of GPU time and one extra tessellation of the shapes
//! near the surface, and the frost follows whatever moves under it: a
//! scroll, a hover, a playing waveform. It replaces an earlier design that
//! screenshotted the frame once when a surface opened, which froze whatever
//! was under the surface for as long as it stayed up.
//!
//! Surfaces are rendered in paint order, cumulatively: the second surface's
//! backdrop is the first's plus everything painted between them, so a menu
//! over a window frosts the window too, frost and all.

use eframe::egui;
use eframe::egui_wgpu::{RenderState, ScreenDescriptor};
use eframe::wgpu;

/// Long side of the offscreen render, in pixels. Big enough that hairlines
/// and small text still register before the blur eats them; small enough
/// that drawing the whole screen into it is nothing.
const OFF_LONG: u32 = 1280;

/// Long side of the blurred texture, in texels. The two halvings from
/// [`OFF_LONG`] land here; the blur runs at this size and bilinear
/// magnification back up is itself most of the softness.
const BLUR_LONG: u32 = 320;

/// How far, in points, a shape can be from a surface and still show in its
/// frost: three sigmas of the blur at the screen sizes the app runs at.
pub const REACH: f32 = 64.0;

const SHADER: &str = r#"
struct Params { step: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

// One triangle over the whole target.
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var out: VOut;
    let x = f32((i & 1u) * 4u) - 1.0;
    let y = f32((i & 2u) * 2u) - 1.0;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

// Plain resample. Sampled at the centre of a texel of a target half the
// size, bilinear filtering makes this an exact 2x2 average.
@fragment fn fs_blit(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(tex, smp, in.uv);
}

// 13-tap gaussian, sigma 2.4 texels, along `params.step`. Run once each
// way, twice over, for a sigma of about 3.4.
fn tap(uv: vec2<f32>, k: f32) -> vec4<f32> {
    let o = params.step * k;
    return textureSample(tex, smp, uv + o) + textureSample(tex, smp, uv - o);
}

@fragment fn fs_blur(in: VOut) -> @location(0) vec4<f32> {
    var acc = textureSample(tex, smp, in.uv) * 0.1673;
    acc += tap(in.uv, 1.0) * 0.1534;
    acc += tap(in.uv, 2.0) * 0.1182;
    acc += tap(in.uv, 3.0) * 0.0766;
    acc += tap(in.uv, 4.0) * 0.0417;
    acc += tap(in.uv, 5.0) * 0.0191;
    acc += tap(in.uv, 6.0) * 0.0073;
    return acc;
}
"#;

/// Format of every texture after the offscreen render. sRGB so egui, which
/// samples it as a linear-light texture, shows the colours as they were.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A texture that is drawn into and sampled from.
struct Target {
    #[allow(dead_code)]
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
}

impl Target {
    fn new(device: &wgpu::Device, label: &str, size: [u32; 2], format: wgpu::TextureFormat) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0].max(1),
                height: size[1].max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        Self { tex, view, size }
    }
}

/// The textures every surface's frost is made through, sized to the screen.
struct Shared {
    /// Screen size in pixels these were made for.
    screen_px: [u32; 2],
    /// The offscreen render, in the surface's own format (egui's pipeline
    /// only draws to that).
    off: Target,
    half: Target,
    ping: Target,
    pong: Target,
    /// Samples `off`, `half`, `ping` (horizontal blur), `pong` (vertical).
    blit_off: wgpu::BindGroup,
    blit_half: wgpu::BindGroup,
    blur_h: wgpu::BindGroup,
    blur_v: wgpu::BindGroup,
    #[allow(dead_code)]
    uniforms: [wgpu::Buffer; 3],
}

/// One surface's blurred backdrop: a texture egui can paint with.
pub struct Backdrop {
    target: Target,
    id: egui::TextureId,
}

impl Backdrop {
    /// The texture, for a shape's `fill_texture_id`.
    pub fn id(&self) -> egui::TextureId {
        self.id
    }
}

/// The GPU side: pipelines, the shared textures, and the renderer they
/// draw egui's shapes through.
pub struct Engine {
    state: RenderState,
    blit: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    shared: Option<Shared>,
}

/// A surface whose backdrop is to be made: everything painted before
/// `position` in the frame's shape list goes into it.
pub struct Cut<'a> {
    pub position: usize,
    pub backdrop: &'a mut Backdrop,
}

impl Engine {
    pub fn new(state: RenderState) -> Self {
        let device = &state.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frost"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frost"),
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
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frost"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: entry,
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview: None,
                cache: None,
            })
        };
        let blit = pipeline("fs_blit");
        let blur = pipeline("fs_blur");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frost"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            state,
            blit,
            blur,
            layout,
            sampler,
            shared: None,
        }
    }

    /// Screen size in pixels, and the size of a frost for it.
    fn sizes(ctx: &egui::Context) -> ([u32; 2], [u32; 2]) {
        let ppp = ctx.pixels_per_point();
        let s = ctx.screen_rect().size() * ppp;
        let px = [(s.x.round() as u32).max(1), (s.y.round() as u32).max(1)];
        (px, scaled(px, BLUR_LONG))
    }

    /// A backdrop texture for a new surface, registered with egui.
    pub fn backdrop(&mut self, ctx: &egui::Context) -> Backdrop {
        let (_, size) = Self::sizes(ctx);
        let target = Target::new(&self.state.device, "frost_surface", size, FORMAT);
        let id = self.state.renderer.write().register_native_texture(
            &self.state.device,
            &target.view,
            wgpu::FilterMode::Linear,
        );
        Backdrop { target, id }
    }

    /// Let a surface's backdrop go. Call at the top of a frame, before
    /// anything that could still paint with it is encoded.
    pub fn free(&mut self, backdrop: Backdrop) {
        self.state.renderer.write().free_texture(&backdrop.id);
    }

    fn bind(&self, target: &Target, uniform: &wgpu::Buffer) -> wgpu::BindGroup {
        self.state.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frost"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&target.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    fn uniform(&self, step: [f32; 2]) -> wgpu::Buffer {
        let buf = self.state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frost_params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&step[0].to_ne_bytes());
        bytes[4..8].copy_from_slice(&step[1].to_ne_bytes());
        self.state.queue.write_buffer(&buf, 0, &bytes);
        buf
    }

    /// The shared textures for this screen size, made afresh when it changes.
    fn shared(&mut self, screen_px: [u32; 2]) -> &Shared {
        if self.shared.as_ref().map_or(true, |s| s.screen_px != screen_px) {
            let device = &self.state.device;
            let off = Target::new(device, "frost_off", scaled(screen_px, OFF_LONG), self.state.target_format);
            let half = Target::new(device, "frost_half", scaled(screen_px, OFF_LONG / 2), FORMAT);
            let blur = scaled(screen_px, BLUR_LONG);
            let ping = Target::new(device, "frost_ping", blur, FORMAT);
            let pong = Target::new(device, "frost_pong", blur, FORMAT);
            let uniforms = [
                self.uniform([0.0, 0.0]),
                self.uniform([1.0 / blur[0] as f32, 0.0]),
                self.uniform([0.0, 1.0 / blur[1] as f32]),
            ];
            let blit_off = self.bind(&off, &uniforms[0]);
            let blit_half = self.bind(&half, &uniforms[0]);
            let blur_h = self.bind(&ping, &uniforms[1]);
            let blur_v = self.bind(&pong, &uniforms[2]);
            self.shared = Some(Shared {
                screen_px,
                off,
                half,
                ping,
                pong,
                blit_off,
                blit_half,
                blur_h,
                blur_v,
                uniforms,
            });
        }
        self.shared.as_ref().unwrap()
    }

    /// Make every cut's backdrop from `shapes`, the frame's shapes in paint
    /// order, already trimmed to what can show in a frost. Cuts must be in
    /// ascending `position`.
    pub fn render(&mut self, ctx: &egui::Context, shapes: Vec<egui::epaint::ClippedShape>, cuts: Vec<Cut<'_>>) {
        if cuts.is_empty() {
            return;
        }
        let (screen_px, blur_size) = Self::sizes(ctx);
        let ppp = ctx.pixels_per_point();
        // A surface's texture is sized to the screen; re-make it when the
        // window was resized, keeping egui's id for it.
        let mut cuts = cuts;
        for cut in cuts.iter_mut() {
            if cut.backdrop.target.size != blur_size {
                let target = Target::new(&self.state.device, "frost_surface", blur_size, FORMAT);
                self.state.renderer.write().update_egui_texture_from_wgpu_texture(
                    &self.state.device,
                    &target.view,
                    wgpu::FilterMode::Linear,
                    cut.backdrop.id,
                );
                cut.backdrop.target = target;
            }
        }
        // Tessellate each segment between cuts on its own: the tessellator
        // merges neighbouring shapes, so the cuts must be made before it runs.
        let mut shapes = shapes;
        let mut segments = Vec::with_capacity(cuts.len() + 1);
        for cut in cuts.iter().rev() {
            let tail = shapes.split_off(cut.position.min(shapes.len()));
            segments.push(tail);
        }
        segments.push(shapes);
        segments.reverse();
        // Segment `i` is what lies between cut `i - 1` and cut `i`; the
        // last, after the last cut, is never drawn: nothing frosts it.
        segments.truncate(cuts.len());
        let jobs: Vec<Vec<egui::epaint::ClippedPrimitive>> = segments
            .into_iter()
            .map(|seg| {
                if seg.is_empty() {
                    Vec::new()
                } else {
                    ctx.tessellate(seg, ppp)
                }
            })
            .collect();

        self.shared(screen_px);
        let shared = self.shared.as_ref().unwrap();
        let device = self.state.device.clone();
        let queue = self.state.queue.clone();
        let desc = ScreenDescriptor {
            size_in_pixels: shared.off.size,
            pixels_per_point: ppp * shared.off.size[0] as f32 / screen_px[0] as f32,
        };
        let mut renderer = self.state.renderer.write();
        for (i, (cut, mut jobs)) in cuts.into_iter().zip(jobs).enumerate() {
            // A texture egui has not uploaded yet (a cover that arrived this
            // frame) is drawn next frame; a paint callback never.
            jobs.retain(|j| match &j.primitive {
                egui::epaint::Primitive::Mesh(m) => renderer.texture(&m.texture_id).is_some(),
                egui::epaint::Primitive::Callback(_) => false,
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frost"),
            });
            let user = if jobs.is_empty() {
                Vec::new()
            } else {
                renderer.update_buffers(&device, &queue, &mut encoder, &jobs, &desc)
            };
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("frost_egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &shared.off.view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: if i == 0 {
                                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                                } else {
                                    wgpu::LoadOp::Load
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    })
                    .forget_lifetime();
                if !jobs.is_empty() {
                    renderer.render(&mut pass, &jobs, &desc);
                }
            }
            let passes: [(&wgpu::RenderPipeline, &wgpu::BindGroup, &wgpu::TextureView); 6] = [
                (&self.blit, &shared.blit_off, &shared.half.view),
                (&self.blit, &shared.blit_half, &shared.ping.view),
                (&self.blur, &shared.blur_h, &shared.pong.view),
                (&self.blur, &shared.blur_v, &shared.ping.view),
                (&self.blur, &shared.blur_h, &shared.pong.view),
                (&self.blur, &shared.blur_v, &cut.backdrop.target.view),
            ];
            for (pipeline, bind, view) in passes {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("frost_blur"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..3, 0..1);
            }
            // One submit per cut: the renderer's vertex buffers are shared
            // with the next segment, and a write to them lands before the
            // submit that follows it.
            queue.submit(user.into_iter().chain(std::iter::once(encoder.finish())));
        }
    }
}

/// `px` scaled so its long side is `long` (never scaled up).
fn scaled(px: [u32; 2], long: u32) -> [u32; 2] {
    let s = (long as f32 / px[0].max(px[1]) as f32).min(1.0);
    [
        ((px[0] as f32 * s).round() as u32).max(1),
        ((px[1] as f32 * s).round() as u32).max(1),
    ]
}
