//! Media metadata and thumbnails for the controller's media list (`ToEngine::Probe`, R7).

use std::path::Path;

use anyhow::{Context, ensure};
use ff::ffi;
use ffmpeg_next as ff;
use image::RgbImage;
use tessera_show::{MediaKind, ProbeInfo};
use uuid::Uuid;

use crate::video_decode::{init, sws};

/// Thumbnail width; the height keeps the aspect ratio.
const THUMB_WIDTH: u32 = 160;

/// What `path` is, and a `THUMB_WIDTH`-wide JPEG thumbnail written to `<thumb_dir>/<media_id>.jpg`
/// (`thumb` is set when it was). Stills are what the player loads with the `image` crate (by
/// extension); anything else must be a video FFmpeg decodes, thumbnailed at 10 % of its duration.
/// Blocking (decodes in software): run it off the render thread.
pub fn probe(path: &Path, thumb_dir: &Path, media_id: Uuid) -> anyhow::Result<ProbeInfo> {
    // ffmpeg-next unwraps the path as a CString.
    ensure!(!path.as_os_str().as_encoded_bytes().contains(&0), "{}: path contains a NUL byte", path.display());
    let (mut info, thumb) = match image::ImageFormat::from_path(path) {
        Ok(format) if format.reading_enabled() => still(path),
        _ => video(path),
    }
    .with_context(|| path.display().to_string())?;
    // The metadata stands without it: the item is still usable, just without a picture.
    let name = format!("{media_id}.jpg");
    match save_jpeg(&thumb, thumb_dir, &name) {
        Ok(()) => info.thumb = Some(name),
        Err(e) => log!("probe {}: no thumbnail in {}: {e:#}", path.display(), thumb_dir.display()),
    }
    Ok(info)
}

fn save_jpeg(image: &RgbImage, dir: &Path, name: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    Ok(image.save_with_format(dir.join(name), image::ImageFormat::Jpeg)?)
}

fn still(path: &Path) -> anyhow::Result<(ProbeInfo, RgbImage)> {
    let image = image::open(path).context("cannot open")?;
    let (width, height, has_alpha) = (image.width(), image.height(), image.color().has_alpha());
    let thumb = image.resize(THUMB_WIDTH, u32::MAX, image::imageops::FilterType::Triangle).into_rgb8();
    let info = ProbeInfo { kind: MediaKind::Image, width, height, duration_ms: None, has_audio: false, has_alpha, thumb: None };
    Ok((info, thumb))
}

fn video(path: &Path) -> anyhow::Result<(ProbeInfo, RgbImage)> {
    init();
    let mut input = ff::format::input(path).context("cannot open")?;
    let stream = input.streams().best(ff::media::Type::Video).context("no video stream")?;
    let (index, time_base) = (stream.index(), f64::from(stream.time_base()));
    let start = if stream.start_time() == ffi::AV_NOPTS_VALUE { 0 } else { stream.start_time() };
    // As the player reads it (`Decoder::open`), which can't play a file without one.
    let seconds = if stream.duration() > 0 {
        stream.duration() as f64 * time_base
    } else {
        input.duration() as f64 / ffi::AV_TIME_BASE as f64
    };
    ensure!(seconds > 0., "unknown duration");
    let context = ff::codec::Context::from_parameters(stream.parameters())?;
    let mut decoder = context.decoder().video().context("unsupported video codec")?;
    let has_audio = input.streams().best(ff::media::Type::Audio).is_some();
    // From the keyframe at or before 10 % in (if the file seeks), to the first frame at 10 %.
    let at = seconds / 10.;
    let _ = input.seek((at * ffi::AV_TIME_BASE as f64) as i64, ..(at * ffi::AV_TIME_BASE as f64) as i64);
    let frame = frame_at(&mut input, &mut decoder, index, start + (at / time_base) as i64)?;
    let (w, h) = (frame.width(), frame.height());
    ensure!(w > 0 && h > 0, "{w}x{h} frame");
    // SAFETY: plain field reads; the format came from FFmpeg, so it is a valid AVPixelFormat, and
    // its descriptor is static (or null).
    let (has_alpha, full_range) = unsafe {
        let p = frame.as_ptr();
        let desc = ffi::av_pix_fmt_desc_get(std::mem::transmute::<i32, ffi::AVPixelFormat>((*p).format));
        let alpha = !desc.is_null() && (*desc).flags & ffi::AV_PIX_FMT_FLAG_ALPHA as u64 != 0;
        (alpha, (*p).color_range == ffi::AVColorRange::AVCOL_RANGE_JPEG)
    };
    let size = (THUMB_WIDTH, ((THUMB_WIDTH as u64 * h as u64 + w as u64 / 2) / w as u64).max(1) as u32);
    let rgb = sws(&mut None, &frame, ff::format::Pixel::RGB24, size, full_range)?;
    let row = size.0 as usize * 3;
    let pixels = rgb.data(0).chunks(rgb.stride(0)).take(size.1 as usize).flat_map(|r| &r[..row]).copied().collect();
    let thumb = RgbImage::from_raw(size.0, size.1, pixels).context("thumbnail size")?;
    let duration_ms = Some((seconds * 1000.).round() as u64);
    let info = ProbeInfo { kind: MediaKind::Video, width: decoder.width(), height: decoder.height(), duration_ms, has_audio, has_alpha, thumb: None };
    Ok((info, thumb))
}

/// The first frame of `stream` at or after `target` (stream time base), else its last frame.
fn frame_at(input: &mut ff::format::context::Input, decoder: &mut ff::decoder::Video, stream: usize, target: i64) -> anyhow::Result<ff::frame::Video> {
    let (mut next, mut last, mut eof) = (ff::frame::Video::empty(), None, false);
    let mut packets = input.packets();
    loop {
        // A failed receive unrefs the frame it was given, so each decoded frame is moved out.
        while decoder.receive_frame(&mut next).is_ok() {
            let frame = std::mem::replace(&mut next, ff::frame::Video::empty());
            if frame.timestamp().is_none_or(|t| t >= target) {
                return Ok(frame);
            }
            last = Some(frame);
        }
        if eof {
            return last.context("no video frames");
        }
        match packets.next() {
            Some((s, packet)) if s.index() == stream => decoder.send_packet(&packet).context("decode")?,
            Some(_) => {}
            None => {
                eof = true;
                let _ = decoder.send_eof();
            }
        }
    }
}
