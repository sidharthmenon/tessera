//! Hardware-decoded frames drawn straight from the decoder's buffer, with no copy at all (macOS:
//! a VideoToolbox frame's IOSurface planes wrapped as Metal textures; Windows: a D3D12VA frame's
//! NV12/P010 texture, decoded on wgpu's own DX12 device). A frame this can't take (layout,
//! alignment, a texture array, an error) goes through `staging` instead.
//!
//! Lifetime: the imported textures sample the decoder's buffer, which the decoder reuses once the
//! frame is released, so the frame rides along in the `GpuFrame` (`keep`) and is released through
//! `Gpu::retire` only after the GPU has finished drawing from it.

use crate::gpu::Gpu;
use crate::player::GpuFrame;
use crate::video_decode::Frame;

/// Zero-copy unless `TESSERA_ZERO_COPY=0` (a kill switch back to the copying path, should a GPU or
/// OS version draw imported frames wrong).
pub fn enabled() -> bool {
    cfg!(any(target_os = "macos", windows)) && std::env::var_os("TESSERA_ZERO_COPY").is_none_or(|v| v != "0")
}

/// Decoders on `gpu` use D3D12VA on its own device, so their frames are drawn without a copy:
/// Windows on DX12 with zero-copy enabled, except on Intel's legacy driver line unless
/// `TESSERA_ZERO_COPY=1` (then D3D11VA, and frames are copied).
pub fn d3d12va(gpu: &Gpu) -> bool {
    let info = gpu.adapter.get_info();
    if !cfg!(windows) || info.backend != wgpu::Backend::Dx12 || !enabled() {
        return false;
    }
    let forced = std::env::var_os("TESSERA_ZERO_COPY").is_some_and(|v| v == "1");
    if legacy_intel_driver(info.vendor, &info.driver) && !forced {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            log!(
                "zero-copy off: Intel driver {} (the line for 6th-10th gen GPUs) crashes a D3D12 video decoder once \
                 others on the device are freed; decoding with D3D11VA and copying (TESSERA_ZERO_COPY=1 forces zero-copy)",
                info.driver
            )
        });
        return false;
    }
    true
}

/// Intel's Windows drivers for 6th-10th gen GPUs (build 101.2xxx, `31.0.101.2114`) and older: after a
/// second D3D12 video decoder on a device is freed while another lives, the next decode on that one
/// reads null inside the driver (the debug layer sees nothing wrong before). Xe and later drivers
/// are 101.4000 and up. An unreadable version counts as legacy.
fn legacy_intel_driver(vendor: u32, driver: &str) -> bool {
    let build = || -> Option<(u32, u32)> {
        let mut parts = driver.split('.').skip(2).map(|p| p.parse().ok());
        Some((parts.next()??, parts.next()??))
    };
    vendor == 0x8086 && build().is_none_or(|build| build < (101, 3000))
}

/// A D3D12VA device context on `gpu`'s own ID3D12Device, so the decoder's frames are textures
/// that device can draw; null if it isn't a DX12 device (or not Windows) or FFmpeg refuses it.
pub(crate) fn d3d12_device(gpu: &Gpu) -> *mut ffmpeg_next::ffi::AVBufferRef {
    #[cfg(windows)]
    return win::device(gpu);
    #[cfg(not(windows))]
    {
        let _ = gpu;
        std::ptr::null_mut()
    }
}

/// `frame` (kept on the hardware: `Frame::pixel_buffer`) as textures sampling its buffer, or the
/// frame back with the reason it can't be.
pub(crate) fn import(gpu: &Gpu, frame: Frame) -> Result<GpuFrame, (anyhow::Error, Frame)> {
    #[cfg(target_os = "macos")]
    return match mac::import(gpu, &frame) {
        Ok(textures) => {
            let (w, h) = frame.size();
            Ok(GpuFrame::from_textures(frame.kind, w, h, textures, Some(frame)))
        }
        Err(e) => Err((e, frame)),
    };
    #[cfg(windows)]
    return match win::import(gpu, &frame) {
        Ok(texture) => {
            let (w, h) = frame.size();
            Ok(GpuFrame::from_textures(frame.kind, w, h, vec![texture], Some(frame)))
        }
        Err(e) => Err((e, frame)),
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = gpu;
        Err((anyhow::anyhow!("no zero-copy import on this platform"), frame))
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use anyhow::{Context, ensure};
    use objc2_io_surface::IOSurfaceRef;
    use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
    use wgpu::TextureFormat as F;

    use crate::gpu::Gpu;
    use crate::player::layout;
    use crate::video_decode::Frame;

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        /// The IOSurface backing a CVPixelBuffer (not retained: valid while the buffer is).
        fn CVPixelBufferGetIOSurface(pixel_buffer: *mut c_void) -> *const IOSurfaceRef;
    }

    /// One texture per plane of `layout(frame.kind)`, sampling the frame's IOSurface.
    pub(super) fn import(gpu: &Gpu, frame: &Frame) -> anyhow::Result<Vec<wgpu::Texture>> {
        let pixel_buffer = frame.pixel_buffer().context("not a hardware frame")?;
        // SAFETY: a VideoToolbox frame's data[3] is its CVPixelBuffer, alive as long as `frame`.
        let surface = unsafe { CVPixelBufferGetIOSurface(pixel_buffer).as_ref() }.context("the frame has no IOSurface")?;
        // SAFETY: only the raw MTLDevice is read; wgpu keeps owning it.
        let hal = unsafe { gpu.device.as_hal::<wgpu::hal::api::Metal>() }.context("not a Metal device")?;
        let device = hal.raw_device();
        let (w, h) = frame.size();
        let planar = surface.plane_count() > 0;
        objc2::rc::autoreleasepool(|_| {
            layout(frame.kind)
                .iter()
                .enumerate()
                .map(|(i, &(format, (dx, dy), _))| {
                    let (pw, ph) = (w.div_ceil(dx).max(1), h.div_ceil(dy).max(1));
                    let pixel_format = match format {
                        F::R8Unorm => MTLPixelFormat::R8Unorm,
                        F::Rg8Unorm => MTLPixelFormat::RG8Unorm,
                        F::R16Unorm => MTLPixelFormat::R16Unorm,
                        F::Rg16Unorm => MTLPixelFormat::RG16Unorm,
                        F::Rgba16Unorm => MTLPixelFormat::RGBA16Unorm,
                        other => anyhow::bail!("no zero-copy for {other:?} planes"),
                    };
                    let (stride, surface_w, surface_h) = if planar {
                        (surface.bytes_per_row_of_plane(i), surface.width_of_plane(i), surface.height_of_plane(i))
                    } else {
                        (surface.bytes_per_row(), surface.width(), surface.height())
                    };
                    ensure!(surface_w >= pw as usize && surface_h >= ph as usize, "plane {i}: {surface_w}x{surface_h}, expected {pw}x{ph}");
                    // FFmpeg doesn't ask VideoToolbox for Metal-compatible buffers: check what Metal
                    // needs of a buffer-backed texture's rows.
                    let align = device.minimumLinearTextureAlignmentForPixelFormat(pixel_format);
                    ensure!(align == 0 || stride % align == 0, "plane {i}: {stride}-byte rows aren't {align}-byte aligned");
                    // SAFETY: a plain descriptor for a 2D, unmipmapped texture.
                    let desc = unsafe {
                        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(pixel_format, pw as usize, ph as usize, false)
                    };
                    desc.setUsage(MTLTextureUsage::ShaderRead);
                    let raw = device.newTextureWithDescriptor_iosurface_plane(&desc, surface, i).context("Metal refused the IOSurface plane")?;
                    let extent = wgpu::hal::CopyExtent { width: pw, height: ph, depth: 1 };
                    // SAFETY: `raw` is a live 2D texture of `format`, `pw`×`ph`; the frame (and so its
                    // IOSurface) outlives every use of it (see the module docs).
                    let texture = unsafe {
                        let hal_texture = wgpu::hal::metal::Device::texture_from_raw(raw, format, MTLTextureType::Type2D, 1, 1, extent, None);
                        gpu.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                            hal_texture,
                            &wgpu::TextureDescriptor {
                                label: Some("video plane (zero-copy)"),
                                size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                                view_formats: &[],
                            },
                            wgpu::TextureUses::RESOURCE,
                        )
                    };
                    Ok(texture)
                })
                .collect()
        })
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::ptr;

    use anyhow::{Context, bail, ensure};
    use ffmpeg_next::ffi;
    use wgpu::TextureFormat as F;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE, ID3D12Fence, ID3D12Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
    use windows::core::Interface;

    use crate::gpu::Gpu;
    use crate::video_decode::{Frame, PixelFormatKind};

    // From libavutil/hwcontext_d3d12va.h: the bindings have these only when bindgen could see the
    // Windows SDK headers.

    /// `AVHWDeviceContext.hwctx` of a D3D12VA device; only its first field is touched here.
    #[repr(C)]
    struct AVD3D12VADeviceContext {
        device: *mut c_void,
    }

    #[repr(C)]
    struct AVD3D12VASyncContext {
        fence: *mut c_void,
        _event: HANDLE,
        fence_value: u64,
    }

    /// What a D3D12VA frame's `data[0]` points to.
    #[repr(C)]
    struct AVD3D12VAFrame {
        texture: *mut c_void,
        subresource_index: i32,
        sync_ctx: AVD3D12VASyncContext,
        flags: i32,
    }

    const AV_D3D12VA_FRAME_FLAG_TEXTURE_ARRAY: i32 = 1 << 1;

    pub(super) fn device(gpu: &Gpu) -> *mut ffi::AVBufferRef {
        // SAFETY: only the raw ID3D12Device is read (wgpu keeps owning its reference); FFmpeg gets a
        // reference of its own, released with the context (also when init fails).
        unsafe {
            let Some(hal) = gpu.device.as_hal::<wgpu::hal::api::Dx12>() else { return ptr::null_mut() };
            let mut dev = ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA);
            if dev.is_null() {
                return ptr::null_mut();
            }
            let ctx = (*((*dev).data as *mut ffi::AVHWDeviceContext)).hwctx as *mut AVD3D12VADeviceContext;
            (*ctx).device = hal.raw_device().clone().into_raw();
            if ffi::av_hwdevice_ctx_init(dev) < 0 {
                ffi::av_buffer_unref(&mut dev);
                log!("zero-copy: FFmpeg refused the DX12 device for D3D12VA; decoding with D3D11VA");
                return ptr::null_mut();
            }
            dev
        }
    }

    /// The frame's texture, once the decoder has finished writing it, as a multi-planar NV12/P010
    /// texture (`GpuFrame::from_textures` makes a view of each plane). It can be larger than the
    /// frame (padded rows): `from_textures` scales uv to the frame's part.
    pub(super) fn import(gpu: &Gpu, frame: &Frame) -> anyhow::Result<wgpu::Texture> {
        let f = frame.pixel_buffer().context("not a hardware frame")? as *const AVD3D12VAFrame;
        // SAFETY: a kept Windows frame is a D3D12VA frame (`Decoder::convert`), whose data[0] is
        // its AVD3D12VAFrame, alive (with its texture and fence) as long as `frame`.
        let f = unsafe { &*f };
        ensure!(f.flags & AV_D3D12VA_FRAME_FLAG_TEXTURE_ARRAY == 0 && f.subresource_index == 0, "frames in a texture array");
        let (format, dxgi) = match frame.kind {
            PixelFormatKind::Nv12 => (F::NV12, DXGI_FORMAT_NV12),
            PixelFormatKind::P010 => (F::P010, DXGI_FORMAT_P010),
            other => bail!("no zero-copy for {other:?} frames"),
        };
        ensure!(gpu.device.features().contains(format.required_features()), "the device has no {format:?} textures");
        // SAFETY: COM pointers owned by the frame (borrowed, not released here).
        let (resource, fence) = unsafe { (ID3D12Resource::from_raw_borrowed(&f.texture), ID3D12Fence::from_raw_borrowed(&f.sync_ctx.fence)) };
        let (resource, fence) = (resource.context("frame without a texture")?, fence.context("frame without a fence")?);
        // SAFETY: plain COM calls on live objects.
        let desc = unsafe { resource.GetDesc() };
        ensure!(
            desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
                && desc.DepthOrArraySize == 1
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1
                && desc.Format == dxgi
                && !desc.Flags.contains(D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE),
            "unexpected decoder texture ({:?}, {}x{}, {} layers, flags {:#x})",
            desc.Format,
            desc.Width,
            desc.Height,
            desc.DepthOrArraySize,
            desc.Flags.0
        );
        let (w, h) = frame.size();
        let width = u32::try_from(desc.Width).context("texture width")?;
        ensure!(width >= w && desc.Height >= h, "texture {width}x{} smaller than the frame {w}x{h}", desc.Height);
        // The decoder signals the frame's fence once its texture is written. Waiting here, on the
        // decode thread, keeps the render thread's queue free of cross-queue waits. No event:
        // SetEventOnCompletion then blocks until the fence reaches the value.
        // SAFETY: plain COM calls on a live fence.
        unsafe {
            if fence.GetCompletedValue() < f.sync_ctx.fence_value {
                fence.SetEventOnCompletion(f.sync_ctx.fence_value, HANDLE::default()).context("wait for the decoder")?;
            }
        }
        let size = wgpu::Extent3d { width, height: desc.Height, depth_or_array_layers: 1 };
        // SAFETY: `resource` is a live 2D NV12/P010 texture of `size` on wgpu's own device (the
        // decoder's device context is that device), sampleable, written (fence above) and back in
        // the COMMON state, which a direct queue promotes to a shader resource implicitly; the frame
        // outlives every use of it (see the module docs).
        let texture = unsafe {
            let hal_texture = wgpu::hal::dx12::Device::texture_from_raw(resource.clone(), format, wgpu::TextureDimension::D2, size, 1, 1);
            gpu.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("video frame (zero-copy)"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        Ok(texture)
    }
}

#[cfg(test)]
mod tests {
    use super::legacy_intel_driver;

    const INTEL: u32 = 0x8086;

    #[test]
    fn intel_legacy_driver_line_is_recognised() {
        assert!(legacy_intel_driver(INTEL, "31.0.101.2114"), "UHD 630, Aug 2022");
        assert!(legacy_intel_driver(INTEL, "31.0.101.2135"), "the line's later builds");
        assert!(legacy_intel_driver(INTEL, "30.0.100.9805"), "older still");
        assert!(legacy_intel_driver(INTEL, "0.0.0.0"), "unknown: assume the worst");
        assert!(!legacy_intel_driver(INTEL, "31.0.101.4502"), "Xe driver line");
        assert!(!legacy_intel_driver(INTEL, "32.0.101.6979"), "a later Xe/Arc driver");
        assert!(!legacy_intel_driver(0x10DE, "31.0.15.5222"), "not Intel");
    }
}
