// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PNG encoding: chunk framing, per-row filter selection, and the zlib stream.
//!
//! Everything aterm writes is 8-bit — framebuffer screenshots, the macOS
//! window capture, palette fixtures — so the encoder covers 8-bit
//! Grayscale/RGB/RGBA/Indexed with optional `PLTE`, `tRNS` and `sRGB`, and
//! refuses anything else at [`Encoder::write_header`] rather than emitting a
//! file it cannot describe.
//!
//! The pixels stream through: each scanline is filtered as it arrives and the
//! compressed stream goes to the sink as `IDAT` chunks of `IDAT_CHUNK` bytes
//! while it is produced. Beside its input and its sink the encoder holds
//! [`encoder_scratch_len`] bytes, and its output is at most
//! [`max_encoded_len`].

use std::io::Write;

use crate::checksum::Crc32;
use crate::deflate::{self, ZlibStream};
use crate::{BitDepth, ColorType, EncodingError, SIGNATURE, SrgbRenderingIntent, row_bytes};

/// The `IDAT` payload the writer emits each time the compressor has produced
/// this much: the compressed stream goes to the sink as it is made and is
/// never held whole. A stream shorter than this is one `IDAT`. The
/// specification permits any split, and every decoder concatenates.
const IDAT_CHUNK: usize = 64 * 1024;

/// Bytes a chunk adds around its payload: length, type and CRC.
const CHUNK_FRAMING: usize = 12;

/// The most bytes [`Encoder`] writes for an 8-bit `width` x `height` image of
/// `color_type`, whatever its pixels and its optional chunks: the signature,
/// `IHDR`, a full `PLTE` and `tRNS`, `sRGB`, `IEND`, and the `IDAT` chunks of
/// a zlib stream in which every filtered byte costs its longest fixed-Huffman
/// code (9 bits; a match never costs more per byte it covers). `None` when the
/// geometry overflows.
#[must_use]
#[inline]
pub fn max_encoded_len(width: u32, height: u32, color_type: ColorType) -> Option<usize> {
    let stride = row_bytes(width, color_type.samples(), BitDepth::Eight.bits())?;
    let filtered = stride.checked_add(1)?.checked_mul(height as usize)?;
    // Block header (3 bits), every byte at 9 bits, end-of-block (7 bits),
    // padded to a byte; then the 2-byte zlib header and the 4-byte Adler-32.
    let block = filtered.checked_mul(9)?.checked_add(3 + 7)?.div_ceil(8);
    let zlib = block.checked_add(2 + 4)?;
    let idat = zlib.checked_add(zlib.div_ceil(IDAT_CHUNK).checked_mul(CHUNK_FRAMING)?)?;
    let fixed = SIGNATURE.len()
        + (CHUNK_FRAMING + 13) // IHDR
        + (CHUNK_FRAMING + 1) // sRGB
        + (CHUNK_FRAMING + 768) // PLTE
        + (CHUNK_FRAMING + 256) // tRNS
        + CHUNK_FRAMING; // IEND
    idat.checked_add(fixed)
}

/// The most bytes a [`Writer`] holds beside its input and its sink while it
/// encodes `width`-pixel rows of `color_type`: the current and previous
/// scanline, the filter's candidate and chosen rows, the compressed bytes
/// waiting to fill an `IDAT` ([`pending_capacity`], one allocation that never
/// grows), and the compressor's fixed state. `None` when the geometry
/// overflows.
#[must_use]
#[inline]
pub fn encoder_scratch_len(width: u32, color_type: ColorType) -> Option<usize> {
    let stride = row_bytes(width, color_type.samples(), BitDepth::Eight.bits())?;
    let rows = stride.checked_mul(5)?.checked_add(1)?;
    rows.checked_add(pending_capacity(stride)?)?
        .checked_add(deflate::STATE_BYTES)
}

/// The compressor output reserved once for `stride`-byte rows, which
/// [`stream_rows`] never outgrows: each flush leaves less than [`IDAT_CHUNK`],
/// and one row's write then adds at most the row, its filter byte and the
/// lookahead the compressor held back before it at 9 bits a byte (a match
/// never costs more per byte it covers). The 6 bytes past them are the
/// accumulator's partial byte, the end-of-block code with its pad, and the
/// Adler-32, so `finish` (at most the held-back lookahead and that tail) fits
/// the same room. `None` when the geometry overflows.
#[inline]
fn pending_capacity(stride: usize) -> Option<usize> {
    let row_out = stride
        .checked_add(1 + deflate::LOOKAHEAD)?
        .checked_mul(9)?
        .div_ceil(8)
        .checked_add(1 + 1 + 4)?;
    IDAT_CHUNK.checked_add(row_out)
}

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
    /// Start a `width` x `height` PNG. Defaults to 8-bit grayscale.
    #[inline]
    pub const fn new(sink: W, width: u32, height: u32) -> Self {
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
    #[inline]
    pub const fn set_color(&mut self, color_type: ColorType) {
        self.color_type = color_type;
    }

    /// Set the bits per sample.
    #[inline]
    pub const fn set_depth(&mut self, bit_depth: BitDepth) {
        self.bit_depth = bit_depth;
    }

    /// Declare an `sRGB` chunk with this rendering intent.
    #[inline]
    pub const fn set_source_srgb(&mut self, intent: SrgbRenderingIntent) {
        self.srgb = Some(intent);
    }

    /// Set the `PLTE` palette (RGB triples). Required for
    /// [`ColorType::Indexed`].
    #[inline]
    pub fn set_palette(&mut self, palette: impl Into<Vec<u8>>) {
        self.palette = Some(palette.into());
    }

    /// Set the `tRNS` chunk (per-index alpha for an indexed image).
    #[inline]
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
            return Err(EncodingError::Parameter("only 8-bit output is supported"));
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
        let pending =
            pending_capacity(stride).ok_or(EncodingError::Parameter("image geometry overflows"))?;

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
            rows: self.height as usize,
            expected,
            pending,
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
    /// `None` once [`Writer::finish`] has consumed it, or once a streaming
    /// write failed part way, so `Drop` closes nothing over a partial stream.
    sink: Option<W>,
    stride: usize,
    rows: usize,
    expected: usize,
    /// The compressor output reserved for this row width
    /// ([`pending_capacity`]).
    pending: usize,
    /// Filter distance to the pixel on the left.
    bytes_per_pixel: usize,
    /// Palette entry count for an INDEXED image, so the samples can be checked
    /// against it. `None` for every other colour type.
    palette_entries: Option<usize>,
    wrote_data: bool,
}

impl<W: Write> Writer<W> {
    /// Write the whole image. `data` must be exactly the size the header
    /// implies: `height * row_bytes`. Both checks run before anything is
    /// written, so a refused buffer leaves the writer as it was.
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
        // An index past the palette is not a legal PNG. Output is 8-bit, so
        // one byte is one index.
        if let Some(entries) = self.palette_entries
            && let Some(&worst) = data.iter().max()
            && usize::from(worst) >= entries
        {
            return Err(EncodingError::Parameter(
                "indexed sample is past the end of the palette",
            ));
        }
        let stride = self.stride;
        self.write_rows(|y, row| {
            if let Some(scanline) = data.get(y * stride..(y + 1) * stride) {
                row.extend_from_slice(scanline);
            }
        })
    }

    /// Write the image a scanline at a time: `row(y, buf)` appends scanline
    /// `y`'s `row_bytes` to the empty `buf`, for `y` in `0..height`. Each row
    /// is filtered and compressed as it arrives, so the image is never held
    /// whole, filtered or compressed.
    ///
    /// # Errors
    /// [`EncodingError::WrongDataSize`] names a row that is not `row_bytes`
    /// long (`expected` and `got` are that row's), [`EncodingError::Parameter`]
    /// on a second call or on an indexed sample past the end of the palette,
    /// and [`EncodingError::Io`] from the sink. An error after the first row
    /// may leave `IDAT` chunks in the sink: the file is incomplete, the writer
    /// closes nothing over it, and the caller discards it.
    pub fn write_rows(
        &mut self,
        mut row: impl FnMut(usize, &mut Vec<u8>),
    ) -> Result<(), EncodingError> {
        if self.wrote_data {
            return Err(EncodingError::Parameter(
                "image data has already been written",
            ));
        }
        let sink = self
            .sink
            .as_mut()
            .ok_or(EncodingError::Parameter("writer already finished"))?;
        self.wrote_data = true;
        let streamed = stream_rows(
            sink,
            Geometry {
                stride: self.stride,
                rows: self.rows,
                pending: self.pending,
                bytes_per_pixel: self.bytes_per_pixel,
                palette_entries: self.palette_entries,
            },
            &mut row,
        );
        if streamed.is_err() {
            self.sink = None;
        }
        streamed
    }

    /// Write `IEND` and flush.
    ///
    /// # Errors
    /// [`EncodingError::Io`] if the sink refuses the final write;
    /// [`EncodingError::Parameter`] after a failed image-data write, whose
    /// file is incomplete.
    pub fn finish(mut self) -> Result<(), EncodingError> {
        let Some(mut sink) = self.sink.take() else {
            return Err(EncodingError::Parameter(
                "image data failed part way; the file is incomplete",
            ));
        };
        write_chunk(&mut sink, b"IEND", &[])?;
        io(sink.flush())
    }
}

impl<W: Write> Drop for Writer<W> {
    fn drop(&mut self) {
        // Call sites rely on drop closing the file. A failure here cannot be
        // reported; a best-effort IEND beats a file silently missing one.
        if let Some(mut sink) = self.sink.take() {
            let _ = write_chunk(&mut sink, b"IEND", &[]);
            let _ = sink.flush();
        }
    }
}

/// The scanline layout [`stream_rows`] encodes.
struct Geometry {
    stride: usize,
    rows: usize,
    /// [`pending_capacity`] of `stride`.
    pending: usize,
    bytes_per_pixel: usize,
    palette_entries: Option<usize>,
}

/// Filter each row as `row` supplies it, compress it, and hand the sink every
/// full `IDAT` as it fills; then the rest of the stream. The compressed bytes
/// wait in the one allocation of `pending` bytes the stream was born with.
fn stream_rows<W: Write>(
    sink: &mut W,
    geometry: Geometry,
    row: &mut impl FnMut(usize, &mut Vec<u8>),
) -> Result<(), EncodingError> {
    let Geometry {
        stride,
        rows,
        pending,
        bytes_per_pixel,
        palette_entries,
    } = geometry;
    let mut filter = RowFilter::new(stride, bytes_per_pixel);
    let mut cur = Vec::with_capacity(stride);
    // The row above the first is zeros.
    let mut prev = vec![0u8; stride];
    let mut zlib = ZlibStream::with_output_capacity(pending);
    for y in 0..rows {
        cur.clear();
        row(y, &mut cur);
        if cur.len() != stride {
            return Err(EncodingError::WrongDataSize {
                expected: stride,
                got: cur.len(),
            });
        }
        // An index past the palette is not a legal PNG. Output is 8-bit, so
        // one byte is one index.
        if let Some(entries) = palette_entries
            && let Some(&worst) = cur.iter().max()
            && usize::from(worst) >= entries
        {
            return Err(EncodingError::Parameter(
                "indexed sample is past the end of the palette",
            ));
        }
        zlib.write(filter.filter(&cur, &prev));
        debug_assert_eq!(
            zlib.output().capacity(),
            pending,
            "a row outgrew the output"
        );
        write_full_idat_chunks(sink, zlib.output())?;
        std::mem::swap(&mut cur, &mut prev);
    }
    let tail = zlib.finish();
    debug_assert_eq!(
        tail.capacity(),
        pending,
        "the stream's end outgrew the output"
    );
    for chunk in tail.chunks(IDAT_CHUNK) {
        write_chunk(sink, b"IDAT", chunk)?;
    }
    Ok(())
}

/// Hand the sink every full [`IDAT_CHUNK`] of `pending`, keeping the rest.
fn write_full_idat_chunks<W: Write>(
    sink: &mut W,
    pending: &mut Vec<u8>,
) -> Result<(), EncodingError> {
    let (chunks, _) = pending.as_chunks::<IDAT_CHUNK>();
    if chunks.is_empty() {
        return Ok(());
    }
    for chunk in chunks {
        write_chunk(sink, b"IDAT", chunk)?;
    }
    let full = chunks.len() * IDAT_CHUNK;
    pending.drain(..full);
    Ok(())
}

/// Map an I/O result into the encoder's error type.
#[inline]
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

/// Filters one scanline at a time, choosing per row the filter with the
/// smallest sum of absolute signed residuals — the standard heuristic, and the
/// one that makes the compressor's job easy on gradients and flat fills alike.
struct RowFilter {
    bytes_per_pixel: usize,
    candidate: Vec<u8>,
    /// The filtered row the zlib layer compresses: the filter-type byte, then
    /// the row's residuals.
    best: Vec<u8>,
}

impl RowFilter {
    fn new(stride: usize, bytes_per_pixel: usize) -> Self {
        Self {
            bytes_per_pixel,
            candidate: vec![0u8; stride],
            best: vec![0u8; stride + 1],
        }
    }

    /// `cur` filtered against `prev` (the row above; zeros for the first),
    /// both `stride` bytes: the filter-type byte followed by the residuals.
    fn filter(&mut self, cur: &[u8], prev: &[u8]) -> &[u8] {
        let mut best_score = u64::MAX;
        for kind in 0..5u8 {
            apply_filter(kind, cur, prev, self.bytes_per_pixel, &mut self.candidate);
            let score: u64 = self
                .candidate
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if score < best_score {
                best_score = score;
                self.best[0] = kind;
                self.best[1..].copy_from_slice(&self.candidate);
            }
        }
        &self.best
    }
}

/// Apply one PNG filter to a row (the forward direction of `decode::unfilter`).
#[inline]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference whole-image filter pass used as an oracle for `RowFilter`.
    fn whole_image_filter(data: &[u8], stride: usize, bpp: usize) -> Vec<u8> {
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

    /// Every chunk of a PNG this encoder wrote, as (type, payload).
    fn chunks(png: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut rest = &png[SIGNATURE.len()..];
        let mut out = Vec::new();
        while !rest.is_empty() {
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let kind = [rest[4], rest[5], rest[6], rest[7]];
            out.push((kind, &rest[8..8 + len]));
            rest = &rest[8 + len + 4..];
        }
        out
    }

    /// Verify streamed `IDAT` payloads match the one-shot compression of
    /// `whole_image_filter` across gradient, noise, and flat images at 1, 3,
    /// and 4 bytes per pixel.
    #[test]
    fn the_streamed_idat_is_the_whole_image_encoders_stream() {
        let mut state = 0x1234_5678u32;
        let mut multi_chunk = false;
        for (samples, color) in [
            (1usize, ColorType::Grayscale),
            (3, ColorType::Rgb),
            (4, ColorType::Rgba),
        ] {
            for (shape, w, h) in [
                ("gradient", 61u32, 47u32),
                ("noise", 211, 160),
                ("flat", 300, 2),
            ] {
                let stride = w as usize * samples;
                let data: Vec<u8> = (0..stride * h as usize)
                    .map(|i| match shape {
                        "gradient" => ((i % stride) * 3 + i / stride) as u8,
                        "noise" => {
                            state ^= state << 13;
                            state ^= state >> 17;
                            state ^= state << 5;
                            (state >> 24) as u8
                        }
                        _ => 7,
                    })
                    .collect();
                let mut png = Vec::new();
                let mut encoder = Encoder::new(&mut png, w, h);
                encoder.set_color(color);
                let mut writer = encoder.write_header().expect("header");
                writer.write_image_data(&data).expect("data");
                writer.finish().expect("finish");

                let idats: Vec<&[u8]> = chunks(&png)
                    .into_iter()
                    .filter(|(kind, _)| kind == b"IDAT")
                    .map(|(_, payload)| payload)
                    .collect();
                let (last, full) = idats.split_last().expect("one IDAT at least");
                assert!(
                    full.iter().all(|chunk| chunk.len() == IDAT_CHUNK)
                        && !last.is_empty()
                        && last.len() <= IDAT_CHUNK,
                    "{shape}/{samples}: IDATs are full chunks and one remainder"
                );
                multi_chunk |= !full.is_empty();
                let oracle = deflate::zlib_compress(&whole_image_filter(&data, stride, samples));
                assert_eq!(idats.concat(), oracle, "{shape}/{samples}: the zlib stream");
            }
        }
        assert!(multi_chunk, "the noise images span more than one IDAT");
    }
}
