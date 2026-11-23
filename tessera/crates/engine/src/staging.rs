//! Frames reach the GPU through staging buffers their producer (a decode or live-input thread)
//! fills, so the render thread only records the buffer → texture copies. The memcpy itself (25 MB
//! for a 4K 10-bit frame) runs on the producers' threads, in parallel across layers, instead of
//! serially on the render loop every frame.
//!
//! A `Pool` hands out mapped buffers; a `Staged` frame goes back to its pool when dropped: right
//! away if it was never drawn, or (through `Gpu::retire`) once the GPU has copied it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use anyhow::Context;

use crate::gpu::Gpu;
use crate::player::{layout, pitch};
use crate::video_decode::{Frame, PixelFormatKind};

/// A frame's planes in a staging buffer, ready for `copy_to`.
pub struct Staged {
    /// `None` only while dropping.
    buffer: Option<wgpu::Buffer>,
    /// Pixel layout, width, height.
    pub(crate) key: (PixelFormatKind, u32, u32),
    pub(crate) full_range: bool,
    /// Byte offset and row pitch of each plane (`layout(kind)` order).
    planes: [(u64, u32); 2],
    back: Sender<wgpu::Buffer>,
}

impl Staged {
    pub fn size(&self) -> (u32, u32) {
        (self.key.1, self.key.2)
    }

    /// Records the copies of the planes into `textures` (made for `key`'s layout) in the frame's
    /// encoder. Keep `self` alive until that work is done: `Gpu::retire` it.
    pub(crate) fn copy_to(&self, gpu: &Gpu, textures: &[wgpu::Texture]) {
        let Some(buffer) = &self.buffer else { return };
        gpu.record(|enc| {
            for (texture, &(offset, pitch)) in textures.iter().zip(&self.planes) {
                let layout = wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(pitch), rows_per_image: Some(texture.height()) };
                enc.copy_buffer_to_texture(wgpu::TexelCopyBufferInfo { buffer, layout }, texture.as_image_copy(), texture.size());
            }
        });
    }
}

impl Drop for Staged {
    /// Maps the buffer for writing again and, once mapped, hands it back to its pool (gone: freed).
    fn drop(&mut self) {
        let Some(buffer) = self.buffer.take() else { return };
        let (back, mapped) = (self.back.clone(), buffer.clone());
        buffer.map_async(wgpu::MapMode::Write, .., move |r| {
            if r.is_ok() {
                let _ = back.send(mapped);
            }
        });
    }
}

/// Up to `cap` staging buffers of one frame size, recycled through `Staged`'s drop.
pub(crate) struct Pool {
    device: wgpu::Device,
    free: Receiver<wgpu::Buffer>,
    back: Sender<wgpu::Buffer>,
    /// Size of the current buffers; others coming back are freed.
    bytes: u64,
    /// Buffers of that size alive (free, filled or in flight).
    made: usize,
    cap: usize,
    /// A row being interleaved (planar frames).
    scratch: Vec<u8>,
}

impl Pool {
    pub(crate) fn new(gpu: &Gpu, cap: usize) -> Pool {
        let (back, free) = channel();
        Pool { device: gpu.device.clone(), free, back, bytes: 0, made: 0, cap: cap.max(1), scratch: vec![] }
    }

    /// `frame`'s planes copied into a staging buffer. `wait`: block until one is free (a decode
    /// thread: the render loop's pace holds it back); otherwise `None` when every buffer is in use
    /// (a live input: drop the frame, the next one is newer).
    pub(crate) fn stage(&mut self, frame: &Frame, wait: bool) -> anyhow::Result<Option<Staged>> {
        let mapped = frame.mapped()?; // a frame kept on the hardware for zero-copy, which didn't take
        let frame = mapped.as_ref().unwrap_or(frame);
        let (w, h) = frame.size();
        let mut planes = [(0, 0); 2];
        let mut bytes = 0;
        for (plane, &(_, (dx, dy), bpp)) in planes.iter_mut().zip(layout(frame.kind)) {
            let (pw, rows) = (w.div_ceil(dx).max(1), h.div_ceil(dy).max(1));
            let pitch = pitch(pw, bpp);
            *plane = (bytes, u32::try_from(pitch).context("frame too wide")?);
            bytes += pitch * rows as u64;
        }
        if bytes != self.bytes {
            (self.bytes, self.made) = (bytes, 0); // a new size: old buffers are freed as they return
        }
        let Some(buffer) = self.buffer(wait) else { return Ok(None) };
        {
            let mut staging = buffer.slice(..).get_mapped_range_mut().context("staging buffer not mapped")?;
            if let Some(shift) = frame.planar {
                let layout = layout(frame.kind);
                let wide = layout[0].2 == 2; // 16-bit samples
                let sample = |s: &[u8], out: &mut Vec<u8>| match wide {
                    true => out.extend_from_slice(&(u16::from_le_bytes([s[0], s[1]]) << shift).to_le_bytes()),
                    false => out.push(s[0]),
                };
                let n = if wide { 2 } else { 1 };
                // Y: as is (shifted when 10-bit).
                let (y, y_stride) = frame.plane(0, w as usize * n, h as usize)?;
                for r in 0..h as usize {
                    self.scratch.clear();
                    y[r * y_stride..r * y_stride + w as usize * n].chunks_exact(n).for_each(|s| sample(s, &mut self.scratch));
                    let at = planes[0].0 as usize + r * planes[0].1 as usize;
                    staging.slice(at..at + self.scratch.len()).copy_from_slice(&self.scratch);
                }
                // U and V interleaved into the UV plane.
                let (_, (dx, dy), _) = layout[1];
                let (cw, ch) = (w.div_ceil(dx) as usize, h.div_ceil(dy) as usize);
                let ((u, u_stride), (v, v_stride)) = (frame.plane(1, cw * n, ch)?, frame.plane(2, cw * n, ch)?);
                for r in 0..ch {
                    self.scratch.clear();
                    let (ur, vr) = (&u[r * u_stride..r * u_stride + cw * n], &v[r * v_stride..r * v_stride + cw * n]);
                    for (a, b) in ur.chunks_exact(n).zip(vr.chunks_exact(n)) {
                        sample(a, &mut self.scratch);
                        sample(b, &mut self.scratch);
                    }
                    let at = planes[1].0 as usize + r * planes[1].1 as usize;
                    staging.slice(at..at + self.scratch.len()).copy_from_slice(&self.scratch);
                }
            } else {
                for (i, (&(offset, pitch), &(_, (dx, dy), bpp))) in planes.iter().zip(layout(frame.kind)).enumerate() {
                    let (row, rows) = ((w.div_ceil(dx).max(1) * bpp) as usize, h.div_ceil(dy).max(1) as usize);
                    let (src, stride) = frame.plane(i, row, rows)?;
                    let (offset, pitch) = (offset as usize, pitch as usize);
                    if stride == pitch {
                        staging.slice(offset..offset + src.len()).copy_from_slice(src);
                    } else {
                        for (r, line) in src.chunks(stride).enumerate() {
                            staging.slice(offset + r * pitch..offset + r * pitch + row).copy_from_slice(&line[..row]);
                        }
                    }
                }
            }
        }
        buffer.unmap();
        let key = (frame.kind, w, h);
        Ok(Some(Staged { buffer: Some(buffer), key, full_range: frame.full_range, planes, back: self.back.clone() }))
    }

    /// A mapped buffer of the current size: a returned one, a new one while fewer than `cap` exist,
    /// or (`wait`) the next one returned.
    fn buffer(&mut self, wait: bool) -> Option<wgpu::Buffer> {
        loop {
            let returned = match self.free.try_recv() {
                Ok(b) => Some(b),
                Err(_) if self.made < self.cap => {
                    self.made += 1;
                    return Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("staging"),
                        size: self.bytes,
                        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: true,
                    }));
                }
                Err(_) if !wait => return None,
                // Buffers come back from map callbacks, which run when someone polls the device:
                // the render loop does every frame, but poll here too in case nothing else does.
                Err(_) => match self.free.recv_timeout(Duration::from_millis(10)) {
                    Ok(b) => Some(b),
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = self.device.poll(wgpu::PollType::Poll);
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => unreachable!("the pool holds a sender"),
                },
            };
            match returned {
                Some(b) if b.size() == self.bytes => return Some(b),
                Some(_) | None => {} // an old size's buffer is freed; keep looking
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buffers go round: staged, dropped (never drawn), mapped again, reused; never more than `cap`.
    #[test]
    fn buffers_are_recycled_up_to_the_cap() {
        let gpu = Gpu::new_headless().expect("these tests need a GPU");
        let mut pool = Pool::new(&gpu, 2);
        let frame = Frame::test_rgba(4, 4, [1, 2, 3, 4]);
        let (a, b) = (pool.stage(&frame, false).unwrap().unwrap(), pool.stage(&frame, false).unwrap().unwrap());
        assert!(pool.stage(&frame, false).unwrap().is_none(), "both buffers in use");
        drop((a, b));
        for _ in 0..20 {
            let staged = pool.stage(&frame, true).unwrap().expect("waits for a returned buffer");
            assert_eq!((staged.size(), pool.made), ((4, 4), 2));
        }
        // A new size: new buffers; the old ones are freed as they come back.
        let big = Frame::test_rgba(8, 8, [0; 4]);
        assert_eq!(pool.stage(&big, true).unwrap().unwrap().size(), (8, 8));
    }
}
