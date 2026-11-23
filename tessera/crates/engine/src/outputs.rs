use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use tessera_show::{EngineConfig, Mapping, MonitorInfo, Output, OutputState, OutputTarget};
use uuid::Uuid;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowAttributes};

use crate::compositor::Compositor;
use crate::gpu::Gpu;

/// Where an output's window lives. Any change (or a new output size) recreates the window.
#[derive(Clone, Debug)]
enum Place {
    Window { x: i32, y: i32 },
    Display(MonitorInfo),
}

/// The refresh rate doesn't count: the frame clock follows it (`refresh_mhz`), and a monitor whose
/// reported rate wavers must not get its live window closed and reopened (a visible blink).
impl PartialEq for Place {
    fn eq(&self, other: &Place) -> bool {
        match (self, other) {
            (Place::Window { x, y }, Place::Window { x: x2, y: y2 }) => (x, y) == (x2, y2),
            (Place::Display(a), Place::Display(b)) => MonitorInfo { refresh_mhz: b.refresh_mhz, ..a.clone() } == *b,
            _ => false,
        }
    }
}

struct OutputWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    /// Always the output's resolution (mapping space); the OS scales it to the window.
    config: wgpu::SurfaceConfiguration,
    place: Place,
    presented: bool,
    /// After a drawable timeout: not drawn until then, so one stuck display costs the others a
    /// single 1 s stall per `STUCK_RETRY`, not one per frame.
    stuck_until: Option<Instant>,
    /// What the last frame's acquire said, logged when it changes.
    state: &'static str,
}

/// How long an output that timed out waiting for a drawable is left alone.
const STUCK_RETRY: Duration = Duration::from_secs(5);

/// One window per bound output, keyed by output id.
#[derive(Default)]
pub struct Outputs {
    windows: HashMap<Uuid, OutputWindow>,
    /// Time spent waiting in `get_current_texture` (vsync), summed until the caller takes it.
    pub acquire: Duration,
    /// `render` scratch: the frame's acquired surface textures.
    frames: Vec<(Uuid, wgpu::SurfaceTexture)>,
}

impl Outputs {
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Opens, recreates and closes windows to match `config`. Window targets get a decorated
    /// window at `x,y` with a `width×height` physical inner size; bound Display targets go
    /// borderless fullscreen on their monitor; unbound ones get no window.
    pub fn apply_config(
        &mut self,
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        config: &EngineConfig,
        monitors: &[MonitorInfo],
    ) -> Vec<OutputState> {
        let mut old = std::mem::take(&mut self.windows);
        let mut states = vec![];
        for output in &config.outputs {
            if self.windows.contains_key(&output.id) {
                log!("output {}: duplicate id, ignored", output.id);
                continue;
            }
            let place = match &output.target {
                OutputTarget::Window { x, y } => Some(Place::Window { x: *x, y: *y }),
                target => bind_monitor(target, monitors).map(|i| Place::Display(monitors[i].clone())),
            };
            let size = gpu.target_size(output.width, output.height);
            let had = old.contains_key(&output.id);
            // Dropping a mismatched window closes it before its replacement opens.
            let kept = old.remove(&output.id).filter(|w| Some(&w.place) == place.as_ref() && (w.config.width, w.config.height) == size);
            let window = match (kept, place) {
                (Some(w), _) => Some(w),
                (None, Some(place)) => match open(event_loop, gpu, output, place, size) {
                    Ok(w) => {
                        log!("output '{}': window open {}x{} on {:?}", output.name, size.0, size.1, w.place);
                        Some(w)
                    }
                    Err(e) => {
                        log!("output '{}': no window: {e:#}", output.name);
                        None
                    }
                },
                (None, None) => {
                    if had {
                        log!("output '{}': monitor gone, window closed", output.name);
                    }
                    None
                }
            };
            let monitor = window.as_ref().and_then(|w| match &w.place {
                Place::Display(m) => Some(m.name.clone()),
                Place::Window { .. } => None,
            });
            states.push(OutputState { output_id: output.id, bound: window.is_some(), monitor });
            if let Some(w) = window {
                if w.window.title() != output.name {
                    w.window.set_title(&output.name); // a rename; checked every monitor poll
                }
                self.windows.insert(output.id, w);
            }
        }
        for id in old.keys() {
            log!("output {id}: window closed");
        }
        states
    }

    /// Slowest refresh rate among the windows' monitors: the single render loop runs at that rate.
    pub fn refresh_mhz(&self) -> Option<u32> {
        self.windows.values().filter_map(|w| w.window.current_monitor()?.refresh_rate_millihertz()).min()
    }

    /// Draws each windowed output from `screens` through `mappings`, submits that (with whatever
    /// else was recorded) and presents them all. Returns whether anything presented (all surfaces
    /// are Fifo, so that frame waited for vsync). Never panics on surface errors: Outdated
    /// reconfigures, Lost recreates the surface, the rest skip.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        compositor: &mut Compositor,
        outputs: &[Output],
        mappings: &[Mapping],
        screens: &HashMap<Uuid, wgpu::TextureView>,
    ) -> bool {
        let mut frames = std::mem::take(&mut self.frames);
        for output in outputs {
            let Some(w) = self.windows.get_mut(&output.id) else { continue };
            let start = Instant::now();
            if w.stuck_until.is_some_and(|until| start < until) {
                continue;
            }
            let acquired = w.surface.get_current_texture();
            self.acquire += start.elapsed();
            let state = match &acquired {
                wgpu::CurrentSurfaceTexture::Success(_) | wgpu::CurrentSurfaceTexture::Suboptimal(_) => "showing",
                wgpu::CurrentSurfaceTexture::Occluded => "hidden (occluded)",
                wgpu::CurrentSurfaceTexture::Timeout => "stuck (no drawable)",
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => "reconfiguring",
                _ => "not drawable",
            };
            if std::mem::replace(&mut w.state, state) != state {
                log!("output '{}': {state}", output.name); // for the show log: when an output stopped and came back
            }
            let frame = match acquired {
                wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                wgpu::CurrentSurfaceTexture::Outdated => {
                    configure(gpu, &w.surface, &w.config);
                    continue;
                }
                wgpu::CurrentSurfaceTexture::Lost => {
                    if let Err(e) = replace_surface(gpu, w) {
                        log!("output '{}': surface lost, recreate failed: {e}", output.name);
                    }
                    continue;
                }
                wgpu::CurrentSurfaceTexture::Timeout => {
                    log!("output '{}': no drawable for {:?}, skipped for {STUCK_RETRY:?}", output.name, start.elapsed());
                    w.stuck_until = Some(Instant::now() + STUCK_RETRY);
                    continue;
                }
                _ => continue, // Occluded, Validation: try again next frame.
            };
            w.stuck_until = None;
            let view = frame.texture.create_view(&Default::default());
            compositor.render_output(gpu, &view, output, mappings, screens);
            frames.push((output.id, frame));
        }
        // One submit for every output's pass, then the presents back to back (a present must
        // follow the submit that draws its surface).
        gpu.submit();
        let paced = !frames.is_empty();
        for (id, frame) in frames.drain(..) {
            let Some(w) = self.windows.get_mut(&id) else { continue };
            w.window.pre_present_notify();
            gpu.queue.present(frame);
            if !w.presented {
                w.presented = true;
                log!("output '{}': first frame presented ({:?})", w.window.title(), w.config.present_mode);
            }
        }
        self.frames = frames;
        paced
    }
}

fn open(
    event_loop: &ActiveEventLoop,
    gpu: &Gpu,
    output: &Output,
    place: Place,
    (width, height): (u32, u32),
) -> anyhow::Result<OutputWindow> {
    // Normal level: Window targets are dev/confidence monitors and must not cover the controller UI.
    let attrs = Window::default_attributes().with_title(&output.name);
    let window = Arc::new(match &place {
        Place::Window { x, y } => {
            let pos = PhysicalPosition::new(*x, *y);
            let window = event_loop.create_window(attrs.with_inner_size(PhysicalSize::new(width, height)).with_position(pos))?;
            window.set_outer_position(pos); // macOS ignores with_position for other monitors
            window
        }
        Place::Display(info) => {
            let monitor = event_loop
                .available_monitors()
                .find(|m| monitor_info(m) == *info)
                .context("monitor disappeared")?;
            fullscreen(event_loop, attrs, &monitor)?
        }
    });
    let surface = gpu.instance.create_surface(window.clone())?;
    let caps = surface.get_capabilities(&gpu.adapter);
    anyhow::ensure!(!caps.formats.is_empty(), "GPU cannot present to this window");
    // Non-sRGB: the compositor blends in gamma space, premultiplied.
    let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
        .into_iter()
        .find(|f| caps.formats.contains(f))
        .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
        .unwrap_or(caps.formats[0]);
    let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        caps.alpha_modes[0]
    };
    // ponytail: every output Fifo in one render loop (R18): Immediate tears on LED walls and Metal
    // has no Mailbox. Ceiling: the loop runs at the slowest display's rate and one output's hitch
    // stalls all. Upgrade (spike FINDINGS): a render thread per output plus one device-wide lock
    // around every queue.submit and Surface::configure.
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: vec![],
    };
    configure(gpu, &surface, &config);
    keep_on_top(&window, &place, true);
    Ok(OutputWindow { window, surface, config, place, presented: false, stuck_until: None, state: "" })
}

/// After `Lost`: a new surface for `w`'s window. The old one goes first: DXGI makes no second swap
/// chain for a window while it has one (E_ACCESSDENIED).
fn replace_surface(gpu: &Gpu, w: &mut OutputWindow) -> anyhow::Result<()> {
    let surface = gpu.instance.create_surface(w.window.clone())?;
    keep_on_top(&w.window, &w.place, false);
    w.surface = surface; // drops the old surface, and its swap chain
    configure(gpu, &w.surface, &w.config);
    keep_on_top(&w.window, &w.place, true);
    Ok(())
}

/// Display outputs sit above every other window (macOS: `fullscreen` sets the level). DXGI makes no
/// swap chain for a window that is always on top and covers its monitor (DXGI_ERROR_INVALID_CALL),
/// so a Display window goes on top only once its swap chain exists, and down while it gets a new one.
fn keep_on_top(window: &Window, place: &Place, on_top: bool) {
    #[cfg(not(target_os = "macos"))]
    if matches!(place, Place::Display(_)) {
        use winit::window::WindowLevel;
        window.set_window_level(if on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
    }
    #[cfg(target_os = "macos")]
    let _ = (window, place, on_top);
}

/// `Surface::configure`, then (macOS) a 1 s limit on waiting for a drawable. wgpu turns that limit
/// off, so a display that stops taking frames without its window counting as occluded (an LED
/// processor losing sync) would hang the single render loop, and every output with it, until the
/// controller restarts the engine. With the limit that output skips frames (`Timeout`) and the
/// others carry on.
fn configure(gpu: &Gpu, surface: &wgpu::Surface<'_>, config: &wgpu::SurfaceConfiguration) {
    surface.configure(&gpu.device, config);
    #[cfg(target_os = "macos")]
    // SAFETY: only a CAMetalLayer property is set; the surface stays wgpu's.
    if let Some(hal) = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() } {
        hal.render_layer().lock().setAllowsNextDrawableTimeout(true);
    }
}

#[cfg(target_os = "macos")]
fn fullscreen(event_loop: &ActiveEventLoop, attrs: WindowAttributes, monitor: &MonitorHandle) -> anyhow::Result<Window> {
    use winit::platform::macos::WindowExtMacOS;
    // Logical = the monitor's CoreGraphics origin; physical coords differ per monitor scale.
    let pos = monitor.position().to_logical::<f64>(monitor.scale_factor());
    let window = event_loop.create_window(attrs.with_position(pos))?;
    window.set_outer_position(pos);
    // Simple (pre-Lion) fullscreen: no Spaces animation, the window just covers the screen.
    anyhow::ensure!(window.set_simple_fullscreen(true), "simple fullscreen refused");
    // The engine is never the active app, so the active app's menu bar (level 24) would draw across
    // the wall. Screen-saver level (1000) also clears the Dock, notification banners and pop-up
    // menus, which sit between 24 and 1000; only the cursor stays above. Not on the main display
    // (origin 0,0: the menu bar's): the operator's controller is there, and a window above
    // everything with no Dock icon to click would lock them out (I3). A menu-bar strip is fine there.
    if monitor.position() == PhysicalPosition::new(0, 0) {
        log!("output on the main display: normal window level, the controller can come in front");
        return Ok(window);
    }
    use objc2_app_kit::{NSScreenSaverWindowLevel, NSView};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else { anyhow::bail!("not an AppKit window") };
    // SAFETY: `ns_view` is the live NSView `window` owns; we are on the main (event loop) thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window().context("view has no NSWindow")?.setLevel(NSScreenSaverWindowLevel);
    Ok(window)
}

/// Borderless fullscreen at normal level: `keep_on_top` lifts it once it has a swap chain.
#[cfg(not(target_os = "macos"))]
fn fullscreen(event_loop: &ActiveEventLoop, attrs: WindowAttributes, monitor: &MonitorHandle) -> anyhow::Result<Window> {
    use winit::window::Fullscreen;
    Ok(event_loop.create_window(attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone())))))?)
}

pub fn monitor_info(m: &MonitorHandle) -> MonitorInfo {
    let (pos, size) = (m.position(), m.size());
    MonitorInfo {
        name: m.name().unwrap_or_default(),
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        scale: m.scale_factor(),
        refresh_mhz: m.refresh_rate_millihertz(),
    }
}

pub fn monitors(event_loop: &ActiveEventLoop) -> Vec<MonitorInfo> {
    event_loop.available_monitors().map(|m| monitor_info(&m)).collect()
}

/// Index of the monitor a Display target binds to: exact name (preferring one also at the saved
/// position, so identical monitors stay apart), else exact `(x, y)`. Window targets never bind.
pub fn bind_monitor(target: &OutputTarget, monitors: &[MonitorInfo]) -> Option<usize> {
    let OutputTarget::Display { monitor_name, monitor_x, monitor_y } = target else { return None };
    let named = |m: &MonitorInfo| m.name == *monitor_name;
    let at = |m: &MonitorInfo| (m.x, m.y) == (*monitor_x, *monitor_y);
    (monitors.iter().position(|m| named(m) && at(m)))
        .or_else(|| monitors.iter().position(named))
        .or_else(|| monitors.iter().position(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(name: &str, x: i32, y: i32) -> MonitorInfo {
        MonitorInfo { name: name.into(), x, y, width: 1920, height: 1080, scale: 1.0, refresh_mhz: Some(60_000) }
    }

    fn display(name: &str, x: i32, y: i32) -> OutputTarget {
        OutputTarget::Display { monitor_name: name.into(), monitor_x: x, monitor_y: y }
    }

    /// Windows: a Display output gets a swap chain and presents, also once its surface is replaced
    /// (as after `Lost`). Opens a fullscreen window on the last monitor for a moment; skipped with
    /// no monitor.
    #[test]
    #[cfg(windows)]
    fn display_output_presents_after_open_and_surface_replacement() {
        use winit::application::ApplicationHandler;
        use winit::event_loop::EventLoop;
        use winit::platform::windows::EventLoopBuilderExtWindows;

        struct Check {
            gpu: Gpu,
            result: Option<anyhow::Result<()>>,
        }
        impl ApplicationHandler for Check {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                if self.result.is_none() {
                    self.result = Some(check(event_loop, &self.gpu));
                }
                event_loop.exit();
            }
            fn window_event(&mut self, _: &ActiveEventLoop, _: winit::window::WindowId, _: winit::event::WindowEvent) {}
        }
        /// Any validation error from `f` (wgpu reports surface failures that way).
        fn scoped<T>(gpu: &Gpu, f: impl FnOnce() -> T) -> anyhow::Result<T> {
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let value = f();
            match pollster::block_on(scope.pop()) {
                Some(e) => Err(anyhow::anyhow!("{e}")),
                None => Ok(value),
            }
        }
        fn present(gpu: &Gpu, w: &OutputWindow) -> anyhow::Result<()> {
            match scoped(gpu, || w.surface.get_current_texture())? {
                wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => scoped(gpu, || gpu.queue.present(frame)),
                other => anyhow::bail!("no frame to present: {:?}", std::mem::discriminant(&other)),
            }
        }
        fn check(event_loop: &ActiveEventLoop, gpu: &Gpu) -> anyhow::Result<()> {
            let Some(monitor) = event_loop.available_monitors().last() else {
                eprintln!("skipped: no monitor");
                return Ok(());
            };
            let info = monitor_info(&monitor);
            let output = Output {
                id: Uuid::new_v4(),
                name: "display output test".into(),
                node: "local".into(),
                target: display(&info.name, info.x, info.y),
                width: info.width,
                height: info.height,
                brightness: 1.0,
            };
            let mut w = scoped(gpu, || open(event_loop, gpu, &output, Place::Display(info.clone()), (info.width, info.height)))??;
            present(gpu, &w).context("after open")?;
            scoped(gpu, || replace_surface(gpu, &mut w))??;
            present(gpu, &w).context("after replacing the surface")
        }
        let gpu = Gpu::new_headless().expect("these tests need a GPU");
        let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
        let mut check = Check { gpu, result: None };
        event_loop.run_app(&mut check).unwrap();
        check.result.expect("the event loop never resumed").unwrap();
    }

    #[test]
    fn bind_monitor_by_name() {
        let monitors = [mon("Built-in", 0, 0), mon("LED A", 1920, 0), mon("LED A", 3840, 0)];
        // Name wins over a stale position.
        assert_eq!(bind_monitor(&display("LED A", 0, 0), &monitors), Some(1));
        // Duplicate names: the one at the saved position.
        assert_eq!(bind_monitor(&display("LED A", 3840, 0), &monitors), Some(2));
        assert_eq!(bind_monitor(&OutputTarget::Window { x: 0, y: 0 }, &monitors), None);
    }

    #[test]
    fn bind_monitor_falls_back_to_position() {
        let monitors = [mon("Built-in", 0, 0), mon("Renamed", 1920, 0)];
        assert_eq!(bind_monitor(&display("LED A", 1920, 0), &monitors), Some(1));
    }

    #[test]
    fn a_wavering_refresh_rate_keeps_the_window() {
        let a = Place::Display(mon("LED A", 1920, 0));
        let b = Place::Display(MonitorInfo { refresh_mhz: Some(59_940), ..mon("LED A", 1920, 0) });
        assert_eq!(a, b);
        assert_ne!(a, Place::Display(MonitorInfo { width: 3840, ..mon("LED A", 1920, 0) }));
    }

    #[test]
    fn bind_monitor_missing_is_none() {
        let monitors = [mon("Built-in", 0, 0)];
        assert_eq!(bind_monitor(&display("LED A", 1920, 0), &monitors), None);
        assert_eq!(bind_monitor(&display("LED A", 1920, 0), &[]), None);
    }
}
