// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! WINDOWS' OWN IMAGE DECODER: the Windows Imaging Component (WIC) behind the
//! wallpaper on Windows, for every format the first-party decoders do not read
//! (`app_config::decode_wallpaper_portable`: PNG and JPEG). WIC ships BMP, GIF,
//! TIFF, ICO and JPEG XR with the OS, and HEIC, WebP and AVIF wherever their
//! Store codecs are installed: the Windows counterpart of the AppKit decoder on
//! macOS, so a wallpaper the system can show is a wallpaper aterm can draw.
//!
//! **One table for the decoder and the picker.** [`decodable_extensions`] asks
//! WIC which file extensions its installed decoders claim, and the wallpaper
//! picker offers exactly those (with the first-party ones), so the picker can
//! never offer a file the decoder refuses, nor hide one it reads.
//!
//! **The pixels.** The first frame, colour-managed into sRGB where the file
//! carries a colour profile (an `IWICColorTransform` from the frame's own
//! context to the EXIF sRGB one, as the macOS path converts rather than
//! retags), then converted to straight 32bpp BGRA and copied out as RGBA8,
//! top-down. Its size is checked against the caller's bound before any pixel
//! is copied. A step that fails is the decode's refusal, named.
//!
//! **FFI style**: hand-rolled by-hand-vtable COM against the system's
//! `windowscodecs` component, the house style of [`super::file_picker_win`]
//! (no `windows`-crate feature for four interfaces). The vtables are
//! transcribed from `wincodec.h` in COM inheritance order; every slot before a
//! called one is declared, so offsets are load-bearing, and each table is
//! truncated after the last slot this module calls.
#![cfg(windows)]

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::win_ffi::{Apartment, Com, Guid, StepError, check, co_create};

// ---------------------------------------------------------------------------
// GUIDs and constants (wincodec.h)
// ---------------------------------------------------------------------------

/// `CLSID_WICImagingFactory` {CACAF262-9370-4615-A13B-9F5539DA4C0A}: the
/// factory every Windows since Vista registers (WICImagingFactory2 adds only
/// Direct2D interop, which this module has no use for).
const CLSID_WIC_IMAGING_FACTORY: Guid = Guid {
    data1: 0xCACA_F262,
    data2: 0x9370,
    data3: 0x4615,
    data4: [0xA1, 0x3B, 0x9F, 0x55, 0x39, 0xDA, 0x4C, 0x0A],
};
/// `IID_IWICImagingFactory` {EC5EC8A9-C395-4314-9C77-54D7A935FF70}.
const IID_IWIC_IMAGING_FACTORY: Guid = Guid {
    data1: 0xEC5E_C8A9,
    data2: 0xC395,
    data3: 0x4314,
    data4: [0x9C, 0x77, 0x54, 0xD7, 0xA9, 0x35, 0xFF, 0x70],
};
/// `IID_IWICBitmapCodecInfo` {E87A44C4-B76E-4C47-8B09-298EB12A2714}.
const IID_IWIC_BITMAP_CODEC_INFO: Guid = Guid {
    data1: 0xE87A_44C4,
    data2: 0xB76E,
    data3: 0x4C47,
    data4: [0x8B, 0x09, 0x29, 0x8E, 0xB1, 0x2A, 0x27, 0x14],
};
/// `GUID_WICPixelFormat32bppBGRA` {6FDDC324-4E03-4BFE-B185-3D77768DC90F}:
/// straight (not premultiplied) alpha, the format every WIC converter and
/// colour transform supports.
const PIXEL_FORMAT_32BPP_BGRA: Guid = Guid {
    data1: 0x6FDD_C324,
    data2: 0x4E03,
    data3: 0x4BFE,
    data4: [0xB1, 0x85, 0x3D, 0x77, 0x76, 0x8D, 0xC9, 0x0F],
};

/// `WICDecodeMetadataCacheOnDemand`: read metadata only when asked.
const DECODE_METADATA_ON_DEMAND: i32 = 0;
/// `WICDecoder`, the component type [`decodable_extensions`] enumerates.
const COMPONENT_DECODER: u32 = 0x1;
/// `WICComponentEnumerateDefault`.
const ENUMERATE_DEFAULT: u32 = 0;
/// `WICBitmapDitherTypeNone`.
const DITHER_NONE: i32 = 0;
/// `WICBitmapPaletteTypeCustom` (no palette is handed over).
const PALETTE_CUSTOM: i32 = 0;
/// The EXIF colour space id of sRGB, what the transform converts into.
const EXIF_COLOR_SPACE_SRGB: u32 = 1;
/// How many colour contexts a frame may carry that the decoder looks at.
const MOST_COLOR_CONTEXTS: u32 = 8;

// ---------------------------------------------------------------------------
// Vtables, in COM inheritance order (wincodec.h, objidl.h)
// ---------------------------------------------------------------------------

type Raw = *mut c_void;
type Out = *mut *mut c_void;

/// `IWICImagingFactory`, truncated after `CreateComponentEnumerator`.
#[repr(C)]
struct ImagingFactoryVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    create_decoder_from_filename:
        unsafe extern "system" fn(Raw, *const u16, *const Guid, u32, i32, Out) -> i32,
    create_decoder_from_stream: unsafe extern "system" fn(Raw, Raw, *const Guid, i32, Out) -> i32,
    create_decoder_from_file_handle:
        unsafe extern "system" fn(Raw, usize, *const Guid, i32, Out) -> i32,
    create_component_info: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    create_decoder: unsafe extern "system" fn(Raw, *const Guid, *const Guid, Out) -> i32,
    create_encoder: unsafe extern "system" fn(Raw, *const Guid, *const Guid, Out) -> i32,
    create_palette: unsafe extern "system" fn(Raw, Out) -> i32,
    create_format_converter: unsafe extern "system" fn(Raw, Out) -> i32,
    create_bitmap_scaler: unsafe extern "system" fn(Raw, Out) -> i32,
    create_bitmap_clipper: unsafe extern "system" fn(Raw, Out) -> i32,
    create_bitmap_flip_rotator: unsafe extern "system" fn(Raw, Out) -> i32,
    create_stream: unsafe extern "system" fn(Raw, Out) -> i32,
    create_color_context: unsafe extern "system" fn(Raw, Out) -> i32,
    create_color_transformer: unsafe extern "system" fn(Raw, Out) -> i32,
    create_bitmap: unsafe extern "system" fn(Raw, u32, u32, *const Guid, i32, Out) -> i32,
    create_bitmap_from_source: unsafe extern "system" fn(Raw, Raw, i32, Out) -> i32,
    create_bitmap_from_source_rect:
        unsafe extern "system" fn(Raw, Raw, u32, u32, u32, u32, Out) -> i32,
    create_bitmap_from_memory:
        unsafe extern "system" fn(Raw, u32, u32, *const Guid, u32, u32, *const u8, Out) -> i32,
    create_bitmap_from_hbitmap: unsafe extern "system" fn(Raw, isize, isize, i32, Out) -> i32,
    create_bitmap_from_hicon: unsafe extern "system" fn(Raw, isize, Out) -> i32,
    create_component_enumerator: unsafe extern "system" fn(Raw, u32, u32, Out) -> i32,
}

/// `IWICStream` (IUnknown → ISequentialStream → IStream → IWICStream),
/// truncated after `InitializeFromMemory`.
#[repr(C)]
struct StreamVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    // ISequentialStream
    read: unsafe extern "system" fn(Raw, *mut c_void, u32, *mut u32) -> i32,
    write: unsafe extern "system" fn(Raw, *const c_void, u32, *mut u32) -> i32,
    // IStream
    seek: unsafe extern "system" fn(Raw, i64, u32, *mut u64) -> i32,
    set_size: unsafe extern "system" fn(Raw, u64) -> i32,
    copy_to: unsafe extern "system" fn(Raw, Raw, u64, *mut u64, *mut u64) -> i32,
    commit: unsafe extern "system" fn(Raw, u32) -> i32,
    revert: unsafe extern "system" fn(Raw) -> i32,
    lock_region: unsafe extern "system" fn(Raw, u64, u64, u32) -> i32,
    unlock_region: unsafe extern "system" fn(Raw, u64, u64, u32) -> i32,
    stat: unsafe extern "system" fn(Raw, *mut c_void, u32) -> i32,
    clone: unsafe extern "system" fn(Raw, Out) -> i32,
    // IWICStream
    initialize_from_istream: unsafe extern "system" fn(Raw, Raw) -> i32,
    initialize_from_filename: unsafe extern "system" fn(Raw, *const u16, u32) -> i32,
    initialize_from_memory: unsafe extern "system" fn(Raw, *const u8, u32) -> i32,
}

/// `IWICBitmapDecoder`, truncated after `GetFrame`.
#[repr(C)]
struct DecoderVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    query_capability: unsafe extern "system" fn(Raw, Raw, *mut u32) -> i32,
    initialize: unsafe extern "system" fn(Raw, Raw, i32) -> i32,
    get_container_format: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_decoder_info: unsafe extern "system" fn(Raw, Out) -> i32,
    copy_palette: unsafe extern "system" fn(Raw, Raw) -> i32,
    get_metadata_query_reader: unsafe extern "system" fn(Raw, Out) -> i32,
    get_preview: unsafe extern "system" fn(Raw, Out) -> i32,
    get_color_contexts: unsafe extern "system" fn(Raw, u32, Out, *mut u32) -> i32,
    get_thumbnail: unsafe extern "system" fn(Raw, Out) -> i32,
    get_frame_count: unsafe extern "system" fn(Raw, *mut u32) -> i32,
    get_frame: unsafe extern "system" fn(Raw, u32, Out) -> i32,
}

/// `WICRect`.
#[repr(C)]
struct WicRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// `IWICBitmapFrameDecode` (IUnknown → IWICBitmapSource →
/// IWICBitmapFrameDecode), truncated after `GetColorContexts`. Its first
/// eight slots are every bitmap source's ([`SourceVtbl`]).
#[repr(C)]
struct FrameVtbl {
    source: SourceVtbl,
    get_metadata_query_reader: unsafe extern "system" fn(Raw, Out) -> i32,
    get_color_contexts: unsafe extern "system" fn(Raw, u32, Out, *mut u32) -> i32,
}

/// `IWICBitmapSource`: the frame, the converter and the transform alike.
#[repr(C)]
struct SourceVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    get_size: unsafe extern "system" fn(Raw, *mut u32, *mut u32) -> i32,
    get_pixel_format: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_resolution: unsafe extern "system" fn(Raw, *mut f64, *mut f64) -> i32,
    copy_palette: unsafe extern "system" fn(Raw, Raw) -> i32,
    copy_pixels: unsafe extern "system" fn(Raw, *const WicRect, u32, u32, *mut u8) -> i32,
}

/// `IWICFormatConverter` (IWICBitmapSource → IWICFormatConverter),
/// truncated after `Initialize`.
#[repr(C)]
struct FormatConverterVtbl {
    source: SourceVtbl,
    initialize: unsafe extern "system" fn(Raw, Raw, *const Guid, i32, Raw, f64, i32) -> i32,
}

/// `IWICColorTransform` (IWICBitmapSource → IWICColorTransform).
#[repr(C)]
struct ColorTransformVtbl {
    source: SourceVtbl,
    initialize: unsafe extern "system" fn(Raw, Raw, Raw, Raw, *const Guid) -> i32,
}

/// `IWICColorContext`, truncated after `InitializeFromExifColorSpace`.
#[repr(C)]
struct ColorContextVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    initialize_from_filename: unsafe extern "system" fn(Raw, *const u16) -> i32,
    initialize_from_memory: unsafe extern "system" fn(Raw, *const u8, u32) -> i32,
    initialize_from_exif_color_space: unsafe extern "system" fn(Raw, u32) -> i32,
}

/// `IEnumUnknown` (objidl.h), truncated after `Next`.
#[repr(C)]
struct EnumUnknownVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    next: unsafe extern "system" fn(Raw, u32, Out, *mut u32) -> i32,
}

/// `IWICBitmapCodecInfo` (IUnknown → IWICComponentInfo →
/// IWICBitmapCodecInfo), truncated after `GetFileExtensions`.
#[repr(C)]
struct CodecInfoVtbl {
    query_interface: unsafe extern "system" fn(Raw, *const Guid, Out) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    // IWICComponentInfo
    get_component_type: unsafe extern "system" fn(Raw, *mut i32) -> i32,
    get_clsid: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_signing_status: unsafe extern "system" fn(Raw, *mut u32) -> i32,
    get_author: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_vendor_guid: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_version: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_spec_version: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_friendly_name: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    // IWICBitmapCodecInfo
    get_container_format: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_pixel_formats: unsafe extern "system" fn(Raw, u32, *mut Guid, *mut u32) -> i32,
    get_color_management_version: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_device_manufacturer: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_device_models: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_mime_types: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_file_extensions: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
}

/// The vtable of the live interface `object`, viewed as `V`.
///
/// # Safety
///
/// `object` must be a live interface pointer whose vtable begins with `V`'s
/// slots, in order.
unsafe fn vtable<'a, V>(object: &'a Com) -> &'a V {
    // SAFETY: every COM object begins with its vtable pointer; the caller
    // vouches that the vtable starts with `V`'s slots.
    unsafe { &**object.0.cast::<*const V>() }
}

/// Call a factory method that hands back one interface pointer, owned.
fn created(hr: i32, out: *mut c_void, step: &'static str) -> Result<Com, StepError> {
    check(hr, step)?;
    if out.is_null() {
        return Err((step, 0));
    }
    Ok(Com(out))
}

/// The imaging factory, in process.
fn factory() -> Result<Com, StepError> {
    // SAFETY: standard COM object creation of a registered system class.
    unsafe {
        co_create(
            &CLSID_WIC_IMAGING_FACTORY,
            &IID_IWIC_IMAGING_FACTORY,
            "create WICImagingFactory",
        )
    }
}

// ---------------------------------------------------------------------------
// The decoder
// ---------------------------------------------------------------------------

/// Decode `bytes` (any format an installed WIC decoder reads) to straight
/// sRGB RGBA8, top-down: `(rgba, width, height)`. Either side past
/// `max_dimension` is refused before any pixel is copied. `Err` names why, in
/// words the Wallpaper page shows.
pub(crate) fn decode_rgba8(
    bytes: &[u8],
    max_dimension: usize,
) -> Result<(Vec<u8>, usize, usize), String> {
    let Ok(len) = u32::try_from(bytes.len()) else {
        return Err("too large a file to decode".to_string());
    };
    match decode(bytes, len, max_dimension) {
        Ok(Decoded::Pixels(decoded)) => Ok(decoded),
        Ok(Decoded::TooLarge) => Err(format!(
            "larger than {max_dimension} px on a side, the most a wallpaper can be"
        )),
        Err((step, hr)) => Err(format!(
            "not an image Windows can decode ({step} failed, {hr:#010x})"
        )),
    }
}

enum Decoded {
    Pixels((Vec<u8>, usize, usize)),
    TooLarge,
}

fn decode(bytes: &[u8], len: u32, max_dimension: usize) -> Result<Decoded, StepError> {
    let _apartment = Apartment::enter()?;
    let factory = factory()?;
    // SAFETY: `factory` is a live IWICImagingFactory.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(&factory) };

    // The bytes as a stream. WIC reads them in place: `bytes` outlives the
    // stream, the decoder and every source built on it (all dropped before
    // this function returns).
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let stream = created(
        unsafe { (fv.create_stream)(factory.0, &mut out) },
        out,
        "CreateStream",
    )?;
    // SAFETY: `stream` is a live IWICStream; the memory is `len` readable
    // bytes that outlive it.
    let sv = unsafe { vtable::<StreamVtbl>(&stream) };
    check(
        unsafe { (sv.initialize_from_memory)(stream.0, bytes.as_ptr(), len) },
        "InitializeFromMemory",
    )?;

    let mut out = std::ptr::null_mut();
    // SAFETY: the live stream, no preferred vendor, a valid out pointer.
    let decoder = created(
        unsafe {
            (fv.create_decoder_from_stream)(
                factory.0,
                stream.0,
                std::ptr::null(),
                DECODE_METADATA_ON_DEMAND,
                &mut out,
            )
        },
        out,
        "CreateDecoderFromStream",
    )?;
    // SAFETY: `decoder` is a live IWICBitmapDecoder.
    let dv = unsafe { vtable::<DecoderVtbl>(&decoder) };
    let mut out = std::ptr::null_mut();
    // SAFETY: frame 0 exists for every decodable file; a valid out pointer.
    let frame = created(
        unsafe { (dv.get_frame)(decoder.0, 0, &mut out) },
        out,
        "GetFrame",
    )?;
    // SAFETY: `frame` is a live IWICBitmapFrameDecode.
    let frv = unsafe { vtable::<FrameVtbl>(&frame) };

    let (mut width, mut height) = (0u32, 0u32);
    check(
        unsafe { (frv.source.get_size)(frame.0, &mut width, &mut height) },
        "GetSize",
    )?;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Err(("GetSize", 0));
    }
    if w > max_dimension || h > max_dimension {
        return Ok(Decoded::TooLarge);
    }

    // Colour-managed into sRGB where the frame carries a profile; the frame
    // as it is otherwise, or where the transform cannot be built or read.
    let managed =
        to_srgb(&factory, &frame).and_then(|managed| copy_rgba(&factory, &managed, w, h).ok());
    match managed {
        Some(decoded) => Ok(decoded),
        None => copy_rgba(&factory, &frame, w, h),
    }
}

/// `source`'s pixels, `w` by `h`, as straight RGBA8 rows top-down: through a
/// format converter to straight BGRA, copied whole, the channels swapped.
fn copy_rgba(factory: &Com, source: &Com, w: usize, h: usize) -> Result<Decoded, StepError> {
    // SAFETY: `factory` is a live IWICImagingFactory.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(factory) };
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let converter = created(
        unsafe { (fv.create_format_converter)(factory.0, &mut out) },
        out,
        "CreateFormatConverter",
    )?;
    // SAFETY: `converter` is a live IWICFormatConverter; the source is a live
    // bitmap source, the format a static GUID, no palette.
    let cv = unsafe { vtable::<FormatConverterVtbl>(&converter) };
    check(
        unsafe {
            (cv.initialize)(
                converter.0,
                source.0,
                &PIXEL_FORMAT_32BPP_BGRA,
                DITHER_NONE,
                std::ptr::null_mut(),
                0.0,
                PALETTE_CUSTOM,
            )
        },
        "FormatConverter.Initialize",
    )?;

    let stride = w.checked_mul(4).ok_or(("stride", 0))?;
    let size = stride.checked_mul(h).ok_or(("buffer", 0))?;
    let (Ok(stride32), Ok(size32)) = (u32::try_from(stride), u32::try_from(size)) else {
        return Ok(Decoded::TooLarge);
    };
    let mut pixels = vec![0u8; size];
    // SAFETY: the whole image (a null rect), into a buffer exactly
    // `stride * height` bytes long.
    check(
        unsafe {
            (cv.source.copy_pixels)(
                converter.0,
                std::ptr::null(),
                stride32,
                size32,
                pixels.as_mut_ptr(),
            )
        },
        "CopyPixels",
    )?;
    bgra_to_rgba(&mut pixels);
    Ok(Decoded::Pixels((pixels, w, h)))
}

/// Swap each pixel's blue and red in place: BGRA8 to RGBA8.
fn bgra_to_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

/// The frame converted from its own colour profile into sRGB, as straight
/// BGRA: `None` where the frame carries no profile, or any step fails (the
/// frame is then drawn as it is, as the first-party decoders draw theirs).
fn to_srgb(factory: &Com, frame: &Com) -> Option<Com> {
    // SAFETY: both are live interfaces of the named kinds.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(factory) };
    let frv = unsafe { vtable::<FrameVtbl>(frame) };
    let mut count = 0u32;
    // SAFETY: a count query (no array) with a valid out pointer.
    let hr = unsafe { (frv.get_color_contexts)(frame.0, 0, std::ptr::null_mut(), &mut count) };
    if hr < 0 || count == 0 {
        return None;
    }
    let count = count.min(MOST_COLOR_CONTEXTS);
    // The array WIC fills: contexts made by the factory, owned here.
    let contexts = (0..count)
        .map(|_| {
            let mut out = std::ptr::null_mut();
            // SAFETY: a factory method with a valid out pointer.
            created(
                unsafe { (fv.create_color_context)(factory.0, &mut out) },
                out,
                "CreateColorContext",
            )
            .ok()
        })
        .collect::<Option<Vec<Com>>>()?;
    let mut raw: Vec<*mut c_void> = contexts.iter().map(|context| context.0).collect();
    let mut filled = 0u32;
    // SAFETY: `raw` holds `count` live, uninitialised colour contexts that WIC
    // initialises in place; it does not take ownership of them.
    let hr = unsafe { (frv.get_color_contexts)(frame.0, count, raw.as_mut_ptr(), &mut filled) };
    if hr < 0 || filled == 0 {
        return None;
    }
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let srgb = created(
        unsafe { (fv.create_color_context)(factory.0, &mut out) },
        out,
        "CreateColorContext",
    )
    .ok()?;
    // SAFETY: `srgb` is a live, uninitialised IWICColorContext.
    let ccv = unsafe { vtable::<ColorContextVtbl>(&srgb) };
    if unsafe { (ccv.initialize_from_exif_color_space)(srgb.0, EXIF_COLOR_SPACE_SRGB) } < 0 {
        return None;
    }
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let transform = created(
        unsafe { (fv.create_color_transformer)(factory.0, &mut out) },
        out,
        "CreateColorTransformer",
    )
    .ok()?;
    // SAFETY: `transform` is a live IWICColorTransform; the frame, the
    // frame's first context and the sRGB context are live; the format is a
    // static GUID.
    let tv = unsafe { vtable::<ColorTransformVtbl>(&transform) };
    let hr = unsafe {
        (tv.initialize)(
            transform.0,
            frame.0,
            contexts[0].0,
            srgb.0,
            &PIXEL_FORMAT_32BPP_BGRA,
        )
    };
    (hr >= 0).then_some(transform)
}

// ---------------------------------------------------------------------------
// What the decoder reads
// ---------------------------------------------------------------------------

/// THE FILE EXTENSIONS WIC'S INSTALLED DECODERS CLAIM, lower case and without
/// their dot (`bmp`, `gif`, `tif`, `heic` where the HEIF codec is installed),
/// each once: what [`decode_rgba8`] reads beyond the first-party decoders, and
/// so what the wallpaper picker offers. Empty where WIC cannot be reached.
/// Asked once: the codecs a running process can use do not change under it.
pub(crate) fn decodable_extensions() -> &'static [String] {
    static EXTENSIONS: OnceLock<Vec<String>> = OnceLock::new();
    EXTENSIONS.get_or_init(|| match enumerate_extensions() {
        Ok(extensions) => extensions,
        Err((step, hr)) => {
            aterm_log::warn!("image decoders not listed ({step}: hr={hr:#010x})");
            Vec::new()
        }
    })
}

fn enumerate_extensions() -> Result<Vec<String>, StepError> {
    let _apartment = Apartment::enter()?;
    let factory = factory()?;
    // SAFETY: `factory` is a live IWICImagingFactory.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(&factory) };
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let decoders = created(
        unsafe {
            (fv.create_component_enumerator)(
                factory.0,
                COMPONENT_DECODER,
                ENUMERATE_DEFAULT,
                &mut out,
            )
        },
        out,
        "CreateComponentEnumerator",
    )?;
    // SAFETY: `decoders` is a live IEnumUnknown.
    let ev = unsafe { vtable::<EnumUnknownVtbl>(&decoders) };
    let mut extensions: Vec<String> = Vec::new();
    loop {
        let mut item = std::ptr::null_mut();
        let mut fetched = 0u32;
        // SAFETY: one element into a valid out pointer; S_FALSE (1) with none
        // fetched ends the enumeration.
        let hr = unsafe { (ev.next)(decoders.0, 1, &mut item, &mut fetched) };
        if hr < 0 || fetched == 0 || item.is_null() {
            break;
        }
        let item = Com(item);
        // SAFETY: `item` is a live IUnknown; every vtable begins with
        // QueryInterface.
        let uv = unsafe { vtable::<crate::win_ffi::IUnknownVtbl>(&item) };
        let mut info = std::ptr::null_mut();
        let hr = unsafe { (uv.query_interface)(item.0, &IID_IWIC_BITMAP_CODEC_INFO, &mut info) };
        let Ok(info) = created(hr, info, "QueryInterface") else {
            continue;
        };
        for extension in file_extensions(&info) {
            if !extensions.contains(&extension) {
                extensions.push(extension);
            }
        }
    }
    Ok(extensions)
}

/// One codec's extensions (`.bmp,.dib,.rle`), split.
fn file_extensions(info: &Com) -> Vec<String> {
    // SAFETY: `info` is a live IWICBitmapCodecInfo.
    let iv = unsafe { vtable::<CodecInfoVtbl>(info) };
    let mut needed = 0u32;
    // SAFETY: a length query (no buffer) with a valid out pointer.
    let hr = unsafe { (iv.get_file_extensions)(info.0, 0, std::ptr::null_mut(), &mut needed) };
    if hr < 0 || needed == 0 || needed > 4096 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; needed as usize];
    let mut actual = 0u32;
    // SAFETY: `buffer` holds `needed` u16s, the length WIC asked for.
    let hr = unsafe { (iv.get_file_extensions)(info.0, needed, buffer.as_mut_ptr(), &mut actual) };
    if hr < 0 {
        return Vec::new();
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    split_extensions(&String::from_utf16_lossy(&buffer[..end]))
}

/// `.bmp,.DIB, .rle` as `["bmp", "dib", "rle"]`: lower case, no dot, no
/// blanks, each once.
fn split_extensions(list: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for extension in list.split(',') {
        let extension = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !extension.is_empty()
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
            && !out.contains(&extension)
        {
            out.push(extension);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        CodecInfoVtbl, DecoderVtbl, FormatConverterVtbl, FrameVtbl, ImagingFactoryVtbl, SourceVtbl,
        StreamVtbl, bgra_to_rgba, decodable_extensions, decode_rgba8, split_extensions,
    };

    /// STRUCTURAL FENCE on the hand-rolled vtables: each called slot sits at
    /// its `wincodec.h` / `objidl.h` index (a slot is one pointer).
    #[test]
    fn the_vtables_put_each_called_slot_at_its_index() {
        let slot = |offset: usize| offset / std::mem::size_of::<usize>();
        assert_eq!(
            slot(std::mem::offset_of!(
                ImagingFactoryVtbl,
                create_decoder_from_stream
            )),
            4
        );
        assert_eq!(
            slot(std::mem::offset_of!(
                ImagingFactoryVtbl,
                create_format_converter
            )),
            10
        );
        assert_eq!(
            slot(std::mem::offset_of!(ImagingFactoryVtbl, create_stream)),
            14
        );
        assert_eq!(
            slot(std::mem::offset_of!(
                ImagingFactoryVtbl,
                create_color_context
            )),
            15
        );
        assert_eq!(
            slot(std::mem::offset_of!(
                ImagingFactoryVtbl,
                create_color_transformer
            )),
            16
        );
        assert_eq!(
            slot(std::mem::offset_of!(
                ImagingFactoryVtbl,
                create_component_enumerator
            )),
            23
        );
        assert_eq!(
            slot(std::mem::offset_of!(StreamVtbl, initialize_from_memory)),
            16
        );
        assert_eq!(slot(std::mem::offset_of!(DecoderVtbl, get_frame)), 13);
        assert_eq!(slot(std::mem::offset_of!(SourceVtbl, get_size)), 3);
        assert_eq!(slot(std::mem::offset_of!(SourceVtbl, copy_pixels)), 7);
        assert_eq!(slot(std::mem::offset_of!(FrameVtbl, get_color_contexts)), 9);
        assert_eq!(
            slot(std::mem::offset_of!(FormatConverterVtbl, initialize)),
            8
        );
        assert_eq!(
            slot(std::mem::offset_of!(CodecInfoVtbl, get_file_extensions)),
            17
        );
    }

    /// A codec's comma-separated list becomes bare lower-case extensions,
    /// each once.
    #[test]
    fn a_codecs_extensions_are_bare_and_lower_case() {
        assert_eq!(
            split_extensions(".bmp,.DIB, .rle,,.bmp"),
            ["bmp", "dib", "rle"]
        );
        assert!(split_extensions("").is_empty());
    }

    #[test]
    fn bgra_becomes_rgba() {
        let mut pixels = [1, 2, 3, 4, 5, 6, 7, 8];
        bgra_to_rgba(&mut pixels);
        assert_eq!(pixels, [3, 2, 1, 4, 7, 6, 5, 8]);
    }

    /// A 2x2 24-bit BMP: red, green on the top row; blue, white below,
    /// stored bottom-up as B,G,R with 4-byte row padding.
    fn two_by_two_bmp() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"BM");
        v.extend_from_slice(&70_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&54_u32.to_le_bytes());
        v.extend_from_slice(&40_u32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&1_u16.to_le_bytes());
        v.extend_from_slice(&24_u16.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&16_u32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        // Bottom row first: blue, white, padding.
        v.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        // Top row: red, green, padding.
        v.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        v
    }

    /// WIC reads what the first-party decoders do not (a BMP), top-down and
    /// in RGBA order, and its decoders claim it; a picture past the bound is
    /// refused, and bytes that are no image are refused by name.
    #[test]
    fn wic_decodes_a_bmp_top_down_in_rgba() {
        let (rgba, w, h) = decode_rgba8(&two_by_two_bmp(), 8192).expect("WIC read the BMP");
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "top left is red");
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255], "top right is green");
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255], "bottom left is blue");
        assert_eq!(
            &rgba[12..16],
            &[255, 255, 255, 255],
            "bottom right is white"
        );
        assert!(decodable_extensions().iter().any(|e| e == "bmp"));
        let refused = decode_rgba8(&two_by_two_bmp(), 1).expect_err("past the bound");
        assert!(refused.contains("larger than 1 px"), "{refused}");
        let refused = decode_rgba8(b"not an image", 8192).expect_err("no image");
        assert!(
            refused.starts_with("not an image Windows can decode"),
            "{refused}"
        );
    }
}
