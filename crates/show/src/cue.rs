use std::collections::HashMap;

use uuid::Uuid;

use crate::model::{Cue, Group, Scene, Source};

/// Insert `cue`, replacing any existing cue in the same slot.
pub fn set_cue(group: &mut Group, cue: Cue) {
    match group.cues.iter_mut().find(|c| c.slot == cue.slot) {
        Some(existing) => *existing = cue,
        None => group.cues.push(cue),
    }
}

/// Move the cue at `from` to `to`; swaps with the occupant of `to` if any.
pub fn move_cue(group: &mut Group, from: u32, to: u32) -> Result<(), String> {
    if !group.cues.iter().any(|c| c.slot == from) {
        return Err(format!("no cue in slot {from}"));
    }
    for c in &mut group.cues {
        if c.slot == from {
            c.slot = to;
        } else if c.slot == to {
            c.slot = from;
        }
    }
    Ok(())
}

pub fn duplicate_cue(group: &mut Group, from: u32, to: u32) -> Result<(), String> {
    if group.cues.iter().any(|c| c.slot == to) {
        return Err(format!("slot {to} occupied"));
    }
    let mut copy = group.cues.iter().find(|c| c.slot == from).ok_or(format!("no cue in slot {from}"))?.clone();
    copy.id = Uuid::new_v4();
    copy.slot = to;
    copy.layers_by_screen.values_mut().flatten().for_each(|l| l.id = Uuid::new_v4());
    group.cues.push(copy);
    Ok(())
}

pub fn delete_cue(group: &mut Group, slot: u32) {
    group.cues.retain(|c| c.slot != slot);
}

/// Lowest occupied slot greater than `after` (lowest overall for `None`). No wrap.
pub fn next_cue_slot(group: &Group, after: Option<u32>) -> Option<u32> {
    group.cues.iter().map(|c| c.slot).filter(|s| after.is_none_or(|a| *s > a)).min()
}

/// Screens defined in `incoming` replace those in `pgm`; others are kept.
pub fn take_merge(pgm: &Scene, incoming: &Scene) -> Scene {
    let mut out = pgm.clone();
    out.extend(incoming.iter().map(|(k, v)| (*k, v.clone())));
    out
}

/// Screens whose layers differ between `old` and `new` (including added/removed).
pub fn changed_screens(old: &Scene, new: &Scene) -> Vec<Uuid> {
    old.keys().chain(new.keys()).collect::<std::collections::BTreeSet<_>>().into_iter()
        .filter(|k| old.get(k) != new.get(k)).copied().collect()
}

/// Incoming layer id -> pgm layer id, for media layers on the same screen, same media, same index.
pub fn resolve_shared(pgm: &Scene, incoming: &Scene) -> HashMap<Uuid, Uuid> {
    let mut map = HashMap::new();
    for (screen, layers) in incoming {
        let Some(live) = pgm.get(screen) else { continue };
        for (inc, cur) in layers.iter().zip(live) {
            if let (Source::Media { media_id: a }, Source::Media { media_id: b }) = (&inc.source, &cur.source) {
                if a == b {
                    map.insert(inc.id, cur.id);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::model::{Layer, Pattern, Transition, TransitionKind};

    fn rect() -> Rect { Rect { x: 0., y: 0., w: 10., h: 10. } }
    fn media(id: Uuid) -> Layer { Layer::new(Source::Media { media_id: id }, rect()) }
    fn pattern() -> Layer { Layer::new(Source::Pattern { pattern: Pattern::Bars }, rect()) }
    fn cue(slot: u32, scene: Scene) -> Cue {
        Cue { id: Uuid::new_v4(), name: format!("c{slot}"), slot, layers_by_screen: scene,
            transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 } }
    }
    fn group(slots: &[u32]) -> Group {
        Group { id: Uuid::new_v4(), name: "g".into(), cues: slots.iter().map(|s| cue(*s, Scene::new())).collect() }
    }

    #[test]
    fn move_cue_swaps() {
        let mut g = group(&[1, 2]);
        let (a, b) = (g.cues[0].id, g.cues[1].id);
        move_cue(&mut g, 1, 2).unwrap();
        assert_eq!(g.cues.iter().find(|c| c.id == a).unwrap().slot, 2);
        assert_eq!(g.cues.iter().find(|c| c.id == b).unwrap().slot, 1);
        move_cue(&mut g, 2, 5).unwrap();
        assert_eq!(g.cues.iter().find(|c| c.id == a).unwrap().slot, 5);
        assert!(move_cue(&mut g, 9, 1).is_err());
    }

    #[test]
    fn duplicate_cue_fresh_ids() {
        let mut g = group(&[]);
        let s = Uuid::new_v4();
        g.cues.push(cue(1, Scene::from([(s, vec![pattern(), pattern()])])));
        duplicate_cue(&mut g, 1, 2).unwrap();
        let (o, c) = (&g.cues[0], &g.cues[1]);
        assert_ne!(o.id, c.id);
        assert_eq!(c.slot, 2);
        for l in &c.layers_by_screen[&s] {
            assert!(o.layers_by_screen[&s].iter().all(|x| x.id != l.id));
        }
        assert_eq!(c.layers_by_screen[&s].len(), 2);
    }

    #[test]
    fn duplicate_to_occupied_is_err() {
        let mut g = group(&[1, 2]);
        assert!(duplicate_cue(&mut g, 1, 2).is_err());
        assert!(duplicate_cue(&mut g, 5, 6).is_err());
    }

    #[test]
    fn set_and_delete() {
        let mut g = group(&[1]);
        set_cue(&mut g, cue(1, Scene::new()));
        assert_eq!(g.cues.len(), 1);
        set_cue(&mut g, cue(2, Scene::new()));
        assert_eq!(g.cues.len(), 2);
        delete_cue(&mut g, 1);
        assert_eq!(g.cues.len(), 1);
    }

    #[test]
    fn next_cue_slot_skips_gaps() {
        let g = group(&[7, 1, 3]);
        assert_eq!(next_cue_slot(&g, None), Some(1));
        assert_eq!(next_cue_slot(&g, Some(1)), Some(3));
        assert_eq!(next_cue_slot(&g, Some(7)), None);
    }

    #[test]
    fn take_merge_keeps_undefined_screens() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (la, lb, lc) = (pattern(), pattern(), pattern());
        let pgm = Scene::from([(a, vec![la]), (b, vec![lb.clone()])]);
        let inc = Scene::from([(a, vec![lc.clone()])]);
        assert_eq!(take_merge(&pgm, &inc), Scene::from([(a, vec![lc]), (b, vec![lb])]));
    }

    #[test]
    fn take_merge_empty_vec_blacks_screen() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let pgm = Scene::from([(a, vec![pattern()]), (b, vec![pattern()])]);
        let out = take_merge(&pgm, &Scene::from([(b, vec![])]));
        assert_eq!(out[&b], vec![]);
        assert_eq!(out[&a], pgm[&a]);
    }

    #[test]
    fn changed_screens_detects_only_diffs() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (la, lb) = (pattern(), pattern());
        let old = Scene::from([(a, vec![la.clone()]), (b, vec![lb.clone()])]);
        let new = Scene::from([(a, vec![la]), (b, vec![pattern()]), (c, vec![pattern()])]);
        let mut got = changed_screens(&old, &new);
        got.sort();
        let mut want = vec![b, c];
        want.sort();
        assert_eq!(got, want);
        // removed screen counts as changed
        assert_eq!(changed_screens(&old, &Scene::new()).len(), 2);
    }

    #[test]
    fn resolve_shared_same_media_same_index() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (m1, m2) = (Uuid::new_v4(), Uuid::new_v4());
        let bg = media(m1);
        let pgm = Scene::from([(a, vec![bg.clone()])]);
        let (bg2, ov) = (media(m1), media(m2));
        let inc = Scene::from([(a, vec![bg2.clone(), ov])]);
        assert_eq!(resolve_shared(&pgm, &inc), HashMap::from([(bg2.id, bg.id)]));
        // different index
        let inc = Scene::from([(a, vec![media(m2), media(m1)])]);
        assert!(resolve_shared(&pgm, &inc).is_empty());
        // different screen
        let inc = Scene::from([(b, vec![media(m1)])]);
        assert!(resolve_shared(&pgm, &inc).is_empty());
        // patterns never shared
        let p = Scene::from([(a, vec![pattern()])]);
        assert!(resolve_shared(&p, &Scene::from([(a, vec![pattern()])])).is_empty());
    }
}
