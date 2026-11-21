//! PVW/PGM buses over a refcounted player pool, Take with transitions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tessera_show::{
    Bus, FromEngine, Layer, MediaItem, MediaKind, Playback, Scene, Source, Transition, TransitionKind, TransportAction,
    changed_screens, resolve_shared, take_merge,
};
use uuid::Uuid;

use crate::audio::{AudioTap, lock};
use crate::compositor::{LayerTexture, ScreenSlot};
use crate::gpu::Gpu;
use crate::live::{Feed, LiveMedia};
use crate::player::{ImageSource, Player};

/// Players newly on PGM wait for each other (so they start in sync) this long at most: one that
/// never gets ready (a hung network share) must not hold the others paused.
const START_WAIT: Duration = Duration::from_secs(1);

/// What the pool drives: a video, a still image, or a test fake.
pub trait Media {
    type Frame: Clone;
    /// Every loop iteration, drawn or not: collect decode results, advance to `now`.
    fn poll(&mut self, now: Instant, period: Duration);
    /// The frame to draw (uploaded lazily); asked only for visible layers being drawn.
    fn frame(&mut self) -> Option<Self::Frame>;
    fn play(&mut self, now: Instant);
    fn pause(&mut self, now: Instant);
    fn stop(&mut self);
    /// First frame decoded, or failed: waiting longer won't change how it starts.
    fn ready(&self) -> bool;
    /// It will never show anything again: the next scene that wants it opens a new one (I1).
    fn failed(&self) -> bool;
    fn take_error(&mut self) -> Option<String>;
    fn take_dropped(&mut self) -> u64;
    fn audio(&self) -> Option<AudioTap>;
    /// Playback position; `None` for media without a timeline (stills).
    fn position_ms(&self, now: Instant) -> Option<u64>;
    /// The layer's speed, changed live (no restart). Media without a timeline ignore it.
    fn set_speed(&mut self, _speed: f32, _now: Instant) {}
}

/// Opens a layer's media. The `Err` message names the path (or the unknown media id).
pub trait Opener {
    type Media: Media;
    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Result<Self::Media, String>;
}

type Frame<O> = <<O as Opener>::Media as Media>::Frame;

/// A pool player. Every scene (PVW, PGM, a transition's outgoing) whose layers use it holds a
/// clone; it is freed once only the pool holds it.
type Handle<M> = Arc<Mutex<Entry<M>>>;

struct Entry<M> {
    media: M,
    media_id: Uuid,
    playback: Playback,
    /// The layer's trim `(in, out)` in ms: a different trim needs a different player.
    trim: (u64, Option<u64>),
    /// Newly on PGM: plays once all such players are ready.
    start: bool,
}

/// A player can stand in for `layer`: same media, playback mode and trim, not failed. (Speed is
/// changed live, so it doesn't count.)
fn fits<M: Media>(h: &Handle<M>, layer: &Layer) -> bool {
    let e = lock(h);
    layer.source == Source::Media { media_id: e.media_id }
        && layer.playback == e.playback
        && (layer.trim_in_ms, layer.trim_out_ms) == e.trim
        && !e.media.failed()
}

struct PlayerPool<O: Opener> {
    opener: O,
    players: Vec<Handle<O::Media>>,
    /// Pending starts go ahead at this time even if not every player is ready.
    start_by: Option<Instant>,
}

impl<O: Opener> PlayerPool<O> {
    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Result<Handle<O::Media>, String> {
        let media = self.opener.open(media_id, layer)?;
        let entry = Entry { media, media_id, playback: layer.playback.clone(), trim: (layer.trim_in_ms, layer.trim_out_ms), start: false };
        let handle = Arc::new(Mutex::new(entry));
        self.players.push(handle.clone());
        Ok(handle)
    }

    /// Frees the players no scene holds.
    fn gc(&mut self) {
        self.players.retain(|h| Arc::strong_count(h) > 1);
    }

    /// Plays the players waiting to start, all at `now`, once every one is ready (or time is up).
    fn start_ready(&mut self, now: Instant) {
        let Some(by) = self.start_by else { return };
        if now < by && self.players.iter().any(|h| matches!(&*lock(h), e if e.start && !e.media.ready())) {
            return;
        }
        for h in &self.players {
            let mut e = lock(h);
            if std::mem::take(&mut e.start) {
                e.media.play(now);
            }
        }
        self.start_by = None;
    }
}

/// One bus: its scene, its media layers' players (by layer id) and, per screen, the layer list the
/// compositor draws (built when the scene changes; only the frames are refreshed per draw).
struct BusState<M: Media> {
    scene: Scene,
    players: HashMap<Uuid, Handle<M>>,
    draw: HashMap<Uuid, Drawn<M::Frame>>,
}

/// A screen's layers with the frame each shows.
type Drawn<F> = Vec<(Layer, Option<F>)>;

impl<M: Media> BusState<M> {
    fn new(scene: Scene, players: HashMap<Uuid, Handle<M>>) -> Self {
        let draw = scene.iter().map(|(s, layers)| (*s, layers.iter().map(|l| (l.clone(), None)).collect())).collect();
        BusState { scene, players, draw }
    }

    fn holds(&self, h: &Handle<M>) -> bool {
        self.players.values().any(|p| Arc::ptr_eq(p, h))
    }
}

/// The PGM a Take replaced, shown until its transition ends.
struct Outgoing<M: Media> {
    bus: BusState<M>,
    kind: TransitionKind,
    start: Instant,
    duration: Duration,
    /// Only the screens whose layers changed transition.
    screens: Vec<Uuid>,
}

impl<M: Media> Outgoing<M> {
    fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()).min(1.)
    }
}

/// PVW and PGM scenes, their players, and the running transition.
pub struct Buses<O: Opener> {
    pool: PlayerPool<O>,
    pvw: BusState<O::Media>,
    pgm: BusState<O::Media>,
    outgoing: Option<Outgoing<O::Media>>,
    /// For the controller, handed out by `tick`.
    events: Vec<FromEngine>,
    dropped: u64,
    /// `audio_taps` scratch.
    gains: Vec<(Handle<O::Media>, f32)>,
    /// Open errors already reported: every SetConfig retries the layers that couldn't open, and
    /// one message per layer/path is enough (M5).
    open_errors: HashSet<String>,
    /// The last `tick`'s time: when a PVW edit changes a playing player's speed.
    now: Instant,
}

/// Each layer's speed to its player, except players `skip` names.
fn set_speeds<M: Media>(bus: &BusState<M>, now: Instant, skip: impl Fn(&Handle<M>) -> bool) {
    for l in bus.scene.values().flatten() {
        if let Some(h) = bus.players.get(&l.id).filter(|h| !skip(h)) {
            lock(h).media.set_speed(l.speed, now);
        }
    }
}

impl<O: Opener> Buses<O> {
    pub fn new(opener: O) -> Self {
        Buses {
            pool: PlayerPool { opener, players: vec![], start_by: None },
            pvw: BusState::new(Scene::new(), HashMap::new()),
            pgm: BusState::new(Scene::new(), HashMap::new()),
            outgoing: None,
            events: vec![],
            dropped: 0,
            gains: vec![],
            open_errors: HashSet::new(),
            now: Instant::now(),
        }
    }

    pub fn opener(&mut self) -> &mut O {
        &mut self.pool.opener
    }

    pub fn scene(&self, bus: Bus) -> &Scene {
        match bus {
            Bus::Pgm => &self.pgm.scene,
            Bus::Pvw => &self.pvw.scene,
        }
    }

    /// Loads PVW. A media layer reuses the PGM player at the same (screen, media, index), so PVW
    /// shows continuity and Take never restarts it; else its own PVW player (kept by layer id
    /// across PVW edits) unless that one is live, since after a Take PVW holds PGM's players and a
    /// live player is shared only by that match; else a new player, paused on its first frame
    /// until Take.
    pub fn set_preview(&mut self, scene: Scene) {
        let shared = resolve_shared(&self.pgm.scene, &scene);
        let mut players = HashMap::new();
        for layer in scene.values().flatten() {
            let Source::Media { media_id } = layer.source else { continue };
            let own = self.pvw.players.get(&layer.id).filter(|h| !self.live(h));
            let reuse = [shared.get(&layer.id).and_then(|id| self.pgm.players.get(id)), own];
            let reuse = reuse.into_iter().flatten().find(|h| fits(h, layer)).cloned();
            if let Some(h) = reuse.or_else(|| self.open(media_id, layer)) {
                players.insert(layer.id, h);
            }
        }
        self.pvw = BusState::new(scene, players);
        // A player PGM shares keeps PGM's speed until the Take (R26: PVW never touches air).
        set_speeds(&self.pvw, self.now, |h| self.live(h));
        self.pool.gc();
    }

    /// Replaces PGM without a transition (a live edit): players kept by layer id (PGM's, or PVW's);
    /// new ones play once ready.
    pub fn set_program(&mut self, scene: Scene, now: Instant) {
        let mut players = HashMap::new();
        for layer in scene.values().flatten() {
            let Source::Media { media_id } = layer.source else { continue };
            let reuse = [self.pgm.players.get(&layer.id), self.pvw.players.get(&layer.id)];
            let reuse = reuse.into_iter().flatten().find(|h| fits(h, layer)).cloned();
            if let Some(h) = reuse.or_else(|| self.open(media_id, layer)) {
                players.insert(layer.id, h);
            }
        }
        let old = std::mem::replace(&mut self.pgm, BusState::new(scene, players));
        set_speeds(&self.pgm, now, |_| false);
        self.hand_over(&old, now);
        drop(old);
        self.pool.gc();
        self.pool.start_ready(now);
    }

    /// PGM := PVW merged over PGM (screens PVW doesn't define keep their content; an empty one goes
    /// black), with PVW's players; new ones play at `now` (once all are ready). Changed screens
    /// transition from the old PGM; a Take during a transition first snaps it to its end. PVW keeps
    /// its scene, so its layers now share PGM's players.
    pub fn take(&mut self, t: &Transition, now: Instant) {
        if self.pvw.scene.is_empty() {
            return self.error("take: preview is empty, nothing to take".into());
        }
        if self.outgoing.take().is_some() {
            self.events.push(FromEngine::TakeDone);
        }
        let scene = take_merge(&self.pgm.scene, &self.pvw.scene);
        let screens = changed_screens(&self.pgm.scene, &scene);
        let mut players = HashMap::new();
        let mut failed = vec![];
        for (screen, layers) in &scene {
            let from = if self.pvw.scene.contains_key(screen) { &self.pvw } else { &self.pgm };
            for l in layers {
                match from.players.get(&l.id) {
                    Some(h) if !lock(h).media.failed() => _ = players.insert(l.id, h.clone()),
                    Some(_) => failed.push(l),
                    None => {}
                }
            }
        }
        // A failed player never shows again: a fresh one goes on air in its place (I1), and PVW,
        // which keeps its scene, shows it too.
        for l in failed {
            if let Source::Media { media_id } = l.source
                && let Some(h) = self.open(media_id, l)
            {
                if self.pvw.players.contains_key(&l.id) {
                    self.pvw.players.insert(l.id, h.clone());
                }
                players.insert(l.id, h);
            }
        }
        let old = std::mem::replace(&mut self.pgm, BusState::new(scene, players));
        set_speeds(&self.pgm, now, |_| false);
        self.hand_over(&old, now);
        log!("take: {:?} {} ms, {} screen(s) change", t.kind, t.duration_ms, screens.len());
        let duration = Duration::from_millis(t.duration_ms.into());
        if t.kind == TransitionKind::Cut || duration.is_zero() || screens.is_empty() {
            drop(old);
            self.events.push(FromEngine::TakeDone);
        } else {
            self.outgoing = Some(Outgoing { bus: old, kind: t.kind.clone(), start: now, duration, screens });
        }
        self.pool.gc();
        self.pool.start_ready(now);
    }

    /// Play/Pause/Stop/Restart one layer's player on `bus`, or (`None`) every player on it. PVW
    /// transport never touches a player live output also shows (R26): cueing in the PVW panel
    /// must not pause, stop or restart PGM.
    pub fn transport(&mut self, bus: Bus, layer: Option<Uuid>, action: TransportAction, now: Instant) {
        let state = match bus {
            Bus::Pgm => &self.pgm,
            Bus::Pvw => &self.pvw,
        };
        let mut found = false;
        for (id, h) in state.players.iter().filter(|(id, _)| layer.is_none_or(|l| l == **id)) {
            found = true;
            if bus == Bus::Pvw && self.live(h) {
                log!("transport: PVW layer {id} is shared with PGM, not touched");
                continue;
            }
            let mut e = lock(h);
            e.start = false; // the operator's call wins over a pending start
            match action {
                TransportAction::Play => e.media.play(now),
                TransportAction::Pause => e.media.pause(now),
                TransportAction::Stop => e.media.stop(),
                TransportAction::Restart => {
                    e.media.stop();
                    e.media.play(now);
                }
            }
        }
        if let (Some(id), false) = (layer, found) {
            self.error(format!("transport: no media layer {id} on {bus:?}"));
        }
    }

    /// Every loop iteration: ends a finished transition (TakeDone; players only it held are freed),
    /// polls every player whether or not anything is drawn (so errors and readiness surface) and
    /// starts new players once ready. Returns what to tell the controller.
    pub fn tick(&mut self, now: Instant, period: Duration) -> Vec<FromEngine> {
        self.now = now;
        if self.outgoing.as_ref().is_some_and(|o| now >= o.start + o.duration) {
            self.outgoing = None;
            self.events.push(FromEngine::TakeDone);
            self.pool.gc();
        }
        for h in &self.pool.players {
            let mut e = lock(h);
            e.media.poll(now, period);
            self.dropped += e.media.take_dropped();
            if let Some(message) = e.media.take_error() {
                self.events.push(FromEngine::Error { message });
            }
        }
        self.pool.start_ready(now);
        std::mem::take(&mut self.events)
    }

    /// Position (ms) of every PVW and PGM media layer that has a player, by layer id; PGM's wins
    /// where both buses use one layer id.
    pub fn positions(&self, now: Instant) -> BTreeMap<Uuid, u64> {
        let players = [&self.pvw, &self.pgm].into_iter().flat_map(|bus| &bus.players);
        players.filter_map(|(id, h)| Some((*id, lock(h).media.position_ms(now)?))).collect()
    }

    /// Frames skipped or repeated since the last call.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    /// The layers `slot` (PGM, PVW, or a transition's outgoing) draws on `screen`, each with its
    /// player's frame (`None`: hidden, a pattern, or nothing to show). `Transition` has no layers.
    pub fn layers(&mut self, slot: ScreenSlot, screen: Uuid) -> &[(Layer, Option<Frame<O>>)] {
        let state = match slot {
            ScreenSlot::Pgm => &mut self.pgm,
            ScreenSlot::Pvw => &mut self.pvw,
            ScreenSlot::Outgoing => match &mut self.outgoing {
                Some(o) => &mut o.bus,
                None => return &[],
            },
            ScreenSlot::Transition => return &[],
        };
        let Some(layers) = state.draw.get_mut(&screen) else { return &[] };
        for (layer, frame) in layers.iter_mut() {
            *frame = state.players.get(&layer.id).filter(|_| layer.visible).and_then(|h| lock(h).media.frame());
        }
        layers
    }

    /// `screen`'s running transition: (kind, progress 0..1).
    pub fn transition(&self, screen: Uuid, now: Instant) -> Option<(TransitionKind, f32)> {
        let o = self.outgoing.as_ref().filter(|o| o.screens.contains(&screen))?;
        Some((o.kind.clone(), o.progress(now)))
    }

    /// PGM's audio, refilled into `out`: each player once, at its layer volume. During a transition
    /// outgoing-only players fade by (1 − p), incoming-only by p; shared ones stay at full. Every
    /// other player (invisible layers, PVW) is listed at gain 0: silent, but still drained, since a
    /// playing player's tap left out of the mix fills to 1 s and would come back that far behind.
    pub fn audio_taps(&mut self, now: Instant, out: &mut Vec<(AudioTap, f32)>) {
        self.gains(now);
        out.clear();
        out.extend(self.gains.drain(..).filter_map(|(h, gain)| Some((lock(&h).media.audio()?, gain))));
    }

    fn gains(&mut self, now: Instant) {
        self.gains.clear();
        let fade = self.outgoing.as_ref().map(|o| (o, o.progress(now)));
        for (screen, layers) in &self.pgm.scene {
            let fading = fade.filter(|(o, _)| o.screens.contains(screen));
            for layer in layers.iter().filter(|l| l.visible) {
                let Some(h) = self.pgm.players.get(&layer.id) else { continue };
                let gain = match fading {
                    Some((o, p)) if !o.bus.holds(h) => p,
                    _ => 1.,
                };
                push_once(&mut self.gains, h, audible(layer) * gain);
            }
        }
        if let Some((o, p)) = fade {
            for layer in o.screens.iter().filter_map(|s| o.bus.scene.get(s)).flatten().filter(|l| l.visible) {
                if let Some(h) = o.bus.players.get(&layer.id) {
                    push_once(&mut self.gains, h, audible(layer) * (1. - p));
                }
            }
        }
        for h in &self.pool.players {
            push_once(&mut self.gains, h, 0.);
        }
    }

    /// PGM changed from `old`: its new players are marked to start; players that left it before
    /// starting stay paused (R27: PVW players wait for their Take).
    fn hand_over(&mut self, old: &BusState<O::Media>, now: Instant) {
        for h in self.pgm.players.values().filter(|h| !old.holds(h)) {
            lock(h).start = true;
            self.pool.start_by.get_or_insert(now + START_WAIT);
        }
        for h in old.players.values().filter(|h| !self.pgm.holds(h)) {
            lock(h).start = false;
        }
    }

    /// On an output: PGM's, or the fading-out scene's.
    fn live(&self, h: &Handle<O::Media>) -> bool {
        self.pgm.holds(h) || self.outgoing.as_ref().is_some_and(|o| o.bus.holds(h))
    }

    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Option<Handle<O::Media>> {
        match self.pool.open(media_id, layer) {
            Ok(h) => Some(h),
            Err(e) => {
                if self.open_errors.insert(e.clone()) {
                    self.error(e);
                }
                None
            }
        }
    }

    fn error(&mut self, message: String) {
        log!("{message}");
        self.events.push(FromEngine::Error { message });
    }
}

/// Adds `h` unless it's in already (a player shared by two layers counts once).
/// A layer's volume, or 0 while it is muted.
fn audible(layer: &Layer) -> f32 {
    if layer.muted { 0. } else { layer.volume }
}

fn push_once<M>(gains: &mut Vec<(Handle<M>, f32)>, h: &Handle<M>, gain: f32) {
    if !gains.iter().any(|(g, _)| Arc::ptr_eq(g, h)) {
        gains.push((h.clone(), gain));
    }
}

/// The engine's media: a video player or a still image.
pub enum FileMedia {
    Video { player: Box<Player>, gpu: Gpu },
    Image(Box<ImageSource>),
    /// A capture device: no timeline (Play is live, Pause/Stop freeze).
    Live { live: Box<LiveMedia>, gpu: Gpu },
}

impl Media for FileMedia {
    type Frame = LayerTexture;

    fn poll(&mut self, now: Instant, period: Duration) {
        match self {
            FileMedia::Video { player, .. } => player.poll(now, period),
            FileMedia::Image(image) => {
                let _ = image.texture(); // collects the loader's result (and error)
            }
            FileMedia::Live { live, .. } => live.poll(),
        }
    }

    fn frame(&mut self) -> Option<LayerTexture> {
        match self {
            FileMedia::Video { player, gpu } => player.texture(gpu),
            FileMedia::Image(image) => image.texture(),
            FileMedia::Live { live, gpu } => live.texture(gpu),
        }
    }

    fn play(&mut self, now: Instant) {
        match self {
            FileMedia::Video { player, .. } => player.play(now),
            FileMedia::Live { live, .. } => live.set_live(true),
            FileMedia::Image(_) => {}
        }
    }

    fn pause(&mut self, now: Instant) {
        match self {
            FileMedia::Video { player, .. } => player.pause(now),
            FileMedia::Live { live, .. } => live.set_live(false),
            FileMedia::Image(_) => {}
        }
    }

    fn stop(&mut self) {
        match self {
            FileMedia::Video { player, .. } => player.stop(),
            FileMedia::Live { live, .. } => live.set_live(false),
            FileMedia::Image(_) => {}
        }
    }

    fn ready(&self) -> bool {
        match self {
            FileMedia::Video { player, .. } => player.ready(),
            FileMedia::Image(_) => true, // nothing to start
            FileMedia::Live { live, .. } => live.ready(),
        }
    }

    fn failed(&self) -> bool {
        match self {
            FileMedia::Video { player, .. } => player.failed(),
            FileMedia::Image(image) => image.failed(),
            FileMedia::Live { live, .. } => live.failed(),
        }
    }

    fn take_error(&mut self) -> Option<String> {
        match self {
            FileMedia::Video { player, .. } => player.take_error(),
            FileMedia::Image(image) => image.take_error(),
            FileMedia::Live { live, .. } => live.take_error(),
        }
    }

    fn take_dropped(&mut self) -> u64 {
        match self {
            FileMedia::Video { player, .. } => player.take_dropped(),
            FileMedia::Image(_) | FileMedia::Live { .. } => 0,
        }
    }

    fn audio(&self) -> Option<AudioTap> {
        match self {
            FileMedia::Video { player, .. } => player.audio(),
            FileMedia::Image(_) | FileMedia::Live { .. } => None,
        }
    }

    fn position_ms(&self, now: Instant) -> Option<u64> {
        match self {
            FileMedia::Video { player, .. } => Some(player.position_ms(now)),
            FileMedia::Image(_) | FileMedia::Live { .. } => None,
        }
    }

    fn set_speed(&mut self, speed: f32, now: Instant) {
        if let FileMedia::Video { player, .. } = self {
            player.set_speed(speed, now);
        }
    }
}

/// Opens config media: videos as `Player`s (audio resampled to the mixer's format), images as
/// `ImageSource`s. Neither blocks (R22); open and decode errors surface through `take_error`.
pub struct FileOpener {
    gpu: Gpu,
    /// The mixer's (sample rate, channels); `None`: no audio.
    audio: Option<(u32, u16)>,
    pub media: Vec<MediaItem>,
    /// Open live inputs by (media id, device id): layers showing one input share its session.
    feeds: HashMap<(Uuid, String), Weak<Feed>>,
}

impl FileOpener {
    pub fn new(gpu: Gpu, audio: Option<(u32, u16)>) -> FileOpener {
        FileOpener { gpu, audio, media: vec![], feeds: HashMap::new() }
    }

    /// The input's running feed, or a new one (`item` is a live kind).
    fn feed(&mut self, item: &MediaItem) -> anyhow::Result<Arc<Feed>> {
        let key = (item.id, item.path.clone());
        if let Some(feed) = self.feeds.get(&key).and_then(Weak::upgrade).filter(|f| f.running()) {
            return Ok(feed);
        }
        self.feeds.retain(|_, f| f.strong_count() > 0);
        let id = item.path.clone();
        let feed = if item.kind == MediaKind::Ndi {
            Feed::spawn(&item.name, Box::new(move || crate::ndi::open(&id)))?
        } else {
            Feed::spawn(&item.name, Box::new(move || crate::capture::open(&id)))?
        };
        self.feeds.insert(key, Arc::downgrade(&feed));
        Ok(feed)
    }
}

impl Opener for FileOpener {
    type Media = FileMedia;

    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Result<FileMedia, String> {
        let item = self.media.iter().find(|m| m.id == media_id).cloned();
        let item = item.ok_or_else(|| format!("layer {}: media {media_id} is not in the config", layer.id))?;
        let path = Path::new(&item.path);
        let media = match item.kind {
            MediaKind::Video => Player::open_trimmed(path, layer.playback.clone(), self.audio, layer.trim_in_ms, layer.trim_out_ms)
                .map(|player| FileMedia::Video { player: Box::new(player), gpu: self.gpu.clone() }),
            MediaKind::Image => ImageSource::open(&self.gpu, path).map(|image| FileMedia::Image(Box::new(image))),
            MediaKind::Capture | MediaKind::Ndi => self.feed(&item).map(|feed| FileMedia::Live { live: Box::new(LiveMedia::new(feed)), gpu: self.gpu.clone() }),
        };
        media.map_err(|e| format!("{}: {e:#}", item.path))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Weak;

    use tessera_show::{Pattern, Rect, TransitionKind};

    use super::*;

    const PERIOD: Duration = Duration::from_millis(16);

    struct Fake {
        media: Uuid,
        playing: bool,
        ready: bool,
        failed: bool,
        error: Option<String>,
        stops: u32,
        tap: AudioTap,
        speed: f32,
    }

    impl Media for Fake {
        type Frame = Uuid;
        fn poll(&mut self, _: Instant, _: Duration) {}
        fn frame(&mut self) -> Option<Uuid> {
            (self.ready && !self.failed).then_some(self.media)
        }
        fn play(&mut self, _: Instant) {
            self.playing = true;
        }
        fn pause(&mut self, _: Instant) {
            self.playing = false;
        }
        fn stop(&mut self) {
            (self.playing, self.stops) = (false, self.stops + 1);
        }
        fn ready(&self) -> bool {
            self.ready || self.failed
        }
        fn failed(&self) -> bool {
            self.failed
        }
        fn take_error(&mut self) -> Option<String> {
            self.error.take()
        }
        fn take_dropped(&mut self) -> u64 {
            0
        }
        fn audio(&self) -> Option<AudioTap> {
            Some(self.tap.clone())
        }
        fn position_ms(&self, _: Instant) -> Option<u64> {
            Some(self.playing as u64) // 1: playing, 0: paused
        }
        fn set_speed(&mut self, speed: f32, _: Instant) {
            self.speed = speed;
        }
    }

    /// Media ids in `fail` don't open; `broken` open but fail (as a missing file does in a real
    /// player); `slow` aren't ready until a test says so.
    #[derive(Default)]
    struct FakeOpener {
        fail: HashSet<Uuid>,
        broken: HashSet<Uuid>,
        slow: HashSet<Uuid>,
    }

    fn path(media: Uuid) -> String {
        format!("/media/{media}.mp4")
    }

    impl Opener for FakeOpener {
        type Media = Fake;
        fn open(&mut self, media: Uuid, _: &Layer) -> Result<Fake, String> {
            if self.fail.contains(&media) {
                return Err(format!("{}: cannot open", path(media)));
            }
            let failed = self.broken.contains(&media);
            let error = failed.then(|| format!("{}: decode error", path(media)));
            let (ready, tap) = (!self.slow.contains(&media), AudioTap::new((48_000, 2)));
            Ok(Fake { media, playing: false, ready, failed, error, stops: 0, tap, speed: 1. })
        }
    }

    fn buses() -> Buses<FakeOpener> {
        Buses::new(FakeOpener::default())
    }

    fn clip(media: Uuid) -> Layer {
        Layer::new(Source::Media { media_id: media }, Rect { x: 0., y: 0., w: 10., h: 10. })
    }

    fn bars() -> Layer {
        Layer::new(Source::Pattern { pattern: Pattern::Bars }, Rect { x: 0., y: 0., w: 10., h: 10. })
    }

    fn ids<const N: usize>() -> [Uuid; N] {
        std::array::from_fn(|_| Uuid::new_v4())
    }

    fn fade(ms: u32) -> Transition {
        Transition { kind: TransitionKind::Fade, duration_ms: ms }
    }

    fn cut() -> Transition {
        Transition { kind: TransitionKind::Cut, duration_ms: 0 }
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn pgm(b: &Buses<FakeOpener>, layer: &Layer) -> Handle<Fake> {
        b.pgm.players[&layer.id].clone()
    }

    fn pvw(b: &Buses<FakeOpener>, layer: &Layer) -> Handle<Fake> {
        b.pvw.players[&layer.id].clone()
    }

    fn playing(h: &Handle<Fake>) -> bool {
        lock(h).media.playing
    }

    fn take_dones(events: &[FromEngine]) -> usize {
        events.iter().filter(|e| **e == FromEngine::TakeDone).count()
    }

    fn frames(b: &mut Buses<FakeOpener>, slot: ScreenSlot, screen: Uuid) -> Vec<Option<Uuid>> {
        b.layers(slot, screen).iter().map(|(_, f)| *f).collect()
    }

    /// A trim change needs a seek and a new length: a new player.
    #[test]
    fn trim_change_opens_a_new_player() {
        let [a, m1] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let mut l = clip(m1);
        b.set_program(Scene::from([(a, vec![l.clone()])]), t0);
        let first = pgm(&b, &l);
        l.trim_in_ms = 500;
        b.set_program(Scene::from([(a, vec![l.clone()])]), t0);
        assert!(!Arc::ptr_eq(&first, &pgm(&b, &l)), "new trim, new player");
        let second = pgm(&b, &l);
        l.trim_out_ms = Some(900);
        b.set_program(Scene::from([(a, vec![l.clone()])]), t0);
        assert!(!Arc::ptr_eq(&second, &pgm(&b, &l)), "new out point, new player");
    }

    /// Speed is live: a change keeps the player. A PVW edit leaves a player PGM shares at PGM's
    /// speed until the Take.
    #[test]
    fn speed_changes_keep_the_player() {
        let [a, m1] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let mut l = clip(m1);
        b.set_program(Scene::from([(a, vec![l.clone()])]), t0);
        let h = pgm(&b, &l);
        l.speed = 2.;
        b.set_program(Scene::from([(a, vec![l.clone()])]), t0);
        assert!(Arc::ptr_eq(&h, &pgm(&b, &l)), "same player");
        assert_eq!(lock(&h).media.speed, 2.);
        let mut p = clip(m1); // same screen, media and index: PVW shares PGM's player
        p.speed = 0.5;
        b.set_preview(Scene::from([(a, vec![p.clone()])]));
        assert!(Arc::ptr_eq(&h, &pvw(&b, &p)));
        assert_eq!(lock(&h).media.speed, 2., "PGM's speed until the Take");
        b.take(&fade(0), t0);
        assert_eq!(lock(&h).media.speed, 0.5);
    }

    #[test]
    fn shared_player_identity_across_take() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let bg = clip(m1);
        b.set_program(Scene::from([(a, vec![bg.clone()])]), t0);
        let live = pgm(&b, &bg);
        assert!(playing(&live), "new on PGM: plays");
        let (bg2, overlay) = (clip(m1), clip(m2));
        b.set_preview(Scene::from([(a, vec![bg2.clone(), overlay.clone()])]));
        assert!(Arc::ptr_eq(&pvw(&b, &bg2), &live), "PVW shows the running PGM player");
        b.take(&fade(1000), t0);
        assert!(Arc::ptr_eq(&pgm(&b, &bg2), &live), "Take keeps it");
        b.tick(t0 + ms(1000), PERIOD);
        assert!(Arc::ptr_eq(&pgm(&b, &bg2), &live), "and so does the end of the transition");
        assert!(Arc::ptr_eq(&pvw(&b, &bg2), &live), "PVW keeps its scene, resolved to PGM's players");
        assert!(Arc::ptr_eq(&pvw(&b, &overlay), &pgm(&b, &overlay)));
        let fake = lock(&live);
        assert!(fake.media.playing && fake.media.stops == 0, "shared media never restarts");
        drop(fake);
        assert_eq!(b.pool.players.len(), 2, "the old bg layer's player is the shared one");
    }

    #[test]
    fn new_pvw_players_paused_until_take() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let bg = clip(m1);
        b.set_program(Scene::from([(a, vec![bg])]), t0);
        let (bg2, overlay) = (clip(m1), clip(m2));
        b.set_preview(Scene::from([(a, vec![bg2.clone(), overlay.clone()])]));
        b.tick(t0 + ms(500), PERIOD);
        assert!(playing(&pvw(&b, &bg2)), "shared: keeps running");
        assert!(!playing(&pvw(&b, &overlay)), "new in PVW: paused");
        assert_eq!(frames(&mut b, ScreenSlot::Pvw, a), [Some(m1), Some(m2)], "first frame shown while paused");
        b.take(&cut(), t0 + ms(500));
        assert!(playing(&pgm(&b, &overlay)), "starts on Take");
    }

    #[test]
    fn new_players_start_together_once_ready() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        b.pool.opener.slow.insert(m2);
        let (x, y) = (clip(m1), clip(m2));
        b.set_preview(Scene::from([(a, vec![x.clone(), y.clone()])]));
        b.take(&cut(), t0);
        assert!(!playing(&pgm(&b, &x)) && !playing(&pgm(&b, &y)), "wait for the slow one");
        lock(&pgm(&b, &y)).media.ready = true;
        b.tick(t0 + ms(100), PERIOD);
        assert!(playing(&pgm(&b, &x)) && playing(&pgm(&b, &y)), "then start together");

        // One that never gets ready (a hung network share) doesn't hold the others forever.
        let [m3, m4] = ids();
        b.pool.opener.slow.insert(m4);
        let (w, z) = (clip(m3), clip(m4));
        b.set_preview(Scene::from([(a, vec![w.clone(), z.clone()])]));
        b.take(&cut(), t0 + ms(200));
        b.tick(t0 + ms(200) + START_WAIT / 2, PERIOD);
        assert!(!playing(&pgm(&b, &w)));
        b.tick(t0 + ms(200) + START_WAIT, PERIOD);
        assert!(playing(&pgm(&b, &w)), "started after {START_WAIT:?}");
    }

    #[test]
    fn undefined_screen_untouched_by_take() {
        let [a, s, m1, m2, m3] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (la, lb) = (clip(m1), clip(m2));
        b.set_program(Scene::from([(a, vec![la]), (s, vec![lb.clone()])]), t0);
        let player_b = pgm(&b, &lb);
        b.set_preview(Scene::from([(a, vec![clip(m3)])]));
        b.take(&fade(1000), t0);
        assert_eq!(b.scene(Bus::Pgm)[&s], vec![lb.clone()]);
        assert!(Arc::ptr_eq(&pgm(&b, &lb), &player_b));
        assert_eq!(b.transition(s, t0 + ms(500)), None, "no transition on an unchanged screen");
        assert_eq!(b.transition(a, t0 + ms(500)), Some((TransitionKind::Fade, 0.5)));
        b.tick(t0 + ms(1000), PERIOD);
        assert!(Arc::ptr_eq(&pgm(&b, &lb), &player_b) && playing(&player_b));
    }

    #[test]
    fn explicit_empty_screen_goes_black() {
        let [a, s, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (la, lb) = (clip(m1), clip(m2));
        b.set_program(Scene::from([(a, vec![la.clone()]), (s, vec![lb.clone()])]), t0);
        let gone = Arc::downgrade(&pgm(&b, &lb));
        b.set_preview(Scene::from([(s, vec![])]));
        b.take(&fade(1000), t0);
        assert_eq!(b.scene(Bus::Pgm)[&s], vec![]);
        assert_eq!(b.scene(Bus::Pgm)[&a], vec![la]);
        assert!(b.layers(ScreenSlot::Pgm, s).is_empty(), "black");
        assert_eq!(frames(&mut b, ScreenSlot::Outgoing, s), [Some(m2)], "fading out from the old layers");
        assert!(b.transition(s, t0 + ms(10)).is_some());
        b.tick(t0 + ms(1000), PERIOD);
        assert!(gone.upgrade().is_none(), "freed with the outgoing scene");
    }

    #[test]
    fn players_freed_after_transition() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (x, y) = (clip(m1), clip(m2));
        b.set_program(Scene::from([(a, vec![x.clone()])]), t0);
        let old: Weak<_> = Arc::downgrade(&pgm(&b, &x));
        b.set_preview(Scene::from([(a, vec![y])]));
        b.take(&fade(1000), t0);
        assert_eq!(take_dones(&b.tick(t0 + ms(500), PERIOD)), 0);
        assert!(old.upgrade().is_some(), "kept while the transition shows it");
        assert_eq!(frames(&mut b, ScreenSlot::Outgoing, a), [Some(m1)]);
        assert_eq!(take_dones(&b.tick(t0 + ms(1000), PERIOD)), 1);
        assert!(old.upgrade().is_none(), "freed at the end");
        assert_eq!(b.pool.players.len(), 1);
        assert!(b.transition(a, t0 + ms(1000)).is_none() && b.layers(ScreenSlot::Outgoing, a).is_empty());
    }

    #[test]
    fn take_during_transition_snaps() {
        let [a, m1, m2, m3] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (x, y, z) = (clip(m1), clip(m2), clip(m3));
        b.set_program(Scene::from([(a, vec![x.clone()])]), t0);
        let first_out = Arc::downgrade(&pgm(&b, &x));
        b.set_preview(Scene::from([(a, vec![y.clone()])]));
        b.take(&fade(1000), t0);
        let second_out = Arc::downgrade(&pgm(&b, &y));
        b.set_preview(Scene::from([(a, vec![z.clone()])]));
        b.take(&fade(1000), t0 + ms(500));
        assert!(first_out.upgrade().is_none(), "the first transition's outgoing is freed at once");
        assert_eq!(b.transition(a, t0 + ms(500)), Some((TransitionKind::Fade, 0.)), "one transition, restarted");
        assert_eq!(frames(&mut b, ScreenSlot::Outgoing, a), [Some(m2)]);
        assert_eq!(b.pool.players.len(), 2);
        assert_eq!(take_dones(&b.tick(t0 + ms(600), PERIOD)), 1, "the snapped Take is done");
        assert_eq!(take_dones(&b.tick(t0 + ms(1500), PERIOD)), 1);
        assert!(second_out.upgrade().is_none());
        assert_eq!(b.pool.players.len(), 1);
    }

    #[test]
    fn failed_player_does_not_block_take() {
        let [a, bad, broken, good] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        b.pool.opener.fail.insert(bad);
        b.pool.opener.broken.insert(broken);
        let ok = clip(good);
        b.set_preview(Scene::from([(a, vec![clip(bad), clip(broken), ok.clone(), bars()])]));
        b.take(&fade(1000), t0);
        assert!(playing(&pgm(&b, &ok)), "a failed player counts as ready");
        let events = b.tick(t0 + ms(1000), PERIOD);
        assert_eq!(take_dones(&events), 1);
        for media in [bad, broken] {
            let named = |e: &FromEngine| matches!(e, FromEngine::Error { message } if message.contains(&path(media)));
            assert!(events.iter().any(named), "error names {}: {events:?}", path(media));
        }
        assert_eq!(frames(&mut b, ScreenSlot::Pgm, a), [None, None, Some(good), None], "failed layers transparent");
    }

    /// I1: a failed player is never reused: reloading PVW, Take, SetProgram (and so SetConfig,
    /// which re-sends both scenes) open a new one.
    #[test]
    fn failed_player_is_reopened() {
        let [a, m] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        b.pool.opener.broken.insert(m);
        let x = clip(m);
        let scene = Scene::from([(a, vec![x.clone()])]);
        b.set_preview(scene.clone());
        let first = pvw(&b, &x);
        b.set_preview(scene.clone());
        let failed = pvw(&b, &x);
        assert!(!Arc::ptr_eq(&failed, &first), "reloading PVW: a new player");
        // The file is back: Take puts a working player on air, and PVW shows it too.
        b.pool.opener.broken.remove(&m);
        b.take(&cut(), t0);
        let live = pgm(&b, &x);
        assert!(!Arc::ptr_eq(&live, &failed) && !lock(&live).media.failed && playing(&live));
        assert!(Arc::ptr_eq(&pvw(&b, &x), &live));
        drop((first, failed));
        // It fails on air: the next SetProgram replaces it, and the new one plays.
        lock(&live).media.failed = true;
        b.set_program(b.scene(Bus::Pgm).clone(), t0 + ms(100));
        let again = pgm(&b, &x);
        assert!(!Arc::ptr_eq(&again, &live) && playing(&again));
        // A working player is kept as before.
        b.set_program(b.scene(Bus::Pgm).clone(), t0 + ms(200));
        assert!(Arc::ptr_eq(&pgm(&b, &x), &again));
    }

    /// M5: every SetConfig retries layers that couldn't open; the error is reported once.
    #[test]
    fn open_errors_reported_once() {
        let [a, m] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        b.pool.opener.fail.insert(m);
        let scene = Scene::from([(a, vec![clip(m)])]);
        for _ in 0..3 {
            b.set_program(scene.clone(), t0);
            b.set_preview(scene.clone());
        }
        let events = b.tick(t0, PERIOD);
        assert_eq!(events.iter().filter(|e| matches!(e, FromEngine::Error { .. })).count(), 1, "{events:?}");
    }

    #[test]
    fn take_with_empty_preview_is_an_error() {
        let t0 = Instant::now();
        let mut b = buses();
        b.take(&cut(), t0);
        let events = b.tick(t0, PERIOD);
        assert!(matches!(events[..], [FromEngine::Error { .. }]), "{events:?}");
    }

    #[test]
    fn transport_targets_bus_players() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (x, y) = (clip(m1), clip(m2));
        b.set_program(Scene::from([(a, vec![x.clone(), y.clone()])]), t0);
        b.transport(Bus::Pgm, Some(x.id), TransportAction::Pause, t0);
        assert!(!playing(&pgm(&b, &x)) && playing(&pgm(&b, &y)));
        b.transport(Bus::Pgm, None, TransportAction::Restart, t0);
        assert!(playing(&pgm(&b, &x)) && lock(&pgm(&b, &y)).media.stops == 1);
        b.transport(Bus::Pvw, Some(x.id), TransportAction::Play, t0);
        b.transport(Bus::Pgm, Some(Uuid::new_v4()), TransportAction::Play, t0);
        let events = b.tick(t0, PERIOD);
        assert_eq!(events.iter().filter(|e| matches!(e, FromEngine::Error { .. })).count(), 2, "{events:?}");
    }

    #[test]
    fn pvw_transport_never_touches_pgm_player() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let bg = clip(m1);
        b.set_program(Scene::from([(a, vec![bg.clone()])]), t0);
        let (bg2, overlay) = (clip(m1), clip(m2));
        b.set_preview(Scene::from([(a, vec![bg2.clone(), overlay.clone()])]));
        let live = pgm(&b, &bg);
        for action in [TransportAction::Pause, TransportAction::Stop, TransportAction::Restart] {
            b.transport(Bus::Pvw, Some(bg2.id), action, t0);
            b.transport(Bus::Pvw, None, action, t0);
        }
        assert!(playing(&live) && lock(&live).media.stops == 0, "live output untouched");
        b.transport(Bus::Pvw, None, TransportAction::Play, t0);
        assert!(playing(&pvw(&b, &overlay)), "a PVW-only player plays");
        b.transport(Bus::Pvw, None, TransportAction::Pause, t0);
        assert!(!playing(&pvw(&b, &overlay)), "and pauses");
        assert!(playing(&live));
        let events = b.tick(t0, PERIOD);
        assert!(events.is_empty(), "skipping a shared player is not an error: {events:?}");
    }

    /// R41: the operator pauses live clips by the ids the controller's PGM scene holds: the taken
    /// cue's layer ids (a player carried over by sharing is re-keyed by Take), and an earlier cue's
    /// ids on a screen the taken cue doesn't define.
    #[test]
    fn pgm_transport_reaches_shared_and_kept_players() {
        let [a, c, m1, m2, m3] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (bg, side) = (clip(m1), clip(m3));
        b.set_preview(Scene::from([(a, vec![bg.clone()]), (c, vec![side.clone()])]));
        b.take(&cut(), t0);
        let (live, kept) = (pgm(&b, &bg), pgm(&b, &side));
        let (bg2, overlay) = (clip(m1), clip(m2)); // the next cue: same bg (shared), screen c untouched
        b.set_preview(Scene::from([(a, vec![bg2.clone(), overlay.clone()])]));
        b.take(&fade(500), t0);
        assert!(Arc::ptr_eq(&pgm(&b, &bg2), &live), "bg carried over by sharing");
        b.transport(Bus::Pgm, Some(bg2.id), TransportAction::Pause, t0);
        assert!(!playing(&live), "the taken cue's id reaches the shared player");
        b.transport(Bus::Pgm, Some(side.id), TransportAction::Stop, t0);
        assert!(!playing(&kept) && lock(&kept).media.stops == 1, "an earlier cue's id reaches its kept player");
        b.transport(Bus::Pvw, Some(bg2.id), TransportAction::Play, t0);
        assert!(!playing(&live), "R26: PVW transport still never touches it");
        let events = b.tick(t0, PERIOD);
        assert!(!events.iter().any(|e| matches!(e, FromEngine::Error { .. })), "{events:?}");
    }

    #[test]
    fn pending_start_cancelled_when_player_leaves_pgm() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        b.pool.opener.slow.insert(m1);
        let x = clip(m1);
        b.set_preview(Scene::from([(a, vec![x.clone()])]));
        b.take(&cut(), t0);
        let player = pgm(&b, &x);
        assert!(lock(&player).start, "waiting to start");
        b.set_program(Scene::new(), t0); // gone from PGM; PVW still holds it
        lock(&player).media.ready = true;
        b.tick(t0 + ms(100), PERIOD);
        b.tick(t0 + START_WAIT * 2, PERIOD);
        assert!(!playing(&player), "SetProgram dropped it: stays paused in PVW");

        // A Take that replaces it: it fades out paused instead of starting in the outgoing scene.
        b.pool.opener.slow.insert(m2);
        let y = clip(m2);
        b.set_preview(Scene::from([(a, vec![y.clone()])]));
        b.take(&cut(), t0 + ms(200));
        let leaving = pgm(&b, &y);
        b.set_preview(Scene::from([(a, vec![bars()])]));
        b.take(&fade(1000), t0 + ms(300));
        lock(&leaving).media.ready = true;
        b.tick(t0 + ms(400), PERIOD);
        assert!(!playing(&leaving) && !lock(&leaving).start);
    }

    #[test]
    fn pvw_edit_never_borrows_live_player() {
        // After a Take PVW holds PGM's players; a PVW edit that breaks (screen, media, index) must
        // get its own paused player, not keep the live one under the same layer id.
        let [a, s, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let bg = clip(m1);
        b.set_preview(Scene::from([(a, vec![bg.clone()])]));
        b.take(&cut(), t0);
        let live = pgm(&b, &bg);
        b.set_preview(Scene::from([(a, vec![bg.clone()])]));
        assert!(Arc::ptr_eq(&pvw(&b, &bg), &live), "unchanged: shared");
        b.set_preview(Scene::from([(s, vec![bg.clone()])]));
        assert!(!Arc::ptr_eq(&pvw(&b, &bg), &live) && !playing(&pvw(&b, &bg)), "moved to another screen: new, paused");
        b.set_preview(Scene::from([(a, vec![bg.clone()])]));
        assert!(Arc::ptr_eq(&pvw(&b, &bg), &live), "back in place: shared again");
        b.set_preview(Scene::from([(a, vec![clip(m2), bg.clone()])]));
        let fresh = pvw(&b, &bg);
        assert!(!Arc::ptr_eq(&fresh, &live) && !playing(&fresh), "a layer inserted below: new, paused");
        assert!(playing(&live), "the live one plays on");
        let old = Arc::downgrade(&live);
        drop(live);
        b.take(&cut(), t0 + ms(100));
        assert!(Arc::ptr_eq(&pgm(&b, &bg), &fresh) && playing(&fresh), "the Take starts it from the top");
        assert!(old.upgrade().is_none());
    }

    #[test]
    fn muted_taps_keep_draining() {
        // A playing player left out of the mixer's list fills its tap to 1 s and comes back that
        // far behind, so hidden and PVW-only players are listed at gain 0 (silent, still drained).
        let [a, m1, m2, m3] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (shown, mut hidden, cued) = (clip(m1), clip(m2), clip(m3));
        hidden.visible = false;
        b.set_program(Scene::from([(a, vec![shown.clone(), hidden.clone()])]), t0);
        b.set_preview(Scene::from([(a, vec![cued.clone()])]));
        b.transport(Bus::Pvw, None, TransportAction::Play, t0);
        // Tell the taps apart by how much each holds.
        let taps = [(pgm(&b, &shown), 10), (pgm(&b, &hidden), 20), (pvw(&b, &cued), 30)];
        for (h, n) in &taps {
            let fake = lock(h);
            fake.media.tap.push(0, &vec![0.5; *n]);
            fake.media.tap.set_playing(true);
        }
        let mut out = vec![];
        b.audio_taps(t0, &mut out);
        let mut listed: Vec<(usize, f32)> = out.iter().map(|(tap, gain)| (tap.buffered(), *gain)).collect();
        listed.sort_by_key(|(n, _)| *n);
        assert_eq!(listed, [(10, 1.), (20, 0.), (30, 0.)], "hidden and PVW-only listed, muted");
        let mut mix = [0.; 4];
        for (tap, gain) in &out {
            tap.mix(&mut mix, *gain);
        }
        assert_eq!(mix, [0.5; 4], "only the shown layer is heard");
        let drained: Vec<usize> = taps.iter().map(|(h, _)| lock(h).media.tap.buffered()).collect();
        assert_eq!(drained, [6, 16, 26], "every playing tap keeps draining");
    }

    #[test]
    fn audio_gains_follow_transition() {
        let [a, s, m1, m2, m3, m4, m5] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (shared, mut out, mut kept) = (clip(m1), clip(m2), clip(m4));
        (out.volume, kept.muted) = (0.8, true);
        b.set_program(Scene::from([(a, vec![shared.clone(), out.clone()]), (s, vec![kept.clone()])]), t0);
        let (mut shared2, mut inc, mut hidden) = (clip(m1), clip(m3), clip(m5));
        (shared2.volume, inc.volume, hidden.visible) = (0.9, 0.5, false);
        b.set_preview(Scene::from([(a, vec![shared2.clone(), inc.clone(), hidden.clone()])]));
        let out_player = pgm(&b, &out);
        b.take(&fade(1000), t0);
        let gain = |b: &Buses<FakeOpener>, h: &Handle<Fake>| -> Vec<f32> {
            b.gains.iter().filter(|(g, _)| Arc::ptr_eq(g, h)).map(|(_, v)| *v).collect()
        };
        b.gains(t0 + ms(250));
        assert_eq!(gain(&b, &pgm(&b, &shared2)), [0.9], "shared: once, at full (incoming) volume");
        assert_eq!(gain(&b, &pgm(&b, &inc)), [0.5 * 0.25], "incoming only: × p");
        assert_eq!(gain(&b, &out_player), [0.8 * 0.75], "outgoing only: × (1 − p)");
        assert_eq!(gain(&b, &pgm(&b, &kept)), [0.], "unchanged screen, muted layer: silent but listed (drains)");
        assert_eq!(gain(&b, &pgm(&b, &hidden)), [0.], "invisible: muted");
        assert_eq!(b.gains.len(), 5);
        // The test's clones would keep it in the pool (`audio_taps` drains `gains` at once).
        drop(out_player);
        b.gains.clear();
        b.tick(t0 + ms(1000), PERIOD);
        b.gains(t0 + ms(1000));
        assert_eq!(b.gains.len(), 4, "the outgoing layer is gone at the end");
        let pvw_only = clip(Uuid::new_v4());
        b.set_preview(Scene::from([(a, vec![pvw_only.clone()])]));
        b.gains(t0 + ms(1000));
        assert_eq!(gain(&b, &pvw(&b, &pvw_only)), [0.], "PVW is silent");
    }

    #[test]
    fn positions_cover_pgm_and_pvw_media_layers() {
        let [a, m1, m2] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (live, cued) = (clip(m1), clip(m2));
        b.set_program(Scene::from([(a, vec![live.clone(), bars()])]), t0);
        b.set_preview(Scene::from([(a, vec![cued.clone()])]));
        let want = BTreeMap::from([(live.id, 1), (cued.id, 0)]);
        assert_eq!(b.positions(t0), want, "PGM's (playing) and PVW's (paused) media layers; patterns have none");
    }
}
