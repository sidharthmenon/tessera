//! The engine sidecar process and its WebSocket. A watchdog respawns the engine when it
//! exits, when 3 connects in a row fail, or when no event arrives for 5 s (it sends Stats
//! every second); every (re)connect starts with a full resync.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tessera_show::{FromEngine, ToEngine};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_tungstenite::tungstenite::Message;

pub type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Sink = futures_util::stream::SplitSink<Ws, Message>;

const MIN_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
const MAX_CONNECT_FAILURES: u32 = 3;
const STALE: Duration = Duration::from_secs(5);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(10);
/// An engine that dies this soon after its resync counts toward the crash-loop breaker...
const QUICK_DEATH: Duration = Duration::from_secs(10);
/// ...and one that ran this long resets it.
const STABLE: Duration = Duration::from_secs(60);

/// Crash-loop breaker (I5). An engine killed by what it is asked to play (a clip that crashes the
/// decoder) dies right after every resync, and each death is a blackout. After two deaths within
/// `QUICK_DEATH` of their resync, the next resync leaves PGM's media out.
#[derive(Debug, Default)]
pub struct Breaker {
    resynced: Option<std::time::Instant>,
    quick: u32,
}

impl Breaker {
    pub fn resynced(&mut self, now: std::time::Instant) {
        self.resynced = Some(now);
    }

    pub fn died(&mut self, now: std::time::Instant) {
        match self.resynced.take().map(|t| now.saturating_duration_since(t)) {
            Some(up) if up < QUICK_DEATH => self.quick += 1,
            Some(up) if up >= STABLE => self.quick = 0,
            _ => {}
        }
    }

    /// Whether this resync must leave media out; true once per trip.
    pub fn trip(&mut self) -> bool {
        let tripped = self.quick >= 2;
        if tripped {
            self.quick = 0;
        }
        tripped
    }
}

/// Where the frontend finds the engine's previews and thumbnails, and what it may do with it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EngineInfo {
    pub port: u16,
    pub token: String,
    /// Adopted from an earlier controller: its PGM is never rewritten (R35). Set by `info()`.
    pub adopted: bool,
    /// Adopted and no show opened yet: only requests reach it. Set by `info()`.
    pub held: bool,
}

/// 32 random bytes as hex.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("no OS random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The port from the engine's stdout line `TESSERA_ENGINE_LISTENING <port>`.
pub fn parse_port(line: &str) -> Option<u16> {
    line.trim().strip_prefix("TESSERA_ENGINE_LISTENING ")?.parse().ok()
}

/// `connect`, given up after `limit`: a wedged engine's kernel still accepts the TCP
/// connection, but the WebSocket handshake never answers.
pub async fn try_connect(port: u16, token: &str, limit: Duration) -> Result<Ws, String> {
    let handshake = tokio::time::timeout(limit, connect(port, token)).await;
    handshake.map_err(|_| format!("no WebSocket handshake within {} ms", limit.as_millis()))?
}

pub async fn connect(port: u16, token: &str) -> Result<Ws, String> {
    let url = format!("ws://127.0.0.1:{port}/ws?token={token}");
    Ok(tokio_tungstenite::connect_async(url).await.map_err(|e| e.to_string())?.0)
}

/// `<app data dir>/engine.json`: the running engine, so a restarted controller can adopt it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineFile {
    pub pid: u32,
    pub port: u16,
    pub token: String,
}

const ADOPT_TIMEOUT: Duration = Duration::from_secs(1);

/// The engine recorded in `file`, if `connect(port, token)` succeeds within 1 s. Anything
/// else (no file, garbage, refused, wrong token, no answer) is None: spawn a fresh engine
/// and leave that pid alone.
pub async fn adoptable<F, Fut, T>(file: &Path, connect: F) -> Option<EngineFile>
where
    F: FnOnce(u16, String) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let rec: EngineFile = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    let answer = tokio::time::timeout(ADOPT_TIMEOUT, connect(rec.port, rec.token.clone())).await;
    matches!(answer, Ok(Ok(_))).then_some(rec)
}

/// Whether `pid` still exists (unix; elsewhere we only learn it from the socket).
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists and may be signalled.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

/// Tauri managed state: the queue to the engine and the current process.
pub struct Engine {
    tx: mpsc::UnboundedSender<ToEngine>,
    info: Mutex<Option<EngineInfo>>,
    /// The engine's pid, and its handle unless it was adopted (then it isn't our child).
    proc: Mutex<Option<(u32, Option<CommandChild>)>>,
    /// The current engine was adopted: its PGM isn't ours to rewrite, so never SetProgram.
    adopted: AtomicBool,
    /// Adopted and the operator hasn't loaded a show yet: only requests go through.
    hold: AtomicBool,
    /// The app is quitting: no more engines.
    stopping: AtomicBool,
    /// `engine.json`
    file: PathBuf,
    breaker: Mutex<Breaker>,
}

impl Engine {
    pub fn new(file: PathBuf) -> (Engine, mpsc::UnboundedReceiver<ToEngine>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let engine = Engine {
            tx,
            info: Mutex::new(None),
            proc: Mutex::new(None),
            adopted: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            file,
            breaker: Mutex::new(Breaker::default()),
        };
        (engine, rx)
    }

    /// Queue messages for the engine. Call with the controller locked, so the queue holds
    /// state in the order the controller produced it.
    pub fn send(&self, msgs: Vec<ToEngine>) {
        for msg in msgs {
            let _ = self.tx.send(msg); // the receiver lives as long as the app
        }
    }

    /// The operator loaded a show: an adopted engine may now take config and PVW.
    pub fn release_hold(&self) {
        self.hold.store(false, Ordering::SeqCst);
    }

    /// Checked as each message goes out.
    fn allowed(&self, msg: &ToEngine) -> bool {
        if self.hold.load(Ordering::SeqCst) {
            matches!(msg, ToEngine::ListMonitors | ToEngine::Probe { .. } | ToEngine::Identify { .. })
        } else {
            !(self.adopted.load(Ordering::SeqCst) && matches!(msg, ToEngine::SetProgram { .. }))
        }
    }

    pub fn info(&self) -> Option<EngineInfo> {
        let info = self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        Some(EngineInfo { adopted: self.adopted.load(Ordering::SeqCst), held: self.hold.load(Ordering::SeqCst), ..info })
    }

    /// Kill the engine process (if any) and wait for it. `unreaped`: also SIGKILL it by pid
    /// first. On macOS waitid() returns for a *stopped* child, after which shared_child holds
    /// its lock in a blocking waitpid, so `CommandChild::kill` (and `pid`) would wait for the
    /// child to exit by itself, forever for a hung engine. Pass true only while the process
    /// can't have been reaped, so its pid can't have been reused. An adopted engine isn't our
    /// child: the pid signal is the only kill, and launchd reaps it.
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub fn kill(&self, unreaped: bool) {
        let Some((pid, child)) = self.proc.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        #[cfg(unix)]
        if unreaped {
            // SAFETY: a plain syscall on the engine's pid, which can't have been reused (above).
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        match child {
            Some(child) => match child.kill() {
                Ok(()) => log!("engine: stopped pid {pid}"),
                Err(e) => log!("engine: killing pid {pid}: {e}"),
            },
            None => {
                for _ in 0..40 {
                    if !alive(pid) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                log!("engine: stopped adopted pid {pid}");
            }
        }
    }

    /// Record the current engine. False (and the engine stopped again) once quitting: checked
    /// under the same lock `shutdown` takes, so no engine outlives the app.
    fn set_proc(&self, pid: u32, child: Option<CommandChild>) -> bool {
        let mut proc = self.proc.lock().unwrap_or_else(|e| e.into_inner());
        *proc = Some((pid, child));
        drop(proc);
        if self.stopping.load(Ordering::SeqCst) {
            self.kill(true);
            return false;
        }
        true
    }

    /// The current engine was adopted from an earlier controller (we don't know its PGM).
    pub fn adopted(&self) -> bool {
        self.adopted.load(Ordering::SeqCst)
    }

    /// App exit that keeps the outputs running (C1): spawn no other engine, leave this one and
    /// `engine.json`, so the next controller adopts it.
    pub fn detach(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        log!("engine: left running for the next controller to adopt");
    }

    fn breaker(&self) -> std::sync::MutexGuard<'_, Breaker> {
        self.breaker.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Graceful app exit: stop the engine, forget it, spawn no other.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.kill(true);
        let _ = std::fs::remove_file(&self.file);
    }

    fn set_info(&self, app: &AppHandle, info: Option<EngineInfo>) {
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info;
        let _ = app.emit("engine-info", self.info());
    }
}

/// Runs for the app's lifetime: adopt the engine a previous controller left running, if it
/// answers its token; then one fresh engine after another.
pub async fn supervise(app: AppHandle, mut rx: mpsc::UnboundedReceiver<ToEngine>) {
    let file = app.state::<Engine>().file.clone();
    if let Some(rec) = adoptable(&file, |port, token| async move { connect(port, &token).await }).await {
        let (_, why) = adopt(&app, &mut rx, rec).await;
        app.state::<Engine>().breaker().died(std::time::Instant::now());
        restarting(&app, &why);
    }
    let mut delay = MIN_BACKOFF;
    while !app.state::<Engine>().stopping.load(Ordering::SeqCst) {
        let (connected, why) = run_engine(&app, &mut rx).await;
        app.state::<Engine>().breaker().died(std::time::Instant::now());
        restarting(&app, &why);
        // A crash loop (e.g. the binary can't start) backs off to one try per 2 s.
        delay = if connected { MIN_BACKOFF } else { (delay * 2).min(MAX_BACKOFF) };
        sleep(delay).await;
    }
}

fn restarting(app: &AppHandle, why: &str) {
    if app.state::<Engine>().stopping.load(Ordering::SeqCst) {
        return log!("engine: {why}; app quitting");
    }
    log!("engine: {why}; respawning");
    let _ = app.emit("engine-event", FromEngine::Error { message: format!("engine restarting: {why}") });
}

/// Keep an engine a previous controller left running. Its outputs stay as they are: nothing
/// but requests until the operator loads a show, and never SetProgram (we don't know its PGM).
async fn adopt(app: &AppHandle, rx: &mut mpsc::UnboundedReceiver<ToEngine>, rec: EngineFile) -> (bool, String) {
    let engine = app.state::<Engine>();
    log!("engine: adopted running engine pid {} on port {}", rec.pid, rec.port);
    engine.adopted.store(true, Ordering::SeqCst);
    engine.hold.store(true, Ordering::SeqCst);
    if !engine.set_proc(rec.pid, None) {
        return (false, "app quitting".into());
    }
    let (exit_tx, mut exited) = watch::channel(false);
    let pid = rec.pid;
    tauri::async_runtime::spawn(async move {
        while alive(pid) && !exit_tx.is_closed() {
            sleep(Duration::from_millis(500)).await;
        }
        let _ = exit_tx.send(true);
    });
    let ended = drive(app, rx, rec.port, rec.token, &mut exited).await;
    let gone = *exited.borrow(); // seen gone: never signal that pid again
    engine.kill(!gone);
    ended
}

/// Spawn one engine and keep it connected until it has to be replaced.
/// Returns whether it ever connected, and why it ended.
async fn run_engine(app: &AppHandle, rx: &mut mpsc::UnboundedReceiver<ToEngine>) -> (bool, String) {
    let engine = app.state::<Engine>();
    engine.adopted.store(false, Ordering::SeqCst);
    engine.hold.store(false, Ordering::SeqCst);
    let token = new_token();
    let spawned = app.path().app_cache_dir().map_err(|e| e.to_string()).and_then(|cache| {
        let cmd = app.shell().sidecar("tessera-render").map_err(|e| e.to_string())?;
        let cmd = cmd.args(["--port".as_ref(), "0".as_ref(), "--cache-dir".as_ref(), cache.as_os_str()]);
        cmd.env("TESSERA_TOKEN", &token).spawn().map_err(|e| e.to_string())
    });
    let (events, child) = match spawned {
        Ok(spawned) => spawned,
        Err(e) => return (false, format!("can't start the tessera-render sidecar: {e}")),
    };
    let pid = child.pid(); // before the pump's wait thread can sit on shared_child's lock
    log!("engine: spawned pid {pid}");
    if !engine.set_proc(pid, Some(child)) {
        return (false, "app quitting".into());
    }
    let (port_rx, mut exited) = pump(events);
    let port = tokio::select! {
        Ok(port) = port_rx => Ok(port),
        _ = gone(&mut exited) => Err("exited before listening"),
        _ = sleep(LISTEN_TIMEOUT) => Err("no TESSERA_ENGINE_LISTENING line"),
    };
    let ended = match port {
        Ok(port) => {
            let rec = EngineFile { pid, port, token: token.clone() };
            let json = serde_json::to_vec(&rec).map_err(|e| e.to_string());
            if let Err(e) = json.and_then(|json| crate::files::write_atomic(&engine.file, &json, true)) {
                log!("engine: {e}"); // only adoption after a controller crash needs it
            }
            drive(app, rx, port, token, &mut exited).await
        }
        Err(why) => (false, why.into()),
    };
    let reaped = *exited.borrow(); // true only once the pump saw the child reaped
    engine.kill(!reaped);
    ended
}

/// Connect, reconnecting with backoff, until the engine is gone or must be replaced.
async fn drive(
    app: &AppHandle,
    rx: &mut mpsc::UnboundedReceiver<ToEngine>,
    port: u16,
    token: String,
    exited: &mut watch::Receiver<bool>,
) -> (bool, String) {
    let engine = app.state::<Engine>();
    let info = EngineInfo { port, token, adopted: false, held: false };
    let (mut failures, mut backoff, mut connected) = (0, MIN_BACKOFF, false);
    loop {
        let attempt = tokio::select! {
            ws = try_connect(port, &info.token, STALE) => ws,
            _ = gone(exited) => return (connected, "process exited".into()),
        };
        match attempt {
            Ok(ws) => {
                (failures, backoff, connected) = (0, MIN_BACKOFF, true);
                log!("engine: connected on port {port}");
                engine.set_info(app, Some(info.clone()));
                let end = session(app, ws, rx, exited).await;
                engine.set_info(app, None);
                match end {
                    End::Exited => return (true, "process exited".into()),
                    End::Stale => return (true, format!("no event for {} s", STALE.as_secs())),
                    End::Closed(why) => log!("engine: connection lost ({why}); reconnecting"),
                }
            }
            Err(e) => {
                failures += 1;
                log!("engine: connect failed ({e}), {failures} in a row");
                if failures >= MAX_CONNECT_FAILURES {
                    return (connected, format!("{failures} connects failed"));
                }
            }
        }
        tokio::select! {
            _ = sleep(backoff) => {}
            _ = gone(exited) => return (connected, "process exited".into()),
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Drain the sidecar's output (it blocks when nobody reads): log stderr, pick out the port
/// line, flag the exit.
fn pump(mut events: tauri::async_runtime::Receiver<CommandEvent>) -> (oneshot::Receiver<u16>, watch::Receiver<bool>) {
    let (port_tx, port_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = watch::channel(false);
    tauri::async_runtime::spawn(async move {
        let mut port_tx = Some(port_tx);
        while let Some(event) = events.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    let line = String::from_utf8_lossy(&line);
                    if let Some(port) = parse_port(&line)
                        && let Some(tx) = port_tx.take()
                    {
                        let _ = tx.send(port);
                    } else {
                        log!("[engine stdout] {}", line.trim_end());
                    }
                }
                CommandEvent::Stderr(line) => log!("[engine] {}", String::from_utf8_lossy(&line).trim_end()),
                CommandEvent::Terminated(t) => log!("engine: exited (code {:?}, signal {:?})", t.code, t.signal),
                CommandEvent::Error(e) => log!("engine: {e}"),
                _ => {}
            }
        }
        let _ = exit_tx.send(true);
    });
    (port_rx, exit_rx)
}

/// Resolves once the process has exited.
async fn gone(exited: &mut watch::Receiver<bool>) {
    let _ = exited.wait_for(|e| *e).await;
}

enum End {
    Exited,
    Stale,
    Closed(String),
}

/// Send, but give up at `deadline`: an engine that stops reading fills the socket buffer and
/// would block us (and the stale check) forever.
async fn send(sink: &mut Sink, msg: &ToEngine, deadline: Instant) -> Result<(), End> {
    let json = serde_json::to_string(msg).map_err(|e| End::Closed(e.to_string()))?;
    match tokio::time::timeout_at(deadline, sink.send(Message::text(json))).await {
        Ok(sent) => sent.map_err(|e| End::Closed(e.to_string())),
        Err(_) => Err(End::Stale),
    }
}

async fn session(
    app: &AppHandle,
    ws: Ws,
    rx: &mut mpsc::UnboundedReceiver<ToEngine>,
    exited: &mut watch::Receiver<bool>,
) -> End {
    let engine = app.state::<Engine>();
    let (mut sink, mut stream) = ws.split();
    let safe = engine.breaker().trip();
    let resync: Vec<_> = crate::resync(app, rx, safe).into_iter().filter(|m| engine.allowed(m)).collect();
    // Before sending: an engine that dies of the resync itself must count.
    engine.breaker().resynced(std::time::Instant::now());
    let mut deadline = Instant::now() + STALE;
    for msg in &resync {
        if let Err(end) = send(&mut sink, msg, deadline).await {
            return end;
        }
    }
    log!("engine: resync sent ({} messages)", resync.len());
    loop {
        tokio::select! {
            _ = gone(exited) => return End::Exited,
            _ = sleep_until(deadline) => return End::Stale,
            msg = rx.recv() => match msg {
                Some(msg) if !engine.allowed(&msg) => {}
                Some(msg) => if let Err(end) = send(&mut sink, &msg, deadline).await { return end },
                None => return End::Closed("app shutting down".into()),
            },
            frame = stream.next() => {
                deadline = Instant::now() + STALE;
                match frame {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<FromEngine>(&text) {
                        Ok(event) => crate::engine_event(app, event),
                        Err(e) => log!("engine: bad event ({e}): {text}"),
                    },
                    Some(Ok(Message::Close(_))) | None => return End::Closed("closed by engine".into()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return End::Closed(e.to_string()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_line() {
        assert_eq!(parse_port("TESSERA_ENGINE_LISTENING 58126\n"), Some(58126));
        assert_eq!(parse_port("TESSERA_ENGINE_LISTENING x"), None);
        assert_eq!(parse_port("hello"), None);
    }

    #[tokio::test]
    async fn adopt_only_when_token_authenticates() {
        let dir = std::env::temp_dir().join(format!("tessera-adopt-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("engine.json");
        // The stub engine on port 4000 accepts only "good".
        let stub = |port: u16, token: String| async move {
            if port == 4000 && token == "good" { Ok(()) } else { Err("HTTP 401".to_string()) }
        };
        assert_eq!(adoptable(&file, stub).await, None, "no file");
        let rec = |token: &str| EngineFile { pid: 4242, port: 4000, token: token.into() };
        std::fs::write(&file, serde_json::to_vec(&rec("good")).unwrap()).unwrap();
        assert_eq!(adoptable(&file, stub).await, Some(rec("good")));
        std::fs::write(&file, serde_json::to_vec(&rec("stale")).unwrap()).unwrap();
        assert_eq!(adoptable(&file, stub).await, None, "wrong token");
        std::fs::write(&file, b"{not json").unwrap();
        assert_eq!(adoptable(&file, stub).await, None, "garbage");
        // Something that never answers: given up after ~1 s.
        std::fs::write(&file, serde_json::to_vec(&rec("good")).unwrap()).unwrap();
        let start = std::time::Instant::now();
        let hang = |_: u16, _: String| std::future::pending::<Result<(), String>>();
        assert_eq!(adoptable(&file, hang).await, None, "no answer");
        assert!(start.elapsed() < Duration::from_secs(2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn connect_gives_up_on_silent_listener() {
        // Accepts the TCP connection (like a stopped engine's kernel backlog), never answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(5));
            drop(held);
        });
        let start = std::time::Instant::now();
        assert!(try_connect(port, "t", Duration::from_millis(300)).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    }

    #[test]
    fn adopted_engine_keeps_its_program() {
        let (engine, _rx) = Engine::new(PathBuf::from("/nonexistent/engine.json"));
        let program = ToEngine::SetProgram { scene: Default::default() };
        let preview = ToEngine::SetPreview { scene: Default::default() };
        let probe = ToEngine::Probe { media_id: uuid::Uuid::new_v4(), path: "/a.mp4".into() };
        assert!(engine.allowed(&program) && engine.allowed(&preview), "our own engine gets everything");
        engine.adopted.store(true, Ordering::SeqCst);
        engine.hold.store(true, Ordering::SeqCst);
        assert!(!engine.allowed(&program) && !engine.allowed(&preview), "no show loaded yet");
        assert!(engine.allowed(&probe) && engine.allowed(&ToEngine::ListMonitors));
        engine.release_hold();
        assert!(engine.allowed(&preview));
        assert!(!engine.allowed(&program), "never rewrite an adopted engine's PGM");
    }

    /// I5: two deaths within 10 s of their resync trip the breaker for one resync; a death after a
    /// minute up resets the count; a death before any resync doesn't count.
    #[test]
    fn crash_loop_breaker() {
        use std::time::Instant;
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = Breaker::default();
        b.died(s(0));
        assert!(!b.trip(), "died before it was ever resynced");
        b.resynced(s(1));
        b.died(s(3));
        assert!(!b.trip(), "one quick death");
        b.resynced(s(4));
        b.died(s(6));
        assert!(b.trip(), "two quick deaths: this resync leaves media out");
        assert!(!b.trip(), "once per trip");
        // 30 s up neither counts nor resets; a minute up resets.
        b.resynced(s(10));
        b.died(s(12));
        b.resynced(s(13));
        b.died(s(43));
        b.resynced(s(44));
        b.died(s(46));
        assert!(b.trip(), "quick, 30 s, quick");
        b.resynced(s(50));
        b.died(s(52));
        b.resynced(s(53));
        b.died(s(120));
        b.resynced(s(121));
        b.died(s(123));
        assert!(!b.trip(), "a stable minute in between resets the count");
    }

    #[test]
    fn token_is_32_random_bytes() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
