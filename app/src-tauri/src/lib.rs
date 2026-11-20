//! Tauri wiring: managed state, commands, events, autosave, the engine watchdog.
//!
//! Events to the frontend:
//! - `show-changed`: the full `Show`, after every show mutation
//! - `live-changed`: `Live { pvw, lastTaken }` (cue refs or null), after every command that may move them
//! - `engine-event`: every `FromEngine`, plus `Error`s about engine restarts
//! - `engine-info`: `EngineInfo { port, token }` on connect, `null` on disconnect

/// `eprintln!` that never panics: stderr may be a pipe whose reader is gone (a closed
/// terminal, a killed `tauri dev`), and the watchdog must outlive that.
macro_rules! log {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod controller;
pub mod engine_client;
pub mod files;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use controller::{Controller, CueRef, Live};
use engine_client::{Engine, EngineInfo};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tessera_show::{Bus, FromEngine, Show, ShowOp, ToEngine, TransportAction};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);

type Res<T> = Result<T, String>;

pub struct AppState {
    ctl: Mutex<Controller>,
    autosave: Notify,
    data_dir: PathBuf,
}

impl AppState {
    /// A panic elsewhere must not lock the operator out of the show.
    fn ctl(&self) -> MutexGuard<'_, Controller> {
        self.ctl.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Run `f` on the controller and queue its engine messages under the lock (keeps them in
/// order). `show_changed`: the Show may have changed (emit it, schedule an autosave).
fn dispatch(app: &AppHandle, show_changed: bool, f: impl FnOnce(&mut Controller) -> Res<Vec<ToEngine>>) -> Res<()> {
    let state = app.state::<AppState>();
    let (show, live) = {
        let mut ctl = state.ctl();
        app.state::<Engine>().send(f(&mut ctl)?);
        (show_changed.then(|| ctl.show.clone()), ctl.live())
    };
    if let Some(show) = show {
        let _ = app.emit("show-changed", show);
        state.autosave.notify_one();
    }
    let _ = app.emit("live-changed", live);
    Ok(())
}

/// Called by the engine client on (re)connect: the full state, plus queued requests the
/// resync doesn't cover (queued state messages are older than it).
pub(crate) fn resync(app: &AppHandle, rx: &mut mpsc::UnboundedReceiver<ToEngine>) -> Vec<ToEngine> {
    let state = app.state::<AppState>();
    let ctl = state.ctl();
    let superseded = |m: &ToEngine| {
        use ToEngine::*;
        matches!(m, SetConfig { .. } | SetPreview { .. } | SetProgram { .. } | SetMasterVolume { .. } | Take { .. })
    };
    let queued: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|m| !superseded(m)).collect();
    ctl.resync_messages().into_iter().chain(queued).collect()
}

/// Every engine event goes to the UI; probe results also update the show.
pub(crate) fn engine_event(app: &AppHandle, event: FromEngine) {
    if let FromEngine::Probed { media_id, info: Some(info), .. } = &event {
        let _ = dispatch(app, true, |c| Ok(c.probed(*media_id, info.clone())));
    }
    let _ = app.emit("engine-event", event);
}

async fn autosave(app: AppHandle) {
    let state = app.state::<AppState>();
    loop {
        state.autosave.notified().await;
        while tokio::time::timeout(AUTOSAVE_DEBOUNCE, state.autosave.notified()).await.is_ok() {}
        let (show, path) = {
            let ctl = state.ctl();
            (ctl.show.clone(), files::autosave_path(ctl.path.as_deref(), &state.data_dir))
        };
        if let Err(e) = files::save_atomic(&path, &show) {
            log!("autosave: {e}");
        }
    }
}

#[tauri::command]
fn get_show(state: State<AppState>) -> Res<Show> {
    Ok(state.ctl().show.clone())
}

#[tauri::command]
fn get_live(state: State<AppState>) -> Res<Live> {
    Ok(state.ctl().live())
}

#[tauri::command]
fn apply_op(app: AppHandle, op: ShowOp) -> Res<()> {
    dispatch(&app, true, |c| c.apply(op))
}

#[tauri::command]
fn undo(app: AppHandle) -> Res<()> {
    dispatch(&app, true, |c| Ok(c.undo()))
}

#[tauri::command]
fn redo(app: AppHandle) -> Res<()> {
    dispatch(&app, true, |c| Ok(c.redo()))
}

#[tauri::command]
fn load_cue(app: AppHandle, group_id: Uuid, slot: u32) -> Res<()> {
    dispatch(&app, false, |c| c.load_cue(CueRef { group_id, slot }))
}

#[tauri::command]
fn take(app: AppHandle) -> Res<()> {
    dispatch(&app, false, |c| Ok(c.take()))
}

#[tauri::command]
fn load_next(app: AppHandle) -> Res<()> {
    dispatch(&app, false, |c| Ok(c.load_next()))
}

#[tauri::command]
fn transport(app: AppHandle, bus: Bus, layer: Option<Uuid>, action: TransportAction) -> Res<()> {
    dispatch(&app, false, |_| Ok(vec![ToEngine::Transport { bus, layer, action }]))
}

#[tauri::command]
fn list_monitors(app: AppHandle) -> Res<()> {
    dispatch(&app, false, |_| Ok(vec![ToEngine::ListMonitors]))
}

#[tauri::command]
fn identify(app: AppHandle, output_id: Uuid) -> Res<()> {
    dispatch(&app, false, |_| Ok(vec![ToEngine::Identify { output_id }]))
}

#[tauri::command]
fn import_media(app: AppHandle, paths: Vec<PathBuf>) -> Res<()> {
    dispatch(&app, true, |c| c.import_media(&paths))
}

/// Opening an `.autosave` recovers it as an untitled show (save it somewhere to keep it).
#[tauri::command]
fn open_show(app: AppHandle, path: PathBuf) -> Res<()> {
    let show = files::open(&path)?;
    app.state::<Engine>().release_hold(); // an adopted engine may take this show
    let titled = path.extension().is_none_or(|e| e != "autosave");
    dispatch(&app, true, |c| Ok(c.open(show, titled.then_some(path))))
}

/// `path: None` saves to the show's current file.
#[tauri::command]
fn save_show(state: State<AppState>, path: Option<PathBuf>) -> Res<()> {
    let mut ctl = state.ctl();
    let path = path.or_else(|| ctl.path.clone()).ok_or("the show has no file yet: choose where to save it")?;
    files::save_atomic(&path, &ctl.show)?;
    ctl.path = Some(path);
    Ok(())
}

#[tauri::command]
fn engine_info(engine: State<Engine>) -> Res<Option<EngineInfo>> {
    Ok(engine.info())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let (engine, rx) = Engine::new(data_dir.join("engine.json"));
            app.manage(AppState { ctl: Mutex::new(Controller::new()), autosave: Notify::new(), data_dir });
            app.manage(engine);
            tauri::async_runtime::spawn(engine_client::supervise(app.handle().clone(), rx));
            tauri::async_runtime::spawn(autosave(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_show, get_live, apply_op, undo, redo, load_cue, take, load_next, transport, list_monitors,
            identify, import_media, open_show, save_show, engine_info
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Engine>().shutdown();
            }
        });
}
