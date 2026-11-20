//! PVW/PGM buses over a refcounted player pool, Take with transitions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tessera_show::{
    Bus, FromEngine, Layer, MediaItem, MediaKind, Playback, Scene, Source, Transition, TransitionKind, TransportAction,
    changed_screens, resolve_shared, take_merge,
};
use uuid::Uuid;

use crate::audio::{AudioTap, lock};
use crate::compositor::{LayerTexture, ScreenSlot};
use crate::gpu::Gpu;
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
    fn take_error(&mut self) -> Option<String>;
    fn take_dropped(&mut self) -> u64;
    fn audio(&self) -> Option<AudioTap>;
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
    /// Newly on PGM: plays once all such players are ready.
    start: bool,
}

/// A player can stand in for `layer`: same media, same playback mode.
fn fits<M>(h: &Handle<M>, layer: &Layer) -> bool {
    let e = lock(h);
    layer.source == Source::Media { media_id: e.media_id } && layer.playback == e.playback
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
        let entry = Entry { media, media_id, playback: layer.playback.clone(), start: false };
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
    /// across PVW edits); else a new player, paused on its first frame until Take.
    pub fn set_preview(&mut self, scene: Scene) {
        let shared = resolve_shared(&self.pgm.scene, &scene);
        let mut players = HashMap::new();
        for layer in scene.values().flatten() {
            let Source::Media { media_id } = layer.source else { continue };
            let reuse = [shared.get(&layer.id).and_then(|id| self.pgm.players.get(id)), self.pvw.players.get(&layer.id)];
            let reuse = reuse.into_iter().flatten().find(|h| fits(h, layer)).cloned();
            if let Some(h) = reuse.or_else(|| self.open(media_id, layer)) {
                players.insert(layer.id, h);
            }
        }
        self.pvw = BusState::new(scene, players);
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
        self.start_new(&old, now);
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
        for (screen, layers) in &scene {
            let from = if self.pvw.scene.contains_key(screen) { &self.pvw } else { &self.pgm };
            players.extend(layers.iter().filter_map(|l| Some((l.id, from.players.get(&l.id)?.clone()))));
        }
        let old = std::mem::replace(&mut self.pgm, BusState::new(scene, players));
        self.start_new(&old, now);
        eprintln!("take: {:?} {} ms, {} screen(s) change", t.kind, t.duration_ms, screens.len());
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

    /// Play/Pause/Stop/Restart one layer's player on `bus`, or (`None`) every player on it.
    pub fn transport(&mut self, bus: Bus, layer: Option<Uuid>, action: TransportAction, now: Instant) {
        let state = match bus {
            Bus::Pgm => &self.pgm,
            Bus::Pvw => &self.pvw,
        };
        let mut found = false;
        for (_, h) in state.players.iter().filter(|(id, _)| layer.is_none_or(|l| l == **id)) {
            found = true;
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

    /// PGM's audio, refilled into `out`: each player once, at its layer volume; invisible layers
    /// muted, PVW silent. During a transition outgoing-only players fade by (1 − p), incoming-only
    /// by p; shared ones stay at full.
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
                push_once(&mut self.gains, h, layer.volume * gain);
            }
        }
        if let Some((o, p)) = fade {
            for layer in o.screens.iter().filter_map(|s| o.bus.scene.get(s)).flatten().filter(|l| l.visible) {
                if let Some(h) = o.bus.players.get(&layer.id) {
                    push_once(&mut self.gains, h, layer.volume * (1. - p));
                }
            }
        }
    }

    /// Marks PGM's players that `old` didn't have to start.
    fn start_new(&mut self, old: &BusState<O::Media>, now: Instant) {
        for h in self.pgm.players.values().filter(|h| !old.holds(h)) {
            lock(h).start = true;
            self.pool.start_by.get_or_insert(now + START_WAIT);
        }
    }

    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Option<Handle<O::Media>> {
        self.pool.open(media_id, layer).map_err(|e| self.error(e)).ok()
    }

    fn error(&mut self, message: String) {
        eprintln!("{message}");
        self.events.push(FromEngine::Error { message });
    }
}

/// Adds `h` unless it's in already (a player shared by two layers counts once).
fn push_once<M>(gains: &mut Vec<(Handle<M>, f32)>, h: &Handle<M>, gain: f32) {
    if !gains.iter().any(|(g, _)| Arc::ptr_eq(g, h)) {
        gains.push((h.clone(), gain));
    }
}

/// The engine's media: a video player or a still image.
pub enum FileMedia {
    Video { player: Box<Player>, gpu: Gpu },
    Image(Box<ImageSource>),
}

impl Media for FileMedia {
    type Frame = LayerTexture;

    fn poll(&mut self, now: Instant, period: Duration) {
        match self {
            FileMedia::Video { player, .. } => player.poll(now, period),
            FileMedia::Image(image) => {
                let _ = image.texture(); // collects the loader's result (and error)
            }
        }
    }

    fn frame(&mut self) -> Option<LayerTexture> {
        match self {
            FileMedia::Video { player, gpu } => player.texture(gpu),
            FileMedia::Image(image) => image.texture(),
        }
    }

    fn play(&mut self, now: Instant) {
        if let FileMedia::Video { player, .. } = self {
            player.play(now);
        }
    }

    fn pause(&mut self, now: Instant) {
        if let FileMedia::Video { player, .. } = self {
            player.pause(now);
        }
    }

    fn stop(&mut self) {
        if let FileMedia::Video { player, .. } = self {
            player.stop();
        }
    }

    fn ready(&self) -> bool {
        match self {
            FileMedia::Video { player, .. } => player.ready(),
            FileMedia::Image(_) => true, // nothing to start
        }
    }

    fn take_error(&mut self) -> Option<String> {
        match self {
            FileMedia::Video { player, .. } => player.take_error(),
            FileMedia::Image(image) => image.take_error(),
        }
    }

    fn take_dropped(&mut self) -> u64 {
        match self {
            FileMedia::Video { player, .. } => player.take_dropped(),
            FileMedia::Image(_) => 0,
        }
    }

    fn audio(&self) -> Option<AudioTap> {
        match self {
            FileMedia::Video { player, .. } => player.audio(),
            FileMedia::Image(_) => None,
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
}

impl FileOpener {
    pub fn new(gpu: Gpu, audio: Option<(u32, u16)>) -> FileOpener {
        FileOpener { gpu, audio, media: vec![] }
    }
}

impl Opener for FileOpener {
    type Media = FileMedia;

    fn open(&mut self, media_id: Uuid, layer: &Layer) -> Result<FileMedia, String> {
        let item = self.media.iter().find(|m| m.id == media_id);
        let item = item.ok_or_else(|| format!("layer {}: media {media_id} is not in the config", layer.id))?;
        let path = Path::new(&item.path);
        let media = match item.kind {
            MediaKind::Video => Player::open(path, layer.playback.clone(), self.audio)
                .map(|player| FileMedia::Video { player: Box::new(player), gpu: self.gpu.clone() }),
            MediaKind::Image => ImageSource::open(&self.gpu, path).map(|image| FileMedia::Image(Box::new(image))),
        };
        media.map_err(|e| format!("{}: {e:#}", item.path))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Weak;

    use tessera_show::{Pattern, Rect, TransitionKind};

    use super::*;

    const PERIOD: Duration = Duration::from_millis(16);

    #[derive(Default)]
    struct Fake {
        media: Uuid,
        playing: bool,
        ready: bool,
        failed: bool,
        error: Option<String>,
        stops: u32,
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
        fn take_error(&mut self) -> Option<String> {
            self.error.take()
        }
        fn take_dropped(&mut self) -> u64 {
            0
        }
        fn audio(&self) -> Option<AudioTap> {
            None
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
            Ok(Fake { media, ready: !self.slow.contains(&media), failed, error, ..Fake::default() })
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
    fn audio_gains_follow_transition() {
        let [a, s, m1, m2, m3, m4, m5] = ids();
        let t0 = Instant::now();
        let mut b = buses();
        let (shared, mut out, kept) = (clip(m1), clip(m2), clip(m4));
        out.volume = 0.8;
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
        assert_eq!(gain(&b, &pgm(&b, &kept)), [1.], "unchanged screen");
        assert_eq!(gain(&b, &pgm(&b, &hidden)), [] as [f32; 0], "invisible: muted");
        assert_eq!(b.gains.len(), 4);
        b.tick(t0 + ms(1000), PERIOD);
        b.gains(t0 + ms(1000));
        assert_eq!(b.gains.len(), 3, "the outgoing layer is gone at the end");
        b.set_preview(Scene::from([(a, vec![clip(Uuid::new_v4())])]));
        b.gains(t0 + ms(1000));
        assert_eq!(b.gains.len(), 3, "PVW is silent");
    }
}
