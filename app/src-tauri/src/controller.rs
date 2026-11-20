//! The show's single source of truth: undo/redo and what PVW and PGM hold. Every
//! method returns the messages that bring the engine in line; nothing here knows
//! about Tauri or sockets.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tessera_show::{
    EngineConfig, MediaItem, MediaKind, ProbeInfo, Scene, Show, ShowOp, ToEngine, next_cue_slot, take_merge,
};
use uuid::Uuid;

const UNDO_CAP: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CueRef {
    pub group_id: Uuid,
    pub slot: u32,
}

/// Which cues are on the buses, for the UI (`live-changed`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Live {
    pub pvw: Option<CueRef>,
    pub last_taken: Option<CueRef>,
}

#[derive(Default)]
pub struct Controller {
    pub show: Show,
    pub path: Option<PathBuf>,
    undo: Vec<Show>,
    redo: Vec<Show>,
    pub pvw: Option<CueRef>,
    pub pgm: Scene,
    pub pvw_scene: Scene,
    /// Edits to this cue go live (R32).
    pub last_taken: Option<CueRef>,
}

fn scene_of(show: &Show, cue: Option<CueRef>) -> Option<&Scene> {
    let cue = cue?;
    let group = show.groups.iter().find(|g| g.id == cue.group_id)?;
    group.cues.iter().find(|c| c.slot == cue.slot).map(|c| &c.layers_by_screen)
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
        Live { pvw: self.pvw, last_taken: self.last_taken }
    }

    /// The show's render setup. Media paths are absolute: every path that enters is
    /// (`apply` rejects relative ones, `import_media` and `files::open` resolve them).
    pub fn engine_config(&self) -> EngineConfig {
        let s = &self.show;
        EngineConfig { outputs: s.outputs.clone(), screens: s.screens.clone(), mappings: s.mappings.clone(), media: s.media.clone() }
    }

    /// Apply a user edit (one undo step). PGM changes only when the last taken cue is edited.
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
        let taken = scene_of(&self.show, self.last_taken).cloned();
        let moved = match op {
            ShowOp::MoveCue { group_id, from, to } => Some((group_id, from, to)),
            _ => None,
        };
        let before = self.show.clone();
        self.show.apply(op)?;
        self.undo.push(before);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
        if let Some((group_id, from, to)) = moved {
            // Bus refs follow their cue (move_cue swaps from and to).
            for r in [&mut self.pvw, &mut self.last_taken].into_iter().flatten() {
                if r.group_id == group_id && (r.slot == from || r.slot == to) {
                    r.slot = if r.slot == from { to } else { from };
                }
            }
        }
        let mut out = vec![];
        if self.engine_config() != config {
            out.push(ToEngine::SetConfig { config: self.engine_config() });
        }
        match scene_of(&self.show, self.last_taken) {
            None => self.last_taken = None,
            Some(scene) if Some(scene) != taken.as_ref() => {
                self.pgm = take_merge(&self.pgm, scene);
                out.push(ToEngine::SetProgram { scene: self.pgm.clone() });
            }
            Some(_) => {}
        }
        out.extend(self.refresh_pvw(false));
        if self.show.master_volume != volume {
            out.push(ToEngine::SetMasterVolume { volume: self.show.master_volume });
        }
        Ok(out)
    }

    /// Re-read the PVW cue (emptying PVW if it's gone); `SetPreview` if it differs or `always`.
    fn refresh_pvw(&mut self, always: bool) -> Option<ToEngine> {
        let scene = scene_of(&self.show, self.pvw).cloned();
        if scene.is_none() {
            self.pvw = None;
        }
        let scene = scene.unwrap_or_default();
        if !always && scene == self.pvw_scene {
            return None;
        }
        self.pvw_scene = scene;
        Some(ToEngine::SetPreview { scene: self.pvw_scene.clone() })
    }

    pub fn undo(&mut self) -> Vec<ToEngine> {
        match self.undo.pop() {
            Some(prev) => {
                let cur = std::mem::replace(&mut self.show, prev);
                self.redo.push(cur);
                self.history_moved()
            }
            None => vec![],
        }
    }

    pub fn redo(&mut self) -> Vec<ToEngine> {
        match self.redo.pop() {
            Some(next) => {
                let cur = std::mem::replace(&mut self.show, next);
                self.undo.push(cur);
                self.history_moved()
            }
            None => vec![],
        }
    }

    /// After undo/redo: config, PVW and volume; PGM never changes (R32, R36).
    fn history_moved(&mut self) -> Vec<ToEngine> {
        if scene_of(&self.show, self.last_taken).is_none() {
            self.last_taken = None;
        }
        let preview = self.refresh_pvw(true);
        let volume = ToEngine::SetMasterVolume { volume: self.show.master_volume };
        [Some(ToEngine::SetConfig { config: self.engine_config() }), preview, Some(volume)].into_iter().flatten().collect()
    }

    pub fn load_cue(&mut self, cue: CueRef) -> Result<Vec<ToEngine>, String> {
        if scene_of(&self.show, Some(cue)).is_none() {
            return Err(format!("no cue in slot {}", cue.slot));
        }
        self.pvw = Some(cue);
        Ok(self.refresh_pvw(true).into_iter().collect())
    }

    /// PVW cue to PGM with its transition; the PVW cue stays loaded.
    pub fn take(&mut self) -> Vec<ToEngine> {
        let Some(cue) = self.pvw else { return vec![] };
        let Some(c) = self.show.groups.iter().find(|g| g.id == cue.group_id)
            .and_then(|g| g.cues.iter().find(|c| c.slot == cue.slot))
        else {
            return vec![];
        };
        self.pgm = take_merge(&self.pgm, &self.pvw_scene);
        self.last_taken = Some(cue);
        vec![ToEngine::Take { transition: c.transition.clone() }]
    }

    /// The next occupied slot after the PVW cue (first group's first cue when PVW is empty).
    pub fn load_next(&mut self) -> Vec<ToEngine> {
        let (group, after) = match self.pvw {
            Some(r) => (self.show.groups.iter().find(|g| g.id == r.group_id), Some(r.slot)),
            None => (self.show.groups.first(), None),
        };
        let Some(group) = group else { return vec![] };
        match next_cue_slot(group, after) {
            Some(slot) => self.load_cue(CueRef { group_id: group.id, slot }).unwrap_or_default(),
            None => vec![],
        }
    }

    /// Everything a freshly (re)started engine needs, in order.
    pub fn resync_messages(&self) -> Vec<ToEngine> {
        vec![
            ToEngine::SetConfig { config: self.engine_config() },
            ToEngine::SetProgram { scene: self.pgm.clone() },
            ToEngine::SetPreview { scene: self.pvw_scene.clone() },
            ToEngine::SetMasterVolume { volume: self.show.master_volume },
        ]
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
        for show in std::iter::once(&mut self.show).chain(&mut self.undo).chain(&mut self.redo) {
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
        if self.engine_config() == config { vec![] } else { vec![ToEngine::SetConfig { config: self.engine_config() }] }
    }

    /// Replace the show (opened from disk). History and PVW clear; PGM keeps running.
    pub fn open(&mut self, show: Show, path: Option<PathBuf>) -> Vec<ToEngine> {
        *self = Controller { show, path, pgm: std::mem::take(&mut self.pgm), ..Controller::default() };
        vec![
            ToEngine::SetConfig { config: self.engine_config() },
            ToEngine::SetPreview { scene: Scene::new() },
            ToEngine::SetMasterVolume { volume: self.show.master_volume },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_show::{Layer, MediaKind, Pattern, Rect, Screen, Source, Transition, TransitionKind, Cue};

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

    #[test]
    fn edit_pvw_cue_emits_set_preview() {
        let mut c = Controller::new();
        let s = screen().id;
        set_cue(&mut c, 1, Scene::from([(s, vec![layer()])]));
        set_cue(&mut c, 2, Scene::new());
        let pvw = at(&c, 1);
        let msgs = c.load_cue(pvw).unwrap();
        assert_eq!(msgs, vec![ToEngine::SetPreview { scene: c.show.groups[0].cues[0].layers_by_screen.clone() }]);
        assert_eq!(c.pvw, Some(pvw));
        let edited = Scene::from([(s, vec![layer(), layer()])]);
        let msgs = set_cue(&mut c, 1, edited.clone());
        assert_eq!(msgs, vec![ToEngine::SetPreview { scene: edited.clone() }]);
        assert_eq!(c.pvw_scene, edited);
        // Another cue: PVW untouched.
        assert!(set_cue(&mut c, 2, Scene::from([(s, vec![])])).is_empty());
        // The PVW cue moves: PVW follows it, nothing to send.
        let group_id = group(&c);
        assert!(c.apply(ShowOp::MoveCue { group_id, from: 1, to: 5 }).unwrap().is_empty());
        assert_eq!(c.pvw, Some(at(&c, 5)));
        // Deleted: PVW empties.
        let msgs = c.apply(ShowOp::DeleteCue { group_id, slot: 5 }).unwrap();
        assert_eq!(msgs, vec![ToEngine::SetPreview { scene: Scene::new() }]);
        assert_eq!(c.pvw, None);
        assert!(c.load_cue(at(&c, 9)).is_err());
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
        assert_eq!(c.pvw, Some(at(&c, 1)), "PVW cue stays loaded");
        assert_eq!(c.last_taken, Some(at(&c, 1)));
        // A cue touching only screen a leaves b on PGM.
        let only_a = Scene::from([(a, vec![])]);
        set_cue(&mut c, 2, only_a.clone());
        c.load_cue(at(&c, 2)).unwrap();
        assert_eq!(c.take(), vec![ToEngine::Take { transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 } }]);
        assert_eq!(c.pgm, tessera_show::take_merge(&first.layers_by_screen, &only_a));
    }

    #[test]
    fn edit_last_taken_cue_emits_set_program() {
        let mut c = Controller::new();
        let (a, b) = (screen().id, screen().id);
        set_cue(&mut c, 1, Scene::from([(a, vec![layer()])]));
        set_cue(&mut c, 2, Scene::from([(b, vec![layer()])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        c.load_cue(at(&c, 2)).unwrap();
        c.take();
        set_cue(&mut c, 3, Scene::new());
        c.load_cue(at(&c, 3)).unwrap();
        let pgm_a = c.pgm[&a].clone();
        let edited = Scene::from([(b, vec![layer(), layer()])]);
        let msgs = set_cue(&mut c, 2, edited.clone());
        assert_eq!(c.pgm, Scene::from([(a, pgm_a), (b, edited[&b].clone())]));
        assert_eq!(msgs, vec![ToEngine::SetProgram { scene: c.pgm.clone() }]);
        // Cue 1 was taken earlier but isn't the last taken: editing it stays off air.
        assert!(set_cue(&mut c, 1, Scene::from([(a, vec![])])).is_empty());
    }

    #[test]
    fn undo_never_emits_set_program() {
        let mut c = Controller::new();
        let a = screen().id;
        set_cue(&mut c, 1, Scene::from([(a, vec![layer()])]));
        c.load_cue(at(&c, 1)).unwrap();
        c.take();
        let msgs = set_cue(&mut c, 1, Scene::from([(a, vec![layer(), layer()])]));
        assert!(has(&msgs, is_program));
        let pgm = c.pgm.clone();
        let msgs = c.undo();
        assert_eq!(msgs, vec![
            ToEngine::SetConfig { config: c.engine_config() },
            ToEngine::SetPreview { scene: c.show.groups[0].cues[0].layers_by_screen.clone() },
            ToEngine::SetMasterVolume { volume: 1.0 },
        ]);
        assert_eq!(c.pgm, pgm, "undo leaves PGM alone");
        assert_eq!(c.pvw_scene, c.show.groups[0].cues[0].layers_by_screen);
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

    #[test]
    fn load_next_walks_slots() {
        let mut c = Controller::new();
        assert!(c.load_next().is_empty());
        for slot in [7, 2, 4] {
            set_cue(&mut c, slot, Scene::new());
        }
        for slot in [2, 4, 7] {
            assert_eq!(c.load_next().len(), 1);
            assert_eq!(c.pvw, Some(at(&c, slot)));
        }
        assert!(c.load_next().is_empty(), "no wrap");
        assert_eq!(c.pvw, Some(at(&c, 7)));
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
