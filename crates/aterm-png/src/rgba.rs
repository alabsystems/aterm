// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one hardened PNG → packed RGBA8 decode every consumer shares: the
//! renderer's inline images and colour-emoji strikes, the wallpaper lane, and
//! the engine's Kitty frame composition.

use crate::{BitDepth, ColorType, Decoder, Limits, Transformations};

/// Floor of the output budget (64 MiB): what a caller with a small
/// `max_dimension` may still decode. The dimension check in [`decode_rgba8`] is
/// the load-bearing guard against the IHDR-dimension allocation bomb.
const DECODE_BYTE_LIMIT: usize = 64 * 1024 * 1024;

/// Decode an 8-bit PNG (16-bit channels folded to 8, palettes and `tRNS`
/// expanded) to packed RGBA8, returning `(rgba, width, height)`.
///
/// Hardened against the allocation-bomb DoS a tiny file declaring huge IHDR
/// dimensions would be: either axis past `max_dimension` is refused after IHDR
/// is parsed and BEFORE the output buffer is allocated, and the output budget
/// scales with that dimension budget (never below [`DECODE_BYTE_LIMIT`]). The
/// decoder's limit IS that budget — it streams the image data, so the output
/// buffer is all it allocates in proportion to the image — and so every image
/// within `max_dimension` decodes. Any failure — a corrupt file, an overflowing
/// pixel count, an unsupported layout — is `None`: never a panic, never an
/// unbounded allocation.
#[must_use]
pub fn decode_rgba8(bytes: &[u8], max_dimension: u32) -> Option<(Vec<u8>, usize, usize)> {
    // Checked math so a pixel-count overflow bails instead of wrapping.
    let out_budget = (max_dimension as usize)
        .saturating_mul(max_dimension as usize)
        .saturating_mul(4)
        .max(DECODE_BYTE_LIMIT);
    let mut decoder = Decoder::new(bytes);
    decoder.set_limits(Limits { bytes: out_budget });
    // EXPAND palette (Indexed) → RGB(A) and apply tRNS transparency; STRIP_16
    // folds any 16-bit channel to 8-bit. Noto Color Emoji's CBDT strikes are
    // INDEXED PNGs (palette + tRNS) — without EXPAND they cannot be read and
    // every emoji renders blank. RGB/RGBA PNGs (Apple sbix) are unaffected
    // (EXPAND is a no-op for them, save honouring a tRNS chunk).
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > max_dimension || h > max_dimension {
        return None;
    }
    let out_bytes = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if out_bytes > out_budget {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != BitDepth::Eight {
        return None;
    }
    let (width, height) = (info.width as usize, info.height as usize);
    buf.truncate(info.buffer_size());
    let rgba = to_rgba8(buf, info.color_type, width, height)?;
    Some((rgba, width, height))
}

/// Convert a decoded 8-bit buffer to packed RGBA8. After EXPAND, palette images
/// arrive as RGB/RGBA, so the cases are RGBA, RGB, and grayscale with or
/// without alpha; anything else is `None`. RGBA is already packed RGBA8 and is
/// handed back as it is, so the common case holds one image-sized buffer.
fn to_rgba8(mut buf: Vec<u8>, color_type: ColorType, w: usize, h: usize) -> Option<Vec<u8>> {
    let n = w.checked_mul(h)?;
    let len = n.checked_mul(color_type.samples())?;
    if buf.len() < len {
        return None;
    }
    buf.truncate(len);
    if color_type == ColorType::Rgba {
        return Some(buf);
    }
    let mut out = Vec::with_capacity(n * 4);
    match color_type {
        ColorType::Rgb => {
            for px in buf.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        ColorType::GrayscaleAlpha => {
            for px in buf.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        ColorType::Grayscale => {
            for &g in &buf {
                out.extend_from_slice(&[g, g, g, 255]);
            }
        }
        // RGBA was handed back above; an index means EXPAND did not run.
        ColorType::Rgba | ColorType::Indexed => return None,
    }
    Some(out)
}
