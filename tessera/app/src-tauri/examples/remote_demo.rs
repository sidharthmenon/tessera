//! The remote-control API over a demo show, with no engine and no window: for trying the
//! Companion module (or curl) without running Tessera.
//!
//! `cargo run --example remote_demo -- [--port 7411] [--key demo] [--host 127.0.0.1]`

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, RwLock};

use tessera_app_lib::controller::Controller;
use tessera_app_lib::remote::{self, Command, Control, Refused, Snapshot};
use tessera_show::{Cue, Layer, Pattern, Rect, Scene, Screen, ShowOp, Source, Transition, TransitionKind};
use tokio::sync::watch;

struct Demo {
    ctl: Mutex<Controller>,
    changes: watch::Sender<u64>,
}

impl Control for Demo {
    fn run(&self, command: Command) -> Result<(), Refused> {
        remote::apply(&mut self.ctl.lock().unwrap(), &command, true)?;
        self.changes.send_modify(|n| *n += 1);
        println!("ran {command:?}");
        Ok(())
    }
    fn state(&self) -> Snapshot {
        remote::snapshot(&self.ctl.lock().unwrap(), "connected")
    }
    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

/// Two groups of pattern cues on one screen.
fn demo_show() -> Controller {
    let mut c = Controller::new();
    let screen = Screen { id: uuid::Uuid::new_v4(), name: "LED wall".into(), width: 1920, height: 1080, layout_x: 0, layout_y: 0 };
    let s = screen.id;
    c.apply(ShowOp::AddScreen { screen }).unwrap();
    let first = c.show.groups[0].id;
    c.apply(ShowOp::RenameGroup { id: first, name: "Main".into() }).unwrap();
    c.apply(ShowOp::AddGroup { name: "Stingers".into() }).unwrap();
    let groups = [c.show.groups[0].id, c.show.groups[1].id];
    let cues = [(0, 0, "Walk-in", Pattern::Gradient), (0, 1, "Opener", Pattern::Bars), (0, 4, "Keynote", Pattern::Grid), (1, 0, "Logo sting", Pattern::Solid { rgba: [1., 0.5, 0., 1.] })];
    for (g, slot, name, pattern) in cues {
        let layer = Layer { name: name.into(), ..Layer::new(Source::Pattern { pattern }, Rect { x: 0., y: 0., w: 1920., h: 1080. }) };
        let cue = Cue {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            slot,
            layers_by_screen: Scene::from([(s, vec![layer])]),
            transition: Transition { kind: TransitionKind::Fade, duration_ms: 500 },
            hotkey: None,
        };
        c.apply(ShowOp::SetCue { group_id: groups[g], cue }).unwrap();
    }
    c
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str, default: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or(default.into());
    let (host, port, key) = (arg("--host", "127.0.0.1"), arg("--port", "7411").parse::<u16>().expect("--port"), arg("--key", "demo"));
    let demo = Arc::new(Demo { ctl: Mutex::new(demo_show()), changes: watch::Sender::new(0) });
    let router = remote::router(demo, Arc::new(RwLock::new(key.clone())), Arc::new(AtomicUsize::new(0)), Arc::new(|| {}));
    let server = remote::start(router, &host, port).expect("start");
    println!("Tessera remote demo on http://{}  key: {key}", server.addr);
    loop {
        std::thread::park();
    }
}
