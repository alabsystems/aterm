// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PNG encoding: chunk framing, per-row filter selection, and the zlib stream.
//!
//! Everything aterm writes is 8-bit — framebuffer screenshots, the macOS
//! window capture, palette fixtures — so the encoder covers 8-bit
//! Grayscale/RGB/RGBA/Indexed with optional `PLTE`, `tRNS` and `sRGB`, and
//! refuses anything else at [`Encoder::write_header`] rather than emitting a
//! file it cannot describe.

use std::io::Write;

use crate::checksum::Crc32;
use crate::{
    BitDepth, ColorType, EncodingError, SIGNATURE, SrgbRenderingIntent, deflate, row_bytes,
};

/// Builds a PNG's header, then hands back a [`Writer`] for the pixels.
pub struct Encoder<W: Write> {
    sink: W,
    width: u32,
    height: u32,
    color_type: ColorType,
    bit_depth: BitDepth,
    palette: Option<Vec<u8>>,
    trns: Option<Vec<u8>>,
    srgb: Option<SrgbRenderingIntent>,
}

impl<W: Write> Encoder<W> {
    /// Start a `width` x `height` PNG. Defaults to 8-bit grayscale, as the
    /// retired crate did; every consumer sets both explicitly.
    pub fn new(sink: W, width: u32, height: u32) -> Self {
        Self {
            sink,
            width,
            height,
            color_type: ColorType::Grayscale,
            bit_depth: BitDepth::Eight,
            palette: None,
            trns: None,
            srgb: None,
        }
    }

    /// Set the sample layout.
    pub fn set_color(&mut self, color_type: ColorType) {
        self.color_type = color_type;
    }

    /// Set the bits per sample.
    pub fn set_depth(&mut self, bit_depth: BitDepth) {
        self.bit_depth = bit_depth;
    }

    /// Declare an `sRGB` chunk with this rendering intent.
    pub fn set_source_srgb(&mut self, intent: SrgbRenderingIntent) {
        self.srgb = Some(intent);
    }

    /// Set the `PLTE` palette (RGB triples). Required for
    /// [`ColorType::Indexed`].
    pub fn set_palette(&mut self, palette: impl Into<Vec<u8>>) {
        self.palette = Some(palette.into());
    }

    /// Set the `tRNS` chunk (per-index alpha for an indexed image).
    pub fn set_trns(&mut self, trns: impl Into<Vec<u8>>) {
        self.trns = Some(trns.into());
    }

    /// Write the signature and header chunks.
    ///
    /// # Errors
    /// [`EncodingError::Parameter`] for geometry or a colour-type/bit-depth
    /// pair PNG forbids, or an indexed image with no palette;
    /// [`EncodingError::Io`] if the sink refuses a write.
    pub fn write_header(mut self) -> Result<Writer<W>, EncodingError> {
        if self.width == 0 || self.height == 0 {
            return Err(EncodingError::Parameter("zero image dimension"));
        }
        if self.bit_depth != BitDepth::Eight {
            return Err(EncodingError::Parameter(
                "only 8-bit output is supported (nothing in aterm writes 1/2/4/16-bit PNGs)",
            ));
        }
        if self.color_type == ColorType::Indexed && self.palette.is_none() {
            return Err(EncodingError::Parameter("indexed PNG with no palette"));
        }
        if let Some(palette) = &self.palette {
            if palette.is_empty() || palette.len() % 3 != 0 || palette.len() > 768 {
                return Err(EncodingError::Parameter(
                    "palette must be 1..=256 RGB triples",
                ));
            }
            if self.color_type != ColorType::Indexed {
                return Err(EncodingError::Parameter(
                    "a palette is only meaningful for an indexed PNG",
                ));
            }
        }
        if let Some(trns) = &self.trns {
            let ok = match self.color_type {
                ColorType::Indexed => {
                    trns.len() <= self.palette.as_ref().map_or(0, |p| p.len() / 3)
                }
                ColorType::Grayscale => trns.len() == 2,
                ColorType::Rgb => trns.len() == 6,
                ColorType::GrayscaleAlpha | ColorType::Rgba => false,
            };
            if !ok {
                return Err(EncodingError::Parameter(
                    "tRNS length is wrong for this colour type",
                ));
            }
        }

        let stride = row_bytes(self.width, self.color_type.samples(), self.bit_depth.bits())
            .ok_or(EncodingError::Parameter("image geometry overflows"))?;
        let expected = stride
            .checked_mul(self.height as usize)
            .ok_or(EncodingError::Parameter("image geometry overflows"))?;

        io(self.sink.write_all(&SIGNATURE))?;
        let mut ihdr = [0u8; 13];
        ihdr[0..4].copy_from_slice(&self.width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&self.height.to_be_bytes());
        ihdr[8] = self.bit_depth as u8;
        ihdr[9] = self.color_type as u8;
        // compression = deflate, filter = adaptive, interlace = none. aterm
        // never WRITES an interlaced PNG: it buys nothing for a file that is
        // decoded whole, and it would quadruple this encoder's surface.
        ihdr[10] = 0;
        ihdr[11] = 0;
        ihdr[12] = 0;
        write_chunk(&mut self.sink, b"IHDR", &ihdr)?;
        if let Some(intent) = self.srgb {
            write_chunk(&mut self.sink, b"sRGB", &[intent as u8])?;
        }
        if let Some(palette) = &self.palette {
            write_chunk(&mut self.sink, b"PLTE", palette)?;
        }
        if let Some(trns) = &self.trns {
            write_chunk(&mut self.sink, b"tRNS", trns)?;
        }

        Ok(Writer {
            sink: Some(self.sink),
            stride,
            expected,
            // Filter distance to the pixel on the left, rounded up to a byte.
            bytes_per_pixel: (self.color_type.samples() * self.bit_depth.bits() as usize)
                .div_ceil(8)
                .max(1),
            palette_entries: match self.color_type {
                ColorType::Indexed => Some(self.palette.as_ref().map_or(0, |p| p.len() / 3)),
                _ => None,
            },
            wrote_data: false,
        })
    }
}

/// Accepts the pixel data, then closes the file.
pub struct Writer<W: Write> {
    /// `None` once [`Writer::finish`] has consumed it, so `Drop` does nothing.
    sink: Option<W>,
    stride: usize,
    expected: usize,
    /// Filter distance to the pixel on the left.
    bytes_per_pixel: usize,
    /// Palette entry count for an INDEXED image, so the samples can be checked
    /// against it. `None` for every other colour type.
    palette_entries: Option<usize>,
    wrote_data: bool,
}

impl<W: Write> Writer<W> {
    /// Write the whole image. `data` must be exactly the size the header
    /// implies: `height * row_bytes`.
    ///
    /// # Errors
    /// [`EncodingError::WrongDataSize`] if the buffer does not match the
    /// declared geometry, [`EncodingError::Parameter`] on a second call or on an
    /// indexed sample past the end of the palette, or [`EncodingError::Io`] from
    /// the sink.
    pub fn write_image_data(&mut self, data: &[u8]) -> Result<(), EncodingError> {
        if self.wrote_data {
            return Err(EncodingError::Parameter(
                "image data has already been written",
            ));
        }
        if data.len() != self.expected {
            return Err(EncodingError::WrongDataSize {
                expected: self.expected,
                got: data.len(),
            });
        }
        // An index past the palette is not a legal indexed PNG, and writing one
        // would leave this crate's encoder and its own decoder disagreeing about
        // what it produces. Output is 8-bit only, so one byte is one index and
        // the check is a single scan of a buffer that is about to be copied
        // anyway.
        if let Some(entries) = self.palette_entries
            && let Some(&worst) = data.iter().max()
            && usize::from(worst) >= entries
        {
            return Err(EncodingError::Parameter(
                "indexed sample is past the end of the palette",
            ));
        }
        let filtered = filter_image(data, self.stride, self.bytes_per_pixel);
        let compressed = deflate::zlib_compress(&filtered);
        let sink = self
            .sink
            .as_mut()
            .ok_or(EncodingError::Parameter("writer already finished"))?;
        // One IDAT. The specification permits any split; a single chunk is the
        // simplest thing that is correct, and every decoder concatenates.
        write_chunk(sink, b"IDAT", &compressed)?;
        self.wrote_data = true;
        Ok(())
    }

    /// Write `IEND` and flush.
    ///
    /// # Errors
    /// [`EncodingError::Io`] if the sink refuses the final write.
    pub fn finish(mut self) -> Result<(), EncodingError> {
        if let Some(mut sink) = self.sink.take() {
            write_chunk(&mut sink, b"IEND", &[])?;
            io(sink.flush())?;
        }
        Ok(())
    }
}

impl<W: Write> Drop for Writer<W> {
    fn drop(&mut self) {
        // Closing the file on drop is what the retired crate did, and a dozen
        // call sites rely on it: they build an encoder in a block and let it
        // fall out of scope. A failure here cannot be reported, but a truncated
        // file is strictly better than one that is silently missing its IEND.
        if let Some(mut sink) = self.sink.take() {
            let _ = write_chunk(&mut sink, b"IEND", &[]);
            let _ = sink.flush();
        }
    }
}

/// Map an I/O result into the encoder's error type.
fn io(r: std::io::Result<()>) -> Result<(), EncodingError> {
    r.map_err(|e| EncodingError::Io(e.kind()))
}

/// Write one chunk: big-endian length, four-byte type, body, CRC over type+body.
fn write_chunk<W: Write>(sink: &mut W, kind: &[u8; 4], data: &[u8]) -> Result<(), EncodingError> {
    let len = u32::try_from(data.len())
        .map_err(|_| EncodingError::Parameter("chunk exceeds 2^32-1 bytes"))?;
    io(sink.write_all(&len.to_be_bytes()))?;
    io(sink.write_all(kind))?;
    io(sink.write_all(data))?;
    io(sink.write_all(&Crc32::of(&[kind, data]).to_be_bytes()))
}

/// Filter every scanline, choosing per row the filter with the smallest sum of
/// absolute signed residuals — the standard heuristic, and the one that makes
/// the compressor's job easy on gradients and flat fills alike.
///
/// The output is the filtered stream the zlib layer compresses: one filter-type
/// byte followed by `stride` bytes, per row.
fn filter_image(data: &[u8], stride: usize, bpp: usize) -> Vec<u8> {
    let rows = data.len() / stride.max(1);
    let mut out = Vec::with_capacity(rows * (stride + 1));
    let mut candidate = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    let zero = vec![0u8; stride];
    for row in 0..rows {
        let cur = &data[row * stride..(row + 1) * stride];
        let prev: &[u8] = if row == 0 {
            &zero
        } else {
            &data[(row - 1) * stride..row * stride]
        };
        let mut best_kind = 0u8;
        let mut best_score = u64::MAX;
        for kind in 0..5u8 {
            apply_filter(kind, cur, prev, bpp, &mut candidate);
            let score: u64 = candidate
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if score < best_score {
                best_score = score;
                best_kind = kind;
                best.copy_from_slice(&candidate);
            }
        }
        out.push(best_kind);
        out.extend_from_slice(&best);
    }
    out
}

/// Apply one PNG filter to a row (the forward direction of `decode::unfilter`).
fn apply_filter(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    match kind {
        0 => out.copy_from_slice(cur),
        1 => {
            for i in 0..cur.len() {
                let left = if i >= bpp { cur[i - bpp] } else { 0 };
                out[i] = cur[i].wrapping_sub(left);
            }
        }
        2 => {
            for i in 0..cur.len() {
                out[i] = cur[i].wrapping_sub(prev[i]);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let left = if i >= bpp { u16::from(cur[i - bpp]) } else { 0 };
                // `as u8` is the specified truncation of the floor average.
                out[i] = cur[i].wrapping_sub(((left + u16::from(prev[i])) / 2) as u8);
            }
        }
        _ => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                out[i] = cur[i].wrapping_sub(crate::decode::paeth_predictor(a, b, c));
            }
        }
    }
}
