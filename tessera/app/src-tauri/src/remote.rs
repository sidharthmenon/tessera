//! Remote control for external systems (Bitfocus Companion, scripts): HTTP commands and a
//! WebSocket of live state, on the LAN, every request gated by the operator's access key. Off
//! unless the operator turns it on (status bar → Remote). The protocol is documented in
//! `docs/external-control.md`.
//!
//! The router only knows a `Control` (run a command, read the state, wait for changes), so it is
//! tested without Tauri; the app's `Control` lives in `lib.rs`.

use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tessera_show::{Bus, TransportAction};
use tokio::sync::{oneshot, watch};

use crate::controller::Controller;

pub const DEFAULT_PORT: u16 = 7410;

/// The operator's remote-control settings (`remote.json` in the app data dir, not the show).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub port: u16,
    /// Every request must carry it. Made when remote control is first turned on.
    pub key: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { enabled: false, port: DEFAULT_PORT, key: String::new() }
    }
}

impl Settings {
    /// Missing or unreadable: the defaults (off).
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Owner-only: the file holds the key.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::files::write_atomic(path, &json, true)
    }
}

/// What an external system can ask for. Group and cue numbers are 1-based, as on screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Go,
    Load { group: usize, cue: u32 },
    /// Load + GO.
    Fire { group: usize, cue: u32 },
    Next,
    Previous,
    /// The whole bus, as the Transport bar with nothing selected.
    Transport { bus: Bus, action: TransportAction },
    /// 0–1.
    Volume(f32),
    /// Added to the volume (negative: down).
    VolumeBy(f32),
}

/// Why a command was refused: becomes the HTTP status and `{ "error": … }`.
#[derive(Debug, PartialEq)]
pub enum Refused {
    BadRequest(String),
    Conflict(String),
    Unavailable(String),
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Refused::BadRequest(e) => (StatusCode::BAD_REQUEST, e),
            Refused::Conflict(e) => (StatusCode::CONFLICT, e),
            Refused::Unavailable(e) => (StatusCode::SERVICE_UNAVAILABLE, e),
        };
        (status, Json(serde_json::json!({ "error": error }))).into_response()
    }
}

/// The live state external systems see (also each WebSocket message's `state`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// The show file's name, or "Untitled show".
    pub show: String,
    /// `connected`, `held`, `connecting` or `stopped`.
    pub engine: String,
    pub groups: Vec<GroupInfo>,
    pub preview: BusInfo,
    pub program: BusInfo,
    /// Master volume, 0–100.
    pub volume: u32,
    /// The Program clip that ends last, while one plays.
    pub time: Option<ClipTime>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfo {
    pub number: usize,
    pub name: String,
    pub cues: Vec<CueInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CueInfo {
    pub number: u32,
    pub name: String,
    pub hotkey: Option<String>,
}

/// A bus: the cue it came from (if any), whether it shows anything, and (Preview) whether it was
/// edited since that cue was loaded.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusInfo {
    pub group: Option<usize>,
    pub cue: Option<u32>,
    pub name: Option<String>,
    pub empty: bool,
    pub edited: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTime {
    pub elapsed_ms: u64,
    pub remaining_ms: u64,
    pub clip: String,
}

/// The state from the controller and the engine's status word.
pub fn snapshot(ctl: &Controller, engine: &str) -> Snapshot {
    let show = &ctl.show;
    let locate = |r: Option<crate::controller::CueRef>| {
        let r = r?;
        let (gi, g) = show.groups.iter().enumerate().find(|(_, g)| g.id == r.group_id)?;
        let cue = g.cues.iter().find(|c| c.slot == r.slot)?;
        Some((gi + 1, cue))
    };
    let bus = |r, empty: bool, edited: &dyn Fn(&tessera_show::Cue) -> bool| match locate(r) {
        Some((group, cue)) => BusInfo { group: Some(group), cue: Some(cue.slot + 1), name: Some(cue.name.clone()), empty, edited: edited(cue) },
        None => BusInfo { empty, ..BusInfo::default() },
    };
    let pvw_edited = |c: &tessera_show::Cue| c.layers_by_screen != ctl.pvw.scene || c.transition != ctl.pvw.transition;
    let nothing = |scene: &tessera_show::Scene| scene.values().all(|layers| layers.is_empty());
    Snapshot {
        show: ctl.path.as_ref().and_then(|p| p.file_name()).map_or("Untitled show".into(), |n| n.to_string_lossy().into()),
        engine: engine.into(),
        groups: show
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let mut cues: Vec<_> = g.cues.iter().map(|c| CueInfo { number: c.slot + 1, name: c.name.clone(), hotkey: c.hotkey.clone() }).collect();
                cues.sort_by_key(|c| c.number);
                GroupInfo { number: i + 1, name: g.name.clone(), cues }
            })
            .collect(),
        preview: bus(ctl.pvw.cue, nothing(&ctl.pvw.scene), &pvw_edited),
        program: bus(ctl.last_taken, nothing(&ctl.pgm), &|_| false),
        volume: (show.master_volume.clamp(0., 1.) * 100.).round() as u32,
        time: ctl.program_time().map(|(elapsed_ms, remaining_ms, clip)| ClipTime { elapsed_ms, remaining_ms, clip }),
    }
}

/// What the router drives: the app (lib.rs), or a fake in tests.
pub trait Control: Send + Sync + 'static {
    fn run(&self, command: Command) -> Result<(), Refused>;
    fn state(&self) -> Snapshot;
    /// Bumped whenever the state may have changed.
    fn changes(&self) -> watch::Receiver<u64>;
}

#[derive(Clone)]
struct Shared {
    control: Arc<dyn Control>,
    key: Arc<RwLock<String>>,
    clients: Arc<AtomicUsize>,
    /// Called when a WebSocket client comes or goes (the UI shows the count).
    on_clients: Arc<dyn Fn() + Send + Sync>,
}

/// The HTTP API. `key` can change while serving ("New key").
pub fn router(control: Arc<dyn Control>, key: Arc<RwLock<String>>, clients: Arc<AtomicUsize>, on_clients: Arc<dyn Fn() + Send + Sync>) -> Router {
    let shared = Shared { control, key, clients, on_clients };
    Router::new()
        .route("/api/v1/state", get(|State(s): State<Shared>| async move { Json(s.control.state()) }))
        .route("/api/v1/go", post(|State(s): State<Shared>| async move { run(&s, Ok(Command::Go)) }))
        .route("/api/v1/next", post(|State(s): State<Shared>| async move { run(&s, Ok(Command::Next)) }))
        .route("/api/v1/previous", post(|State(s): State<Shared>| async move { run(&s, Ok(Command::Previous)) }))
        .route(
            "/api/v1/cue/{group}/{cue}/load",
            post(|State(s): State<Shared>, UrlPath((g, c)): UrlPath<(String, String)>| async move {
                run(&s, cue(&g, &c).map(|(group, cue)| Command::Load { group, cue }))
            }),
        )
        .route(
            "/api/v1/cue/{group}/{cue}/fire",
            post(|State(s): State<Shared>, UrlPath((g, c)): UrlPath<(String, String)>| async move {
                run(&s, cue(&g, &c).map(|(group, cue)| Command::Fire { group, cue }))
            }),
        )
        .route(
            "/api/v1/transport/{bus}/{action}",
            post(|State(s): State<Shared>, UrlPath((b, a)): UrlPath<(String, String)>| async move { run(&s, transport(&b, &a)) }),
        )
        .route(
            "/api/v1/volume/{value}",
            post(|State(s): State<Shared>, UrlPath(v): UrlPath<String>| async move { run(&s, percent(&v, 0.).map(|v| Command::Volume(v / 100.))) }),
        )
        .route(
            "/api/v1/volume/up/{step}",
            post(|State(s): State<Shared>, UrlPath(v): UrlPath<String>| async move { run(&s, percent(&v, 0.1).map(|v| Command::VolumeBy(v / 100.))) }),
        )
        .route(
            "/api/v1/volume/down/{step}",
            post(|State(s): State<Shared>, UrlPath(v): UrlPath<String>| async move { run(&s, percent(&v, 0.1).map(|v| Command::VolumeBy(-v / 100.))) }),
        )
        .route("/api/v1/events", get(|State(s): State<Shared>, ws: WebSocketUpgrade| async move { ws.on_upgrade(move |socket| client(s, socket)) }))
        .layer(middleware::from_fn_with_state(shared.clone(), auth))
        .with_state(shared)
}

/// Runs a parsed command; answers with the new state.
fn run(s: &Shared, command: Result<Command, Refused>) -> Response {
    match command.and_then(|c| s.control.run(c)) {
        Ok(()) => Json(s.control.state()).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// A 1-based group and cue number.
fn cue(group: &str, cue: &str) -> Result<(usize, u32), Refused> {
    let number = |what: &str, v: &str| v.parse::<u32>().ok().filter(|n| *n >= 1).ok_or_else(|| Refused::BadRequest(format!("{what} must be a number from 1: {v:?}")));
    Ok((number("group", group)? as usize, number("cue", cue)?))
}

fn transport(bus: &str, action: &str) -> Result<Command, Refused> {
    let bus = match bus {
        "program" | "pgm" => Bus::Pgm,
        "preview" | "pvw" => Bus::Pvw,
        _ => return Err(Refused::BadRequest(format!("bus must be program or preview: {bus:?}"))),
    };
    let action = match action {
        "play" => TransportAction::Play,
        "pause" => TransportAction::Pause,
        "stop" => TransportAction::Stop,
        "restart" => TransportAction::Restart,
        _ => return Err(Refused::BadRequest(format!("action must be play, pause, stop or restart: {action:?}"))),
    };
    Ok(Command::Transport { bus, action })
}

/// A percentage from `min` to 100.
fn percent(v: &str, min: f32) -> Result<f32, Refused> {
    v.parse::<f32>().ok().filter(|p| p.is_finite() && *p >= min && *p <= 100.).ok_or_else(|| Refused::BadRequest(format!("expected a number from {min} to 100: {v:?}")))
}

/// 401 unless the request carries the key: header `X-Tessera-Key` or query `key`.
async fn auth(State(s): State<Shared>, request: Request, next: Next) -> Response {
    let header = request.headers().get("x-tessera-key").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let query = Query::<std::collections::HashMap<String, String>>::try_from_uri(request.uri()).ok().and_then(|q| q.0.get("key").cloned());
    let key = s.key.read().unwrap_or_else(|e| e.into_inner()).clone();
    if !key.is_empty() && header.or(query).is_some_and(|k| same(k.as_bytes(), key.as_bytes())) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "missing or wrong access key" }))).into_response()
    }
}

/// Equal, in a time that depends only on the lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && std::hint::black_box(a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y))) == 0
}

#[derive(Serialize)]
struct Event<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    state: &'a Snapshot,
}

/// One WebSocket client: the state on connect, then whenever it changes (checked on every change
/// and once a second, for the clip time). Messages from the client are ignored.
async fn client(s: Shared, mut socket: WebSocket) {
    s.clients.fetch_add(1, Ordering::SeqCst);
    (s.on_clients)();
    let mut changes = s.control.changes();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last = String::new();
    loop {
        let state = s.control.state();
        let json = serde_json::to_string(&Event { kind: "state", state: &state }).unwrap_or_default();
        if json != last {
            if socket.send(Message::Text(json.clone().into())).await.is_err() {
                break;
            }
            last = json;
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { break },
            _ = tick.tick() => {}
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    s.clients.fetch_sub(1, Ordering::SeqCst);
    (s.on_clients)();
}

/// A running server; dropping it stops it.
pub struct Server {
    stop: Option<oneshot::Sender<()>>,
    pub addr: SocketAddr,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Serves `router` on `host` (the app: every interface, `0.0.0.0`) at `port` (0: any free one).
/// Fails at once if the port is taken.
pub fn start(router: Router, host: &str, port: u16) -> Result<Server, String> {
    let listener = std::net::TcpListener::bind((host, port)).map_err(|e| format!("port {port}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let (stop, stopped) = oneshot::channel::<()>();
    tauri::async_runtime::spawn(async move {
        let served = async {
            let listener = tokio::net::TcpListener::from_std(listener)?;
            axum::serve(listener, router).with_graceful_shutdown(async { _ = stopped.await }).await
        };
        if let Err(e) = served.await {
            log!("remote: server stopped: {e}");
        }
    });
    log!("remote: listening on {addr}");
    Ok(Server { stop: Some(stop), addr })
}

/// This machine's address on its main network (what to type into Companion), if it has one. No
/// packet is sent: connecting a UDP socket only picks the route.
pub fn lan_address() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use axum::body::Body;
    use futures_util::StreamExt;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const KEY: &str = "secret-key";

    /// Records commands; refuses GO while `preview_empty`.
    struct Fake {
        commands: Mutex<Vec<Command>>,
        changes: watch::Sender<u64>,
        state: Mutex<Snapshot>,
    }

    impl Control for Fake {
        fn run(&self, command: Command) -> Result<(), Refused> {
            if command == Command::Go && self.state.lock().unwrap().preview.empty {
                return Err(Refused::Conflict("Preview is empty".into()));
            }
            self.commands.lock().unwrap().push(command);
            Ok(())
        }
        fn state(&self) -> Snapshot {
            self.state.lock().unwrap().clone()
        }
        fn changes(&self) -> watch::Receiver<u64> {
            self.changes.subscribe()
        }
    }

    fn fake() -> Arc<Fake> {
        let state = Snapshot { show: "show.tshow".into(), engine: "connected".into(), volume: 80, ..Snapshot::default() };
        Arc::new(Fake { commands: Mutex::new(vec![]), changes: watch::Sender::new(0), state: Mutex::new(state) })
    }

    fn app(f: &Arc<Fake>) -> Router {
        router(f.clone(), Arc::new(RwLock::new(KEY.into())), Arc::new(AtomicUsize::new(0)), Arc::new(|| {}))
    }

    async fn call(r: &Router, method: &str, uri: &str, key: Option<&str>) -> (StatusCode, serde_json::Value) {
        let mut req = axum::http::Request::builder().method(method).uri(uri);
        if let Some(k) = key {
            req = req.header("X-Tessera-Key", k);
        }
        let res = r.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn the_key_is_required() {
        let f = fake();
        let r = app(&f);
        assert_eq!(call(&r, "GET", "/api/v1/state", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&r, "GET", "/api/v1/state", Some("wrong")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&r, "POST", "/api/v1/go?key=nope", None).await.0, StatusCode::UNAUTHORIZED);
        let (status, state) = call(&r, "GET", "/api/v1/state", Some(KEY)).await;
        assert_eq!((status, state["show"].as_str(), state["volume"].as_u64()), (StatusCode::OK, Some("show.tshow"), Some(80)));
        assert_eq!(call(&r, "GET", &format!("/api/v1/state?key={KEY}"), None).await.0, StatusCode::OK, "key in the query");
        assert!(f.commands.lock().unwrap().is_empty(), "nothing ran without the key");
        // An empty key (never set) lets nobody in.
        let open = router(f.clone(), Arc::new(RwLock::new(String::new())), Arc::new(AtomicUsize::new(0)), Arc::new(|| {}));
        assert_eq!(call(&open, "GET", "/api/v1/state", Some("")).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn endpoints_run_their_commands() {
        let f = fake();
        let r = app(&f);
        for uri in [
            "/api/v1/go",
            "/api/v1/cue/1/3/load",
            "/api/v1/cue/2/10/fire",
            "/api/v1/next",
            "/api/v1/previous",
            "/api/v1/transport/program/pause",
            "/api/v1/transport/pvw/restart",
            "/api/v1/volume/55",
            "/api/v1/volume/up/5",
            "/api/v1/volume/down/2.5",
        ] {
            let (status, body) = call(&r, "POST", uri, Some(KEY)).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            assert_eq!(body["show"], "show.tshow", "{uri} answers with the state");
        }
        assert_eq!(*f.commands.lock().unwrap(), [
            Command::Go,
            Command::Load { group: 1, cue: 3 },
            Command::Fire { group: 2, cue: 10 },
            Command::Next,
            Command::Previous,
            Command::Transport { bus: Bus::Pgm, action: TransportAction::Pause },
            Command::Transport { bus: Bus::Pvw, action: TransportAction::Restart },
            Command::Volume(0.55),
            Command::VolumeBy(0.05),
            Command::VolumeBy(-0.025),
        ]);
    }

    #[tokio::test]
    async fn bad_requests_say_why() {
        let f = fake();
        let r = app(&f);
        for uri in ["/api/v1/cue/0/1/load", "/api/v1/cue/1/x/fire", "/api/v1/transport/air/play", "/api/v1/transport/program/rewind", "/api/v1/volume/101", "/api/v1/volume/up/0"] {
            let (status, body) = call(&r, "POST", uri, Some(KEY)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()), "{uri}: {body}");
        }
        f.state.lock().unwrap().preview.empty = true;
        let (status, body) = call(&r, "POST", "/api/v1/go", Some(KEY)).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("Preview is empty")));
        assert!(f.commands.lock().unwrap().is_empty());
    }

    /// The WebSocket sends the state at once, and again after a change.
    #[tokio::test]
    async fn events_stream_the_state() {
        let f = fake();
        let clients = Arc::new(AtomicUsize::new(0));
        let r = router(f.clone(), Arc::new(RwLock::new(KEY.into())), clients.clone(), Arc::new(|| {}));
        let server = start(r, "127.0.0.1", 0).unwrap();
        let url = format!("ws://127.0.0.1:{}/api/v1/events?key={KEY}", server.addr.port());
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
        async fn next(ws: &mut Ws) -> serde_json::Value {
            let msg = tokio::time::timeout(Duration::from_secs(3), ws.next()).await.expect("a message").unwrap().unwrap();
            serde_json::from_str(msg.to_text().unwrap()).unwrap()
        }
        let first = next(&mut ws).await;
        assert_eq!((first["type"].as_str(), first["state"]["volume"].as_u64()), (Some("state"), Some(80)));
        assert_eq!(clients.load(Ordering::SeqCst), 1);
        f.state.lock().unwrap().volume = 40;
        f.changes.send_modify(|n| *n += 1);
        assert_eq!(next(&mut ws).await["state"]["volume"].as_u64(), Some(40));
        // Without the key: refused before the upgrade.
        let bare = format!("ws://127.0.0.1:{}/api/v1/events", server.addr.port());
        assert!(tokio_tungstenite::connect_async(bare).await.is_err());
    }

    #[test]
    fn settings_round_trip_and_default_off() {
        let dir = std::env::temp_dir().join(format!("tessera-remote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("remote.json");
        assert_eq!(Settings::load(&path), Settings { enabled: false, port: DEFAULT_PORT, key: String::new() });
        let s = Settings { enabled: true, port: 9000, key: "k".into() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The snapshot from a real controller: groups and cues 1-based, PVW/PGM cues, edited flag.
    #[test]
    fn snapshot_from_the_controller() {
        use tessera_show::{Layer, Pattern, Rect, Scene, ShowOp, Source};
        let mut c = Controller::new();
        let s = uuid::Uuid::new_v4();
        let group_id = c.show.groups[0].id;
        let mut cue = |slot: u32, name: &str| {
            let layers = Scene::from([(s, vec![Layer::new(Source::Pattern { pattern: Pattern::Bars }, Rect { x: 0., y: 0., w: 1., h: 1. })])]);
            let cue = tessera_show::Cue {
                id: uuid::Uuid::new_v4(),
                name: name.into(),
                slot,
                layers_by_screen: layers,
                transition: tessera_show::Transition { kind: tessera_show::TransitionKind::Cut, duration_ms: 0 },
                hotkey: (slot == 2).then(|| "H".into()),
            };
            c.apply(ShowOp::SetCue { group_id, cue }).unwrap();
        };
        cue(2, "Opener");
        cue(0, "Walk-in");
        let at = |slot| crate::controller::CueRef { group_id, slot };
        c.load_cue(at(0)).unwrap();
        c.take();
        c.load_cue(at(2)).unwrap();
        let snap = snapshot(&c, "connected");
        assert_eq!(snap.show, "Untitled show");
        assert_eq!(snap.groups[0].cues.iter().map(|c| (c.number, c.name.as_str())).collect::<Vec<_>>(), [(1, "Walk-in"), (3, "Opener")]);
        assert_eq!(snap.groups[0].cues[1].hotkey.as_deref(), Some("H"));
        assert_eq!((snap.program.group, snap.program.cue, snap.program.name.as_deref(), snap.program.empty), (Some(1), Some(1), Some("Walk-in"), false));
        assert_eq!((snap.preview.cue, snap.preview.edited, snap.preview.empty), (Some(3), false, false));
        c.set_preview(Scene::new(), c.pvw.transition.clone());
        let snap = snapshot(&c, "connected");
        assert!(snap.preview.edited && snap.preview.empty);
    }
}
