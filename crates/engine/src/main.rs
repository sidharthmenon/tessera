use std::sync::mpsc;

use tessera_engine::app::{App, event_loop};
use tessera_show::{EngineConfig, Layer, Mapping, Output, OutputTarget, Pattern, Rect, Scene, Screen, Source, ToEngine};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let (event_loop, handle) = event_loop()?;
    let (events, replies) = mpsc::channel();
    // Task 10 forwards these to the controller; for now just log them.
    std::thread::spawn(move || {
        for event in replies {
            eprintln!("engine -> {event:?}");
        }
    });
    if std::env::args().any(|a| a == "--demo") {
        for command in demo() {
            handle.send(command);
        }
    }
    event_loop.run_app(&mut App::new(events)?)?;
    Ok(())
}

/// Smoke test: two 960×540 windows side by side, each showing one half of a 3840×1080 Bars screen.
fn demo() -> [ToEngine; 2] {
    let r = |x, y, w, h| Rect { x, y, w, h };
    let window = |name: &str, x| Output {
        id: Uuid::new_v4(),
        name: name.into(),
        node: "local".into(),
        target: OutputTarget::Window { x, y: 100 },
        width: 960,
        height: 540,
        brightness: 1.0,
    };
    let (left, right) = (window("Demo Left", 0), window("Demo Right", 960));
    let screen = Screen { id: Uuid::new_v4(), name: "Wall".into(), width: 3840, height: 1080, layout_x: 0, layout_y: 0 };
    let half = |output: &Output, x| Mapping {
        id: Uuid::new_v4(),
        screen_id: screen.id,
        src: r(x, 0., 1920., 1080.),
        output_id: output.id,
        dst: r(0., 0., 960., 540.),
    };
    let mappings = vec![half(&left, 0.), half(&right, 1920.)];
    let bars = Layer::new(Source::Pattern { pattern: Pattern::Bars }, r(0., 0., 3840., 1080.));
    let scene = Scene::from([(screen.id, vec![bars])]);
    let config = EngineConfig { outputs: vec![left, right], screens: vec![screen], mappings, media: vec![] };
    [ToEngine::SetConfig { config }, ToEngine::SetProgram { scene }]
}
