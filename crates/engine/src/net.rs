//! The controller's way in (R29): WebSocket control, MJPEG previews and thumbnails on 127.0.0.1,
//! every request gated by `?token=`. axum runs on a thread of its own, so the render loop never
//! waits on the network, and a server failure is logged, not fatal.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, ensure};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tessera_show::{Bus, FromEngine, ToEngine};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

use crate::app::EngineHandle;
use crate::probe::probe;

/// Probes decoding at once; the rest wait their turn.
const PROBES: usize = 2;
const JPEG_QUALITY: u8 = 70;

/// The latest preview JPEG of each bus (indexed by `Bus as usize`). The render loop reads a bus
/// back only while it is `watched`: the channel's receiver count is the per-bus subscriber count.
pub struct Previews {
    frames: [watch::Sender<Bytes>; 2],
}

impl Previews {
    pub fn new() -> Arc<Previews> {
        Arc::new(Previews { frames: [watch::Sender::new(Bytes::new()), watch::Sender::new(Bytes::new())] })
    }

    /// At least one MJPEG client is streaming `bus`.
    pub fn watched(&self, bus: Bus) -> bool {
        self.frames[bus as usize].receiver_count() > 0
    }

    /// Encodes `rgba` (`width`×`height`, rows tightly packed) as `bus`'s next JPEG. Takes a few ms:
    /// call it off the render thread.
    pub fn publish_rgba(&self, bus: Bus, width: u32, height: u32, rgba: Vec<u8>) -> anyhow::Result<()> {
        let image = image::RgbaImage::from_raw(width, height, rgba).context("preview: buffer doesn't match its size")?;
        let mut jpeg = vec![];
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).encode_image(&image)?;
        self.frames[bus as usize].send_replace(jpeg.into());
        Ok(())
    }
}

#[derive(Clone)]
struct Shared {
    handle: EngineHandle,
    events: broadcast::Sender<FromEngine>,
    previews: Arc<Previews>,
    thumbs: Arc<Path>,
    probes: Arc<Semaphore>,
}

/// Serves the controller on `addr` (loopback only; port 0 picks a free one) from its own thread
/// and returns the bound address. WebSocket commands go to `handle`, except `Probe`, answered here
/// (thumbnails go to `thumbs`, which `/thumb/` serves); everything sent on `events` goes to every
/// WebSocket client.
pub fn serve(
    addr: SocketAddr,
    token: String,
    handle: EngineHandle,
    events: broadcast::Sender<FromEngine>,
    previews: Arc<Previews>,
    thumbs: PathBuf,
) -> anyhow::Result<SocketAddr> {
    ensure!(addr.ip().is_loopback(), "refusing to listen on {addr}: loopback only");
    ensure!(!token.is_empty(), "empty token");
    let listener = std::net::TcpListener::bind(addr).with_context(|| format!("listen on {addr}"))?;
    listener.set_nonblocking(true)?;
    let bound = listener.local_addr()?;
    let shared = Shared { handle, events, previews, thumbs: thumbs.into(), probes: Arc::new(Semaphore::new(PROBES)) };
    let app = Router::new()
        .route("/ws", get(ws))
        .route("/preview/pgm.mjpg", get(|State(s): State<Shared>| async move { mjpeg(&s.previews, Bus::Pgm) }))
        .route("/preview/pvw.mjpg", get(|State(s): State<Shared>| async move { mjpeg(&s.previews, Bus::Pvw) }))
        .route("/thumb/{file}", get(thumb))
        .layer(middleware::from_fn_with_state(Arc::<str>::from(token), auth))
        .with_state(shared);
    std::thread::Builder::new().name("net".into()).spawn(move || {
        let served = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(anyhow::Error::from).and_then(|rt| {
            rt.block_on(async { Ok(axum::serve(tokio::net::TcpListener::from_std(listener)?, app).await?) })
        });
        if let Err(e) = served {
            eprintln!("net: server stopped: {e:#}");
        }
    })?;
    Ok(bound)
}

/// 401 unless the query's `token` is the engine's.
async fn auth(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    let query = Query::<HashMap<String, String>>::try_from_uri(request.uri());
    if query.is_ok_and(|q| q.get("token").is_some_and(|t| same(t.as_bytes(), token.as_bytes()))) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Equal, in a time that depends only on the lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && std::hint::black_box(a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y))) == 0
}

async fn ws(State(s): State<Shared>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| client(s, socket))
}

/// One controller connection: JSON `ToEngine` in; every engine event out, plus an `Error` for a
/// message that isn't a command (the connection stays up).
async fn client(s: Shared, mut socket: WebSocket) {
    let mut events = s.events.subscribe();
    loop {
        let event = tokio::select! {
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => match serde_json::from_str(text.as_str()) {
                    Ok(command) => {
                        s.command(command);
                        continue;
                    }
                    Err(e) => FromEngine::Error { message: format!("bad command: {e}") },
                },
                Some(Ok(Message::Binary(_))) => FromEngine::Error { message: "bad command: expected JSON text".into() },
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => continue, // ping, pong
            },
            event = events.recv() => match event {
                Ok(event) => event,
                Err(RecvError::Lagged(n)) => {
                    eprintln!("net: a client fell behind, {n} events dropped");
                    continue;
                }
                Err(RecvError::Closed) => return,
            },
        };
        let Ok(json) = serde_json::to_string(&event) else { continue };
        if socket.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }
}

impl Shared {
    fn command(&self, command: ToEngine) {
        match command {
            ToEngine::Probe { media_id, path } => self.probe(media_id, path),
            command => {
                if !self.handle.send(command) {
                    let _ = self.events.send(FromEngine::Error { message: "engine stopped".into() });
                }
            }
        }
    }

    /// Answers with `Probed`, `PROBES` at a time, on blocking threads; a failure (even a panic)
    /// is reported in `error`.
    fn probe(&self, media_id: Uuid, path: String) {
        let (events, thumbs, probes) = (self.events.clone(), self.thumbs.clone(), self.probes.clone());
        tokio::spawn(async move {
            let Ok(_permit) = probes.acquire_owned().await else { return };
            let probed = tokio::task::spawn_blocking(move || probe(Path::new(&path), &thumbs, media_id)).await;
            let (info, error) = match probed {
                Ok(Ok(info)) => (Some(info), None),
                Ok(Err(e)) => (None, Some(format!("{e:#}"))),
                Err(e) => (None, Some(format!("probe failed: {e}"))),
            };
            let _ = events.send(FromEngine::Probed { media_id, info, error });
        });
    }
}

/// `bus`'s JPEGs as `multipart/x-mixed-replace`, a part per new frame (the render loop makes at
/// most 15 a second while a stream is open). A new stream waits for a fresh frame.
fn mjpeg(previews: &Previews, bus: Bus) -> Response {
    let frames = previews.frames[bus as usize].subscribe(); // marks the current (stale) frame seen
    let parts = futures_util::stream::unfold(frames, |mut frames| async move {
        frames.changed().await.ok()?;
        let jpeg = frames.borrow_and_update().clone();
        let mut part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).into_bytes();
        part.extend_from_slice(&jpeg);
        part.extend_from_slice(b"\r\n");
        Some((Ok::<_, Infallible>(Bytes::from(part)), frames))
    });
    let headers = [(header::CONTENT_TYPE, "multipart/x-mixed-replace; boundary=frame"), (header::CACHE_CONTROL, "no-store")];
    (headers, Body::from_stream(parts)).into_response()
}

/// `/thumb/<media id>.jpg` from the thumbnail dir. The name is rebuilt from the parsed UUID, so
/// nothing outside that dir is reachable.
async fn thumb(State(s): State<Shared>, UrlPath(file): UrlPath<String>) -> Response {
    let Some(id) = file.strip_suffix(".jpg").and_then(|id| Uuid::parse_str(id).ok()) else {
        return (StatusCode::BAD_REQUEST, "expected /thumb/<media id>.jpg").into_response();
    };
    match tokio::fs::read(s.thumbs.join(format!("{id}.jpg"))).await {
        Ok(jpeg) => ([(header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
