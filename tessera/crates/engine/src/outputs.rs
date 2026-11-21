use std::collections::HashMap;
use std::sync::Arc;

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
#[derive(Clone, Debug, PartialEq)]
enum Place {
    Window { x: i32, y: i32 },
    Display(MonitorInfo),
}

struct OutputWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    /// Always the output's resolution (mapping space); the OS scales it to the window.
    config: wgpu::SurfaceConfiguration,
    place: Place,
    presented: bool,
}

/// One window per bound output, keyed by output id.
#[derive(Default)]
pub struct Outputs {
    windows: HashMap<Uuid, OutputWindow>,
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
                w.window.set_title(&output.name);
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

    /// Draws each windowed output from `screens` through `mappings` and presents it. Returns
    /// whether anything presented (all surfaces are Fifo, so that frame waited for vsync). Never
    /// panics on surface errors: Outdated reconfigures, Lost recreates the surface, the rest skip.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        compositor: &mut Compositor,
        outputs: &[Output],
        mappings: &[Mapping],
        screens: &HashMap<Uuid, wgpu::TextureView>,
    ) -> bool {
        let mut paced = false;
        for output in outputs {
            let Some(w) = self.windows.get_mut(&output.id) else { continue };
            let frame = match w.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                wgpu::CurrentSurfaceTexture::Outdated => {
                    w.surface.configure(&gpu.device, &w.config);
                    continue;
                }
                wgpu::CurrentSurfaceTexture::Lost => {
                    match gpu.instance.create_surface(w.window.clone()) {
                        Ok(s) => {
                            s.configure(&gpu.device, &w.config);
                            w.surface = s;
                        }
                        Err(e) => log!("output '{}': surface lost, recreate failed: {e}", output.name),
                    }
                    continue;
                }
                _ => continue, // Timeout, Occluded, Validation: try again next frame.
            };
            let view = frame.texture.create_view(&Default::default());
            compositor.render_output(gpu, &view, output, mappings, screens);
            w.window.pre_present_notify();
            gpu.queue.present(frame);
            paced = true;
            if !w.presented {
                w.presented = true;
                log!("output '{}': first frame presented ({:?})", output.name, w.config.present_mode);
            }
        }
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
    surface.configure(&gpu.device, &config);
    Ok(OutputWindow { window, surface, config, place, presented: false })
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

#[cfg(not(target_os = "macos"))]
fn fullscreen(event_loop: &ActiveEventLoop, attrs: WindowAttributes, monitor: &MonitorHandle) -> anyhow::Result<Window> {
    use winit::window::{Fullscreen, WindowLevel};
    let attrs = attrs.with_window_level(WindowLevel::AlwaysOnTop);
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
    fn bind_monitor_missing_is_none() {
        let monitors = [mon("Built-in", 0, 0)];
        assert_eq!(bind_monitor(&display("LED A", 1920, 0), &monitors), None);
        assert_eq!(bind_monitor(&display("LED A", 1920, 0), &[]), None);
    }
}
