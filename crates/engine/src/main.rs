use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use tessera_engine::app::{App, event_loop};
use tessera_show::{
    EngineConfig, Layer, Mapping, MediaItem, MediaKind, Output, OutputTarget, Pattern, Rect, Scene, Screen, Source, ToEngine,
    Transition, TransitionKind,
};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // (commands now, commands 3 s later)
    let (demo, later) = if args.iter().any(|a| a == "--demo") {
        let (config, screen) = demo(1080, vec![]);
        let scene = |pattern| Scene::from([(screen, vec![Layer::new(Source::Pattern { pattern }, rect(0., 0., 3840., 1080.))])]);
        let fade = Transition { kind: TransitionKind::Fade, duration_ms: 1000 };
        let now = vec![ToEngine::SetConfig { config }, ToEngine::SetProgram { scene: scene(Pattern::Bars) }];
        (now, vec![ToEngine::SetPreview { scene: scene(Pattern::Grid) }, ToEngine::Take { transition: fade }])
    } else if let Some(i) = args.iter().position(|a| a == "--demo-4k") {
        (demo_4k(Path::new(args.get(i + 1).context("--demo-4k needs a clip directory")?)), vec![])
    } else {
        (vec![], vec![])
    };
    let (event_loop, handle) = event_loop()?;
    let (events, replies) = mpsc::channel();
    // Task 10 forwards these to the controller; for now just log them.
    std::thread::spawn(move || {
        for event in replies {
            eprintln!("engine -> {event:?}");
        }
    });
    for command in demo {
        handle.send(command);
    }
    if !later.is_empty() {
        let handle = handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            later.into_iter().for_each(|command| _ = handle.send(command));
        });
    }
    event_loop.run_app(&mut App::new(events)?)?;
    Ok(())
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// Perf check: the spike's four 4K clips (`gen_clips.sh` names) as full-screen layers, looped,
/// ProRes 4444 alpha on top.
fn demo_4k(dir: &Path) -> Vec<ToEngine> {
    let media: Vec<MediaItem> = ["h264.mp4", "hevc8.mp4", "hevc10.mp4", "prores4444a.mov"]
        .map(|clip| MediaItem {
            id: Uuid::new_v4(),
            name: clip.into(),
            path: dir.join(clip).to_string_lossy().into(),
            kind: MediaKind::Video,
            width: 3840,
            height: 2160,
            duration_ms: None,
            has_audio: false,
            has_alpha: clip.ends_with(".mov"),
            thumb: None,
        })
        .into();
    let layers = media.iter().map(|m| Layer::new(Source::Media { media_id: m.id }, rect(0., 0., 3840., 2160.))).collect();
    let (config, screen) = demo(2160, media);
    vec![ToEngine::SetConfig { config }, ToEngine::SetProgram { scene: Scene::from([(screen, layers)]) }]
}

/// Smoke test: two windows side by side, each showing one half of a 3840×`height` screen at half
/// size (`--demo`: 960×540 windows of a Bars screen that fades to Grid after 3 s). Returns the
/// config and the screen's id.
fn demo(height: u32, media: Vec<MediaItem>) -> (EngineConfig, Uuid) {
    let window = |name: &str, x| Output {
        id: Uuid::new_v4(),
        name: name.into(),
        node: "local".into(),
        target: OutputTarget::Window { x, y: 100 },
        width: 960,
        height: height / 2,
        brightness: 1.0,
    };
    let (left, right) = (window("Demo Left", 0), window("Demo Right", 960));
    let screen = Screen { id: Uuid::new_v4(), name: "Wall".into(), width: 3840, height, layout_x: 0, layout_y: 0 };
    let half = |output: &Output, x| Mapping {
        id: Uuid::new_v4(),
        screen_id: screen.id,
        src: rect(x, 0., 1920., height as f32),
        output_id: output.id,
        dst: rect(0., 0., 960., (height / 2) as f32),
    };
    let mappings = vec![half(&left, 0.), half(&right, 1920.)];
    let id = screen.id;
    (EngineConfig { outputs: vec![left, right], screens: vec![screen], mappings, media }, id)
}
