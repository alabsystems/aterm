// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PNG decoding: chunk structure, filters, Adam7 de-interlacing, and the two
//! output transforms.
//!
//! The pipeline is per-scanline, never per-image-buffer-of-u16s: a 4096x4096
//! RGBA image is 64 MiB of output, and materialising an intermediate at wider
//! samples would double or quadruple that for no gain. One row of source
//! samples is unfiltered, transformed, and written into the caller's buffer
//! before the next is touched.
//!
//! Adam7 is the one place that shape bends: an interlaced pass row's pixels land
//! at scattered (x, y) positions, so each pixel is written individually — which
//! is also why [`write_pixel`] has to know how to place a sub-byte sample at an
//! arbitrary pixel index.

use std::io::Read;

use crate::checksum::Crc32;
use crate::{
    BitDepth, ColorType, DecodingError, Info, Limits, OutputInfo, SIGNATURE, SrgbRenderingIntent,
    Transformations, row_bytes,
};

/// Reads a PNG's header and image data. Mirrors the shape of the retired
/// crate's decoder so consuming code reads the same.
pub struct Decoder<R> {
    source: R,
    limits: Limits,
    transformations: Transformations,
}

impl<R: Read> Decoder<R> {
    /// Wrap a source of PNG bytes.
    pub fn new(source: R) -> Self {
        Self {
            source,
            limits: Limits::default(),
            transformations: Transformations::IDENTITY,
        }
    }

    /// Bound what a decode may allocate. See [`Limits`].
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Choose the output transforms. See [`Transformations`].
    pub fn set_transformations(&mut self, transformations: Transformations) {
        self.transformations = transformations;
    }

    /// Parse the header chunks and collect the compressed image data, WITHOUT
    /// decompressing it. Dimensions are known after this returns, which is what
    /// lets a caller reject an oversized image before anything is allocated
    /// for its pixels.
    ///
    /// # Errors
    /// [`DecodingError`] for a bad signature, a truncated or CRC-failing chunk,
    /// an illegal header, or a structure PNG does not allow.
    pub fn read_info(mut self) -> Result<Reader, DecodingError> {
        let mut bytes = Vec::new();
        self.source
            .read_to_end(&mut bytes)
            .map_err(|e| DecodingError::Io(e.kind()))?;
        Reader::parse(&bytes, self.limits, self.transformations)
    }
}

/// A parsed PNG, ready to produce pixels.
pub struct Reader {
    info: Info,
    /// The concatenated IDAT payload — one zlib stream.
    compressed: Vec<u8>,
    limits: Limits,
    transformations: Transformations,
    output: OutputInfo,
}

/// One chunk, as it sits in the file.
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    /// Offset of the next chunk.
    next: usize,
}

/// Read the chunk starting at `offset`, verifying its CRC.
fn read_chunk(bytes: &[u8], offset: usize) -> Result<Chunk<'_>, DecodingError> {
    let header = bytes
        .get(offset..offset + 8)
        .ok_or(DecodingError::Truncated)?;
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    // PNG caps a chunk length at 2^31 - 1; a longer one is malformed, and
    // rejecting it here keeps the arithmetic below inside usize on 32-bit.
    if len > 0x7FFF_FFFF {
        return Err(DecodingError::Malformed("chunk length exceeds 2^31-1"));
    }
    let len = len as usize;
    let kind = [header[4], header[5], header[6], header[7]];
    let data_start = offset + 8;
    let data = bytes
        .get(data_start..data_start + len)
        .ok_or(DecodingError::Truncated)?;
    let crc_bytes = bytes
        .get(data_start + len..data_start + len + 4)
        .ok_or(DecodingError::Truncated)?;
    let stored = u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
    if Crc32::of(&[&kind, data]) != stored {
        return Err(DecodingError::BadCrc { chunk: kind });
    }
    Ok(Chunk {
        kind,
        data,
        next: data_start + len + 4,
    })
}

/// Every output pixel an indexed image can produce, resolved once per decode.
///
/// PNG allows an indexed image a bit depth of 1, 2, 4 or 8, so an index is at
/// most 255 and there are at most 256 distinct answers — for the whole image,
/// because PLTE and tRNS are fixed once the header is read. Resolving them up
/// front turns the inner loop from a range-checked palette slice, three
/// widenings, an `Option` and a second bounds check into one indexed load.
type PaletteLut = [[u16; 4]; 256];

/// Resolve the palette (and tRNS, when present) into [`PaletteLut`].
///
/// Every rule the per-pixel path applied is applied here instead, unchanged and
/// in the same order — including the two that look like edge cases and are not:
/// an index past the end of PLTE resolves to opaque black rather than failing
/// the whole image, and a tRNS shorter than the palette leaves its missing
/// entries fully opaque (PNG specification 11.3.2.1).
fn build_palette_lut(info: &Info) -> Result<Box<PaletteLut>, DecodingError> {
    let palette = info
        .palette
        .as_ref()
        .ok_or(DecodingError::Malformed("indexed image with no PLTE"))?;
    let mut lut: Box<PaletteLut> = Box::new([[0u16; 4]; 256]);
    for (index, slot) in lut.iter_mut().enumerate() {
        let base = index * 3;
        // An index past the palette is a malformed file, but refusing the WHOLE
        // image over one stray byte is the worse failure: the retired crate
        // renders the file and gives the undefined index a defined colour, and
        // this crate's own header cites a colour-emoji strike that vanished
        // entirely rather than rendering with one wrong pixel. So: black and
        // opaque, which is exactly the entry `png` leaves in its palette table
        // for an index it never filled in.
        const UNDEFINED_ENTRY: &[u8; 3] = &[0, 0, 0];
        let entry = palette
            .get(base..base + 3)
            .unwrap_or(UNDEFINED_ENTRY.as_slice());
        slot[0] = u16::from(entry[0]);
        slot[1] = u16::from(entry[1]);
        slot[2] = u16::from(entry[2]);
        if let Some(trns) = &info.trns {
            // tRNS may be SHORTER than the palette; missing entries are fully
            // opaque (PNG specification 11.3.2.1). With no tRNS at all the
            // alpha slot stays 0 and the output colour type is Rgb, which never
            // reads it — the per-pixel path left it 0 for the same reason.
            slot[3] = u16::from(trns.get(index).copied().unwrap_or(255));
        }
    }
    Ok(lut)
}

impl Reader {
    fn parse(
        bytes: &[u8],
        limits: Limits,
        transformations: Transformations,
    ) -> Result<Self, DecodingError> {
        if bytes.len() < SIGNATURE.len() || bytes[..SIGNATURE.len()] != SIGNATURE {
            return Err(DecodingError::NotPng);
        }

        let first = read_chunk(bytes, SIGNATURE.len())?;
        if &first.kind != b"IHDR" || first.data.len() != 13 {
            return Err(DecodingError::Malformed(
                "first chunk is not a 13-byte IHDR",
            ));
        }
        let d = first.data;
        let width = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
        let height = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        let bit_depth =
            BitDepth::from_u8(d[8]).ok_or(DecodingError::Malformed("illegal bit depth"))?;
        let color_type =
            ColorType::from_u8(d[9]).ok_or(DecodingError::Malformed("illegal colour type"))?;
        if width == 0 || height == 0 {
            return Err(DecodingError::Malformed("zero image dimension"));
        }
        if !color_type.allows_depth(bit_depth) {
            return Err(DecodingError::Malformed(
                "bit depth is not legal for this colour type",
            ));
        }
        if d[10] != 0 {
            return Err(DecodingError::Malformed("unknown compression method"));
        }
        if d[11] != 0 {
            return Err(DecodingError::Malformed("unknown filter method"));
        }
        let interlaced = match d[12] {
            0 => false,
            1 => true,
            _ => return Err(DecodingError::Malformed("unknown interlace method")),
        };

        let mut info = Info {
            width,
            height,
            bit_depth,
            color_type,
            interlaced,
            srgb: None,
            palette: None,
            trns: None,
        };
        let mut compressed = Vec::new();
        let mut seen_idat = false;
        let mut seen_plte = false;
        let mut seen_trns = false;
        // Set once the run of IDATs has been interrupted by any other chunk.
        // PNG requires the IDATs to be CONSECUTIVE, and a decoder that quietly
        // concatenates across a gap accepts files the retired crate refused.
        let mut idat_run_ended = false;
        let mut seen_iend = false;
        let mut offset = first.next;

        while offset < bytes.len() {
            let chunk = read_chunk(bytes, offset)?;
            offset = chunk.next;
            if &chunk.kind != b"IDAT" {
                idat_run_ended = seen_idat;
            }
            // A ZERO-LENGTH chunk carries nothing to parse, and the retired
            // crate never parsed one — its reader jumps straight from the length
            // to the CRC. So an empty `tRNS` is ABSENT rather than present-
            // and-empty, which matters: `output_info` branches on
            // `trns.is_some()`, so parsing it would change the output's SHAPE
            // (Rgba, w*4) and not merely its pixels. Same for an empty `PLTE`
            // or `sRGB`. IDAT and IEND are structural and still count.
            if chunk.data.is_empty() && !matches!(&chunk.kind, b"IDAT" | b"IEND") {
                continue;
            }
            match &chunk.kind {
                b"IHDR" => return Err(DecodingError::Malformed("duplicate IHDR")),
                b"PLTE" => {
                    if seen_idat {
                        return Err(DecodingError::Malformed("PLTE after IDAT"));
                    }
                    if seen_plte {
                        return Err(DecodingError::Malformed("duplicate PLTE"));
                    }
                    seen_plte = true;
                    if chunk.data.len() % 3 != 0 || chunk.data.len() > 768 {
                        return Err(DecodingError::Malformed("PLTE is not 1..=256 RGB triples"));
                    }
                    info.palette = Some(chunk.data.to_vec());
                }
                b"tRNS" => {
                    if seen_idat {
                        return Err(DecodingError::Malformed("tRNS after IDAT"));
                    }
                    if seen_trns {
                        return Err(DecodingError::Malformed("duplicate tRNS"));
                    }
                    seen_trns = true;
                    let ok = match color_type {
                        ColorType::Grayscale => chunk.data.len() == 2,
                        ColorType::Rgb => chunk.data.len() == 6,
                        // An indexed tRNS names palette entries, so it can only
                        // be read once the palette is known. Before PLTE it is
                        // not merely early, it is unreadable.
                        ColorType::Indexed => {
                            if info.palette.is_none() {
                                return Err(DecodingError::Malformed("tRNS before PLTE"));
                            }
                            true
                        }
                        // tRNS is meaningless where an alpha channel already exists.
                        ColorType::GrayscaleAlpha | ColorType::Rgba => false,
                    };
                    if !ok {
                        return Err(DecodingError::Malformed(
                            "tRNS length is wrong for this colour type",
                        ));
                    }
                    info.trns = Some(if color_type == ColorType::Indexed {
                        let entries = info.palette.as_ref().map_or(0, |p| p.len() / 3);
                        if chunk.data.len() > entries {
                            // "The tRNS chunk shall not contain more alpha
                            // values than there are palette entries"
                            // (specification 11.3.2.1). One that does is
                            // IGNORED — the chunk still promotes the output to
                            // RGBA, but every entry reads back opaque. That is
                            // what the retired crate does, and reading the
                            // over-long chunk anyway would silently make a
                            // malformed strike translucent.
                            Vec::new()
                        } else {
                            chunk.data.to_vec()
                        }
                    } else {
                        chunk.data.to_vec()
                    });
                }
                b"sRGB" => {
                    let intent = chunk
                        .data
                        .first()
                        .copied()
                        .and_then(SrgbRenderingIntent::from_u8)
                        .ok_or(DecodingError::Malformed("illegal sRGB rendering intent"))?;
                    info.srgb = Some(intent);
                }
                b"IDAT" => {
                    if idat_run_ended {
                        return Err(DecodingError::Malformed("IDAT chunks are not consecutive"));
                    }
                    seen_idat = true;
                    compressed.extend_from_slice(chunk.data);
                }
                b"IEND" => {
                    seen_iend = true;
                    break;
                }
                // Every other chunk is ancillary to what aterm reads. Its CRC has
                // already been checked by `read_chunk`, which is the part that
                // matters: a file whose chunk structure does not hold together
                // is not silently half-read.
                _ => {}
            }
        }

        if !seen_iend {
            return Err(DecodingError::Truncated);
        }
        if !seen_idat {
            return Err(DecodingError::Malformed("no IDAT"));
        }
        if color_type == ColorType::Indexed && info.palette.is_none() {
            return Err(DecodingError::Malformed("indexed image with no PLTE"));
        }

        let output = output_info(&info, transformations)?;
        Ok(Self {
            info,
            compressed,
            limits,
            transformations,
            output,
        })
    }

    /// What the file says about itself, before transforms.
    #[must_use]
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// Bytes [`Reader::next_frame`] will fill.
    #[must_use]
    pub fn output_buffer_size(&self) -> usize {
        self.output.buffer_size()
    }

    /// Decode the image into `buf`.
    ///
    /// # Errors
    /// [`DecodingError`] if `buf` is too small, the compressed data is corrupt,
    /// a filter byte is not one of the five defined types, or the image needs
    /// more than the configured [`Limits`].
    pub fn next_frame(&mut self, buf: &mut [u8]) -> Result<OutputInfo, DecodingError> {
        let out_size = self.output.buffer_size();
        if buf.len() < out_size {
            return Err(DecodingError::Malformed("output buffer is too small"));
        }
        if out_size > self.limits.bytes {
            return Err(DecodingError::LimitsExceeded);
        }

        let samples = self.info.color_type.samples();
        let depth = self.info.bit_depth.bits();
        // Bytes the filtered, decompressed stream must hold: every pass's rows,
        // each prefixed by its one filter byte.
        let mut filtered_len = 0usize;
        for (_, pass_w, pass_h) in self.passes() {
            let stride = row_bytes(pass_w, samples, depth).ok_or(DecodingError::LimitsExceeded)?;
            filtered_len = filtered_len
                .checked_add(
                    (stride.checked_add(1).ok_or(DecodingError::LimitsExceeded)?)
                        .checked_mul(pass_h as usize)
                        .ok_or(DecodingError::LimitsExceeded)?,
                )
                .ok_or(DecodingError::LimitsExceeded)?;
        }
        if filtered_len > self.limits.bytes {
            return Err(DecodingError::LimitsExceeded);
        }

        // A well-formed stream decompresses to EXACTLY `filtered_len`. The slack
        // admits an encoder that padded the zlib stream without letting a
        // decompression bomb through: the bound is still the image's own size
        // plus one maximal DEFLATE stored block, which is the largest single
        // block of padding an encoder can emit (RFC 1951 caps LEN at 65535).
        // Past that the excess is not padding, and it gets its own error rather
        // than being reported as corruption.
        const TRAILING_SLACK: usize = 65_535;
        // The PREFIX form: the Adler-32 sits at the end of the zlib STREAM, not
        // at the end of the buffer. A concatenated IDAT payload with any bytes
        // after the stream — one is enough — would otherwise have its checksum
        // read out of the padding and a perfectly good image reported corrupt.
        let raw = match aterm_codec::inflate::zlib_decompress_prefix(
            &self.compressed,
            filtered_len.saturating_add(TRAILING_SLACK),
        ) {
            Ok((raw, _consumed)) => raw,
            Err(aterm_codec::inflate::InflateError::OutputTooLarge) => {
                return Err(DecodingError::ExcessImageData);
            }
            Err(_) => return Err(DecodingError::BadZlib),
        };
        if raw.len() < filtered_len {
            return Err(DecodingError::Truncated);
        }

        // Resolved once, here, rather than per pixel in `transform_pixel`.
        // `None` for every other colour type and whenever EXPAND is off, which
        // is what keeps the fast path from claiming rows it must not touch.
        let palette_lut = if self.transformations.contains(Transformations::EXPAND)
            && self.info.color_type == ColorType::Indexed
        {
            Some(build_palette_lut(&self.info)?)
        } else {
            None
        };
        let palette_lut = palette_lut.as_deref();

        let bpp = (samples * depth as usize).div_ceil(8).max(1);
        let mut cursor = 0usize;
        // Reused across rows: `prev` is the previous unfiltered row, which the
        // Up/Average/Paeth filters read.
        let mut prev: Vec<u8> = Vec::new();
        let mut current: Vec<u8> = Vec::new();

        for (pass, pass_w, pass_h) in self.passes() {
            let stride = row_bytes(pass_w, samples, depth).ok_or(DecodingError::LimitsExceeded)?;
            prev.clear();
            prev.resize(stride, 0);
            current.clear();
            current.resize(stride, 0);
            for row in 0..pass_h {
                let filter = raw[cursor];
                cursor += 1;
                current.copy_from_slice(&raw[cursor..cursor + stride]);
                cursor += stride;
                unfilter(filter, bpp, &prev, &mut current)?;
                if self.info.interlaced {
                    self.place_interlaced_row(buf, pass, row, pass_w, &current, palette_lut)?;
                } else {
                    self.place_row(buf, row, pass_w, &current, palette_lut)?;
                }
                std::mem::swap(&mut prev, &mut current);
            }
        }

        Ok(self.output)
    }

    /// Each pass that has pixels in it, as `(Adam7 pass number, width, height)`:
    /// one entry for a progressive image, up to seven for an interlaced one.
    ///
    /// The pass NUMBER travels with the geometry because empty passes are
    /// filtered out — a 3x5 image has no pass 2 — so a positional index into
    /// [`ADAM7`] would silently address the wrong origin and stride.
    fn passes(&self) -> impl Iterator<Item = (usize, u32, u32)> + '_ {
        let (w, h, interlaced) = (self.info.width, self.info.height, self.info.interlaced);
        (0..if interlaced { 7 } else { 1 }).filter_map(move |pass| {
            if interlaced {
                let (pw, ph) = adam7_pass_size(pass, w, h);
                (pw > 0 && ph > 0).then_some((pass, pw, ph))
            } else {
                Some((0, w, h))
            }
        })
    }

    /// Transform one non-interlaced source row into the output buffer.
    fn place_row(
        &self,
        buf: &mut [u8],
        y: u32,
        width: u32,
        row: &[u8],
        palette_lut: Option<&PaletteLut>,
    ) -> Result<(), DecodingError> {
        let line = self.output.line_size;
        let start = (y as usize) * line;
        let out_row = &mut buf[start..start + line];
        // When the requested transforms turn out to be no-ops for THIS image —
        // the overwhelmingly common case, an 8-bit RGBA screenshot asked for
        // EXPAND | STRIP_16 — the output format equals the source format and the
        // row is a straight copy. Going through `transform_pixel` per pixel
        // would compute the same bytes far more slowly.
        if self.output.color_type == self.info.color_type
            && self.output.bit_depth == self.info.bit_depth
        {
            out_row.copy_from_slice(&row[..line]);
            return Ok(());
        }
        let n = self.output.color_type.samples();
        for x in 0..width {
            let pixel = self.transform_pixel(row, x, palette_lut)?;
            write_pixel(out_row, x as usize, &pixel, n, self.output.bit_depth);
        }
        Ok(())
    }

    /// Transform one Adam7 pass row, scattering its pixels to their true
    /// positions in the full image.
    fn place_interlaced_row(
        &self,
        buf: &mut [u8],
        pass: usize,
        row: u32,
        pass_w: u32,
        data: &[u8],
        palette_lut: Option<&PaletteLut>,
    ) -> Result<(), DecodingError> {
        let line = self.output.line_size;
        let (x0, y0, dx, dy) = ADAM7[pass];
        let y = y0 + row * dy;
        let start = (y as usize) * line;
        let out_row = &mut buf[start..start + line];
        let n = self.output.color_type.samples();
        for i in 0..pass_w {
            let x = x0 + i * dx;
            let pixel = self.transform_pixel(data, i, palette_lut)?;
            write_pixel(out_row, x as usize, &pixel, n, self.output.bit_depth);
        }
        Ok(())
    }

    /// Read source pixel `x` out of an unfiltered row and produce its output
    /// samples, as values at the OUTPUT bit depth.
    fn transform_pixel(
        &self,
        row: &[u8],
        x: u32,
        palette_lut: Option<&PaletteLut>,
    ) -> Result<[u16; 4], DecodingError> {
        let depth = self.info.bit_depth.bits();
        let samples = self.info.color_type.samples();
        // Indexed + EXPAND, resolved. The caller built this exactly when the
        // branch below would have run, so reaching it here is the same test.
        // Indexed depth is 1, 2, 4 or 8 (the header parser rejects anything
        // else), so the sample cannot exceed 255 and the mask is a no-op that
        // simply lets the compiler see it — no bounds check, no panic edge.
        if let Some(lut) = palette_lut {
            let index = read_sample(row, x as usize, depth) as usize;
            return Ok(lut[index & 0xFF]);
        }
        let mut src = [0u16; 4];
        for (s, slot) in src.iter_mut().enumerate().take(samples) {
            *slot = read_sample(row, (x as usize) * samples + s, depth);
        }

        let expand = self.transformations.contains(Transformations::EXPAND);
        let strip = self.transformations.contains(Transformations::STRIP_16);
        // Full-scale value at the source depth: what "opaque" and "white" are.
        let src_max = ((1u32 << depth) - 1) as u16;

        // Indexed + EXPAND does not appear below: it is answered above, out of
        // the table. Keeping a second copy of the palette rules here would be
        // unreachable code that drifts from the one that runs.
        let mut out = [0u16; 4];

        match self.info.color_type {
            ColorType::Grayscale => {
                out[0] = src[0];
                if expand {
                    if depth < 8 {
                        // Scale the narrow sample across the full 8-bit range so
                        // 1-bit white is 255, not 1.
                        out[0] = (u32::from(src[0]) * 255 / u32::from(src_max)) as u16;
                    }
                    if let Some(trns) = &self.info.trns {
                        // The key is stored as two bytes whatever the depth, and
                        // below 16 bits only the LOW byte is the sample — the
                        // high byte is required to be zero and is ignored, not
                        // compared. Comparing the full 16-bit word makes a
                        // malformed key silently never match, where the retired
                        // crate makes it match.
                        let key = mask_trns_key(u16::from_be_bytes([trns[0], trns[1]]), depth);
                        // The comparison is against the RAW sample, before any
                        // scaling — tRNS names a stored value, not a colour.
                        let opaque = if depth < 8 { 255 } else { src_max };
                        out[1] = if src[0] == key { 0 } else { opaque };
                    }
                }
            }
            ColorType::Rgb => {
                out[..3].copy_from_slice(&src[..3]);
                if expand && let Some(trns) = &self.info.trns {
                    let key = [
                        mask_trns_key(u16::from_be_bytes([trns[0], trns[1]]), depth),
                        mask_trns_key(u16::from_be_bytes([trns[2], trns[3]]), depth),
                        mask_trns_key(u16::from_be_bytes([trns[4], trns[5]]), depth),
                    ];
                    out[3] = if src[..3] == key { 0 } else { src_max };
                }
            }
            ColorType::GrayscaleAlpha => {
                out[..2].copy_from_slice(&src[..2]);
            }
            ColorType::Rgba => {
                out[..4].copy_from_slice(&src[..4]);
            }
            // Indexed WITHOUT expand: the index passes through untouched.
            ColorType::Indexed => {
                out[0] = src[0];
            }
        }

        if strip && depth == 16 {
            for slot in &mut out {
                *slot >>= 8;
            }
        }
        Ok(out)
    }
}

/// A `tRNS` colour key, reduced to the width the samples actually have.
///
/// PNG stores the key as a 16-bit big-endian value for every depth, but at a
/// depth below 16 only the low byte holds the sample; the high byte is required
/// to be zero. A file that puts something else there is malformed, and the
/// question is only what a decoder does with it. `png` masks (its `parse_trns`
/// does `vec[0] = vec[1]; vec.truncate(1)` for `bit_depth < 16`), so a key of
/// `0x012A` at depth 8 matches the sample `0x2A`. Comparing the full word
/// instead would make that key match nothing at all — a silent pixel-level
/// divergence on exactly the malformed input where the two decoders should
/// agree.
fn mask_trns_key(key: u16, depth: u32) -> u16 {
    if depth < 16 { key & 0x00FF } else { key }
}

/// Read sample number `index` from a packed row at `depth` bits per sample.
/// Out-of-range reads yield zero rather than panicking; the row length is
/// derived from the header, so this can only be reached on a malformed file.
fn read_sample(row: &[u8], index: usize, depth: u32) -> u16 {
    match depth {
        16 => {
            let b = index * 2;
            match (row.get(b), row.get(b + 1)) {
                (Some(&hi), Some(&lo)) => u16::from_be_bytes([hi, lo]),
                _ => 0,
            }
        }
        8 => u16::from(row.get(index).copied().unwrap_or(0)),
        // 1, 2 or 4 bits: samples are packed most-significant first.
        _ => {
            let per_byte = 8 / depth as usize;
            let byte = row.get(index / per_byte).copied().unwrap_or(0);
            let shift = 8 - depth as usize * (index % per_byte + 1);
            let mask = (1u16 << depth) - 1;
            (u16::from(byte) >> shift) & mask
        }
    }
}

/// Write one output pixel's `n` samples at pixel index `x`.
///
/// Sub-byte output only ever happens when the transform is a no-op for this
/// image, and the only sub-byte colour types are Grayscale and Indexed — both
/// one sample per pixel — so the packed case never has to interleave channels.
fn write_pixel(row: &mut [u8], x: usize, samples: &[u16; 4], n: usize, depth: BitDepth) {
    match depth {
        BitDepth::Sixteen => {
            let base = x * n * 2;
            for (s, &value) in samples.iter().enumerate().take(n) {
                let b = base + s * 2;
                if let Some(slot) = row.get_mut(b..b + 2) {
                    slot.copy_from_slice(&value.to_be_bytes());
                }
            }
        }
        BitDepth::Eight => {
            let base = x * n;
            for (s, &value) in samples.iter().enumerate().take(n) {
                if let Some(slot) = row.get_mut(base + s) {
                    // `as u8` is exact: an 8-bit output sample is <= 255.
                    *slot = value as u8;
                }
            }
        }
        // 1, 2 or 4 bits, one sample per pixel, packed most-significant first.
        _ => {
            let bits = depth.bits() as usize;
            let per_byte = 8 / bits;
            if let Some(byte) = row.get_mut(x / per_byte) {
                let shift = 8 - bits * (x % per_byte + 1);
                // `as u8` is exact: the mask is at most 0x0F.
                let mask = (((1u16 << bits) - 1) as u8) << shift;
                *byte = (*byte & !mask) | (((samples[0] as u8) << shift) & mask);
            }
        }
    }
}

/// Adam7 pass origins and strides: `(x0, y0, dx, dy)` for passes 1..7.
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// How many pixels wide and tall Adam7 pass `pass` is for a `w` x `h` image.
fn adam7_pass_size(pass: usize, w: u32, h: u32) -> (u32, u32) {
    let (x0, y0, dx, dy) = ADAM7[pass];
    let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
    let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
    (pw, ph)
}

/// Reverse one scanline filter in place. `prev` is the already-unfiltered row
/// above (all zeroes for the first row of a pass), `bpp` the byte distance to
/// the pixel to the left.
fn unfilter(filter: u8, bpp: usize, prev: &[u8], row: &mut [u8]) -> Result<(), DecodingError> {
    match filter {
        0 => {}
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= bpp { u16::from(row[i - bpp]) } else { 0 };
                let up = u16::from(prev[i]);
                // `as u8` is the specified truncation of the floor average.
                row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                row[i] = row[i].wrapping_add(paeth_predictor(a, b, c));
            }
        }
        _ => return Err(DecodingError::Malformed("unknown scanline filter type")),
    }
    Ok(())
}

/// The Paeth predictor (PNG specification 9.4): pick whichever of left, above
/// and upper-left is closest to `a + b - c`, ties going to `a` then `b`.
///
/// Shared with the encoder, which subtracts exactly what this adds back.
pub(crate) fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Work out the colour type and bit depth the decoder will actually produce,
/// and the row stride that follows from them.
fn output_info(info: &Info, t: Transformations) -> Result<OutputInfo, DecodingError> {
    let mut color_type = info.color_type;
    let mut bit_depth = info.bit_depth;

    if t.contains(Transformations::EXPAND) {
        match color_type {
            ColorType::Indexed => {
                color_type = if info.trns.is_some() {
                    ColorType::Rgba
                } else {
                    ColorType::Rgb
                };
                bit_depth = BitDepth::Eight;
            }
            ColorType::Grayscale => {
                if bit_depth < BitDepth::Eight {
                    bit_depth = BitDepth::Eight;
                }
                if info.trns.is_some() {
                    color_type = ColorType::GrayscaleAlpha;
                }
            }
            ColorType::Rgb => {
                if info.trns.is_some() {
                    color_type = ColorType::Rgba;
                }
            }
            ColorType::GrayscaleAlpha | ColorType::Rgba => {}
        }
    }
    if t.contains(Transformations::STRIP_16) && bit_depth == BitDepth::Sixteen {
        bit_depth = BitDepth::Eight;
    }

    let line_size = row_bytes(info.width, color_type.samples(), bit_depth.bits())
        .ok_or(DecodingError::LimitsExceeded)?;
    if line_size.checked_mul(info.height as usize).is_none() {
        return Err(DecodingError::LimitsExceeded);
    }
    Ok(OutputInfo {
        width: info.width,
        height: info.height,
        color_type,
        bit_depth,
        line_size,
    })
}
