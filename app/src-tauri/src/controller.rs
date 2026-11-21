//! The show's single source of truth: undo/redo and what PVW and PGM hold. Every
//! method returns the messages that bring the engine in line; nothing here knows
//! about Tauri or sockets.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tessera_show::{
    Bus, Cue, EngineConfig, MediaItem, MediaKind, Playback, ProbeInfo, Scene, Show, ShowOp, Source, ToEngine, Transition,
    TransitionKind, TransportAction, next_cue_slot, resolve_shared, take_merge,
};
use uuid::Uuid;

const UNDO_CAP: usize = 100;
/// A Once clip this close to its end (or past it) at the last position report counts as ended.
const ENDED_SLACK_MS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CueRef {
    pub group_id: Uuid,
    pub slot: u32,
}

/// What the buses hold, for the UI (`live-changed`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Live {
    /// The cue Preview was last loaded from or stored into.
    pub pvw: Option<CueRef>,
    pub last_taken: Option<CueRef>,
    /// What PGM shows (the engine's PGM scene, by the same layer ids): the last take's screens
    /// plus whatever earlier takes left on the others. Transport targets these ids.
    pub program: Scene,
    /// What PVW shows, and the transition GO takes it with.
    pub preview: Scene,
    pub transition: Transition,
}

/// PVW: a working scene. Editing it changes no cue; a cue changes only when PVW is stored into it.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The cue it was last loaded from or stored into (Next walks on from it).
    pub cue: Option<CueRef>,
    pub scene: Scene,
    pub transition: Transition,
}

impl Default for Preview {
    fn default() -> Preview {
        Preview { cue: None, scene: Scene::new(), transition: Transition { kind: TransitionKind::Fade, duration_ms: 500 } }
    }
}

#[derive(Default)]
pub struct Controller {
    pub show: Show,
    pub path: Option<PathBuf>,
    /// Each step is the show and PVW before it (PGM is never undone: R32, R36).
    undo: Vec<(Show, Preview)>,
    redo: Vec<(Show, Preview)>,
    pub pvw: Preview,
    pub pgm: Scene,
    /// The cue PVW came from at the last Take (none for a Take of an unstored PVW).
    pub last_taken: Option<CueRef>,
    /// The show changed since it was last written (autosave or save).
    pub dirty: bool,
    /// The last Pause or Stop of each PGM media layer still on air (I4): a restarted engine plays
    /// every clip, so the resync repeats them.
    held: HashMap<Uuid, TransportAction>,
    /// The engine's last reported position (ms) of each PGM layer.
    positions: BTreeMap<Uuid, u64>,
}

fn cue_at(show: &Show, cue: Option<CueRef>) -> Option<&Cue> {
    let cue = cue?;
    let group = show.groups.iter().find(|g| g.id == cue.group_id)?;
    group.cues.iter().find(|c| c.slot == cue.slot)
}

/// Drops from `scene` what `op` removes from the show (a screen, or a media item's layers), as the
/// show does from its cues; whether anything went.
fn prune(scene: &mut Scene, op: &ShowOp) -> bool {
    match *op {
        ShowOp::RemoveScreen { id } => scene.remove(&id).is_some(),
        ShowOp::RemoveMedia { id } => {
            let before = scene.clone();
            for layers in scene.values_mut() {
                layers.retain(|l| !matches!(l.source, Source::Media { media_id } if media_id == id));
            }
            *scene != before
        }
        _ => false,
    }
}

fn guess_kind(path: &Path) -> MediaKind {
    // The engine plays png/jpeg as stills; the probe corrects anything else.
    match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("png" | "jpg" | "jpeg") => MediaKind::Image,
        _ => MediaKind::Video,
    }
}

impl Controller {
    pub fn new() -> Controller {
        Controller::default()
    }

    pub fn live(&self) -> Live {
        Live {
            pvw: self.pvw.cue,
            last_taken: self.last_taken,
            program: self.pgm.clone(),
            preview: self.pvw.scene.clone(),
            transition: self.pvw.transition.clone(),
        }
    }

    fn push_undo(&mut self, step: (Show, Preview)) {
        self.undo.push(step);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Something is on the outputs: a cue was taken, or PGM shows a layer.
    pub fn on_air(&self) -> bool {
        self.last_taken.is_some() || self.pgm.values().any(|layers| !layers.is_empty())
    }

    /// The show's render setup. Media paths are absolute: every path that enters is
    /// (`apply` rejects relative ones, `import_media` and `files::open` resolve them).
    pub fn engine_config(&self) -> EngineConfig {
        let s = &self.show;
        EngineConfig { outputs: s.outputs.clone(), screens: s.screens.clone(), mappings: s.mappings.clone(), media: s.media.clone() }
    }

    /// Apply a user edit (one undo step). PVW and PGM change only where the show lost what they
    /// show (a screen or media removed); editing a cue changes neither.
    pub fn apply(&mut self, op: ShowOp) -> Result<Vec<ToEngine>, String> {
        let relative = |m: &MediaItem| Path::new(&m.path).is_relative();
        if match &op {
            ShowOp::AddMedia { items } => items.iter().any(relative),
            ShowOp::UpdateMedia { item } => relative(item),
            _ => false,
        } {
            return Err("media paths must be absolute".into());
        }
        let config = self.engine_config();
        let volume = self.show.master_volume;
        let moved = match op {
            ShowOp::MoveCue { group_id, from, to } => Some((group_id, from, to)),
            _ => None,
        };
        let before = (self.show.clone(), self.pvw.clone());
        self.show.apply(op.clone())?;
        self.dirty = true;
        self.push_undo(before);
        if let Some((group_id, from, to)) = moved {
            // Bus refs follow their cue (move_cue swaps from and to).
            for r in [&mut self.pvw.cue, &mut self.last_taken].into_iter().flatten() {
                if r.group_id == group_id && (r.slot == from || r.slot == to) {
                    r.slot = if r.slot == from { to } else { from };
                }
            }
        }
        let mut out = vec![];
        if self.engine_config() != config {
            out.push(ToEngine::SetConfig { config: self.engine_config() });
        }
        self.forget_gone_cues();
        if prune(&mut self.pgm, &op) {
            self.forget_gone();
            out.push(ToEngine::SetProgram { scene: self.pgm.clone() });
        }
        if prune(&mut self.pvw.scene, &op) {
            out.push(ToEngine::SetPreview { scene: self.pvw.scene.clone() });
        }
        if self.show.master_volume != volume {
            out.push(ToEngine::SetMasterVolume { volume: self.show.master_volume });
        }
        Ok(out)
    }

    /// Bus refs to cues that are gone become none.
    fn forget_gone_cues(&mut self) {
        for r in [&mut self.pvw.cue, &mut self.last_taken] {
            if cue_at(&self.show, *r).is_none() {
                *r = None;
            }
        }
    }

    pub fn undo(&mut self) -> Vec<ToEngine> {
        let Some(prev) = self.undo.pop() else { return vec![] };
        let cur = (std::mem::replace(&mut self.show, prev.0), std::mem::replace(&mut self.pvw, prev.1));
        self.dirty |= cur.0 != self.show;
        self.redo.push(cur);
        self.history_moved()
    }

    pub fn redo(&mut self) -> Vec<ToEngine> {
        let Some(next) = self.redo.pop() else { return vec![] };
        let cur = (std::mem::replace(&mut self.show, next.0), std::mem::replace(&mut self.pvw, next.1));
        self.dirty |= cur.0 != self.show;
        self.undo.push(cur);
        self.history_moved()
    }

    /// After undo/redo: config, PVW and volume; PGM never changes (R32, R36).
    fn history_moved(&mut self) -> Vec<ToEngine> {
        self.forget_gone_cues();
        vec![
            ToEngine::SetConfig { config: self.engine_config() },
            ToEngine::SetPreview { scene: self.pvw.scene.clone() },
            ToEngine::SetMasterVolume { volume: self.show.master_volume },
        ]
    }

    /// PVW becomes `next` (one undo step, unless nothing changes).
    fn set_pvw(&mut self, next: Preview) -> Vec<ToEngine> {
        if next == self.pvw {
            return vec![];
        }
        let send = next.scene != self.pvw.scene;
        let before = std::mem::replace(&mut self.pvw, next);
        self.push_undo((self.show.clone(), before));
        if send { vec![ToEngine::SetPreview { scene: self.pvw.scene.clone() }] } else { vec![] }
    }

    /// An edit of PVW: its scene and transition. No cue changes.
    pub fn set_preview(&mut self, scene: Scene, transition: Transition) -> Vec<ToEngine> {
        self.set_pvw(Preview { scene, transition, ..self.pvw.clone() })
    }

    /// A copy of the cue at `cue` into PVW (what PVW held before is one undo away).
    pub fn load_cue(&mut self, cue: CueRef) -> Result<Vec<ToEngine>, String> {
        let c = cue_at(&self.show, Some(cue)).ok_or_else(|| format!("no cue in slot {}", cue.slot))?;
        let next = Preview { cue: Some(cue), scene: c.layers_by_screen.clone(), transition: c.transition.clone() };
        Ok(self.set_pvw(next))
    }

    /// PVW stored into the cue at `at` (one undo step): its layers and transition replace the
    /// cue's, or make a new cue in an empty slot. PVW then counts as that cue; PGM is untouched.
    pub fn store_preview(&mut self, at: CueRef) -> Result<Vec<ToEngine>, String> {
        let (scene, transition) = (self.pvw.scene.clone(), self.pvw.transition.clone());
        let cue = match cue_at(&self.show, Some(at)) {
            Some(c) => Cue { layers_by_screen: scene, transition, ..c.clone() },
            None => Cue { id: Uuid::new_v4(), name: format!("Cue {}", at.slot + 1), slot: at.slot, layers_by_screen: scene, transition },
        };
        let out = self.apply(ShowOp::SetCue { group_id: at.group_id, cue })?;
        self.pvw.cue = Some(at);
        Ok(out)
    }

    /// A live edit of PGM (not undoable, like every PGM change: R32).
    pub fn set_program(&mut self, scene: Scene) -> Vec<ToEngine> {
        if scene == self.pgm {
            return vec![];
        }
        self.pgm = scene;
        self.forget_gone();
        vec![ToEngine::SetProgram { scene: self.pgm.clone() }]
    }

    /// PVW to PGM with PVW's transition; PVW keeps what it holds.
    pub fn take(&mut self) -> Vec<ToEngine> {
        if self.pvw.scene.is_empty() {
            return vec![];
        }
        // Players PVW shares with PGM stay as they are (paused, stopped) under the new layer ids.
        for (incoming, live) in resolve_shared(&self.pgm, &self.pvw.scene) {
            if let Some(&action) = self.held.get(&live) {
                self.held.insert(incoming, action);
            }
        }
        self.pgm = take_merge(&self.pgm, &self.pvw.scene);
        self.forget_gone();
        self.last_taken = self.pvw.cue;
        vec![ToEngine::Take { transition: self.pvw.transition.clone() }]
    }

    /// A transport command, remembered for PGM (I4): Pause/Stop until Play/Restart.
    pub fn transport(&mut self, bus: Bus, layer: Option<Uuid>, action: TransportAction) -> Vec<ToEngine> {
        if bus == Bus::Pgm {
            let all: Vec<Uuid> = match layer {
                Some(id) => vec![id],
                None => self.pgm.values().flatten().filter(|l| matches!(l.source, Source::Media { .. })).map(|l| l.id).collect(),
            };
            for id in all {
                match action {
                    TransportAction::Pause | TransportAction::Stop => _ = self.held.insert(id, action),
                    TransportAction::Play | TransportAction::Restart => _ = self.held.remove(&id),
                }
            }
        }
        vec![ToEngine::Transport { bus, layer, action }]
    }

    /// The engine's `Positions` report: kept for PGM layers (a layer it no longer plays keeps its
    /// last position).
    pub fn positions(&mut self, positions: &BTreeMap<Uuid, u64>) {
        let on_pgm = |id: &Uuid| self.pgm.values().flatten().any(|l| l.id == *id);
        let report: Vec<_> = positions.iter().filter(|(id, _)| on_pgm(id)).map(|(id, ms)| (*id, *ms)).collect();
        self.forget_gone();
        self.positions.extend(report);
    }

    /// Drops transport and position state of layers no longer on PGM.
    fn forget_gone(&mut self) {
        let ids: std::collections::HashSet<Uuid> = self.pgm.values().flatten().map(|l| l.id).collect();
        self.held.retain(|id, _| ids.contains(id));
        self.positions.retain(|id, _| ids.contains(id));
    }

    /// PGM as a restarted engine should get it: without Once clips that had ended (they'd play
    /// again on air); everything else starts from its first frame.
    fn resync_program(&self) -> Scene {
        let ended = |l: &tessera_show::Layer| {
            let Source::Media { media_id } = l.source else { return false };
            let duration = self.show.media.iter().find(|m| m.id == media_id).and_then(|m| m.duration_ms);
            l.playback == Playback::Once
                && duration.zip(self.positions.get(&l.id)).is_some_and(|(d, &pos)| pos + ENDED_SLACK_MS >= d)
        };
        self.pgm.iter().map(|(s, layers)| (*s, layers.iter().filter(|l| !ended(l)).cloned().collect())).collect()
    }

    /// Crash-loop fallback (I5): PGM keeps only its pattern layers, PVW empties (its cue may be
    /// what kills the engine, and a Take must not put a stripped copy on air). Returns what was
    /// dropped, for the operator.
    pub fn drop_media(&mut self) -> Vec<String> {
        let show = &self.show;
        let describe = |l: &tessera_show::Layer, media_id: Uuid| {
            let path = show.media.iter().find(|m| m.id == media_id).map_or("missing media", |m| m.path.as_str());
            if l.name.is_empty() { path.to_string() } else { format!("“{}” ({path})", l.name) }
        };
        let mut dropped = vec![];
        for layers in self.pgm.values_mut() {
            layers.retain(|l| match l.source {
                Source::Media { media_id } => {
                    dropped.push(format!("PGM {}", describe(l, media_id)));
                    false
                }
                Source::Pattern { .. } => true,
            });
        }
        for l in self.pvw.scene.values().flatten() {
            if let Source::Media { media_id } = l.source {
                dropped.push(format!("PVW {}", describe(l, media_id)));
            }
        }
        self.pvw = Preview::default();
        self.forget_gone();
        dropped
    }

    /// The next occupied slot after the PVW cue (first group's first cue when PVW is empty).
    pub fn load_next(&mut self) -> Vec<ToEngine> {
        let (group, after) = match self.pvw.cue {
            Some(r) => (self.show.groups.iter().find(|g| g.id == r.group_id), Some(r.slot)),
            None => (self.show.groups.first(), None),
        };
        let Some(group) = group else { return vec![] };
        match next_cue_slot(group, after) {
            Some(slot) => self.load_cue(CueRef { group_id: group.id, slot }).unwrap_or_default(),
            None => vec![],
        }
    }

    /// Everything a freshly (re)started engine needs, in order. Clips restart from their first
    /// frame; paused and stopped ones are paused/stopped again (a Transport cancels the pending
    /// start), and ended Once clips stay off (I4).
    pub fn resync_messages(&self) -> Vec<ToEngine> {
        let program = self.resync_program();
        let on = |id: &Uuid| program.values().flatten().any(|l| l.id == *id);
        let mut held: Vec<_> = self.held.iter().filter(|(id, _)| on(id)).collect();
        held.sort_by_key(|(id, _)| **id); // deterministic order
        let transports = held.into_iter().map(|(id, action)| ToEngine::Transport { bus: Bus::Pgm, layer: Some(*id), action: *action });
        let mut out = vec![ToEngine::SetConfig { config: self.engine_config() }, ToEngine::SetProgram { scene: program.clone() }];
        out.extend(transports);
        out.push(ToEngine::SetPreview { scene: self.pvw.scene.clone() });
        out.push(ToEngine::SetMasterVolume { volume: self.show.master_volume });
        out
    }

    /// Add media (one undo step) and ask the engine to probe each item.
    pub fn import_media(&mut self, paths: &[PathBuf]) -> Result<Vec<ToEngine>, String> {
        let mut items = vec![];
        for p in paths {
            let p = std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let p = crate::files::normalize(&p);
            items.push(MediaItem {
                id: Uuid::new_v4(),
                name: p.file_name().map(|n| n.to_string_lossy().into()).unwrap_or_default(),
                path: p.to_string_lossy().into(),
                kind: guess_kind(&p),
                width: 0,
                height: 0,
                duration_ms: None,
                has_audio: false,
                has_alpha: false,
                thumb: None,
            });
        }
        let probes: Vec<_> =
            items.iter().map(|m| ToEngine::Probe { media_id: m.id, path: m.path.clone() }).collect();
        let mut out = self.apply(ShowOp::AddMedia { items })?;
        out.extend(probes);
        Ok(out)
    }

    /// A probe result: an `UpdateMedia` that is not an undo step. Patched into the undo/redo
    /// snapshots too, so undoing an unrelated edit doesn't forget it.
    pub fn probed(&mut self, media_id: Uuid, info: ProbeInfo) -> Vec<ToEngine> {
        let config = self.engine_config();
        // Only a change to the show itself is unsaved work (M1): not one to an undo snapshot.
        let before = self.show.media.iter().find(|m| m.id == media_id).cloned();
        let steps = self.undo.iter_mut().chain(&mut self.redo).map(|(show, _)| show);
        for show in std::iter::once(&mut self.show).chain(steps) {
            if let Some(m) = show.media.iter_mut().find(|m| m.id == media_id) {
                m.kind = info.kind.clone();
                m.width = info.width;
                m.height = info.height;
                m.duration_ms = info.duration_ms;
                m.has_audio = info.has_audio;
                m.has_alpha = info.has_alpha;
                m.thumb = info.thumb.clone();
            }
        }
        self.dirty |= before.as_ref() != self.show.media.iter().find(|m| m.id == media_id);
        if self.engine_config() == config { vec![] } else { vec![ToEngine::SetConfig { config: self.engine_config() }] }
    }

    /// Replace the show (opened from disk). History and PVW clear; PGM keeps running, and so does
    /// what the resync must know about it. Every media item is probed again, so metadata and
    /// thumbnails from an older engine (or a cleared thumbnail cache) catch up.
    pub fn open(&mut self, show: Show, path: Option<PathBuf>) -> Vec<ToEngine> {
        let (pgm, held, positions) = (std::mem::take(&mut self.pgm), std::mem::take(&mut self.held), std::mem::take(&mut self.positions));
        *self = Controller { show, path, pgm, held, positions, ..Controller::default() };
        let probes = self.show.media.iter().map(|m| ToEngine::Probe { media_id: m.id, path: m.path.clone() });
        [
            ToEngine::SetConfig { config: self.engine_config() },
            ToEngine::SetPreview { scene: Scene::new() },
            ToEngine::SetMasterVolume { volume: self.show.master_volume },
        ]
        .into_iter()
        .chain(probes)
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_show::{Layer, MediaKind, Pattern, Rect, Screen, Source, Transition, TransitionKind, Cue};

    fn item(duration_ms: Option<u64>) -> MediaItem {
        let id = Uuid::new_v4();
        MediaItem { id, name: "m".into(), path: format!("/media/{id}.mp4"), kind: MediaKind::Video, width: 10, height: 10,
            duration_ms, has_audio: false, has_alpha: false, thumb: None }
    }
    fn clip(media: &MediaItem, playback: Playback) -> Layer {
        Layer { playback, name: "clip".into(), ..Layer::new(Source::Media { media_id: media.id }, rect()) }
    }

    fn rect() -> Rect { Rect { x: 0., y: 0., w: 10., h: 10. } }
    fn layer() -> Layer { Layer::new(Source::Pattern { pattern: Pattern::Bars }, rect()) }
    fn screen() -> Screen { Screen { id: Uuid::new_v4(), name: "S".into(), width: 10, height: 10, layout_x: 0, layout_y: 0 } }
    fn cue(slot: u32, scene: Scene) -> Cue {
        Cue { id: Uuid::new_v4(), name: format!("c{slot}"), slot, layers_by_screen: scene,
            transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 } }
    }
    fn group(c: &Controller) -> Uuid { c.show.groups[0].id }
    fn set_cue(c: &mut Controller, slot: u32, scene: Scene) -> Vec<ToEngine> {
        let group_id = group(c);
        c.apply(ShowOp::SetCue { group_id, cue: cue(slot, scene) }).unwrap()
    }
    fn at(c: &Controller, slot: u32) -> CueRef { CueRef { group_id: group(c), slot } }
    fn has(msgs: &[ToEngine], f: impl Fn(&ToEngine) -> bool) -> bool { msgs.iter().any(f) }
    fn is_program(m: &ToEngine) -> bool { matches!(m, ToEngine::SetProgram { .. }) }

    #[test]
    fn undo_redo_roundtrip() {
        let mut c = Controller::new();
        let s0 = c.show.clone();
        c.apply(ShowOp::SetMasterVolume { volume: 0.5 }).unwrap();
        let s1 = c.show.clone();
        c.apply(ShowOp::AddScreen { screen: screen() }).unwrap();
        let s2 = c.show.clone();
        c.undo();
        assert_eq!(c.show, s1);
        c.undo();
        assert_eq!(c.show, s0);
        assert!(c.undo().is_empty(), "nothing left to undo");
        assert_eq!(c.show, s0);
        c.redo();
        assert_eq!(c.show, s1);
        c.redo();
        assert_eq!(c.show, s2);
        assert!(c.redo().is_empty());
        // A new op after an undo drops the redo stack.
        c.undo();
        c.apply(ShowOp::SetMasterVolume { volume: 0.1 }).unwrap();
        assert!(c.redo().is_empty());
        // A failed op is not an undo step.
        let before = c.show.clone();
        assert!(c.apply(ShowOp::RemoveScreen { id: Uuid::new_v4() }).is_err());
        c.undo();
        assert_eq!(c.show, s1);
        assert_ne!(before, s1);
    }

    #[test]
    fn undo_cap_100() {
        let mut c = Controller::new();
        let mut states = vec![];
        for i in 0..150 {
            c.apply(ShowOp::SetMasterVolume { volume: i as f32 / 1000.0 }).unwrap();
            states.push(c.show.clone());
        }
        for _ in 0..100 {
            c.undo();
        }
        assert_eq!(c.show, states[49]);
        assert!(c.undo().is_empty());
        assert_eq!(c.show, states[49]);
    }

    #[test]
    fn apply_screen_change_emits_set_config() {
        let mut c = Controller::new();
        let s = screen();
        let msgs = c.apply(ShowOp::AddScreen { screen: s.clone() }).unwrap();
        assert_eq!(msgs, vec![ToEngine::SetConfig { config: c.engine_config() }]);
        assert_eq!(c.engine_config().screens, vec![s]);
        // A cue that is on no bus changes nothing in the engine.
        assert!(set_cue(&mut c, 1, Scene::new()).is_empty());
    }

    /// Editing PVW changes no cue, and editing a cue doesn't change PVW: they meet only on a load
    /// (cue into PVW) or a store (PVW into a cue).
    #[test]
    fn preview_is_its_own_scene() {
        let mut c = Controller::new();
        let s = screen().id;
        let original = Scene::from([(s, vec![layer()])]);
        set_cue(&mut c, 1, original.clone());
        let pvw = at(&c, 1);
        let msgs = c.load_cue(pvw).unwrap();
        assert_eq!(msgs, vec![ToEngine::SetPreview { scene: original.clone() }]);
        assert_eq!(c.pvw.cue, Some(pvw));
        let fade = Transition { kind: TransitionKind::Fade, duration_ms: 300 };
        let edited = Scene::from([(s, vec![layer(), layer()])]);
        assert_eq!(c.set_preview(edited.clone(), fade.clone()), vec![ToEngine::SetPreview { scene: edited.clone() }]);
        assert_eq!(c.show.groups[0].cues[0].layers_by_screen, original, "the cue keeps its layers");
        assert!(c.set_preview(edited.clone(), fade.clone()).is_empty(), "no change, nothing sent");
        // Editing the cue leaves PVW alone.
        assert!(set_cue(&mut c, 1, Scene::new()).is_empty());
        assert_eq!((c.pvw.scene.clone(), c.pvw.transition.clone()), (edited.clone(), fade));
        // The cue moves: PVW's ref follows it. Deleted: PVW keeps its scene.
        let group_id = group(&c);
        assert!(c.apply(ShowOp::MoveCue { group_id, from: 1, to: 5 }).unwrap().is_empty());
        assert_eq!(c.pvw.cue, Some(at(&c, 5)));
        assert!(c.apply(ShowOp::DeleteCue { group_id, slot: 5 }).unwrap().is_empty());
        assert_eq!((c.pvw.cue, c.pvw.scene.clone()), (None, edited.clone()));
        assert!(c.load_cue(at(&c, 9)).is_err());
        // PVW edits undo like show edits, without being unsaved work.
        c.dirty = false;
        c.set_preview(Scene::new(), Transition { kind: TransitionKind::Cut, duration_ms: 0 });
        assert!(has(&c.undo(), |m| *m == ToEngine::SetPreview { scene: edited.clone() }));
        assert!(!c.dirty);
    }

    #[test]
    fn store_preview_into_cues() {
        let mut c = Controller::new();
        let s = screen().id;
        let fade = Transition { kind: TransitionKind::Fade, duration_ms: 750 };
        let first = Scene::from([(s, vec![layer()])]);
        c.set_preview(first.clone(), fade.clone());
        assert!(c.store_preview(at(&c, 3)).unwrap().is_empty(), "storing sends nothing to the engine");
        let cue = c.show.groups[0].cues[0].clone();
        assert_eq!((cue.slot, cue.name.as_str(), &cue.layers_by_screen, &cue.transition), (3, "Cue 4", &first, &fade));
        assert_eq!(c.pvw.cue, Some(at(&c, 3)));
        // Updating keeps the cue's id and name.
        let second = Scene::from([(s, vec![layer(), layer()])]);
        c.set_preview(second.clone(), fade.clone());
        c.store_preview(at(&c, 3)).unwrap();
        let updated = &c.show.groups[0].cues[0];
        assert_eq!((updated.id, &updated.name, &updated.layers_by_screen), (cue.id, &cue.name, &second));
        c.undo();
        assert_eq!(c.show.groups[0].cues[0].layers_by_screen, first);
    }

    #[test]
    fn take_emits_take_with_cue_transition() {
        let mut c = Controller::new();
        assert!(c.take().is_empty(), "nothing in PVW");
        let (a, b) = (screen().id, screen().id);
        let fade = Transition { kind: TransitionKind::Fade, duration_ms: 750 };
        let group_id = group(&c);
        let mut first = cue(1, Scene::from([(a, vec![layer()]), (b, vec![layer()])]));
        first.transition = fade.clone();
        c.apply(ShowOp::SetCue { group_id, cue: first.clone() }).unwrap();
        c.load_cue(at(&c, 1)).unwrap();
        assert_eq!(c.take(), vec![ToEngine::Take { transition: fade }]);
        assert_eq!(c.pgm, first.layers_by_screen);
        assert_eq!(c.pvw.cue, Some(at(&c, 1)), "PVW stays loaded");
        assert_eq!(c.last_taken, Some(at(&c, 1)));
        // A cue touching only screen a leaves b on PGM.
        let only_a = Scene::from([(a, vec![])]);
        set_cue(&mut c, 2, only_a.clone());
        c.load_cue(at(&c, 2)).unwrap();
        assert_eq!(c.take(), vec![ToEngine::Take { transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 } }]);
        assert_eq!(c.pgm, tessera_show::take_merge(&first.layers_by_screen, &only_a));
        // An unstored PVW goes to air too, with its own transition; no cue is on air then.
        let wipe = Transition { kind: TransitionKind::WipeLeft, duration_ms: 400 };
        c.set_preview(Scene::from([(b, vec![layer()])]), wipe.clone());
        c.pvw.cue = None;
        assert_eq!(c.take(), vec![ToEngine::Take { transition: wipe }]);
        assert_eq!(c.last_taken, None);
    }

    /// R41: the UI addresses live layers (transport) by the ids PGM holds: the last taken cue's on
    /// the screens it defines, an earlier cue's elsewhere. `live().program` is that scene.
    #[test]
    fn live_reports_program_scene() {
        let mut c = Controller::new();
        let (a, b) = (screen().id, screen().id);
        let first = Scene::from([(a, vec![layer()]), (b, vec![layer()])]);
        let second = Scene::from([(a, vec![layer()])]);
        set_cue(&mut c, 1, first.clone());
        set_cue(&mut c, 2, second.clone());
        assert!(c.live().program.is_empty());
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        c.load_cue(at(&c, 2)).unwrap();
        assert_eq!(c.live().program, first, "loading PVW doesn't change PGM");
        c.take();
        assert_eq!(c.live().program, Scene::from([(a, second[&a].clone()), (b, first[&b].clone())]));
    }

    /// PGM changes by a Take, a live edit (`set_program`), or losing what it shows from the show;
    /// never by editing a cue, even the one on air.
    #[test]
    fn program_changes_only_by_take_or_live_edit() {
        let mut c = Controller::new();
        let a = screen();
        let m = item(Some(1000));
        c.apply(ShowOp::AddScreen { screen: a.clone() }).unwrap();
        c.apply(ShowOp::AddMedia { items: vec![m.clone()] }).unwrap();
        let (bars, bg) = (layer(), clip(&m, Playback::Loop));
        set_cue(&mut c, 1, Scene::from([(a.id, vec![bars.clone()])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        let pgm = c.pgm.clone();
        assert!(set_cue(&mut c, 1, Scene::from([(a.id, vec![])])).is_empty());
        assert_eq!(c.pgm, pgm, "editing the cue on air leaves PGM");
        let live = Scene::from([(a.id, vec![bars.clone(), bg.clone()])]);
        assert_eq!(c.set_program(live.clone()), vec![ToEngine::SetProgram { scene: live.clone() }]);
        assert!(c.set_program(live).is_empty());
        let msgs = c.apply(ShowOp::RemoveMedia { id: m.id }).unwrap();
        assert_eq!(c.pgm, Scene::from([(a.id, vec![bars])]), "removed media leaves PGM");
        assert!(has(&msgs, |x| *x == ToEngine::SetProgram { scene: c.pgm.clone() }));
    }

    #[test]
    fn undo_never_emits_set_program() {
        let mut c = Controller::new();
        let a = screen().id;
        set_cue(&mut c, 1, Scene::from([(a, vec![layer()])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        assert!(has(&c.set_program(Scene::from([(a, vec![layer(), layer()])])), is_program));
        set_cue(&mut c, 1, Scene::from([(a, vec![])]));
        let pgm = c.pgm.clone();
        let pvw = c.pvw.scene.clone();
        let msgs = c.undo();
        assert_eq!(msgs, vec![
            ToEngine::SetConfig { config: c.engine_config() },
            ToEngine::SetPreview { scene: pvw.clone() },
            ToEngine::SetMasterVolume { volume: 1.0 },
        ]);
        assert_eq!(c.pgm, pgm, "undo leaves PGM alone");
        assert_eq!(c.pvw.scene, pvw);
        // An unrelated op afterwards doesn't push the undone cue on air either.
        assert!(!has(&c.apply(ShowOp::SetMasterVolume { volume: 0.5 }).unwrap(), is_program));
        // Undoing the volume change puts the engine's volume back too (R36).
        assert!(has(&c.undo(), |m| *m == ToEngine::SetMasterVolume { volume: 1.0 }));
        assert!(has(&c.redo(), |m| *m == ToEngine::SetMasterVolume { volume: 0.5 }));
        c.undo();
        assert!(!has(&c.redo(), is_program));
        assert!(!has(&c.redo(), is_program));
        assert_eq!(c.pgm, pgm);
    }

    #[test]
    fn resync_messages_order() {
        let mut c = Controller::new();
        let s = screen();
        c.apply(ShowOp::AddScreen { screen: s.clone() }).unwrap();
        set_cue(&mut c, 1, Scene::from([(s.id, vec![layer()])]));
        set_cue(&mut c, 2, Scene::from([(s.id, vec![])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        c.load_cue(at(&c, 2)).unwrap();
        c.apply(ShowOp::SetMasterVolume { volume: 0.5 }).unwrap();
        assert_eq!(c.resync_messages(), vec![
            ToEngine::SetConfig { config: c.engine_config() },
            ToEngine::SetProgram { scene: c.show.groups[0].cues[0].layers_by_screen.clone() },
            ToEngine::SetPreview { scene: Scene::from([(s.id, vec![])]) },
            ToEngine::SetMasterVolume { volume: 0.5 },
        ]);
    }

    /// I4: a restarted engine gets PGM as it was: ended Once clips left out, paused and stopped
    /// clips paused/stopped again; Play/Restart forget it; a player shared across a Take keeps it.
    #[test]
    fn resync_restores_transport_state() {
        let mut c = Controller::new();
        let s = screen().id;
        let (short, long) = (item(Some(1000)), item(Some(60_000)));
        c.apply(ShowOp::AddMedia { items: vec![short.clone(), long.clone()] }).unwrap();
        let (bg, once, bars) = (clip(&long, Playback::Loop), clip(&short, Playback::Once), layer());
        set_cue(&mut c, 1, Scene::from([(s, vec![bg.clone(), once.clone(), bars.clone()])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        let program = |msgs: &[ToEngine]| match &msgs[1] { ToEngine::SetProgram { scene } => scene.clone(), m => panic!("{m:?}") };
        let transports = |msgs: &[ToEngine]| -> Vec<(Option<Uuid>, TransportAction)> {
            msgs.iter().filter_map(|m| match m { ToEngine::Transport { bus: Bus::Pgm, layer, action } => Some((*layer, *action)), _ => None }).collect()
        };
        c.positions(&BTreeMap::from([(bg.id, 400), (once.id, 950), (Uuid::new_v4(), 7)]));
        assert_eq!(c.transport(Bus::Pgm, Some(bg.id), TransportAction::Pause), vec![ToEngine::Transport { bus: Bus::Pgm, layer: Some(bg.id), action: TransportAction::Pause }]);
        c.transport(Bus::Pvw, Some(once.id), TransportAction::Stop); // PVW: not remembered
        let msgs = c.resync_messages();
        assert_eq!(program(&msgs), Scene::from([(s, vec![bg.clone(), bars.clone()])]), "the ended Once clip stays off");
        assert_eq!(transports(&msgs), [(Some(bg.id), TransportAction::Pause)]);
        assert!(matches!(msgs[2], ToEngine::Transport { .. }) && matches!(msgs[3], ToEngine::SetPreview { .. }), "right after SetProgram");
        // Opening a show keeps PGM running, and what the resync knows about it.
        c.open(c.show.clone(), None);
        assert_eq!(transports(&c.resync_messages()), [(Some(bg.id), TransportAction::Pause)]);
        // Play on every PGM layer forgets it; a Once clip short of its end comes back.
        c.transport(Bus::Pgm, None, TransportAction::Play);
        c.positions(&BTreeMap::from([(once.id, 500)]));
        let msgs = c.resync_messages();
        assert_eq!(program(&msgs), c.pgm);
        assert!(transports(&msgs).is_empty());
        // Stop everything, then take a cue whose first layer shares bg's player (same screen,
        // media, index): the stop follows it to the new layer id; the rest left PGM.
        c.transport(Bus::Pgm, None, TransportAction::Stop);
        let bg2 = clip(&long, Playback::Loop);
        set_cue(&mut c, 2, Scene::from([(s, vec![bg2.clone()])]));
        c.load_cue(at(&c, 2)).unwrap();
        c.take();
        assert_eq!(transports(&c.resync_messages()), [(Some(bg2.id), TransportAction::Stop)]);
        c.transport(Bus::Pgm, Some(bg2.id), TransportAction::Restart);
        assert!(transports(&c.resync_messages()).is_empty());
    }

    /// I5: after a crash loop the resync keeps PGM's patterns only, and PVW empties.
    #[test]
    fn drop_media_keeps_patterns_and_clears_pvw() {
        let mut c = Controller::new();
        let s = screen().id;
        let m = item(Some(5000));
        c.apply(ShowOp::AddMedia { items: vec![m.clone()] }).unwrap();
        let (bg, bars) = (clip(&m, Playback::Loop), layer());
        set_cue(&mut c, 1, Scene::from([(s, vec![bg.clone(), bars.clone()])]));
        set_cue(&mut c, 2, Scene::from([(s, vec![clip(&m, Playback::Loop)])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        c.load_cue(at(&c, 2)).unwrap();
        c.transport(Bus::Pgm, Some(bg.id), TransportAction::Pause);
        let dropped = c.drop_media();
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert!(dropped[0].starts_with("PGM “clip”") && dropped[0].contains(&m.path), "{dropped:?}");
        assert!(dropped[1].starts_with("PVW") && dropped[1].contains(&m.path), "{dropped:?}");
        assert_eq!((c.pvw.clone(), c.pgm.clone()), (Preview::default(), Scene::from([(s, vec![bars])])));
        assert_eq!(c.resync_messages()[1..3], [ToEngine::SetProgram { scene: c.pgm.clone() }, ToEngine::SetPreview { scene: Scene::new() }]);
        assert!(c.on_air(), "patterns and the taken cue are still on air");
    }

    /// M1: only changes to the show are unsaved work.
    #[test]
    fn dirty_tracks_show_changes() {
        let mut c = Controller::new();
        assert!(!c.dirty && !c.on_air());
        let info = || ProbeInfo { kind: MediaKind::Video, width: 64, height: 32, duration_ms: Some(1000), has_audio: false,
            has_alpha: false, thumb: None };
        c.probed(Uuid::new_v4(), info());
        assert!(!c.dirty, "a probe of media the show doesn't have");
        c.import_media(&[PathBuf::from("/shows/a.mov")]).unwrap();
        assert!(c.dirty);
        let id = c.show.media[0].id;
        c.dirty = false;
        c.probed(id, info());
        assert!(c.dirty, "a probe that fills in an item");
        c.dirty = false;
        c.probed(id, info());
        assert!(!c.dirty, "the same probe again");
        set_cue(&mut c, 1, Scene::from([(screen().id, vec![])]));
        c.dirty = false;
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        c.transport(Bus::Pgm, None, TransportAction::Pause);
        assert!(!c.dirty && c.on_air(), "bus moves aren't show changes");
        c.undo();
        assert!(!c.dirty, "undoing the load is a PVW change, not a show change");
        c.undo();
        assert!(c.dirty);
        c.open(Show::new(), None);
        assert!(!c.dirty);
    }

    #[test]
    fn load_next_walks_slots() {
        let mut c = Controller::new();
        assert!(c.load_next().is_empty());
        for slot in [7, 2, 4] {
            set_cue(&mut c, slot, Scene::new());
        }
        for slot in [2, 4, 7] {
            c.load_next();
            assert_eq!(c.pvw.cue, Some(at(&c, slot)));
        }
        c.load_next();
        assert_eq!(c.pvw.cue, Some(at(&c, 7)), "no wrap");
    }

    #[test]
    fn import_then_probe_updates_media() {
        let mut c = Controller::new();
        let paths = [PathBuf::from("/shows/a.PNG"), PathBuf::from("/shows/b.mov")];
        let msgs = c.import_media(&paths).unwrap();
        let (img, vid) = (c.show.media[0].clone(), c.show.media[1].clone());
        assert_eq!((img.kind, vid.kind.clone()), (MediaKind::Image, MediaKind::Video));
        assert_eq!(img.name, "a.PNG");
        assert_eq!(msgs, vec![
            ToEngine::SetConfig { config: c.engine_config() },
            ToEngine::Probe { media_id: img.id, path: "/shows/a.PNG".into() },
            ToEngine::Probe { media_id: vid.id, path: "/shows/b.mov".into() },
        ]);
        c.apply(ShowOp::SetMasterVolume { volume: 0.5 }).unwrap();
        let info = ProbeInfo { kind: MediaKind::Video, width: 64, height: 32, duration_ms: Some(1000), has_audio: true,
            has_alpha: false, thumb: Some(format!("{}.jpg", vid.id)) };
        let msgs = c.probed(vid.id, info);
        assert_eq!(msgs, vec![ToEngine::SetConfig { config: c.engine_config() }]);
        let m = &c.show.media[1];
        assert_eq!((m.width, m.height, m.duration_ms, m.has_audio), (64, 32, Some(1000), true));
        assert_eq!(m.thumb, Some(format!("{}.jpg", vid.id)));
        // Not an undo step, and undoing past it keeps the probe result.
        c.undo();
        assert_eq!(c.show.media[1].width, 64);
        assert_eq!(c.show.master_volume, 1.0);
        // Opening a show probes its media again: metadata and thumbnails from an older engine, or a
        // cleared thumbnail cache, catch up.
        let msgs = c.open(c.show.clone(), None);
        assert_eq!(msgs[3..], [
            ToEngine::Probe { media_id: img.id, path: "/shows/a.PNG".into() },
            ToEngine::Probe { media_id: vid.id, path: "/shows/b.mov".into() },
        ]);
    }

    #[test]
    fn relative_media_paths_rejected() {
        let item = |path: &str| tessera_show::MediaItem { id: Uuid::new_v4(), name: "m".into(), path: path.into(),
            kind: MediaKind::Video, width: 1, height: 1, duration_ms: None, has_audio: false, has_alpha: false, thumb: None };
        for titled in [None, Some(PathBuf::from("/shows/s.tshow"))] {
            let mut c = Controller::new();
            c.path = titled;
            let before = c.show.clone();
            assert!(c.apply(ShowOp::AddMedia { items: vec![item("clips/m.mp4")] }).is_err());
            assert_eq!(c.show, before);
            let good = item("/shows/clips/m.mp4");
            c.apply(ShowOp::AddMedia { items: vec![good.clone()] }).unwrap();
            assert!(c.apply(ShowOp::UpdateMedia { item: tessera_show::MediaItem { path: "m.mp4".into(), ..good.clone() } }).is_err());
            assert_eq!(c.engine_config().media, vec![good]);
        }
    }

    #[test]
    fn import_normalizes_paths() {
        let mut c = Controller::new();
        c.import_media(&[PathBuf::from("/shows/a/../b.mov"), PathBuf::from("/shows/./c.png")]).unwrap();
        let paths: Vec<_> = c.show.media.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["/shows/b.mov", "/shows/c.png"]);
    }
}
