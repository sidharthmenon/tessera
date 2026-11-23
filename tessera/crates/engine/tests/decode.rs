use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tessera_engine::audio::AudioTap;
use tessera_engine::compositor::{Compositor, LayerTexture, ScreenSlot};
use tessera_engine::gpu::{Gpu, read_rgba};
use tessera_engine::player::{ImageSource, Player};
use tessera_engine::video_decode::{PixelFormatKind, decode_all_frames};
use tessera_show::{Layer, Playback, Rect, Screen, Source};
use uuid::Uuid;

const PERIOD: Duration = Duration::from_micros(16_667);
const MISSING: &str = "/nonexistent/clip.mp4";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A device for the calling test, which from here on has the GPU to itself until its thread ends:
/// each device reserves its own blocks of GPU memory (hundreds of MB once it uploads frames), and a
/// dozen side by side ran a 1 GB integrated GPU out of memory. Not one device for all: the engine
/// records and submits from one thread, and tests running at once would interleave their submits.
fn gpu() -> Gpu {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    thread_local! {
        static TURN: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    }
    TURN.with_borrow_mut(|turn| {
        if turn.is_none() {
            *turn = Some(ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner));
        }
    });
    Gpu::new_headless().expect("no GPU adapter: these tests need a real GPU (headless Metal on macOS)")
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// Composites `tex` full-screen onto a 64×64 screen and reads it back (premultiplied RGBA8).
fn render(gpu: &Gpu, tex: LayerTexture) -> Vec<u8> {
    let screen = Screen { id: Uuid::new_v4(), name: "S".into(), width: 64, height: 64, layout_x: 0, layout_y: 0 };
    let layer = Layer::new(Source::Media { media_id: Uuid::new_v4() }, Rect { x: 0., y: 0., w: 64., h: 64. });
    let view = Compositor::new(gpu).render_screen(gpu, ScreenSlot::Pgm, &screen, &[(layer, Some(tex))]);
    read_rgba(gpu, view.texture()).unwrap()
}

fn px(data: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 64 + x) * 4;
    data[i..i + 4].try_into().unwrap()
}

#[track_caller]
fn assert_near(got: [u8; 4], want: [u8; 4], tol: u8) {
    let ok = got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol);
    assert!(ok, "pixel {got:?} not within ±{tol} of {want:?}");
}

/// Polls `f` (a frame, or the reason there never will be one) until it answers; 5 s at most.
fn poll<T>(mut f: impl FnMut() -> Option<Result<T, String>>) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(answer) = f() {
            return answer;
        }
        assert!(Instant::now() < deadline, "no frame and no error within 5 s");
        std::thread::sleep(ms(1));
    }
}

/// The player's frame for `now` once the decode thread delivers it, or its error.
fn frame(player: &mut Player, gpu: &Gpu, now: Instant) -> Result<LayerTexture, String> {
    poll(|| player.current_frame(gpu, now, PERIOD).map(Ok).or_else(|| player.take_error().map(Err)))
}

fn image_frame(image: &mut ImageSource) -> Result<LayerTexture, String> {
    poll(|| image.texture().map(Ok).or_else(|| image.take_error().map(Err)))
}

#[test]
fn decode_h264_frame_count() {
    let frames = decode_all_frames(&fixture("h264_64.mp4")).unwrap();
    assert!((29..=31).contains(&frames.len()), "{} frames", frames.len());
    let pts: Vec<u64> = frames.iter().map(|f| f.pts_ms).collect();
    assert!(pts.windows(2).all(|w| w[0] < w[1]), "pts not strictly increasing: {pts:?}");
}

#[test]
#[cfg(target_os = "macos")]
fn videotoolbox_decodes_fixtures() {
    for (name, kind) in [
        ("h264_64.mp4", PixelFormatKind::Nv12),
        ("prores4444_64.mov", PixelFormatKind::Ayuv64),
        ("hevc10_64.mp4", PixelFormatKind::P010),
        ("prores422hq_64.mov", PixelFormatKind::P210),
    ] {
        let frames = decode_all_frames(&fixture(name)).unwrap();
        assert!(!frames.is_empty(), "{name}: no frames");
        assert!(frames.iter().all(|f| f.hardware && f.format == kind), "{name}: {:?}", frames[0]);
    }
}

#[test]
fn prores4444_has_alpha() {
    let frames = decode_all_frames(&fixture("prores4444_64.mov")).unwrap();
    assert!(matches!(frames[0].format, PixelFormatKind::Ayuv64 | PixelFormatKind::Rgba), "{:?}", frames[0]);
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("prores4444_64.mov"), Playback::Loop, None).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    let (left, right) = (px(&data, 8, 32), px(&data, 56, 32));
    assert!(left[3] < 26, "left alpha {left:?} should be < 0.1");
    assert!(right[3] > 229, "right alpha {right:?} should be > 0.9");
    assert_near(right, [255, 0, 0, 255], 4); // red: proves the A/Y/Cb/Cr swizzle
    assert_zero_copy(&gpu, &player, frames[0].hardware);
}

/// A hardware-decoded frame is drawn from the decoder's buffer (`zero_copy`: macOS IOSurface,
/// Windows D3D12 texture where `zero_copy::d3d12va`), unless the kill switch is set: a silent
/// fallback to copying mustn't pass the format tests.
#[track_caller]
fn assert_zero_copy(gpu: &Gpu, player: &Player, hardware: bool) {
    let platform = cfg!(target_os = "macos") || tessera_engine::zero_copy::d3d12va(gpu);
    let want = hardware && platform && tessera_engine::zero_copy::enabled();
    assert_eq!(player.imported(), want, "zero-copy import (hardware decode: {hardware})");
}

/// 64×40 H.264 (coded 64×48): a hardware decoder may allocate the padded size, but only the
/// frame's rows are drawn. White, the bottom 8 rows red: the screen's bottom fifth is red.
#[test]
fn padded_frame_draws_only_its_rows() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64x40.mp4"), Playback::Loop, None).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    assert_near(px(&data, 32, 40), [255, 255, 255, 255], 8);
    assert_near(px(&data, 32, 60), [255, 0, 0, 255], 8);
    assert_near(px(&data, 32, 63), [255, 0, 0, 255], 8); // not the padding below the frame
    assert_zero_copy(&gpu, &player, decode_all_frames(&fixture("h264_64x40.mp4")).unwrap()[0].hardware);
}

/// Windows on DX12: D3D12VA frames, NV12 and P010, are drawn from the decoder's own textures, not
/// copied (the engine log says why if they are). Skipped where decoders don't use D3D12VA: other
/// backends (`WGPU_BACKEND`), zero-copy off, Intel's legacy driver line (`zero_copy::d3d12va`).
#[test]
#[cfg(windows)]
fn d3d12_frames_are_imported() {
    let gpu = gpu();
    if !tessera_engine::zero_copy::d3d12va(&gpu) {
        let info = gpu.adapter.get_info();
        eprintln!("skipped: no D3D12VA ({:?}, driver {}, zero-copy enabled: {})", info.backend, info.driver, tessera_engine::zero_copy::enabled());
        return;
    }
    for name in ["h264_64.mp4", "h264_64x40.mp4", "hevc10_64.mp4"] {
        let mut player = Player::open(&gpu, &fixture(name), Playback::Loop, None).unwrap();
        frame(&mut player, &gpu, Instant::now()).unwrap();
        assert!(player.imported(), "{name}: copied, not imported");
    }
}

/// Windows on DX12: the D3D12 debug layer (on in debug builds once the Graphics Tools optional
/// feature is installed) finds no error in drawing zero-copy NV12 and P010 frames: no resource
/// state or fence errors. Read from the device's info queue: Windows 10's layer writes to the debug
/// output, where wgpu listens, only while a debugger is attached. Skipped without the layer.
#[test]
#[cfg(windows)]
fn d3d12_debug_layer_finds_no_errors() {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::core::Interface;
    let gpu = gpu();
    // SAFETY: only the raw device is read; wgpu keeps owning it.
    let Some(queue) = (unsafe { gpu.device.as_hal::<wgpu::hal::api::Dx12>() }).and_then(|hal| hal.raw_device().cast::<ID3D12InfoQueue>().ok())
    else {
        eprintln!("skipped: no D3D12 debug layer (a debug build on DX12 with Graphics Tools installed has one)");
        return;
    };
    // Store only errors: the device (one per process) is shared with the tests running alongside,
    // whose warnings would otherwise fill the queue.
    let mut severities = [D3D12_MESSAGE_SEVERITY_CORRUPTION, D3D12_MESSAGE_SEVERITY_ERROR];
    let filter = D3D12_INFO_QUEUE_FILTER {
        AllowList: D3D12_INFO_QUEUE_FILTER_DESC { NumSeverities: 2, pSeverityList: severities.as_mut_ptr(), ..Default::default() },
        ..Default::default()
    };
    // SAFETY: `filter` and its list outlive the call, which copies them.
    unsafe { queue.PushStorageFilter(&filter) }.unwrap();
    // The device can outlive a test, its queue holding that test's messages: from here on, ours.
    unsafe { queue.ClearStoredMessages() };
    let screen = Screen { id: Uuid::new_v4(), name: "S".into(), width: 64, height: 64, layout_x: 0, layout_y: 0 };
    let mut compositor = Compositor::new(&gpu);
    // Decoding side by side, as a show's layers do; freed only once the queue is read.
    let mut players: Vec<Player> =
        ["h264_64.mp4", "h264_64x40.mp4", "hevc10_64.mp4"].map(|name| Player::open(&gpu, &fixture(name), Playback::Loop, None).unwrap()).into();
    let t0 = Instant::now();
    players.iter_mut().for_each(|p| p.play(t0));
    for t in [0, 100, 200, 300] {
        for player in &mut players {
            let tex = frame(player, &gpu, t0 + ms(t)).unwrap();
            let layer = Layer::new(Source::Media { media_id: Uuid::new_v4() }, Rect { x: 0., y: 0., w: 64., h: 64. });
            let view = compositor.render_screen(&gpu, ScreenSlot::Pgm, &screen, &[(layer, Some(tex))]);
            read_rgba(&gpu, view.texture()).unwrap();
            compositor.end_frame();
        }
    }
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    // SAFETY: plain info-queue calls; each message is read into a buffer of the size the queue asks for.
    let errors: Vec<String> = unsafe {
        (0..queue.GetNumStoredMessages())
            .filter_map(|i| {
                let mut len = 0;
                queue.GetMessage(i, None, &mut len).ok()?;
                let mut buf = vec![0u64; len.div_ceil(8)];
                let message = buf.as_mut_ptr().cast::<D3D12_MESSAGE>();
                queue.GetMessage(i, Some(message), &mut len).ok()?;
                Some(std::ffi::CStr::from_ptr((*message).pDescription.cast()).to_string_lossy().into_owned())
            })
            .collect()
    };
    unsafe {
        queue.ClearStoredMessages();
        queue.PopStorageFilter();
    }
    assert!(errors.is_empty(), "D3D12 debug layer: {errors:#?}");
}

#[test]
fn software_fallback_keeps_alpha() {
    // FFV1 has no hardware decoder: yuva420p → sws_scale → straight-alpha RGBA8.
    let frames = decode_all_frames(&fixture("ffv1_alpha_64.mkv")).unwrap();
    assert!(frames.iter().all(|f| !f.hardware && f.format == PixelFormatKind::Rgba), "{:?}", frames[0]);
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("ffv1_alpha_64.mkv"), Playback::Loop, None).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    assert!(px(&data, 8, 32)[3] < 26, "left alpha {:?}", px(&data, 8, 32));
    assert_near(px(&data, 56, 32), [255, 0, 0, 255], 4); // BT.709, limited range
    assert_zero_copy(&gpu, &player, false);
}

#[test]
fn hevc_main10_converts() {
    // Real decoded Main10 (P010, MSB-aligned) frame: white left half, 75 % grey right half.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("hevc10_64.mp4"), Playback::Loop, None).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    // ±2: scale 256 lands within 1 code value here; 255 (LSB-style scaling) is 3 off.
    assert_near(px(&data, 8, 32), [255, 255, 255, 255], 2);
    assert_near(px(&data, 56, 32), [191, 191, 191, 255], 2);
    assert_zero_copy(&gpu, &player, decode_all_frames(&fixture("hevc10_64.mp4")).unwrap()[0].hardware);
}

#[test]
fn prores422_converts() {
    // ProRes 422 HQ (VideoToolbox: p210le, 4:2:2 with a full-height CbCr plane): quadrants
    // red | blue / green | white. Wrong UV plane height or width would smear the bottom row or
    // the right column's colour.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("prores422hq_64.mov"), Playback::Loop, None).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    assert_near(px(&data, 16, 16), [255, 0, 0, 255], 4);
    assert_near(px(&data, 48, 16), [0, 0, 255, 255], 4);
    assert_near(px(&data, 16, 48), [0, 255, 0, 255], 4);
    assert_near(px(&data, 48, 48), [255, 255, 255, 255], 4);
    assert_zero_copy(&gpu, &player, decode_all_frames(&fixture("prores422hq_64.mov")).unwrap()[0].hardware);
}

#[test]
fn open_missing_file_is_err() {
    // `open` never blocks (R22), so the error arrives through `take_error`; no frame meanwhile.
    let gpu = gpu();
    let mut missing = Player::open(&gpu, Path::new(MISSING), Playback::Loop, None).unwrap();
    let error = frame(&mut missing, &gpu, Instant::now()).unwrap_err();
    assert!(error.contains(MISSING) && error.contains("cannot open"), "{error}");
    let mut not_video = Player::open(&gpu, &fixture("gen.sh"), Playback::Loop, None).unwrap();
    assert!(frame(&mut not_video, &gpu, Instant::now()).is_err(), "not a video");
    assert!(decode_all_frames(Path::new(MISSING)).is_err());
    let mut image = ImageSource::open(&gpu, Path::new("/nonexistent/still.png")).unwrap();
    assert!(image_frame(&mut image).is_err());
}

#[test]
fn nul_in_path_is_error_not_panic() {
    // MediaItem.path is unvalidated JSON: a NUL must not panic (thread name, CString), only error.
    let gpu = gpu();
    let mut player = Player::open(&gpu, Path::new("/tmp/bad\0clip.mp4"), Playback::Loop, None).unwrap();
    let error = frame(&mut player, &gpu, Instant::now()).unwrap_err();
    assert!(error.contains("/tmp/bad\0clip.mp4") && error.contains("NUL"), "{error:?}");
    let mut image = ImageSource::open(&gpu, Path::new("/tmp/bad\0still.png")).unwrap();
    let error = image_frame(&mut image).unwrap_err();
    assert!(error.contains("/tmp/bad\0still.png") && error.contains("NUL"), "{error:?}");
}

#[test]
#[cfg(unix)]
fn open_does_not_block() {
    // Opening a FIFO blocks until a writer shows up, so an open on the caller's thread hangs here.
    // (The decode thread stays blocked until the test process exits.)
    let fifo = std::env::temp_dir().join(format!("tessera-test-{}.fifo", Uuid::new_v4()));
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let (tx, rx) = std::sync::mpsc::channel();
    let (path, gpu) = (fifo.clone(), gpu());
    std::thread::spawn(move || tx.send(Player::open(&gpu, &path, Playback::Loop, None).is_ok()));
    let opened = rx.recv_timeout(Duration::from_secs(2));
    std::fs::remove_file(&fifo).unwrap();
    assert_eq!(opened, Ok(true), "Player::open blocked on opening the file");
}

#[test]
fn loop_wraps_position() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, None).unwrap();
    let t0 = Instant::now();
    frame(&mut player, &gpu, t0).unwrap(); // opened: duration known
    assert_eq!(player.position_ms(t0 + ms(700)), 0, "starts paused at 0");
    player.play(t0);
    let pos = player.position_ms(t0 + ms(1500));
    assert!(pos.abs_diff(500) <= 40, "position {pos} ms at 1.5 s of a 1 s loop");
}

#[test]
fn once_ends_transparent() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Once, None).unwrap();
    let t0 = Instant::now();
    player.play(t0);
    frame(&mut player, &gpu, t0).expect("first frame while playing");
    assert!(player.current_frame(&gpu, t0 + ms(1200), PERIOD).is_none(), "transparent after the end");
    assert_eq!(player.position_ms(t0 + ms(1200)), 1000);
}

#[test]
fn hold_last_keeps_frame() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::HoldLast, None).unwrap();
    let t0 = Instant::now();
    player.play(t0);
    frame(&mut player, &gpu, t0 + ms(1200)).expect("last frame held");
}

/// A mixer format, no device needed: players resample to it.
const AUDIO: Option<(u32, u16)> = Some((48_000, 2));

/// Waits until the decode thread has stopped adding to `tap` (blocked on its full video queue,
/// since these tests never poll frames) and returns how much it holds.
fn settled(tap: &AudioTap) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = 0;
    loop {
        std::thread::sleep(ms(100));
        let now = tap.buffered();
        if now > 0 && now == last {
            return now;
        }
        assert!(Instant::now() < deadline, "tap still filling after 5 s ({now} samples)");
        last = now;
    }
}

fn mixed(tap: &AudioTap, samples: usize) -> Vec<f32> {
    let mut out = vec![0.; samples];
    tap.mix(&mut out, 1.);
    out
}

fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|&s| s == 0.)
}

#[test]
fn audio_tap_receives_tone() {
    // The fixture's audio is ffmpeg's `sine` source: 440 Hz at amplitude 1/8, mono 44.1 kHz AAC
    // (−21.1 dB RMS per astats). Resampled to 48 kHz stereo, swresample puts mono at −3 dB in each
    // channel: RMS 0.125 × 0.707 / √2 ≈ 0.062. Silence, a wrong sample format or doubled mixing
    // misses 0.04..0.08 (measured 0.0635). The zero-crossing count proves the rate conversion:
    // 44.1 kHz samples played as 48 kHz would read 479 Hz, 48 crossings in 0.05 s instead of 44.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, AUDIO).unwrap();
    let tap = player.audio().expect("a tap when a mixer format is given");
    assert!(settled(&tap) >= 4800, "at least 0.05 s decoded ahead (measured 0.116 s)");
    player.play(Instant::now());
    let out = mixed(&tap, 4800);
    assert!(out.chunks(2).all(|f| f[0] == f[1]), "mono lands equally in both interleaved channels");
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let rms = (left.iter().map(|s| s * s).sum::<f32>() / left.len() as f32).sqrt();
    let crossings = left.windows(2).filter(|w| (w[0] < 0.) != (w[1] < 0.)).count();
    assert!((0.04..0.08).contains(&rms), "RMS {rms}");
    assert!(crossings.abs_diff(44) <= 2, "{crossings} zero crossings in 0.05 s; 440 Hz has 44");
}

#[test]
fn paused_tap_not_consumed() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, AUDIO).unwrap();
    let tap = player.audio().unwrap();
    let held = settled(&tap); // decoded while paused (a player starts paused)
    assert!(silent(&mixed(&tap, 4800)), "paused: silent");
    assert_eq!(tap.buffered(), held, "paused: nothing consumed");
    player.play(Instant::now());
    assert!(!silent(&mixed(&tap, 4800)), "playing: the held audio plays");
    assert_eq!(tap.buffered(), held - 4800);
    player.pause(Instant::now());
    assert!(silent(&mixed(&tap, 4800)), "paused again");
    assert_eq!(tap.buffered(), held - 4800);
}

#[test]
fn stop_clears_tap() {
    // The decode thread is blocked on its full video queue, so it can't take the seek (and refill
    // the tap) until `current_frame` drains the stale frames.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, AUDIO).unwrap();
    let tap = player.audio().unwrap();
    settled(&tap);
    let t0 = Instant::now();
    player.restart(t0); // stop (new epoch) + play
    assert_eq!(tap.buffered(), 0, "the old position's audio is gone");
    assert!(silent(&mixed(&tap, 4800)), "nothing stale plays");
    frame(&mut player, &gpu, t0).unwrap(); // the decoder seeks and decodes from 0 again
    assert!(settled(&tap) >= 4800, "the new epoch's audio is accepted");
    assert!(!silent(&mixed(&tap, 4800)));
}

#[test]
fn audio_is_optional() {
    let gpu = gpu();
    let player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, None).unwrap();
    assert!(player.audio().is_none(), "no mixer: audio skipped");
    let mut player = Player::open(&gpu, &fixture("prores4444_64.mov"), Playback::Loop, AUDIO).unwrap();
    frame(&mut player, &gpu, Instant::now()).expect("a file without audio plays");
    assert_eq!(player.audio().unwrap().buffered(), 0);
    assert_eq!(player.take_error(), None);
}

#[test]
fn corrupt_audio_keeps_video() {
    // h264_64.mp4's streams with every AAC packet corrupted (gen.sh, `noise` bsf).
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_badaudio_64.mp4"), Playback::Loop, AUDIO).unwrap();
    let t0 = Instant::now();
    player.play(t0);
    let error = poll(|| {
        player.current_frame(&gpu, t0, PERIOD);
        player.take_error().map(Ok)
    })
    .unwrap();
    assert!(error.contains("h264_badaudio_64.mp4: audio"), "{error}");
    for t in [300, 600, 900, 1500] {
        frame(&mut player, &gpu, t0 + ms(t)).expect("video keeps playing, no further errors");
    }
}

/// Writes a w×h PNG whose every pixel is `rgba` (straight alpha) and returns its path.
fn png(w: u32, h: u32, rgba: [u8; 4]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("tessera-test-{}.png", Uuid::new_v4()));
    image::RgbaImage::from_pixel(w, h, image::Rgba(rgba)).save(&path).unwrap();
    path
}

#[test]
fn image_is_premultiplied() {
    let gpu = gpu();
    let path = png(64, 64, [100, 0, 0, 128]);
    let texture = image_frame(&mut ImageSource::open(&gpu, &path).unwrap());
    std::fs::remove_file(&path).unwrap();
    let data = render(&gpu, texture.unwrap());
    assert_near(px(&data, 32, 32), [50, 0, 0, 128], 2);
}

#[test]
fn oversized_image_is_err() {
    let gpu = gpu();
    let path = png(gpu.device.limits().max_texture_dimension_2d + 1, 1, [255; 4]);
    let texture = image_frame(&mut ImageSource::open(&gpu, &path).unwrap());
    std::fs::remove_file(&path).unwrap();
    let error = texture.unwrap_err();
    assert!(error.contains("exceeds the GPU texture limit"), "{error}");
}

/// Mean absolute difference per byte of two read-backs.
fn diff(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as f64).sum::<f64>() / a.len() as f64
}

/// The frame for `now` once the decode thread has caught up with it (~0.2 s of polling).
fn settled_frame(player: &mut Player, gpu: &Gpu, now: Instant) -> Vec<u8> {
    for _ in 0..100 {
        player.current_frame(gpu, now, PERIOD);
        std::thread::sleep(ms(2));
    }
    render(gpu, frame(player, gpu, now).unwrap())
}

#[test]
fn player_stop_shows_first_frame() {
    // After stop the layer shows nothing until the new epoch's first frame is back, then that
    // frame: never the stale mid-clip one.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, None).unwrap();
    let t0 = Instant::now();
    let first = render(&gpu, frame(&mut player, &gpu, t0).unwrap());
    player.play(t0);
    let mid = settled_frame(&mut player, &gpu, t0 + ms(500));
    assert!(diff(&first, &mid) > 10., "testsrc2 0.5 s in differs from its first frame ({})", diff(&first, &mid));
    player.stop();
    let after = render(&gpu, frame(&mut player, &gpu, t0 + ms(600)).unwrap());
    assert!(diff(&first, &after) < 1., "first frame again, not the stale one ({})", diff(&first, &after));
    assert_eq!(player.position_ms(t0 + ms(5000)), 0, "stopped: paused at 0");
}

#[test]
fn player_restart_resets_position() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, None).unwrap();
    let t0 = Instant::now();
    let first = render(&gpu, frame(&mut player, &gpu, t0).unwrap());
    player.play(t0);
    settled_frame(&mut player, &gpu, t0 + ms(600));
    assert_eq!(player.position_ms(t0 + ms(600)), 600);
    let t1 = t0 + ms(600);
    player.restart(t1);
    assert_eq!(player.position_ms(t1), 0);
    assert_eq!(player.position_ms(t1 + ms(200)), 200, "playing from 0");
    let again = render(&gpu, frame(&mut player, &gpu, t1).unwrap());
    assert!(diff(&first, &again) < 1., "shows the first frame ({})", diff(&first, &again));
}

#[test]
fn player_pause_holds_position() {
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::Loop, None).unwrap();
    let t0 = Instant::now();
    frame(&mut player, &gpu, t0).unwrap(); // opened: duration known
    player.play(t0);
    let at_pause = settled_frame(&mut player, &gpu, t0 + ms(400));
    player.pause(t0 + ms(400));
    assert_eq!(player.position_ms(t0 + ms(2000)), 400);
    let later = settled_frame(&mut player, &gpu, t0 + ms(2000));
    assert!(diff(&at_pause, &later) < 1., "paused: the frame holds ({})", diff(&at_pause, &later));
    let t2 = t0 + ms(3000);
    player.play(t2);
    assert_eq!(player.position_ms(t2 + ms(100)), 500, "resumes where it paused");
}

/// Speed scales the timeline live: a change carries on from the current position; pause keeps it.
#[test]
fn speed_rebases_the_timeline() {
    let t0 = Instant::now();
    // HoldLast: no wrap within the 1 s fixture, so positions read straight.
    let gpu = gpu();
    let mut player = Player::open(&gpu, &fixture("h264_64.mp4"), Playback::HoldLast, None).unwrap();
    player.play(t0);
    player.set_speed(2., t0 + ms(200));
    assert_eq!(player.position_ms(t0 + ms(400)), 600, "200 at 1×, then 200 ms at 2×");
    player.pause(t0 + ms(400));
    assert_eq!(player.position_ms(t0 + ms(900)), 600, "paused");
    player.play(t0 + ms(900));
    player.set_speed(0.5, t0 + ms(1000));
    assert_eq!(player.position_ms(t0 + ms(1200)), 900, "600 + 100 ms at 2× + 200 ms at 0.5×");
    player.set_speed(f32::NAN, t0 + ms(1200));
    player.set_speed(100., t0 + ms(1200));
    assert_eq!(player.position_ms(t0 + ms(1225)), 1000, "clamped to 4× (900 + 100), not 100×");
}

/// Software-decoded planar YUV (FFV1 here; also DNxHR, VP9, H.264 4:2:2 the hardware won't take)
/// goes up as the YUV shader's planes, U and V interleaved on the decode thread, instead of an
/// RGBA conversion: quadrants red | blue / green | white in 4:2:0, 4:2:2 10-bit and 4:4:4 8/10-bit.
#[test]
fn planar_yuv_converts() {
    let gpu = gpu();
    let cases = [("yuv420p", PixelFormatKind::Nv12), ("yuv422p10le", PixelFormatKind::P210), ("yuv444p", PixelFormatKind::Nv24), ("yuv444p10le", PixelFormatKind::P410)];
    for (fmt, kind) in cases {
        let path = fixture(&format!("ffv1_{fmt}_64.mkv"));
        let frames = decode_all_frames(&path).unwrap();
        assert!(frames.iter().all(|f| !f.hardware && f.format == kind), "{fmt}: {:?}", frames[0]);
        let mut player = Player::open(&gpu, &path, Playback::Loop, None).unwrap();
        let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame"));
        for ((x, y), want) in [((16, 16), [255, 0, 0, 255]), ((48, 16), [0, 0, 255, 255]), ((16, 48), [0, 255, 0, 255]), ((48, 48), [255, 255, 255, 255])] {
            let got = px(&data, x, y);
            assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 4), "{fmt} at ({x}, {y}): {got:?}, want {want:?}");
        }
    }
}
