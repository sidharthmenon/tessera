//! Video players (decode thread → small frame queue → GPU planes) and still images.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Context;
use tessera_show::Playback;
use wgpu::TextureFormat as F;
use wgpu::util::StagingBelt;

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
}

#[derive(Clone, Copy)]
enum State {
    Paused(Duration),
    /// Timeline position 0 was at this instant.
    Playing(Instant),
}

/// One playing video file. Starts paused on its first frame. All timing comes in as `now`
/// (the render loop's frame clock), so the player never reads the wall clock itself.
pub struct Player {
    playback: Playback,
    /// Known once the decode thread has opened the file.
    duration: Option<Duration>,
    frames: Receiver<Msg>,
    seek: Sender<u64>,
    /// Bumped by every seek; frames decoded before it are discarded.
    epoch: u64,
    state: State,
    /// Next frame, not due yet.
    pending: Option<(Duration, Frame)>,
    ended: bool,
    failed: bool,
    gpu: Option<GpuFrame>,
    error: Option<String>,
    dropped: u64,
}

impl Player {
    /// Returns at once (R22: opening must never stall the render loop); a new decode thread opens
    /// the file and decodes ahead. Until the first frame is ready `current_frame` is `None`; a
    /// missing file, unsupported codec or decode error shows up in `take_error`. `Err` only if
    /// the thread can't be spawned.
    pub fn open(path: &Path, playback: Playback) -> anyhow::Result<Player> {
        let (tx, frames) = mpsc::sync_channel(QUEUE);
        let (seek, seeks) = mpsc::channel();
        let (owned, looping) = (path.to_path_buf(), playback == Playback::Loop);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        std::thread::Builder::new().name(format!("decode {name}")).spawn(move || {
            let result = Decoder::open(&owned).and_then(|d| {
                let _ = tx.send(Msg::Opened(d.duration));
                decode(d, looping, &tx, &seeks)
            });
            if let Err(e) = result {
                let _ = tx.send(Msg::Error(format!("{}: {e:#}", owned.display())));
            }
        })?;
        let state = State::Paused(Duration::ZERO);
        Ok(Player { playback, duration: None, frames, seek, epoch: 0, state, pending: None, ended: false, failed: false, gpu: None, error: None, dropped: 0 })
    }

    pub fn play(&mut self, at: Instant) {
        if let State::Paused(t) = self.state {
            self.state = State::Playing(at.checked_sub(t).unwrap_or(at));
        }
    }

    pub fn pause(&mut self, now: Instant) {
        self.state = State::Paused(self.elapsed(now));
    }

    /// Back to the first frame, paused.
    pub fn stop(&mut self) {
        self.epoch += 1;
        let _ = self.seek.send(self.epoch);
        (self.state, self.pending, self.ended) = (State::Paused(Duration::ZERO), None, false);
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
        position.as_millis() as u64
    }

    /// Showing its first frame, or failed: waiting longer won't change how it starts.
    pub fn ready(&self) -> bool {
        self.gpu.is_some() || self.failed
    }

    /// The newest frame due at `now` (pts ≤ position + `period`/2, so frame boundaries sit half
    /// a vsync away from the ticks: R19), uploaded into this player's reused textures. `None`:
    /// nothing to show (first frame not decoded yet, Once after its end, or the player failed;
    /// see `take_error`). Call it every frame: it also collects the decode thread's messages.
    pub fn current_frame(&mut self, gpu: &Gpu, now: Instant, period: Duration) -> Option<LayerTexture> {
        let t = self.elapsed(now);
        if self.failed || (self.playback == Playback::Once && self.duration.is_some_and(|d| t >= d)) {
            return None;
        }
        let due = t + period / 2;
        let (mut newest, mut taken) = (None, 0);
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
                Msg::Frame { frame, .. } => (newest, taken) = (Some(frame), taken + 1),
                Msg::End { epoch } => self.ended |= epoch == self.epoch,
                Msg::Error(e) => return self.fail(e),
            }
        }
        match newest {
            // More than one due: the ones before the newest are never shown.
            Some(frame) => {
                self.dropped += taken - 1;
                if let Err(e) = self.upload(gpu, &frame) {
                    return self.fail(format!("{e:#}"));
                }
            }
            // Playing, showing something, nothing queued and not at the end: the decoder is late,
            // the frame repeats.
            None if matches!(self.state, State::Playing(_)) && self.gpu.is_some() && self.pending.is_none() && !self.ended => {
                self.dropped += 1
            }
            None => {}
        }
        self.gpu.as_ref().map(|g| g.layer.clone())
    }

    /// The error that made this player go transparent, once.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// Frames skipped or repeated since the last call because the decoder (or the loop) was late.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    fn elapsed(&self, now: Instant) -> Duration {
        match self.state {
            State::Paused(t) => t,
            State::Playing(start) => now.saturating_duration_since(start),
        }
    }

    fn fail(&mut self, error: String) -> Option<LayerTexture> {
        (self.failed, self.error, self.gpu) = (true, Some(error), None);
        None
    }

    fn upload(&mut self, gpu: &Gpu, frame: &Frame) -> anyhow::Result<()> {
        let (w, h) = frame.size();
        check_size(gpu, w, h)?;
        if self.gpu.as_ref().is_none_or(|g| g.key != (frame.kind, w, h)) {
            self.gpu = Some(GpuFrame::new(gpu, frame.kind, w, h));
        }
        let g = self.gpu.as_mut().context("no textures")?;
        g.layer.full_range = frame.full_range;
        g.upload(gpu, frame)
    }
}

/// Decode thread: frames in order into `tx` (blocking when the queue is full); Loop rewinds at
/// the end without a gap, Once/HoldLast report `End` and wait for a seek.
fn decode(mut d: Decoder, looping: bool, tx: &SyncSender<Msg>, seeks: &Receiver<u64>) -> anyhow::Result<()> {
    let (mut epoch, mut offset, mut sent) = (0, Duration::ZERO, false);
    loop {
        if let Some(e) = seeks.try_iter().last() {
            (epoch, offset) = (e, Duration::ZERO);
            d.rewind()?;
        }
        match d.next()? {
            Some(frame) => {
                if tx.send(Msg::Frame { epoch, t: offset + frame.pts, frame }).is_err() {
                    return Ok(()); // player dropped
                }
                sent = true;
            }
            // Would loop forever without output (and never show anything).
            None if !sent => anyhow::bail!("no video frames"),
            None if looping => {
                offset += d.duration;
                d.rewind()?;
            }
            None => {
                if tx.send(Msg::End { epoch }).is_err() {
                    return Ok(());
                }
                let Ok(e) = seeks.recv() else { return Ok(()) }; // idle until a seek or the player drops
                (epoch, offset) = (e, Duration::ZERO);
                d.rewind()?;
            }
        }
    }
}

/// Larger than the GPU's 2D texture limit can't be uploaded (R20): error, layer transparent.
fn check_size(gpu: &Gpu, w: u32, h: u32) -> anyhow::Result<()> {
    let max = gpu.device.limits().max_texture_dimension_2d;
    anyhow::ensure!(w <= max && h <= max, "{w}x{h} exceeds the GPU texture limit ({max} px)");
    Ok(())
}

/// Texture format, subsampling divisor (both axes) and bytes per texel of each plane.
fn layout(kind: PixelFormatKind) -> &'static [(F, u32, u32)] {
    match kind {
        PixelFormatKind::Nv12 => &[(F::R8Unorm, 1, 1), (F::Rg8Unorm, 2, 2)],
        PixelFormatKind::P010 => &[(F::R16Unorm, 1, 2), (F::Rg16Unorm, 2, 4)],
        PixelFormatKind::Ayuv64 => &[(F::Rgba16Unorm, 1, 8)],
        PixelFormatKind::Rgba => &[(F::Rgba8Unorm, 1, 4)],
    }
}

/// A player's GPU side, reused frame to frame until the format or size changes.
struct GpuFrame {
    key: (PixelFormatKind, u32, u32),
    textures: Vec<wgpu::Texture>,
    layer: LayerTexture,
    /// Persistent staging (R20): `Queue::write_texture` allocates a staging buffer per call.
    belt: StagingBelt,
}

impl GpuFrame {
    fn new(gpu: &Gpu, kind: PixelFormatKind, w: u32, h: u32) -> GpuFrame {
        let textures: Vec<_> = layout(kind)
            .iter()
            .map(|&(format, div, _)| {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("video plane"),
                    size: wgpu::Extent3d { width: w.div_ceil(div).max(1), height: h.div_ceil(div).max(1), depth_or_array_layers: 1 },
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
            PixelFormatKind::P010 => Planes::P010 { y: view(0), uv: view(1) },
            PixelFormatKind::Ayuv64 => Planes::Ayuv64(view(0)),
            PixelFormatKind::Rgba => Planes::Rgba(view(0)),
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
}

impl ImageSource {
    /// Returns at once (R22): a 4K PNG takes ~40 ms to decode and ~10 ms to upload, so a thread
    /// does both. Errors show up in `take_error`; `Err` only if the thread can't be spawned.
    pub fn open(gpu: &Gpu, path: &Path) -> anyhow::Result<ImageSource> {
        let (tx, loaded) = mpsc::sync_channel(1);
        let (gpu, owned) = (gpu.clone(), path.to_path_buf());
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        std::thread::Builder::new().name(format!("image {name}")).spawn(move || {
            let _ = tx.send(load_image(&gpu, &owned).with_context(|| owned.display().to_string()));
        })?;
        Ok(ImageSource { loaded: Some(loaded), layer: None, error: None })
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
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }
        self.layer.clone()
    }

    /// Why the image will never show, once.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
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
