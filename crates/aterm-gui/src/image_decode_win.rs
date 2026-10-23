// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! WINDOWS' OWN IMAGE DECODER.
#![cfg(windows)]

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::win_ffi::{Apartment, Com, Guid, StepError, check, co_create};

const CLSID_WIC_IMAGING_FACTORY: Guid = Guid {
    data1: 0xCACA_F262,
    data2: 0x9370,
    data3: 0x4615,
    data4: [0xA1, 0x3B, 0x9F, 0x55, 0x39, 0xDA, 0x4C, 0x0A],
};
const IID_IWIC_IMAGING_FACTORY: Guid = Guid {
    data1: 0xEC5E_C8A9,
    data2: 0xC395,
    data3: 0x4314,
    data4: [0x9C, 0x77, 0x54, 0xD7, 0xA9, 0x35, 0xFF, 0x70],
};
const IID_IWIC_BITMAP_CODEC_INFO: Guid = Guid {
    data1: 0xE87A_44C4,
    data2: 0xB76E,
    data3: 0x4C47,
    data4: [0x8B, 0x09, 0x29, 0x8E, 0xB1, 0x2A, 0x27, 0x14],
};
const PIXEL_FORMAT_32BPP_BGRA: Guid = Guid {
    data1: 0x6FDD_C324,
    data2: 0x4E03,
    data3: 0x4BFE,
    data4: [0xB1, 0x85, 0x3D, 0x77, 0x76, 0x8D, 0xC9, 0x0F],
};

const DECODE_METADATA_ON_DEMAND: i32 = 0;
const COMPONENT_DECODER: u32 = 0x1;
const ENUMERATE_DEFAULT: u32 = 0;
const DITHER_NONE: i32 = 0;
const PALETTE_CUSTOM: i32 = 0;
const EXIF_COLOR_SPACE_SRGB: u32 = 1;
const MOST_COLOR_CONTEXTS: u32 = 8;

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
    read: unsafe extern "system" fn(Raw, *mut c_void, u32, *mut u32) -> i32,
    write: unsafe extern "system" fn(Raw, *const c_void, u32, *mut u32) -> i32,
    seek: unsafe extern "system" fn(Raw, i64, u32, *mut u64) -> i32,
    set_size: unsafe extern "system" fn(Raw, u64) -> i32,
    copy_to: unsafe extern "system" fn(Raw, Raw, u64, *mut u64, *mut u64) -> i32,
    commit: unsafe extern "system" fn(Raw, u32) -> i32,
    revert: unsafe extern "system" fn(Raw) -> i32,
    lock_region: unsafe extern "system" fn(Raw, u64, u64, u32) -> i32,
    unlock_region: unsafe extern "system" fn(Raw, u64, u64, u32) -> i32,
    stat: unsafe extern "system" fn(Raw, *mut c_void, u32) -> i32,
    clone: unsafe extern "system" fn(Raw, Out) -> i32,
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
    get_component_type: unsafe extern "system" fn(Raw, *mut i32) -> i32,
    get_clsid: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_signing_status: unsafe extern "system" fn(Raw, *mut u32) -> i32,
    get_author: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_vendor_guid: unsafe extern "system" fn(Raw, *mut Guid) -> i32,
    get_version: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_spec_version: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
    get_friendly_name: unsafe extern "system" fn(Raw, u32, *mut u16, *mut u32) -> i32,
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
    // SAFETY: the caller vouches that the vtable starts with `V`'s slots.
    unsafe { object.vtable::<V>() }
}

fn created(hr: i32, out: *mut c_void, step: &'static str) -> Result<Com, StepError> {
    check(hr, step)?;
    // SAFETY: every caller passes the out pointer of the factory method whose result is `hr`, which
    // on success hands over one reference.
    unsafe { Com::from_raw(out) }.ok_or((step, 0))
}

fn factory() -> Result<Com, StepError> {
    co_create(
        &CLSID_WIC_IMAGING_FACTORY,
        &IID_IWIC_IMAGING_FACTORY,
        "create WICImagingFactory",
    )
}

/// Decode `bytes` to straight sRGB RGBA8, top-down: `(rgba, width, height)`.
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

    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let stream = created(
        unsafe { (fv.create_stream)(factory.as_raw(), &mut out) },
        out,
        "CreateStream",
    )?;
    // SAFETY: `stream` is a live IWICStream; the memory is `len` readable bytes that outlive it.
    let sv = unsafe { vtable::<StreamVtbl>(&stream) };
    check(
        unsafe { (sv.initialize_from_memory)(stream.as_raw(), bytes.as_ptr(), len) },
        "InitializeFromMemory",
    )?;

    let mut out = std::ptr::null_mut();
    // SAFETY: the live stream, no preferred vendor, a valid out pointer.
    let decoder = created(
        unsafe {
            (fv.create_decoder_from_stream)(
                factory.as_raw(),
                stream.as_raw(),
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
        unsafe { (dv.get_frame)(decoder.as_raw(), 0, &mut out) },
        out,
        "GetFrame",
    )?;
    // SAFETY: `frame` is a live IWICBitmapFrameDecode.
    let frv = unsafe { vtable::<FrameVtbl>(&frame) };

    let (mut width, mut height) = (0u32, 0u32);
    check(
        unsafe { (frv.source.get_size)(frame.as_raw(), &mut width, &mut height) },
        "GetSize",
    )?;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Err(("GetSize", 0));
    }
    if w > max_dimension || h > max_dimension {
        return Ok(Decoded::TooLarge);
    }

    let managed =
        to_srgb(&factory, &frame).and_then(|managed| copy_rgba(&factory, &managed, w, h).ok());
    match managed {
        Some(decoded) => Ok(decoded),
        None => copy_rgba(&factory, &frame, w, h),
    }
}

fn copy_rgba(factory: &Com, source: &Com, w: usize, h: usize) -> Result<Decoded, StepError> {
    // SAFETY: `factory` is a live IWICImagingFactory.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(factory) };
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let converter = created(
        unsafe { (fv.create_format_converter)(factory.as_raw(), &mut out) },
        out,
        "CreateFormatConverter",
    )?;
    // SAFETY: `converter` is a live IWICFormatConverter; the source is a live bitmap source, the
    // format a static GUID, no palette.
    let cv = unsafe { vtable::<FormatConverterVtbl>(&converter) };
    check(
        unsafe {
            (cv.initialize)(
                converter.as_raw(),
                source.as_raw(),
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
    // SAFETY: the whole image (a null rect), into a buffer exactly `stride * height` bytes long.
    check(
        unsafe {
            (cv.source.copy_pixels)(
                converter.as_raw(),
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

fn bgra_to_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

fn to_srgb(factory: &Com, frame: &Com) -> Option<Com> {
    // SAFETY: both are live interfaces of the named kinds.
    let fv = unsafe { vtable::<ImagingFactoryVtbl>(factory) };
    let frv = unsafe { vtable::<FrameVtbl>(frame) };
    let mut count = 0u32;
    // SAFETY: a count query (no array) with a valid out pointer.
    let hr =
        unsafe { (frv.get_color_contexts)(frame.as_raw(), 0, std::ptr::null_mut(), &mut count) };
    if hr < 0 || count == 0 {
        return None;
    }
    let count = count.min(MOST_COLOR_CONTEXTS);
    let contexts = (0..count)
        .map(|_| {
            let mut out = std::ptr::null_mut();
            // SAFETY: a factory method with a valid out pointer.
            created(
                unsafe { (fv.create_color_context)(factory.as_raw(), &mut out) },
                out,
                "CreateColorContext",
            )
            .ok()
        })
        .collect::<Option<Vec<Com>>>()?;
    let mut raw: Vec<*mut c_void> = contexts.iter().map(|context| context.as_raw()).collect();
    let mut filled = 0u32;
    // SAFETY: `raw` holds `count` live, uninitialised colour contexts that WIC initialises in
    // place; it does not take ownership of them.
    let hr =
        unsafe { (frv.get_color_contexts)(frame.as_raw(), count, raw.as_mut_ptr(), &mut filled) };
    if hr < 0 || filled == 0 {
        return None;
    }
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let srgb = created(
        unsafe { (fv.create_color_context)(factory.as_raw(), &mut out) },
        out,
        "CreateColorContext",
    )
    .ok()?;
    // SAFETY: `srgb` is a live, uninitialised IWICColorContext.
    let ccv = unsafe { vtable::<ColorContextVtbl>(&srgb) };
    if unsafe { (ccv.initialize_from_exif_color_space)(srgb.as_raw(), EXIF_COLOR_SPACE_SRGB) } < 0 {
        return None;
    }
    let mut out = std::ptr::null_mut();
    // SAFETY: a factory method with a valid out pointer.
    let transform = created(
        unsafe { (fv.create_color_transformer)(factory.as_raw(), &mut out) },
        out,
        "CreateColorTransformer",
    )
    .ok()?;
    // SAFETY: `transform` is a live IWICColorTransform; the frame, the frame's first context and
    // the sRGB context are live; the format is a static GUID.
    let tv = unsafe { vtable::<ColorTransformVtbl>(&transform) };
    let hr = unsafe {
        (tv.initialize)(
            transform.as_raw(),
            frame.as_raw(),
            contexts[0].as_raw(),
            srgb.as_raw(),
            &PIXEL_FORMAT_32BPP_BGRA,
        )
    };
    (hr >= 0).then_some(transform)
}

/// Lowercase dotless file extensions claimed by installed WIC decoders.
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
                factory.as_raw(),
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
        // SAFETY: one element into a valid out pointer; S_FALSE (1) with none fetched ends the
        // enumeration.
        let hr = unsafe { (ev.next)(decoders.as_raw(), 1, &mut item, &mut fetched) };
        if hr < 0 || fetched == 0 || item.is_null() {
            break;
        }
        // SAFETY: a fetched element is one reference handed over.
        let Some(item) = (unsafe { Com::from_raw(item) }) else {
            break;
        };
        let Ok(info) = item.query(&IID_IWIC_BITMAP_CODEC_INFO) else {
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

fn file_extensions(info: &Com) -> Vec<String> {
    // SAFETY: `info` is a live IWICBitmapCodecInfo.
    let iv = unsafe { vtable::<CodecInfoVtbl>(info) };
    let mut needed = 0u32;
    // SAFETY: a length query (no buffer) with a valid out pointer.
    let hr =
        unsafe { (iv.get_file_extensions)(info.as_raw(), 0, std::ptr::null_mut(), &mut needed) };
    if hr < 0 || needed == 0 || needed > 4096 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; needed as usize];
    let mut actual = 0u32;
    // SAFETY: `buffer` holds `needed` u16s, the length WIC asked for.
    let hr = unsafe {
        (iv.get_file_extensions)(info.as_raw(), needed, buffer.as_mut_ptr(), &mut actual)
    };
    if hr < 0 {
        return Vec::new();
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    split_extensions(&String::from_utf16_lossy(&buffer[..end]))
}

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
        v.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        v.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        v
    }

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
