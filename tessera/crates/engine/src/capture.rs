//! Capture devices (cameras, USB capture, capture cards with drivers) through FFmpeg's device
//! inputs: AVFoundation on macOS, DirectShow on Windows. Video only (input audio comes later).
//! Listing uses AVFoundation itself on macOS, where FFmpeg's input can't list devices.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use ffmpeg_next as ff;
use tessera_show::InputInfo;

use crate::live::LiveSource;
use crate::video_decode::{Decoder, Frame, init};

/// The capture devices present now.
pub fn list() -> anyhow::Result<Vec<InputInfo>> {
    #[cfg(target_os = "macos")]
    return mac::list();
    #[cfg(windows)]
    return dshow::list();
    #[cfg(not(any(target_os = "macos", windows)))]
    Ok(vec![])
}

/// Opens device `id` (an `InputInfo::id`) as a live source.
pub(crate) fn open(id: &str) -> anyhow::Result<Box<dyn LiveSource>> {
    init();
    #[cfg(target_os = "macos")]
    let input = mac::open(id)?;
    #[cfg(windows)]
    let input = dshow::open(id)?;
    #[cfg(not(any(target_os = "macos", windows)))]
    let input: ff::format::context::Input = bail!("capture devices: not supported on this platform ({id})");
    let decoder = Decoder::from_input(input, None, Path::new(id), true, None)?;
    Ok(Box::new(Device(decoder)))
}

/// A device's frames, decoded as they arrive.
struct Device(Decoder);

impl LiveSource for Device {
    fn next(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>> {
        let until = Instant::now() + timeout;
        loop {
            match self.0.next() {
                Ok(Some(frame)) => return Ok(Some(frame)),
                Ok(None) => bail!("the device stopped sending"),
                // AVFoundation returns EAGAIN while no frame is ready.
                Err(e) if again(&e) => {
                    if Instant::now() >= until {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

fn again(e: &anyhow::Error) -> bool {
    e.chain().any(|c| matches!(c.downcast_ref::<ff::Error>(), Some(ff::Error::Other { errno }) if *errno == ff::util::error::EAGAIN))
}

/// Opens `url` with FFmpeg's input device `format` and `options`.
fn open_with(format: &str, url: &str, options: &[(&str, &str)]) -> anyhow::Result<ff::format::context::Input> {
    let input = ff::device::input::video().find(|f| f.name() == format).with_context(|| format!("FFmpeg was built without the {format} input"))?;
    let mut dict = ff::Dictionary::new();
    for (k, v) in options {
        dict.set(k, v);
    }
    match ff::format::open_with(url, &input, dict).with_context(|| format!("open {url}"))? {
        ff::format::context::Context::Input(i) => Ok(i),
        _ => bail!("{url}: not an input"),
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use anyhow::{Context, bail, ensure};
    use objc2::encode::{Encoding, RefEncode};
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, msg_send_id};
    use objc2_foundation::{NSArray, NSBundle, NSString};
    use tessera_show::{InputInfo, MediaKind};

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    /// `CMFormatDescriptionRef`'s target (opaque).
    #[repr(C)]
    struct FormatDescription {
        _opaque: [u8; 0],
    }

    // SAFETY: the encoding AVFoundation declares for `formatDescription`.
    unsafe impl RefEncode for FormatDescription {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("opaqueCMFormatDescription", &[]));
    }

    #[repr(C)]
    struct Dimensions {
        width: i32,
        height: i32,
    }

    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMVideoFormatDescriptionGetDimensions(description: *const FormatDescription) -> Dimensions;
    }

    /// `AVMediaTypeVideo`.
    const VIDEO: &str = "vide";

    struct Device {
        id: String,
        name: String,
        width: u32,
        height: u32,
        fps: f64,
    }

    fn class(name: &str) -> anyhow::Result<&'static AnyClass> {
        AnyClass::get(name).with_context(|| format!("AVFoundation: no {name}"))
    }

    /// Every video capture device, with its active format.
    fn devices() -> anyhow::Result<Vec<Device>> {
        autoreleasepool(|_| {
            let media = NSString::from_str(VIDEO);
            // SAFETY: AVFoundation's documented class methods and properties, with the types they
            // return; every object is retained (or only used inside this pool).
            unsafe {
                // ponytail: `devicesWithMediaType` is deprecated but lists every camera on any macOS
                // without device-type names; move to a discovery session if Apple removes it.
                let list: Retained<NSArray<AnyObject>> = msg_send_id![class("AVCaptureDevice")?, devicesWithMediaType: &*media];
                let mut out = vec![];
                for d in list.iter() {
                    let id: Retained<NSString> = msg_send_id![d, uniqueID];
                    let name: Retained<NSString> = msg_send_id![d, localizedName];
                    let format: Option<Retained<AnyObject>> = msg_send_id![d, activeFormat];
                    let (mut width, mut height, mut fps) = (0, 0, 0.);
                    if let Some(format) = format {
                        let description: *const FormatDescription = msg_send![&format, formatDescription];
                        if !description.is_null() {
                            let dims = CMVideoFormatDescriptionGetDimensions(description);
                            (width, height) = (dims.width.max(0) as u32, dims.height.max(0) as u32);
                        }
                        let ranges: Retained<NSArray<AnyObject>> = msg_send_id![&format, videoSupportedFrameRateRanges];
                        for r in ranges.iter() {
                            let max: f64 = msg_send![r, maxFrameRate];
                            fps = f64::max(fps, max);
                        }
                    }
                    out.push(Device { id: id.to_string(), name: name.to_string(), width, height, fps });
                }
                Ok(out)
            }
        })
    }

    pub fn list() -> anyhow::Result<Vec<InputInfo>> {
        Ok(devices()?
            .into_iter()
            .map(|d| InputInfo { kind: MediaKind::Capture, id: d.id, name: d.name, width: d.width, height: d.height })
            .collect())
    }

    /// Opens the device with this unique id through FFmpeg's avfoundation input (by name: the
    /// input takes a name or an index, and indexes change as devices come and go).
    pub fn open(id: &str) -> anyhow::Result<ffmpeg_next::format::context::Input> {
        check_access()?;
        let d = devices()?.into_iter().find(|d| d.id == id).context("the device is not connected")?;
        // FFmpeg reads "a:b" as video device a, audio device b.
        ensure!(!d.name.contains(':'), "{}: names with ':' can't be opened", d.name);
        let size = format!("{}x{}", d.width, d.height);
        // The rate must be one the active format offers (FFmpeg checks within 0.01).
        let fps = format!("{:.3}", if d.fps > 0. { d.fps } else { 30. });
        let mut options = vec![("framerate", fps.as_str())];
        if d.width > 0 && d.height > 0 {
            options.push(("video_size", size.as_str()));
        }
        // NV12 uploads as is; UYVY (the other common camera format) goes through RGBA conversion.
        let with = |pixel: &'static str| [&options[..], &[("pixel_format", pixel)]].concat();
        super::open_with("avfoundation", &d.name, &with("nv12"))
            .or_else(|_| super::open_with("avfoundation", &d.name, &with("uyvy422")))
            .with_context(|| format!("{} ({size} at {fps} fps)", d.name))
    }

    /// Camera access must be allowed; in an app bundle, macOS kills a process that asks for the
    /// camera without a usage description, so that is checked too.
    fn check_access() -> anyhow::Result<()> {
        let media = NSString::from_str(VIDEO);
        // SAFETY: documented class methods returning an NSInteger, the main bundle and its info
        // dictionary value (an NSString or nil).
        unsafe {
            let status: isize = msg_send![class("AVCaptureDevice")?, authorizationStatusForMediaType: &*media];
            if matches!(status, 1 | 2) {
                bail!("camera access is off for Tessera: allow it in System Settings → Privacy & Security → Camera");
            }
            let bundle = NSBundle::mainBundle();
            let path: Retained<NSString> = msg_send_id![&bundle, bundlePath];
            let key = NSString::from_str("NSCameraUsageDescription");
            let usage: Option<Retained<AnyObject>> = msg_send_id![&bundle, objectForInfoDictionaryKey: &*key];
            if path.to_string().ends_with(".app") && usage.is_none() {
                bail!("this build of Tessera can't use cameras (its Info.plist has no NSCameraUsageDescription)");
            }
        }
        Ok(())
    }
}

/// DirectShow. Compiled everywhere so it type-checks on any machine; used on Windows only.
#[cfg_attr(not(windows), allow(dead_code))]
mod dshow {
    use std::ffi::CStr;

    use anyhow::{Context, ensure};
    use ff::ffi;
    use ffmpeg_next as ff;
    use tessera_show::{InputInfo, MediaKind};

    /// FFmpeg's DirectShow input lists its devices: the id is the device's "alternative name"
    /// (`@device_pnp_…`), stable across replugging, unlike its friendly name.
    pub fn list() -> anyhow::Result<Vec<InputInfo>> {
        crate::video_decode::init();
        let Some(ff::Format::Input(format)) = ff::device::input::video().find(|f| f.name() == "dshow") else {
            anyhow::bail!("FFmpeg was built without the dshow input");
        };
        let mut out = vec![];
        // SAFETY: avdevice_list_input_sources fills `list` (freed below); each entry's strings and
        // media-type array live as long as the list.
        unsafe {
            let mut list: *mut ffi::AVDeviceInfoList = std::ptr::null_mut();
            let r = ffi::avdevice_list_input_sources(format.as_ptr(), std::ptr::null(), std::ptr::null_mut(), &mut list);
            ensure!(r >= 0 && !list.is_null(), "DirectShow devices: {}", ff::Error::from(r));
            for i in 0..(*list).nb_devices.max(0) as usize {
                let d = *(*list).devices.add(i);
                if (*d).device_name.is_null() || !media_types(&*d).contains(&ffi::AVMediaType::AVMEDIA_TYPE_VIDEO) {
                    continue;
                }
                let text = |p: *const std::ffi::c_char| if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() };
                let id = text((*d).device_name);
                let name = Some(text((*d).device_description)).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
                out.push(InputInfo { kind: MediaKind::Capture, id, name, width: 0, height: 0 });
            }
            ffi::avdevice_free_list_devices(&mut list);
        }
        Ok(out)
    }

    /// What `d` offers. A device with none (some virtual cameras) has a null array, which
    /// `slice::from_raw_parts` must not get even for 0 items.
    fn media_types(d: &ffi::AVDeviceInfo) -> &[ffi::AVMediaType] {
        if d.media_types.is_null() || d.nb_media_types <= 0 {
            return &[];
        }
        // SAFETY: FFmpeg's array of `nb_media_types` entries, alive as long as the device list.
        unsafe { std::slice::from_raw_parts(d.media_types, d.nb_media_types as usize) }
    }

    /// The options to try, in order: NV12 (uploads as is), then the device's default format
    /// (YUY2 or MJPEG, decoded and converted). A large real-time buffer avoids dshow's frame drops.
    pub fn attempts() -> [Vec<(&'static str, &'static str)>; 2] {
        [vec![("rtbufsize", "702000k"), ("pixel_format", "nv12")], vec![("rtbufsize", "702000k")]]
    }

    pub fn open(id: &str) -> anyhow::Result<ff::format::context::Input> {
        let url = format!("video={id}");
        let [nv12, default] = attempts();
        super::open_with("dshow", &url, &nv12)
            .or_else(|_| super::open_with("dshow", &url, &default))
            .context("is another app using it? Windows privacy: allow desktop apps to use the camera")
    }

    #[cfg(test)]
    mod tests {
        use super::ffi;

        #[test]
        fn a_device_without_media_types_is_not_video() {
            // SAFETY: AVDeviceInfo is pointers and integers, all valid as zero.
            let mut d: ffi::AVDeviceInfo = unsafe { std::mem::zeroed() };
            assert!(super::media_types(&d).is_empty(), "null array, 0 types");
            let mut types = [ffi::AVMediaType::AVMEDIA_TYPE_AUDIO, ffi::AVMediaType::AVMEDIA_TYPE_VIDEO];
            (d.media_types, d.nb_media_types) = (types.as_mut_ptr(), 2);
            assert_eq!(super::media_types(&d), types);
        }

        #[test]
        fn nv12_first_then_the_device_default() {
            let [first, then] = super::attempts();
            assert!(first.contains(&("pixel_format", "nv12")) && !then.iter().any(|(k, _)| *k == "pixel_format"));
            assert!(first.iter().chain(&then).filter(|(k, _)| *k == "rtbufsize").count() == 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists without crashing on any machine (devices may be none, or blocked by privacy settings).
    #[test]
    fn list_does_not_fail() {
        let inputs = list().unwrap();
        eprintln!("capture devices: {inputs:?}");
        assert!(inputs.iter().all(|i| !i.id.is_empty() && !i.name.is_empty()), "{inputs:?}");
    }
}
