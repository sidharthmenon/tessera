//! Task 10: the engine's network surface (in-process server on port 0, stub engine) and probe.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tessera_engine::app::EngineHandle;
use tessera_engine::net::{self, Previews};
use tessera_engine::probe::probe;
use tessera_show::{Bus, FromEngine, MediaKind, ToEngine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self, Message};
use uuid::Uuid;

const TOKEN: &str = "test-token";
const WAIT: Duration = Duration::from_secs(5);

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tessera-net-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Server {
    addr: SocketAddr,
    previews: Arc<Previews>,
    /// The cache dir; thumbnails live in `thumbs/`.
    dir: PathBuf,
}

/// Server on 127.0.0.1:0 in front of a stub engine that answers ListMonitors with no monitors.
fn server() -> Server {
    let (events, _) = broadcast::channel(16);
    let replies = events.clone();
    let handle = EngineHandle::new(move |command| {
        if command == ToEngine::ListMonitors {
            let _ = replies.send(FromEngine::Monitors { monitors: vec![] });
        }
        true
    });
    let (previews, dir) = (Previews::new(), temp_dir());
    let addr = net::serve(([127, 0, 0, 1], 0).into(), TOKEN.into(), handle, events, previews.clone(), dir.join("thumbs")).unwrap();
    assert_ne!(addr.port(), 0, "reports the bound port");
    Server { addr, previews, dir }
}

async fn connect(s: &Server, query: &str) -> Result<Ws, tungstenite::Error> {
    Ok(tokio_tungstenite::connect_async(format!("ws://{}/ws{query}", s.addr)).await?.0)
}

async fn send(ws: &mut Ws, text: &str) {
    ws.send(Message::text(text)).await.unwrap();
}

async fn next_event(ws: &mut Ws) -> FromEngine {
    loop {
        match timeout(WAIT, ws.next()).await.expect("no event within 5 s") {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("connection ended: {other:?}"),
        }
    }
}

/// Raw HTTP/1.1 GET; reads until `done(response so far)`, the server closes, or 5 s pass.
async fn get_until(s: &Server, path: &str, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut tcp = TcpStream::connect(s.addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(request.as_bytes()).await.unwrap();
    let (mut response, mut chunk) = (vec![], [0; 4096]);
    let _ = timeout(WAIT, async {
        while let Ok(n @ 1..) = tcp.read(&mut chunk).await {
            response.extend_from_slice(&chunk[..n]);
            if done(&response) {
                break;
            }
        }
    })
    .await;
    response
}

async fn get(s: &Server, path: &str) -> Vec<u8> {
    get_until(s, path, |_| false).await
}

fn status(response: &[u8]) -> u16 {
    std::str::from_utf8(response.get(9..12).unwrap_or_default()).ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The first multipart part's body (what follows `--frame`, its headers and the blank line).
fn first_part(response: &[u8]) -> Option<(&[u8], &[u8])> {
    let part = &response[find(response, b"--frame\r\n")?..];
    let body = find(part, b"\r\n\r\n")? + 4;
    Some((&part[..body], &part[body..]))
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let at = find(headers, b"Content-Length: ")? + 16;
    std::str::from_utf8(&headers[at..]).ok()?.split("\r\n").next()?.parse().ok()
}

#[tokio::test]
async fn ws_rejects_bad_token() {
    let s = server();
    for query in ["?token=wrong", "?token=", "", &format!("?token={TOKEN}x"), &format!("?other={TOKEN}")] {
        match connect(&s, query).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401, "{query:?}"),
            Err(e) => panic!("{query:?}: expected HTTP 401, got {e}"),
            Ok(_) => panic!("{query:?}: connected without the token"),
        }
    }
    assert!(connect(&s, &format!("?token={TOKEN}")).await.is_ok(), "the right token connects");
}

#[tokio::test]
async fn ws_list_monitors_replies() {
    let s = server();
    let mut ws = connect(&s, &format!("?token={TOKEN}")).await.unwrap();
    send(&mut ws, r#"{"type":"ListMonitors"}"#).await;
    assert!(matches!(next_event(&mut ws).await, FromEngine::Monitors { .. }));
}

#[tokio::test]
async fn ws_malformed_json_returns_error() {
    let s = server();
    let mut ws = connect(&s, &format!("?token={TOKEN}")).await.unwrap();
    for bad in ["{not json", r#"{"type":"NoSuchCommand"}"#, r#"{"type":"Take"}"#] {
        send(&mut ws, bad).await;
        assert!(matches!(next_event(&mut ws).await, FromEngine::Error { .. }), "{bad}");
    }
    send(&mut ws, r#"{"type":"ListMonitors"}"#).await;
    assert!(matches!(next_event(&mut ws).await, FromEngine::Monitors { .. }), "connection kept");
}

/// Probe over WS: answered with `Probed` (info or error, never a dropped request), and the
/// thumbnail it wrote is then served.
#[tokio::test]
async fn ws_probe_replies_and_serves_thumb() {
    let s = server();
    let mut ws = connect(&s, &format!("?token={TOKEN}")).await.unwrap();
    let (good, bad) = (Uuid::new_v4(), Uuid::new_v4());
    let probe = |id: Uuid, path: &Path| serde_json::to_string(&ToEngine::Probe { media_id: id, path: path.display().to_string() });
    send(&mut ws, &probe(good, &fixture("h264_64.mp4")).unwrap()).await;
    send(&mut ws, &probe(bad, Path::new("/nonexistent/clip.mp4")).unwrap()).await;
    let mut answered = vec![];
    while answered.len() < 2 {
        if let FromEngine::Probed { media_id, info, error } = next_event(&mut ws).await {
            answered.push((media_id, info.is_some(), error.is_some()));
        }
    }
    answered.sort_by_key(|(id, ..)| *id != good);
    assert_eq!(answered, [(good, true, false), (bad, false, true)]);
    let thumb = get(&s, &format!("/thumb/{good}.jpg?token={TOKEN}")).await;
    assert_eq!(status(&thumb), 200);
    assert!(find(&thumb, b"image/jpeg").is_some());
    assert!(find(&thumb, b"\r\n\r\n\xFF\xD8").is_some(), "a JPEG body");
}

#[tokio::test]
async fn preview_requires_token() {
    let s = server();
    for path in ["/preview/pgm.mjpg", "/preview/pvw.mjpg?token=wrong", "/thumb/x.jpg", "/ws"] {
        assert_eq!(status(&get(&s, path).await), 401, "{path}");
    }
    assert!(!s.previews.watched(Bus::Pgm) && !s.previews.watched(Bus::Pvw), "rejected requests don't subscribe");
}

#[tokio::test]
async fn thumb_rejects_non_uuid() {
    let s = server();
    let thumbs = s.dir.join("thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    std::fs::write(s.dir.join("secret.jpg"), b"SECRET").unwrap(); // outside thumbs/
    let id = Uuid::new_v4();
    std::fs::write(thumbs.join(format!("{id}.jpg")), b"\xFF\xD8thumb").unwrap();
    let bad = ["not-a-uuid.jpg", "..%2Fsecret.jpg", "%2E%2E%2Fsecret.jpg", "..%5Csecret.jpg", &format!("{id}.png"), &format!("{id}")];
    for name in bad {
        let response = get(&s, &format!("/thumb/{name}?token={TOKEN}")).await;
        assert!(matches!(status(&response), 400 | 404), "{name}: {}", status(&response));
        assert!(find(&response, b"SECRET").is_none() && find(&response, b"\xFF\xD8thumb").is_none(), "{name} served a file");
    }
    assert_eq!(status(&get(&s, &format!("/thumb/{}.jpg?token={TOKEN}", Uuid::new_v4())).await), 404, "absent");
    let ok = get(&s, &format!("/thumb/{id}.jpg?token={TOKEN}")).await;
    assert_eq!(status(&ok), 200);
    assert!(ok.ends_with(b"\r\n\r\n\xFF\xD8thumb"));
}

#[tokio::test]
async fn mjpeg_preview_streams_jpeg_parts() {
    let s = server();
    assert!(!s.previews.watched(Bus::Pgm), "nobody watching yet");
    // Stand-in for the render loop: a frame every 10 ms while someone watches.
    let previews = s.previews.clone();
    std::thread::spawn(move || {
        for _ in 0..500 {
            if previews.watched(Bus::Pgm) {
                let red = [[220, 30, 40, 255]; 64 * 16].concat();
                previews.publish_rgba(Bus::Pgm, 64, 16, red).unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let whole_part = |r: &[u8]| first_part(r).is_some_and(|(h, b)| content_length(h).is_some_and(|n| b.len() >= n));
    let response = get_until(&s, &format!("/preview/pgm.mjpg?token={TOKEN}"), whole_part).await;
    assert_eq!(status(&response), 200);
    assert!(find(&response, b"multipart/x-mixed-replace; boundary=frame").is_some(), "{}", String::from_utf8_lossy(&response));
    let (headers, body) = first_part(&response).expect("a multipart part");
    assert!(find(headers, b"Content-Type: image/jpeg").is_some(), "{}", String::from_utf8_lossy(headers));
    assert_eq!(body[..2], [0xFF, 0xD8], "JPEG SOI");
    let jpeg = image::load_from_memory(&body[..content_length(headers).unwrap()]).unwrap().into_rgb8();
    assert_eq!(jpeg.dimensions(), (64, 16));
    let got = jpeg.get_pixel(32, 8).0;
    assert!(got.iter().zip([220, 30, 40]).all(|(g, w)| g.abs_diff(w) <= 8), "the published picture: {got:?}");
    assert!(!s.previews.watched(Bus::Pvw), "PVW unwatched: never read back");
    let deadline = Instant::now() + WAIT;
    while s.previews.watched(Bus::Pgm) {
        assert!(Instant::now() < deadline, "the closed connection still counts as watching");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn probe_fixture_video() {
    let dir = temp_dir();
    let id = Uuid::new_v4();
    let info = probe(&fixture("h264_64.mp4"), &dir, id).unwrap();
    assert_eq!((info.kind, info.width, info.height), (MediaKind::Video, 64, 64));
    assert!(info.has_audio && !info.has_alpha);
    let ms = info.duration_ms.expect("duration");
    assert!(ms.abs_diff(1000) <= 50, "{ms} ms");
    assert_eq!(info.thumb, Some(format!("{id}.jpg")));
    assert_eq!(image::image_dimensions(dir.join(format!("{id}.jpg"))).unwrap(), (160, 160));
    let prores = probe(&fixture("prores4444_64.mov"), &dir, Uuid::new_v4()).unwrap();
    assert!(prores.has_alpha && !prores.has_audio, "ProRes 4444 with alpha, silent");
}

#[test]
fn probe_image() {
    let dir = temp_dir();
    let png = dir.join("still.png");
    image::RgbaImage::from_pixel(320, 80, image::Rgba([255, 0, 0, 128])).save(&png).unwrap();
    let id = Uuid::new_v4();
    let info = probe(&png, &dir.join("thumbs"), id).unwrap();
    assert_eq!((info.kind, info.width, info.height, info.duration_ms), (MediaKind::Image, 320, 80, None));
    assert!(info.has_alpha && !info.has_audio);
    assert_eq!(image::image_dimensions(dir.join("thumbs").join(format!("{id}.jpg"))).unwrap(), (160, 40));
}

#[test]
fn probe_missing_is_err() {
    let dir = temp_dir();
    for path in [Path::new("/nonexistent/clip.mp4"), Path::new("/nonexistent/still.png"), Path::new("/tmp/bad\0clip.mp4"), &fixture("gen.sh")] {
        assert!(probe(path, &dir, Uuid::new_v4()).is_err(), "{path:?}");
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no thumbnails written");
}
