//! The controller owns the engine's stdout/stderr pipes. When it dies (force-quit) the pipes
//! break; the engine must keep running and serving events, not panic on its next log line.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn keeps_running_after_stderr_reader_is_gone() {
    let cache = std::env::temp_dir().join(format!("tessera-broken-stderr-{}", std::process::id()));
    let mut engine = Kill(
        Command::new(env!("CARGO_BIN_EXE_tessera-engine"))
            .args(["--port".as_ref(), "0".as_ref(), "--cache-dir".as_ref(), cache.as_os_str()])
            .env("TESSERA_TOKEN", "t0ken")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(engine.0.stdout.take().unwrap()).read_line(&mut line).unwrap(); // stdout closes too
    let port: u16 = line.trim().strip_prefix("TESSERA_ENGINE_LISTENING ").unwrap().parse().unwrap();
    drop(engine.0.stderr.take()); // every later log write now fails with EPIPE
    tokio::time::sleep(Duration::from_millis(2500)).await; // Stats (logged) every second
    assert!(engine.0.try_wait().unwrap().is_none(), "engine exited");
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?token=t0ken")).await.unwrap();
    let stats = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(text) = msg
                && text.contains(r#""type":"Stats""#)
            {
                return;
            }
        }
    });
    stats.await.expect("no Stats: the event bridge died on a log write");
    drop(engine);
    let _ = std::fs::remove_dir_all(cache);
}
