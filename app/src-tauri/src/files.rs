//! Show files: JSON `.tshow`, atomic writes, autosave paths. In memory every
//! `MediaItem.path` is absolute; on disk it is relative to the show file's folder
//! when the media lives inside it.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use tessera_show::Show;

/// Read a show file (`.tshow` or `.autosave`). Relative media paths come back absolute.
pub fn open(path: &Path) -> Result<Show, String> {
    let path = &std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Version first: a newer file may not parse as today's Show at all.
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("not a show file: {e}"))?;
    if json.get("version").and_then(|v| v.as_u64()).is_some_and(|v| v > 1) {
        return Err("show file is from a newer Tessera".into());
    }
    let mut show: Show = serde_json::from_value(json).map_err(|e| format!("not a show file: {e}"))?;
    for group in &show.groups {
        let mut slots = HashSet::new();
        if let Some(cue) = group.cues.iter().find(|c| !slots.insert(c.slot)) {
            return Err(format!("group {:?} has two cues in slot {}", group.name, cue.slot));
        }
    }
    let dir = path.parent().unwrap_or(Path::new(""));
    for m in &mut show.media {
        m.path = normalize(&dir.join(&m.path)).to_string_lossy().into(); // join keeps an absolute path
    }
    Ok(show)
}

/// Save atomically (see `write_atomic`): a crash leaves either the old file or the new one,
/// never half of one.
pub fn save_atomic(path: &Path, show: &Show) -> Result<(), String> {
    let abs = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = normalize(abs.parent().unwrap_or(Path::new("")));
    let mut show = show.clone();
    for m in &mut show.media {
        // Normalized first, so a relative path never climbs out with "..".
        let media = normalize(Path::new(&m.path));
        m.path = match media.strip_prefix(&dir) {
            // '/' so a show folder moves between macOS and Windows.
            Ok(rel) => rel.iter().map(|c| c.to_string_lossy()).collect::<Vec<_>>().join("/"),
            Err(_) => media.to_string_lossy().into(),
        };
    }
    let json = serde_json::to_vec_pretty(&show).map_err(|e| e.to_string())?;
    write_atomic(path, &json, false)
}

/// `bytes` to `<path>.tmp`, fsync, rename over `path`. `private`: owner-only (unix 0600).
#[cfg_attr(not(unix), allow(unused_variables))]
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    let tmp = suffixed(path, ".tmp");
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    // ponytail: no fsync of the folder after the rename; add it if a power cut may lose the rename.
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("saving {}: {e}", path.display())
    })
}

/// Unsaved work from an earlier session, moved to `<autosave>.prev` so this session's
/// autosaves can't overwrite it: the untitled autosave, or `<show>.autosave` when it is newer
/// than the show file. An older `.prev` is never overwritten (I8): the work goes to
/// `<autosave>.<unix ms>.prev` then. Returns where it went (the newest).
pub fn take_recovery(show_path: Option<&Path>, data_dir: &Path) -> Option<PathBuf> {
    let auto = autosave_path(show_path, data_dir);
    let auto_time = std::fs::metadata(&auto).and_then(|m| m.modified()).ok()?;
    let saved_since = show_path
        .and_then(|show| std::fs::metadata(show).and_then(|m| m.modified()).ok())
        .is_some_and(|show_time| show_time >= auto_time);
    if saved_since {
        return None;
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let prev = std::iter::once(suffixed(&auto, ".prev"))
        .chain((0..).map(|n| suffixed(&auto, &if n == 0 { format!(".{ms}.prev") } else { format!(".{ms}-{n}.prev") })))
        .find(|p| !p.exists())?;
    // ponytail: exists-then-rename can race another instance; the single-instance guard (I7) rules that out.
    std::fs::rename(&auto, &prev).ok()?;
    Some(prev)
}

/// An autosave or a kept one (`….autosave`, `….autosave.prev`, `….autosave.<time>.prev`): it opens
/// as untitled work, never as the show it came from.
pub fn is_autosave(p: &Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    name.ends_with(".autosave") || (name.ends_with(".prev") && name.contains(".autosave."))
}

/// `p` with `.` and `..` resolved lexically (no filesystem access; symlinks are not followed).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => _ = out.pop(),
                Some(Component::RootDir | Component::Prefix(_)) => {} // "/.." is "/"
                _ => out.push(".."),
            },
            c => out.push(c),
        }
    }
    out
}

fn suffixed(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

/// `<show>.autosave` next to the show, or `<data_dir>/untitled.tshow.autosave` for an untitled one.
pub fn autosave_path(show_path: Option<&Path>, data_dir: &Path) -> PathBuf {
    match show_path {
        Some(p) => suffixed(p, ".autosave"),
        None => data_dir.join("untitled.tshow.autosave"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tessera_show::{Cue, MediaItem, MediaKind, Scene, Transition, TransitionKind};
    use uuid::Uuid;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tessera-app-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn media(path: &Path) -> MediaItem {
        MediaItem { id: Uuid::new_v4(), name: "m".into(), path: path.to_string_lossy().into(), kind: MediaKind::Video,
            width: 10, height: 10, duration_ms: None, has_audio: false, has_alpha: false, thumb: None }
    }

    fn cue(slot: u32) -> Cue {
        Cue { id: Uuid::new_v4(), name: "c".into(), slot, layers_by_screen: Scene::new(),
            transition: Transition { kind: TransitionKind::Cut, duration_ms: 0 }, hotkey: None }
    }

    #[test]
    fn open_garbage_is_err() {
        let dir = temp_dir();
        let path = dir.join("bad.tshow");
        for garbage in ["", "not json {", "[1, 2]", r#"{"version": 1}"#] {
            std::fs::write(&path, garbage).unwrap();
            assert!(open(&path).is_err(), "opened {garbage:?}");
        }
        assert!(open(&dir.join("missing.tshow")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_future_version_is_err() {
        let dir = temp_dir();
        let path = dir.join("future.tshow");
        // A newer file may not even parse as today's Show: the version is checked first.
        std::fs::write(&path, r#"{"version": 2, "somethingNew": true}"#).unwrap();
        assert_eq!(open(&path), Err("show file is from a newer Tessera".into()));
        let mut json: serde_json::Value = serde_json::to_value(Show::new()).unwrap();
        json["version"] = 2.into();
        std::fs::write(&path, json.to_string()).unwrap();
        assert_eq!(open(&path), Err("show file is from a newer Tessera".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_rejects_duplicate_slots() {
        let dir = temp_dir();
        let path = dir.join("dup.tshow");
        let mut show = Show::new();
        show.groups[0].cues = vec![cue(1), cue(2)];
        save_atomic(&path, &show).unwrap();
        assert_eq!(open(&path).unwrap(), show);
        show.groups[0].cues.push(cue(1));
        std::fs::write(&path, serde_json::to_string(&show).unwrap()).unwrap();
        let err = open(&path).unwrap_err();
        assert!(err.contains("slot 1"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_is_atomic() {
        let dir = temp_dir();
        let path = dir.join("show.tshow");
        let mut show = Show::new();
        save_atomic(&path, &show).unwrap();
        show.master_volume = 0.25;
        save_atomic(&path, &show).unwrap(); // over an existing file
        assert_eq!(open(&path).unwrap(), show);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["show.tshow"], "temp file left behind");
        // A read-only folder: the temp file can't be created, the original stays intact.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let mut other = show.clone();
            other.master_volume = 0.75;
            let result = save_atomic(&path, &other);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(result.is_err());
            assert_eq!(open(&path).unwrap(), show);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_path_relative_inside_show_dir() {
        let (dir, elsewhere) = (temp_dir(), temp_dir());
        let inside = dir.join("media").join("clip.mp4");
        let outside = elsewhere.join("logo.png");
        let mut show = Show::new();
        show.media = vec![media(&inside), media(&outside)];
        let path = dir.join("show.tshow");
        save_atomic(&path, &show).unwrap();
        let raw: Show = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw.media[0].path, "media/clip.mp4");
        assert_eq!(raw.media[1].path, outside.to_string_lossy());
        // Opening resolves back to absolute paths.
        assert_eq!(open(&path).unwrap(), show);
        // Saving elsewhere re-relativizes: the clip is now outside the show's folder.
        let moved = elsewhere.join("moved.tshow");
        save_atomic(&moved, &show).unwrap();
        let raw: Show = serde_json::from_str(&std::fs::read_to_string(&moved).unwrap()).unwrap();
        assert_eq!(raw.media[0].path, inside.to_string_lossy());
        assert_eq!(raw.media[1].path, "logo.png");
        assert_eq!(open(&moved).unwrap(), show);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn save_never_stores_parent_relative_paths() {
        let (dir, elsewhere) = (temp_dir(), temp_dir());
        let name = elsewhere.file_name().unwrap().to_str().unwrap().to_owned();
        let mut show = Show::new();
        show.media = vec![
            media(&dir.join("a/../b/x.mp4")),               // inside, after normalizing
            media(&dir.join("./z.png")),                     // inside
            media(&dir.join("..").join(&name).join("y.mp4")), // outside, spelled from inside
        ];
        let path = dir.join("show.tshow");
        save_atomic(&path, &show).unwrap();
        let raw: Show = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stored: Vec<_> = raw.media.iter().map(|m| m.path.clone()).collect();
        assert_eq!(stored, ["b/x.mp4".to_string(), "z.png".into(), elsewhere.join("y.mp4").to_string_lossy().into()]);
        let opened: Vec<_> = open(&path).unwrap().media.into_iter().map(|m| m.path).collect();
        assert_eq!(opened, [dir.join("b/x.mp4"), dir.join("z.png"), elsewhere.join("y.mp4")]
            .map(|p| p.to_string_lossy().into_owned()));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(elsewhere).unwrap();
    }

    fn set_mtime(path: &Path, t: std::time::SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn recovery_moves_newer_autosave_aside() {
        let dir = temp_dir();
        let show = dir.join("x.tshow");
        save_atomic(&show, &Show::new()).unwrap();
        let auto = autosave_path(Some(&show), &dir);
        let mut unsaved = Show::new();
        unsaved.master_volume = 0.3;
        save_atomic(&auto, &unsaved).unwrap();
        let now = std::time::SystemTime::now();
        set_mtime(&show, now - Duration::from_secs(60));
        set_mtime(&auto, now);
        let prev = take_recovery(Some(&show), &dir).expect("newer autosave is recoverable");
        assert_eq!(prev, dir.join("x.tshow.autosave.prev"));
        assert!(!auto.exists(), "moved, so this session's autosave can't overwrite it");
        assert_eq!(open(&prev).unwrap(), unsaved);
        // An autosave older than the show file was saved since: nothing to recover.
        save_atomic(&auto, &unsaved).unwrap();
        set_mtime(&auto, now - Duration::from_secs(120));
        assert_eq!(take_recovery(Some(&show), &dir), None);
        assert!(auto.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn untitled_autosave_moved_aside() {
        let data = temp_dir();
        assert_eq!(take_recovery(None, &data), None);
        let auto = autosave_path(None, &data);
        save_atomic(&auto, &Show::new()).unwrap();
        let prev = take_recovery(None, &data).unwrap();
        assert_eq!(prev, data.join("untitled.tshow.autosave.prev"));
        assert!(!auto.exists() && prev.exists());
        std::fs::remove_dir_all(data).unwrap();
    }

    /// I8: a later session's unsaved work never overwrites an earlier one's: both are kept, the
    /// newest is the one offered.
    #[test]
    fn recovery_never_overwrites_prev() {
        let data = temp_dir();
        let auto = autosave_path(None, &data);
        let mut kept = vec![];
        for volume in [0.1, 0.2, 0.3] {
            let mut show = Show::new();
            show.master_volume = volume;
            save_atomic(&auto, &show).unwrap();
            let prev = take_recovery(None, &data).expect("recoverable");
            assert!(is_autosave(&prev), "{}", prev.display());
            kept.push((prev, show));
        }
        assert_eq!(kept[0].0, data.join("untitled.tshow.autosave.prev"));
        let name = kept[1].0.file_name().unwrap().to_string_lossy().into_owned();
        let stamp = name.strip_prefix("untitled.tshow.autosave.").and_then(|n| n.strip_suffix(".prev"));
        assert!(stamp.is_some_and(|t| t.chars().all(|c| c.is_ascii_digit() || c == '-')), "{name}");
        for (prev, show) in &kept {
            assert_eq!(&open(prev).unwrap(), show, "{}", prev.display());
        }
        assert_ne!(kept[1].0, kept[2].0);
        assert!(!is_autosave(Path::new("/s/show.tshow")) && !is_autosave(Path::new("/s/prev.prev")));
        assert!(is_autosave(Path::new("/s/show.tshow.autosave")));
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn autosave_paths() {
        let data = Path::new("/data");
        assert_eq!(autosave_path(None, data), Path::new("/data/untitled.tshow.autosave"));
        assert_eq!(autosave_path(Some(Path::new("/shows/a.tshow")), data), Path::new("/shows/a.tshow.autosave"));
        // open accepts .autosave files
        let dir = temp_dir();
        let path = autosave_path(Some(&dir.join("a.tshow")), data);
        save_atomic(&path, &Show::new()).unwrap();
        assert!(open(&path).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
