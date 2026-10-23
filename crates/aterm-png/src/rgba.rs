// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hardened PNG to packed RGBA8 decoder.

use crate::{BitDepth, ColorType, Decoder, Limits, Transformations};

/// Floor of the output budget (64 MiB) ([`decode_rgba8`]).
const DECODE_BYTE_LIMIT: usize = 64 * 1024 * 1024;

/// Decode a PNG to packed RGBA8: `None` past `max_dimension` on either axis or past an output
/// budget never below [`DECODE_BYTE_LIMIT`] ([`Limits`]).
#[must_use]
pub fn decode_rgba8(bytes: &[u8], max_dimension: u32) -> Option<(Vec<u8>, usize, usize)> {
    let out_budget = (max_dimension as usize)
        .saturating_mul(max_dimension as usize)
        .saturating_mul(4)
        .max(DECODE_BYTE_LIMIT);
    let mut decoder = Decoder::new(bytes);
    decoder.set_limits(Limits { bytes: out_budget });
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

/// Convert a decoded 8-bit buffer to packed RGBA8.
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
        ColorType::Rgba | ColorType::Indexed => return None,
    }
    Some(out)
}
