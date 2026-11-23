//! Live inputs (capture devices; NDI later): one producer thread per input keeps only its newest
//! frame, and every layer showing the input reads that (one device session, a texture per layer).
//! A live input has no timeline: Play shows it live, Pause/Stop freeze it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::compositor::LayerTexture;
use crate::gpu::Gpu;
use crate::player::{GpuFrame, upload_into};
use crate::staging::{Pool, Staged};
use crate::video_decode::Frame;

/// No frame for this long: the input is lost (its layers go transparent, one error is reported)
/// and it is reopened.
const LOST_AFTER: Duration = Duration::from_secs(1);
/// Waits between reopen attempts, growing to the last.
const BACKOFF: [Duration; 3] = [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5)];
/// Staging buffers per input: the newest frame and the few the GPU is still copying from (every
/// layer showing the input uploads from the same one).
const STAGING: usize = 6;

/// Something that delivers frames in real time. Made and used on the input's producer thread only.
pub(crate) trait LiveSource {
    /// The next frame, waiting at most `timeout`; `Ok(None)`: none yet.
    fn next(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>>;
}

/// Opens (or reopens) an input.
pub(crate) type Connect = Box<dyn FnMut() -> anyhow::Result<Box<dyn LiveSource>> + Send>;

#[derive(Default)]
struct Latest {
    /// Bumped by every new frame.
    seq: u64,
    /// Already in a staging buffer (copied on the producer thread).
    frame: Option<Arc<Staged>>,
    lost: bool,
    /// The last problem, numbered so each layer reports it once.
    error: Option<(u64, String)>,
}

/// One input's newest frame, shared by its layers.
pub struct Feed {
    latest: Mutex<Latest>,
    /// The producer thread runs (it ends only when no layer holds the feed, or on a panic).
    running: AtomicBool,
}

impl Feed {
    /// Starts the producer thread for the input `connect` opens.
    pub(crate) fn spawn(gpu: &Gpu, name: &str, connect: Connect) -> anyhow::Result<Arc<Feed>> {
        let feed = Arc::new(Feed { latest: Mutex::new(Latest::default()), running: AtomicBool::new(true) });
        let weak = Arc::downgrade(&feed);
        let pool = Pool::new(gpu, STAGING);
        std::thread::Builder::new().name(format!("live {}", name.replace('\0', ""))).spawn(move || {
            crate::os::set_priority(crate::os::Priority::Decode);
            produce(weak, connect, pool)
        })?;
        Ok(feed)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn latest(&self) -> MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The producer: opens the input, publishes each frame as the newest, and on an error or
/// `LOST_AFTER` without a frame marks the input lost and reopens it after a backoff. Returns once
/// no layer holds the feed.
fn produce(weak: Weak<Feed>, mut connect: Connect, mut pool: Pool) {
    struct Stopped(Weak<Feed>);
    impl Drop for Stopped {
        fn drop(&mut self) {
            if let Some(feed) = self.0.upgrade() {
                feed.running.store(false, Ordering::SeqCst);
            }
        }
    }
    let _stopped = Stopped(weak.clone());
    let (mut attempt, mut quiet, mut errors) = (0, false, 0);
    // Reports `message` once per loss (`quiet` until a frame comes again); false: the feed is gone.
    let mut lose = |message: String, quiet: &mut bool| -> bool {
        let Some(feed) = weak.upgrade() else { return false };
        let mut l = feed.latest();
        (l.lost, l.frame) = (true, None);
        if !std::mem::replace(quiet, true) {
            errors += 1;
            l.error = Some((errors, message));
        }
        true
    };
    loop {
        let failure = match connect() {
            Err(e) => format!("{e:#}"),
            Ok(mut source) => {
                let mut last = Instant::now();
                loop {
                    match source.next(Duration::from_millis(100)) {
                        Ok(Some(frame)) => {
                            let frame = match pool.stage(&frame, false) {
                                Ok(Some(staged)) => staged,
                                Ok(None) => continue, // every buffer still in use: skip it, the next is newer
                                Err(e) => break format!("{e:#}"),
                            };
                            let Some(feed) = weak.upgrade() else { return };
                            let old = {
                                let mut l = feed.latest();
                                (l.seq, l.lost) = (l.seq + 1, false);
                                l.frame.replace(Arc::new(frame))
                            };
                            drop(old); // freed here, not on the render thread
                            (attempt, quiet, last) = (0, false, Instant::now());
                        }
                        Ok(None) if last.elapsed() < LOST_AFTER => {
                            if weak.strong_count() == 0 {
                                return;
                            }
                        }
                        Ok(None) => break format!("no picture for {} s", LOST_AFTER.as_secs()),
                        Err(e) => break format!("{e:#}"),
                    }
                }
            }
        };
        if !lose(format!("{failure}; reconnecting"), &mut quiet) || !wait(&weak, BACKOFF[attempt.min(BACKOFF.len() - 1)]) {
            return;
        }
        attempt += 1;
    }
}

/// Sleeps `d`, returning false early once no layer holds the feed.
fn wait(weak: &Weak<Feed>, d: Duration) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if weak.strong_count() == 0 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    weak.strong_count() > 0
}

/// A layer showing a live input.
pub struct LiveMedia {
    feed: Arc<Feed>,
    seen: u64,
    seen_error: u64,
    frozen: bool,
    lost: bool,
    due: Option<Arc<Staged>>,
    gpu: Option<GpuFrame>,
    error: Option<String>,
    failed: bool,
}

impl LiveMedia {
    /// Live from the start, so Preview shows the input before the Take.
    pub fn new(feed: Arc<Feed>) -> LiveMedia {
        LiveMedia { feed, seen: 0, seen_error: 0, frozen: false, lost: false, due: None, gpu: None, error: None, failed: false }
    }

    /// Takes the newest frame (unless frozen) and the input's state.
    pub fn poll(&mut self) {
        let l = self.feed.latest();
        self.lost = l.lost;
        if let Some((n, message)) = &l.error
            && *n != self.seen_error
        {
            (self.seen_error, self.error) = (*n, Some(message.clone()));
        }
        if !self.frozen && l.seq != self.seen {
            (self.seen, self.due) = (l.seq, l.frame.clone());
        }
        drop(l);
        self.failed |= !self.feed.running();
    }

    /// The frame for the last `poll`, uploaded if new; `None` while lost, before the first frame,
    /// or failed.
    pub fn texture(&mut self, gpu: &Gpu) -> Option<LayerTexture> {
        if self.failed || self.lost {
            return None;
        }
        if let Some(frame) = self.due.take() {
            if let Err(e) = upload_into(&mut self.gpu, gpu, &frame) {
                (self.failed, self.error) = (true, Some(format!("{e:#}")));
                return None;
            }
            gpu.retire(frame); // its buffer goes back once every layer's copy is done
        }
        self.gpu.as_ref().map(|g| g.layer.clone())
    }

    pub fn set_live(&mut self, live: bool) {
        self.frozen = !live;
    }

    pub fn ready(&self) -> bool {
        self.gpu.is_some() || self.due.is_some() || self.failed
    }

    /// The producer thread is gone (a panic): open a new feed.
    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Frames from a channel; a closed channel is an error (an unplugged device).
    struct Fake(mpsc::Receiver<Frame>);
    impl LiveSource for Fake {
        fn next(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>> {
            match self.0.recv_timeout(timeout) {
                Ok(f) => Ok(Some(f)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("unplugged"),
            }
        }
    }

    fn feed() -> (Arc<Feed>, mpsc::Sender<Frame>) {
        let (tx, rx) = mpsc::channel();
        let mut rx = Some(rx);
        let connect: Connect = Box::new(move || Ok(Box::new(Fake(rx.take().ok_or_else(|| anyhow::anyhow!("gone"))?)) as Box<dyn LiveSource>));
        (Feed::spawn(&Gpu::new_headless().expect("these tests need a GPU"), "test", connect).unwrap(), tx)
    }

    /// Waits for the producer to publish `seq`.
    fn until_seq(feed: &Feed, seq: u64) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while feed.latest().seq < seq {
            assert!(Instant::now() < deadline, "frame {seq} never published");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn width(m: &LiveMedia) -> Option<u32> {
        m.due.as_ref().map(|f| f.size().0)
    }

    #[test]
    fn layers_take_only_the_newest_frame() {
        let (feed, tx) = feed();
        let (mut a, mut b) = (LiveMedia::new(feed.clone()), LiveMedia::new(feed.clone()));
        for w in [1, 2, 3] {
            tx.send(Frame::test_rgba(w, 1, [255; 4])).unwrap();
        }
        until_seq(&feed, 3);
        a.poll();
        b.poll();
        assert_eq!((width(&a), width(&b)), (Some(3), Some(3)), "both layers get the third, newest frame");
        assert!(a.ready());
        // Frozen (Pause/Stop): newer frames are not taken.
        a.due = None;
        a.set_live(false);
        tx.send(Frame::test_rgba(4, 1, [255; 4])).unwrap();
        until_seq(&feed, 4);
        a.poll();
        assert_eq!(width(&a), None, "frozen");
        a.set_live(true);
        a.poll();
        assert_eq!(width(&a), Some(4), "live again: the newest");
    }

    #[test]
    fn an_unplugged_input_is_lost_and_reported_once() {
        let (feed, tx) = feed();
        let mut m = LiveMedia::new(feed.clone());
        tx.send(Frame::test_rgba(2, 2, [255; 4])).unwrap();
        until_seq(&feed, 1);
        drop(tx); // unplugged; the reconnect then fails ("gone") and backs off
        let deadline = Instant::now() + Duration::from_secs(2);
        while !feed.latest().lost {
            assert!(Instant::now() < deadline, "never marked lost");
            std::thread::sleep(Duration::from_millis(5));
        }
        m.poll();
        assert!(m.lost && !m.failed(), "lost, not failed: it keeps trying");
        let error = m.take_error().expect("reported");
        assert!(error.contains("unplugged") && error.contains("reconnecting"), "{error}");
        m.poll();
        assert_eq!(m.take_error(), None, "once per loss");
    }

    /// When the last layer goes, the producer ends and closes the input (here: drops the receiver).
    #[test]
    fn the_producer_ends_with_its_last_layer() {
        let (feed, tx) = feed();
        drop(LiveMedia::new(feed));
        let deadline = Instant::now() + Duration::from_secs(2);
        while tx.send(Frame::test_rgba(1, 1, [0; 4])).is_ok() {
            assert!(Instant::now() < deadline, "the input stayed open with no layer");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
