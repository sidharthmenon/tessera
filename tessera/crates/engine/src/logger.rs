//! `log!`'s writer. Lines go through a bounded queue to a thread that appends them to the log file,
//! which hands them on to a second thread for stderr. So neither a full disk nor a stuck reader of
//! our stderr pipe (the controller owns it) can block the render loop, and the file keeps every
//! line even when stderr is stuck. A full queue drops lines; the count is logged with the next one.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{SystemTime, UNIX_EPOCH};

/// Lines waiting for each thread.
const QUEUE: usize = 1024;
/// The file is moved to `engine.log.1` (replacing it) past this size.
const ROTATE: u64 = 8 << 20;

static TX: OnceLock<SyncSender<String>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// `log!`: never blocks once `init` ran; before that, a direct stderr write.
pub fn write(args: std::fmt::Arguments) {
    match TX.get() {
        Some(tx) => {
            if tx.try_send(args.to_string()).is_err() {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
        None => _ = writeln!(std::io::stderr().lock(), "{args}"),
    }
}

/// From now on `log!` lines go to `<dir>/engine.log` (rotated) and stderr, off the caller's thread.
pub fn init(dir: &Path) {
    let file = std::fs::create_dir_all(dir).map(|()| dir.join("engine.log"));
    if let Err(e) = &file {
        write(format_args!("log: no log file in {}: {e}", dir.display()));
    }
    if let Ok(tx) = spawn(file.ok(), std::io::stderr()) {
        let _ = TX.set(tx);
    }
}

/// The two writer threads; returns the queue into the first.
fn spawn(path: Option<PathBuf>, stderr: impl Write + Send + 'static) -> std::io::Result<SyncSender<String>> {
    let (tx, lines) = sync_channel::<String>(QUEUE);
    let (to_stderr, err_lines) = sync_channel::<String>(QUEUE);
    std::thread::Builder::new().name("log stderr".into()).spawn(move || {
        crate::os::set_priority(crate::os::Priority::Background);
        write_stderr(err_lines, stderr)
    })?;
    std::thread::Builder::new().name("log".into()).spawn(move || {
        crate::os::set_priority(crate::os::Priority::Background);
        write_file(lines, path, to_stderr)
    })?;
    Ok(tx)
}

fn write_file(lines: Receiver<String>, path: Option<PathBuf>, to_stderr: SyncSender<String>) {
    let mut log = path.map(LogFile::open);
    for line in lines {
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        let lines = (dropped > 0).then(|| format!("log: {dropped} lines dropped")).into_iter().chain([line]);
        for line in lines {
            if let Some(log) = &mut log {
                log.append(&line);
            }
            let _ = to_stderr.try_send(line); // full: stderr is stuck, and the file has the line
        }
    }
}

fn write_stderr(lines: Receiver<String>, mut stderr: impl Write) {
    for line in lines {
        let _ = writeln!(stderr, "{line}"); // a broken pipe (controller gone) is fine
    }
}

struct LogFile {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> LogFile {
        let mut log = LogFile { path, file: None, size: 0 };
        log.reopen();
        log
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.size = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
    }

    /// `[HH:MM:SS.mmm UTC] line`; rotates first when the file is full.
    fn append(&mut self, line: &str) {
        if self.size >= ROTATE {
            self.file = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
            self.reopen();
        }
        let Some(file) = &mut self.file else { return };
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let (s, ms) = (t.as_secs() % 86_400, t.subsec_millis());
        let text = format!("[{:02}:{:02}:{:02}.{ms:03}] {line}\n", s / 3600, s / 60 % 60, s % 60);
        if file.write_all(text.as_bytes()).is_ok() {
            self.size += text.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A stderr whose reader never reads: every write blocks forever.
    struct Stuck;
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            loop {
                std::thread::park();
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tessera-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stuck_stderr_never_blocks_and_the_file_gets_the_lines() {
        let dir = dir("stuck");
        let tx = spawn(Some(dir.join("engine.log")), Stuck).unwrap();
        // More lines than both queues hold: the file thread must keep going past the stuck stderr.
        let want = 3 * QUEUE;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut sent = 0;
        while sent < want {
            if tx.try_send(format!("line {sent}")).is_ok() {
                sent += 1;
            } else {
                assert!(Instant::now() < deadline, "the file thread stopped taking lines behind a stuck stderr ({sent} sent)");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let lines = || std::fs::read_to_string(dir.join("engine.log")).unwrap_or_default().lines().count();
        while lines() < want {
            assert!(Instant::now() < deadline, "{} of {want} lines in the file", lines());
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rotates_past_the_limit() {
        let dir = dir("rotate");
        let mut log = LogFile::open(dir.join("engine.log"));
        log.size = ROTATE; // as if full
        log.append("after");
        assert!(dir.join("engine.log.1").exists(), "old file kept as .1");
        let now = std::fs::read_to_string(dir.join("engine.log")).unwrap();
        assert!(now.ends_with("] after\n") && log.size == now.len() as u64, "{now:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
