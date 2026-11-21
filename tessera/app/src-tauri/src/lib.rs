//! Tauri wiring: managed state, commands, events, autosave, the engine watchdog.
//!
//! Events to the frontend:
//! - `show-changed`: the full `Show`, after every show mutation
//! - `live-changed`: `Live { pvw, lastTaken, program, preview, transition }` (cue refs or null; the
//!   PGM and PVW scenes; PVW's transition), after every command that may move them
//! - `engine-event`: every `FromEngine`, plus `Error`s about engine restarts
//! - `engine-info`: `EngineInfo { port, token, adopted, held }` on connect and when a show is
//!   loaded (which releases the hold), `null` on disconnect
//! - `engine-stopped`: `true` when the operator stops the engine, `false` when they start it
//! - `recovery-available`: path of unsaved work from an earlier session (also `get_recovery`)
//!
//! Quitting (⌘Q, closing the window) asks first while something is on air (C1).

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
pub mod remote;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use controller::{Controller, CueRef, Live};
use engine_client::{Engine, EngineInfo};
use tauri::menu::{Menu, MenuItem, MenuItemKind};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
use tessera_show::{Bus, FromEngine, Scene, Show, ShowOp, ToEngine, Transition, TransportAction};
use tokio::sync::{Notify, mpsc, watch};
use uuid::Uuid;

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_secs(2);

type Res<T> = Result<T, String>;

pub struct AppState {
    ctl: Mutex<Controller>,
    autosave: Notify,
    data_dir: PathBuf,
    /// Unsaved work from an earlier session, moved aside (`files::take_recovery`).
    recovery: Mutex<Option<PathBuf>>,
    /// How the operator chose to quit; set just before `exit`.
    quit: Mutex<Option<QuitChoice>>,
    /// The quit question is up.
    asking: AtomicBool,
    /// Bumped whenever the state external systems see may have changed (remote control).
    changes: watch::Sender<u64>,
    remote: Mutex<RemoteRuntime>,
}

/// Remote control: the operator's settings and, while on, the server.
struct RemoteRuntime {
    settings: remote::Settings,
    server: Option<remote::Server>,
    error: Option<String>,
    /// Shared with the running server, so "New key" applies at once.
    key: Arc<RwLock<String>>,
    clients: Arc<AtomicUsize>,
}

/// What a command may have changed.
#[derive(PartialEq)]
enum Change {
    /// Only which cues are on the buses.
    Live,
    /// The show, maybe by an edit: emit it, autosave it if it changed.
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
    let (show, live, dirty) = {
        let mut ctl = state.ctl();
        app.state::<Engine>().send(f(&mut ctl)?);
        ((change != Change::Live).then(|| ctl.show.clone()), ctl.live(), ctl.dirty)
    };
    if let Some(show) = show {
        let _ = app.emit("show-changed", show);
    }
    // Only edits restart the debounce: a Take every second mustn't hold the autosave off.
    if change == Change::Edit && dirty {
        state.autosave.notify_one();
    }
    let _ = app.emit("live-changed", live);
    changed(app);
    Ok(())
}

/// The state external systems see may have changed: remote clients get a fresh snapshot.
pub(crate) fn changed(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// The engine's state in one word, as remote clients see it.
fn engine_word(app: &AppHandle) -> &'static str {
    let engine = app.state::<Engine>();
    match engine.info() {
        _ if engine.stopped() => "stopped",
        Some(info) if info.held => "held",
        Some(_) => "connected",
        None => "connecting",
    }
}

/// Remote commands run through `dispatch`, exactly as the UI's do, so the UI follows.
struct AppControl(AppHandle);

impl AppControl {
    /// The cue at 1-based (group, cue), if the show has it.
    fn cue(&self, group: usize, cue: u32) -> Result<CueRef, remote::Refused> {
        let state = self.0.state::<AppState>();
        let ctl = state.ctl();
        let g = ctl.show.groups.get(group - 1).ok_or_else(|| remote::Refused::BadRequest(format!("there is no group {group}")))?;
        let slot = cue - 1;
        if !g.cues.iter().any(|c| c.slot == slot) {
            return Err(remote::Refused::BadRequest(format!("group {group} ({}) has no cue {cue}", g.name)));
        }
        Ok(CueRef { group_id: g.id, slot })
    }
}

impl remote::Control for AppControl {
    fn run(&self, command: remote::Command) -> Result<(), remote::Refused> {
        use remote::{Command as C, Refused};
        let app = &self.0;
        let preview_empty = || app.state::<AppState>().ctl().pvw.scene.is_empty();
        let done = match command {
            C::Go if preview_empty() => return Err(Refused::Conflict("Preview is empty: load a cue first".into())),
            C::Go => dispatch(app, Change::Live, |c| Ok(c.take())),
            C::Load { group, cue } => {
                let r = self.cue(group, cue)?;
                dispatch(app, Change::Live, |c| c.load_cue(r))
            }
            C::Fire { group, cue } => {
                let r = self.cue(group, cue)?;
                dispatch(app, Change::Live, |c| {
                    let mut out = c.load_cue(r)?;
                    out.extend(c.take());
                    Ok(out)
                })
            }
            C::Next => dispatch(app, Change::Live, |c| Ok(c.load_next())),
            C::Previous => dispatch(app, Change::Live, |c| Ok(c.load_prev())),
            C::Transport { .. } if app.state::<Engine>().info().is_none() => {
                return Err(Refused::Unavailable("the engine is not connected".into()));
            }
            C::Transport { bus, action } => dispatch(app, Change::Live, |c| Ok(c.transport(bus, None, action))),
            C::Volume(v) => dispatch(app, Change::Edit, |c| c.apply(ShowOp::SetMasterVolume { volume: v.clamp(0., 1.) })),
            C::VolumeBy(d) => dispatch(app, Change::Edit, |c| {
                let volume = (c.show.master_volume + d).clamp(0., 1.);
                c.apply(ShowOp::SetMasterVolume { volume })
            }),
        };
        done.map_err(Refused::BadRequest)
    }

    fn state(&self) -> remote::Snapshot {
        let word = engine_word(&self.0);
        remote::snapshot(&self.0.state::<AppState>().ctl(), word)
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.0.state::<AppState>().changes.subscribe()
    }
}

/// What the UI shows about remote control.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteStatus {
    enabled: bool,
    running: bool,
    port: u16,
    key: String,
    /// This machine's LAN address, to type into Companion.
    address: Option<String>,
    clients: usize,
    error: Option<String>,
}

fn remote_status_of(r: &RemoteRuntime) -> RemoteStatus {
    RemoteStatus {
        enabled: r.settings.enabled,
        running: r.server.is_some(),
        port: r.server.as_ref().map_or(r.settings.port, |s| s.addr.port()),
        key: r.settings.key.clone(),
        address: remote::lan_address(),
        clients: r.clients.load(Ordering::SeqCst),
        error: r.error.clone(),
    }
}

fn emit_remote(app: &AppHandle) {
    let status = remote_status_of(&app.state::<AppState>().remote.lock().unwrap_or_else(|e| e.into_inner()));
    let _ = app.emit("remote-changed", status);
}

/// Starts or stops the server to match the settings (a key is made on first use), saves them.
fn apply_remote(app: &AppHandle) -> RemoteStatus {
    let state = app.state::<AppState>();
    let mut r = state.remote.lock().unwrap_or_else(|e| e.into_inner());
    r.server = None; // stops a running one
    r.error = None;
    if r.settings.enabled {
        if r.settings.key.is_empty() {
            r.settings.key = engine_client::new_token();
        }
        *r.key.write().unwrap_or_else(|e| e.into_inner()) = r.settings.key.clone();
        let notify = app.clone();
        let router = remote::router(Arc::new(AppControl(app.clone())), r.key.clone(), r.clients.clone(), Arc::new(move || emit_remote(&notify)));
        match remote::start(router, "0.0.0.0", r.settings.port) {
            Ok(server) => r.server = Some(server),
            Err(e) => r.error = Some(e),
        }
    }
    if let Err(e) = r.settings.save(&state.data_dir.join("remote.json")) {
        log!("remote: settings: {e}");
    }
    let status = remote_status_of(&r);
    drop(r);
    let _ = app.emit("remote-changed", status.clone());
    status
}

#[tauri::command]
fn remote_status(state: State<AppState>) -> Res<RemoteStatus> {
    Ok(remote_status_of(&state.remote.lock().unwrap_or_else(|e| e.into_inner())))
}

/// Turn remote control on or off, on `port`.
#[tauri::command]
fn remote_set(app: AppHandle, enabled: bool, port: u16) -> Res<RemoteStatus> {
    {
        let state = app.state::<AppState>();
        let mut r = state.remote.lock().unwrap_or_else(|e| e.into_inner());
        r.settings.enabled = enabled;
        r.settings.port = if port == 0 { remote::DEFAULT_PORT } else { port };
    }
    Ok(apply_remote(&app))
}

/// A new access key: the old one stops working at once.
#[tauri::command]
fn remote_new_key(app: AppHandle) -> Res<RemoteStatus> {
    {
        let state = app.state::<AppState>();
        let mut r = state.remote.lock().unwrap_or_else(|e| e.into_inner());
        r.settings.key = engine_client::new_token();
        *r.key.write().unwrap_or_else(|e| e.into_inner()) = r.settings.key.clone();
        if let Err(e) = r.settings.save(&state.data_dir.join("remote.json")) {
            log!("remote: settings: {e}");
        }
    }
    emit_remote(&app);
    Ok(remote_status_of(&app.state::<AppState>().remote.lock().unwrap_or_else(|e| e.into_inner())))
}

/// Called by the engine client on (re)connect: the full state, plus queued requests the
/// resync doesn't cover (queued state messages are older than it). `safe`: the engine keeps dying
/// right after its resync (I5), so PGM goes without its media and PVW empties; the operator is told
/// what was left out.
pub(crate) fn resync(app: &AppHandle, rx: &mut mpsc::UnboundedReceiver<ToEngine>, safe: bool) -> Vec<ToEngine> {
    let state = app.state::<AppState>();
    let mut ctl = state.ctl();
    if safe {
        let dropped = ctl.drop_media();
        let what = if dropped.is_empty() { "no media was on the buses".into() } else { format!("left out: {}", dropped.join(", ")) };
        let message = format!("engine crashed twice right after restarting: restarted without media ({what}). Take the cues again to retry them.");
        log!("{message}");
        let _ = app.emit("engine-event", FromEngine::Error { message });
        let _ = app.emit("live-changed", ctl.live());
    }
    let superseded = |m: &ToEngine| {
        use ToEngine::*;
        matches!(m, SetConfig { .. } | SetPreview { .. } | SetProgram { .. } | SetMasterVolume { .. } | Take { .. })
    };
    let queued: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|m| !superseded(m)).collect();
    ctl.resync_messages().into_iter().chain(queued).collect()
}

/// Every engine event goes to the UI; probe results also update the show, positions the
/// controller's PGM state (I4).
pub(crate) fn engine_event(app: &AppHandle, event: FromEngine) {
    match &event {
        FromEngine::Probed { media_id, info: Some(info), .. } => {
            let _ = dispatch(app, Change::Edit, |c| Ok(c.probed(*media_id, info.clone())));
        }
        FromEngine::Positions { positions } => app.state::<AppState>().ctl().positions(positions),
        _ => {}
    }
    let _ = app.emit("engine-event", event);
}

/// Writes the show if it changed since the last write: a show with a file to that file itself (so
/// the file always has the latest changes), an untitled one to the data dir's autosave. If the
/// show's file can't be written, the work goes to `<show>.autosave` (offered back when the show is
/// opened), else to the data dir's autosave (offered at startup), and the error says where.
/// Called with the controller locked, so a save can't race it.
// ponytail: the write (with fsync) holds the controller lock for a few ms; move it off the lock
// with a generation counter if that ever delays a GO.
fn write_autosave(ctl: &mut Controller, data_dir: &Path) -> Res<bool> {
    if !ctl.dirty {
        return Ok(false);
    }
    let autosave = files::autosave_path(ctl.path.as_deref(), data_dir);
    match ctl.path.clone() {
        Some(path) => match files::save_atomic(&path, &ctl.show) {
            // An older fallback would come back next launch as "unsaved work" (M1).
            Ok(()) => _ = std::fs::remove_file(&autosave),
            Err(e) => {
                let untitled = files::autosave_path(None, data_dir);
                let kept = [autosave, untitled].into_iter().find(|p| files::save_atomic(p, &ctl.show).is_ok());
                let kept = kept.ok_or_else(|| format!("{e}; and no autosave could be written either"))?;
                ctl.dirty = false;
                return Err(format!("{e}; the changes are kept in {}", kept.display()));
            }
        },
        None => files::save_atomic(&autosave, &ctl.show)?,
    }
    ctl.dirty = false;
    Ok(true)
}

async fn autosave(app: AppHandle) {
    let state = app.state::<AppState>();
    loop {
        state.autosave.notified().await;
        while tokio::time::timeout(AUTOSAVE_DEBOUNCE, state.autosave.notified()).await.is_ok() {}
        if let Err(e) = write_autosave(&mut state.ctl(), &state.data_dir) {
            log!("autosave: {e}");
            let _ = app.emit("engine-event", FromEngine::Error { message: format!("autosave: {e}") });
        }
    }
}

/// Before another show replaces this one: its last edits (within the 2 s debounce) are written.
fn flush_autosave(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Err(e) = write_autosave(&mut state.ctl(), &state.data_dir) {
        log!("autosave before switching shows: {e}");
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

/// An edit of PVW (no cue changes).
#[tauri::command]
fn set_preview(app: AppHandle, scene: Scene, transition: Transition) -> Res<()> {
    dispatch(&app, Change::Live, |c| Ok(c.set_preview(scene, transition)))
}

/// PVW into the cue at the slot (made if empty).
#[tauri::command]
fn store_preview(app: AppHandle, group_id: Uuid, slot: u32) -> Res<()> {
    dispatch(&app, Change::Edit, |c| c.store_preview(CueRef { group_id, slot }))
}

/// A live edit of PGM.
#[tauri::command]
fn set_program(app: AppHandle, scene: Scene) -> Res<()> {
    dispatch(&app, Change::Live, |c| Ok(c.set_program(scene)))
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
    dispatch(&app, Change::Live, |c| Ok(c.transport(bus, layer, action)))
}

#[tauri::command]
fn list_monitors(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Live, |_| Ok(vec![ToEngine::ListMonitors]))
}

/// Operator: stop the engine (outputs black) until `engine_start`.
#[tauri::command]
fn engine_stop(app: AppHandle) -> Res<()> {
    app.state::<Engine>().stop(&app);
    Ok(())
}

#[tauri::command]
fn engine_start(app: AppHandle) -> Res<()> {
    app.state::<Engine>().start(&app);
    Ok(())
}

/// Operator: replace the engine; the new one gets everything back on connect.
#[tauri::command]
fn engine_restart(engine: State<Engine>) -> Res<()> {
    engine.restart();
    Ok(())
}

#[tauri::command]
fn engine_stopped(engine: State<Engine>) -> Res<bool> {
    Ok(engine.stopped())
}

/// The reply comes as an `engine-event` `Inputs`.
#[tauri::command]
fn list_inputs(app: AppHandle) -> Res<()> {
    dispatch(&app, Change::Live, |_| Ok(vec![ToEngine::ListInputs]))
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
    flush_autosave(&app);
    release_hold(&app);
    let titled = !files::is_autosave(&path);
    let state = app.state::<AppState>();
    let recovery = if titled { files::take_recovery(Some(&path), &state.data_dir) } else { None };
    *state.recovery.lock().unwrap_or_else(|e| e.into_inner()) = recovery.clone();
    dispatch(&app, Change::Load, |c| Ok(c.open(show, titled.then_some(path))))?;
    if let Some(prev) = recovery {
        let _ = app.emit("recovery-available", prev);
    }
    Ok(())
}

/// A fresh untitled show. Like `open_show`: history and PVW clear, PGM keeps running.
#[tauri::command]
fn new_show(app: AppHandle) -> Res<()> {
    flush_autosave(&app);
    release_hold(&app);
    dispatch(&app, Change::Load, |c| Ok(c.open(Show::new(), None)))
}

/// The operator loaded a show: an adopted engine may take it now (R35). Tell the UI.
fn release_hold(app: &AppHandle) {
    let engine = app.state::<Engine>();
    engine.release_hold();
    let _ = app.emit("engine-info", engine.info());
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
    // The work is in a file now: its autosave would only come back as "unsaved work" next launch (M1).
    let _ = std::fs::remove_file(files::autosave_path(ctl.path.as_deref(), &state.data_dir));
    ctl.path = Some(path);
    ctl.dirty = false;
    Ok(())
}

#[tauri::command]
fn engine_info(engine: State<Engine>) -> Res<Option<EngineInfo>> {
    Ok(engine.info())
}

/// How the app quits (C1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuitChoice {
    /// Exit without stopping the engine; the next controller adopts it (R35).
    KeepOutputs,
    /// Stop the engine (outputs go black) and exit.
    StopOutputs,
}

const KEEP: &str = "Keep outputs running and quit";
const STOP: &str = "Stop outputs and quit";
const QUIT_MENU: &str = "tessera-quit";

/// A quit request with nothing on air stops the engine and quits; with something on air the
/// operator chooses (None).
pub fn quit_choice(on_air: bool) -> Option<QuitChoice> {
    (!on_air).then_some(QuitChoice::StopOutputs)
}

/// The quit question's answer; None: Cancel (also Escape or a closed dialog).
pub fn quit_answer(answer: &MessageDialogResult) -> Option<QuitChoice> {
    match answer {
        MessageDialogResult::Yes => Some(QuitChoice::KeepOutputs),
        MessageDialogResult::No => Some(QuitChoice::StopOutputs),
        MessageDialogResult::Custom(label) if label == KEEP => Some(QuitChoice::KeepOutputs),
        MessageDialogResult::Custom(label) if label == STOP => Some(QuitChoice::StopOutputs),
        _ => None,
    }
}

/// What exiting does to the engine: the operator's choice, else (a quit that never asked: the
/// Dock's Quit, logout) never black out air unasked.
pub fn exit_choice(chosen: Option<QuitChoice>, on_air: bool) -> QuitChoice {
    chosen.unwrap_or(if on_air { QuitChoice::KeepOutputs } else { QuitChoice::StopOutputs })
}

/// An adopted engine's PGM is unknown here, so it counts as on air.
fn on_air(app: &AppHandle) -> bool {
    app.state::<Engine>().adopted() || app.state::<AppState>().ctl().on_air()
}

/// ⌘Q, the Quit menu item or closing the window: quit at once if nothing is on air, else ask.
fn request_quit(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.asking.swap(true, Ordering::SeqCst) {
        return; // already asking
    }
    if let Some(choice) = quit_choice(on_air(app)) {
        return quit(app, choice);
    }
    let handle = app.clone();
    app.dialog()
        .message("Outputs are on air.\n\nKeep them running: the screens stay as they are, and Tessera takes the engine over when it opens again.\nStop them: every output goes black.")
        .title("Quit Tessera")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(KEEP.into(), STOP.into(), "Cancel".into()))
        .show_with_result(move |answer| match quit_answer(&answer) {
            Some(choice) => quit(&handle, choice),
            None => handle.state::<AppState>().asking.store(false, Ordering::SeqCst),
        });
}

fn quit(app: &AppHandle, choice: QuitChoice) {
    log!("quit: {choice:?}");
    *app.state::<AppState>().quit.lock().unwrap_or_else(|e| e.into_inner()) = Some(choice);
    app.exit(0);
}

/// The event loop is ending: save unsaved edits, then stop or leave the engine.
fn exiting(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Err(e) = write_autosave(&mut state.ctl(), &state.data_dir) {
        log!("autosave on quit: {e}");
    }
    let chosen = *state.quit.lock().unwrap_or_else(|e| e.into_inner());
    let engine = app.state::<Engine>();
    match exit_choice(chosen, on_air(app)) {
        QuitChoice::KeepOutputs => engine.detach(),
        QuitChoice::StopOutputs => engine.shutdown(),
    }
}

/// Tauri's default menu, with a Quit that asks (C1): the stock item terminates the app at once.
fn menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let is_quit = |item: &MenuItemKind<tauri::Wry>| {
        matches!(item, MenuItemKind::Predefined(p) if p.text().is_ok_and(|t| t.replace('&', "").starts_with("Quit")))
    };
    for item in menu.items()? {
        let MenuItemKind::Submenu(sub) = item else { continue };
        if let Some(i) = sub.items()?.iter().position(is_quit) {
            sub.remove_at(i)?;
            let text = format!("Quit {}", app.package_info().name);
            sub.insert(&MenuItem::with_id(app, QUIT_MENU, text, true, Some("CmdOrCtrl+Q"))?, i)?;
            return Ok(menu);
        }
    }
    log!("menu: no Quit item found; quitting from the menu won't ask");
    Ok(menu)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // First: a second launch (e.g. `tauri dev` on the show Mac) only focuses this window, and
        // never adopts (and on quit kills) this instance's engine (I7).
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .menu(menu)
        .on_menu_event(|app, event| {
            if event.id() == QUIT_MENU {
                request_quit(app);
            }
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let (engine, rx) = Engine::new(data_dir.join("engine.json"));
            let recovery = files::take_recovery(None, &data_dir);
            if let Some(prev) = &recovery {
                log!("autosave: untitled work from an earlier session kept at {}", prev.display());
            }
            let ctl = Mutex::new(Controller::new());
            let recovery = Mutex::new(recovery);
            let settings = remote::Settings::load(&data_dir.join("remote.json"));
            let remote = Mutex::new(RemoteRuntime {
                key: Arc::new(RwLock::new(settings.key.clone())),
                settings,
                server: None,
                error: None,
                clients: Arc::new(AtomicUsize::new(0)),
            });
            let changes = watch::Sender::new(0);
            app.manage(AppState { ctl, autosave: Notify::new(), data_dir, recovery, quit: Mutex::new(None), asking: AtomicBool::new(false), changes, remote });
            app.manage(engine);
            let enabled = app.state::<AppState>().remote.lock().unwrap_or_else(|e| e.into_inner()).settings.enabled;
            if enabled {
                apply_remote(app.handle());
            }
            tauri::async_runtime::spawn(engine_client::supervise(app.handle().clone(), rx));
            tauri::async_runtime::spawn(autosave(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_show, get_live, apply_op, undo, redo, load_cue, set_preview, store_preview, set_program, take, load_next, transport, list_monitors, list_inputs,
            identify, import_media, new_show, open_show, get_recovery, save_show, engine_info, engine_stop, engine_start, engine_restart, engine_stopped, remote_status, remote_set, remote_new_key
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            RunEvent::WindowEvent { event: WindowEvent::CloseRequested { api, .. }, .. } => {
                api.prevent_close();
                request_quit(app);
            }
            RunEvent::ExitRequested { api, .. } => {
                if app.state::<AppState>().quit.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                    api.prevent_exit();
                    request_quit(app);
                }
            }
            RunEvent::Exit => exiting(app),
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A show with a file autosaves into that file; an untitled one into the data dir; a file that
    /// can't be written keeps the work in its `.autosave` and says so.
    #[test]
    fn autosave_writes_the_show_file() {
        let dir = std::env::temp_dir().join(format!("tessera-autosave-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ctl = Controller::new();
        ctl.apply(tessera_show::ShowOp::SetMasterVolume { volume: 0.5 }).unwrap();
        assert!(write_autosave(&mut ctl, &dir).unwrap(), "untitled");
        assert!(dir.join("untitled.tshow.autosave").exists());
        let show = dir.join("show.tshow");
        ctl.path = Some(show.clone());
        std::fs::write(dir.join("show.tshow.autosave"), b"older fallback").unwrap();
        ctl.apply(tessera_show::ShowOp::SetMasterVolume { volume: 0.25 }).unwrap();
        assert!(write_autosave(&mut ctl, &dir).unwrap());
        assert_eq!(files::open(&show).unwrap().master_volume, 0.25, "the show file has the latest change");
        assert!(!dir.join("show.tshow.autosave").exists(), "no stale fallback left");
        assert!(!write_autosave(&mut ctl, &dir).unwrap(), "nothing new: no write");
        // The file can't be written (its folder is gone, so its .autosave can't be either): the
        // work goes to the data dir, which startup offers back.
        let gone = dir.join("missing").join("show.tshow");
        ctl.path = Some(gone.clone());
        ctl.apply(tessera_show::ShowOp::SetMasterVolume { volume: 0.75 }).unwrap();
        let err = write_autosave(&mut ctl, &dir).unwrap_err();
        assert!(err.contains("untitled.tshow.autosave"), "{err}");
        assert_eq!(files::open(&dir.join("untitled.tshow.autosave")).unwrap().master_volume, 0.75);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quit_asks_only_when_on_air() {
        assert_eq!(quit_choice(false), Some(QuitChoice::StopOutputs), "nothing on air: stop and quit");
        assert_eq!(quit_choice(true), None, "on air: ask");
        let label = |s: &str| MessageDialogResult::Custom(s.into());
        assert_eq!(quit_answer(&label(KEEP)), Some(QuitChoice::KeepOutputs));
        assert_eq!(quit_answer(&label(STOP)), Some(QuitChoice::StopOutputs));
        assert_eq!(quit_answer(&label("Cancel")), None);
        assert_eq!(quit_answer(&MessageDialogResult::Cancel), None);
        assert_eq!((quit_answer(&MessageDialogResult::Yes), quit_answer(&MessageDialogResult::No)),
            (Some(QuitChoice::KeepOutputs), Some(QuitChoice::StopOutputs)), "default labels map the same way");
        // A quit that never asked (Dock, logout) leaves air alone.
        assert_eq!(exit_choice(None, true), QuitChoice::KeepOutputs);
        assert_eq!(exit_choice(None, false), QuitChoice::StopOutputs);
        assert_eq!(exit_choice(Some(QuitChoice::StopOutputs), true), QuitChoice::StopOutputs);
    }

    /// C1/M1: the autosave writes only unsaved changes, so quitting flushes the last edits and a
    /// quiet show isn't rewritten.
    #[test]
    fn autosave_writes_only_changes() {
        let dir = std::env::temp_dir().join(format!("tessera-autosave-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let auto = files::autosave_path(None, &dir);
        let mut c = Controller::new();
        assert_eq!(write_autosave(&mut c, &dir), Ok(false), "nothing to save");
        assert!(!auto.exists());
        c.apply(ShowOp::SetMasterVolume { volume: 0.4 }).unwrap();
        assert_eq!(write_autosave(&mut c, &dir), Ok(true), "an edit younger than the debounce");
        assert_eq!(files::open(&auto).unwrap().master_volume, 0.4);
        std::fs::remove_file(&auto).unwrap();
        assert_eq!(write_autosave(&mut c, &dir), Ok(false), "already written");
        assert!(!auto.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
