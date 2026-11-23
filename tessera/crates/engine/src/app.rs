use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, SyncSender};
use std::time::{Duration, Instant};

use tessera_show::{Bus, EngineConfig, FromEngine, Layer, Mapping, Output, OutputState, Pattern, Rect, Screen, Source, ToEngine};
use uuid::Uuid;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::audio::{AudioTap, Mixer, lock};
use crate::buses::{Buses, FileOpener};
use crate::compositor::{Compositor, ScreenSlot};
use crate::frame_clock::FrameClock;
use crate::gpu::{Gpu, Readback};
use crate::net::{Previews, RgbaFrame};
use crate::outputs::{Outputs, monitors};

const MONITOR_POLL: Duration = Duration::from_secs(2);
const IDENTIFY: Duration = Duration::from_secs(3);
/// Loop tick when no Fifo surface is pacing it (no windows, or all occluded).
const IDLE_TICK: Duration = Duration::from_millis(16);
const STATS: Duration = Duration::from_secs(1);
const POSITIONS: Duration = Duration::from_millis(200);
/// Unchanged positions (nothing playing) are re-sent this often.
const POSITIONS_IDLE: Duration = Duration::from_secs(1);
/// How often the perf line (render-loop cost) is logged.
const PERF: Duration = Duration::from_secs(10);
/// At most 15 preview frames a second per bus.
const PREVIEW_PERIOD: Duration = Duration::from_nanos(1_000_000_000 / 15);
const PREVIEW_WIDTH: u32 = 960;
/// Frame clock period when no monitor reports a refresh rate.
const FALLBACK_MHZ: u32 = 60_000;

#[derive(Debug)]
pub enum UserEvent {
    Command(ToEngine),
}

/// Cloneable, thread-safe way to send the engine commands.
#[derive(Clone)]
pub struct EngineHandle(Arc<dyn Fn(ToEngine) -> bool + Send + Sync>);

impl EngineHandle {
    /// Commands go to `send`, which returns false once the engine is gone (tests: a stub engine).
    pub fn new(send: impl Fn(ToEngine) -> bool + Send + Sync + 'static) -> EngineHandle {
        EngineHandle(Arc::new(send))
    }

    /// False once the engine's event loop has exited.
    pub fn send(&self, command: ToEngine) -> bool {
        (self.0)(command)
    }
}

/// The engine's event loop (macOS: Accessory app, no Dock icon) plus a handle into it.
pub fn event_loop() -> anyhow::Result<(EventLoop<UserEvent>, EngineHandle)> {
    // This thread runs the render loop for the process lifetime.
    crate::os::keep_awake();
    crate::os::set_priority(crate::os::Priority::Render);
    let mut builder = EventLoop::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build()?;
    // The Mutex makes it Sync on Windows, whose proxy is only Send.
    let proxy = Mutex::new(event_loop.create_proxy());
    let handle = EngineHandle::new(move |command| lock(&proxy).send_event(UserEvent::Command(command)).is_ok());
    Ok((event_loop, handle))
}

/// Engine state driven by `event_loop().run_app(&mut app)`; replies go out through `events`.
pub struct App {
    gpu: Gpu,
    compositor: Compositor,
    outputs: Outputs,
    config: EngineConfig,
    buses: Buses<FileOpener>,
    events: Sender<FromEngine>,
    status: Vec<OutputState>,
    next_poll: Instant,
    identify: HashMap<Uuid, Instant>,
    clock: FrameClock,
    /// The current frame's time on the frame clock: commands act at it, previews render it.
    now: Instant,
    /// The last frame presented on a Fifo surface (so the next one is one vsync later).
    paced: bool,
    presented: u32,
    dropped: u64,
    stats_since: Instant,
    next_positions: Instant,
    /// The last Positions sent, and when.
    positions: (BTreeMap<Uuid, u64>, Instant),
    perf: Perf,
    previews: Arc<Previews>,
    /// Indexed by `Bus as usize`.
    captures: [Capture; 2],
    /// To each bus's JPEG thread (indexed by `Bus as usize`): (width, height, RGBA).
    encoders: [SyncSender<RgbaFrame>; 2],
    /// `None`: no usable output device, the engine runs silent.
    mixer: Option<Mixer>,
    // Per-frame scratch, reused so the render path doesn't allocate.
    views: HashMap<Uuid, wgpu::TextureView>,
    mappings: Vec<Mapping>,
    taps: Vec<(AudioTap, f32)>,
    /// What the mixer has: `set_taps` (a lock the audio callback contends for) only on a change.
    taps_sent: Vec<(AudioTap, f32)>,
}

/// Render-loop cost since `since`: loop time minus the vsync wait in `get_current_texture`.
struct Perf {
    since: Instant,
    loops: u32,
    busy: Duration,
    busy_max: Duration,
    acquire_max: Duration,
    dropped: u64,
}

impl Perf {
    fn new(now: Instant) -> Perf {
        Perf { since: now, loops: 0, busy: Duration::ZERO, busy_max: Duration::ZERO, acquire_max: Duration::ZERO, dropped: 0 }
    }

    /// One loop iteration; every `PERF` logs the line and starts over.
    fn add(&mut self, busy: Duration, acquire: Duration, dropped: u64, now: Instant) {
        (self.loops, self.busy, self.dropped) = (self.loops + 1, self.busy + busy, self.dropped + dropped);
        (self.busy_max, self.acquire_max) = (self.busy_max.max(busy), self.acquire_max.max(acquire));
        if now - self.since >= PERF {
            let ms = |d: Duration| d.as_secs_f64() * 1000.;
            log!(
                "perf: {} loops, busy mean {:.2} ms max {:.2} ms, acquire max {:.2} ms, {} dropped",
                self.loops,
                ms(self.busy) / self.loops.max(1) as f64,
                ms(self.busy_max),
                ms(self.acquire_max),
                self.dropped
            );
            *self = Perf::new(now);
        }
    }
}

/// A bus's preview: the readback in flight, and when the next one may start.
struct Capture {
    pending: Option<Readback>,
    /// The last readback's buffer, reused by the next.
    spare: Option<wgpu::Buffer>,
    next: Instant,
}

impl App {
    /// Replies go out through `events`; previews of the buses `previews` says are watched are
    /// encoded (on a thread of its own) into it.
    pub fn new(events: Sender<FromEngine>, previews: Arc<Previews>) -> anyhow::Result<App> {
        // Surfaces are created later from this device's instance (Metal/DX12 need no display handle).
        let gpu = Gpu::new_headless()?;
        // A live show must not die on a GPU validation error (the default handler panics).
        gpu.device.on_uncaptured_error(Arc::new(|e| log!("GPU error: {e}")));
        // After a GPU reset the outputs would freeze while Stats keep flowing, so the controller's
        // watchdog would never restart us: exit instead (its supervisor restarts the engine).
        gpu.device.set_device_lost_callback(|reason, message| {
            if matches!(reason, wgpu::DeviceLostReason::Unknown) {
                log!("GPU device lost ({message}): exiting so the engine is restarted");
                std::thread::sleep(Duration::from_millis(200)); // the log thread writes the line
                std::process::exit(70);
            }
        });
        let mixer = match Mixer::start() {
            Ok(mixer) => {
                let (rate, channels) = mixer.format();
                log!("audio: default output, {rate} Hz, {channels} ch");
                Some(mixer)
            }
            Err(e) => {
                log!("audio: {e:#}; running silent");
                let _ = events.send(FromEngine::Error { message: format!("audio: {e:#}") });
                None
            }
        };
        let encoders = previews.spawn_encoders()?;
        let now = Instant::now();
        Ok(App {
            compositor: Compositor::new(&gpu),
            buses: Buses::new(FileOpener::new(gpu.clone(), mixer.as_ref().map(Mixer::format))),
            gpu,
            outputs: Outputs::default(),
            config: EngineConfig { outputs: vec![], screens: vec![], mappings: vec![], media: vec![] },
            events,
            status: vec![],
            next_poll: now + MONITOR_POLL,
            identify: HashMap::new(),
            clock: FrameClock::new(period(FALLBACK_MHZ), now),
            now,
            paced: false,
            presented: 0,
            dropped: 0,
            stats_since: now,
            next_positions: now,
            positions: (BTreeMap::new(), now),
            perf: Perf::new(now),
            previews,
            captures: [(); 2].map(|()| Capture { pending: None, spare: None, next: now }),
            encoders,
            mixer,
            views: HashMap::new(),
            mappings: vec![],
            taps: vec![],
            taps_sent: vec![],
        })
    }

    fn send(&self, message: FromEngine) {
        let _ = self.events.send(message); // receiver gone = nobody listening; keep running
    }

    fn command(&mut self, event_loop: &ActiveEventLoop, command: ToEngine) {
        match command {
            ToEngine::SetConfig { config } => {
                self.buses.opener().media = config.media.clone();
                let ids = config.screens.iter().map(|s| s.id).chain(config.outputs.iter().map(|o| o.id));
                self.compositor.retain(&ids.collect());
                self.config = config;
                self.apply(event_loop, true);
                // Opens media layers that couldn't open before (e.g. their media wasn't listed yet).
                let (pgm, pvw) = (self.buses.scene(Bus::Pgm).clone(), self.buses.scene(Bus::Pvw).clone());
                self.buses.set_program(pgm, self.now);
                self.buses.set_preview(pvw);
            }
            ToEngine::SetPreview { scene } => self.buses.set_preview(scene),
            ToEngine::SetProgram { scene } => self.buses.set_program(scene, self.now),
            ToEngine::Take { transition } => self.buses.take(&transition, self.now),
            ToEngine::Transport { bus, layer, action } => self.buses.transport(bus, layer, action, self.now),
            ToEngine::SetMasterVolume { volume } => match &self.mixer {
                Some(mixer) => mixer.set_master(volume),
                None => log!("master volume: no audio output"),
            },
            ToEngine::ListMonitors => self.send(FromEngine::Monitors { monitors: monitors(event_loop) }),
            ToEngine::Identify { output_id } => {
                if self.config.outputs.iter().any(|o| o.id == output_id) {
                    self.identify.insert(output_id, Instant::now() + IDENTIFY);
                } else {
                    log!("identify: unknown output {output_id}");
                }
            }
            ToEngine::Probe { .. } | ToEngine::ListInputs => log!("probe/inputs: answered by the network server (net::serve), ignored here"),
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
            log!("frame clock: {:.3} Hz", refresh_mhz as f64 / 1000.);
            self.clock = FrameClock::new(period(refresh_mhz), Instant::now());
        }
    }

    /// Forwards audio output errors; reports media positions 5 times a second while they change
    /// (once a second while they don't), presented fps and total dropped frames once a second.
    fn report(&mut self, real: Instant) {
        if real >= self.next_positions {
            self.next_positions = real + POSITIONS;
            let positions = self.buses.positions(self.now);
            if positions != self.positions.0 || real - self.positions.1 >= POSITIONS_IDLE {
                self.positions = (positions.clone(), real);
                self.send(FromEngine::Positions { positions });
            }
        }
        if let Some(message) = self.mixer.as_ref().and_then(Mixer::take_error) {
            self.send(FromEngine::Error { message });
        }
        let dropped = self.buses.take_dropped();
        self.dropped += dropped;
        let acquire = std::mem::take(&mut self.outputs.acquire);
        self.perf.add((Instant::now() - real).saturating_sub(acquire), acquire, dropped, real);
        self.presented += self.paced as u32;
        let elapsed = real - self.stats_since;
        if elapsed >= STATS {
            let fps = self.presented as f32 / elapsed.as_secs_f32();
            self.send(FromEngine::Stats { fps, dropped: self.dropped });
            (self.presented, self.stats_since) = (0, real);
        }
    }

    /// Composites every configured screen of `bus` at `self.now` into `self.views`.
    fn render_screens(&mut self, bus: Bus) {
        self.views.clear();
        for screen in &self.config.screens {
            let view = screen_view(&self.gpu, &mut self.compositor, &mut self.buses, bus, screen, self.now);
            self.views.insert(screen.id, view);
        }
    }

    /// Draws PGM to every output. Returns whether a Fifo present paced this frame.
    fn render(&mut self) -> bool {
        if self.outputs.is_empty() {
            return false;
        }
        self.render_screens(Bus::Pgm);
        // The screens go to the GPU now, so it composites while this thread waits for drawables.
        self.gpu.submit();
        let mappings = if self.identify.is_empty() {
            &self.config.mappings
        } else {
            self.mappings.clone_from(&self.config.mappings);
            for output in self.config.outputs.iter().filter(|o| self.identify.contains_key(&o.id)) {
                let (mapping, view) = identify_overlay(&self.gpu, &mut self.compositor, output);
                self.mappings.push(mapping);
                self.views.insert(output.id, view);
            }
            &self.mappings
        };
        self.outputs.render(&self.gpu, &mut self.compositor, &self.config.outputs, mappings, &self.views)
    }

    /// For each bus an MJPEG client watches, at most 15 times a second: its screens at their
    /// layout positions, `PREVIEW_WIDTH` px wide, read back for the JPEG thread. Never waits on the
    /// GPU: a readback started on one frame is collected on a later one, one in flight per bus.
    /// PVW is drawn only here, so only while watched.
    fn preview(&mut self, real: Instant) {
        let _ = self.gpu.device.poll(wgpu::PollType::Poll); // runs finished readbacks' callbacks
        // PGM first: it reuses the screens this frame drew for the outputs (`views`).
        for bus in [Bus::Pgm, Bus::Pvw] {
            let capture = &mut self.captures[bus as usize];
            if let Some(readback) = &mut capture.pending {
                match readback.try_take() {
                    None => continue,
                    Some(Ok(rgba)) => {
                        let (width, height) = readback.size();
                        let _ = self.encoders[bus as usize].try_send((width, height, rgba)); // its JPEG thread busy: skip it
                        capture.spare = capture.pending.take().map(Readback::into_buffer);
                    }
                    Some(Err(e)) => log!("preview {bus:?}: {e:#}"),
                }
                capture.pending = None;
            }
            if real < capture.next || !self.previews.watched(bus) {
                continue;
            }
            capture.next = real + PREVIEW_PERIOD;
            if bus == Bus::Pvw || self.outputs.is_empty() {
                self.render_screens(bus);
            }
            let view = self.compositor.render_layout(&self.gpu, &self.config.screens, &self.views, PREVIEW_WIDTH);
            let capture = &mut self.captures[bus as usize];
            capture.pending = Some(Readback::start_into(&self.gpu, view.texture(), capture.spare.take()));
        }
    }
}

/// One screen of `bus` at `now`; PGM mid-transition mixes the outgoing render into the incoming.
fn screen_view(
    gpu: &Gpu,
    compositor: &mut Compositor,
    buses: &mut Buses<FileOpener>,
    bus: Bus,
    screen: &Screen,
    now: Instant,
) -> wgpu::TextureView {
    let slot = match bus {
        Bus::Pgm => ScreenSlot::Pgm,
        Bus::Pvw => ScreenSlot::Pvw,
    };
    let view = compositor.render_screen(gpu, slot, screen, buses.layers(slot, screen.id));
    match buses.transition(screen.id, now).filter(|_| bus == Bus::Pgm) {
        Some((kind, p)) => {
            let layers = buses.layers(ScreenSlot::Outgoing, screen.id);
            let outgoing = compositor.render_screen(gpu, ScreenSlot::Outgoing, screen, layers);
            compositor.render_transition(gpu, screen, &outgoing, &view, &kind, p)
        }
        None => view,
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
            log!("output windows follow the config; close ignored");
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let real = Instant::now();
        if real >= self.next_poll {
            self.next_poll = real + MONITOR_POLL;
            self.apply(event_loop, false);
        }
        self.identify.retain(|_, until| *until > real);
        self.now = self.clock.tick(real, self.paced);
        // Polled with or without output windows, so errors and readiness always surface.
        for event in self.buses.tick(self.now, self.clock.period()) {
            self.send(event);
        }
        if let Some(mixer) = &self.mixer {
            self.buses.audio_taps(self.now, &mut self.taps);
            let same = |(a, x): &(AudioTap, f32), (b, y): &(AudioTap, f32)| a.same(b) && x == y;
            if self.taps.len() != self.taps_sent.len() || !self.taps.iter().zip(&self.taps_sent).all(|(a, b)| same(a, b)) {
                mixer.set_taps(self.taps.iter().cloned());
                std::mem::swap(&mut self.taps, &mut self.taps_sent);
            }
        }
        self.paced = self.render();
        self.preview(real);
        self.gpu.submit(); // what's left (PVW screens, uploads), if anything
        self.compositor.end_frame();
        self.report(real);
        let flow = if self.paced { ControlFlow::Poll } else { ControlFlow::WaitUntil(real + IDLE_TICK) };
        event_loop.set_control_flow(flow);
    }
}

fn period(refresh_mhz: u32) -> Duration {
    Duration::from_secs_f64(1000. / refresh_mhz.max(1) as f64)
}
