//! NDI® receive (video only), through the NDI runtime loaded at run time: nothing NDI is linked or
//! bundled, so Tessera runs without it and NDI sources appear once the free NDI Tools / NDI
//! Runtime is installed. NDI® is a registered trademark of Vizrt NDI AB.
//!
//! Only the handful of SDK functions used here are declared, by hand, from the SDK's C headers
//! (`Processing.NDI.*.h`, v5/v6).

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, bail};
use tessera_show::{InputInfo, MediaKind};

use crate::live::LiveSource;
use crate::video_decode::{Frame, PixelFormatKind};

#[repr(C)]
struct Source {
    name: *const c_char,
    url: *const c_char,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    groups: *const c_char,
    extra_ips: *const c_char,
}

#[repr(C)]
struct RecvCreateV3 {
    source: Source,
    color_format: i32,
    bandwidth: i32,
    allow_video_fields: bool,
    name: *const c_char,
}

#[repr(C)]
struct VideoFrameV2 {
    xres: i32,
    yres: i32,
    fourcc: u32,
    frame_rate_n: i32,
    frame_rate_d: i32,
    picture_aspect_ratio: f32,
    frame_format_type: i32,
    timecode: i64,
    data: *mut u8,
    line_stride: i32,
    metadata: *const c_char,
    timestamp: i64,
}

// The SDK's sizes on 64-bit targets: a layout mistake fails the build, not a show.
const _: () = assert!(size_of::<Source>() == 16 && size_of::<FindCreate>() == 24);
const _: () = assert!(size_of::<RecvCreateV3>() == 40 && size_of::<VideoFrameV2>() == 72);

const COLOR_BGRX_BGRA: i32 = 0;
const BANDWIDTH_HIGHEST: i32 = 100;
const FRAME_VIDEO: i32 = 1;
const FRAME_ERROR: i32 = 4;
const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
const FOURCC_BGRA: u32 = fourcc(b"BGRA");
const FOURCC_BGRX: u32 = fourcc(b"BGRX");

/// The runtime's entry points.
struct Ndi {
    // Kept loaded for the process: the function pointers point into it.
    _lib: libloading::Library,
    find_create_v2: unsafe extern "C" fn(*const FindCreate) -> *mut c_void,
    find_wait_for_sources: unsafe extern "C" fn(*mut c_void, u32) -> bool,
    find_get_current_sources: unsafe extern "C" fn(*mut c_void, *mut u32) -> *const Source,
    find_destroy: unsafe extern "C" fn(*mut c_void),
    recv_create_v3: unsafe extern "C" fn(*const RecvCreateV3) -> *mut c_void,
    recv_destroy: unsafe extern "C" fn(*mut c_void),
    recv_capture_v2: unsafe extern "C" fn(*mut c_void, *mut VideoFrameV2, *mut c_void, *mut c_void, u32) -> i32,
    recv_free_video_v2: unsafe extern "C" fn(*mut c_void, *const VideoFrameV2),
}

/// Where the runtime may be, in order: the installers' environment variables, then the usual
/// install places, then the loader's own search.
fn candidates() -> Vec<PathBuf> {
    #[cfg(windows)]
    const FILE: &str = "Processing.NDI.Lib.x64.dll";
    #[cfg(target_os = "macos")]
    const FILE: &str = "libndi.dylib";
    #[cfg(not(any(windows, target_os = "macos")))]
    const FILE: &str = "libndi.so.6";
    let mut paths: Vec<PathBuf> = ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"].iter().filter_map(std::env::var_os).map(|d| PathBuf::from(d).join(FILE)).collect();
    #[cfg(target_os = "macos")]
    paths.extend(["/usr/local/lib/libndi.dylib", "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib"].map(PathBuf::from));
    paths.push(PathBuf::from(FILE));
    paths
}

/// Loads the runtime from the first of `paths` that opens.
fn load_from(paths: &[PathBuf]) -> Result<Ndi, String> {
    let mut tried = vec![];
    for path in paths {
        // SAFETY: loading the NDI runtime runs its initializers, which is what the SDK expects.
        match unsafe { libloading::Library::new(path) } {
            Ok(lib) => return symbols(lib).map_err(|e| format!("{}: {e:#}", path.display())),
            Err(_) => tried.push(path.display().to_string()),
        }
    }
    Err(format!("the NDI runtime is not installed (install the free NDI Tools); looked for {}", tried.join(", ")))
}

/// The function `name` from `lib`, as `T`.
///
/// SAFETY: `T` must be the function's real signature.
unsafe fn sym<T: Copy>(lib: &libloading::Library, name: &str) -> anyhow::Result<T> {
    // SAFETY: per this function's contract.
    unsafe { Ok(*lib.get::<T>(name.as_bytes()).with_context(|| format!("no {name}"))?) }
}

fn symbols(lib: libloading::Library) -> anyhow::Result<Ndi> {
    // SAFETY: each symbol is the SDK function with the signature declared in `Ndi`.
    unsafe {
        let initialize: unsafe extern "C" fn() -> bool = sym(&lib, "NDIlib_initialize")?;
        if !initialize() {
            bail!("NDIlib_initialize failed (this CPU isn't supported by NDI)");
        }
        Ok(Ndi {
            find_create_v2: sym(&lib, "NDIlib_find_create_v2")?,
            find_wait_for_sources: sym(&lib, "NDIlib_find_wait_for_sources")?,
            find_get_current_sources: sym(&lib, "NDIlib_find_get_current_sources")?,
            find_destroy: sym(&lib, "NDIlib_find_destroy")?,
            recv_create_v3: sym(&lib, "NDIlib_recv_create_v3")?,
            recv_destroy: sym(&lib, "NDIlib_recv_destroy")?,
            recv_capture_v2: sym(&lib, "NDIlib_recv_capture_v2")?,
            recv_free_video_v2: sym(&lib, "NDIlib_recv_free_video_v2")?,
            _lib: lib,
        })
    }
}

/// The runtime, loaded once.
fn ndi() -> anyhow::Result<&'static Ndi> {
    static NDI: OnceLock<Result<Ndi, String>> = OnceLock::new();
    NDI.get_or_init(|| load_from(&candidates())).as_ref().map_err(|e| anyhow::anyhow!("{e}"))
}

/// The NDI sources on the network now (waits about a second for them to announce themselves).
pub fn list() -> anyhow::Result<Vec<InputInfo>> {
    let ndi = ndi()?;
    let create = FindCreate { show_local_sources: true, groups: std::ptr::null(), extra_ips: std::ptr::null() };
    // SAFETY: SDK calls on a finder created here and destroyed before returning; the source array
    // and its strings stay valid until the next call on the finder.
    unsafe {
        let finder = (ndi.find_create_v2)(&create);
        if finder.is_null() {
            bail!("NDI: could not start looking for sources");
        }
        for _ in 0..2 {
            (ndi.find_wait_for_sources)(finder, 600);
        }
        let mut n = 0u32;
        let sources = (ndi.find_get_current_sources)(finder, &mut n);
        let mut out = vec![];
        if !sources.is_null() {
            for s in std::slice::from_raw_parts(sources, n as usize) {
                if s.name.is_null() {
                    continue;
                }
                let name = CStr::from_ptr(s.name).to_string_lossy().into_owned();
                out.push(InputInfo { kind: MediaKind::Ndi, id: name.clone(), name, width: 0, height: 0 });
            }
        }
        (ndi.find_destroy)(finder);
        Ok(out)
    }
}

/// A receiver for the source named `name` (NDI finds it and reconnects by itself).
pub(crate) fn open(name: &str) -> anyhow::Result<Box<dyn LiveSource>> {
    let ndi = ndi()?;
    let source = CString::new(name).context("NDI source name contains a NUL")?;
    let me = c"Tessera";
    let create = RecvCreateV3 {
        source: Source { name: source.as_ptr(), url: std::ptr::null() },
        color_format: COLOR_BGRX_BGRA,
        bandwidth: BANDWIDTH_HIGHEST,
        allow_video_fields: false,
        name: me.as_ptr(),
    };
    // SAFETY: the create struct and its strings live through the call (the SDK copies them).
    let recv = unsafe { (ndi.recv_create_v3)(&create) };
    if recv.is_null() {
        bail!("NDI: could not create a receiver for {name}");
    }
    Ok(Box::new(Receiver { ndi, recv }))
}

struct Receiver {
    ndi: &'static Ndi,
    recv: *mut c_void,
}

impl LiveSource for Receiver {
    fn next(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>> {
        // SAFETY: a zeroed frame struct for the SDK to fill; a video frame is copied, then freed
        // with the SDK before returning. Audio and metadata are discarded (null).
        unsafe {
            let mut v: VideoFrameV2 = std::mem::zeroed();
            let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            match (self.ndi.recv_capture_v2)(self.recv, &mut v, std::ptr::null_mut(), std::ptr::null_mut(), ms) {
                FRAME_VIDEO => {
                    let kind = if v.fourcc == FOURCC_BGRX { PixelFormatKind::Bgrx } else { PixelFormatKind::Bgra };
                    let frame = if (v.fourcc == FOURCC_BGRA || v.fourcc == FOURCC_BGRX) && !v.data.is_null() && v.xres > 0 && v.yres > 0 && v.line_stride > 0 {
                        let (w, h, stride) = (v.xres as u32, v.yres as u32, v.line_stride as usize);
                        let data = std::slice::from_raw_parts(v.data, stride * h as usize);
                        Frame::packed(w, h, data, stride, kind).map(Some)
                    } else {
                        Err(anyhow::anyhow!("NDI: unexpected frame format {:08x}", v.fourcc))
                    };
                    (self.ndi.recv_free_video_v2)(self.recv, &v);
                    frame
                }
                FRAME_ERROR => bail!("NDI: the connection was lost"),
                _ => Ok(None), // nothing yet, audio, metadata or a status change
            }
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // SAFETY: the receiver was created by the SDK and is destroyed once.
        unsafe { (self.ndi.recv_destroy)(self.recv) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_runtime_names_what_was_tried() {
        let paths = [PathBuf::from("/nonexistent/libndi.dylib"), PathBuf::from("/also/missing/libndi.dylib")];
        let error = load_from(&paths).err().expect("nothing to load");
        assert!(error.contains("NDI Tools") && error.contains("/nonexistent/libndi.dylib") && error.contains("/also/missing"), "{error}");
    }

    #[test]
    fn env_dirs_come_first() {
        let c = candidates();
        assert!(!c.is_empty());
        #[cfg(target_os = "macos")]
        assert!(c.iter().any(|p| p == std::path::Path::new("/usr/local/lib/libndi.dylib")));
    }

    #[test]
    fn fourcc_codes() {
        assert_eq!(FOURCC_BGRA, 0x4152_4742);
        assert_eq!(FOURCC_BGRX, 0x5852_4742);
    }
}
