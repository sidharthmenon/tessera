use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, bail};
use tessera_engine::app::{App, event_loop};
use tessera_engine::net::{self, Previews};
use tessera_show::{
    EngineConfig, FromEngine, Layer, Mapping, MediaItem, MediaKind, Output, OutputTarget, Pattern, Rect, Scene, Screen, Source,
    ToEngine, Transition, TransitionKind,
};
use tokio::sync::broadcast;
use uuid::Uuid;

const USAGE: &str = "usage: tessera-engine [--port <u16>] [--cache-dir <path>] [--demo | --demo-4k <clip dir>]
  env TESSERA_TOKEN: required unless --demo/--demo-4k; every request must carry ?token=<it>";
/// The `--demo` token when TESSERA_TOKEN isn't set.
const DEV_TOKEN: &str = "tessera-dev";

#[derive(Default)]
struct Args {
    /// 0: any free port (printed on stdout).
    port: u16,
    cache_dir: Option<PathBuf>,
    demo: bool,
    demo_4k: Option<PathBuf>,
}

fn args() -> anyhow::Result<Args> {
    let (mut args, mut argv) = (Args::default(), std::env::args().skip(1));
    while let Some(arg) = argv.next() {
        let mut value = || argv.next().with_context(|| format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--port" => args.port = value()?.parse().with_context(|| format!("--port: not a port number\n{USAGE}"))?,
            "--cache-dir" => args.cache_dir = Some(value()?.into()),
            "--demo" => args.demo = true,
            "--demo-4k" => args.demo_4k = Some(value()?.into()),
            _ => bail!("unknown argument {arg:?}\n{USAGE}"),
        }
    }
    Ok(args)
}

/// The OS per-user cache dir + `/tessera` (the temp dir if there's none).
fn default_cache_dir() -> PathBuf {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Caches"))
    } else if cfg!(windows) {
        var("LOCALAPPDATA")
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|home| home.join(".cache")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("tessera")
}

fn main() -> anyhow::Result<()> {
    let args = args()?;
    let demo_mode = args.demo || args.demo_4k.is_some();
    let token = match std::env::var("TESSERA_TOKEN") {
        Ok(token) if !token.is_empty() => token,
        _ if demo_mode => DEV_TOKEN.into(),
        _ => bail!("TESSERA_TOKEN is not set\n{USAGE}"),
    };
    // (commands now, commands 3 s later)
    let (demo, later) = if args.demo {
        let (config, screen) = demo(1080, vec![]);
        let scene = |pattern| Scene::from([(screen, vec![Layer::new(Source::Pattern { pattern }, rect(0., 0., 3840., 1080.))])]);
        let fade = Transition { kind: TransitionKind::Fade, duration_ms: 1000 };
        let now = vec![ToEngine::SetConfig { config }, ToEngine::SetProgram { scene: scene(Pattern::Bars) }];
        (now, vec![ToEngine::SetPreview { scene: scene(Pattern::Grid) }, ToEngine::Take { transition: fade }])
    } else if let Some(dir) = &args.demo_4k {
        (demo_4k(dir), vec![])
    } else {
        (vec![], vec![])
    };
    let (event_loop, handle) = event_loop()?;
    // The app's replies, to every WebSocket client (and the log).
    let (events, replies) = mpsc::channel();
    let (clients, _) = broadcast::channel(256);
    let to_clients = clients.clone();
    std::thread::spawn(move || {
        for event in replies {
            if !matches!(event, FromEngine::Positions { .. }) {
                eprintln!("engine -> {event:?}");
            }
            let _ = to_clients.send(event); // no clients connected: dropped
        }
    });
    let previews = Previews::new();
    let cache = args.cache_dir.unwrap_or_else(default_cache_dir);
    let listen = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let addr = net::serve(listen, token.clone(), handle.clone(), clients, previews.clone(), cache.join("thumbs"))?;
    // The controller reads the port from this line.
    println!("TESSERA_ENGINE_LISTENING {}", addr.port());
    if demo_mode && token == DEV_TOKEN {
        eprintln!("demo token {DEV_TOKEN}: http://{addr}/preview/pgm.mjpg?token={DEV_TOKEN}");
    }
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
    event_loop.run_app(&mut App::new(events, previews)?)?;
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
