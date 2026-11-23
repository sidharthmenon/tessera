use std::any::Any;
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Context;
use wgpu::util::DeviceExt;

use crate::audio::lock;

/// Texture and buffer bindings live at once on a device (see `Gpu::new`).
const MAX_NON_SAMPLER_BINDINGS: u32 = 65_536;

/// Cheap to clone (wgpu handles are refcounted), e.g. to upload from a loader thread. Clones share
/// one per-frame command encoder (`record` / `submit`), so a frame's passes go to the GPU in a
/// couple of submits instead of one per draw.
#[derive(Clone)]
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pending: Arc<Mutex<Pending>>,
}

/// What the next `submit` sends, and what must outlive it on the GPU.
#[derive(Default)]
struct Pending {
    encoder: Option<wgpu::CommandEncoder>,
    retired: Vec<Box<dyn Any + Send>>,
    submits: u64,
}

impl Gpu {
    /// Device with no window; backend overridable via `WGPU_BACKEND`. Windows: DX12 by default,
    /// whose decoder frames are drawn without a copy (`zero_copy`); any backend if it has no device.
    pub fn new_headless() -> anyhow::Result<Gpu> {
        let desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env;
        #[cfg(windows)]
        if std::env::var_os("WGPU_BACKEND").is_none() {
            let mut dx12 = desc();
            dx12.backends = wgpu::Backends::DX12;
            match Self::new(wgpu::Instance::new(dx12), None) {
                Ok(gpu) => return Ok(gpu),
                Err(e) => log!("gpu: no DX12 device ({e:#}); trying the other backends"),
            }
        }
        Self::new(wgpu::Instance::new(desc()), None)
    }

    pub fn new_for_surface(instance: wgpu::Instance, surface: &wgpu::Surface<'_>) -> anyhow::Result<Gpu> {
        Self::new(instance, Some(surface))
    }

    fn new(instance: wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> anyhow::Result<Gpu> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            ..Default::default()
        }))
        .context("no GPU adapter found")?;
        // R16/Rg16 unorm planes for P010 video; immediates for the compositor's per-draw
        // parameters (no uniform buffer per draw); adapter limits so wide screens (> 8192 px) fit.
        // NV12/P010 textures where the adapter has them: DX12 decoder frames, drawn in place.
        let video = adapter.features() & (wgpu::Features::TEXTURE_FORMAT_NV12 | wgpu::Features::TEXTURE_FORMAT_P010);
        // DX12 sizes the device's shader-visible descriptor heap from max_non_sampler_bindings (a
        // million by default: tens of MB per device, enough to run a 1 GB integrated GPU out of
        // memory with a few devices). The compositor's bind groups (2 views each, dropped 2 frames
        // after last use) need a few hundred.
        let max = adapter.limits();
        let limits = wgpu::Limits { max_non_sampler_bindings: max.max_non_sampler_bindings.min(MAX_NON_SAMPLER_BINDINGS), ..max };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM | wgpu::Features::IMMEDIATES | video,
            required_limits: limits,
            ..Default::default()
        }))
        .context("GPU device lacks 16-bit normalized textures (needed for P010) or immediates")?;
        Ok(Gpu { instance, adapter, device, queue, pending: Arc::default() })
    }

    /// Records into this frame's encoder (made on first use), sent by the next `submit`. `f` must
    /// not call back into this `Gpu` (the encoder is locked meanwhile).
    pub fn record<R>(&self, f: impl FnOnce(&mut wgpu::CommandEncoder) -> R) -> R {
        let mut pending = lock(&self.pending);
        f(pending.encoder.get_or_insert_with(|| self.device.create_command_encoder(&Default::default())))
    }

    /// Keeps `x` alive until the GPU has finished everything recorded before the next `submit`.
    pub fn retire(&self, x: impl Any + Send + 'static) {
        lock(&self.pending).retired.push(Box::new(x));
    }

    /// Submits what was recorded (nothing to do if nothing was); retired things drop once that
    /// work is done.
    pub fn submit(&self) {
        let (encoder, retired) = {
            let mut pending = lock(&self.pending);
            pending.submits += 1;
            (pending.encoder.take(), std::mem::take(&mut pending.retired))
        };
        if let Some(encoder) = encoder {
            self.queue.submit([encoder.finish()]);
        }
        if !retired.is_empty() {
            self.queue.on_submitted_work_done(move || drop(retired));
        }
    }

    /// `submit` calls so far: work recorded before an earlier count has been submitted.
    pub fn submits(&self) -> u64 {
        lock(&self.pending).submits
    }

    /// Size `target(width, height)` really gets: at least 1 px, at most the device's 2D limit.
    pub fn target_size(&self, width: u32, height: u32) -> (u32, u32) {
        let max = self.device.limits().max_texture_dimension_2d;
        (width.clamp(1, max), height.clamp(1, max))
    }

    /// Rgba8Unorm render target that can also be sampled and read back; oversize is clamped
    /// (with a warning) instead of failing.
    pub fn target(&self, width: u32, height: u32) -> wgpu::Texture {
        let (w, h) = self.target_size(width, height);
        if width > w || height > h {
            log!("gpu: {width}x{height} exceeds the texture limit, clamped to {w}x{h}");
        }
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent(w, h),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Sampleable texture filled with tightly packed `data`.
    pub fn upload(&self, width: u32, height: u32, format: wgpu::TextureFormat, data: &[u8]) -> wgpu::Texture {
        let desc = wgpu::TextureDescriptor {
            label: None,
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        };
        self.device.create_texture_with_data(&self.queue, &desc, wgpu::util::TextureDataOrder::LayerMajor, data)
    }
}

/// Zero-sized textures are invalid; clamp to 1 px.
fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 }
}

/// Copies a 4-byte-per-pixel texture (needs `COPY_SRC`) into a mappable buffer without waiting
/// for the GPU: `start` it (it submits whatever was recorded), keep polling the device
/// (`PollType::Poll`), `try_take` the pixels.
pub struct Readback {
    buffer: wgpu::Buffer,
    size: (u32, u32),
    padded: u32,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl Readback {
    pub fn start(gpu: &Gpu, texture: &wgpu::Texture) -> Readback {
        Readback::start_into(gpu, texture, None)
    }

    /// `start`, reusing `spare` (an earlier readback's `into_buffer`) when it is the right size.
    pub fn start_into(gpu: &Gpu, texture: &wgpu::Texture, spare: Option<wgpu::Buffer>) -> Readback {
        let padded = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = (padded * texture.height()) as u64;
        let buffer = spare.filter(|b| b.size() == size).unwrap_or_else(|| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        // After this frame's passes in the shared encoder, so it reads what they drew.
        gpu.record(|enc| {
            enc.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
                },
                texture.size(),
            )
        });
        gpu.submit(); // before map_async: the map waits for the submit that writes the buffer
        let (tx, mapped) = mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| _ = tx.send(r));
        Readback { buffer, size: (texture.width(), texture.height()), padded, mapped }
    }

    /// The buffer, for the next `start_into` (after `try_take` returned the pixels: unmapped).
    pub fn into_buffer(self) -> wgpu::Buffer {
        self.buffer
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The pixels (rows tightly packed) once the GPU has copied them; `None` while it hasn't.
    pub fn try_take(&mut self) -> Option<anyhow::Result<Vec<u8>>> {
        let mapped = match self.mapped.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("readback already taken or dropped")),
            Ok(r) => r.context("map readback buffer"),
        };
        Some(mapped.and_then(|()| {
            let row = self.size.0 as usize * 4;
            let data = self.buffer.get_mapped_range(..).context("readback buffer not mapped")?;
            let mut pixels = Vec::with_capacity(row * self.size.1 as usize);
            for line in data.chunks(self.padded as usize) {
                pixels.extend_from_slice(&line[..row]); // a memcpy per row, even in debug builds
            }
            drop(data);
            self.buffer.unmap();
            Ok(pixels)
        }))
    }
}

/// Blocking readback (tests, diagnostics): `Readback` (which submits what was recorded) plus a wait
/// for the GPU.
pub fn read_rgba(gpu: &Gpu, texture: &wgpu::Texture) -> anyhow::Result<Vec<u8>> {
    let mut readback = Readback::start(gpu, texture);
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).context("GPU poll")?;
    readback.try_take().context("readback not done after waiting for the GPU")?
}
