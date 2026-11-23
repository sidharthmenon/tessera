//! FFmpeg decode of one video stream: hardware first (VideoToolbox; D3D12VA on the engine's DX12
//! device, else D3D11VA), frames kept on the GPU for `zero_copy` or made CPU-addressable in place
//! with `av_hwframe_map` (R20) for the player to upload.

use std::path::Path;
use std::ptr;
use std::time::Duration;

use anyhow::{Context, ensure};
use ffmpeg_next as ff;
use ff::ffi;

use crate::audio::{AudioDecoder, AudioTap};
use crate::gpu::Gpu;

/// How a frame's planes reach the GPU (see `player::layout`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormatKind {
    /// 8-bit 4:2:0, Y plane + interleaved CbCr plane.
    Nv12,
    /// 10-bit 4:2:0 like NV12, MSB-aligned in 16 bits.
    P010,
    /// 10-bit 4:2:2 like P010 with a full-height CbCr plane: VideoToolbox's ProRes 422 (`p210le`).
    P210,
    /// 10-bit 4:4:4 like P010 with a full-size CbCr plane (software `yuv444p10le`, interleaved).
    P410,
    /// 8-bit 4:2:2 like NV12 with a full-height CbCr plane (software `yuv422p`, interleaved).
    Nv16,
    /// 8-bit 4:4:4 like NV12 with a full-size CbCr plane (software `yuv444p`, interleaved).
    Nv24,
    /// Packed 16-bit A, Y, Cb, Cr: VideoToolbox's ProRes 4444 with alpha (`y416`).
    Ayuv64,
    /// Straight-alpha RGBA8, converted by `sws_scale` (software fallback for everything else).
    Rgba,
    /// Straight-alpha BGRA8 (NDI).
    Bgra,
    /// BGR8 plus an undefined fourth byte (NDI without alpha): drawn opaque.
    Bgrx,
}

#[derive(Clone, Copy, Debug)]
pub struct DecodedFrameInfo {
    pub pts_ms: u64,
    pub format: PixelFormatKind,
    /// Decoded by the platform's hardware decoder rather than in software.
    pub hardware: bool,
}

/// Decodes `path` to the end on this thread and describes every frame (tests, diagnostics).
pub fn decode_all_frames(path: &Path) -> anyhow::Result<Vec<DecodedFrameInfo>> {
    let mut decoder = Decoder::open(path, None, None).with_context(|| path.display().to_string())?;
    let mut frames = vec![];
    while let Some(f) = decoder.next()? {
        frames.push(DecodedFrameInfo { pts_ms: f.pts.as_millis() as u64, format: f.kind, hardware: f.hardware });
    }
    Ok(frames)
}

/// A decoded frame with CPU-addressable planes. Holding it holds the decoder's (hardware) buffer,
/// so drop it as soon as it is uploaded.
pub(crate) struct Frame {
    /// From the start of the stream.
    pub pts: Duration,
    pub kind: PixelFormatKind,
    pub hardware: bool,
    pub full_range: bool,
    /// Planar Y, U, V (software decode) for a semi-planar `kind`: staging interleaves U and V,
    /// shifting each sample left this much (6: 10-bit samples to P010's MSB alignment).
    pub(crate) planar: Option<u32>,
    frame: ff::frame::Video,
}

impl Frame {
    /// A copy of packed 4-byte pixels (`Bgra`/`Bgrx`/`Rgba`), `stride` bytes per row, so the source
    /// buffer can be released at once.
    pub(crate) fn packed(w: u32, h: u32, data: &[u8], stride: usize, kind: PixelFormatKind) -> anyhow::Result<Frame> {
        let row = w as usize * 4;
        ensure!(w > 0 && h > 0 && stride >= row && data.len() >= stride * (h as usize - 1) + row, "frame {w}×{h}: buffer too small");
        let pixel = if kind == PixelFormatKind::Rgba { ff::format::Pixel::RGBA } else { ff::format::Pixel::BGRA };
        let mut frame = ff::frame::Video::new(pixel, w, h);
        let out_stride = frame.stride(0);
        for (dst, src) in frame.data_mut(0).chunks_mut(out_stride).zip(data.chunks(stride)).take(h as usize) {
            dst[..row].copy_from_slice(&src[..row]);
        }
        Ok(Frame { pts: Duration::ZERO, kind, hardware: false, full_range: false, planar: None, frame })
    }

    /// A solid `w`×`h` RGBA frame, for tests.
    #[cfg(test)]
    pub(crate) fn test_rgba(w: u32, h: u32, rgba: [u8; 4]) -> Frame {
        let mut frame = ff::frame::Video::new(ff::format::Pixel::RGBA, w, h);
        let stride = frame.stride(0);
        for row in frame.data_mut(0).chunks_mut(stride) {
            for px in row[..w as usize * 4].chunks_mut(4) {
                px.copy_from_slice(&rgba);
            }
        }
        Frame { pts: Duration::ZERO, kind: PixelFormatKind::Rgba, hardware: false, full_range: false, planar: None, frame }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.frame.width(), self.frame.height())
    }

    /// A hardware frame kept as the decoder made it (`Decoder::zero_copy`), whose planes can't be
    /// read until `mapped`: macOS, its CVPixelBuffer; Windows, its `AVD3D12VAFrame`.
    pub(crate) fn pixel_buffer(&self) -> Option<*mut std::ffi::c_void> {
        // SAFETY: plain field reads on a live frame.
        unsafe {
            let p = self.frame.as_ptr();
            let plane = if (*p).format == ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32 { 3 } else { 0 };
            is_hw((*p).format).then(|| (*p).data[plane].cast())
        }
    }

    /// For a frame kept on the hardware (`pixel_buffer`), a copy whose planes are CPU-addressable
    /// (mapped in place where the platform allows); `None` for a frame that already is.
    pub(crate) fn mapped(&self) -> anyhow::Result<Option<Frame>> {
        if self.pixel_buffer().is_none() {
            return Ok(None);
        }
        // SAFETY: a hardware frame (checked above).
        let frame = unsafe { map_hw(&self.frame)? };
        Ok(Some(Frame { pts: self.pts, kind: self.kind, hardware: true, full_range: self.full_range, planar: None, frame }))
    }

    /// Plane `i` (`rows` rows of `row` bytes) and its stride in bytes. Not for a frame kept on the
    /// hardware: `mapped` it first.
    pub fn plane(&self, i: usize, row: usize, rows: usize) -> anyhow::Result<(&[u8], usize)> {
        anyhow::ensure!(self.pixel_buffer().is_none(), "plane {i}: frame not mapped");
        // SAFETY: the frame owns (or maps) its planes for as long as it lives; FFmpeg and
        // CVPixelBuffer planes are `stride × rows` bytes, and the last row is read only to `row`.
        unsafe {
            let p = self.frame.as_ptr();
            let (data, linesize) = ((*p).data[i], (*p).linesize[i]);
            // A negative linesize (bottom-up planes) would read before `data`.
            let stride = usize::try_from(linesize).ok().filter(|s| *s >= row && !data.is_null() && rows > 0);
            let stride = stride.with_context(|| format!("plane {i}: unexpected layout (stride {linesize})"))?;
            Ok((std::slice::from_raw_parts(data, stride * (rows - 1) + row), stride))
        }
    }
}

/// The best video stream, plus (given a tap) the best audio stream, decoded from the same packets.
pub(crate) struct Decoder {
    input: ff::format::context::Input,
    decoder: ff::decoder::Video,
    stream: usize,
    /// Seconds per stream tick.
    time_base: f64,
    start: i64,
    pub frame_interval: Duration,
    last_pts: Option<Duration>,
    flushing: bool,
    sws: Option<Sws>,
    audio: Option<AudioDecoder>,
    /// The file's length.
    pub duration: Duration,
    /// Trim: frames before `trim_in` are decoded but not returned, and with `trim_out` set the
    /// stream ends there. Returned pts are measured from `trim_in`.
    trim_in: Duration,
    trim_out: Option<Duration>,
    /// What plays: `[trim_in, trim_out or duration)`.
    pub length: Duration,
    /// Hardware frames in a layout the GPU can sample straight from the decoder's buffer are
    /// returned unmapped (`Frame::pixel_buffer`), for `zero_copy::import`. Off: always mapped.
    pub(crate) zero_copy: bool,
}

impl Decoder {
    /// `gpu`: decode on its device where the platform can (Windows: D3D12VA on DX12, so
    /// `zero_copy` can draw the frames); `None`, the platform's own hardware device.
    pub fn open(path: &Path, audio: Option<AudioTap>, gpu: Option<&Gpu>) -> anyhow::Result<Decoder> {
        // ffmpeg-next unwraps the path as a CString.
        ensure!(!path.as_os_str().as_encoded_bytes().contains(&0), "path contains a NUL byte");
        init();
        let input = ff::format::input(path).context("cannot open")?;
        let d = Decoder::from_input(input, audio, path, false, gpu)?;
        ensure!(d.duration > Duration::ZERO, "unknown duration");
        Ok(d)
    }

    /// Decodes `input`'s best video stream (and audio, given a tap). `live`: a capture device, which
    /// has no duration and must not queue frames for frame threads (that would add latency).
    /// `gpu`: as for `open`.
    pub fn from_input(
        input: ff::format::context::Input,
        audio: Option<AudioTap>,
        path: &Path,
        live: bool,
        gpu: Option<&Gpu>,
    ) -> anyhow::Result<Decoder> {
        let stream = input.streams().best(ff::media::Type::Video).context("no video stream")?;
        let time_base = f64::from(stream.time_base());
        let start = if stream.start_time() == ffi::AV_NOPTS_VALUE { 0 } else { stream.start_time() };
        let seconds = if live {
            0.
        } else if stream.duration() > 0 {
            stream.duration() as f64 * time_base
        } else {
            input.duration() as f64 / ffi::AV_TIME_BASE as f64
        }
        .max(0.);
        let fps = f64::from(stream.avg_frame_rate());
        let frame_interval = Duration::from_secs_f64(if fps > 0. { 1. / fps } else { 1. / 30. });
        let index = stream.index();
        let mut ctx = ff::codec::Context::from_parameters(stream.parameters())?;
        if !attach_hw(&mut ctx, gpu) {
            let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as i32);
            // SAFETY: the codec context is not opened yet; the descriptor is static (or null).
            unsafe {
                let c = ctx.as_mut_ptr();
                let desc = ffi::avcodec_descriptor_get((*c).codec_id);
                #[allow(clippy::unnecessary_cast)] // the constant's binding type differs by platform
                let intra = !desc.is_null() && (*desc).props & ffi::AV_CODEC_PROP_INTRA_ONLY as i32 != 0;
                // Software only (e.g. FFV1, DNxHR, ProRes without hardware): enough threads that a 4K
                // clip keeps up, but not every core per decoder, since several layers decode at once.
                // Intra-only codecs slice-thread well; others get frame threads too, which cost a
                // frame of memory and of latency each, so fewer. Live inputs: slices only (latency).
                // ponytail: a fixed cap per decoder; share a budget across active players if many
                // software layers still oversubscribe.
                ((*c).thread_type, (*c).thread_count) = if live || intra {
                    (ffi::FF_THREAD_SLICE, cores.min(8))
                } else {
                    (ffi::FF_THREAD_FRAME | ffi::FF_THREAD_SLICE, cores.min(4))
                };
            }
        }
        let decoder = ctx.decoder().video().context("unsupported video codec")?;
        let audio = audio.and_then(|tap| AudioDecoder::open(&input, tap, path));
        Ok(Decoder {
            input,
            decoder,
            stream: index,
            time_base,
            start,
            frame_interval,
            last_pts: None,
            flushing: false,
            sws: None,
            audio,
            duration: Duration::from_secs_f64(seconds),
            trim_in: Duration::ZERO,
            trim_out: None,
            length: Duration::from_secs_f64(seconds),
            zero_copy: false,
        })
    }

    /// Plays only `[in_ms, out_ms)` of the file (`out_ms` none, or past the end: to the end), and
    /// seeks to `in_ms`. Audio outside it is dropped too.
    pub fn trimmed(mut self, in_ms: u64, out_ms: Option<u64>) -> anyhow::Result<Decoder> {
        let start = Duration::from_millis(in_ms);
        let out = out_ms.map(Duration::from_millis).filter(|&o| o < self.duration);
        let end = out.unwrap_or(self.duration);
        ensure!(start < end, "trim: in point {in_ms} ms is not before the out point/end ({} ms)", end.as_millis());
        (self.trim_in, self.trim_out, self.length) = (start, out, end - start);
        if let Some(audio) = &mut self.audio
            && (!start.is_zero() || out.is_some())
        {
            audio.window = Some((start, end));
        }
        if !start.is_zero() {
            self.rewind()?;
        }
        Ok(self)
    }

    /// Next frame in presentation order; `None` at the end of the stream.
    pub fn next(&mut self) -> anyhow::Result<Option<Frame>> {
        let mut hw = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut hw) {
                Ok(()) => {
                    let pts = self.pts(&hw);
                    if pts + self.frame_interval <= self.trim_in {
                        continue; // before the in point (decoded from the keyframe before it)
                    }
                    if self.trim_out.is_some_and(|out| pts >= out) {
                        return Ok(None);
                    }
                    return self.convert(hw, pts.saturating_sub(self.trim_in)).map(Some);
                }
                Err(ff::Error::Eof) => return Ok(None),
                Err(ff::Error::Other { errno: ff::util::error::EAGAIN }) if !self.flushing => self.feed()?,
                Err(e) => return Err(e).context("decode"),
            }
        }
    }

    /// Back to the first frame, or the keyframe at or before the in point (Loop wrap: audio
    /// continues without a gap).
    pub fn rewind(&mut self) -> anyhow::Result<()> {
        if self.trim_in.is_zero() {
            self.input.seek(0, ..).context("seek")?;
        } else {
            let ts = (self.trim_in.as_secs_f64() * ffi::AV_TIME_BASE as f64) as i64;
            self.input.seek(ts, ..ts).context("seek to the in point")?;
        }
        self.decoder.flush();
        if let Some(audio) = &mut self.audio {
            audio.rewind();
        }
        (self.flushing, self.last_pts) = (false, None);
        Ok(())
    }

    /// Loop wrap: `rewind`, the next pass starting at `offset` on the player's timeline (its audio
    /// too, however long the last pass's audio was).
    pub fn wrap(&mut self, offset: Duration) -> anyhow::Result<()> {
        if let Some(audio) = &self.audio {
            audio.wrap(offset);
        }
        self.rewind()
    }

    /// Back to the first frame for the player's seek `epoch`: audio decoded before it is dropped.
    pub fn seek(&mut self, epoch: u64) -> anyhow::Result<()> {
        if let Some(audio) = &mut self.audio {
            audio.seek(epoch);
        }
        self.rewind()
    }

    /// Sends the decoder its next packet, or end-of-stream.
    fn feed(&mut self) -> anyhow::Result<()> {
        let mut packet = ff::Packet::empty();
        loop {
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => return Ok(self.decoder.send_packet(&packet)?),
                Ok(()) => {
                    if let Some(audio) = &mut self.audio {
                        audio.packet(&packet); // errors go to the tap; the video plays on
                    }
                }
                Err(ff::Error::Eof) => {
                    self.flushing = true;
                    if let Some(audio) = &mut self.audio {
                        audio.eof();
                    }
                    return Ok(self.decoder.send_eof()?);
                }
                Err(e) => return Err(e).context("read"),
            }
        }
    }

    /// `decoded`'s time in the file (a frame without one follows the last).
    fn pts(&mut self, decoded: &ff::frame::Video) -> Duration {
        let pts = match decoded.timestamp() {
            Some(ts) => Duration::from_secs_f64(((ts - self.start) as f64 * self.time_base).max(0.)),
            None => self.last_pts.map_or(Duration::ZERO, |p| p + self.frame_interval),
        };
        self.last_pts = Some(pts);
        pts
    }

    fn convert(&mut self, decoded: ff::frame::Video, pts: Duration) -> anyhow::Result<Frame> {
        // SAFETY: plain field reads/writes on live frames; `map_hw` only runs on hardware frames.
        unsafe {
            use ffi::AVPixelFormat::*;
            let format = (*decoded.as_ptr()).format;
            let hardware = is_hw(format);
            let zero_copy_kind = |f: i32| match f {
                f if f == AV_PIX_FMT_NV12 as i32 => Some(PixelFormatKind::Nv12),
                f if f == AV_PIX_FMT_P010LE as i32 => Some(PixelFormatKind::P010),
                f if f == AV_PIX_FMT_P210LE as i32 => Some(PixelFormatKind::P210),
                f if f == AV_PIX_FMT_AYUV64LE as i32 => Some(PixelFormatKind::Ayuv64),
                _ => None,
            };
            // VideoToolbox frames, and D3D12VA ones (made on the engine's own device): kept as they
            // are for `zero_copy::import`. D3D11VA frames are always mapped.
            if self.zero_copy && (format == AV_PIX_FMT_VIDEOTOOLBOX as i32 || format == AV_PIX_FMT_D3D12 as i32) {
                let p = decoded.as_ptr();
                let frames = (*p).hw_frames_ctx;
                if let Some(kind) = (!frames.is_null()).then(|| zero_copy_kind((*((*frames).data as *const ffi::AVHWFramesContext)).sw_format as i32)).flatten() {
                    let full_range = (*p).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG;
                    return Ok(Frame { pts, kind, hardware, full_range, planar: None, frame: decoded });
                }
            }
            let mut frame = if hardware { map_hw(&decoded)? } else { decoded };
            let p = frame.as_ptr();
            let mut full_range = (*p).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG;
            // Software planar YUV: uploaded as the YUV shader's planes (U and V interleaved by
            // staging, on the decode thread) instead of an RGBA conversion here.
            let planar = match (*p).format {
                f if f == AV_PIX_FMT_YUV420P as i32 || f == AV_PIX_FMT_YUVJ420P as i32 => Some((PixelFormatKind::Nv12, 0)),
                f if f == AV_PIX_FMT_YUV422P as i32 || f == AV_PIX_FMT_YUVJ422P as i32 => Some((PixelFormatKind::Nv16, 0)),
                f if f == AV_PIX_FMT_YUV444P as i32 || f == AV_PIX_FMT_YUVJ444P as i32 => Some((PixelFormatKind::Nv24, 0)),
                f if f == AV_PIX_FMT_YUV420P10LE as i32 => Some((PixelFormatKind::P010, 6)),
                f if f == AV_PIX_FMT_YUV422P10LE as i32 => Some((PixelFormatKind::P210, 6)),
                f if f == AV_PIX_FMT_YUV444P10LE as i32 => Some((PixelFormatKind::P410, 6)),
                _ => None,
            };
            if let Some((kind, shift)) = planar {
                let jpeg = [AV_PIX_FMT_YUVJ420P, AV_PIX_FMT_YUVJ422P, AV_PIX_FMT_YUVJ444P].iter().any(|j| *j as i32 == (*p).format);
                full_range |= jpeg; // the deprecated "j" formats are full range by definition
                return Ok(Frame { pts, kind, hardware, full_range, planar: Some(shift), frame });
            }
            let kind = match (*p).format {
                f if f == AV_PIX_FMT_NV12 as i32 => PixelFormatKind::Nv12,
                f if f == AV_PIX_FMT_P010LE as i32 => PixelFormatKind::P010,
                f if f == AV_PIX_FMT_P210LE as i32 => PixelFormatKind::P210,
                f if f == AV_PIX_FMT_AYUV64LE as i32 => PixelFormatKind::Ayuv64,
                _ => {
                    frame = sws(&mut self.sws, &frame, ff::format::Pixel::RGBA, (frame.width(), frame.height()), full_range)?;
                    PixelFormatKind::Rgba
                }
            };
            Ok(Frame { pts, kind, hardware, full_range, planar: None, frame })
        }
    }
}

/// `f` converted by `sws_scale` to `out` at `size` (straight alpha kept) with BT.709 coefficients,
/// the YUV shader path's matrix, so both paths match. The software fallback (RGBA8) and probe
/// thumbnails (RGB24) use it; `cache` keeps the context while the conversion stays the same.
pub(crate) fn sws(
    cache: &mut Option<Sws>,
    f: &ff::frame::Video,
    out: ff::format::Pixel,
    size: (u32, u32),
    full_range: bool,
) -> anyhow::Result<ff::frame::Video> {
    // SAFETY: `f` is a decoded frame (valid format, planes, strides); `frame` is allocated by
    // FFmpeg at the size the context converts to.
    unsafe {
        let p = f.as_ptr();
        let to: ffi::AVPixelFormat = out.into();
        let key = ((*p).format, (*p).width, (*p).height, full_range, to as i32, size.0 as i32, size.1 as i32);
        if cache.as_ref().is_none_or(|s| s.key != key) {
            *cache = None;
            let from = pix_fmt(key.0).with_context(|| format!("unknown pixel format {}", key.0))?;
            let flags = ffi::SwsFlags::SWS_BILINEAR as i32;
            let ctx = ffi::sws_getContext(key.1, key.2, from, key.5, key.6, to, flags, ptr::null_mut(), ptr::null_mut(), ptr::null());
            ensure!(!ctx.is_null(), "no software conversion from pixel format {}", pix_name(from));
            let bt709 = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
            ffi::sws_setColorspaceDetails(ctx, bt709, full_range as i32, bt709, 1, 0, 1 << 16, 1 << 16);
            *cache = Some(Sws { ctx, key });
        }
        let ctx = cache.as_ref().map_or(ptr::null_mut(), |s| s.ctx);
        let mut frame = ff::frame::Video::new(out, size.0, size.1);
        let o = frame.as_mut_ptr();
        let src = (*p).data.as_ptr() as *const *const u8;
        let rows = ffi::sws_scale(ctx, src, (*p).linesize.as_ptr(), 0, key.2, (*o).data.as_ptr(), (*o).linesize.as_ptr());
        ensure!(rows > 0, "software conversion to {out:?} failed");
        Ok(frame)
    }
}

pub(crate) struct Sws {
    ctx: *mut ffi::SwsContext,
    /// Source format, width, height, full range; output format, width, height.
    key: (i32, i32, i32, bool, i32, i32, i32),
}

impl Drop for Sws {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from sws_getContext and is freed once.
        unsafe { ffi::sws_freeContext(self.ctx) }
    }
}

pub(crate) fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ff::init();
        // SAFETY: sets a global int.
        unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING) };
    });
}

/// The platform's hardware decoders, preferred first: device type, and the pixel format of its
/// frames.
#[cfg(target_os = "macos")]
const HW: &[(ffi::AVHWDeviceType, ffi::AVPixelFormat)] =
    &[(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX)];
// D3D12VA only on the engine's own DX12 device (`attach_hw`); D3D11VA frames are mapped to the CPU.
// ponytail: untested on Windows (no machine here); map falls back to transfer if D3D11 can't map.
#[cfg(target_os = "windows")]
const HW: &[(ffi::AVHWDeviceType, ffi::AVPixelFormat)] = &[
    (ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA, ffi::AVPixelFormat::AV_PIX_FMT_D3D12),
    (ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, ffi::AVPixelFormat::AV_PIX_FMT_D3D11),
];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const HW: &[(ffi::AVHWDeviceType, ffi::AVPixelFormat)] = &[];

/// `format` is a hardware decoder's (`HW`): the frame's planes are on the GPU.
fn is_hw(format: i32) -> bool {
    HW.iter().any(|(_, f)| *f as i32 == format)
}

/// Gives the decoder the first of the platform's hardware devices its codec has a decoder for; it
/// is used only for sizes and profiles the hardware supports, else the stream decodes in software.
/// D3D12VA only on `gpu`'s own device, where `zero_copy::d3d12va` allows (its frames are textures
/// the engine draws directly), else the next. Returns whether a device was attached (false:
/// software for sure).
// ponytail: a hardware-capable codec the hardware refuses at runtime decodes single-threaded;
// threads can't be added once the decoder is open.
fn attach_hw(ctx: &mut ff::codec::Context, gpu: Option<&Gpu>) -> bool {
    // SAFETY: the codec context is not opened yet; it takes ownership of the device reference.
    // avcodec_get_hw_config returns static configs, null past the last.
    unsafe {
        let c = ctx.as_mut_ptr();
        let codec = ffi::avcodec_find_decoder((*c).codec_id);
        let has_hw = |device| {
            !codec.is_null()
                && (0..).map(|i| ffi::avcodec_get_hw_config(codec, i)).take_while(|cfg| !cfg.is_null()).any(|cfg| {
                    (*cfg).device_type == device && (*cfg).methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
                })
        };
        for &(device, _) in HW.iter().filter(|(d, _)| has_hw(*d)) {
            let mut dev = ptr::null_mut();
            if device == ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA {
                dev = gpu.filter(|g| crate::zero_copy::d3d12va(g)).map_or(ptr::null_mut(), crate::zero_copy::d3d12_device);
            } else if ffi::av_hwdevice_ctx_create(&mut dev, device, ptr::null(), ptr::null_mut(), 0) < 0 {
                dev = ptr::null_mut();
            }
            if !dev.is_null() {
                (*c).hw_device_ctx = dev;
                (*c).get_format = Some(pick_hw);
                return true;
            }
        }
        false
    }
}

/// `f` as an AVPixelFormat, if it is one (transmuting any other i32 into the enum is UB).
pub(crate) fn pix_fmt(f: i32) -> Option<ffi::AVPixelFormat> {
    // SAFETY: the enum's values are contiguous from AV_PIX_FMT_NONE (-1) to AV_PIX_FMT_NB.
    (-1..=ffi::AVPixelFormat::AV_PIX_FMT_NB as i32).contains(&f).then(|| unsafe { std::mem::transmute::<i32, ffi::AVPixelFormat>(f) })
}

/// The pixel format of the device `attach_hw` gave the decoder.
unsafe extern "C" fn pick_hw(c: *mut ffi::AVCodecContext, formats: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a list terminated by AV_PIX_FMT_NONE; `pick_hw` is only set together
    // with `hw_device_ctx`, a live device context.
    unsafe {
        let device = (*((*(*c).hw_device_ctx).data as *const ffi::AVHWDeviceContext)).type_;
        let hw = HW.iter().find(|(d, _)| *d == device).map(|(_, f)| *f);
        let mut f = formats;
        while *f != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if Some(*f) == hw {
                return *f;
            }
            f = f.add(1);
        }
        ffi::avcodec_default_get_format(c, formats)
    }
}

/// Hardware frame → CPU-addressable frame, mapped in place (no copy) where the platform allows.
unsafe fn map_hw(hw: &ff::frame::Video) -> anyhow::Result<ff::frame::Video> {
    unsafe {
        let src = hw.as_ptr();
        let mut sw = ff::frame::Video::empty();
        let dst = sw.as_mut_ptr();
        ensure!(!(*src).hw_frames_ctx.is_null(), "hardware frame without a frames context");
        let frames = (*(*src).hw_frames_ctx).data as *const ffi::AVHWFramesContext;
        (*dst).format = (*frames).sw_format as i32;
        if ffi::av_hwframe_map(dst, src, ffi::AV_HWFRAME_MAP_READ as i32) < 0 {
            let r = ffi::av_hwframe_transfer_data(dst, src, 0);
            ensure!(r >= 0, "hardware frame download failed: {}", ff::Error::from(r));
        }
        // Only the hardware frame carries these.
        (*dst).color_range = (*src).color_range;
        (*dst).colorspace = (*src).colorspace;
        Ok(sw)
    }
}

fn pix_name(format: ffi::AVPixelFormat) -> String {
    // SAFETY: returns a static string or null.
    let name = unsafe { ffi::av_get_pix_fmt_name(format) };
    if name.is_null() { format!("{format:?}") } else { unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn pts_ms(d: &mut Decoder) -> Vec<u64> {
        std::iter::from_fn(|| d.next().unwrap()).map(|f| f.pts.as_millis() as u64).collect()
    }

    /// 1 s, 30 fps: trimmed to [300, 700) it plays 12 frames, timed from the in point, and a
    /// rewind starts at the in point again.
    #[test]
    fn trim_plays_only_the_window() {
        let mut d = Decoder::open(&fixture("h264_64.mp4"), None, None).unwrap().trimmed(300, Some(700)).unwrap();
        assert_eq!(d.length, Duration::from_millis(400));
        let first = pts_ms(&mut d);
        assert_eq!(first.len(), 12, "{first:?}");
        assert!(first[0] <= 1 && *first.last().unwrap() < 400, "{first:?}");
        d.rewind().unwrap();
        assert_eq!(pts_ms(&mut d), first, "the next pass is the same window");
        // No out point: to the file's end.
        let mut tail = Decoder::open(&fixture("h264_64.mp4"), None, None).unwrap().trimmed(900, None).unwrap();
        assert_eq!((tail.length, pts_ms(&mut tail).len()), (Duration::from_millis(100), 3));
        for (a, b) in [(700, Some(300)), (1000, None), (0, Some(0))] {
            assert!(Decoder::open(&fixture("h264_64.mp4"), None, None).unwrap().trimmed(a, b).is_err(), "{a} {b:?}");
        }
    }

    /// The trimmed audio is the window's length, not the file's (nor from the keyframe before it).
    #[test]
    fn trim_cuts_audio_to_the_window() {
        let tap = AudioTap::new((48_000, 2));
        let mut d = Decoder::open(&fixture("h264_64.mp4"), Some(tap.clone()), None).unwrap().trimmed(300, Some(700)).unwrap();
        while d.next().unwrap().is_some() {}
        let seconds = tap.buffered() as f64 / (48_000. * 2.);
        assert!((seconds - 0.4).abs() < 0.05, "{seconds} s of audio for a 0.4 s window");
    }
}
