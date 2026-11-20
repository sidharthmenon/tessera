use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::Context;
use tessera_show::{
    EngineConfig, FromEngine, Layer, Mapping, MediaKind, Output, OutputState, Pattern, Rect, Scene, Screen, Source,
    ToEngine,
};
use uuid::Uuid;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::compositor::{Compositor, LayerTexture, ScreenSlot};
use crate::frame_clock::FrameClock;
use crate::gpu::Gpu;
use crate::outputs::{Outputs, monitors};
use crate::player::{ImageSource, Player};

const MONITOR_POLL: Duration = Duration::from_secs(2);
const IDENTIFY: Duration = Duration::from_secs(3);
/// Loop tick when no Fifo surface is pacing it (no windows, or all occluded).
const IDLE_TICK: Duration = Duration::from_millis(16);
const STATS: Duration = Duration::from_secs(1);
/// Frame clock period when no monitor reports a refresh rate.
const FALLBACK_MHZ: u32 = 60_000;

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

/// A media layer's frame source.
// ponytail: one per program layer id, opened (blocking) on SetProgram; Task 9's player pool
// (players shared across PVW/PGM by refcount) replaces this map.
enum Media {
    Video(Box<Player>),
    Image(Box<ImageSource>),
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
    /// Program layer id → (media id, source).
    media: HashMap<Uuid, (Uuid, Media)>,
    /// Newly opened videos, started together once all are ready (show their first frame or failed).
    starting: Vec<Uuid>,
    clock: FrameClock,
    /// The last frame presented on a Fifo surface (so the next one is one vsync later).
    paced: bool,
    presented: u32,
    dropped: u64,
    stats_since: Instant,
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
            media: HashMap::new(),
            starting: vec![],
            clock: FrameClock::new(period(FALLBACK_MHZ), Instant::now()),
            paced: false,
            presented: 0,
            dropped: 0,
            stats_since: Instant::now(),
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
                self.sync_media();
            }
            ToEngine::SetProgram { scene } => {
                self.program = scene;
                self.sync_media();
            }
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
        let refresh_mhz = self.outputs.refresh_mhz().unwrap_or(FALLBACK_MHZ);
        if period(refresh_mhz) != self.clock.period() {
            eprintln!("frame clock: {:.3} Hz", refresh_mhz as f64 / 1000.);
            self.clock = FrameClock::new(period(refresh_mhz), Instant::now());
        }
    }

    /// Opens a source for every program media layer that lacks one (without blocking: R22) and
    /// drops the sources of layers that are gone. New videos start together once all are ready
    /// (see `start_ready`), so none skips its opening frames.
    fn sync_media(&mut self) {
        let wanted: HashMap<Uuid, Uuid> = (self.program.values().flatten())
            .filter_map(|l| match l.source {
                Source::Media { media_id } => Some((l.id, media_id)),
                Source::Pattern { .. } => None,
            })
            .collect();
        self.media.retain(|layer, (media, _)| wanted.get(layer) == Some(media));
        for layer in self.program.values().flatten() {
            let Source::Media { media_id } = layer.source else { continue };
            if self.media.contains_key(&layer.id) {
                continue;
            }
            match self.open_media(layer, media_id) {
                Ok(media) => {
                    self.media.insert(layer.id, (media_id, media));
                    self.starting.push(layer.id);
                }
                Err(e) => self.send(FromEngine::Error { message: format!("layer {}: {e:#}", layer.id) }),
            }
        }
    }

    /// Plays the newly opened videos at `now` once every one of them is ready.
    fn start_ready(&mut self, now: Instant) {
        let waiting = |id| matches!(self.media.get(id), Some((_, Media::Video(player))) if !player.ready());
        if self.starting.iter().any(waiting) {
            return;
        }
        for id in std::mem::take(&mut self.starting) {
            if let Some((_, Media::Video(player))) = self.media.get_mut(&id) {
                player.play(now);
            }
        }
    }

    fn open_media(&self, layer: &Layer, media_id: Uuid) -> anyhow::Result<Media> {
        let item = self.config.media.iter().find(|m| m.id == media_id).context("unknown media")?;
        let path = Path::new(&item.path);
        Ok(match item.kind {
            MediaKind::Video => Media::Video(Box::new(Player::open(path, layer.playback.clone())?)),
            MediaKind::Image => Media::Image(Box::new(ImageSource::open(&self.gpu, path)?)),
        })
    }

    /// Forwards media errors; once a second reports presented fps and total dropped frames.
    fn report(&mut self, real: Instant) {
        for (_, media) in self.media.values_mut() {
            let error = match media {
                Media::Video(player) => {
                    self.dropped += player.take_dropped();
                    player.take_error()
                }
                Media::Image(image) => image.take_error(),
            };
            if let Some(message) = error {
                let _ = self.events.send(FromEngine::Error { message });
            }
        }
        self.presented += self.paced as u32;
        let elapsed = real - self.stats_since;
        if elapsed >= STATS {
            let fps = self.presented as f32 / elapsed.as_secs_f32();
            self.send(FromEngine::Stats { fps, dropped: self.dropped });
            (self.presented, self.stats_since) = (0, real);
        }
    }

    /// Composites every screen from `program` with each media layer's frame for `now` (frame
    /// clock time) and draws all outputs. Returns whether a Fifo present paced this frame.
    fn render(&mut self, now: Instant) -> bool {
        if self.outputs.is_empty() {
            return false;
        }
        let period = self.clock.period();
        let mut views = HashMap::new();
        for screen in &self.config.screens {
            let layers: Vec<(Layer, Option<LayerTexture>)> = (self.program.get(&screen.id).into_iter().flatten())
                .map(|l| {
                    // Hidden layers too: polling collects decode errors and keeps the queue moving.
                    let frame = match self.media.get_mut(&l.id) {
                        Some((_, Media::Video(player))) => player.current_frame(&self.gpu, now, period),
                        Some((_, Media::Image(image))) => image.texture(),
                        None => None,
                    };
                    (l.clone(), frame)
                })
                .collect();
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
        let real = Instant::now();
        if real >= self.next_poll {
            self.next_poll = real + MONITOR_POLL;
            self.apply(event_loop, false);
        }
        self.identify.retain(|_, until| *until > real);
        let now = self.clock.tick(real, self.paced);
        self.paced = self.render(now);
        self.start_ready(now);
        self.report(real);
        let flow = if self.paced { ControlFlow::Poll } else { ControlFlow::WaitUntil(real + IDLE_TICK) };
        event_loop.set_control_flow(flow);
    }
}

fn period(refresh_mhz: u32) -> Duration {
    Duration::from_secs_f64(1000. / refresh_mhz.max(1) as f64)
}
