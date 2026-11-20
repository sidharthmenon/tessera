use std::collections::HashMap;

use tessera_engine::compositor::{Compositor, LayerTexture, Planes, ScreenSlot};
use tessera_engine::gpu::{Gpu, Readback, read_rgba};
use tessera_show::{Layer, Mapping, Output, OutputTarget, Pattern, Rect, Screen, Source, TransitionKind};
use uuid::Uuid;
use wgpu::TextureFormat;

fn gpu() -> Gpu {
    Gpu::new_headless().expect("no GPU adapter: these tests need a real GPU (headless Metal on macOS)")
}

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn screen(w: u32, h: u32) -> Screen {
    Screen { id: Uuid::new_v4(), name: "S".into(), width: w, height: h, layout_x: 0, layout_y: 0 }
}

fn output(w: u32, h: u32) -> Output {
    Output {
        id: Uuid::new_v4(),
        name: "O".into(),
        node: "local".into(),
        target: OutputTarget::Window { x: 0, y: 0 },
        width: w,
        height: h,
        brightness: 1.0,
    }
}

fn mapping(s: &Screen, src: Rect, o: &Output, dst: Rect) -> Mapping {
    Mapping { id: Uuid::new_v4(), screen_id: s.id, src, output_id: o.id, dst }
}

fn pattern(p: Pattern, rect: Rect) -> (Layer, Option<LayerTexture>) {
    (Layer::new(Source::Pattern { pattern: p }, rect), None)
}

fn solid(rgba: [f32; 4], rect: Rect) -> (Layer, Option<LayerTexture>) {
    pattern(Pattern::Solid { rgba }, rect)
}

fn px(data: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    data[i..i + 4].try_into().unwrap()
}

#[track_caller]
fn assert_near(got: [u8; 4], want: [u8; 4], tol: u8) {
    let ok = got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol);
    assert!(ok, "pixel {got:?} not within ±{tol} of {want:?}");
}

/// Renders `layers` onto `s` and reads the screen texture back.
fn render(gpu: &Gpu, c: &mut Compositor, s: &Screen, layers: &[(Layer, Option<LayerTexture>)]) -> Vec<u8> {
    let view = c.render_screen(gpu, ScreenSlot::Pgm, s, layers);
    read_rgba(gpu, view.texture()).unwrap()
}

const RED: [f32; 4] = [1., 0., 0., 1.];
const BLUE: [f32; 4] = [0., 0., 1., 1.];

#[test]
fn solid_layer_fills_rect() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(100, 100);
    let mut hidden = solid([0., 1., 0., 1.], r(0., 0., 100., 100.));
    hidden.0.visible = false;
    let media_without_frame = (Layer::new(Source::Media { media_id: Uuid::new_v4() }, r(0., 0., 100., 100.)), None);
    let data = render(&gpu, &mut c, &s, &[solid(RED, r(0., 0., 50., 100.)), hidden, media_without_frame]);
    assert_eq!(px(&data, 100, 25, 50), [255, 0, 0, 255]);
    assert_eq!(px(&data, 100, 75, 50), [0, 0, 0, 0], "hidden and frameless layers draw nothing");
}

#[test]
fn opacity_half() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(100, 100);
    let mut white = solid([1., 1., 1., 1.], r(0., 0., 100., 100.));
    white.0.opacity = 0.5;
    let data = render(&gpu, &mut c, &s, &[white]);
    assert_near(px(&data, 100, 50, 50), [128, 128, 128, 128], 2);
}

#[test]
fn layer_effects_apply_in_order() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(10, 10);
    let mut l = solid([0.6, 0.2, 0.2, 1.], r(0., 0., 10., 10.));
    (l.0.contrast, l.0.brightness, l.0.saturation) = (2.0, 0.1, 0.0);
    // contrast → (0.7,-0.1,-0.1); +0.1 → (0.8,0,0); desaturate → luma 0.2126*0.8 = 0.170 → 43
    let data = render(&gpu, &mut c, &s, &[l]);
    assert_near(px(&data, 10, 5, 5), [43, 43, 43, 255], 2);
}

#[test]
fn patterns_render() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);

    let s = screen(512, 8);
    let data = render(&gpu, &mut c, &s, &[pattern(Pattern::Bars, r(0., 0., 512., 8.))]);
    assert_eq!(px(&data, 512, 32, 4), [255, 255, 255, 255], "bar 0 white");
    assert_eq!(px(&data, 512, 5 * 64 + 32, 4), [255, 0, 0, 255], "bar 5 red");
    assert_eq!(px(&data, 512, 7 * 64 + 32, 4), [0, 0, 0, 255], "bar 7 black");

    let s = screen(256, 256);
    let data = render(&gpu, &mut c, &s, &[pattern(Pattern::Grid, r(0., 0., 256., 256.))]);
    assert_eq!(px(&data, 256, 64, 10), [255, 255, 255, 255], "grid line x=64");
    assert_eq!(px(&data, 256, 10, 192), [255, 255, 255, 255], "grid line y=192");
    assert_eq!(px(&data, 256, 127, 100), [255, 255, 255, 255], "center cross");
    assert_eq!(px(&data, 256, 100, 100), [0, 0, 0, 255], "between lines");

    let s = screen(256, 4);
    let data = render(&gpu, &mut c, &s, &[pattern(Pattern::Gradient, r(0., 0., 256., 4.))]);
    assert_near(px(&data, 256, 0, 2), [0, 0, 0, 255], 2);
    assert_near(px(&data, 256, 128, 2), [128, 128, 128, 255], 2);
    assert_near(px(&data, 256, 255, 2), [255, 255, 255, 255], 2);
}

#[test]
fn rgba_texture_straight_and_premultiplied() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(8, 4);
    let tex = |rgba: [u8; 4], premultiplied| {
        let t = gpu.upload(4, 4, TextureFormat::Rgba8Unorm, &rgba.repeat(16));
        LayerTexture { planes: Planes::Rgba(t.create_view(&Default::default())), premultiplied, full_range: true }
    };
    let media = |rect, t| (Layer::new(Source::Media { media_id: Uuid::new_v4() }, rect), Some(t));
    let data = render(&gpu, &mut c, &s, &[
        media(r(0., 0., 4., 4.), tex([255, 0, 0, 128], false)),
        media(r(4., 0., 4., 4.), tex([128, 0, 0, 128], true)),
    ]);
    assert_near(px(&data, 8, 1, 2), [128, 0, 0, 128], 2);
    assert_near(px(&data, 8, 6, 2), [128, 0, 0, 128], 2);
}

#[test]
fn nv12_solid_converts() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(8, 8);
    let nv12 = |luma: u8| {
        let y = gpu.upload(8, 8, TextureFormat::R8Unorm, &[luma; 64]);
        let uv = gpu.upload(4, 4, TextureFormat::Rg8Unorm, &[128; 32]);
        let planes = Planes::Nv12 { y: y.create_view(&Default::default()), uv: uv.create_view(&Default::default()) };
        let layer = Layer::new(Source::Media { media_id: Uuid::new_v4() }, r(0., 0., 8., 8.));
        (layer, Some(LayerTexture { planes, premultiplied: false, full_range: false }))
    };
    let data = render(&gpu, &mut c, &s, &[nv12(235)]);
    assert_near(px(&data, 8, 4, 4), [255, 255, 255, 255], 3);
    let data = render(&gpu, &mut c, &s, &[nv12(16)]);
    assert_near(px(&data, 8, 4, 4), [0, 0, 0, 255], 3);
}

#[test]
fn p010_solid_converts() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(8, 8);
    // 10-bit code values, MSB-aligned in 16 bits
    let plane = |v: u16, n| (v << 6).to_le_bytes().repeat(n);
    let p010 = |luma: u16| {
        let y = gpu.upload(8, 8, TextureFormat::R16Unorm, &plane(luma, 64));
        let uv = gpu.upload(4, 4, TextureFormat::Rg16Unorm, &plane(512, 32));
        let planes = Planes::P010 { y: y.create_view(&Default::default()), uv: uv.create_view(&Default::default()) };
        let layer = Layer::new(Source::Media { media_id: Uuid::new_v4() }, r(0., 0., 8., 8.));
        (layer, Some(LayerTexture { planes, premultiplied: false, full_range: false }))
    };
    let data = render(&gpu, &mut c, &s, &[p010(940)]);
    assert_near(px(&data, 8, 4, 4), [255, 255, 255, 255], 3);
    let data = render(&gpu, &mut c, &s, &[p010(64)]);
    assert_near(px(&data, 8, 4, 4), [0, 0, 0, 255], 3);
}

#[test]
fn slots_do_not_alias() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(10, 10);
    let full = r(0., 0., 10., 10.);
    let pgm = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(RED, full.clone())]);
    let pvw = c.render_screen(&gpu, ScreenSlot::Pvw, &s, &[solid(BLUE, full)]);
    assert_eq!(px(&read_rgba(&gpu, pgm.texture()).unwrap(), 10, 5, 5), [255, 0, 0, 255]);
    assert_eq!(px(&read_rgba(&gpu, pvw.texture()).unwrap(), 10, 5, 5), [0, 0, 255, 255]);
}

#[test]
fn oversized_screen_does_not_panic() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let max = gpu.device.limits().max_texture_dimension_2d;
    let s = screen(max + 100, 4);
    let view = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(RED, r(0., 0., (max + 100) as f32, 4.))]);
    assert_eq!((view.texture().width(), view.texture().height()), (max, 4), "clamped to the device limit");
    assert_eq!(px(&read_rgba(&gpu, view.texture()).unwrap(), max, max - 1, 2), [255, 0, 0, 255]);
    let tall = Screen { layout_y: i32::MAX, ..screen(1, u32::MAX) };
    c.render_layout(&gpu, &[tall], &HashMap::new(), 100);
}

#[test]
fn screen_split_across_two_outputs() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(200, 100);
    let view = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(RED, r(0., 0., 100., 100.)), solid(BLUE, r(100., 0., 100., 100.))]);
    let screens = HashMap::from([(s.id, view)]);
    let (a, b) = (output(100, 100), output(100, 100));
    let full = r(0., 0., 100., 100.);
    let mappings = [mapping(&s, r(0., 0., 100., 100.), &a, full.clone()), mapping(&s, r(100., 0., 100., 100.), &b, full)];

    for (o, want) in [(&a, [255, 0, 0, 255]), (&b, [0, 0, 255, 255])] {
        let target = gpu.target(100, 100);
        c.render_output(&gpu, &target.create_view(&Default::default()), o, &mappings, &screens);
        assert_eq!(px(&read_rgba(&gpu, &target).unwrap(), 100, 50, 50), want);
    }
}

#[test]
fn mapping_outside_bounds_clips() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(100, 100);
    let view = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(RED, r(0., 0., 100., 100.))]);
    let screens = HashMap::from([(s.id, view)]);
    let o = output(100, 100);
    let draw = |c: &mut Compositor, m: Mapping| {
        let target = gpu.target(100, 100);
        c.render_output(&gpu, &target.create_view(&Default::default()), &o, &[m], &screens);
        read_rgba(&gpu, &target).unwrap()
    };

    // dst hangs off the output's bottom-right
    let data = draw(&mut c, mapping(&s, r(0., 0., 100., 100.), &o, r(50., 50., 100., 100.)));
    assert_eq!(px(&data, 100, 75, 75), [255, 0, 0, 255]);
    assert_eq!(px(&data, 100, 25, 25), [0, 0, 0, 255]);

    // src hangs off the screen's right edge: the missing part stays black, not edge-smeared
    let data = draw(&mut c, mapping(&s, r(50., 0., 100., 100.), &o, r(0., 0., 100., 100.)));
    assert_eq!(px(&data, 100, 25, 50), [255, 0, 0, 255]);
    assert_eq!(px(&data, 100, 75, 50), [0, 0, 0, 255]);
}

#[test]
fn brightness_zero_output_black() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(100, 100);
    let view = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid([1., 1., 1., 1.], r(0., 0., 100., 100.))]);
    let mut o = output(100, 100);
    o.brightness = 0.0;
    let full = r(0., 0., 100., 100.);
    let target = gpu.target(100, 100);
    c.render_output(&gpu, &target.create_view(&Default::default()), &o, &[mapping(&s, full.clone(), &o, full)], &HashMap::from([(s.id, view)]));
    assert_eq!(px(&read_rgba(&gpu, &target).unwrap(), 100, 50, 50), [0, 0, 0, 255]);
}

#[test]
fn layout_places_screens_scaled() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let a = screen(100, 100);
    let b = Screen { layout_x: 100, layout_y: 100, ..screen(100, 100) };
    let full = r(0., 0., 100., 100.);
    let views = HashMap::from([
        (a.id, c.render_screen(&gpu, ScreenSlot::Pgm, &a, &[solid(RED, full.clone())])),
        (b.id, c.render_screen(&gpu, ScreenSlot::Pgm, &b, &[solid(BLUE, full)])),
    ]);
    // 200×200 layout scaled to 100 px wide
    let view = c.render_layout(&gpu, &[a, b], &views, 100);
    assert_eq!((view.texture().width(), view.texture().height()), (100, 100));
    let data = read_rgba(&gpu, view.texture()).unwrap();
    assert_eq!(px(&data, 100, 25, 25), [255, 0, 0, 255]);
    assert_eq!(px(&data, 100, 75, 75), [0, 0, 255, 255]);
    assert_eq!(px(&data, 100, 75, 25), [0, 0, 0, 255]);
}

#[test]
fn transition_mixes_outgoing_into_incoming() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(100, 10);
    let full = r(0., 0., 100., 10.);
    let outgoing = c.render_screen(&gpu, ScreenSlot::Outgoing, &s, &[solid(RED, full.clone())]);
    let incoming = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(BLUE, full)]);
    let mut mix = |kind, p| read_rgba(&gpu, c.render_transition(&gpu, &s, &outgoing, &incoming, &kind, p).texture()).unwrap();
    let fade = mix(TransitionKind::Fade, 0.5);
    assert_near(px(&fade, 100, 50, 5), [128, 0, 128, 255], 2);
    assert_near(px(&mix(TransitionKind::Fade, 0.), 100, 50, 5), [255, 0, 0, 255], 0);
    // Hard edge at p·width; WipeLeft reveals the incoming from the left edge, WipeRight from the right.
    let left = mix(TransitionKind::WipeLeft, 0.5);
    assert_eq!((px(&left, 100, 49, 5), px(&left, 100, 50, 5)), ([0, 0, 255, 255], [255, 0, 0, 255]));
    let right = mix(TransitionKind::WipeRight, 0.5);
    assert_eq!((px(&right, 100, 49, 5), px(&right, 100, 50, 5)), ([255, 0, 0, 255], [0, 0, 255, 255]));
    assert_eq!(px(&mix(TransitionKind::WipeLeft, 1.), 100, 99, 5), [0, 0, 255, 255]);
}

/// The preview readback (Task 10) never waits: it finishes on non-blocking polls alone.
#[test]
fn readback_completes_without_waiting() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(70, 8); // a row of 280 bytes: padded to 512 in the buffer
    let view = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid(RED, r(0., 0., 35., 8.)), solid(BLUE, r(35., 0., 35., 8.))]);
    let mut readback = Readback::start(&gpu, view.texture());
    assert_eq!(readback.size(), (70, 8));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let data = loop {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        if let Some(data) = readback.try_take() {
            break data.unwrap();
        }
        assert!(std::time::Instant::now() < deadline, "readback never finished");
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(data.len(), 70 * 8 * 4, "rows tightly packed");
    assert_eq!((px(&data, 70, 34, 7), px(&data, 70, 35, 0)), ([255, 0, 0, 255], [0, 0, 255, 255]));
}

/// The 960 px preview shrinks big screens a lot: 1 px detail (grid lines) must average in, not
/// fall between the samples (T5 minor: no mips).
#[test]
fn layout_downscale_keeps_thin_lines() {
    let gpu = gpu();
    let mut c = Compositor::new(&gpu);
    let s = screen(256, 8);
    let line = c.render_screen(&gpu, ScreenSlot::Pgm, &s, &[solid([1.; 4], r(64., 0., 1., 8.))]);
    let view = c.render_layout(&gpu, std::slice::from_ref(&s), &HashMap::from([(s.id, line)]), 64); // 4×
    let data = read_rgba(&gpu, view.texture()).unwrap();
    assert_near(px(&data, 64, 16, 1), [64, 64, 64, 255], 4); // texels 64..68: one white of four
    assert_eq!(px(&data, 64, 15, 1), [0, 0, 0, 255]);
    assert_eq!(px(&data, 64, 17, 1), [0, 0, 0, 255]);
}
