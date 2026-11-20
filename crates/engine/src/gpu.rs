use std::sync::mpsc;

use anyhow::Context;
use wgpu::util::DeviceExt;

/// Cheap to clone (wgpu handles are refcounted), e.g. to upload from a loader thread.
#[derive(Clone)]
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Device with no window; backend overridable via `WGPU_BACKEND`.
    pub fn new_headless() -> anyhow::Result<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        Self::new(instance, None)
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
        // R16/Rg16 unorm planes for P010 video; adapter limits so wide screens (> 8192 px) fit.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .context("GPU device lacks 16-bit normalized textures (needed for P010)")?;
        Ok(Gpu { instance, adapter, device, queue })
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
            eprintln!("warning: {width}x{height} exceeds the GPU texture limit, clamped to {w}x{h}");
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
/// for the GPU: `start` it, keep polling the device (`PollType::Poll`), `try_take` the pixels.
pub struct Readback {
    buffer: wgpu::Buffer,
    size: (u32, u32),
    padded: u32,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

impl Readback {
    pub fn start(gpu: &Gpu, texture: &wgpu::Texture) -> Readback {
        let padded = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // ponytail: a new buffer per readback (≤ 2 MB at 15 fps per watched preview); keep a spare if it shows up in a profile.
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * texture.height()) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            texture.size(),
        );
        gpu.queue.submit([enc.finish()]);
        let (tx, mapped) = mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| _ = tx.send(r));
        Readback { buffer, size: (texture.width(), texture.height()), padded, mapped }
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

/// Blocking readback (tests, diagnostics): `Readback` plus a wait for the GPU.
pub fn read_rgba(gpu: &Gpu, texture: &wgpu::Texture) -> anyhow::Result<Vec<u8>> {
    let mut readback = Readback::start(gpu, texture);
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).context("GPU poll")?;
    readback.try_take().context("readback not done after waiting for the GPU")?
}
