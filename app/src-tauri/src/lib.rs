//! Tauri wiring: managed state, commands, events, autosave, the engine watchdog.
//!
//! Events to the frontend:
//! - `show-changed`: the full `Show`, after every show mutation
//! - `live-changed`: `Live { pvw, lastTaken }` (cue refs or null), after every command that may move them
//! - `engine-event`: every `FromEngine`, plus `Error`s about engine restarts
//! - `engine-info`: `EngineInfo { port, token }` on connect, `null` on disconnect
//! - `recovery-available`: path of unsaved work from an earlier session (also `get_recovery`)

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
    /// Unsaved work from an earlier session, moved aside (`files::take_recovery`).
    recovery: Mutex<Option<PathBuf>>,
}

/// What a command may have changed.
#[derive(PartialEq)]
enum Change {
    /// Only which cues are on the buses.
    Live,
    /// The show, by an edit: emit it, autosave it.
    Edit,
    /// The show, replaced from disk: emit it; no autosave (it equals the file).
    Load,
}

impl AppState {
    /// A panic elsewhere must not lock the operator out of the show.
    fn ctl(&self) -> MutexGuard<'_, Controller> {
        self.ctl.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Run `f` on the controller and queue its engine messages under the lock (keeps them in
/// order), then tell the UI.
fn dispatch(app: &AppHandle, change: Change, f: impl FnOnce(&mut Controller) -> Res<Vec<ToEngine>>) -> Res<()> {
    let state = app.state::<AppState>();
    let (show, live) = {
        let mut ctl = state.ctl();
        app.state::<Engine>().send(f(&mut ctl)?);
        ((change != Change::Live).then(|| ctl.show.clone()), ctl.live())
    };
    if let Some(show) = show {
        let _ = app.emit("show-changed", show);
    }
    if change == Change::Edit {
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
        let _ = dispatch(app, Change::Edit, |c| Ok(c.probed(*media_id, info.clone())));
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
    dispatch(&app, Change::Edit, |c| c.apply(op))
}

#[tauri::command]
fn undo(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Edit, |c| Ok(c.undo()))
}

#[tauri::command]
fn redo(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Edit, |c| Ok(c.redo()))
}

#[tauri::command]
fn load_cue(app: AppHandle, group_id: Uuid, slot: u32) -> Res<()> {
    dispatch(&app, Change::Live, |c| c.load_cue(CueRef { group_id, slot }))
}

#[tauri::command]
fn take(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Live, |c| Ok(c.take()))
}

#[tauri::command]
fn load_next(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Live, |c| Ok(c.load_next()))
}

#[tauri::command]
fn transport(app: AppHandle, bus: Bus, layer: Option<Uuid>, action: TransportAction) -> Res<()> {
    dispatch(&app, Change::Live, |_| Ok(vec![ToEngine::Transport { bus, layer, action }]))
}

#[tauri::command]
fn list_monitors(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Live, |_| Ok(vec![ToEngine::ListMonitors]))
}

#[tauri::command]
fn identify(app: AppHandle, output_id: Uuid) -> Res<()> {
    dispatch(&app, Change::Live, |_| Ok(vec![ToEngine::Identify { output_id }]))
}

#[tauri::command]
fn import_media(app: AppHandle, paths: Vec<PathBuf>) -> Res<()> {
    dispatch(&app, Change::Edit, |c| c.import_media(&paths))
}

/// Opening an `.autosave` / `.autosave.prev` recovers it as an untitled show (save it
/// somewhere to keep it). Opening a show whose autosave is newer moves that aside first and
/// emits `recovery-available`.
#[tauri::command]
fn open_show(app: AppHandle, path: PathBuf) -> Res<()> {
    let show = files::open(&path)?;
    app.state::<Engine>().release_hold(); // an adopted engine may take this show
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let titled = !(name.ends_with(".autosave") || name.ends_with(".autosave.prev"));
    let state = app.state::<AppState>();
    let recovery = if titled { files::take_recovery(Some(&path), &state.data_dir) } else { None };
    *state.recovery.lock().unwrap_or_else(|e| e.into_inner()) = recovery.clone();
    dispatch(&app, Change::Load, |c| Ok(c.open(show, titled.then_some(path))))?;
    if let Some(prev) = recovery {
        let _ = app.emit("recovery-available", prev);
    }
    Ok(())
}

/// Unsaved work from an earlier session (`*.autosave.prev`), if any: offer `open_show` on it.
#[tauri::command]
fn get_recovery(state: State<AppState>) -> Res<Option<PathBuf>> {
    Ok(state.recovery.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// `path: None` saves to the show's current file.
#[tauri::command]
fn save_show(state: State<AppState>, path: Option<PathBuf>) -> Res<()> {
    let mut ctl = state.ctl();
    let path = path.or_else(|| ctl.path.clone()).ok_or("the show has no file yet: choose where to save it")?;
    files::save_atomic(&path, &ctl.show)?;
    if ctl.path.is_none() {
        // The untitled work is in a file now; don't offer it for recovery next launch.
        let _ = std::fs::remove_file(files::autosave_path(None, &state.data_dir));
    }
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
            let recovery = files::take_recovery(None, &data_dir);
            if let Some(prev) = &recovery {
                log!("autosave: untitled work from an earlier session kept at {}", prev.display());
            }
            let ctl = Mutex::new(Controller::new());
            app.manage(AppState { ctl, autosave: Notify::new(), data_dir, recovery: Mutex::new(recovery) });
            app.manage(engine);
            tauri::async_runtime::spawn(engine_client::supervise(app.handle().clone(), rx));
            tauri::async_runtime::spawn(autosave(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_show, get_live, apply_op, undo, redo, load_cue, take, load_next, transport, list_monitors,
            identify, import_media, open_show, get_recovery, save_show, engine_info
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Engine>().shutdown();
            }
        });
}
