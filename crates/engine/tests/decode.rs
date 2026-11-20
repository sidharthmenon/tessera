use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

fn gpu() -> Gpu {
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
    read_rgba(gpu, view.texture())
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
    let mut player = Player::open(&fixture("prores4444_64.mov"), Playback::Loop).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    let (left, right) = (px(&data, 8, 32), px(&data, 56, 32));
    assert!(left[3] < 26, "left alpha {left:?} should be < 0.1");
    assert!(right[3] > 229, "right alpha {right:?} should be > 0.9");
    assert_near(right, [255, 0, 0, 255], 4); // red: proves the A/Y/Cb/Cr swizzle
}

#[test]
fn software_fallback_keeps_alpha() {
    // FFV1 has no hardware decoder: yuva420p → sws_scale → straight-alpha RGBA8.
    let frames = decode_all_frames(&fixture("ffv1_alpha_64.mkv")).unwrap();
    assert!(frames.iter().all(|f| !f.hardware && f.format == PixelFormatKind::Rgba), "{:?}", frames[0]);
    let gpu = gpu();
    let mut player = Player::open(&fixture("ffv1_alpha_64.mkv"), Playback::Loop).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    assert!(px(&data, 8, 32)[3] < 26, "left alpha {:?}", px(&data, 8, 32));
    assert_near(px(&data, 56, 32), [255, 0, 0, 255], 4); // BT.709, limited range
}

#[test]
fn hevc_main10_converts() {
    // Real decoded Main10 (P010, MSB-aligned) frame: white left half, 75 % grey right half.
    let gpu = gpu();
    let mut player = Player::open(&fixture("hevc10_64.mp4"), Playback::Loop).unwrap();
    let data = render(&gpu, frame(&mut player, &gpu, Instant::now()).expect("first frame, shown while paused"));
    // ±2: scale 256 lands within 1 code value here; 255 (LSB-style scaling) is 3 off.
    assert_near(px(&data, 8, 32), [255, 255, 255, 255], 2);
    assert_near(px(&data, 56, 32), [191, 191, 191, 255], 2);
}

#[test]
fn open_missing_file_is_err() {
    // `open` never blocks (R22), so the error arrives through `take_error`; no frame meanwhile.
    let gpu = gpu();
    let mut missing = Player::open(Path::new(MISSING), Playback::Loop).unwrap();
    let error = frame(&mut missing, &gpu, Instant::now()).unwrap_err();
    assert!(error.contains(MISSING) && error.contains("cannot open"), "{error}");
    let mut not_video = Player::open(&fixture("gen.sh"), Playback::Loop).unwrap();
    assert!(frame(&mut not_video, &gpu, Instant::now()).is_err(), "not a video");
    assert!(decode_all_frames(Path::new(MISSING)).is_err());
    let mut image = ImageSource::open(&gpu, Path::new("/nonexistent/still.png")).unwrap();
    assert!(image_frame(&mut image).is_err());
}

#[test]
#[cfg(unix)]
fn open_does_not_block() {
    // Opening a FIFO blocks until a writer shows up, so an open on the caller's thread hangs here.
    // (The decode thread stays blocked until the test process exits.)
    let fifo = std::env::temp_dir().join(format!("tessera-test-{}.fifo", Uuid::new_v4()));
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let (tx, rx) = std::sync::mpsc::channel();
    let path = fifo.clone();
    std::thread::spawn(move || tx.send(Player::open(&path, Playback::Loop).is_ok()));
    let opened = rx.recv_timeout(Duration::from_secs(2));
    std::fs::remove_file(&fifo).unwrap();
    assert_eq!(opened, Ok(true), "Player::open blocked on opening the file");
}

#[test]
fn loop_wraps_position() {
    let gpu = gpu();
    let mut player = Player::open(&fixture("h264_64.mp4"), Playback::Loop).unwrap();
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
    let mut player = Player::open(&fixture("h264_64.mp4"), Playback::Once).unwrap();
    let t0 = Instant::now();
    player.play(t0);
    frame(&mut player, &gpu, t0).expect("first frame while playing");
    assert!(player.current_frame(&gpu, t0 + ms(1200), PERIOD).is_none(), "transparent after the end");
    assert_eq!(player.position_ms(t0 + ms(1200)), 1000);
}

#[test]
fn hold_last_keeps_frame() {
    let gpu = gpu();
    let mut player = Player::open(&fixture("h264_64.mp4"), Playback::HoldLast).unwrap();
    let t0 = Instant::now();
    player.play(t0);
    frame(&mut player, &gpu, t0 + ms(1200)).expect("last frame held");
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
