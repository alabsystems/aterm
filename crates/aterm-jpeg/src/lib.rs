// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-jpeg` — first-party JPEG decode: the wallpaper decoder off macOS
//! (where AppKit's colour-managed decoder serves) and the portable floor
//! everywhere.
//!
//! ## Scope, stated as limits
//!
//! What cameras, phones, browsers and image editors write (ITU T.81):
//! baseline and extended sequential Huffman (SOF0, SOF1) and progressive
//! Huffman (SOF2), 8-bit samples, one, three or four components (grey, JFIF
//! YCbCr or RGB, Adobe CMYK and YCCK), any sampling factors from 1 to 4,
//! interleaved and single-component scans, restart intervals, and the EXIF
//! orientation (the image comes back upright, its sides swapped where the
//! orientation turns it).
//!
//! Not here: arithmetic coding (SOF9 and up), lossless and hierarchical
//! modes, 12-bit samples, and colour management (an embedded ICC profile is
//! skipped; the samples are taken as sRGB). A file that uses one of those
//! is refused, never misread.
//!
//! ## Hostile input
//!
//! Dimensions are checked against the caller's bound before anything is
//! allocated, every table and index is checked, and a corrupt or truncated
//! file is `None` or decodes to its end with the missing data as zeros:
//! never a panic, never an allocation the dimensions bound does not cover.

mod color;
mod huffman;
mod idct;

use color::{ColorModel, Plane};
use huffman::{BitReader, HuffmanTable};

/// Where coefficient `k` of a zigzag-ordered block sits in natural order.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The file name extensions of the format this crate reads, lower case.
pub const EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif"];

/// Decode a JPEG to packed straight RGBA8 (alpha 255), upright, returning
/// `(rgba, width, height)`. Either side past `max_dimension` is refused
/// before any pixel buffer is allocated. `None` for anything that is not a
/// JPEG this crate reads (see the crate docs) or is too corrupt to frame.
#[inline]
#[must_use]
pub fn decode_rgba8(bytes: &[u8], max_dimension: u32) -> Option<(Vec<u8>, usize, usize)> {
    let mut decoder = Decoder::new(bytes, max_dimension as usize)?;
    decoder.run()?;
    decoder.finish()
}

/// Whether `bytes` start like a JPEG (the SOI marker and another marker).
#[inline]
#[must_use]
pub const fn sniff(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant: usize,
    /// Blocks across and down in the MCU-padded plane.
    blocks_w: usize,
    blocks_h: usize,
    /// The component's real extent in samples.
    width: usize,
    height: usize,
    dc_table: usize,
    ac_table: usize,
    dc_pred: i32,
    /// Sequential: the samples, written block by block as scans decode.
    samples: Vec<u8>,
    /// Progressive: every block's coefficients, zigzag positions already in
    /// natural order, dequantized only once all scans are in.
    coefficients: Vec<i16>,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    max_h: usize,
    max_v: usize,
    mcus_x: usize,
    mcus_y: usize,
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    max_dimension: usize,
    quant: [Option<[u16; 64]>; 4],
    dc_tables: [Option<HuffmanTable>; 4],
    ac_tables: [Option<HuffmanTable>; 4],
    restart_interval: usize,
    frame: Option<Frame>,
    components: Vec<Component>,
    /// An Adobe APP14 segment's colour transform (0 none, 1 YCbCr, 2 YCCK).
    adobe_transform: Option<u8>,
    jfif: bool,
    orientation: u16,
    eob_run: u32,
    /// At least one scan's data was read.
    scanned: bool,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8], max_dimension: usize) -> Option<Self> {
        if !sniff(data) {
            return None;
        }
        Some(Self {
            data,
            pos: 2,
            max_dimension,
            quant: [None; 4],
            dc_tables: [None, None, None, None],
            ac_tables: [None, None, None, None],
            restart_interval: 0,
            frame: None,
            components: Vec::new(),
            adobe_transform: None,
            jfif: false,
            orientation: 1,
            eob_run: 0,
            scanned: false,
        })
    }

    /// The next marker at or after `pos`, skipping fill bytes and any stray
    /// entropy bytes before it.
    fn next_marker(&mut self) -> Option<u8> {
        loop {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            if byte != 0xFF {
                continue;
            }
            let mut marker = *self.data.get(self.pos)?;
            while marker == 0xFF {
                self.pos += 1;
                marker = *self.data.get(self.pos)?;
            }
            self.pos += 1;
            if marker != 0x00 && !(0xD0..=0xD7).contains(&marker) {
                return Some(marker);
            }
        }
    }

    /// The payload of the length-prefixed segment at `pos`, stepping past it.
    fn segment(&mut self) -> Option<&'a [u8]> {
        let high = *self.data.get(self.pos)?;
        let low = *self.data.get(self.pos + 1)?;
        let length = usize::from(u16::from_be_bytes([high, low]));
        if length < 2 {
            return None;
        }
        let payload = self.data.get(self.pos + 2..self.pos + length)?;
        self.pos += length;
        Some(payload)
    }

    fn run(&mut self) -> Option<()> {
        loop {
            // A file that ends without its EOI after a scan is an image cut
            // short, decoded as far as it goes.
            let Some(marker) = self.next_marker() else {
                return self.scanned.then_some(());
            };
            match marker {
                0xD9 => return self.scanned.then_some(()),
                // TEM: a marker with no segment.
                0x01 => {}
                0xC0 | 0xC1 => self.start_of_frame(false)?,
                0xC2 => self.start_of_frame(true)?,
                // Lossless, hierarchical, arithmetic: not read.
                0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
                0xC4 => self.huffman_tables()?,
                0xDB => self.quant_tables()?,
                0xDD => {
                    let payload = self.segment()?;
                    self.restart_interval =
                        usize::from(u16::from_be_bytes([*payload.first()?, *payload.get(1)?]));
                }
                0xDA => {
                    let done = self.scan()?;
                    if done {
                        return Some(());
                    }
                }
                0xE0 => {
                    let payload = self.segment()?;
                    self.jfif |= payload.starts_with(b"JFIF\0");
                }
                0xE1 => {
                    let payload = self.segment()?;
                    if let Some(orientation) =
                        payload.strip_prefix(b"Exif\0\0").and_then(exif_orientation)
                    {
                        self.orientation = orientation;
                    }
                }
                0xEE => {
                    let payload = self.segment()?;
                    if payload.starts_with(b"Adobe") {
                        self.adobe_transform = payload.get(11).copied();
                    }
                }
                // DNL: the height after the first scan is not supported.
                0xDC => return None,
                _ => {
                    self.segment()?;
                }
            }
        }
    }

    fn quant_tables(&mut self) -> Option<()> {
        let mut payload = self.segment()?;
        while let Some((&info, rest)) = payload.split_first() {
            let precision = info >> 4;
            let id = usize::from(info & 15);
            if id > 3 {
                return None;
            }
            let mut table = [0u16; 64];
            let size = if precision == 0 { 64 } else { 128 };
            let values = rest.get(..size)?;
            for (k, slot) in ZIGZAG.iter().enumerate() {
                table[*slot] = if precision == 0 {
                    u16::from(values[k])
                } else {
                    u16::from_be_bytes([values[2 * k], values[2 * k + 1]])
                };
            }
            self.quant[id] = Some(table);
            payload = &rest[size..];
        }
        Some(())
    }

    fn huffman_tables(&mut self) -> Option<()> {
        let mut payload = self.segment()?;
        while let Some((&info, rest)) = payload.split_first() {
            let class = info >> 4;
            let id = usize::from(info & 15);
            if id > 3 || class > 1 {
                return None;
            }
            let counts: [u8; 16] = rest.get(..16)?.try_into().ok()?;
            let total: usize = counts.iter().map(|&count| usize::from(count)).sum();
            let symbols = rest.get(16..16 + total)?;
            let table = HuffmanTable::new(&counts, symbols)?;
            if class == 0 {
                self.dc_tables[id] = Some(table);
            } else {
                self.ac_tables[id] = Some(table);
            }
            payload = &rest[16 + total..];
        }
        Some(())
    }

    fn start_of_frame(&mut self, progressive: bool) -> Option<()> {
        if self.frame.is_some() {
            return None;
        }
        let payload = self.segment()?;
        let precision = *payload.first()?;
        let height = usize::from(u16::from_be_bytes([*payload.get(1)?, *payload.get(2)?]));
        let width = usize::from(u16::from_be_bytes([*payload.get(3)?, *payload.get(4)?]));
        let count = usize::from(*payload.get(5)?);
        if precision != 8
            || width == 0
            || height == 0
            || width > self.max_dimension
            || height > self.max_dimension
            || !matches!(count, 1 | 3 | 4)
        {
            return None;
        }
        let mut specs = Vec::with_capacity(count);
        for c in 0..count {
            let spec = payload.get(6 + 3 * c..9 + 3 * c)?;
            let (h, v) = (usize::from(spec[1] >> 4), usize::from(spec[1] & 15));
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || spec[2] > 3 {
                return None;
            }
            specs.push((spec[0], h, v, usize::from(spec[2])));
        }
        let max_h = specs.iter().map(|spec| spec.1).max()?;
        let max_v = specs.iter().map(|spec| spec.2).max()?;
        let mcus_x = width.div_ceil(8 * max_h);
        let mcus_y = height.div_ceil(8 * max_v);
        for (id, h, v, quant) in specs {
            let blocks_w = mcus_x * h;
            let blocks_h = mcus_y * v;
            let blocks = blocks_w.checked_mul(blocks_h)?;
            let samples = blocks.checked_mul(64)?;
            self.components.push(Component {
                id,
                h,
                v,
                quant,
                blocks_w,
                blocks_h,
                width: (width * h).div_ceil(max_h),
                height: (height * v).div_ceil(max_v),
                dc_table: 0,
                ac_table: 0,
                dc_pred: 0,
                samples: if progressive {
                    Vec::new()
                } else {
                    vec![0; samples]
                },
                coefficients: if progressive {
                    vec![0; samples]
                } else {
                    Vec::new()
                },
            });
        }
        self.frame = Some(Frame {
            width,
            height,
            progressive,
            max_h,
            max_v,
            mcus_x,
            mcus_y,
        });
        Some(())
    }

    /// One scan: its header, then its entropy-coded data. `Some(true)` where
    /// the data ran to the end of the file (a truncated image).
    fn scan(&mut self) -> Option<bool> {
        let payload = self.segment()?;
        let frame = self.frame.as_ref()?;
        let progressive = frame.progressive;
        let count = usize::from(*payload.first()?);
        if count == 0 || count > 4 {
            return None;
        }
        let mut members = Vec::with_capacity(count);
        for c in 0..count {
            let spec = payload.get(1 + 2 * c..3 + 2 * c)?;
            let index = self
                .components
                .iter()
                .position(|component| component.id == spec[0])?;
            let component = &mut self.components[index];
            component.dc_table = usize::from(spec[1] >> 4);
            component.ac_table = usize::from(spec[1] & 15);
            if component.dc_table > 3 || component.ac_table > 3 {
                return None;
            }
            members.push(index);
        }
        let tail = payload.get(1 + 2 * count..4 + 2 * count)?;
        let (start, end) = (usize::from(tail[0]), usize::from(tail[1]));
        let (high, low) = (u32::from(tail[2] >> 4), u32::from(tail[2] & 15));
        if progressive {
            // T.81 G.1.1.1.1: DC alone or one component's AC band.
            if end > 63
                || start > end
                || (start == 0 && end != 0)
                || (start > 0 && count != 1)
                || low > 13
            {
                return None;
            }
        } else if start != 0 || end != 63 || high != 0 || low != 0 {
            return None;
        }
        let scan = Scan {
            members,
            start,
            end,
            high,
            low,
        };
        for &index in &scan.members {
            self.components[index].dc_pred = 0;
        }
        self.eob_run = 0;
        let mut reader = BitReader::new(self.data, self.pos);
        self.decode_scan(&scan, &mut reader)?;
        self.scanned = true;
        self.pos = reader.position();
        Some(self.pos >= self.data.len())
    }

    fn decode_scan(&mut self, scan: &Scan, reader: &mut BitReader<'_>) -> Option<()> {
        let frame = self.frame.as_ref()?;
        let (mcus_x, mcus_y) = (frame.mcus_x, frame.mcus_y);
        let progressive = frame.progressive;
        // A single-component scan walks that component's own blocks (A.2.2),
        // not the MCU-padded grid.
        let single = scan.members.len() == 1;
        let units = if single {
            let component = &self.components[scan.members[0]];
            (component.width.div_ceil(8), component.height.div_ceil(8))
        } else {
            (mcus_x, mcus_y)
        };
        let mut done = 0usize;
        for unit_y in 0..units.1 {
            for unit_x in 0..units.0 {
                if self.restart_interval > 0
                    && done > 0
                    && done.is_multiple_of(self.restart_interval)
                {
                    // Missing marker: carry on from wherever the data is.
                    reader.restart();
                    for &index in &scan.members {
                        self.components[index].dc_pred = 0;
                    }
                    self.eob_run = 0;
                }
                done += 1;
                for &index in &scan.members {
                    let (blocks_h, blocks_v) = if single {
                        (1, 1)
                    } else {
                        (self.components[index].h, self.components[index].v)
                    };
                    for by in 0..blocks_v {
                        for bx in 0..blocks_h {
                            let (block_x, block_y) = if single {
                                (unit_x, unit_y)
                            } else {
                                (unit_x * blocks_h + bx, unit_y * blocks_v + by)
                            };
                            if progressive {
                                self.progressive_block(index, block_x, block_y, scan, reader)?;
                            } else {
                                self.sequential_block(index, block_x, block_y, reader)?;
                            }
                        }
                    }
                }
            }
        }
        Some(())
    }

    fn sequential_block(
        &mut self,
        index: usize,
        block_x: usize,
        block_y: usize,
        reader: &mut BitReader<'_>,
    ) -> Option<()> {
        let component = &self.components[index];
        let dc = self.dc_tables[component.dc_table].as_ref()?;
        let ac = self.ac_tables[component.ac_table].as_ref()?;
        let quant = self.quant[component.quant].as_ref()?;
        let mut block = [0i32; 64];
        let category = u32::from(reader.decode(dc)?);
        let diff = reader.receive_extend(category);
        let prediction = component.dc_pred.wrapping_add(diff);
        block[0] = dequantize(prediction, quant[0]);
        let mut k = 1;
        while k < 64 {
            let rs = reader.decode(ac)?;
            let (run, size) = (usize::from(rs >> 4), u32::from(rs & 15));
            if size == 0 {
                if rs != 0xF0 {
                    break;
                }
                k += 16;
                continue;
            }
            k += run;
            if k > 63 {
                break;
            }
            let natural = ZIGZAG[k];
            block[natural] = dequantize(reader.receive_extend(size), quant[natural]);
            k += 1;
        }
        let component = &mut self.components[index];
        component.dc_pred = prediction;
        let stride = component.blocks_w * 8;
        let at = block_y * 8 * stride + block_x * 8;
        if block_x < component.blocks_w && block_y < component.blocks_h {
            idct::idct_block(&block, component.samples.get_mut(at..)?, stride);
        }
        Some(())
    }

    fn progressive_block(
        &mut self,
        index: usize,
        block_x: usize,
        block_y: usize,
        scan: &Scan,
        reader: &mut BitReader<'_>,
    ) -> Option<()> {
        let component = &self.components[index];
        if block_x >= component.blocks_w || block_y >= component.blocks_h {
            return Some(());
        }
        let at = (block_y * component.blocks_w + block_x) * 64;
        if scan.start == 0 {
            // DC first or refining.
            if scan.high == 0 {
                let dc = self.dc_tables[component.dc_table].as_ref()?;
                let category = u32::from(reader.decode(dc)?);
                let diff = reader.receive_extend(category);
                let component = &mut self.components[index];
                component.dc_pred = component.dc_pred.wrapping_add(diff);
                let value = component.dc_pred.wrapping_shl(scan.low);
                *component.coefficients.get_mut(at)? = value as i16;
            } else if reader.bit() {
                let slot = self.components[index].coefficients.get_mut(at)?;
                *slot |= 1 << scan.low;
            }
            return Some(());
        }
        let ac = self.ac_tables[component.ac_table].as_ref()?;
        let coefficients = self.components[index].coefficients.get_mut(at..at + 64)?;
        if scan.high == 0 {
            ac_first(coefficients, ac, scan, reader, &mut self.eob_run)
        } else {
            ac_refine(coefficients, ac, scan, reader, &mut self.eob_run)
        }
    }

    fn finish(mut self) -> Option<(Vec<u8>, usize, usize)> {
        let frame = self.frame.take()?;
        if frame.progressive {
            // Dequantize and transform every block now that all scans are in.
            for component in &mut self.components {
                let quant = self.quant[component.quant].as_ref()?;
                let stride = component.blocks_w * 8;
                let mut samples = vec![0u8; component.coefficients.len()];
                for block_y in 0..component.blocks_h {
                    for block_x in 0..component.blocks_w {
                        let at = (block_y * component.blocks_w + block_x) * 64;
                        let mut block = [0i32; 64];
                        for (i, slot) in block.iter_mut().enumerate() {
                            *slot = dequantize(i32::from(component.coefficients[at + i]), quant[i]);
                        }
                        let out = block_y * 8 * stride + block_x * 8;
                        idct::idct_block(&block, samples.get_mut(out..)?, stride);
                    }
                }
                component.samples = samples;
                component.coefficients = Vec::new();
            }
        }
        let model = self.color_model();
        let max = (frame.max_h, frame.max_v);
        let planes: Vec<Plane> = self
            .components
            .drain(..)
            .map(|component| Plane {
                stride: component.blocks_w * 8,
                width: component.width,
                height: component.height,
                factor: (frame.max_h % component.h == 0 && frame.max_v % component.v == 0)
                    .then(|| (frame.max_h / component.h, frame.max_v / component.v)),
                sampling: (component.h, component.v),
                samples: component.samples,
            })
            .collect();
        let (width, height) = (frame.width, frame.height);
        let mut rgba = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
        let mut rows: Vec<Vec<u8>> = planes.iter().map(|_| vec![0u8; width]).collect();
        for y in 0..height {
            for (plane, row) in planes.iter().zip(rows.iter_mut()) {
                plane.upsampled_row(y, width, max, row);
            }
            color::convert_row(
                model,
                &rows,
                width,
                &mut rgba[y * width * 4..(y + 1) * width * 4],
            );
        }
        Some(color::orient(rgba, width, height, self.orientation))
    }

    #[inline]
    fn color_model(&self) -> ColorModel {
        match self.components.len() {
            1 => ColorModel::Gray,
            3 => match self.adobe_transform {
                Some(0) => ColorModel::Rgb,
                Some(_) => ColorModel::YCbCr,
                None if !self.jfif
                    && [
                        self.components[0].id,
                        self.components[1].id,
                        self.components[2].id,
                    ] == *b"RGB" =>
                {
                    ColorModel::Rgb
                }
                None => ColorModel::YCbCr,
            },
            _ => match self.adobe_transform {
                Some(2) => ColorModel::Ycck,
                _ => ColorModel::Cmyk,
            },
        }
    }
}

struct Scan {
    members: Vec<usize>,
    start: usize,
    end: usize,
    high: u32,
    low: u32,
}

/// A coefficient times its quantizer, held to the range an 8-bit DCT
/// coefficient can take so hostile values cannot overflow the transform.
#[inline]
fn dequantize(value: i32, quant: u16) -> i32 {
    value
        .saturating_mul(i32::from(quant))
        .clamp(-32_768, 32_767)
}

/// A progressive scan's first pass over an AC band (T.81 G.1.2.2).
fn ac_first(
    coefficients: &mut [i16],
    table: &HuffmanTable,
    scan: &Scan,
    reader: &mut BitReader<'_>,
    eob_run: &mut u32,
) -> Option<()> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Some(());
    }
    let mut k = scan.start;
    while k <= scan.end {
        let rs = reader.decode(table)?;
        let (run, size) = (u32::from(rs >> 4), u32::from(rs & 15));
        if size == 0 {
            if run < 15 {
                *eob_run = (1 << run) - 1 + reader.bits(run);
                break;
            }
            k += 16;
            continue;
        }
        k += run as usize;
        if k > 63 {
            break;
        }
        let value = reader.receive_extend(size).wrapping_shl(scan.low);
        coefficients[ZIGZAG[k]] = value as i16;
        k += 1;
    }
    Some(())
}

/// A progressive scan's refinement of an AC band (T.81 G.1.2.3): one more
/// bit for every coefficient already non-zero, and newly non-zero ones of
/// magnitude one at this bit.
fn ac_refine(
    coefficients: &mut [i16],
    table: &HuffmanTable,
    scan: &Scan,
    reader: &mut BitReader<'_>,
    eob_run: &mut u32,
) -> Option<()> {
    let bit: i16 = 1 << scan.low;
    let refine = |slot: &mut i16, reader: &mut BitReader<'_>| {
        if reader.bit() && (*slot & bit) == 0 {
            *slot = if *slot >= 0 {
                slot.wrapping_add(bit)
            } else {
                slot.wrapping_sub(bit)
            };
        }
    };
    if *eob_run > 0 {
        *eob_run -= 1;
        for k in scan.start..=scan.end {
            let slot = &mut coefficients[ZIGZAG[k]];
            if *slot != 0 {
                refine(slot, reader);
            }
        }
        return Some(());
    }
    let mut k = scan.start;
    while k <= scan.end {
        let rs = reader.decode(table)?;
        let (mut run, size) = (u32::from(rs >> 4), u32::from(rs & 15));
        let mut value: i16 = 0;
        if size == 0 {
            if run < 15 {
                *eob_run = (1 << run) - 1 + reader.bits(run);
                // Refine the rest of the band, then stop.
                run = 64;
            }
        } else {
            if size != 1 {
                return None;
            }
            value = if reader.bit() { bit } else { -bit };
        }
        while k <= scan.end {
            let slot = &mut coefficients[ZIGZAG[k]];
            k += 1;
            if *slot != 0 {
                refine(slot, reader);
            } else {
                if run == 0 {
                    *slot = value;
                    break;
                }
                run -= 1;
            }
        }
    }
    Some(())
}

/// The orientation (TIFF tag 274) in an EXIF payload: a TIFF header and its
/// first IFD.
fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes = [
            *tiff.get(at)?,
            *tiff.get(at + 1)?,
            *tiff.get(at + 2)?,
            *tiff.get(at + 3)?,
        ];
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let entries = usize::from(u16_at(ifd)?);
    (0..entries.min(256)).find_map(|entry| {
        let at = ifd + 2 + entry * 12;
        (u16_at(at)? == 274 && u16_at(at + 2)? == 3)
            .then(|| u16_at(at + 8))
            .flatten()
            .filter(|orientation| (1..=8).contains(orientation))
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_rgba8, exif_orientation};

    /// Not a JPEG, or a JPEG cut off before its frame, is `None`.
    #[test]
    fn what_is_not_a_framed_jpeg_is_refused() {
        assert!(decode_rgba8(b"\x89PNG\r\n\x1a\n", 64).is_none());
        assert!(decode_rgba8(&[0xFF, 0xD8, 0xFF, 0xD9], 64).is_none());
        assert!(decode_rgba8(&[], 64).is_none());
    }

    /// The orientation tag is read from either byte order.
    #[test]
    fn the_exif_orientation_is_read_in_both_byte_orders() {
        let little = [
            b'I', b'I', 42, 0, 8, 0, 0, 0, // header, IFD at 8
            1, 0, // one entry
            0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0,
        ];
        assert_eq!(exif_orientation(&little), Some(6));
        let big = [
            b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 8, 0, 0,
        ];
        assert_eq!(exif_orientation(&big), Some(8));
        assert_eq!(exif_orientation(b"XX"), None);
    }
}
