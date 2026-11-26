use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::cue::{delete_cue, duplicate_cue, move_cue, set_cue};
use crate::geom::Rect;
use crate::model::{Cue, Group, Mapping, MediaItem, Output, Pattern, Screen, Show, Source};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all_fields = "camelCase")]
pub enum ShowOp {
    AddOutput { output: Output },
    UpdateOutput { output: Output },
    RemoveOutput { id: Uuid },
    AddScreen { screen: Screen },
    UpdateScreen { screen: Screen },
    RemoveScreen { id: Uuid },
    SetMapping { mapping: Mapping },
    RemoveMapping { id: Uuid },
    AddMedia { items: Vec<MediaItem> },
    UpdateMedia { item: MediaItem },
    /// Several at once ("remove unused" is one undo step).
    RemoveMedia { ids: Vec<Uuid> },
    /// The media list's order (Resources): item `id` ends up at index `to`.
    MoveMedia { id: Uuid, to: u32 },
    AddGroup { name: String },
    RenameGroup { id: Uuid, name: String },
    RemoveGroup { id: Uuid },
    SetCue { group_id: Uuid, cue: Cue },
    MoveCue { group_id: Uuid, from: u32, to: u32 },
    DuplicateCue { group_id: Uuid, from: u32, to: u32 },
    DeleteCue { group_id: Uuid, slot: u32 },
    SetMasterVolume { volume: f32 },
}

/// Index of the item with `id`, or an error naming `what`.
fn find<T>(v: &[T], id: Uuid, key: impl Fn(&T) -> Uuid, what: &str) -> Result<usize, String> {
    v.iter().position(|x| key(x) == id).ok_or_else(|| format!("unknown {what} {id}"))
}

fn add<T>(v: &mut Vec<T>, item: T, key: impl Fn(&T) -> Uuid, what: &str) -> Result<(), String> {
    let id = key(&item);
    if v.iter().any(|x| key(x) == id) {
        return Err(format!("{what} {id} already exists"));
    }
    v.push(item);
    Ok(())
}

/// Every float `op` carries (they must all be finite).
fn floats(op: &ShowOp) -> Vec<f32> {
    let rect = |r: &Rect| [r.x, r.y, r.w, r.h];
    match op {
        ShowOp::AddOutput { output } | ShowOp::UpdateOutput { output } => vec![output.brightness],
        ShowOp::SetMapping { mapping } => [rect(&mapping.src), rect(&mapping.dst)].concat(),
        ShowOp::SetCue { cue, .. } => cue.layers_by_screen.values().flatten()
            .flat_map(|l| {
                let solid = match &l.source {
                    Source::Pattern { pattern: Pattern::Solid { rgba } } => &rgba[..],
                    _ => &[],
                };
                let c = &l.crop;
                [&rect(&l.rect)[..], &[l.opacity, l.brightness, l.contrast, l.saturation, l.volume, l.rotation, l.speed], &[c.left, c.top, c.right, c.bottom], solid].concat()
            })
            .collect(),
        ShowOp::SetMasterVolume { volume } => vec![*volume],
        _ => vec![],
    }
}

impl Show {
    /// Apply `op`. On `Err` the show is unchanged.
    pub fn apply(&mut self, op: ShowOp) -> Result<(), String> {
        if floats(&op).iter().any(|v| !v.is_finite()) {
            return Err("non-finite number in op".into());
        }
        match op {
            ShowOp::AddOutput { output } => add(&mut self.outputs, output, |o| o.id, "output"),
            ShowOp::UpdateOutput { output } => {
                let i = find(&self.outputs, output.id, |o| o.id, "output")?;
                self.outputs[i] = output;
                Ok(())
            }
            ShowOp::RemoveOutput { id } => {
                let i = find(&self.outputs, id, |o| o.id, "output")?;
                self.outputs.remove(i);
                self.mappings.retain(|m| m.output_id != id);
                Ok(())
            }
            ShowOp::AddScreen { screen } => add(&mut self.screens, screen, |s| s.id, "screen"),
            ShowOp::UpdateScreen { screen } => {
                let i = find(&self.screens, screen.id, |s| s.id, "screen")?;
                self.screens[i] = screen;
                Ok(())
            }
            ShowOp::RemoveScreen { id } => {
                let i = find(&self.screens, id, |s| s.id, "screen")?;
                self.screens.remove(i);
                self.mappings.retain(|m| m.screen_id != id);
                for cue in self.groups.iter_mut().flat_map(|g| &mut g.cues) {
                    cue.layers_by_screen.remove(&id);
                }
                Ok(())
            }
            ShowOp::SetMapping { mapping } => {
                match self.mappings.iter_mut().find(|m| m.id == mapping.id) {
                    Some(m) => *m = mapping,
                    None => self.mappings.push(mapping),
                }
                Ok(())
            }
            ShowOp::RemoveMapping { id } => {
                let i = find(&self.mappings, id, |m| m.id, "mapping")?;
                self.mappings.remove(i);
                Ok(())
            }
            ShowOp::AddMedia { items } => {
                // Validate all first so a failure leaves the show unchanged.
                for (n, it) in items.iter().enumerate() {
                    if self.media.iter().chain(&items[..n]).any(|m| m.id == it.id) {
                        return Err(format!("media {} already exists", it.id));
                    }
                }
                self.media.extend(items);
                Ok(())
            }
            ShowOp::UpdateMedia { item } => {
                let i = find(&self.media, item.id, |m| m.id, "media")?;
                self.media[i] = item;
                Ok(())
            }
            ShowOp::RemoveMedia { ids } => {
                // Validate all first so a failure leaves the show unchanged.
                for &id in &ids {
                    find(&self.media, id, |m| m.id, "media")?;
                }
                self.media.retain(|m| !ids.contains(&m.id));
                for cue in self.groups.iter_mut().flat_map(|g| &mut g.cues) {
                    for layers in cue.layers_by_screen.values_mut() {
                        layers.retain(|l| !matches!(&l.source, Source::Media { media_id } if ids.contains(media_id)));
                    }
                }
                Ok(())
            }
            ShowOp::MoveMedia { id, to } => {
                let i = find(&self.media, id, |m| m.id, "media")?;
                if to as usize >= self.media.len() {
                    return Err(format!("media index {to} out of range"));
                }
                let m = self.media.remove(i);
                self.media.insert(to as usize, m);
                Ok(())
            }
            ShowOp::AddGroup { name } => {
                self.groups.push(Group { id: Uuid::new_v4(), name, cues: vec![] });
                Ok(())
            }
            ShowOp::RenameGroup { id, name } => {
                self.group_mut(id)?.name = name;
                Ok(())
            }
            ShowOp::RemoveGroup { id } => {
                let i = find(&self.groups, id, |g| g.id, "group")?;
                if self.groups.len() == 1 {
                    return Err("cannot remove the last group".into());
                }
                self.groups.remove(i);
                Ok(())
            }
            ShowOp::SetCue { group_id, cue } => set_cue(self.group_mut(group_id)?, cue),
            ShowOp::MoveCue { group_id, from, to } => move_cue(self.group_mut(group_id)?, from, to),
            ShowOp::DuplicateCue { group_id, from, to } => duplicate_cue(self.group_mut(group_id)?, from, to),
            ShowOp::DeleteCue { group_id, slot } => {
                delete_cue(self.group_mut(group_id)?, slot);
                Ok(())
            }
            ShowOp::SetMasterVolume { volume } => {
                self.master_volume = volume.clamp(0.0, 1.0);
                Ok(())
            }
        }
    }

    fn group_mut(&mut self, id: Uuid) -> Result<&mut Group, String> {
        let i = find(&self.groups, id, |g| g.id, "group")?;
        Ok(&mut self.groups[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Layer, MediaKind, OutputTarget, Scene, Transition, TransitionKind};

    fn rect() -> Rect { Rect { x: 0., y: 0., w: 10., h: 10. } }
    fn screen() -> Screen { Screen { id: Uuid::new_v4(), name: "S".into(), width: 10, height: 10, layout_x: 0, layout_y: 0 } }
    fn output() -> Output {
        Output { id: Uuid::new_v4(), name: "O".into(), node: "local".into(),
            target: OutputTarget::Window { x: 0, y: 0 }, width: 10, height: 10, brightness: 1.0 }
    }
    fn mapping(s: Uuid, o: Uuid) -> Mapping {
        Mapping { id: Uuid::new_v4(), screen_id: s, src: rect(), output_id: o, dst: rect() }
    }
    fn item() -> MediaItem {
        MediaItem { id: Uuid::new_v4(), name: "m".into(), path: "m.mp4".into(), kind: MediaKind::Video, width: 10,
            height: 10, duration_ms: None, has_audio: false, has_alpha: false, thumb: None }
    }
    fn cue(slot: u32, scene: Scene) -> Cue {
        Cue { id: Uuid::new_v4(), name: "c".into(), slot, layers_by_screen: scene,
            transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 }, hotkey: None }
    }

    #[test]
    fn remove_screen_cascades() {
        let mut show = Show::new();
        let (s1, s2, o) = (screen(), screen(), output());
        show.mappings = vec![mapping(s1.id, o.id), mapping(s2.id, o.id)];
        let l = Layer::new(Source::Pattern { pattern: Pattern::Bars }, rect());
        show.groups[0].cues.push(cue(0, Scene::from([(s1.id, vec![l.clone()]), (s2.id, vec![l])])));
        let id = s1.id;
        show.screens = vec![s1, s2];
        show.outputs = vec![o];
        show.apply(ShowOp::RemoveScreen { id }).unwrap();
        assert_eq!(show.screens.len(), 1);
        assert_eq!(show.mappings.len(), 1);
        assert!(show.mappings.iter().all(|m| m.screen_id != id));
        assert!(!show.groups[0].cues[0].layers_by_screen.contains_key(&id));
        assert_eq!(show.groups[0].cues[0].layers_by_screen.len(), 1);
    }

    #[test]
    fn remove_output_cascades() {
        let mut show = Show::new();
        let (s, o1, o2) = (screen(), output(), output());
        show.mappings = vec![mapping(s.id, o1.id), mapping(s.id, o2.id)];
        let id = o1.id;
        show.screens = vec![s];
        show.outputs = vec![o1, o2];
        show.apply(ShowOp::RemoveOutput { id }).unwrap();
        assert_eq!(show.outputs.len(), 1);
        assert_eq!(show.mappings.len(), 1);
        assert!(show.mappings.iter().all(|m| m.output_id != id));
    }

    #[test]
    fn remove_media_cascades() {
        let mut show = Show::new();
        let (m1, m2, s) = (item(), item(), Uuid::new_v4());
        let l1 = Layer::new(Source::Media { media_id: m1.id }, rect());
        let l2 = Layer::new(Source::Media { media_id: m2.id }, rect());
        let lp = Layer::new(Source::Pattern { pattern: Pattern::Grid }, rect());
        show.groups[0].cues.push(cue(0, Scene::from([(s, vec![l1.clone(), l2.clone(), lp.clone()])])));
        show.groups[0].cues.push(cue(1, Scene::from([(s, vec![l1])])));
        let id = m1.id;
        show.media = vec![m1, m2];
        show.apply(ShowOp::RemoveMedia { ids: vec![id] }).unwrap();
        assert_eq!(show.media.len(), 1);
        assert_eq!(show.groups[0].cues[0].layers_by_screen[&s], vec![l2, lp.clone()]);
        assert!(show.groups[0].cues[1].layers_by_screen[&s].is_empty());
        // Several at once; an unknown id leaves the show unchanged.
        let m2 = show.media[0].id;
        let before = show.clone();
        assert!(show.apply(ShowOp::RemoveMedia { ids: vec![m2, Uuid::new_v4()] }).is_err());
        assert_eq!(show, before);
        show.apply(ShowOp::RemoveMedia { ids: vec![m2] }).unwrap();
        assert!(show.media.is_empty());
        assert_eq!(show.groups[0].cues[0].layers_by_screen[&s], vec![lp]);
    }

    #[test]
    fn move_media_reorders() {
        let mut show = Show::new();
        let (a, b, c) = (item(), item(), item());
        show.media = vec![a.clone(), b.clone(), c.clone()];
        show.apply(ShowOp::MoveMedia { id: a.id, to: 2 }).unwrap();
        assert_eq!(show.media, [b.clone(), c.clone(), a.clone()]);
        show.apply(ShowOp::MoveMedia { id: a.id, to: 0 }).unwrap();
        assert_eq!(show.media, [a.clone(), b.clone(), c.clone()]);
        show.apply(ShowOp::MoveMedia { id: b.id, to: 1 }).unwrap();
        assert_eq!(show.media, [a.clone(), b, c]);
        // Out of range or unknown: unchanged.
        let before = show.clone();
        assert!(show.apply(ShowOp::MoveMedia { id: a.id, to: 3 }).is_err());
        assert!(show.apply(ShowOp::MoveMedia { id: Uuid::new_v4(), to: 0 }).is_err());
        assert_eq!(show, before);
    }

    #[test]
    fn remove_last_group_is_err() {
        let mut show = Show::new();
        let id = show.groups[0].id;
        assert!(show.apply(ShowOp::RemoveGroup { id }).is_err());
        assert_eq!(show.groups.len(), 1);
        show.apply(ShowOp::AddGroup { name: "G2".into() }).unwrap();
        show.apply(ShowOp::RemoveGroup { id }).unwrap();
        assert_eq!(show.groups.len(), 1);
    }

    #[test]
    fn unknown_id_is_err_and_unchanged() {
        let mut show = Show::new();
        show.screens.push(screen());
        let before = show.clone();
        assert!(show.apply(ShowOp::UpdateScreen { screen: screen() }).is_err());
        assert!(show.apply(ShowOp::RemoveOutput { id: Uuid::new_v4() }).is_err());
        assert!(show.apply(ShowOp::RenameGroup { id: Uuid::new_v4(), name: "x".into() }).is_err());
        assert!(show.apply(ShowOp::SetCue { group_id: Uuid::new_v4(), cue: cue(0, Scene::new()) }).is_err());
        assert!(show.apply(ShowOp::MoveCue { group_id: show.groups[0].id, from: 3, to: 4 }).is_err());
        assert_eq!(show, before);
    }

    #[test]
    fn master_volume_clamped() {
        let mut show = Show::new();
        show.apply(ShowOp::SetMasterVolume { volume: 3.0 }).unwrap();
        assert_eq!(show.master_volume, 1.0);
        show.apply(ShowOp::SetMasterVolume { volume: -1.0 }).unwrap();
        assert_eq!(show.master_volume, 0.0);
    }

    #[test]
    fn mapping_upsert_and_cue_ops() {
        let mut show = Show::new();
        let (s, o) = (screen(), output());
        let mut m = mapping(s.id, o.id);
        show.apply(ShowOp::SetMapping { mapping: m.clone() }).unwrap();
        m.dst.w = 5.;
        show.apply(ShowOp::SetMapping { mapping: m.clone() }).unwrap();
        assert_eq!(show.mappings, vec![m]);
        let g = show.groups[0].id;
        show.apply(ShowOp::SetCue { group_id: g, cue: cue(1, Scene::new()) }).unwrap();
        show.apply(ShowOp::DuplicateCue { group_id: g, from: 1, to: 2 }).unwrap();
        show.apply(ShowOp::MoveCue { group_id: g, from: 1, to: 3 }).unwrap();
        show.apply(ShowOp::DeleteCue { group_id: g, slot: 2 }).unwrap();
        assert_eq!(show.groups[0].cues.len(), 1);
        assert_eq!(show.groups[0].cues[0].slot, 3);
    }

    #[test]
    fn non_finite_floats_rejected() {
        let mut show = Show::new();
        let (s, o) = (screen(), output());
        show.apply(ShowOp::AddScreen { screen: s.clone() }).unwrap();
        show.apply(ShowOp::AddOutput { output: o.clone() }).unwrap();
        let g = show.groups[0].id;
        let before = show.clone();
        let layer = || Layer::new(Source::Pattern { pattern: Pattern::Bars }, rect());
        let with_layer = |l: Layer| ShowOp::SetCue { group_id: g, cue: cue(0, Scene::from([(s.id, vec![l])])) };
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut ops = vec![ShowOp::SetMasterVolume { volume: bad }];
            let mut out = o.clone();
            out.brightness = bad;
            ops.push(ShowOp::UpdateOutput { output: out.clone() });
            out.id = Uuid::new_v4();
            ops.push(ShowOp::AddOutput { output: out });
            let mut m = mapping(s.id, o.id);
            m.src.w = bad;
            ops.push(ShowOp::SetMapping { mapping: m.clone() });
            m.src = rect();
            m.dst.y = bad;
            ops.push(ShowOp::SetMapping { mapping: m });
            let mut l = layer();
            l.rect.x = bad;
            ops.push(with_layer(l));
            for f in 0..11 {
                let mut l = layer();
                let c = &mut l.crop;
                *[&mut l.opacity, &mut l.brightness, &mut l.contrast, &mut l.saturation, &mut l.volume, &mut l.rotation, &mut l.speed, &mut c.left, &mut c.top, &mut c.right, &mut c.bottom][f] = bad;
                ops.push(with_layer(l));
            }
            let solid = Layer::new(Source::Pattern { pattern: Pattern::Solid { rgba: [0., bad, 0., 1.] } }, rect());
            ops.push(with_layer(solid));
            for op in ops {
                assert!(show.apply(op.clone()).is_err(), "accepted {op:?}");
                assert_eq!(show, before);
            }
        }
        // finite values still pass
        show.apply(with_layer(layer())).unwrap();
    }

    #[test]
    fn op_json_tag() {
        let json = serde_json::to_string(&ShowOp::SetMasterVolume { volume: 0.5 }).unwrap();
        assert_eq!(json, r#"{"op":"SetMasterVolume","volume":0.5}"#);
    }
}
