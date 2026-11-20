//! FFmpeg decode of one video stream: hardware first (VideoToolbox / D3D11VA), frames made
//! CPU-addressable in place with `av_hwframe_map` (R20) for the player to upload.

use std::path::Path;
use std::ptr;
use std::time::Duration;

use anyhow::{Context, ensure};
use ffmpeg_next as ff;
use ff::ffi;

/// How a frame's planes reach the GPU (see `player::layout`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormatKind {
    /// 8-bit 4:2:0, Y plane + interleaved CbCr plane.
    Nv12,
    /// 10-bit 4:2:0 like NV12, MSB-aligned in 16 bits.
    P010,
    /// Packed 16-bit A, Y, Cb, Cr: VideoToolbox's ProRes 4444 with alpha (`y416`).
    Ayuv64,
    /// Straight-alpha RGBA8, converted by `sws_scale` (software fallback for everything else).
    Rgba,
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
    let mut decoder = Decoder::open(path)?;
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
    frame: ff::frame::Video,
}

impl Frame {
    pub fn size(&self) -> (u32, u32) {
        (self.frame.width(), self.frame.height())
    }

    /// Plane `i` (`rows` rows of `row` bytes) and its stride in bytes.
    pub fn plane(&self, i: usize, row: usize, rows: usize) -> anyhow::Result<(&[u8], usize)> {
        // SAFETY: the frame owns (or maps) its planes for as long as it lives; FFmpeg and
        // CVPixelBuffer planes are `stride × rows` bytes, and the last row is read only to `row`.
        unsafe {
            let p = self.frame.as_ptr();
            let (data, stride) = ((*p).data[i], (*p).linesize[i]);
            ensure!(!data.is_null() && rows > 0 && stride as usize >= row, "plane {i}: unexpected layout (stride {stride})");
            let stride = stride as usize;
            Ok((std::slice::from_raw_parts(data, stride * (rows - 1) + row), stride))
        }
    }
}

/// Opens the best video stream only; audio (Task 8) gets its own stream decoder alongside.
pub(crate) struct Decoder {
    input: ff::format::context::Input,
    decoder: ff::decoder::Video,
    stream: usize,
    /// Seconds per stream tick.
    time_base: f64,
    start: i64,
    frame_interval: Duration,
    last_pts: Option<Duration>,
    flushing: bool,
    sws: Option<Sws>,
    pub duration: Duration,
}

impl Decoder {
    pub fn open(path: &Path) -> anyhow::Result<Decoder> {
        init();
        let input = ff::format::input(path).with_context(|| format!("cannot open {}", path.display()))?;
        let stream = input.streams().best(ff::media::Type::Video).context("no video stream")?;
        let time_base = f64::from(stream.time_base());
        let start = if stream.start_time() == ffi::AV_NOPTS_VALUE { 0 } else { stream.start_time() };
        let seconds = if stream.duration() > 0 {
            stream.duration() as f64 * time_base
        } else {
            input.duration() as f64 / ffi::AV_TIME_BASE as f64
        };
        ensure!(seconds > 0., "{}: unknown duration", path.display());
        let fps = f64::from(stream.avg_frame_rate());
        let frame_interval = Duration::from_secs_f64(if fps > 0. { 1. / fps } else { 1. / 30. });
        let index = stream.index();
        let mut ctx = ff::codec::Context::from_parameters(stream.parameters())?;
        attach_hw(&mut ctx);
        let decoder = ctx.decoder().video().with_context(|| format!("{}: unsupported video codec", path.display()))?;
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
            duration: Duration::from_secs_f64(seconds),
        })
    }

    /// Next frame in presentation order; `None` at the end of the stream.
    pub fn next(&mut self) -> anyhow::Result<Option<Frame>> {
        let mut hw = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut hw) {
                Ok(()) => return self.convert(hw).map(Some),
                Err(ff::Error::Eof) => return Ok(None),
                Err(ff::Error::Other { errno: ff::util::error::EAGAIN }) if !self.flushing => self.feed()?,
                Err(e) => return Err(e).context("decode"),
            }
        }
    }

    /// Back to the first frame.
    pub fn rewind(&mut self) -> anyhow::Result<()> {
        self.input.seek(0, ..).context("seek")?;
        self.decoder.flush();
        (self.flushing, self.last_pts) = (false, None);
        Ok(())
    }

    /// Sends the decoder its next packet, or end-of-stream.
    fn feed(&mut self) -> anyhow::Result<()> {
        let mut packet = ff::Packet::empty();
        loop {
            match packet.read(&mut self.input) {
                Ok(()) if packet.stream() == self.stream => return Ok(self.decoder.send_packet(&packet)?),
                Ok(()) => {} // other streams (audio: Task 8)
                Err(ff::Error::Eof) => {
                    self.flushing = true;
                    return Ok(self.decoder.send_eof()?);
                }
                Err(e) => return Err(e).context("read"),
            }
        }
    }

    fn convert(&mut self, decoded: ff::frame::Video) -> anyhow::Result<Frame> {
        let pts = match decoded.timestamp() {
            Some(ts) => Duration::from_secs_f64(((ts - self.start) as f64 * self.time_base).max(0.)),
            None => self.last_pts.map_or(Duration::ZERO, |p| p + self.frame_interval),
        };
        self.last_pts = Some(pts);
        // SAFETY: plain field reads/writes on live frames; `map_hw` only runs on hardware frames.
        unsafe {
            let hardware = HW.is_some_and(|(_, f)| (*decoded.as_ptr()).format == f as i32);
            let mut frame = if hardware { map_hw(&decoded)? } else { decoded };
            let p = frame.as_ptr();
            let full_range = (*p).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG;
            use ffi::AVPixelFormat::*;
            let kind = match (*p).format {
                f if f == AV_PIX_FMT_NV12 as i32 => PixelFormatKind::Nv12,
                f if f == AV_PIX_FMT_P010LE as i32 => PixelFormatKind::P010,
                f if f == AV_PIX_FMT_AYUV64LE as i32 => PixelFormatKind::Ayuv64,
                _ => {
                    frame = self.sws_rgba(&frame, full_range)?;
                    PixelFormatKind::Rgba
                }
            };
            Ok(Frame { pts, kind, hardware, full_range, frame })
        }
    }

    /// Software fallback: `sws_scale` to straight-alpha RGBA8 with BT.709 coefficients (the YUV
    /// shader path's matrix, so both paths match).
    fn sws_rgba(&mut self, f: &ff::frame::Video, full_range: bool) -> anyhow::Result<ff::frame::Video> {
        // SAFETY: `f` is a decoded frame (valid format, planes, strides); `out` is allocated by
        // FFmpeg at the size the context converts to.
        unsafe {
            let p = f.as_ptr();
            let key = ((*p).format, (*p).width, (*p).height, full_range);
            if self.sws.as_ref().is_none_or(|s| s.key != key) {
                self.sws = None;
                // The format came from FFmpeg, so it is a valid AVPixelFormat.
                let format = std::mem::transmute::<i32, ffi::AVPixelFormat>(key.0);
                let rgba = ffi::AVPixelFormat::AV_PIX_FMT_RGBA;
                let flags = ffi::SwsFlags::SWS_BILINEAR as i32;
                let ctx = ffi::sws_getContext(key.1, key.2, format, key.1, key.2, rgba, flags, ptr::null_mut(), ptr::null_mut(), ptr::null());
                ensure!(!ctx.is_null(), "no software conversion from pixel format {}", pix_name(format));
                let bt709 = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
                ffi::sws_setColorspaceDetails(ctx, bt709, full_range as i32, bt709, 1, 0, 1 << 16, 1 << 16);
                self.sws = Some(Sws { ctx, key });
            }
            let ctx = self.sws.as_ref().map_or(ptr::null_mut(), |s| s.ctx);
            let mut out = ff::frame::Video::new(ff::format::Pixel::RGBA, key.1 as u32, key.2 as u32);
            let o = out.as_mut_ptr();
            let src = (*p).data.as_ptr() as *const *const u8;
            let rows = ffi::sws_scale(ctx, src, (*p).linesize.as_ptr(), 0, key.2, (*o).data.as_ptr(), (*o).linesize.as_ptr());
            ensure!(rows > 0, "software conversion to RGBA failed");
            Ok(out)
        }
    }
}

struct Sws {
    ctx: *mut ffi::SwsContext,
    /// Source format, width, height, full range.
    key: (i32, i32, i32, bool),
}

impl Drop for Sws {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from sws_getContext and is freed once.
        unsafe { ffi::sws_freeContext(self.ctx) }
    }
}

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ff::init();
        // SAFETY: sets a global int.
        unsafe { ffi::av_log_set_level(ffi::AV_LOG_WARNING) };
    });
}

#[cfg(target_os = "macos")]
const HW: Option<(ffi::AVHWDeviceType, ffi::AVPixelFormat)> =
    Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX));
// ponytail: untested on Windows (no machine here); map falls back to transfer if D3D11 can't map.
#[cfg(target_os = "windows")]
const HW: Option<(ffi::AVHWDeviceType, ffi::AVPixelFormat)> =
    Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, ffi::AVPixelFormat::AV_PIX_FMT_D3D11));
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const HW: Option<(ffi::AVHWDeviceType, ffi::AVPixelFormat)> = None;

/// Gives the decoder the platform's hardware device; it uses it only for codecs (and sizes) the
/// hardware supports and silently decodes in software otherwise.
// ponytail: software decode runs single-threaded; set thread_count for software-only streams if
// a 4K software codec (no hardware support) has to play.
fn attach_hw(ctx: &mut ff::codec::Context) {
    let Some((device, _)) = HW else { return };
    // SAFETY: the codec context is not opened yet; it takes ownership of the device reference.
    unsafe {
        let mut dev = ptr::null_mut();
        if ffi::av_hwdevice_ctx_create(&mut dev, device, ptr::null(), ptr::null_mut(), 0) < 0 {
            return;
        }
        let c = ctx.as_mut_ptr();
        (*c).hw_device_ctx = dev;
        (*c).get_format = Some(pick_hw);
    }
}

unsafe extern "C" fn pick_hw(c: *mut ffi::AVCodecContext, formats: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let mut f = formats;
        while *f != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if HW.is_some_and(|(_, hw)| *f == hw) {
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
