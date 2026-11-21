//! Audio: each player's decode thread resamples its audio stream into an `AudioTap`; the cpal
//! output callback mixes the taps × layer volume × master volume.
//!
//! ponytail: no audio-clock slaving. Video runs on the frame clock and audio on the device clock,
//! so they drift by the clocks' ~ppm difference; add resample-based sync if long clips drift. A tap
//! is a plain FIFO that starts and stops with its video, so sync also assumes the audio is
//! contiguous from the video's t = 0. A stall longer than the decode lead (~4 video frames) leaves
//! audio late by the excess until the next stop or seek. A loop whose audio is longer or shorter
//! than its video drifts by the difference on each pass (the fixture's matches exactly). Upgrade:
//! place samples by pts on the player's timeline and read them by wall-clock position.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use anyhow::{Context, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ff::ffi;
use ffmpeg_next as ff;

/// One player's decoded audio: interleaved f32 at the mixer's rate and channel count, about 1 s
/// deep. Clones share it. The mixer consumes it only while its player plays.
#[derive(Clone)]
pub struct AudioTap {
    ring: Arc<Mutex<Ring>>,
    /// (sample rate, channels) the decode thread resamples to.
    format: (u32, u16),
}

struct Ring {
    samples: VecDeque<f32>,
    capacity: usize,
    /// The player's seek epoch: samples decoded before its last stop/seek are dropped.
    epoch: u64,
    playing: bool,
    /// Samples pushed while set are dropped (a clip at a speed other than 1 is silent).
    silent: bool,
    /// Why the audio failed, reported once through `Player::take_error`.
    error: Option<String>,
}

impl AudioTap {
    pub(crate) fn new(format: (u32, u16)) -> AudioTap {
        let capacity = format.0 as usize * format.1 as usize;
        let ring = Ring { samples: VecDeque::with_capacity(capacity), capacity, epoch: 0, playing: false, silent: false, error: None };
        AudioTap { ring: Arc::new(mutex(ring)), format }
    }

    /// Decode thread. When full, the oldest samples go: the decoder never waits for the mixer.
    pub(crate) fn push(&self, epoch: u64, samples: &[f32]) {
        let mut ring = lock(&self.ring);
        if ring.epoch != epoch || ring.silent {
            return;
        }
        let keep = &samples[samples.len().saturating_sub(ring.capacity)..];
        let excess = (ring.samples.len() + keep.len()).saturating_sub(ring.capacity);
        ring.samples.drain(..excess);
        ring.samples.extend(keep);
    }

    /// Player stop/restart/seek: empty and paused; from now on only `epoch`'s samples go in.
    pub(crate) fn reset(&self, epoch: u64) {
        let mut ring = lock(&self.ring);
        ring.samples.clear();
        (ring.epoch, ring.playing) = (epoch, false);
    }

    pub(crate) fn set_playing(&self, playing: bool) {
        lock(&self.ring).playing = playing;
    }

    /// While silent nothing is kept (not even samples already in): a clip off 1× speed. Dropping
    /// rather than muting keeps the tap from filling and coming back a second late.
    // ponytail: silent off 1×; varispeed through swr (output rate = rate / speed) later.
    pub(crate) fn set_silent(&self, silent: bool) {
        let mut ring = lock(&self.ring);
        if silent {
            ring.samples.clear();
        }
        ring.silent = silent;
    }

    /// Keeps the first error until it is taken.
    fn fail(&self, error: String) {
        lock(&self.ring).error.get_or_insert(error);
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        lock(&self.ring).error.take()
    }

    /// Adds the next `out.len()` samples × `volume` into `out` and consumes them (the mixer
    /// callback: no allocation, no blocking). Nothing while paused, or if the lock stays contended
    /// (this buffer is then silent for this tap).
    pub fn mix(&self, out: &mut [f32], volume: f32) {
        let Some(mut ring) = try_lock(&self.ring) else { return };
        if !ring.playing {
            return;
        }
        let n = out.len().min(ring.samples.len());
        let (a, b) = ring.samples.as_slices();
        add(out, a, volume);
        if let Some(rest) = out.get_mut(a.len()..) {
            add(rest, b, volume);
        }
        ring.samples.drain(..n);
    }

    /// Samples waiting to be mixed.
    pub fn buffered(&self) -> usize {
        lock(&self.ring).samples.len()
    }
}

/// Sums each tap × its volume into `out` (overwritten), then applies `master` and clamps to ±1.
/// The output callback does the same, one tap at a time.
pub fn mix_into(out: &mut [f32], taps: &[(&[f32], f32)], master: f32) {
    out.fill(0.);
    for &(samples, volume) in taps {
        add(out, samples, volume);
    }
    finish(out, master);
}

fn add(out: &mut [f32], samples: &[f32], volume: f32) {
    for (o, s) in out.iter_mut().zip(samples) {
        *o += s * volume;
    }
}

fn finish(out: &mut [f32], master: f32) {
    for o in out {
        let v = *o * master;
        *o = if v.is_nan() { 0. } else { v.clamp(-1., 1.) }; // a broken stream's NaN stays here
    }
}

/// cpal's default output device, mixing the current taps.
pub struct Mixer {
    shared: Arc<Shared>,
    format: (u32, u16),
    _stream: cpal::Stream,
}

impl Mixer {
    /// The default output device at its default rate and channel count. `Err` (no device, or it
    /// won't open): the engine runs silent.
    pub fn start() -> anyhow::Result<Mixer> {
        let device = cpal::default_host().default_output_device().context("no audio output device")?;
        let config = device.default_output_config().context("no output config")?;
        // ponytail: f32 only (CoreAudio and WASAPI shared mode mix in f32); convert in the
        // callback if a device ever wants integer samples.
        let sample_format = config.sample_format();
        ensure!(sample_format == cpal::SampleFormat::F32, "output wants {sample_format} samples; only f32 is supported");
        let shared = Arc::new(Shared::new());
        let (data, errors) = (shared.clone(), shared.clone());
        let stream = device
            .build_output_stream(
                config.config(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| data.render(out),
                move |e| errors.stream_error(e),
                None,
            )
            .context("cannot open the audio output")?;
        stream.play().context("cannot start the audio output")?;
        Ok(Mixer { shared, format: (config.sample_rate(), config.channels()), _stream: stream })
    }

    /// (sample rate, channels): what players resample to (`Player::open`).
    pub fn format(&self) -> (u32, u16) {
        self.format
    }

    /// Replaces the mixed taps; volumes clamp to 0..1 (non-finite: 0). Refilled in place (no
    /// allocation once the list has grown) under a lock the callback only try-locks; replaced taps
    /// are dropped here, never in the callback.
    pub fn set_taps(&self, taps: impl IntoIterator<Item = (AudioTap, f32)>) {
        let mut current = lock(&self.shared.taps);
        current.clear();
        current.extend(taps.into_iter().map(|(tap, volume)| (tap, unit(volume).unwrap_or(0.))));
    }

    /// Clamped to 0..1; a non-finite volume is ignored.
    pub fn set_master(&self, volume: f32) {
        match unit(volume) {
            Some(v) => self.shared.master.store(v.to_bits(), Relaxed),
            None => log!("master volume {volume}: not finite, ignored"),
        }
    }

    /// The first stream error that stopped the sound (device gone…), once.
    pub fn take_error(&self) -> Option<String> {
        lock(&self.shared.error).take()
    }
}

struct Shared {
    taps: Mutex<Vec<(AudioTap, f32)>>,
    /// f32 bits.
    master: AtomicU32,
    error: Mutex<Option<String>>,
    failed: AtomicBool,
}

impl Shared {
    fn new() -> Shared {
        Shared { taps: mutex(vec![]), master: AtomicU32::new(1f32.to_bits()), error: Mutex::new(None), failed: AtomicBool::new(false) }
    }

    /// cpal's data callback, on the real-time audio thread: no allocation, no blocking.
    fn render(&self, out: &mut [f32]) {
        out.fill(0.);
        if let Some(taps) = try_lock(&self.taps) {
            for (tap, volume) in taps.iter() {
                tap.mix(out, *volume);
            }
        }
        finish(out, f32::from_bits(self.master.load(Relaxed)));
    }

    fn stream_error(&self, e: cpal::Error) {
        log!("audio output: {e}");
        use cpal::ErrorKind::{DeviceChanged, RealtimeDenied, Xrun};
        let fatal = !matches!(e.kind(), DeviceChanged | RealtimeDenied | Xrun);
        if fatal && !self.failed.swap(true, Relaxed) {
            *lock(&self.error) = Some(format!("audio output: {e}"));
        }
    }
}

fn unit(v: f32) -> Option<f32> {
    v.is_finite().then(|| v.clamp(0., 1.))
}

/// `Mutex::new` plus a first lock: on macOS that lock allocates the pthread mutex, which must
/// happen here rather than in the real-time callback.
fn mutex<T>(value: T) -> Mutex<T> {
    let m = Mutex::new(value);
    drop(lock(&m));
    m
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `try_lock`, retried briefly: the decode thread holds a tap for well under a microsecond per
/// push, so a few retries ride that out instead of dropping a buffer to silence. Bounded, so a
/// preempted holder still can't stall the real-time thread.
fn try_lock<T>(m: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    for _ in 0..100 {
        match m.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
        }
    }
    None
}

/// A player's audio stream, decoded on its decode thread from the packets the video decoder reads,
/// resampled (swresample) into its tap.
pub(crate) struct AudioDecoder {
    decoder: ff::decoder::Audio,
    stream: usize,
    /// Seconds per stream tick, and the stream's start.
    time_base: f64,
    start: i64,
    /// A trimmed clip's `[in, out)` in the file: samples outside it are dropped.
    pub(crate) window: Option<(std::time::Duration, std::time::Duration)>,
    swr: Option<Swr>,
    tap: AudioTap,
    /// Stamped on pushed samples so the tap drops audio decoded before a seek.
    epoch: u64,
    /// For error messages: the file.
    name: String,
    reported: bool,
}

impl AudioDecoder {
    /// The best audio stream of `input`. `None` if there is none, or it can't be decoded: that is
    /// reported on the tap and the video plays on.
    pub fn open(input: &ff::format::context::Input, tap: AudioTap, path: &Path) -> Option<AudioDecoder> {
        let stream = input.streams().best(ff::media::Type::Audio)?;
        let name = path.display().to_string();
        match ff::codec::Context::from_parameters(stream.parameters()).and_then(|c| c.decoder().audio()) {
            Ok(decoder) => {
                let start = if stream.start_time() == ffi::AV_NOPTS_VALUE { 0 } else { stream.start_time() };
                let time_base = f64::from(stream.time_base());
                Some(AudioDecoder { decoder, stream: stream.index(), time_base, start, window: None, swr: None, tap, epoch: 0, name, reported: false })
            }
            Err(e) => {
                tap.fail(format!("{name}: audio: unsupported codec: {e}"));
                None
            }
        }
    }

    /// Decodes `packet` if it belongs to this stream.
    pub fn packet(&mut self, packet: &ff::Packet) {
        if packet.stream() == self.stream {
            let result = self.decoder.send_packet(packet).map_err(Into::into).and_then(|()| self.drain());
            self.report(result);
        }
    }

    /// End of file: the decoder's last frames.
    pub fn eof(&mut self) {
        let result = self.decoder.send_eof().map_err(Into::into).and_then(|()| self.drain());
        self.report(result);
    }

    /// Back to the start (loop wrap or seek). A loop's next pass follows on without a gap.
    pub fn rewind(&mut self) {
        self.decoder.flush();
    }

    /// After a player seek: from now on samples belong to `epoch`; the resampler's leftovers go.
    pub fn seek(&mut self, epoch: u64) {
        (self.epoch, self.swr) = (epoch, None);
    }

    fn drain(&mut self) -> anyhow::Result<()> {
        let mut frame = ff::frame::Audio::empty();
        loop {
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => self.resample(&frame)?,
                Err(ff::Error::Eof | ff::Error::Other { errno: ff::util::error::EAGAIN }) => return Ok(()),
                Err(e) => return Err(e).context("decode"),
            }
        }
    }

    /// Converts a decoded frame to the tap's interleaved f32 format and pushes it.
    fn resample(&mut self, frame: &ff::frame::Audio) -> anyhow::Result<()> {
        let (rate, channels) = self.tap.format;
        let swr = match &self.swr {
            Some(swr) => swr.0,
            None => self.swr.insert(Swr::new()?).0,
        };
        let mut out = ff::frame::Audio::empty();
        // SAFETY: `out` is an empty frame described (packed f32, the tap's rate and layout) for
        // swr_convert_frame to allocate; `frame` is a decoded frame; `swr` is live. The pushed
        // slice is `out`'s `nb_samples × channels` floats, read while `out` lives.
        unsafe {
            let o = out.as_mut_ptr();
            (*o).format = ffi::AVSampleFormat::AV_SAMPLE_FMT_FLT as i32;
            (*o).sample_rate = rate as i32;
            ffi::av_channel_layout_default(&mut (*o).ch_layout, channels as i32);
            // Unconfigured, swr_convert_frame configures itself from the two frames.
            let mut r = ffi::swr_convert_frame(swr, o, frame.as_ptr());
            if r == ffi::AVERROR_INPUT_CHANGED {
                ffi::swr_close(swr); // the stream's format changed: reconfigure from this frame
                r = ffi::swr_convert_frame(swr, o, frame.as_ptr());
            }
            ensure!(r >= 0, "resample: {}", ff::Error::from(r));
            let frames = (*o).nb_samples.max(0) as usize;
            let samples = std::slice::from_raw_parts((*o).data[0] as *const f32, frames * channels as usize);
            let (a, b) = self.in_window(frame, frames, rate);
            if a < b {
                self.tap.push(self.epoch, &samples[a * channels as usize..b * channels as usize]);
            }
        }
        Ok(())
    }

    /// Which of a converted frame's `frames` output frames lie inside the trim window (all of them
    /// untrimmed, or when the frame has no timestamp). ponytail: ignores the resampler's few-ms delay.
    fn in_window(&self, frame: &ff::frame::Audio, frames: usize, rate: u32) -> (usize, usize) {
        let (Some((start, end)), Some(pts)) = (self.window, frame.pts()) else { return (0, frames) };
        let t = (pts - self.start) as f64 * self.time_base;
        let at = |edge: std::time::Duration| ((edge.as_secs_f64() - t) * rate as f64).round().clamp(0., frames as f64) as usize;
        (at(start), at(end))
    }

    /// Reports the first error only. Later packets are still decoded, so one corrupt packet
    /// doesn't silence the rest of the clip.
    fn report(&mut self, result: anyhow::Result<()>) {
        if let Err(e) = result
            && !std::mem::replace(&mut self.reported, true)
        {
            self.tap.fail(format!("{}: audio: {e:#}", self.name));
        }
    }
}

struct Swr(*mut ffi::SwrContext);

impl Swr {
    fn new() -> anyhow::Result<Swr> {
        // SAFETY: plain allocation; swr_convert_frame configures it.
        let ctx = unsafe { ffi::swr_alloc() };
        ensure!(!ctx.is_null(), "no resampler");
        Ok(Swr(ctx))
    }
}

impl Drop for Swr {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from swr_alloc and is freed once.
        unsafe { ffi::swr_free(&mut self.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_sums_and_clamps() {
        let mut out = [9.; 4]; // overwritten, not added to
        mix_into(&mut out, &[(&[0.8; 4], 1.), (&[0.8; 4], 1.)], 1.);
        assert_eq!(out, [1.; 4], "0.8 + 0.8 clamps to 1");
        mix_into(&mut out, &[(&[-0.8; 4], 1.), (&[-0.8; 4], 1.)], 1.);
        assert_eq!(out, [-1.; 4]);
        mix_into(&mut out, &[(&[0.8, -0.8, 0.4, 0.], 0.5)], 1.);
        assert_eq!(out, [0.4, -0.4, 0.2, 0.], "volume 0.5 halves");
        mix_into(&mut out, &[(&[0.8; 4], 1.)], 0.5);
        assert_eq!(out, [0.4; 4], "master 0.5 halves");
        mix_into(&mut out, &[(&[f32::NAN, 0.5], 1.)], 1.);
        assert_eq!(out, [0., 0.5, 0., 0.], "NaN never reaches the device; a short tap ends in silence");
    }

    /// 4 Hz mono: the tap holds 4 samples (1 s).
    fn tap() -> AudioTap {
        AudioTap::new((4, 1))
    }

    fn mixed(tap: &AudioTap, n: usize, volume: f32) -> Vec<f32> {
        let mut out = vec![0.; n];
        tap.mix(&mut out, volume);
        out
    }

    #[test]
    fn tap_plays_only_while_playing() {
        let tap = tap();
        tap.push(0, &[0.5, 0.25]);
        assert_eq!(mixed(&tap, 2, 1.), [0., 0.], "paused: silent");
        assert_eq!(tap.buffered(), 2, "paused: nothing consumed");
        tap.set_playing(true);
        assert_eq!(mixed(&tap, 3, 0.5), [0.25, 0.125, 0.], "× volume, then silence once drained");
        assert_eq!(tap.buffered(), 0);
    }

    #[test]
    fn reset_drops_old_epoch() {
        let tap = tap();
        tap.set_playing(true);
        tap.push(0, &[0.5; 2]);
        tap.reset(1);
        assert_eq!(tap.buffered(), 0);
        tap.push(0, &[0.5; 2]); // decoded before the decode thread took the seek
        assert_eq!(tap.buffered(), 0, "stale epoch dropped");
        tap.push(1, &[0.25; 2]);
        assert_eq!(mixed(&tap, 2, 1.), [0., 0.], "reset pauses");
        tap.set_playing(true);
        assert_eq!(mixed(&tap, 2, 1.), [0.25; 2]);
    }

    #[test]
    fn full_tap_drops_oldest() {
        let tap = tap();
        tap.set_playing(true);
        tap.push(0, &[1., 2., 3.]);
        tap.push(0, &[4., 5.]);
        assert_eq!(mixed(&tap, 4, 1.), [2., 3., 4., 5.]);
        tap.push(0, &[1., 2., 3., 4., 5., 6.]); // more than the tap holds: its newest second
        assert_eq!(mixed(&tap, 4, 1.), [3., 4., 5., 6.]);
    }

    #[test]
    fn callback_mixes_taps_with_master() {
        let shared = Shared::new();
        let (a, b) = (tap(), tap());
        for tap in [&a, &b] {
            tap.set_playing(true);
            tap.push(0, &[0.5; 2]);
        }
        *lock(&shared.taps) = vec![(a, 1.), (b, 0.5)];
        shared.master.store(0.5f32.to_bits(), Relaxed);
        let mut out = [9.; 3];
        shared.render(&mut out);
        assert_eq!(out, [0.375, 0.375, 0.], "(0.5 + 0.25) × 0.5, then silence");
    }
}
