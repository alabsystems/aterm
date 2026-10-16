// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from read-fonts 0.43.2 src/tables/gvar.rs, src/tables/variations.rs
// and generated/generated_gvar.rs, generated/generated_variations.rs, and
// modified by the aterm project in 2026: the `gvar` glyph variation data as
// FreeType's 16.16 loader reads it (tuple headers, shared and private packed
// point numbers, packed deltas, the fixed-point tuple scalar), specialized to
// point deltas in 16.16, with every read checked.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! `gvar` tuple variation data for the autohinter's outline loader.
//!
//! aterm-font's own `gvar` reader (`tables/gvar.rs`, ttf-parser's) applies
//! deltas in `f32` as it outlines; the autohinter needs FreeType's answer
//! instead, every delta in 16.16 and rounded to a font unit, so this is the
//! read-fonts reader skrifa's FreeType-style scaler sits on.

use super::fixed::{F2Dot14, Fixed, Point};
use super::glyf::{PointFlags, PointMarker};

/// A read past the end of the data, or data that cannot be what it claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadError;

fn read_u8(data: &[u8], offset: usize) -> Result<u8, ReadError> {
    data.get(offset).copied().ok_or(ReadError)
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, ReadError> {
    let end = offset.checked_add(2).ok_or(ReadError)?;
    let b = data.get(offset..end).ok_or(ReadError)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, ReadError> {
    let end = offset.checked_add(4).ok_or(ReadError)?;
    let b = data.get(offset..end).ok_or(ReadError)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A forward reader over a byte slice (read-fonts' `Cursor`).
#[derive(Clone, Copy, Debug)]
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn advance_by(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    fn read_u8(&mut self) -> Result<u8, ReadError> {
        let v = read_u8(self.data, self.pos)?;
        self.pos += 1;
        Ok(v)
    }

    fn read_i8(&mut self) -> Result<i8, ReadError> {
        self.read_u8().map(|v| v as i8)
    }

    fn read_u16(&mut self) -> Result<u16, ReadError> {
        let v = read_u16(self.data, self.pos)?;
        self.pos += 2;
        Ok(v)
    }

    fn read_i16(&mut self) -> Result<i16, ReadError> {
        self.read_u16().map(|v| v as i16)
    }

    fn read_i32(&mut self) -> Result<i32, ReadError> {
        let v = read_u32(self.data, self.pos)?;
        self.pos += 4;
        Ok(v as i32)
    }

    /// The next `len` bytes, all or nothing.
    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ReadError> {
        let end = self.pos.checked_add(len).ok_or(ReadError)?;
        let bytes = self.data.get(self.pos..end).ok_or(ReadError)?;
        self.pos = end;
        Ok(bytes)
    }
}

/// A tuple: one 2.14 coordinate per axis.
#[derive(Clone, Copy, Default, Debug)]
struct Tuple<'a> {
    values: &'a [u8],
}

impl Tuple<'_> {
    fn len(&self) -> usize {
        self.values.len() / 2
    }

    fn get(&self, idx: usize) -> Option<F2Dot14> {
        read_u16(self.values, idx.checked_mul(2)?)
            .ok()
            .map(|v| F2Dot14::from_bits(v as i16))
    }
}

/// The shared tuple records: `count` tuples of `axis_count` coordinates.
#[derive(Clone, Copy, Default, Debug)]
struct SharedTuples<'a> {
    data: &'a [u8],
    axis_count: u16,
}

impl<'a> SharedTuples<'a> {
    fn get(&self, index: usize) -> Option<Tuple<'a>> {
        let len = usize::from(self.axis_count) * 2;
        let start = index.checked_mul(len)?;
        let values = self.data.get(start..start.checked_add(len)?)?;
        Some(Tuple { values })
    }
}

/// The `gvar` table.
#[derive(Clone, Copy)]
pub(crate) struct Gvar<'a> {
    data: &'a [u8],
}

impl<'a> Gvar<'a> {
    /// The table, if it has its 20-byte header.
    pub(crate) fn new(data: &'a [u8]) -> Option<Self> {
        (data.len() >= 20).then_some(Self { data })
    }

    fn axis_count(&self) -> u16 {
        read_u16(self.data, 4).unwrap_or_default()
    }

    fn shared_tuples(&self) -> Result<SharedTuples<'a>, ReadError> {
        let count = read_u16(self.data, 6)?;
        let offset = read_u32(self.data, 8)? as usize;
        let axis_count = self.axis_count();
        // A non-nullable offset: 0 does not resolve.
        if offset == 0 {
            return Err(ReadError);
        }
        let data = self.data.get(offset..).ok_or(ReadError)?;
        // The tuple array, or none when it runs past the table.
        let len = usize::from(count).saturating_mul(usize::from(axis_count) * 2);
        let data = data.get(..len).unwrap_or_default();
        Ok(SharedTuples { data, axis_count })
    }

    fn data_range_for_gid(&self, gid: u16) -> Result<core::ops::Range<usize>, ReadError> {
        let glyph_count = read_u16(self.data, 12)?;
        let long_offsets = read_u16(self.data, 14)? & 1 != 0;
        let data_start = read_u32(self.data, 16)?;
        let offsets_len = (usize::from(glyph_count) + 1) * if long_offsets { 4 } else { 2 };
        // The offsets array, or none when it runs past the table.
        let offsets = self
            .data
            .get(20..)
            .and_then(|d| d.get(..offsets_len))
            .unwrap_or_default();
        let offset_at = |i: usize| -> Result<u32, ReadError> {
            if long_offsets {
                read_u32(offsets, i * 4)
            } else {
                read_u16(offsets, i * 2).map(|v| u32::from(v) * 2)
            }
        };
        let start_idx = usize::from(gid);
        let start = data_start.checked_add(offset_at(start_idx)?);
        let end = data_start.checked_add(offset_at(start_idx + 1)?);
        let (Some(start), Some(end)) = (start, end) else {
            return Err(ReadError);
        };
        Ok(start as usize..end as usize)
    }

    /// The variation data for a glyph: `Ok(None)` when it has none, an error
    /// when it has some that is malformed.
    pub(crate) fn glyph_variation_data(
        &self,
        gid: u16,
    ) -> Result<Option<GlyphVariationData<'a>>, ReadError> {
        let shared_tuples = self.shared_tuples()?;
        let axis_count = self.axis_count();
        let range = self.data_range_for_gid(gid)?;
        if range.is_empty() {
            return Ok(None);
        }
        let data = self.data.get(range).ok_or(ReadError)?;
        GlyphVariationData::new(data, axis_count, shared_tuples).map(Some)
    }

    /// The deltas of the four phantom points (`[left, right, top, bottom]`)
    /// at `coords`, if the glyph that carries the metrics has variation data.
    pub(crate) fn phantom_point_deltas(
        &self,
        glyf: &super::glyf::Glyf<'a>,
        coords: &[F2Dot14],
        glyph_id: u16,
    ) -> Result<Option<[Point<Fixed>; 4]>, ReadError> {
        let (glyph_id, point_count) = find_glyph_and_point_count(glyf, glyph_id, 0)?;
        let mut phantom_deltas = [Point::default(); 4];
        let phantom_range = point_count..point_count + 4;
        let Some(var_data) = self.glyph_variation_data(glyph_id)? else {
            return Ok(None);
        };
        // Phantom points never belong to a contour, so no IUP here.
        for (tuple, scalar) in var_data.active_tuples_at(coords) {
            for tuple_delta in tuple.deltas() {
                let ix = tuple_delta.position as usize;
                if phantom_range.contains(&ix) {
                    phantom_deltas[ix - phantom_range.start] += tuple_delta.apply_scalar(scalar);
                }
            }
        }
        Ok(Some(phantom_deltas))
    }
}

/// The glyph whose phantom points carry the metrics, and its point count
/// (for a composite, its component count): the glyph itself, or the last
/// component marked USE_MY_METRICS, recursively.
fn find_glyph_and_point_count(
    glyf: &super::glyf::Glyf,
    glyph_id: u16,
    recurse_depth: usize,
) -> Result<(u16, usize), ReadError> {
    // Matches HarfBuzz's nesting limit.
    const RECURSION_LIMIT: usize = 64;
    if recurse_depth > RECURSION_LIMIT {
        return Err(ReadError);
    }
    let Some(glyph) = glyf.get(glyph_id)? else {
        // Empty glyphs might still contain gvar data that only affects
        // phantom points.
        return Ok((glyph_id, 0));
    };
    match glyph {
        super::glyf::Glyph::Simple(simple) => Ok((glyph_id, simple.num_points())),
        super::glyf::Glyph::Composite(composite) => {
            let (count, inherit_metrics) = composite.component_glyphs_and_flags().fold(
                (0, None),
                |(count, inherit_metrics), (component_id, flags)| {
                    let has_flag = flags & super::glyf::USE_MY_METRICS != 0;
                    let preferred = has_flag.then_some(component_id).or(inherit_metrics);
                    (count + 1, preferred)
                },
            );
            if let Some(component) = inherit_metrics {
                find_glyph_and_point_count(glyf, component, recurse_depth + 1)
            } else {
                Ok((glyph_id, count))
            }
        }
    }
}

/// One glyph's tuple variation store.
#[derive(Clone, Copy)]
pub(crate) struct GlyphVariationData<'a> {
    axis_count: u16,
    shared_tuples: SharedTuples<'a>,
    shared_point_numbers: Option<PackedPointNumbers<'a>>,
    tuple_count: u16,
    header_data: &'a [u8],
    serialized_data: &'a [u8],
}

impl<'a> GlyphVariationData<'a> {
    fn new(
        data: &'a [u8],
        axis_count: u16,
        shared_tuples: SharedTuples<'a>,
    ) -> Result<Self, ReadError> {
        if data.len() < 4 {
            return Err(ReadError);
        }
        let count = read_u16(data, 0)?;
        let serialized_offset = usize::from(read_u16(data, 2)?);
        let header_data = data.get(4..).unwrap_or_default();
        // A non-nullable offset: 0 does not resolve.
        if serialized_offset == 0 {
            return Err(ReadError);
        }
        let serialized = data.get(serialized_offset..).ok_or(ReadError)?;
        // Shared point numbers, if any, open the serialized data.
        let (shared_point_numbers, serialized_data) = if count & 0x8000 != 0 {
            let (packed, data) = PackedPointNumbers::split_off_front(serialized);
            (Some(packed), data)
        } else {
            (None, serialized)
        };
        Ok(Self {
            axis_count,
            shared_tuples,
            shared_point_numbers,
            tuple_count: count & 0x0FFF,
            header_data,
            serialized_data,
        })
    }

    /// The (tuple, scalar) pairs active at `coords`.
    pub(crate) fn active_tuples_at(
        &self,
        coords: &'a [F2Dot14],
    ) -> impl Iterator<Item = (TupleVariation<'a>, Fixed)> + 'a {
        ActiveTupleVariationIter {
            coords,
            parent: *self,
            header_iter: TupleVariationHeaderIter {
                data: self.header_data,
                n_headers: usize::from(self.tuple_count),
                current: 0,
                axis_count: self.axis_count,
            },
            serialized_data: self.serialized_data,
            data_offset: 0,
        }
    }
}

/// A tuple variation header: data size, tuple index, and the embedded peak
/// and intermediate tuples its flags announce.
#[derive(Clone, Copy)]
struct TupleVariationHeader<'a> {
    data: &'a [u8],
    axis_count: u16,
}

const EMBEDDED_PEAK_TUPLE: u16 = 0x8000;
const INTERMEDIATE_REGION: u16 = 0x4000;
const PRIVATE_POINT_NUMBERS: u16 = 0x2000;
const TUPLE_INDEX_MASK: u16 = 0x0FFF;

impl<'a> TupleVariationHeader<'a> {
    fn variation_data_size(&self) -> u16 {
        read_u16(self.data, 0).unwrap_or_default()
    }

    fn tuple_index(&self) -> u16 {
        read_u16(self.data, 2).unwrap_or_default()
    }

    fn tuple_len(&self) -> usize {
        usize::from(self.axis_count) * 2
    }

    fn peak_tuple(&self) -> Option<Tuple<'a>> {
        (self.tuple_index() & EMBEDDED_PEAK_TUPLE != 0).then_some(())?;
        let values = self.data.get(4..4 + self.tuple_len())?;
        Some(Tuple { values })
    }

    fn intermediate_tuples(&self) -> Option<(Tuple<'a>, Tuple<'a>)> {
        (self.tuple_index() & INTERMEDIATE_REGION != 0).then_some(())?;
        let peak_len = if self.tuple_index() & EMBEDDED_PEAK_TUPLE != 0 {
            self.tuple_len()
        } else {
            0
        };
        let start = 4 + peak_len;
        let mid = start + self.tuple_len();
        let end = mid + self.tuple_len();
        Some((
            Tuple {
                values: self.data.get(start..mid)?,
            },
            Tuple {
                values: self.data.get(mid..end)?,
            },
        ))
    }

    fn byte_len(&self) -> usize {
        let index = self.tuple_index();
        4 + if index & EMBEDDED_PEAK_TUPLE != 0 {
            self.tuple_len()
        } else {
            0
        } + if index & INTERMEDIATE_REGION != 0 {
            self.tuple_len() * 2
        } else {
            0
        }
    }
}

struct TupleVariationHeaderIter<'a> {
    data: &'a [u8],
    n_headers: usize,
    current: usize,
    axis_count: u16,
}

impl<'a> Iterator for TupleVariationHeaderIter<'a> {
    type Item = Result<TupleVariationHeader<'a>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.n_headers {
            return None;
        }
        self.current += 1;
        let next = if self.data.len() < 4 {
            Err(ReadError)
        } else {
            Ok(TupleVariationHeader {
                data: self.data,
                axis_count: self.axis_count,
            })
        };
        let next_len = next.as_ref().map(|h| h.byte_len()).unwrap_or(0);
        // A header that runs past the data ends the iteration unread.
        self.data = self.data.get(next_len..)?;
        Some(next)
    }
}

struct ActiveTupleVariationIter<'a> {
    coords: &'a [F2Dot14],
    parent: GlyphVariationData<'a>,
    header_iter: TupleVariationHeaderIter<'a>,
    serialized_data: &'a [u8],
    data_offset: usize,
}

impl<'a> Iterator for ActiveTupleVariationIter<'a> {
    type Item = (TupleVariation<'a>, Fixed);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let header = self.header_iter.next()?.ok()?;
            let data_len = usize::from(header.variation_data_size());
            let data_start = self.data_offset;
            let data_end = data_start.checked_add(data_len)?;
            self.data_offset = data_end;
            if let Some(scalar) = compute_scalar(
                &header,
                usize::from(self.parent.axis_count),
                &self.parent.shared_tuples,
                self.coords,
            ) {
                let var_data = self.serialized_data.get(data_start..data_end)?;
                return Some((
                    TupleVariation {
                        header,
                        serialized_data: var_data,
                        shared_point_numbers: self.parent.shared_point_numbers,
                    },
                    scalar,
                ));
            }
        }
    }
}

/// The fixed-point scalar of a tuple at `coords`; `None` when the tuple does
/// not apply there (a zero scalar included). Missing trailing coordinates
/// are zero.
fn compute_scalar(
    header: &TupleVariationHeader,
    axis_count: usize,
    shared_tuples: &SharedTuples,
    coords: &[F2Dot14],
) -> Option<Fixed> {
    let mut scalar = Fixed::ONE;
    let tuple_idx = header.tuple_index();
    let peak = if tuple_idx & EMBEDDED_PEAK_TUPLE == 0 {
        shared_tuples.get(usize::from(tuple_idx & TUPLE_INDEX_MASK))?
    } else {
        header.peak_tuple()?
    };
    if peak.len() != axis_count {
        return None;
    }
    let intermediate = header.intermediate_tuples();
    for i in 0..peak.len() {
        let peak = peak.get(i)?;
        if peak == F2Dot14::ZERO {
            continue;
        }
        let coord = coords.get(i).copied().unwrap_or_default();
        if coord == F2Dot14::ZERO {
            return None;
        }
        if peak == coord {
            continue;
        }
        if let Some((inter_start, inter_end)) = &intermediate {
            let start = inter_start.get(i).unwrap_or_default();
            let end = inter_end.get(i).unwrap_or_default();
            if coord <= start || coord >= end {
                return None;
            }
            let coord = coord.to_fixed();
            let peak = peak.to_fixed();
            if coord < peak {
                let start = start.to_fixed();
                scalar = scalar.mul_div(coord - start, peak - start);
            } else {
                let end = end.to_fixed();
                scalar = scalar.mul_div(end - coord, end - peak);
            }
        } else {
            if coord < peak.min(F2Dot14::ZERO) || coord > peak.max(F2Dot14::ZERO) {
                return None;
            }
            let coord = coord.to_fixed();
            let peak = peak.to_fixed();
            scalar = scalar.mul_div(coord, peak);
        }
    }
    (scalar != Fixed::ZERO).then_some(scalar)
}

impl Fixed {
    /// Multiplies `self` by `a` and divides the product by `b`, rounding
    /// (FreeType's `FT_MulDiv`).
    pub(crate) const fn mul_div(&self, a: Self, b: Self) -> Self {
        let mut sign = 1;
        let mut su = self.to_bits() as u64;
        let mut au = a.to_bits() as u64;
        let mut bu = b.to_bits() as u64;
        if self.to_bits() < 0 {
            su = 0u64.wrapping_sub(su);
            sign = -1;
        }
        if a.to_bits() < 0 {
            au = 0u64.wrapping_sub(au);
            sign = -sign;
        }
        if b.to_bits() < 0 {
            bu = 0u64.wrapping_sub(bu);
            sign = -sign;
        }
        let result = match su.wrapping_mul(au).wrapping_add(bu >> 1).checked_div(bu) {
            Some(q) => q,
            None => 0x7FFFFFFF,
        };
        Self::from_bits(if sign < 0 {
            (result as i32).wrapping_neg()
        } else {
            result as i32
        })
    }
}

/// One tuple's variation data.
#[derive(Clone, Copy)]
pub(crate) struct TupleVariation<'a> {
    header: TupleVariationHeader<'a>,
    serialized_data: &'a [u8],
    shared_point_numbers: Option<PackedPointNumbers<'a>>,
}

/// A point's (or a component's) delta in one tuple, before scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlyphDelta {
    /// The point or component index.
    pub position: u16,
    /// The x delta.
    pub x_delta: i32,
    /// The y delta.
    pub y_delta: i32,
}

impl GlyphDelta {
    /// The delta scaled by a tuple scalar, in 16.16.
    pub(crate) fn apply_scalar(self, scalar: Fixed) -> Point<Fixed> {
        Point::new(self.x_delta, self.y_delta).map(Fixed::from_i32) * scalar
    }
}

impl<'a> TupleVariation<'a> {
    /// True if this tuple has a delta for every point of the glyph.
    pub(crate) fn has_deltas_for_all_points(&self) -> bool {
        if self.header.tuple_index() & PRIVATE_POINT_NUMBERS != 0 {
            PackedPointNumbers {
                data: self.serialized_data,
            }
            .count()
                == 0
        } else if let Some(shared) = &self.shared_point_numbers {
            shared.count() == 0
        } else {
            false
        }
    }

    /// The explicitly encoded deltas, unscaled (an omission by IUP is not
    /// among them).
    pub(crate) fn deltas(&self) -> TupleDeltaIter<'a> {
        let (point_numbers, packed_deltas) = self.point_numbers_and_packed_deltas();
        let count = usize::from(point_numbers.count());
        let packed_deltas = if count == 0 {
            PackedDeltas {
                data: packed_deltas,
                count: None,
            }
        } else {
            PackedDeltas {
                data: packed_deltas,
                count: Some(count * 2),
            }
        };
        TupleDeltaIter::new(&point_numbers, packed_deltas)
    }

    fn point_numbers_and_packed_deltas(&self) -> (PackedPointNumbers<'a>, &'a [u8]) {
        if self.header.tuple_index() & PRIVATE_POINT_NUMBERS != 0 {
            PackedPointNumbers::split_off_front(self.serialized_data)
        } else {
            (
                self.shared_point_numbers.unwrap_or_default(),
                self.serialized_data,
            )
        }
    }

    /// Adds this tuple's deltas, times `scalar`, to every point's: the tuple
    /// must be dense ([`Self::has_deltas_for_all_points`]).
    pub(crate) fn accumulate_dense_deltas(
        &self,
        deltas: &mut [Point<Fixed>],
        scalar: Fixed,
    ) -> Result<(), ReadError> {
        let (_, packed_deltas) = self.point_numbers_and_packed_deltas();
        let mut cursor = Cursor::new(packed_deltas);
        if scalar == Fixed::ONE {
            read_dense_deltas(&mut cursor, deltas, |delta, new_delta| {
                delta.x += Fixed::from_i32(new_delta);
            })?;
            read_dense_deltas(&mut cursor, deltas, |delta, new_delta| {
                delta.y += Fixed::from_i32(new_delta);
            })?;
        } else {
            read_dense_deltas(&mut cursor, deltas, |delta, new_delta| {
                delta.x += Fixed::from_i32(new_delta) * scalar;
            })?;
            read_dense_deltas(&mut cursor, deltas, |delta, new_delta| {
                delta.y += Fixed::from_i32(new_delta) * scalar;
            })?;
        }
        Ok(())
    }

    /// Adds this tuple's deltas, times `scalar`, to the points it names, and
    /// marks each with [`PointMarker::HAS_DELTA`]: the tuple must be sparse.
    pub(crate) fn accumulate_sparse_deltas(
        &self,
        deltas: &mut [Point<Fixed>],
        flags: &mut [PointFlags],
        scalar: Fixed,
    ) -> Result<(), ReadError> {
        let (point_numbers, packed_deltas) = self.point_numbers_and_packed_deltas();
        let mut cursor = Cursor::new(packed_deltas);
        let count = usize::from(point_numbers.count());
        if scalar == Fixed::ONE {
            read_sparse_deltas(&mut cursor, &point_numbers, count, |ix, new_delta| {
                if let Some((delta, flag)) = deltas.get_mut(ix).zip(flags.get_mut(ix)) {
                    delta.x += Fixed::from_i32(new_delta);
                    flag.set_marker(PointMarker::HAS_DELTA);
                }
            })?;
            read_sparse_deltas(&mut cursor, &point_numbers, count, |ix, new_delta| {
                if let Some(delta) = deltas.get_mut(ix) {
                    delta.y += Fixed::from_i32(new_delta);
                }
            })?;
        } else {
            read_sparse_deltas(&mut cursor, &point_numbers, count, |ix, new_delta| {
                if let Some((delta, flag)) = deltas.get_mut(ix).zip(flags.get_mut(ix)) {
                    delta.x += Fixed::from_i32(new_delta) * scalar;
                    flag.set_marker(PointMarker::HAS_DELTA);
                }
            })?;
            read_sparse_deltas(&mut cursor, &point_numbers, count, |ix, new_delta| {
                if let Some(delta) = deltas.get_mut(ix) {
                    delta.y += Fixed::from_i32(new_delta) * scalar;
                }
            })?;
        }
        Ok(())
    }
}

const DELTAS_ARE_ZERO: u8 = 0x80;
const DELTAS_ARE_WORDS: u8 = 0x40;
const DELTA_RUN_COUNT_MASK: u8 = 0x3F;

/// The value type of a delta run; the discriminant is its byte size.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DeltaRunType {
    Zero = 0,
    I8 = 1,
    I16 = 2,
    I32 = 4,
}

impl DeltaRunType {
    fn new(control: u8) -> Self {
        let are_zero = (control & DELTAS_ARE_ZERO) != 0;
        let are_words = (control & DELTAS_ARE_WORDS) != 0;
        match (are_zero, are_words) {
            (false, false) => Self::I8,
            (false, true) => Self::I16,
            (true, false) => Self::Zero,
            (true, true) => Self::I32,
        }
    }
}

/// Reads delta runs into `deltas` front to back until each has one.
fn read_dense_deltas<T>(
    cursor: &mut Cursor,
    deltas: &mut [T],
    mut f: impl FnMut(&mut T, i32),
) -> Result<(), ReadError> {
    let count = deltas.len();
    let mut cur = 0;
    while cur < count {
        let control = cursor.read_u8()?;
        let value_type = DeltaRunType::new(control);
        let run_count = ((control & DELTA_RUN_COUNT_MASK) + 1) as usize;
        let dest = deltas.get_mut(cur..cur + run_count).ok_or(ReadError)?;
        match value_type {
            DeltaRunType::Zero => {}
            DeltaRunType::I8 => {
                let packed = cursor.read_bytes(run_count)?;
                for (delta, new_delta) in dest.iter_mut().zip(packed) {
                    f(delta, *new_delta as i8 as i32);
                }
            }
            DeltaRunType::I16 => {
                let packed = cursor.read_bytes(run_count * 2)?;
                for (delta, new_delta) in dest.iter_mut().zip(packed.as_chunks::<2>().0) {
                    f(delta, i32::from(i16::from_be_bytes(*new_delta)));
                }
            }
            DeltaRunType::I32 => {
                let packed = cursor.read_bytes(run_count * 4)?;
                for (delta, new_delta) in dest.iter_mut().zip(packed.as_chunks::<4>().0) {
                    f(delta, i32::from_be_bytes(*new_delta));
                }
            }
        }
        cur += run_count;
    }
    Ok(())
}

/// Reads `count` deltas for the points `point_numbers` names.
fn read_sparse_deltas(
    cursor: &mut Cursor,
    point_numbers: &PackedPointNumbers,
    count: usize,
    mut f: impl FnMut(usize, i32),
) -> Result<(), ReadError> {
    let mut cur = 0;
    let mut points_iter = point_numbers.iter().map(usize::from);
    while cur < count {
        let control = cursor.read_u8()?;
        let value_type = DeltaRunType::new(control);
        let run_count = ((control & DELTA_RUN_COUNT_MASK) + 1) as usize;
        match value_type {
            DeltaRunType::Zero => {
                for _ in 0..run_count {
                    let point_ix = points_iter.next().ok_or(ReadError)?;
                    f(point_ix, 0);
                }
            }
            DeltaRunType::I8 => {
                let packed = cursor.read_bytes(run_count)?;
                for (new_delta, point_ix) in packed.iter().zip(points_iter.by_ref()) {
                    f(point_ix, *new_delta as i8 as i32);
                }
            }
            DeltaRunType::I16 => {
                let packed = cursor.read_bytes(run_count * 2)?;
                for (new_delta, point_ix) in
                    packed.as_chunks::<2>().0.iter().zip(points_iter.by_ref())
                {
                    f(point_ix, i32::from(i16::from_be_bytes(*new_delta)));
                }
            }
            DeltaRunType::I32 => {
                let packed = cursor.read_bytes(run_count * 4)?;
                for (new_delta, point_ix) in
                    packed.as_chunks::<4>().0.iter().zip(points_iter.by_ref())
                {
                    f(point_ix, i32::from_be_bytes(*new_delta));
                }
            }
        }
        cur += run_count;
    }
    Ok(())
}

/// Packed point numbers: a count, then runs of point-number increments.
#[derive(Clone, Copy, Default, Debug)]
struct PackedPointNumbers<'a> {
    data: &'a [u8],
}

impl<'a> PackedPointNumbers<'a> {
    /// The point numbers at the front of `data`, and the data after them.
    fn split_off_front(data: &'a [u8]) -> (Self, &'a [u8]) {
        let this = PackedPointNumbers { data };
        let total_len = this.total_len();
        let remainder = data.get(total_len..).unwrap_or_default();
        (this, remainder)
    }

    /// The number of points in the set; 0 means every point.
    fn count(&self) -> u16 {
        self.count_and_count_bytes().0
    }

    /// The point count and the bytes it takes: at most 0x7FFF points, in
    /// one byte or two, whatever the data holds.
    #[cfg_attr(
        trust_verify,
        trust::ensures(result.0 <= 0x7FFF && (result.1 == 1 || result.1 == 2))
    )]
    fn count_and_count_bytes(&self) -> (u16, usize) {
        match read_u8(self.data, 0).unwrap_or(0) {
            0 => (0, 1),
            count @ 1..=127 => (u16::from(count), 1),
            _ => {
                // A count with its high bit set takes a second byte; the
                // count is the big-endian u16 with that bit masked out. As
                // fonttools does, a masked count of 0 reads as no points.
                let count = read_u16(self.data, 0).unwrap_or_default() & 0x7FFF;
                if count == 0 { (0, 2) } else { (count, 2) }
            }
        }
    }

    /// The bytes the packed numbers take.
    fn total_len(&self) -> usize {
        let (n_points, mut n_bytes) = self.count_and_count_bytes();
        if n_points == 0 {
            return n_bytes;
        }
        let mut cursor = Cursor::new(self.data);
        cursor.advance_by(n_bytes);
        let mut n_seen: u16 = 0;
        while n_seen < n_points {
            let Some((count, two_bytes)) = read_control_byte(&mut cursor) else {
                return n_bytes;
            };
            let word_size = 1 + usize::from(two_bytes);
            let run_size = word_size * count as usize;
            n_bytes += run_size + 1;
            cursor.advance_by(run_size);
            n_seen = n_seen.wrapping_add(u16::from(count));
        }
        n_bytes
    }

    fn iter(&self) -> PackedPointNumbersIter<'a> {
        let (count, n_bytes) = self.count_and_count_bytes();
        let mut cursor = Cursor::new(self.data);
        cursor.advance_by(n_bytes);
        PackedPointNumbersIter {
            count,
            seen: 0,
            last_val: 0,
            current_run: PointRunIter {
                remaining: 0,
                two_bytes: false,
                cursor,
            },
        }
    }
}

/// The count and the two-byte flag a run's control byte holds.
fn read_control_byte(cursor: &mut Cursor) -> Option<(u8, bool)> {
    let control = cursor.read_u8().ok()?;
    let two_bytes = (control & 0x80) != 0;
    let count = (control & 0x7F) + 1;
    Some((count, two_bytes))
}

#[derive(Clone, Debug)]
struct PointRunIter<'a> {
    remaining: u8,
    two_bytes: bool,
    cursor: Cursor<'a>,
}

impl Iterator for PointRunIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        while self.remaining == 0 {
            (self.remaining, self.two_bytes) = read_control_byte(&mut self.cursor)?;
        }
        self.remaining -= 1;
        if self.two_bytes {
            self.cursor.read_u16().ok()
        } else {
            self.cursor.read_u8().ok().map(u16::from)
        }
    }
}

#[derive(Clone, Debug)]
struct PackedPointNumbersIter<'a> {
    count: u16,
    seen: u16,
    last_val: u16,
    current_run: PointRunIter<'a>,
}

impl Iterator for PackedPointNumbersIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        // A count of zero counts up forever.
        if self.count == 0 {
            let result = self.last_val;
            self.last_val = self.last_val.checked_add(1)?;
            return Some(result);
        }
        if self.count == self.seen {
            return None;
        }
        self.seen += 1;
        self.last_val = self.last_val.checked_add(self.current_run.next()?)?;
        Some(self.last_val)
    }
}

/// Packed deltas: runs of zero, 8-, 16- or 32-bit values.
#[derive(Clone, Copy, Debug)]
struct PackedDeltas<'a> {
    data: &'a [u8],
    // How many values are expected; `None` reads every run in the data.
    count: Option<usize>,
}

impl<'a> PackedDeltas<'a> {
    fn count_or_compute(&self) -> usize {
        self.count.unwrap_or_else(|| count_all_deltas(self.data))
    }

    fn x_deltas(&self) -> DeltaRunIter<'a> {
        let count = self.count_or_compute() / 2;
        DeltaRunIter::new(Cursor::new(self.data), Some(count))
    }

    fn y_deltas(&self) -> DeltaRunIter<'a> {
        let count = self.count_or_compute();
        DeltaRunIter::new(Cursor::new(self.data), Some(count)).skip_fast(count / 2)
    }
}

fn count_all_deltas(data: &[u8]) -> usize {
    let mut count = 0usize;
    let mut offset = 0usize;
    while let Ok(control) = read_u8(data, offset) {
        let run_count = (control & DELTA_RUN_COUNT_MASK) as usize + 1;
        count += run_count;
        offset += run_count * DeltaRunType::new(control) as usize + 1;
    }
    count
}

#[derive(Clone, Debug)]
struct DeltaRunIter<'a> {
    limit: Option<usize>,
    remaining_in_run: u8,
    value_type: DeltaRunType,
    cursor: Cursor<'a>,
}

impl<'a> DeltaRunIter<'a> {
    fn new(cursor: Cursor<'a>, limit: Option<usize>) -> Self {
        DeltaRunIter {
            limit,
            remaining_in_run: 0,
            value_type: DeltaRunType::I8,
            cursor,
        }
    }

    /// Skips `n` deltas without reading their values.
    fn skip_fast(mut self, n: usize) -> Self {
        let mut wanted = n;
        let mut remaining = self.remaining_in_run as usize;
        let mut value_type = self.value_type;
        loop {
            if wanted > remaining {
                // Consume the rest of this run and move to the next.
                self.cursor.advance_by(remaining * value_type as usize);
                wanted -= remaining;
                if self.read_next_control().is_none() {
                    self.limit = Some(0);
                    break;
                }
                remaining = self.remaining_in_run as usize;
                value_type = self.value_type;
                continue;
            }
            let consumed = wanted;
            self.remaining_in_run -= consumed as u8;
            self.cursor.advance_by(consumed * value_type as usize);
            if let Some(limit) = self.limit.as_mut() {
                *limit = limit.saturating_sub(n);
            }
            break;
        }
        self
    }

    fn read_next_control(&mut self) -> Option<()> {
        self.remaining_in_run = 0;
        let control = self.cursor.read_u8().ok()?;
        self.value_type = DeltaRunType::new(control);
        self.remaining_in_run = (control & DELTA_RUN_COUNT_MASK) + 1;
        Some(())
    }
}

impl Iterator for DeltaRunIter<'_> {
    type Item = i32;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(limit) = self.limit {
            if limit == 0 {
                return None;
            }
            self.limit = Some(limit - 1);
        }
        if self.remaining_in_run == 0 {
            self.read_next_control()?;
        }
        self.remaining_in_run -= 1;
        match self.value_type {
            DeltaRunType::Zero => Some(0),
            DeltaRunType::I8 => self.cursor.read_i8().ok().map(i32::from),
            DeltaRunType::I16 => self.cursor.read_i16().ok().map(i32::from),
            DeltaRunType::I32 => self.cursor.read_i32().ok(),
        }
    }
}

/// The explicitly encoded deltas of one tuple, point by point.
#[derive(Clone, Debug)]
pub(crate) struct TupleDeltaIter<'a> {
    cur: usize,
    // `None`: every point has a delta; `Some`: the points that do.
    points: Option<PackedPointNumbersIter<'a>>,
    next_point: usize,
    x: DeltaRunIter<'a>,
    y: DeltaRunIter<'a>,
}

impl<'a> TupleDeltaIter<'a> {
    fn new(points: &PackedPointNumbers<'a>, deltas: PackedDeltas<'a>) -> TupleDeltaIter<'a> {
        let mut points = points.iter();
        let next_point = points.next();
        TupleDeltaIter {
            cur: 0,
            points: next_point.map(|_| points),
            next_point: usize::from(next_point.unwrap_or_default()),
            x: deltas.x_deltas(),
            y: deltas.y_deltas(),
        }
    }
}

impl Iterator for TupleDeltaIter<'_> {
    type Item = GlyphDelta;

    fn next(&mut self) -> Option<Self::Item> {
        let (position, dx, dy) = loop {
            let position = if let Some(points) = &mut self.points {
                // Sparse: only some points have deltas.
                if self.cur > self.next_point {
                    self.next_point = usize::from(points.next()?);
                }
                self.next_point
            } else {
                // Dense: the next point has the next delta.
                self.cur
            };
            if position == self.cur {
                break (position, self.x.next()?, self.y.next()?);
            }
            self.cur += 1;
        };
        self.cur += 1;
        Some(GlyphDelta {
            position: position as u16,
            x_delta: dx,
            y_delta: dy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // read-fonts 0.43.2 src/tables/variations.rs `packed_points`,
    // `packed_point_byte_len`, `packed_deltas`, `packed_deltas_spec`,
    // `packed_point_split`, `packed_points_dont_panic`,
    // `packed_point_numbers_avoid_overflow`.

    fn decode_points(bytes: &[u8]) -> Option<Vec<u16>> {
        let packed = PackedPointNumbers { data: bytes };
        if packed.count() == 0 {
            None
        } else {
            Some(packed.iter().collect())
        }
    }

    #[test]
    fn packed_points() {
        assert_eq!(decode_points(&[0]), None);
        // all points in glyph (in overly long representation)
        assert_eq!(decode_points(&[0x80, 0]), None);
        // 2 points; first run: [9, 9+6]
        assert_eq!(decode_points(&[0x02, 0x01, 0x09, 0x06]), Some(vec![9, 15]));
        // 2 points; first run: [0xBEEF, 0xBEEF+0x1001]
        assert_eq!(
            decode_points(&[0x02, 0x81, 0xbe, 0xef, 0x10, 0x01]),
            Some(vec![0xbeef, 0xcef0])
        );
        // 5 points; first run: [1, 1+1]; second run: [2+5, 2+5+8, 2+5+8+3]
        assert_eq!(
            decode_points(&[0x05, 0x01, 0x01, 0x01, 0x02, 0x05, 0x08, 0x03]),
            Some(vec![1, 2, 7, 15, 18])
        );
        // 3 points; run of 3 words
        assert_eq!(
            decode_points(&[0x03, 0x82, 0xbe, 0xef, 0x10, 0x01, 0x00, 0x02]),
            Some(vec![0xbeef, 0xcef0, 0xcef2])
        );
    }

    #[test]
    fn packed_point_byte_len() {
        fn count_bytes(bytes: &[u8]) -> usize {
            PackedPointNumbers { data: bytes }.total_len()
        }
        static CASES: &[&[u8]] = &[
            &[0],
            &[0x80, 0],
            &[0x02, 0x01, 0x09, 0x06],
            &[0x02, 0x81, 0xbe, 0xef, 0x10, 0x01],
            &[0x05, 0x01, 0x01, 0x01, 0x02, 0x05, 0x08, 0x03],
        ];
        for case in CASES {
            assert_eq!(count_bytes(case), case.len(), "{case:?}");
        }
    }

    #[test]
    fn packed_deltas() {
        static INPUT: &[u8] = &[0x83, 0x40, 0x01, 0x02, 0x01, 0x81, 0x80];
        let deltas = PackedDeltas {
            data: INPUT,
            count: None,
        };
        assert_eq!(deltas.count_or_compute(), 7);
        assert_eq!(
            DeltaRunIter::new(Cursor::new(INPUT), None).collect::<Vec<_>>(),
            &[0, 0, 0, 0, 258, -127, -128]
        );
        assert_eq!(
            DeltaRunIter::new(Cursor::new(&[0x81]), None).collect::<Vec<_>>(),
            &[0, 0]
        );
    }

    // https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#packed-deltas
    #[test]
    fn packed_deltas_spec() {
        static INPUT: &[u8] = &[
            0x03, 0x0A, 0x97, 0x00, 0xC6, 0x87, 0x41, 0x10, 0x22, 0xFB, 0x34,
        ];
        static EXPECTED: &[i32] = &[10, -105, 0, -58, 0, 0, 0, 0, 0, 0, 0, 0, 4130, -1228];
        let deltas = PackedDeltas {
            data: INPUT,
            count: None,
        };
        assert_eq!(deltas.count_or_compute(), EXPECTED.len());
        assert_eq!(
            DeltaRunIter::new(Cursor::new(INPUT), None).collect::<Vec<_>>(),
            EXPECTED
        );
    }

    #[test]
    fn packed_point_split() {
        static INPUT: &[u8] = &[2, 1, 1, 2, 1, 205, 143, 1, 8, 0, 1, 202, 59, 1, 255, 0];
        let (points, data) = PackedPointNumbers::split_off_front(INPUT);
        assert_eq!(points.count(), 2);
        assert_eq!(points.iter().collect::<Vec<_>>(), &[1, 3]);
        assert_eq!(points.total_len(), 4);
        assert_eq!(data.len(), INPUT.len() - 4);
    }

    #[test]
    fn packed_points_dont_panic() {
        // a single '0' byte means that there are deltas for all points
        static ALL_POINTS: &[u8] = &[0];
        let (all_points, _) = PackedPointNumbers::split_off_front(ALL_POINTS);
        // in which case the iterator just keeps incrementing until u16::MAX
        assert_eq!(all_points.iter().count(), u16::MAX as usize);
    }

    #[test]
    fn packed_point_numbers_avoid_overflow() {
        // Lots of large point numbers that overflow a u16 when summed.
        let buf = vec![0xFF; 0xFFFF];
        let iter = PackedPointNumbersIter {
            count: 0xFFFF,
            seen: 0,
            last_val: 0,
            current_run: PointRunIter {
                remaining: 0,
                two_bytes: false,
                cursor: Cursor::new(&buf),
            },
        };
        // Don't panic; the sum stops the iteration instead.
        let _ = iter.count();
    }

    /// The dense fast path and the sparse path add the same scaled deltas,
    /// and the sparse one marks exactly the points it touches.
    #[test]
    fn accumulate_dense_and_sparse() {
        // read-fonts' `accumulate_dense`/`accumulate_sparse` shapes, on a
        // hand-built tuple: four points, x deltas then y deltas.
        let header = [0, 0, 0x80, 0x00, 0x40, 0x00];
        let header = TupleVariationHeader {
            data: &header,
            axis_count: 1,
        };
        // Dense: every point (a zero point count), 8-bit runs of 4 + 4.
        let dense = [0x00, 0x03, 1, 2, 3, 4, 0x03, 5, 6, 7, 8];
        let tuple = TupleVariation {
            header: TupleVariationHeader {
                data: &[0, 0, 0xA0, 0x00, 0x40, 0x00],
                ..header
            },
            serialized_data: &dense,
            shared_point_numbers: None,
        };
        assert!(tuple.has_deltas_for_all_points());
        let mut deltas = [Point::default(); 4];
        let half = Fixed::from_bits(0x8000);
        tuple.accumulate_dense_deltas(&mut deltas, half).unwrap();
        let expected: Vec<_> = [(1, 5), (2, 6), (3, 7), (4, 8)]
            .iter()
            .map(|&(x, y)| Point::new(Fixed::from_i32(x) * half, Fixed::from_i32(y) * half))
            .collect();
        assert_eq!(&deltas[..], &expected[..]);
        // Sparse: points 1 and 3.
        let sparse = [0x02, 0x01, 0x01, 0x02, 0x01, 10, 20, 0x01, 30, 40];
        let tuple = TupleVariation {
            serialized_data: &sparse,
            ..tuple
        };
        assert!(!tuple.has_deltas_for_all_points());
        let mut deltas = [Point::default(); 4];
        let mut flags = [PointFlags::default(); 4];
        tuple
            .accumulate_sparse_deltas(&mut deltas, &mut flags, Fixed::ONE)
            .unwrap();
        assert_eq!(
            deltas[1],
            Point::new(Fixed::from_i32(10), Fixed::from_i32(30))
        );
        assert_eq!(
            deltas[3],
            Point::new(Fixed::from_i32(20), Fixed::from_i32(40))
        );
        let marked: Vec<bool> = flags
            .iter()
            .map(|f| f.has_marker(PointMarker::HAS_DELTA))
            .collect();
        assert_eq!(marked, [false, true, false, true]);
        // And the iterator yields the same explicit deltas.
        let explicit: Vec<_> = tuple.deltas().collect();
        assert_eq!(
            explicit,
            [
                GlyphDelta {
                    position: 1,
                    x_delta: 10,
                    y_delta: 30
                },
                GlyphDelta {
                    position: 3,
                    x_delta: 20,
                    y_delta: 40
                }
            ]
        );
    }
}
