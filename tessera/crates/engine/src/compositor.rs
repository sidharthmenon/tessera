use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use tessera_show::{Crop, Layer, Mapping, Output, Pattern, Rect, Screen, Source, TransitionKind};
use uuid::Uuid;

use crate::gpu::Gpu;

/// A frame ready to composite. Cheap to clone: views are refcounted.
#[derive(Clone, Debug)]
pub struct LayerTexture {
    pub planes: Planes,
    /// RGB already multiplied by alpha (YUV planes are opaque, so it only matters for `Rgba`).
    pub premultiplied: bool,
    /// YUV full range 0–255 instead of limited Y 16–235 / UV 16–240.
    pub full_range: bool,
    /// The frame's part of its textures in uv: `[1, 1]` unless the decoder padded the allocation
    /// (e.g. a 1080-row frame in a 1088-row texture).
    pub uv_scale: [f32; 2],
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

/// Room for the largest per-draw parameters (`LayerUniform`), passed as immediates.
const IMMEDIATE_SIZE: u32 = size_of::<LayerUniform>() as u32;

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
    crop: [f32; 4],
}

/// `l` is drawn opaque over the whole `size` screen: visible, fully opaque, unrotated, its rect
/// covering the screen, its source without alpha (crop only picks what fills the rect).
fn covers(l: &Layer, tex: Option<&LayerTexture>, size: [f32; 2]) -> bool {
    let opaque_source = match (&l.source, tex) {
        (Source::Pattern { pattern: Pattern::Solid { rgba } }, _) => rgba[3] >= 1.,
        (Source::Pattern { .. }, _) => true, // bars, grid, gradient
        (Source::Media { .. }, Some(t)) => matches!(t.planes, Planes::Nv12 { .. } | Planes::P010 { .. } | Planes::Rgbx(_)),
        (Source::Media { .. }, None) => false, // nothing drawn
    };
    let r = &l.rect;
    l.visible && opaque_source && l.opacity >= 1. && l.rotation % 360. == 0. && r.x <= 0. && r.y <= 0. && r.x + r.w >= size[0] && r.y + r.h >= size[1]
}

/// The part of the source a layer draws, in uv (x0, y0, x1, y1). A crop that leaves (almost)
/// nothing, or isn't finite, is ignored rather than drawing a smear of one texel.
fn crop_uv(c: &Crop) -> [f32; 4] {
    let edge = |v: f32| if v.is_finite() { v.clamp(0., 1.) } else { 0. };
    let (x0, y0, x1, y1) = (edge(c.left), edge(c.top), 1. - edge(c.right), 1. - edge(c.bottom));
    if x1 - x0 < 0.001 || y1 - y0 < 0.001 { [0., 0., 1., 1.] } else { [x0, y0, x1, y1] }
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
    binds: Binds,
    pipeline_layout: wgpu::PipelineLayout,
    map_shader: wgpu::ShaderModule,
    layer_pipeline: wgpu::RenderPipeline,
    transition_pipeline: wgpu::RenderPipeline,
    /// Keyed by target format: outputs may be surfaces (e.g. Bgra8Unorm).
    map_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    blank: wgpu::TextureView,
    /// Pruned to the config's screens (and outputs, for identify overlays) by `retain`.
    screens: HashMap<(Uuid, ScreenSlot), wgpu::TextureView>,
    /// `render_layout`'s target, reused while its size holds. PGM's and PVW's previews share it:
    /// each one's readback copy is queued before the next draw into it.
    layout: Option<wgpu::TextureView>,
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
        let bind_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                entry(0, wgpu::ShaderStages::FRAGMENT, texture),
                entry(1, wgpu::ShaderStages::FRAGMENT, texture),
                entry(2, wgpu::ShaderStages::FRAGMENT, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: IMMEDIATE_SIZE,
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
            binds: Binds {
                layout: bind_layout,
                // Linear + clamp-to-edge (the default address mode).
                sampler: d.create_sampler(&wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                }),
                cache: HashMap::new(),
                frame: 0,
            },
            pipeline_layout,
            layer_pipeline,
            transition_pipeline,
            map_pipelines: HashMap::new(),
            blank: gpu.upload(1, 1, wgpu::TextureFormat::Rgba8Unorm, &[0; 4]).create_view(&Default::default()),
            screens: HashMap::new(),
            layout: None,
        }
    }

    /// Frees the cached textures of screens (and identify overlays) whose id isn't in `ids`: a
    /// removed 4K screen held up to four 33 MB textures for the rest of the session.
    pub fn retain(&mut self, ids: &std::collections::HashSet<Uuid>) {
        self.screens.retain(|(id, _), _| ids.contains(id));
    }

    /// Once per frame: forgets bind groups not used in this one or the last, so the textures they
    /// hold (a replaced video frame's, a closed screen's) are freed.
    pub fn end_frame(&mut self) {
        let frame = self.binds.frame;
        self.binds.cache.retain(|_, (_, used)| *used + 1 >= frame);
        self.binds.frame += 1;
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
        // Layers under the topmost one that hides the whole screen would only be overdrawn.
        let first = layers.iter().rposition(|(l, t)| covers(l, t.as_ref(), size)).unwrap_or(0);
        let groups: Vec<_> = layers[first..]
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
        let group = self.binds.get(gpu, outgoing, incoming);
        draw(gpu, &view, wgpu::Color::TRANSPARENT, &self.transition_pipeline, &[(group, u)]);
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
        let height = (bounds.h * width as f32 / bounds.w).round() as u32;
        let size = gpu.target_size(width, height);
        let view = match self.layout.take() {
            Some(v) if (v.texture().width(), v.texture().height()) == size => v,
            _ => gpu.target(width, height).create_view(&Default::default()),
        };
        self.layout = Some(view.clone());
        let texture = view.texture();
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
        self.map_pass(gpu, &view, &groups);
        view
    }

    fn layer_group(&mut self, gpu: &Gpu, size: [f32; 2], l: &Layer, tex: Option<&LayerTexture>) -> Option<(wgpu::BindGroup, LayerUniform)> {
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
            crop: crop_uv(&l.crop),
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
                let [sx, sy] = t.uv_scale;
                u.crop = [u.crop[0] * sx, u.crop[1] * sy, u.crop[2] * sx, u.crop[3] * sy];
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
        Some((self.binds.get(gpu, a, b), u))
    }

    fn map_group(&mut self, gpu: &Gpu, view: &wgpu::TextureView, uv: &Rect, dst: &Rect, size: [f32; 2], gain: f32) -> (wgpu::BindGroup, MapUniform) {
        let u = MapUniform { src: [uv.x, uv.y, uv.w, uv.h], dst: [dst.x, dst.y, dst.w, dst.h], size, gain, _pad: 0. };
        (self.binds.get(gpu, view, &self.blank), u)
    }

    fn map_pass(&mut self, gpu: &Gpu, target: &wgpu::TextureView, groups: &[(wgpu::BindGroup, MapUniform)]) {
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

/// The bind groups (two textures + the sampler) of the frame's draws, kept while their textures are
/// in use: per-draw parameters go as immediates, so a layer's group is reused frame after frame.
struct Binds {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Each group with the frame it was last used in (`Compositor::end_frame` prunes).
    cache: HashMap<(wgpu::TextureView, wgpu::TextureView), (wgpu::BindGroup, u64)>,
    frame: u64,
}

impl Binds {
    fn get(&mut self, gpu: &Gpu, a: &wgpu::TextureView, b: &wgpu::TextureView) -> wgpu::BindGroup {
        let (layout, sampler, frame) = (&self.layout, &self.sampler, self.frame);
        let (group, used) = self.cache.entry((a.clone(), b.clone())).or_insert_with(|| {
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(a) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(b) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            });
            (group, frame)
        });
        *used = frame;
        group.clone()
    }
}

/// One render pass, recorded into the frame's encoder (see `Gpu::submit`): clear `target`, then a
/// 6-vertex quad per (bind group, parameters).
fn draw<U: Pod>(gpu: &Gpu, target: &wgpu::TextureView, clear: wgpu::Color, pipeline: &wgpu::RenderPipeline, draws: &[(wgpu::BindGroup, U)]) {
    gpu.record(|enc| {
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
        for (group, u) in draws {
            pass.set_bind_group(0, group, &[]);
            pass.set_immediates(0, bytemuck::bytes_of(u));
            pass.draw(0..6, 0..1);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer's bind group is reused while its texture stays, and dropped a frame after the
    /// texture is replaced (a video's new GpuFrame, an imported frame per frame).
    #[test]
    fn bind_groups_are_reused_then_pruned() {
        let gpu = Gpu::new_headless().expect("these tests need a GPU");
        let mut c = Compositor::new(&gpu);
        let screen = Screen { id: Uuid::new_v4(), name: "S".into(), width: 8, height: 8, layout_x: 0, layout_y: 0 };
        let layer = Layer::new(Source::Media { media_id: Uuid::new_v4() }, Rect { x: 0., y: 0., w: 8., h: 8. });
        let texture = || {
            let view = gpu.upload(1, 1, wgpu::TextureFormat::Rgba8Unorm, &[255; 4]).create_view(&Default::default());
            LayerTexture { planes: Planes::Rgba(view), premultiplied: false, full_range: false, uv_scale: [1., 1.] }
        };
        let stable = texture();
        for _ in 0..10 {
            c.render_screen(&gpu, ScreenSlot::Pgm, &screen, &[(layer.clone(), Some(stable.clone()))]);
            gpu.submit();
            c.end_frame();
        }
        assert_eq!(c.binds.cache.len(), 1, "one group, reused");
        for _ in 0..10 {
            c.render_screen(&gpu, ScreenSlot::Pgm, &screen, &[(layer.clone(), Some(texture()))]);
            gpu.submit();
            c.end_frame();
        }
        assert!(c.binds.cache.len() <= 2, "{} groups kept for textures long gone", c.binds.cache.len());
    }

    /// Only layers from the topmost screen-covering opaque one up are drawn.
    #[test]
    fn covered_layers_are_skipped() {
        let size = [100., 50.];
        let full = Rect { x: 0., y: 0., w: 100., h: 50. };
        let solid = |a: f32, rect: Rect| Layer::new(Source::Pattern { pattern: Pattern::Solid { rgba: [1., 0., 0., a] } }, rect);
        assert!(covers(&solid(1., full.clone()), None, size));
        assert!(!covers(&solid(0.5, full.clone()), None, size), "translucent colour");
        assert!(!covers(&solid(1., Rect { w: 99., ..full.clone() }), None, size), "doesn't reach the right edge");
        assert!(!covers(&Layer { opacity: 0.9, ..solid(1., full.clone()) }, None, size));
        assert!(!covers(&Layer { rotation: 10., ..solid(1., full.clone()) }, None, size));
        assert!(!covers(&Layer { visible: false, ..solid(1., full.clone()) }, None, size));
        let media = Layer::new(Source::Media { media_id: Uuid::new_v4() }, full);
        assert!(!covers(&media, None, size), "no frame yet: nothing drawn");
    }

    /// Retired things live until the GPU has done the submit after them, then drop.
    #[test]
    fn retired_drops_after_the_gpu_is_done() {
        let gpu = Gpu::new_headless().expect("these tests need a GPU");
        let marker = std::sync::Arc::new(());
        gpu.retire(marker.clone());
        assert_eq!(std::sync::Arc::strong_count(&marker), 2, "held until a submit");
        gpu.submit();
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        assert_eq!(std::sync::Arc::strong_count(&marker), 1, "dropped once the work is done");
    }
}
