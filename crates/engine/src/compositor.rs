use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use tessera_show::{Layer, Mapping, Output, Pattern, Rect, Screen, Source, TransitionKind};
use uuid::Uuid;
use wgpu::util::DeviceExt;

use crate::gpu::Gpu;

/// A frame ready to composite. Cheap to clone: views are refcounted.
#[derive(Clone, Debug)]
pub struct LayerTexture {
    pub planes: Planes,
    /// RGB already multiplied by alpha (YUV planes are opaque, so it only matters for `Rgba`).
    pub premultiplied: bool,
    /// YUV full range 0–255 instead of limited Y 16–235 / UV 16–240.
    pub full_range: bool,
}

#[derive(Clone, Debug)]
pub enum Planes {
    /// Any 4-channel float-sampleable view (Rgba8/Bgra8 unorm, Rgba16 float/unorm, …).
    Rgba(wgpu::TextureView),
    /// Y `R8Unorm` + half-resolution UV `Rg8Unorm`.
    Nv12 { y: wgpu::TextureView, uv: wgpu::TextureView },
    /// Y `R16Unorm` + half-resolution UV `Rg16Unorm`, 10 bits MSB-aligned.
    P010 { y: wgpu::TextureView, uv: wgpu::TextureView },
    /// One `Rgba16Unorm` of packed A, Y, Cb, Cr (VideoToolbox ProRes 4444 + alpha, `y416`);
    /// alpha is straight, so `premultiplied` stays false.
    Ayuv64(wgpu::TextureView),
    /// Like `Rgba`, but the fourth channel is undefined: drawn opaque (NDI's BGRX).
    Rgbx(wgpu::TextureView),
}

/// Which copy of a screen to render: PGM, PVW and a transition's outgoing PGM can all show the
/// same screen in one frame, so each gets its own texture. During a transition, incoming = `Pgm`
/// and `Transition` holds the mix of `Outgoing` into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenSlot {
    Pgm,
    Pvw,
    Outgoing,
    Transition,
}

/// Mirrors `U` in shaders/layer.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LayerUniform {
    rect: [f32; 4],
    color: [f32; 4],
    screen: [f32; 2],
    kind: u32,
    flags: u32,
    opacity: f32,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    yuv_scale: f32,
    rotation: f32,
    _pad: [f32; 2],
}

/// Mirrors `U` in shaders/transition.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransitionUniform {
    kind: u32,
    p: f32,
    _pad: [f32; 2],
}

/// Mirrors `U` in shaders/map.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MapUniform {
    src: [f32; 4],
    dst: [f32; 4],
    size: [f32; 2],
    gain: f32,
    _pad: f32,
}

pub struct Compositor {
    bind_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    map_shader: wgpu::ShaderModule,
    layer_pipeline: wgpu::RenderPipeline,
    transition_pipeline: wgpu::RenderPipeline,
    /// Keyed by target format: outputs may be surfaces (e.g. Bgra8Unorm).
    map_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
    // ponytail: never evicted; prune against EngineConfig.screens if screens churn.
    screens: HashMap<(Uuid, ScreenSlot), wgpu::TextureView>,
}

impl Compositor {
    pub fn new(gpu: &Gpu) -> Compositor {
        let d = &gpu.device;
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None };
        let texture = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let uniform =
            wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None };
        let bind_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                entry(0, wgpu::ShaderStages::FRAGMENT, texture),
                entry(1, wgpu::ShaderStages::FRAGMENT, texture),
                entry(2, wgpu::ShaderStages::FRAGMENT, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                entry(3, wgpu::ShaderStages::VERTEX_FRAGMENT, uniform),
            ],
        });
        let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let shader = |src: &str| {
            d.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(src.into()) })
        };
        let layer_shader = shader(include_str!("shaders/layer.wgsl"));
        let layer_pipeline = pipeline(gpu, &pipeline_layout, &layer_shader, wgpu::TextureFormat::Rgba8Unorm);
        let transition_shader = shader(include_str!("shaders/transition.wgsl"));
        let transition_pipeline = pipeline(gpu, &pipeline_layout, &transition_shader, wgpu::TextureFormat::Rgba8Unorm);
        Compositor {
            map_shader: shader(include_str!("shaders/map.wgsl")),
            bind_layout,
            pipeline_layout,
            layer_pipeline,
            transition_pipeline,
            map_pipelines: HashMap::new(),
            // Linear + clamp-to-edge (the default address mode).
            sampler: d.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            blank: gpu.upload(1, 1, wgpu::TextureFormat::Rgba8Unorm, &[0; 4]).create_view(&Default::default()),
            screens: HashMap::new(),
        }
    }

    /// Composites `layers` bottom→top into the cached Rgba8Unorm texture for `(screen, slot)`
    /// (premultiplied, cleared transparent) and returns a view of it. Hidden layers and media layers
    /// without a frame are skipped. A screen larger than the device limit is cropped to it.
    pub fn render_screen(
        &mut self,
        gpu: &Gpu,
        slot: ScreenSlot,
        screen: &Screen,
        layers: &[(Layer, Option<LayerTexture>)],
    ) -> wgpu::TextureView {
        let view = self.target(gpu, slot, screen);
        let size = [view.texture().width() as f32, view.texture().height() as f32];
        let groups: Vec<_> = layers
            .iter()
            .filter(|(l, _)| l.visible)
            .filter_map(|(l, t)| self.layer_group(gpu, size, l, t.as_ref()))
            .collect();
        draw(gpu, &view, wgpu::Color::TRANSPARENT, &self.layer_pipeline, &groups);
        view
    }

    /// Mixes two renders of `screen` (premultiplied) into its `Transition` texture at progress `p`
    /// (0 = all `outgoing`): Fade cross-fades; WipeLeft reveals `incoming` from the left edge up to
    /// a hard edge at p·width, WipeRight from the right edge. Cut shows `incoming`.
    pub fn render_transition(
        &mut self,
        gpu: &Gpu,
        screen: &Screen,
        outgoing: &wgpu::TextureView,
        incoming: &wgpu::TextureView,
        kind: &TransitionKind,
        p: f32,
    ) -> wgpu::TextureView {
        let (kind, p) = match kind {
            TransitionKind::Fade => (0, p),
            TransitionKind::WipeLeft => (1, p),
            TransitionKind::WipeRight => (2, p),
            TransitionKind::Cut => (0, 1.),
        };
        let view = self.target(gpu, ScreenSlot::Transition, screen);
        let u = TransitionUniform { kind, p: p.clamp(0., 1.), _pad: [0.; 2] };
        let group = self.bind(gpu, outgoing, incoming, bytemuck::bytes_of(&u));
        draw(gpu, &view, wgpu::Color::TRANSPARENT, &self.transition_pipeline, &[group]);
        view
    }

    /// The cached Rgba8Unorm texture for `(screen, slot)`, (re)made at the screen's clamped size.
    fn target(&mut self, gpu: &Gpu, slot: ScreenSlot, screen: &Screen) -> wgpu::TextureView {
        let size = gpu.target_size(screen.width, screen.height);
        let view = self
            .screens
            .entry((screen.id, slot))
            .or_insert_with(|| gpu.target(screen.width, screen.height).create_view(&Default::default()));
        if (view.texture().width(), view.texture().height()) != size {
            *view = gpu.target(screen.width, screen.height).create_view(&Default::default());
        }
        view.clone()
    }

    /// Clears `target` to black and draws each of `output`'s mappings (others are ignored): the
    /// mapping's `src` (clipped to its screen) lands in `dst`, × `output.brightness`.
    pub fn render_output(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        output: &Output,
        mappings: &[Mapping],
        screens: &HashMap<Uuid, wgpu::TextureView>,
    ) {
        let t = target.texture();
        let size = [t.width() as f32, t.height() as f32];
        let groups: Vec<_> = mappings
            .iter()
            .filter(|m| m.output_id == output.id)
            .filter_map(|m| {
                let view = screens.get(&m.screen_id)?;
                let bounds = Rect { x: 0., y: 0., w: view.texture().width() as f32, h: view.texture().height() as f32 };
                let src = m.src.intersect(&bounds)?;
                Some(self.map_group(gpu, view, &src.uv_in(&bounds), &src.map_between(&m.src, &m.dst), size, output.brightness))
            })
            .collect();
        self.map_pass(gpu, target, &groups);
    }

    /// Preview: screens at their layout positions on black, scaled to `width` px (aspect kept).
    pub fn render_layout(
        &mut self,
        gpu: &Gpu,
        screens: &[Screen],
        views: &HashMap<Uuid, wgpu::TextureView>,
        width: u32,
    ) -> wgpu::TextureView {
        // i64: layout_x + width can overflow i32.
        let x0 = screens.iter().map(|s| s.layout_x as i64).min().unwrap_or(0);
        let y0 = screens.iter().map(|s| s.layout_y as i64).min().unwrap_or(0);
        let x1 = screens.iter().map(|s| s.layout_x as i64 + s.width as i64).max().unwrap_or(1);
        let y1 = screens.iter().map(|s| s.layout_y as i64 + s.height as i64).max().unwrap_or(1);
        let bounds = Rect { x: x0 as f32, y: y0 as f32, w: (x1 - x0).max(1) as f32, h: (y1 - y0).max(1) as f32 };
        // `as u32` saturates and `target` clamps to the device limit, so a tall layout can't overflow.
        let texture = gpu.target(width, (bounds.h * width as f32 / bounds.w).round() as u32);
        let out = Rect { x: 0., y: 0., w: texture.width() as f32, h: texture.height() as f32 };
        let full_uv = Rect { x: 0., y: 0., w: 1., h: 1. };
        let groups: Vec<_> = screens
            .iter()
            .filter_map(|s| {
                let place = Rect { x: s.layout_x as f32, y: s.layout_y as f32, w: s.width as f32, h: s.height as f32 };
                let dst = place.map_between(&bounds, &out);
                Some(self.map_group(gpu, views.get(&s.id)?, &full_uv, &dst, [out.w, out.h], 1.0))
            })
            .collect();
        let view = texture.create_view(&Default::default());
        self.map_pass(gpu, &view, &groups);
        view
    }

    fn layer_group(&self, gpu: &Gpu, size: [f32; 2], l: &Layer, tex: Option<&LayerTexture>) -> Option<wgpu::BindGroup> {
        let mut u = LayerUniform {
            rect: [l.rect.x, l.rect.y, l.rect.w, l.rect.h],
            color: [0.; 4],
            screen: size,
            kind: 0,
            flags: 0,
            opacity: l.opacity,
            brightness: l.brightness,
            contrast: l.contrast,
            saturation: l.saturation,
            yuv_scale: 255.,
            rotation: if l.rotation.is_finite() { l.rotation.to_radians() } else { 0. },
            _pad: [0.; 2],
        };
        let (a, b) = match (&l.source, tex) {
            (Source::Pattern { pattern }, _) => {
                (u.kind, u.color) = match pattern {
                    Pattern::Bars => (2, u.color),
                    Pattern::Grid => (3, u.color),
                    Pattern::Gradient => (4, u.color),
                    Pattern::Solid { rgba } => (5, *rgba),
                };
                (&self.blank, &self.blank)
            }
            (Source::Media { .. }, None) => return None,
            (Source::Media { .. }, Some(t)) => {
                u.flags = t.premultiplied as u32 | (t.full_range as u32) << 1;
                match &t.planes {
                    Planes::Rgba(v) => (v, &self.blank),
                    Planes::Nv12 { y, uv } => {
                        u.kind = 1;
                        (y, uv)
                    }
                    Planes::P010 { y, uv } => {
                        (u.kind, u.yuv_scale) = (1, 256.);
                        (y, uv)
                    }
                    Planes::Ayuv64(v) => {
                        (u.kind, u.yuv_scale) = (6, 256.);
                        (v, &self.blank)
                    }
                    Planes::Rgbx(v) => {
                        u.kind = 7;
                        (v, &self.blank)
                    }
                }
            }
        };
        Some(self.bind(gpu, a, b, bytemuck::bytes_of(&u)))
    }

    fn map_group(&self, gpu: &Gpu, view: &wgpu::TextureView, uv: &Rect, dst: &Rect, size: [f32; 2], gain: f32) -> wgpu::BindGroup {
        let u = MapUniform { src: [uv.x, uv.y, uv.w, uv.h], dst: [dst.x, dst.y, dst.w, dst.h], size, gain, _pad: 0. };
        self.bind(gpu, view, &self.blank, bytemuck::bytes_of(&u))
    }

    // ponytail: a uniform buffer + bind group per draw per frame; move to a dynamic-offset ring if profiling flags it.
    fn bind(&self, gpu: &Gpu, a: &wgpu::TextureView, b: &wgpu::TextureView, uniform: &[u8]) -> wgpu::BindGroup {
        let buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: uniform,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(a) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(b) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: buffer.as_entire_binding() },
            ],
        })
    }

    fn map_pass(&mut self, gpu: &Gpu, target: &wgpu::TextureView, groups: &[wgpu::BindGroup]) {
        let format = target.texture().format();
        let pipeline = self
            .map_pipelines
            .entry(format)
            .or_insert_with(|| pipeline(gpu, &self.pipeline_layout, &self.map_shader, format));
        draw(gpu, target, wgpu::Color::BLACK, pipeline, groups);
    }
}

fn pipeline(
    gpu: &Gpu,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// One render pass: clear `target`, draw a 6-vertex quad per bind group, submit.
fn draw(
    gpu: &Gpu,
    target: &wgpu::TextureView,
    clear: wgpu::Color,
    pipeline: &wgpu::RenderPipeline,
    groups: &[wgpu::BindGroup],
) {
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        for g in groups {
            pass.set_bind_group(0, g, &[]);
            pass.draw(0..6, 0..1);
        }
    }
    gpu.queue.submit([enc.finish()]);
}
