//! The client half against the real `tessera-engine` (built by `cargo build -p tessera-engine`;
//! `cargo test --workspace` builds it). An empty config opens no windows.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tessera_app_lib::engine_client::{connect, new_token, parse_port};
use tessera_show::{FromEngine, ToEngine};
use tokio_tungstenite::tungstenite::Message;

/// Kills the engine even when an assert fails.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn real_engine_answers_list_monitors() {
    let target = option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let exe = target.join("debug").join(format!("tessera-engine{}", std::env::consts::EXE_SUFFIX));
    assert!(exe.exists(), "{} missing: cargo build -p tessera-engine", exe.display());
    let cache = std::env::temp_dir().join(format!("tessera-app-engine-test-{}", std::process::id()));
    let token = new_token();
    let mut engine = Kill(
        Command::new(&exe)
            .args(["--port".as_ref(), "0".as_ref(), "--cache-dir".as_ref(), cache.as_os_str()])
            .env("TESSERA_TOKEN", &token)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(engine.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let port = parse_port(&line).unwrap_or_else(|| panic!("not a port line: {line:?}"));
    assert!(connect(port, "wrong").await.is_err(), "bad token accepted");
    let mut ws = connect(port, &token).await.unwrap();
    ws.send(Message::text(serde_json::to_string(&ToEngine::ListMonitors).unwrap())).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text).unwrap() {
                    FromEngine::Monitors { monitors } => return monitors,
                    _ => continue, // Stats / Positions
                },
                other => panic!("{other:?}"),
            }
        }
    });
    let monitors = reply.await.expect("no Monitors within 10 s");
    eprintln!("monitors: {monitors:?}");
    drop(engine);
    let _ = std::fs::remove_dir_all(cache);
}
