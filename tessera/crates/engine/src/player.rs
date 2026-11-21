//! Video players (decode thread → small frame queue → GPU planes) and still images.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Context;
use tessera_show::Playback;
use wgpu::TextureFormat as F;
use wgpu::util::StagingBelt;

use crate::audio::AudioTap;
use crate::compositor::{LayerTexture, Planes};
use crate::gpu::Gpu;
use crate::video_decode::{Decoder, Frame, PixelFormatKind};

/// Frames decoded ahead of the clock; also the loop pre-roll.
const QUEUE: usize = 4;
/// `copy_buffer_to_texture` needs 256-byte rows and offsets.
const ALIGN: u64 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;

enum Msg {
    /// The file opened; always the first message on success.
    Opened(Duration),
    /// `t` is on the player's timeline: Loop adds one duration per pass.
    Frame { epoch: u64, t: Duration, frame: Frame },
    /// No more frames until the next seek (Once, HoldLast).
    End { epoch: u64 },
    /// The decode thread stopped.
    Error(String),
    /// A decode or read error the clip plays on past (taken as its end): reported, not fatal.
    Warning(String),
}

#[derive(Clone, Copy)]
enum State {
    Paused(Duration),
    /// At `at` the timeline was at `from`; it advances by `speed` × wall time since.
    Playing { at: Instant, from: Duration },
}

/// One playing video file. Starts paused on its first frame. All timing comes in as `now`
/// (the render loop's frame clock), so the player never reads the wall clock itself.
pub struct Player {
    playback: Playback,
    /// Where the trimmed part starts in the file (position reports are in the file).
    trim_in: Duration,
    /// Timeline rate (0.25–4).
    speed: f32,
    /// What plays (the trimmed part), known once the decode thread has opened the file.
    duration: Option<Duration>,
    frames: Receiver<Msg>,
    seek: Sender<u64>,
    /// Bumped by every seek; frames decoded before it are discarded.
    epoch: u64,
    state: State,
    /// Next frame, not due yet.
    pending: Option<(Duration, Frame)>,
    /// Newest due frame, not uploaded yet (`texture` uploads it).
    due: Option<Frame>,
    ended: bool,
    /// Once, past its end: transparent.
    over: bool,
    failed: bool,
    /// Stopped: the uploaded texture is the old position's, so nothing shows until the new
    /// epoch's first frame is uploaded.
    stale: bool,
    /// `texture` ran since the last `poll`: the layer is on screen, so skipped frames count as dropped.
    shown: bool,
    gpu: Option<GpuFrame>,
    error: Option<String>,
    dropped: u64,
    audio: Option<AudioTap>,
}

impl Player {
    /// Returns at once (R22: opening must never stall the render loop); a new decode thread opens
    /// the file and decodes ahead. Until the first frame is ready `current_frame` is `None`; a
    /// missing file, unsupported codec or decode error shows up in `take_error`. `Err` only if
    /// the thread can't be spawned. `audio`: the mixer's (sample rate, channels), which the decode
    /// thread resamples the file's audio to (see `audio()`); `None` skips audio.
    pub fn open(path: &Path, playback: Playback, audio: Option<(u32, u16)>) -> anyhow::Result<Player> {
        Player::open_trimmed(path, playback, audio, 0, None)
    }

    /// `open`, playing only `[trim_in_ms, trim_out_ms)` of the file (see `Decoder::trimmed`):
    /// Loop, Once and HoldLast use that part; positions are still reported in the file.
    pub fn open_trimmed(path: &Path, playback: Playback, audio: Option<(u32, u16)>, trim_in_ms: u64, trim_out_ms: Option<u64>) -> anyhow::Result<Player> {
        let (tx, frames) = mpsc::sync_channel(QUEUE);
        let (seek, seeks) = mpsc::channel();
        let (owned, looping) = (path.to_path_buf(), playback == Playback::Loop);
        let audio = audio.map(AudioTap::new);
        let tap = audio.clone();
        std::thread::Builder::new().name(thread_name("decode", path)).spawn(move || {
            let result = Decoder::open(&owned, tap).and_then(|d| d.trimmed(trim_in_ms, trim_out_ms)).and_then(|d| {
                let _ = tx.send(Msg::Opened(d.length));
                decode(d, looping, &tx, &seeks, &owned)
            });
            if let Err(e) = result {
                let _ = tx.send(Msg::Error(format!("{}: {e:#}", owned.display())));
            }
        })?;
        let state = State::Paused(Duration::ZERO);
        Ok(Player {
            playback,
            trim_in: Duration::from_millis(trim_in_ms),
            speed: 1.,
            duration: None,
            frames,
            seek,
            epoch: 0,
            state,
            pending: None,
            due: None,
            ended: false,
            over: false,
            failed: false,
            stale: false,
            shown: false,
            gpu: None,
            error: None,
            dropped: 0,
            audio,
        })
    }

    pub fn play(&mut self, at: Instant) {
        if let State::Paused(from) = self.state {
            self.state = State::Playing { at, from };
        }
        if let Some(tap) = &self.audio {
            tap.set_playing(true);
        }
    }

    pub fn pause(&mut self, now: Instant) {
        self.state = State::Paused(self.elapsed(now));
        if let Some(tap) = &self.audio {
            tap.set_playing(false);
        }
    }

    /// Back to the first frame, paused. Shows nothing until that frame is decoded again (never the
    /// old position's frame).
    pub fn stop(&mut self) {
        self.epoch += 1;
        if let Some(tap) = &self.audio {
            tap.reset(self.epoch); // before the seek: the decode thread pushes the new epoch's audio only after
        }
        let _ = self.seek.send(self.epoch);
        (self.state, self.pending, self.due, self.ended, self.stale) = (State::Paused(Duration::ZERO), None, None, false, true);
    }

    /// Changes the rate live: the position carries on from where it is at `now`. Audio is silent
    /// unless the speed is 1.
    pub fn set_speed(&mut self, speed: f32, now: Instant) {
        let speed = if speed.is_finite() { speed.clamp(0.25, 4.) } else { 1. };
        if speed == self.speed {
            return;
        }
        if let State::Playing { .. } = self.state {
            self.state = State::Playing { at: now, from: self.elapsed(now) };
        }
        self.speed = speed;
        if let Some(tap) = &self.audio {
            tap.set_silent(speed != 1.);
        }
    }

    pub fn restart(&mut self, at: Instant) {
        self.stop();
        self.play(at);
    }

    pub fn position_ms(&self, now: Instant) -> u64 {
        let t = self.elapsed(now);
        let position = match (&self.playback, self.duration) {
            (_, None) => t,
            (Playback::Loop, Some(d)) => Duration::from_nanos((t.as_nanos() % d.as_nanos().max(1)) as u64),
            (Playback::Once | Playback::HoldLast, Some(d)) => t.min(d),
        };
        (self.trim_in + position).as_millis() as u64
    }

    /// Its first frame is decoded (or it failed): waiting longer won't change how it starts.
    pub fn ready(&self) -> bool {
        self.showing() || self.due.is_some() || self.failed
    }

    /// `poll` then `texture`.
    pub fn current_frame(&mut self, gpu: &Gpu, now: Instant, period: Duration) -> Option<LayerTexture> {
        self.poll(now, period);
        self.texture(gpu)
    }

    /// Advances to `now`: collects the decode thread's messages and keeps the newest frame due
    /// (pts ≤ position + `period`/2, so frame boundaries sit half a vsync away from the ticks: R19)
    /// for `texture`. Call it every loop iteration, drawn or not, so errors and readiness surface
    /// and the queue keeps moving; frames that come due while the layer isn't drawn are skipped
    /// without an upload.
    pub fn poll(&mut self, now: Instant, period: Duration) {
        let shown = std::mem::take(&mut self.shown);
        let t = self.elapsed(now);
        self.over = self.playback == Playback::Once && self.duration.is_some_and(|d| t >= d);
        if self.failed || self.over {
            return;
        }
        let due = t + period.mul_f32(self.speed) / 2;
        let mut taken = 0;
        loop {
            let msg = match self.pending.take() {
                Some((t, frame)) => Msg::Frame { epoch: self.epoch, t, frame },
                None => match self.frames.try_recv() {
                    Ok(msg) => msg,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => Msg::Error("decode thread died".into()),
                },
            };
            match msg {
                Msg::Opened(duration) => self.duration = Some(duration),
                Msg::Frame { epoch, .. } if epoch != self.epoch => {}
                Msg::Frame { t, frame, .. } if t > due => {
                    self.pending = Some((t, frame));
                    break;
                }
                Msg::Frame { frame, .. } => (self.due, taken) = (Some(frame), taken + 1),
                Msg::End { epoch } => self.ended |= epoch == self.epoch,
                Msg::Error(e) => return self.fail(e),
                Msg::Warning(e) => self.error = Some(e),
            }
        }
        if !shown {
            return;
        }
        // Above 1× some frames are skipped on purpose, below 1× some repeat: neither counts.
        let skipped = self.speed.ceil() as u64;
        if taken > skipped {
            self.dropped += taken - skipped; // the ones before the newest are never shown
        } else if taken == 0
            && self.speed >= 1.
            && matches!(self.state, State::Playing { .. })
            && self.showing()
            && self.pending.is_none()
            && !self.ended
        {
            self.dropped += 1; // the decoder is late: the frame repeats
        }
    }

    /// The frame for the last `poll`, uploaded into this player's reused textures if new. `None`:
    /// nothing to show (first frame not decoded yet, stopped and its first frame not back yet,
    /// Once after its end, or the player failed; see `take_error`).
    pub fn texture(&mut self, gpu: &Gpu) -> Option<LayerTexture> {
        self.shown = true;
        if self.failed || self.over {
            return None;
        }
        if let Some(frame) = self.due.take() {
            if let Err(e) = self.upload(gpu, &frame) {
                self.fail(format!("{e:#}"));
                return None;
            }
            self.stale = false;
        }
        self.gpu.as_ref().filter(|_| !self.stale).map(|g| g.layer.clone())
    }

    fn showing(&self) -> bool {
        self.gpu.is_some() && !self.stale
    }

    /// It will never show anything again (open, first-frame or upload error): open a new one.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// The error that made this player go transparent, or why its audio failed (the video plays
    /// on), once.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take().or_else(|| self.audio.as_ref()?.take_error())
    }

    /// This player's audio for the mixer (`None` if opened without a mixer format). It is consumed
    /// only while the player plays; `stop`/`restart` empty it.
    pub fn audio(&self) -> Option<AudioTap> {
        self.audio.clone()
    }

    /// Frames skipped or repeated since the last call because the decoder (or the loop) was late.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    fn elapsed(&self, now: Instant) -> Duration {
        match self.state {
            State::Paused(t) => t,
            State::Playing { at, from } => from + now.saturating_duration_since(at).mul_f32(self.speed),
        }
    }

    fn fail(&mut self, error: String) {
        (self.failed, self.error, self.gpu, self.due) = (true, Some(error), None, None);
    }

    fn upload(&mut self, gpu: &Gpu, frame: &Frame) -> anyhow::Result<()> {
        upload_into(&mut self.gpu, gpu, frame).map(drop)
    }
}

/// Uploads `frame` into `slot`'s textures (made, or remade when the format or size changes) and
/// returns them as a layer texture.
pub(crate) fn upload_into(slot: &mut Option<GpuFrame>, gpu: &Gpu, frame: &Frame) -> anyhow::Result<LayerTexture> {
    let (w, h) = frame.size();
    check_size(gpu, w, h)?;
    if slot.as_ref().is_none_or(|g| g.key != (frame.kind, w, h)) {
        *slot = Some(GpuFrame::new(gpu, frame.kind, w, h));
    }
    let g = slot.as_mut().context("no textures")?;
    g.layer.full_range = frame.full_range;
    g.upload(gpu, frame)?;
    Ok(g.layer.clone())
}

/// Decode thread: frames in order into `tx` (blocking when the queue is full); Loop rewinds at
/// the end without a gap, Once/HoldLast report `End` and wait for a seek. A decode or read error
/// after this pass has shown frames is the pass's end (a corrupt tail, a bad packet: I1), reported
/// once as a `Warning`; one before any frame of the pass is fatal (nothing would ever show, and a
/// Loop would spin rewinding).
fn decode(mut d: Decoder, looping: bool, tx: &SyncSender<Msg>, seeks: &Receiver<u64>, path: &Path) -> anyhow::Result<()> {
    let (mut epoch, mut offset, mut sent, mut warned) = (0, Duration::ZERO, false, false);
    // pts of the last frame sent this pass.
    let mut last: Option<Duration> = None;
    loop {
        if let Some(e) = seeks.try_iter().last() {
            (epoch, offset, last) = (e, Duration::ZERO, None);
            d.seek(e)?;
        }
        let (next, cut) = match d.next() {
            Ok(next) => (next, false),
            Err(e) if last.is_some() => {
                if !std::mem::replace(&mut warned, true) {
                    let _ = tx.send(Msg::Warning(format!("{}: {e:#}; playing on from the start", path.display())));
                }
                (None, true)
            }
            Err(e) => return Err(e),
        };
        match next {
            Some(frame) => {
                last = Some(frame.pts);
                if tx.send(Msg::Frame { epoch, t: offset + frame.pts, frame }).is_err() {
                    return Ok(()); // player dropped
                }
                sent = true;
            }
            // Would loop forever without output (and never show anything).
            None if !sent => anyhow::bail!("no video frames"),
            None if looping => {
                // Cut short: the next pass follows the last frame shown, not a freeze until the
                // clip's full duration.
                offset += match last.filter(|_| cut) {
                    Some(pts) => (pts + d.frame_interval).min(d.length),
                    None => d.length,
                };
                last = None;
                d.rewind()?;
            }
            None => {
                if tx.send(Msg::End { epoch }).is_err() {
                    return Ok(());
                }
                let Ok(e) = seeks.recv() else { return Ok(()) }; // idle until a seek or the player drops
                (epoch, offset, last) = (e, Duration::ZERO, None);
                d.seek(e)?;
            }
        }
    }
}

/// `kind` + file name. `Builder::name` panics on a NUL, and paths come from unvalidated JSON.
fn thread_name(kind: &str, path: &Path) -> String {
    format!("{kind} {}", path.file_name().unwrap_or_default().to_string_lossy()).replace('\0', "")
}

/// Larger than the GPU's 2D texture limit can't be uploaded (R20): error, layer transparent.
fn check_size(gpu: &Gpu, w: u32, h: u32) -> anyhow::Result<()> {
    let max = gpu.device.limits().max_texture_dimension_2d;
    anyhow::ensure!(w <= max && h <= max, "{w}x{h} exceeds the GPU texture limit ({max} px)");
    Ok(())
}

/// Texture format, subsampling divisor (x, y) and bytes per texel of each plane.
fn layout(kind: PixelFormatKind) -> &'static [(F, (u32, u32), u32)] {
    match kind {
        PixelFormatKind::Nv12 => &[(F::R8Unorm, (1, 1), 1), (F::Rg8Unorm, (2, 2), 2)],
        PixelFormatKind::P010 => &[(F::R16Unorm, (1, 1), 2), (F::Rg16Unorm, (2, 2), 4)],
        PixelFormatKind::P210 => &[(F::R16Unorm, (1, 1), 2), (F::Rg16Unorm, (2, 1), 4)],
        PixelFormatKind::Ayuv64 => &[(F::Rgba16Unorm, (1, 1), 8)],
        PixelFormatKind::Rgba => &[(F::Rgba8Unorm, (1, 1), 4)],
        PixelFormatKind::Bgra | PixelFormatKind::Bgrx => &[(F::Bgra8Unorm, (1, 1), 4)],
    }
}

/// A player's GPU side, reused frame to frame until the format or size changes.
pub(crate) struct GpuFrame {
    key: (PixelFormatKind, u32, u32),
    textures: Vec<wgpu::Texture>,
    pub(crate) layer: LayerTexture,
    /// Persistent staging (R20): `Queue::write_texture` allocates a staging buffer per call.
    belt: StagingBelt,
}

impl GpuFrame {
    fn new(gpu: &Gpu, kind: PixelFormatKind, w: u32, h: u32) -> GpuFrame {
        let textures: Vec<_> = layout(kind)
            .iter()
            .map(|&(format, (dx, dy), _)| {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("video plane"),
                    size: wgpu::Extent3d { width: w.div_ceil(dx).max(1), height: h.div_ceil(dy).max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            })
            .collect();
        let view = |i: usize| textures[i].create_view(&Default::default());
        let planes = match kind {
            PixelFormatKind::Nv12 => Planes::Nv12 { y: view(0), uv: view(1) },
            // 4:2:2 is P010's shader path too: the half-width, full-height UV plane is sampled by
            // normalized coordinates.
            PixelFormatKind::P010 | PixelFormatKind::P210 => Planes::P010 { y: view(0), uv: view(1) },
            PixelFormatKind::Ayuv64 => Planes::Ayuv64(view(0)),
            PixelFormatKind::Rgba | PixelFormatKind::Bgra => Planes::Rgba(view(0)),
            PixelFormatKind::Bgrx => Planes::Rgbx(view(0)),
        };
        // One frame per chunk: each frame's planes go up in one belt chunk.
        let bytes = textures.iter().zip(layout(kind)).map(|(t, &(_, _, bpp))| pitch(t.width(), bpp) * t.height() as u64).sum();
        GpuFrame {
            key: (kind, w, h),
            textures,
            layer: LayerTexture { planes, premultiplied: false, full_range: false },
            belt: StagingBelt::new(gpu.device.clone(), bytes),
        }
    }

    fn upload(&mut self, gpu: &Gpu, frame: &Frame) -> anyhow::Result<()> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (i, (texture, &(_, _, bpp))) in self.textures.iter().zip(layout(self.key.0)).enumerate() {
            let (row, rows, pitch) = ((texture.width() * bpp) as usize, texture.height(), pitch(texture.width(), bpp));
            let (src, stride) = frame.plane(i, row, rows as usize)?;
            let size = wgpu::BufferSize::new(pitch * rows as u64).context("empty plane")?;
            let slice = self.belt.allocate(size, wgpu::BufferSize::new(ALIGN).context("alignment")?);
            {
                let mut staging = slice.get_mapped_range_mut().context("staging buffer not mapped")?;
                if stride as u64 == pitch {
                    staging.slice(..src.len()).copy_from_slice(src);
                } else {
                    let pitch = pitch as usize;
                    for (r, line) in src.chunks(stride).enumerate() {
                        staging.slice(r * pitch..r * pitch + row).copy_from_slice(&line[..row]);
                    }
                }
            }
            let layout = wgpu::TexelCopyBufferLayout { offset: slice.offset(), bytes_per_row: Some(pitch as u32), rows_per_image: Some(rows) };
            encoder.copy_buffer_to_texture(wgpu::TexelCopyBufferInfo { buffer: slice.buffer(), layout }, texture.as_image_copy(), texture.size());
        }
        self.belt.finish();
        gpu.queue.submit([encoder.finish()]);
        self.belt.recall();
        let _ = gpu.device.poll(wgpu::PollType::Poll); // hands finished chunks back to the belt
        Ok(())
    }
}

/// Staging row pitch for `width` texels of `bpp` bytes.
fn pitch(width: u32, bpp: u32) -> u64 {
    (width as u64 * bpp as u64).next_multiple_of(ALIGN)
}

/// A still image, decoded, premultiplied and uploaded once, all off the render thread.
pub struct ImageSource {
    loaded: Option<Receiver<anyhow::Result<LayerTexture>>>,
    layer: Option<LayerTexture>,
    error: Option<String>,
    failed: bool,
}

impl ImageSource {
    /// Returns at once (R22): a 4K PNG takes ~40 ms to decode and ~10 ms to upload, so a thread
    /// does both. Errors show up in `take_error`; `Err` only if the thread can't be spawned.
    pub fn open(gpu: &Gpu, path: &Path) -> anyhow::Result<ImageSource> {
        let (tx, loaded) = mpsc::sync_channel(1);
        let (gpu, owned) = (gpu.clone(), path.to_path_buf());
        std::thread::Builder::new().name(thread_name("image", path)).spawn(move || {
            let _ = tx.send(load_image(&gpu, &owned).with_context(|| owned.display().to_string()));
        })?;
        Ok(ImageSource { loaded: Some(loaded), layer: None, error: None, failed: false })
    }

    /// The image once loaded; `None` until then or on error.
    pub fn texture(&mut self) -> Option<LayerTexture> {
        if let Some(loaded) = &self.loaded {
            let result = match loaded.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("image thread died")),
            };
            self.loaded = None;
            match result {
                Ok(layer) => self.layer = Some(layer),
                Err(e) => (self.error, self.failed) = (Some(format!("{e:#}")), true),
            }
        }
        self.layer.clone()
    }

    /// Why the image will never show, once.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// It failed to load and never will.
    pub fn failed(&self) -> bool {
        self.failed
    }
}

/// Loader thread. `Gpu::upload` (write_texture) only queues the copy; the render thread's next
/// submit carries it, so this never submits concurrently with a surface configure.
fn load_image(gpu: &Gpu, path: &Path) -> anyhow::Result<LayerTexture> {
    let (w, h) = image::image_dimensions(path).context("cannot open")?;
    check_size(gpu, w, h)?;
    let mut image = image::open(path).context("cannot open")?.into_rgba8();
    for p in image.pixels_mut() {
        let a = p[3] as u16;
        for c in &mut p.0[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
    let texture = gpu.upload(w, h, F::Rgba8Unorm, &image);
    let planes = Planes::Rgba(texture.create_view(&Default::default()));
    Ok(LayerTexture { planes, premultiplied: true, full_range: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_micros(16_667);

    /// I1(a): a decode error after the clip has shown frames (a file cut short) ends the pass: one
    /// warning, and the Loop plays on from the start right after the last good frame, never
    /// failing and never freezing until the clip's full duration.
    #[test]
    fn corrupt_tail_loops_on() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/h264_trunc_64.mp4");
        assert!(crate::video_decode::decode_all_frames(&path).is_err(), "the fixture must end in an error");
        let mut player = Player::open(&path, Playback::Loop, None).unwrap();
        let t0 = Instant::now();
        player.play(t0);
        let (mut frames, mut errors) = (0, vec![]);
        let steps = 90; // 3 s at 30 fps: three passes of the 1 s clip, each cut short
        for step in 0..steps {
            let now = t0 + Duration::from_secs(1) * step / 30;
            let give_up = Instant::now() + Duration::from_secs(2);
            loop {
                player.poll(now, PERIOD);
                errors.extend(player.take_error());
                assert!(!player.failed(), "failed: {errors:?}");
                if player.due.take().is_some() {
                    frames += 1;
                    break;
                }
                if player.pending.is_some() {
                    break; // the next frame isn't due at this time: the picture holds
                }
                assert!(Instant::now() < give_up, "no frame at step {step}");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("h264_trunc_64.mp4"), "{errors:?}");
        assert!(frames >= steps - 3, "{frames} of {steps} steps showed a new frame");
    }
}

#[cfg(test)]
mod packed_tests {
    use super::*;
    use crate::compositor::{Compositor, ScreenSlot};
    use crate::gpu::read_rgba;
    use tessera_show::{Layer, Rect, Screen, Source};

    /// A 2×2 frame of one packed pixel, uploaded and drawn full-screen; the drawn pixel.
    fn drawn(kind: PixelFormatKind, pixel: [u8; 4]) -> [u8; 4] {
        let gpu = Gpu::new_headless().expect("these tests need a GPU");
        let frame = Frame::packed(2, 2, &pixel.repeat(4), 8, kind).unwrap();
        let mut slot = None;
        let texture = upload_into(&mut slot, &gpu, &frame).unwrap();
        let screen = Screen { id: uuid::Uuid::new_v4(), name: "S".into(), width: 4, height: 4, layout_x: 0, layout_y: 0 };
        let layer = Layer::new(Source::Media { media_id: uuid::Uuid::new_v4() }, Rect { x: 0., y: 0., w: 4., h: 4. });
        let mut c = Compositor::new(&gpu);
        let view = c.render_screen(&gpu, ScreenSlot::Pgm, &screen, &[(layer, Some(texture))]);
        read_rgba(&gpu, view.texture()).unwrap()[..4].try_into().unwrap()
    }

    #[test]
    fn bgra_and_bgrx_draw_their_colours() {
        assert_eq!(drawn(PixelFormatKind::Bgra, [0, 0, 255, 255]), [255, 0, 0, 255], "BGRA red stays red");
        assert_eq!(drawn(PixelFormatKind::Bgrx, [255, 0, 0, 7]), [0, 0, 255, 255], "BGRX blue, fourth byte ignored: opaque");
        let half = drawn(PixelFormatKind::Bgra, [0, 255, 0, 128]);
        assert!(half[1].abs_diff(128) <= 2 && half[3].abs_diff(128) <= 2, "straight alpha, premultiplied on output: {half:?}");
    }
}
