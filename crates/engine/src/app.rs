use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use tessera_show::{
    EngineConfig, FromEngine, Layer, Mapping, Output, OutputState, Pattern, Rect, Scene, Screen, Source, ToEngine,
};
use uuid::Uuid;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::compositor::{Compositor, LayerTexture, ScreenSlot};
use crate::gpu::Gpu;
use crate::outputs::{Outputs, monitors};

const MONITOR_POLL: Duration = Duration::from_secs(2);
const IDENTIFY: Duration = Duration::from_secs(3);
/// Loop tick when no Fifo surface is pacing it (no windows, or all occluded).
const IDLE_TICK: Duration = Duration::from_millis(16);

#[derive(Debug)]
pub enum UserEvent {
    Command(ToEngine),
}

/// Cloneable, thread-safe way to send the engine commands.
#[derive(Clone)]
pub struct EngineHandle(EventLoopProxy<UserEvent>);

impl EngineHandle {
    /// False once the engine's event loop has exited.
    pub fn send(&self, command: ToEngine) -> bool {
        self.0.send_event(UserEvent::Command(command)).is_ok()
    }
}

/// The engine's event loop (macOS: Accessory app, no Dock icon) plus a handle into it.
pub fn event_loop() -> anyhow::Result<(EventLoop<UserEvent>, EngineHandle)> {
    let mut builder = EventLoop::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build()?;
    let handle = EngineHandle(event_loop.create_proxy());
    Ok((event_loop, handle))
}

/// Engine state driven by `event_loop().run_app(&mut app)`; replies go out through `events`.
pub struct App {
    gpu: Gpu,
    compositor: Compositor,
    outputs: Outputs,
    config: EngineConfig,
    program: Scene,
    events: Sender<FromEngine>,
    status: Vec<OutputState>,
    next_poll: Instant,
    identify: HashMap<Uuid, Instant>,
}

impl App {
    pub fn new(events: Sender<FromEngine>) -> anyhow::Result<App> {
        // Surfaces are created later from this device's instance (Metal/DX12 need no display handle).
        let gpu = Gpu::new_headless()?;
        // A live show must not die on a GPU validation error (the default handler panics).
        gpu.device.on_uncaptured_error(Arc::new(|e| eprintln!("GPU error: {e}")));
        Ok(App {
            compositor: Compositor::new(&gpu),
            gpu,
            outputs: Outputs::default(),
            config: EngineConfig { outputs: vec![], screens: vec![], mappings: vec![], media: vec![] },
            program: Scene::new(),
            events,
            status: vec![],
            next_poll: Instant::now() + MONITOR_POLL,
            identify: HashMap::new(),
        })
    }

    fn send(&self, message: FromEngine) {
        let _ = self.events.send(message); // receiver gone = nobody listening; keep running
    }

    fn command(&mut self, event_loop: &ActiveEventLoop, command: ToEngine) {
        match command {
            ToEngine::SetConfig { config } => {
                self.config = config;
                self.apply(event_loop, true);
            }
            ToEngine::SetProgram { scene } => self.program = scene,
            ToEngine::ListMonitors => self.send(FromEngine::Monitors { monitors: monitors(event_loop) }),
            ToEngine::Identify { output_id } => {
                if self.config.outputs.iter().any(|o| o.id == output_id) {
                    self.identify.insert(output_id, Instant::now() + IDENTIFY);
                } else {
                    eprintln!("identify: unknown output {output_id}");
                }
            }
            other => {
                let debug = format!("{other:?}");
                let name = debug.split([' ', '{', '(']).next().unwrap_or_default();
                eprintln!("{name}: unsupported until later task, ignored");
            }
        }
    }

    /// Re-binds outputs to the current monitors; reports status on change (always if `report`).
    fn apply(&mut self, event_loop: &ActiveEventLoop, report: bool) {
        let status = self.outputs.apply_config(event_loop, &self.gpu, &self.config, &monitors(event_loop));
        if report || status != self.status {
            self.send(FromEngine::OutputStatus { outputs: status.clone() });
        }
        self.status = status;
    }

    /// Composites every screen from `program` (media layers have no frames yet, so they stay
    /// transparent) and draws all outputs. Returns whether a Fifo present paced this frame.
    fn render(&mut self) -> bool {
        if self.outputs.is_empty() {
            return false;
        }
        let mut views = HashMap::new();
        for screen in &self.config.screens {
            let layers: Vec<(Layer, Option<LayerTexture>)> =
                self.program.get(&screen.id).into_iter().flatten().map(|l| (l.clone(), None)).collect();
            views.insert(screen.id, self.compositor.render_screen(&self.gpu, ScreenSlot::Pgm, screen, &layers));
        }
        let mut mappings = self.config.mappings.clone();
        for output in self.config.outputs.iter().filter(|o| self.identify.contains_key(&o.id)) {
            let (mapping, view) = identify_overlay(&self.gpu, &mut self.compositor, output);
            mappings.push(mapping);
            views.insert(output.id, view);
        }
        self.outputs.render(&self.gpu, &mut self.compositor, &self.config.outputs, &mappings, &views)
    }
}

/// White border around the whole output, drawn over its content as one extra mapping.
fn identify_overlay(gpu: &Gpu, compositor: &mut Compositor, output: &Output) -> (Mapping, wgpu::TextureView) {
    let (w, h) = (output.width.max(1) as f32, output.height.max(1) as f32);
    let t = (w.min(h) / 20.).max(4.);
    let white = Source::Pattern { pattern: Pattern::Solid { rgba: [1.; 4] } };
    let edges = [(0., 0., w, t), (0., h - t, w, t), (0., 0., t, h), (w - t, 0., t, h)];
    let layers = edges.map(|(x, y, w, h)| (Layer::new(white.clone(), Rect { x, y, w, h }), None));
    // ponytail: borrows the Pvw cache slot under the output's id (ids never collide with screen
    // ids in practice); give overlays their own ScreenSlot if Task 9 makes that awkward.
    let screen = Screen { id: output.id, name: String::new(), width: output.width, height: output.height, layout_x: 0, layout_y: 0 };
    let view = compositor.render_screen(gpu, ScreenSlot::Pvw, &screen, &layers);
    let full = Rect { x: 0., y: 0., w, h };
    let mapping = Mapping { id: Uuid::nil(), screen_id: output.id, src: full.clone(), output_id: output.id, dst: full };
    (mapping, view)
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, UserEvent::Command(command): UserEvent) {
        self.command(event_loop, command);
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let WindowEvent::CloseRequested = event {
            eprintln!("output windows follow the config; close ignored");
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_poll {
            self.next_poll = now + MONITOR_POLL;
            self.apply(event_loop, false);
        }
        self.identify.retain(|_, until| *until > now);
        let paced = self.render();
        event_loop.set_control_flow(if paced { ControlFlow::Poll } else { ControlFlow::WaitUntil(now + IDLE_TICK) });
    }
}
