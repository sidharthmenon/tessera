//! The engine sidecar process and its WebSocket. A watchdog respawns the engine when it
//! exits, when 3 connects in a row fail, or when no event arrives for 5 s (it sends Stats
//! every second); every (re)connect starts with a full resync.

use std::sync::Mutex;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
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

/// Where the frontend finds the engine's previews and thumbnails.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EngineInfo {
    pub port: u16,
    pub token: String,
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

pub async fn connect(port: u16, token: &str) -> Result<Ws, String> {
    let url = format!("ws://127.0.0.1:{port}/ws?token={token}");
    Ok(tokio_tungstenite::connect_async(url).await.map_err(|e| e.to_string())?.0)
}

/// Tauri managed state: the queue to the engine and the current process.
pub struct Engine {
    tx: mpsc::UnboundedSender<ToEngine>,
    info: Mutex<Option<EngineInfo>>,
    child: Mutex<Option<(u32, CommandChild)>>,
}

impl Engine {
    pub fn new() -> (Engine, mpsc::UnboundedReceiver<ToEngine>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Engine { tx, info: Mutex::new(None), child: Mutex::new(None) }, rx)
    }

    /// Queue messages for the engine. Call with the controller locked, so the queue holds
    /// state in the order the controller produced it.
    pub fn send(&self, msgs: Vec<ToEngine>) {
        for msg in msgs {
            let _ = self.tx.send(msg); // the receiver lives as long as the app
        }
    }

    pub fn info(&self) -> Option<EngineInfo> {
        self.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Kill the engine process (if any) and reap it. `unreaped`: also SIGKILL it by pid
    /// first. On macOS waitid() returns for a *stopped* child, after which shared_child holds
    /// its lock in a blocking waitpid, so `CommandChild::kill` (and `pid`) would wait for the
    /// child to exit by itself, forever for a hung engine. Pass true only while the child
    /// can't have been reaped, so its pid can't have been reused.
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub fn kill(&self, unreaped: bool) {
        let Some((pid, child)) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        #[cfg(unix)]
        if unreaped {
            // SAFETY: a plain syscall on our own unreaped child's pid.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        match child.kill() {
            Ok(()) => log!("engine: stopped pid {pid}"),
            Err(e) => log!("engine: killing pid {pid}: {e}"),
        }
    }

    fn set_info(&self, app: &AppHandle, info: Option<EngineInfo>) {
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info.clone();
        let _ = app.emit("engine-info", info);
    }
}

/// Runs for the app's lifetime: one engine process after another.
pub async fn supervise(app: AppHandle, mut rx: mpsc::UnboundedReceiver<ToEngine>) {
    let mut delay = MIN_BACKOFF;
    loop {
        let (connected, why) = run_engine(&app, &mut rx).await;
        log!("engine: {why}; respawning");
        let _ = app.emit("engine-event", FromEngine::Error { message: format!("engine restarting: {why}") });
        // A crash loop (e.g. the binary can't start) backs off to one try per 2 s.
        delay = if connected { MIN_BACKOFF } else { (delay * 2).min(MAX_BACKOFF) };
        sleep(delay).await;
    }
}

/// Spawn one engine and keep it connected until it has to be replaced.
/// Returns whether it ever connected, and why it ended.
async fn run_engine(app: &AppHandle, rx: &mut mpsc::UnboundedReceiver<ToEngine>) -> (bool, String) {
    let engine = app.state::<Engine>();
    let token = new_token();
    let spawned = app.path().app_cache_dir().map_err(|e| e.to_string()).and_then(|cache| {
        let cmd = app.shell().sidecar("tessera-engine").map_err(|e| e.to_string())?;
        let cmd = cmd.args(["--port".as_ref(), "0".as_ref(), "--cache-dir".as_ref(), cache.as_os_str()]);
        cmd.env("TESSERA_TOKEN", &token).spawn().map_err(|e| e.to_string())
    });
    let (events, child) = match spawned {
        Ok(spawned) => spawned,
        Err(e) => return (false, format!("can't start tessera-engine: {e}")),
    };
    let pid = child.pid(); // before the pump's wait thread can sit on shared_child's lock
    log!("engine: spawned pid {pid}");
    *engine.child.lock().unwrap_or_else(|e| e.into_inner()) = Some((pid, child));
    let (port_rx, mut exited) = pump(events);
    let ended = drive(app, rx, port_rx, &mut exited, token).await;
    let reaped = *exited.borrow(); // true only once the pump saw the child reaped
    engine.kill(!reaped);
    ended
}

/// Wait for the port line, then connect, reconnecting with backoff, until the engine is
/// gone or must be replaced.
async fn drive(
    app: &AppHandle,
    rx: &mut mpsc::UnboundedReceiver<ToEngine>,
    port_rx: oneshot::Receiver<u16>,
    exited: &mut watch::Receiver<bool>,
    token: String,
) -> (bool, String) {
    let engine = app.state::<Engine>();
    let port = tokio::select! {
        Ok(port) = port_rx => port,
        _ = gone(exited) => return (false, "exited before listening".into()),
        _ = sleep(LISTEN_TIMEOUT) => return (false, "no TESSERA_ENGINE_LISTENING line".into()),
    };
    let info = EngineInfo { port, token };
    let (mut failures, mut backoff, mut connected) = (0, MIN_BACKOFF, false);
    loop {
        let attempt = tokio::select! {
            ws = connect(port, &info.token) => ws,
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

async fn send(sink: &mut Sink, msg: &ToEngine) -> Result<(), String> {
    let json = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    sink.send(Message::text(json)).await.map_err(|e| e.to_string())
}

async fn session(
    app: &AppHandle,
    ws: Ws,
    rx: &mut mpsc::UnboundedReceiver<ToEngine>,
    exited: &mut watch::Receiver<bool>,
) -> End {
    let (mut sink, mut stream) = ws.split();
    let resync = crate::resync(app, rx);
    log!("engine: resync sent ({} messages)", resync.len());
    for msg in &resync {
        if let Err(e) = send(&mut sink, msg).await {
            return End::Closed(e);
        }
    }
    let mut deadline = Instant::now() + STALE;
    loop {
        tokio::select! {
            _ = gone(exited) => return End::Exited,
            _ = sleep_until(deadline) => return End::Stale,
            msg = rx.recv() => match msg {
                Some(msg) => if let Err(e) = send(&mut sink, &msg).await { return End::Closed(e) },
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

    #[test]
    fn token_is_32_random_bytes() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
