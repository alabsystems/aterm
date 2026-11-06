// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/gvar.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! A [Glyph Variations Table](https://docs.microsoft.com/en-us/typography/opentype/spec/gvar) implementation.

#![allow(clippy::clone_on_copy)]
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use core::cmp;
use core::convert::TryFrom;
use core::num::NonZeroU16;

use crate::parser::{F2DOT14, LazyArray16, Offset, Offset16, Offset32, Stream};
use crate::{GlyphId, NormalizedCoordinate, OutlineSink, PointSink, Rect, RectF, Transform};
use crate::{PhantomPoints, PointF, glyf};

/// 'The TrueType rasterizer dynamically generates 'phantom' points for each glyph
/// that represent horizontal and vertical advance widths and side bearings,
/// and the variation data within the `gvar` table includes data for these phantom points.'
const PHANTOM_POINTS_LEN: usize = 4;

#[derive(Clone, Copy)]
enum GlyphVariationDataOffsets<'a> {
    Short(LazyArray16<'a, Offset16>),
    Long(LazyArray16<'a, Offset32>),
}

#[derive(Clone, Copy, Default)]
struct VariationTuple<'a> {
    set_points: Option<SetPointsIter<'a>>,
    deltas: PackedDeltasIter<'a>,
}

/// The maximum number of variation tuples stored on the stack.
const MAX_STACK_TUPLES_LEN: u16 = 32;

/// The variation tuples of one glyph: on the stack up to
/// [`MAX_STACK_TUPLES_LEN`], on the heap past it.
#[expect(
    clippy::large_enum_variant,
    reason = "the inline array is the point: a glyph of up to 32 tuples (nearly every one) keeps its tuple headers off the heap"
)]
enum VariationTuples<'a> {
    Stack {
        headers: [VariationTuple<'a>; MAX_STACK_TUPLES_LEN as usize],
        len: u16,
    },
    Heap {
        vec: Vec<VariationTuple<'a>>,
    },
}

impl<'a> Default for VariationTuples<'a> {
    fn default() -> Self {
        Self::Stack {
            headers: [VariationTuple::default(); MAX_STACK_TUPLES_LEN as usize],
            len: 0,
        }
    }
}

impl<'a> VariationTuples<'a> {
    /// Make room for `capacity` tuples in all, moving to the heap past
    /// [`MAX_STACK_TUPLES_LEN`].
    fn reserve(&mut self, capacity: u16) {
        match self {
            Self::Stack { headers, len } if capacity > MAX_STACK_TUPLES_LEN => {
                let mut vec = Vec::with_capacity(usize::from(capacity));
                vec.extend(headers.iter().take(usize::from(*len)).copied());
                *self = Self::Heap { vec };
            }
            Self::Heap { vec } => vec.reserve(usize::from(capacity).saturating_sub(vec.len())),
            Self::Stack { .. } => {}
        }
    }

    fn len(&self) -> u16 {
        match self {
            Self::Stack { len, .. } => *len,
            Self::Heap { vec } => u16::try_from(vec.len()).unwrap_or(u16::MAX),
        }
    }

    /// Append a new tuple header, moving to the heap when the stack is full.
    #[inline]
    fn push(&mut self, header: VariationTuple<'a>) {
        self.reserve(self.len().saturating_add(1));
        match self {
            Self::Stack { headers, len } => {
                // `reserve` keeps the stack only while a slot is free, so
                // this slot exists.
                if let Some(slot) = headers.get_mut(usize::from(*len)) {
                    *slot = header;
                    *len += 1;
                }
            }
            Self::Heap { vec } => vec.push(header),
        }
    }

    /// Remove all tuples from the structure.
    fn clear(&mut self) {
        match self {
            Self::Stack { len, .. } => *len = 0,
            Self::Heap { vec } => vec.clear(),
        }
    }

    #[inline]
    fn as_slice(&self) -> &[VariationTuple<'a>] {
        match self {
            Self::Stack { headers, len } => headers.get(..usize::from(*len)).unwrap_or(&[]),
            Self::Heap { vec } => vec.as_slice(),
        }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [VariationTuple<'a>] {
        match self {
            Self::Stack { headers, len } => headers.get_mut(..usize::from(*len)).unwrap_or(&mut []),
            Self::Heap { vec } => vec.as_mut_slice(),
        }
    }

    /// `points`, a simple glyph's outline, moved by every tuple in order.
    ///
    /// A tuple that numbers its points leaves the rest to be inferred from
    /// the numbered points either side of them (IUP), which [`infer_untouched`]
    /// does in a few passes over each contour, so each tuple costs time
    /// linear in the points; [`parse_variation_tuples`] charged that to the
    /// outline's variation budget when it kept the tuple. Each point sums its
    /// deltas in tuple order onto its stored coordinates, the order
    /// ttf-parser's per-point stream added them in, so every coordinate is
    /// the float it was.
    fn varied(&self, points: &[glyf::GlyphPoint]) -> Vec<PointF> {
        let mut out: Vec<PointF> = points
            .iter()
            .map(|p| PointF {
                x: f32::from(p.x),
                y: f32::from(p.y),
            })
            .collect();
        let (mut marks, mut next_numbered) = (Vec::new(), Vec::new());
        for tuple in self.as_slice() {
            let mut deltas = tuple.deltas;
            if let Some(set_points) = tuple.set_points {
                mark_points(set_points, deltas, points.len(), &mut marks);
                infer_untouched(points, &marks, &mut next_numbered, &mut out);
            } else {
                for p in &mut out {
                    if let Some((x_delta, y_delta)) = deltas.next() {
                        p.x += x_delta;
                        p.y += y_delta;
                    }
                }
            }
        }
        out
    }

    fn apply_null(&mut self) -> Option<PointF> {
        let mut x = 0.0;
        let mut y = 0.0;

        for tuple in self.as_mut_slice() {
            if let Some(ref mut set_points) = tuple.set_points {
                if set_points.next()? {
                    if let Some((x_delta, y_delta)) = tuple.deltas.next() {
                        x += x_delta;
                        y += y_delta;
                    }
                }
            } else {
                if let Some((x_delta, y_delta)) = tuple.deltas.next() {
                    x += x_delta;
                    y += y_delta;
                }
            }
        }

        Some(PointF { x, y })
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct TupleVariationHeaderData {
    scalar: f32,
    has_private_point_numbers: bool,
    serialized_data_len: u16,
}

fn parse_variation_tuples<'a>(
    count: u16,
    coordinates: &[NormalizedCoordinate],
    shared_tuple_records: &LazyArray16<F2DOT14>,
    shared_point_numbers: Option<PackedPointsIter<'a>>,
    points_len: u16,
    mut main_s: Stream<'a>,
    mut serialized_s: Stream<'a>,
    tuples: &mut VariationTuples<'a>,
    work: &mut crate::work::OutlineWork,
) -> Option<()> {
    debug_assert!(core::mem::size_of::<VariationTuple>() <= 80);

    // Counted once, not again for every tuple that uses them.
    let mut shared_count = None;
    for _ in 0..count {
        work.vary(1)?;
        let header = parse_tuple_variation_header(coordinates, shared_tuple_records, &mut main_s)?;
        if !(header.scalar > 0.0) {
            serialized_s.advance(usize::from(header.serialized_data_len));
            continue;
        }
        // Each caller walks a kept tuple over all `points_len` points: the
        // outline's inference, the phantom points and a composite's components.
        work.vary(usize::from(points_len))?;

        let serialized_data_start = serialized_s.offset();

        let point_numbers = if header.has_private_point_numbers {
            PackedPointsIter::new(&mut serialized_s)?
        } else {
            shared_point_numbers.clone()
        };

        let deltas_count = match point_numbers {
            None => points_len,
            Some(numbers) if header.has_private_point_numbers => {
                u16::try_from(numbers.count()).ok()?
            }
            Some(numbers) => match shared_count {
                Some(count) => count,
                None => *shared_count.insert(u16::try_from(numbers.count()).ok()?),
            },
        };

        let deltas = {
            let left = usize::from(header.serialized_data_len)
                .checked_sub(serialized_s.offset() - serialized_data_start)?;
            let deltas_data = serialized_s.read_bytes(left)?;
            PackedDeltasIter::new(header.scalar, deltas_count, deltas_data)
        };

        let tuple = VariationTuple {
            set_points: point_numbers.map(SetPointsIter::new),
            deltas,
        };

        tuples.push(tuple);
    }

    Some(())
}

fn parse_tuple_variation_header(
    coordinates: &[NormalizedCoordinate],
    shared_tuple_records: &LazyArray16<F2DOT14>,
    s: &mut Stream,
) -> Option<TupleVariationHeaderData> {
    const EMBEDDED_PEAK_TUPLE_FLAG: u16 = 0x8000;
    const INTERMEDIATE_REGION_FLAG: u16 = 0x4000;
    const PRIVATE_POINT_NUMBERS_FLAG: u16 = 0x2000;
    const TUPLE_INDEX_MASK: u16 = 0x0FFF;

    let serialized_data_size = s.read::<u16>()?;
    let tuple_index = s.read::<u16>()?;

    let has_embedded_peak_tuple = tuple_index & EMBEDDED_PEAK_TUPLE_FLAG != 0;
    let has_intermediate_region = tuple_index & INTERMEDIATE_REGION_FLAG != 0;
    let has_private_point_numbers = tuple_index & PRIVATE_POINT_NUMBERS_FLAG != 0;
    let tuple_index = tuple_index & TUPLE_INDEX_MASK;

    let axis_count = coordinates.len() as u16;

    let peak_tuple = if has_embedded_peak_tuple {
        s.read_array16::<F2DOT14>(axis_count)?
    } else {
        let start = tuple_index.checked_mul(axis_count)?;
        let end = start.checked_add(axis_count)?;
        shared_tuple_records.slice(start..end)?
    };

    let (start_tuple, end_tuple) = if has_intermediate_region {
        (
            s.read_array16::<F2DOT14>(axis_count)?,
            s.read_array16::<F2DOT14>(axis_count)?,
        )
    } else {
        (
            LazyArray16::<F2DOT14>::default(),
            LazyArray16::<F2DOT14>::default(),
        )
    };

    let mut header = TupleVariationHeaderData {
        scalar: 0.0,
        has_private_point_numbers,
        serialized_data_len: serialized_data_size,
    };

    let mut scalar = 1.0;
    for (i, v) in (0..axis_count).zip(coordinates) {
        let v = v.get();
        let peak = peak_tuple.get(i)?.0;
        if peak == 0 || v == peak {
            continue;
        }

        if has_intermediate_region {
            let start = start_tuple.get(i)?.0;
            let end = end_tuple.get(i)?.0;
            if start > peak || peak > end || (start < 0 && end > 0 && peak != 0) {
                continue;
            }

            if v < start || v > end {
                return Some(header);
            }

            let (v, start, peak, end) = (
                i32::from(v),
                i32::from(start),
                i32::from(peak),
                i32::from(end),
            );
            if v < peak {
                if peak != start {
                    scalar *= (v - start) as f32 / (peak - start) as f32;
                }
            } else {
                if peak != end {
                    scalar *= (end - v) as f32 / (end - peak) as f32;
                }
            }
        } else if v == 0 || v < cmp::min(0, peak) || v > cmp::max(0, peak) {
            // 'If the instance coordinate is out of range for some axis, then the
            // region and its associated deltas are not applicable.'
            return Some(header);
        } else {
            scalar *= f32::from(v) / f32::from(peak);
        }
    }

    header.scalar = scalar;
    Some(header)
}

mod packed_points {
    use crate::parser::{FromData, Stream};

    struct Control(u8);

    impl Control {
        const POINTS_ARE_WORDS_FLAG: u8 = 0x80;
        const POINT_RUN_COUNT_MASK: u8 = 0x7F;

        #[inline]
        fn is_points_are_words(&self) -> bool {
            self.0 & Self::POINTS_ARE_WORDS_FLAG != 0
        }

        #[inline]
        fn run_count(&self) -> u8 {
            (self.0 & Self::POINT_RUN_COUNT_MASK) + 1
        }
    }

    impl FromData for Control {
        const SIZE: usize = 1;

        #[inline]
        fn parse(data: &[u8]) -> Option<Self> {
            data.get(0).copied().map(Control)
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Control,
        ShortPoint,
        LongPoint,
    }

    #[derive(Clone, Copy)]
    pub struct PackedPointsIter<'a> {
        data: &'a [u8],
        offset: u16,
        state: State,
        points_left: u8,
    }

    impl<'a> PackedPointsIter<'a> {
        pub fn new<'b>(s: &'b mut Stream<'a>) -> Option<Option<Self>> {
            let b1 = s.read::<u8>()?;
            let mut count = u16::from(b1);
            if b1 & Control::POINTS_ARE_WORDS_FLAG != 0 {
                let b2 = s.read::<u8>()?;
                count = (u16::from(b1 & Control::POINT_RUN_COUNT_MASK) << 8) | u16::from(b2);
            }

            if count == 0 {
                // No points is not an error.
                return Some(None);
            }

            let start = s.offset();
            let tail = s.tail()?;

            let mut i = 0;
            while i < count {
                let control = s.read::<Control>()?;
                let run_count = u16::from(control.run_count());
                let is_points_are_words = control.is_points_are_words();
                s.advance_checked(
                    if is_points_are_words { 2 } else { 1 } * usize::from(run_count),
                )?;
                i += run_count;
            }

            if i == 0 {
                // No points is not an error.
                return Some(None);
            }

            if i > count {
                // Malformed font.
                return None;
            }

            let data_len = s.offset() - start;
            if data_len > usize::from(u16::MAX) {
                return None;
            }

            Some(Some(PackedPointsIter {
                data: tail.get(..data_len)?,
                offset: 0,
                state: State::Control,
                points_left: 0,
            }))
        }
    }

    impl<'a> Iterator for PackedPointsIter<'a> {
        type Item = u16;

        fn next(&mut self) -> Option<Self::Item> {
            if usize::from(self.offset) >= self.data.len() {
                return None;
            }

            if self.state == State::Control {
                let control = Control(*self.data.get(usize::from(self.offset))?);
                self.offset = self.offset.checked_add(1)?;

                self.points_left = control.run_count();
                self.state = if control.is_points_are_words() {
                    State::LongPoint
                } else {
                    State::ShortPoint
                };

                self.next()
            } else {
                let mut s = Stream::new_at(self.data, usize::from(self.offset))?;
                let point = if self.state == State::LongPoint {
                    let point = s.read::<u16>()?;
                    self.offset = self.offset.checked_add(2)?;
                    point
                } else {
                    let point = u16::from(s.read::<u8>()?);
                    self.offset = self.offset.checked_add(1)?;
                    point
                };

                self.points_left = self.points_left.saturating_sub(1);
                if self.points_left == 0 {
                    self.state = State::Control;
                }

                Some(point)
            }
        }
    }

    #[derive(Clone, Copy)]
    pub struct SetPointsIter<'a> {
        iter: PackedPointsIter<'a>,
        unref_count: u16,
    }

    impl<'a> SetPointsIter<'a> {
        #[inline]
        pub fn new(mut iter: PackedPointsIter<'a>) -> Self {
            let unref_count = iter.next().unwrap_or(0);
            SetPointsIter { iter, unref_count }
        }
    }

    impl<'a> Iterator for SetPointsIter<'a> {
        type Item = bool;

        #[inline]
        fn next(&mut self) -> Option<Self::Item> {
            if self.unref_count != 0 {
                self.unref_count -= 1;
                return Some(false);
            }

            if let Some(unref_count) = self.iter.next() {
                self.unref_count = unref_count;
                if self.unref_count != 0 {
                    self.unref_count -= 1;
                }
            }

            Some(true)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct NewControl {
            deltas_are_words: bool,
            run_count: u8,
        }

        fn gen_control(control: NewControl) -> u8 {
            assert!(control.run_count > 0, "run count cannot be zero");

            let mut n = 0;
            if control.deltas_are_words {
                n |= 0x80;
            }
            n |= (control.run_count - 1) & 0x7F;
            n
        }

        #[test]
        fn empty() {
            let mut s = Stream::new(&[]);
            assert!(PackedPointsIter::new(&mut s).is_none());
        }

        #[test]
        fn single_zero_control() {
            let mut s = Stream::new(&[0]);
            assert!(PackedPointsIter::new(&mut s).unwrap().is_none());
        }

        #[test]
        fn single_point() {
            let data = vec![
                1,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 1,
                }),
                1,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn set_0_and_2() {
            let data = vec![
                2,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 2,
                }),
                0,
                2,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn set_1_and_2() {
            let data = vec![
                2,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 2,
                }),
                1,
                1,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn set_1_and_3() {
            let data = vec![
                2,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 2,
                }),
                1,
                2,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn set_2_5_7() {
            let data = vec![
                3,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 3,
                }),
                2,
                3,
                2,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn more_than_127_points() {
            let mut data = vec![];
            data.push(Control::POINTS_ARE_WORDS_FLAG);
            data.push(150);

            data.push(gen_control(NewControl {
                deltas_are_words: false,
                run_count: 100,
            }));
            for _ in 0..100 {
                data.push(2);
            }
            data.push(gen_control(NewControl {
                deltas_are_words: false,
                run_count: 50,
            }));
            for _ in 0..50 {
                data.push(2);
            }
            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            for _ in 0..150 {
                assert_eq!(iter.next().unwrap(), false);
                assert_eq!(iter.next().unwrap(), true);
            }
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn long_points() {
            let data = vec![
                2,
                gen_control(NewControl {
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                2,
                0,
                3,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn multiple_runs() {
            let data = vec![
                5,
                gen_control(NewControl {
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                2,
                0,
                3,
                gen_control(NewControl {
                    deltas_are_words: false,
                    run_count: 3,
                }),
                2,
                3,
                2,
            ];

            let points_iter = PackedPointsIter::new(&mut Stream::new(&data))
                .unwrap()
                .unwrap();
            let mut iter = SetPointsIter::new(points_iter);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), false);
            assert_eq!(iter.next().unwrap(), true);
            assert_eq!(iter.next().unwrap(), true);
        }

        #[test]
        fn runs_overflow() {
            let data = vec![0xFF; 0xFFFF * 2];
            assert!(PackedPointsIter::new(&mut Stream::new(&data)).is_none());
        }
    }
}

use packed_points::*;

mod packed_deltas {
    use crate::parser::Stream;

    struct Control(u8);

    impl Control {
        const DELTAS_ARE_ZERO_FLAG: u8 = 0x80;
        const DELTAS_ARE_WORDS_FLAG: u8 = 0x40;
        const DELTA_RUN_COUNT_MASK: u8 = 0x3F;

        #[inline]
        fn is_deltas_are_zero(&self) -> bool {
            self.0 & Self::DELTAS_ARE_ZERO_FLAG != 0
        }

        #[inline]
        fn is_deltas_are_words(&self) -> bool {
            self.0 & Self::DELTAS_ARE_WORDS_FLAG != 0
        }

        #[inline]
        fn run_count(&self) -> u8 {
            (self.0 & Self::DELTA_RUN_COUNT_MASK) + 1
        }
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum State {
        Control,
        ZeroDelta,
        ShortDelta,
        LongDelta,
    }

    impl Default for State {
        #[inline]
        fn default() -> Self {
            State::Control
        }
    }

    #[derive(Clone, Copy, Default)]
    struct RunState {
        data_offset: u16,
        state: State,
        run_deltas_left: u8,
    }

    impl RunState {
        fn next(&mut self, data: &[u8], scalar: f32) -> Option<f32> {
            if self.state == State::Control {
                if usize::from(self.data_offset) == data.len() {
                    return None;
                }

                let control = Control(Stream::read_at::<u8>(data, usize::from(self.data_offset))?);
                self.data_offset = self.data_offset.checked_add(1)?;

                self.run_deltas_left = control.run_count();
                self.state = if control.is_deltas_are_zero() {
                    State::ZeroDelta
                } else if control.is_deltas_are_words() {
                    State::LongDelta
                } else {
                    State::ShortDelta
                };

                self.next(data, scalar)
            } else {
                let mut s = Stream::new_at(data, usize::from(self.data_offset))?;
                let delta = if self.state == State::LongDelta {
                    let delta = f32::from(s.read::<i16>()?) * scalar;
                    self.data_offset = self.data_offset.checked_add(2)?;
                    delta
                } else if self.state == State::ZeroDelta {
                    0.0
                } else {
                    let delta = f32::from(s.read::<i8>()?) * scalar;
                    self.data_offset = self.data_offset.checked_add(1)?;
                    delta
                };

                self.run_deltas_left = self.run_deltas_left.saturating_sub(1);
                if self.run_deltas_left == 0 {
                    self.state = State::Control;
                }

                Some(delta)
            }
        }

        /// Ends where `n` calls of [`Self::next`] would, a run at a time: a
        /// step per control byte, never one per delta, so skipping the x
        /// deltas of a tuple whose data is short or a run of zeros costs its
        /// bytes, not its point count. A read past the data leaves the state
        /// where `next` would have stopped, answering `None` from there on.
        fn skip(&mut self, data: &[u8], mut n: u32) {
            while n > 0 {
                if self.state == State::Control {
                    let at = usize::from(self.data_offset);
                    let Some(control) = data.get(at).copied().map(Control) else {
                        return;
                    };
                    let Some(offset) = self.data_offset.checked_add(1) else {
                        return;
                    };
                    self.data_offset = offset;
                    self.run_deltas_left = control.run_count();
                    self.state = if control.is_deltas_are_zero() {
                        State::ZeroDelta
                    } else if control.is_deltas_are_words() {
                        State::LongDelta
                    } else {
                        State::ShortDelta
                    };
                }

                let width: u16 = match self.state {
                    State::ZeroDelta => 0,
                    State::ShortDelta => 1,
                    _ => 2,
                };
                let wanted =
                    u8::try_from(n).map_or(self.run_deltas_left, |n| n.min(self.run_deltas_left));
                let readable = match width {
                    0 => wanted,
                    w => {
                        let left = data.len().saturating_sub(usize::from(self.data_offset));
                        u8::try_from(left / usize::from(w)).map_or(wanted, |r| r.min(wanted))
                    }
                };
                // The bytes read are in `data`, whose offsets fit in `u16`.
                self.data_offset = self.data_offset.saturating_add(u16::from(readable) * width);
                self.run_deltas_left -= readable;
                n -= u32::from(readable);
                if readable < wanted {
                    return;
                }
                if self.run_deltas_left == 0 {
                    self.state = State::Control;
                }
            }
        }
    }

    #[derive(Clone, Copy, Default)]
    pub struct PackedDeltasIter<'a> {
        data: &'a [u8],
        x_run: RunState,
        y_run: RunState,
        scalar: f32,
    }

    impl<'a> PackedDeltasIter<'a> {
        /// Creates a delta stream for `count` delta pairs.
        pub fn new(scalar: f32, count: u16, data: &'a [u8]) -> Self {
            debug_assert!(core::mem::size_of::<PackedDeltasIter>() <= 32);

            let mut iter = PackedDeltasIter {
                data,
                scalar,
                ..PackedDeltasIter::default()
            };

            // The y deltas follow all `count` x deltas.
            iter.y_run.skip(data, u32::from(count));

            iter
        }

        #[inline]
        pub fn next(&mut self) -> Option<(f32, f32)> {
            let x = self.x_run.next(self.data, self.scalar)?;
            let y = self.y_run.next(self.data, self.scalar)?;
            Some((x, y))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct NewControl {
            deltas_are_zero: bool,
            deltas_are_words: bool,
            run_count: u8,
        }

        fn gen_control(control: NewControl) -> u8 {
            assert!(control.run_count > 0, "run count cannot be zero");

            let mut n = 0;
            if control.deltas_are_zero {
                n |= 0x80;
            }
            if control.deltas_are_words {
                n |= 0x40;
            }
            n |= (control.run_count - 1) & 0x3F;
            n
        }

        #[test]
        fn empty() {
            let mut iter = PackedDeltasIter::new(1.0, 1, &[]);
            assert!(iter.next().is_none());
        }

        #[test]
        fn single_delta() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                2,
                3,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert_eq!(iter.next().unwrap(), (2.0, 3.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn two_deltas() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 4,
                }),
                2,
                3,
                4,
                5,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 2, &data);
            assert_eq!(iter.next().unwrap(), (2.0, 4.0));
            assert_eq!(iter.next().unwrap(), (3.0, 5.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn single_long_delta() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                2,
                0,
                3,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert_eq!(iter.next().unwrap(), (2.0, 3.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn zeros() {
            let data = vec![gen_control(NewControl {
                deltas_are_zero: true,
                deltas_are_words: false,
                run_count: 4,
            })];

            let mut iter = PackedDeltasIter::new(1.0, 2, &data);
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn zero_words() {
            let data = vec![gen_control(NewControl {
                deltas_are_zero: true,
                deltas_are_words: true,
                run_count: 4,
            })];

            let mut iter = PackedDeltasIter::new(1.0, 2, &data);
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn zero_runs() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: true,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                gen_control(NewControl {
                    deltas_are_zero: true,
                    deltas_are_words: false,
                    run_count: 4,
                }),
                gen_control(NewControl {
                    deltas_are_zero: true,
                    deltas_are_words: false,
                    run_count: 6,
                }),
            ];

            let mut iter = PackedDeltasIter::new(1.0, 6, &data);
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn delta_after_zeros() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: true,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                2,
                3,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 2, &data);
            assert_eq!(iter.next().unwrap(), (0.0, 2.0));
            assert_eq!(iter.next().unwrap(), (0.0, 3.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_1() {
            let data = vec![gen_control(NewControl {
                deltas_are_zero: false,
                deltas_are_words: false,
                run_count: 2,
            })];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_2() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                1,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_3() {
            let data = vec![gen_control(NewControl {
                deltas_are_zero: false,
                deltas_are_words: true,
                run_count: 2,
            })];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_4() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: true,
                    run_count: 2,
                }),
                1,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_6() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                1,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn unexpected_end_of_data_7() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                1,
                0,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn single_run() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 1,
                }),
                2,
                3,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 1, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn too_many_pairs() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                2,
                3,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 10, &data);
            assert!(iter.next().is_none());
        }

        #[test]
        fn invalid_number_of_pairs() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                2,
                3,
                4,
                5,
                6,
                7,
            ];

            let mut iter = PackedDeltasIter::new(1.0, 4, &data);
            assert_eq!(iter.next().unwrap(), (2.0, 7.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn mixed_runs() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 3,
                }),
                2,
                3,
                4,
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: true,
                    run_count: 2,
                }),
                0,
                5,
                0,
                6,
                gen_control(NewControl {
                    deltas_are_zero: true,
                    deltas_are_words: false,
                    run_count: 1,
                }),
            ];

            let mut iter = PackedDeltasIter::new(1.0, 3, &data);
            assert_eq!(iter.next().unwrap(), (2.0, 5.0));
            assert_eq!(iter.next().unwrap(), (3.0, 6.0));
            assert_eq!(iter.next().unwrap(), (4.0, 0.0));
            assert!(iter.next().is_none());
        }

        #[test]
        fn non_default_scalar() {
            let data = vec![
                gen_control(NewControl {
                    deltas_are_zero: false,
                    deltas_are_words: false,
                    run_count: 2,
                }),
                2,
                3,
            ];

            let mut iter = PackedDeltasIter::new(0.5, 1, &data);
            assert_eq!(iter.next().unwrap(), (1.0, 1.5));
            assert!(iter.next().is_none());
        }

        #[test]
        fn runs_overflow() {
            let data = vec![0xFF; 0xFFFF];
            let mut iter = PackedDeltasIter::new(1.0, 0xFFFF, &data);
            assert_eq!(iter.next().unwrap(), (0.0, 0.0));
        }
    }
}

use packed_deltas::PackedDeltasIter;

/// How a tuple that numbers its points reaches one point of a simple glyph.
#[derive(Clone, Copy)]
enum Mark {
    /// Not numbered: its delta is inferred.
    Unset,
    /// Numbered after the tuple's deltas ran out: inferred too, yet it still
    /// ends the search for the next numbered point, and first in its contour
    /// it stands in for that point with no delta.
    Starved,
    /// Numbered, with this delta.
    Delta(f32, f32),
}

/// Marks `len` points as the tuple's point numbers and deltas reach them, in order.
fn mark_points(
    mut set_points: SetPointsIter,
    mut deltas: PackedDeltasIter,
    len: usize,
    marks: &mut Vec<Mark>,
) {
    marks.clear();
    marks.extend((0..len).map(|_| {
        if set_points.next() == Some(true) {
            deltas
                .next()
                .map_or(Mark::Starved, |(x, y)| Mark::Delta(x, y))
        } else {
            Mark::Unset
        }
    }));
}

/// Adds one numbered tuple's deltas to `out`: a [`Mark::Delta`] point's own,
/// every other point's inferred (`infer_delta`) between the contour's points
/// with deltas before and after it, wrapping around the contour.
///
/// It answers what ttf-parser's per-point rescans did, in a fixed number of
/// passes over each contour, and keeps their quirks: the search for the next
/// point stops at any numbered point, a [`Mark::Starved`] one included, and
/// then falls back to the contour's first numbered point; and a contour's
/// last point, when nothing before it in the contour has a delta, takes its
/// previous point from the contour after it.
fn infer_untouched(
    points: &[glyf::GlyphPoint],
    marks: &[Mark],
    next_numbered: &mut Vec<usize>,
    out: &mut [PointF],
) {
    let has_delta = |at: usize| matches!(marks.get(at), Some(Mark::Delta(..)));
    let delta = |at: usize| match marks.get(at) {
        Some(&Mark::Delta(x, y)) => (x, y),
        _ => (0.0, 0.0),
    };
    // A contour runs to its first point flagged last, the glyph's final
    // contour to its final point when none is.
    let contour_end = |start: usize| {
        points
            .iter()
            .skip(start)
            .position(|p| p.last_point)
            .map_or(points.len().saturating_sub(1), |at| start + at)
    };
    let last_delta = |start: usize, end: usize| (start..=end).rev().find(|&at| has_delta(at));

    // The first numbered point after each point in its contour, built from the back.
    next_numbered.clear();
    next_numbered.resize(points.len(), usize::MAX);
    let mut next = usize::MAX;
    for at in (0..points.len()).rev() {
        if points.get(at).is_some_and(|p| p.last_point) {
            next = usize::MAX;
        }
        if let Some(slot) = next_numbered.get_mut(at) {
            *slot = next;
        }
        if !matches!(marks.get(at), Some(Mark::Unset) | None) {
            next = at;
        }
    }

    let mut start = 0;
    while start < points.len() {
        let end = contour_end(start);
        let first_numbered =
            (start..=end).find(|&at| !matches!(marks.get(at), Some(Mark::Unset) | None));
        let last_in_contour = last_delta(start, end);
        let mut prev = None;
        for at in start..=end {
            let Some(point) = points.get(at) else { break };
            if let Some(&Mark::Delta(x_delta, y_delta)) = marks.get(at) {
                if let Some(p) = out.get_mut(at) {
                    p.x += x_delta;
                    p.y += y_delta;
                }
                prev = Some(at);
                continue;
            }

            let before = match prev {
                Some(before) => Some(before),
                // Nothing before it has a delta, so the contour's last one
                // that does comes after it.
                None if !point.last_point => last_in_contour,
                None if end + 1 < points.len() => last_delta(end + 1, contour_end(end + 1)),
                None => None,
            };
            let after = if point.last_point {
                None
            } else {
                next_numbered
                    .get(at)
                    .copied()
                    .filter(|&next| has_delta(next))
            };
            let (Some(before), Some(after)) = (before, after.or(first_numbered)) else {
                continue;
            };
            let (Some(b), Some(a)) = (points.get(before), points.get(after)) else {
                continue;
            };
            let ((bx, by), (ax, ay)) = (delta(before), delta(after));
            if let Some(p) = out.get_mut(at) {
                p.x += infer_delta(b.x, point.x, a.x, bx, ax);
                p.y += infer_delta(b.y, point.y, a.y, by, ay);
            }
        }
        start = end + 1;
    }
}

fn infer_delta(
    prev_point: i16,
    target_point: i16,
    next_point: i16,
    prev_delta: f32,
    next_delta: f32,
) -> f32 {
    if prev_point == next_point {
        if prev_delta == next_delta {
            prev_delta
        } else {
            0.0
        }
    } else if target_point <= prev_point.min(next_point) {
        if prev_point < next_point {
            prev_delta
        } else {
            next_delta
        }
    } else if target_point >= prev_point.max(next_point) {
        if prev_point > next_point {
            prev_delta
        } else {
            next_delta
        }
    } else {
        let target_sub = target_point.checked_sub(prev_point);
        let next_sub = next_point.checked_sub(prev_point);
        let d = if let (Some(target_sub), Some(next_sub)) = (target_sub, next_sub) {
            f32::from(target_sub) / f32::from(next_sub)
        } else {
            return 0.0;
        };
        (1.0 - d) * prev_delta + d * next_delta
    }
}

/// A [Glyph Variations Table](https://docs.microsoft.com/en-us/typography/opentype/spec/gvar).
#[derive(Clone, Copy)]
pub struct Table<'a> {
    axis_count: NonZeroU16,
    shared_tuple_records: LazyArray16<'a, F2DOT14>,
    offsets: GlyphVariationDataOffsets<'a>,
    glyphs_variation_data: &'a [u8],
}

impl<'a> Table<'a> {
    /// Parses a table from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let version = s.read::<u32>()?;
        if version != 0x00010000 {
            return None;
        }

        let axis_count = s.read::<u16>()?;
        let shared_tuple_count = s.read::<u16>()?;
        let shared_tuples_offset = s.read::<Offset32>()?;
        let glyph_count = s.read::<u16>()?;
        let flags = s.read::<u16>()?;
        let glyph_variation_data_array_offset = s.read::<Offset32>()?;

        let axis_count = NonZeroU16::new(axis_count)?;

        let shared_tuple_records = {
            let mut sub_s = Stream::new_at(data, shared_tuples_offset.to_usize())?;
            sub_s.read_array16::<F2DOT14>(shared_tuple_count.checked_mul(axis_count.get())?)?
        };

        let glyphs_variation_data = data.get(glyph_variation_data_array_offset.to_usize()..)?;
        let offsets = {
            let offsets_count = glyph_count.checked_add(1)?;
            let is_long_format = flags & 1 == 1;
            if is_long_format {
                GlyphVariationDataOffsets::Long(s.read_array16::<Offset32>(offsets_count)?)
            } else {
                GlyphVariationDataOffsets::Short(s.read_array16::<Offset16>(offsets_count)?)
            }
        };

        Some(Table {
            axis_count,
            shared_tuple_records,
            offsets,
            glyphs_variation_data,
        })
    }

    #[inline]
    fn parse_variation_data(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
        points_len: u16,
        tuples: &mut VariationTuples<'a>,
        work: &mut crate::work::OutlineWork,
    ) -> Option<()> {
        tuples.clear();

        if coordinates.len() != usize::from(self.axis_count.get()) {
            return None;
        }

        let next_glyph_id = glyph_id.0.checked_add(1)?;

        let (start, end) = match self.offsets {
            GlyphVariationDataOffsets::Short(ref array) => {
                // Short offsets store the byte offset divided by 2.
                (
                    array.get(glyph_id.0)?.to_usize() * 2,
                    array.get(next_glyph_id)?.to_usize() * 2,
                )
            }
            GlyphVariationDataOffsets::Long(ref array) => (
                array.get(glyph_id.0)?.to_usize(),
                array.get(next_glyph_id)?.to_usize(),
            ),
        };

        if start == end {
            return Some(());
        }

        let data = self.glyphs_variation_data.get(start..end)?;
        parse_variation_data(
            coordinates,
            &self.shared_tuple_records,
            points_len,
            data,
            tuples,
            work,
        )
    }

    /// Outlines a glyph.
    pub fn outline(
        &self,
        glyf_table: glyf::Table,
        coordinates: &[NormalizedCoordinate],
        glyph_id: GlyphId,
        builder: &mut dyn OutlineSink,
    ) -> Option<Rect> {
        let mut work = crate::work::OutlineWork::default();
        self.outline_with(glyf_table, coordinates, glyph_id, builder, None, &mut work)
    }

    /// [`Table::outline`] under `work`.
    pub(crate) fn outline_within(
        &self,
        glyf_table: glyf::Table,
        coordinates: &[NormalizedCoordinate],
        glyph_id: GlyphId,
        builder: &mut dyn OutlineSink,
        work: &mut crate::work::OutlineWork,
    ) -> Option<Rect> {
        self.outline_with(glyf_table, coordinates, glyph_id, builder, None, work)
    }

    /// [`Table::outline`], also sending each stored point to `tap`.
    pub(crate) fn outline_tapped(
        &self,
        glyf_table: glyf::Table,
        coordinates: &[NormalizedCoordinate],
        glyph_id: GlyphId,
        builder: &mut dyn OutlineSink,
        tap: &mut dyn PointSink,
    ) -> Option<Rect> {
        let mut work = crate::work::OutlineWork::default();
        self.outline_with(
            glyf_table,
            coordinates,
            glyph_id,
            builder,
            Some(tap),
            &mut work,
        )
    }

    fn outline_with<'s>(
        &self,
        glyf_table: glyf::Table,
        coordinates: &[NormalizedCoordinate],
        glyph_id: GlyphId,
        builder: &'s mut dyn OutlineSink,
        tap: Option<&'s mut dyn PointSink>,
        work: &mut crate::work::OutlineWork,
    ) -> Option<Rect> {
        let mut b = glyf::Builder::tapped(Transform::default(), RectF::new(), builder, tap);
        let glyph_data = glyf_table.get(glyph_id)?;
        let _ = outline_var_impl(
            glyf_table,
            self,
            glyph_id,
            glyph_data,
            coordinates,
            0,
            &mut b,
            work,
        );
        if work.exhausted() {
            return None;
        }
        b.bbox.to_rect()
    }

    /// The glyph's phantom points: each kept tuple stepped through every
    /// outline point to reach them, a walk its parse charged to the same
    /// variation budget an outline has, so a glyph past it has none.
    pub(crate) fn phantom_points(
        &self,
        glyf_table: glyf::Table,
        coordinates: &[NormalizedCoordinate],
        glyph_id: GlyphId,
    ) -> Option<PhantomPoints> {
        let outline_points = glyf_table.outline_points(glyph_id);
        let mut tuples = VariationTuples::default();
        let mut work = crate::work::OutlineWork::default();
        self.parse_variation_data(
            glyph_id,
            coordinates,
            outline_points,
            &mut tuples,
            &mut work,
        )?;

        for _ in 0..outline_points {
            tuples.apply_null()?;
        }

        Some(PhantomPoints {
            left: tuples.apply_null()?,
            right: tuples.apply_null()?,
            top: tuples.apply_null()?,
            bottom: tuples.apply_null()?,
        })
    }
}

impl core::fmt::Debug for Table<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Table {{ ... }}")
    }
}

#[allow(clippy::comparison_chain)]
fn outline_var_impl(
    glyf_table: glyf::Table,
    gvar_table: &Table,
    glyph_id: GlyphId,
    data: &[u8],
    coordinates: &[NormalizedCoordinate],
    depth: u8,
    builder: &mut glyf::Builder,
    work: &mut crate::work::OutlineWork,
) -> Option<()> {
    work.take()?;
    if depth >= glyf::MAX_COMPONENTS {
        return None;
    }

    let mut s = Stream::new(data);
    let number_of_contours = s.read::<i16>()?;

    s.advance(8);

    let mut tuples = VariationTuples::default();

    if number_of_contours > 0 {
        let number_of_contours = NonZeroU16::new(number_of_contours as u16)?;
        let glyph_points = glyf::parse_simple_outline(s.tail()?, number_of_contours)?;
        let points_len = glyph_points.points_left;
        gvar_table.parse_variation_data(glyph_id, coordinates, points_len, &mut tuples, work)?;

        if tuples.len() == 0 {
            for point in glyph_points {
                work.take()?;
                builder.push_point(
                    f32::from(point.x),
                    f32::from(point.y),
                    point.on_curve_point,
                    point.last_point,
                );
            }
            return Some(());
        }

        let mut points = Vec::with_capacity(usize::from(points_len));
        for point in glyph_points {
            work.take()?;
            points.push(point);
        }
        for (point, p) in points.iter().zip(tuples.varied(&points)) {
            builder.push_point(p.x, p.y, point.on_curve_point, point.last_point);
        }

        Some(())
    } else if number_of_contours < 0 {
        let components = glyf::CompositeGlyphIter::new(s.tail()?);
        let components_count = components.clone().count() as u16;
        gvar_table.parse_variation_data(
            glyph_id,
            coordinates,
            components_count,
            &mut tuples,
            work,
        )?;

        for component in components {
            let t = tuples.apply_null()?;

            let mut transform = builder.transform;

            if component.flags.args_are_xy_values() {
                transform = Transform::combine(transform, Transform::new_translate(t.x, t.y));
            }

            transform = Transform::combine(transform, component.transform);

            let mut b = glyf::Builder::tapped(
                transform,
                builder.bbox,
                builder.builder,
                glyf::reborrow(&mut builder.tap),
            );
            if let Some(glyph_data) = glyf_table.get(component.glyph_id) {
                outline_var_impl(
                    glyf_table,
                    gvar_table,
                    component.glyph_id,
                    glyph_data,
                    coordinates,
                    depth + 1,
                    &mut b,
                    work,
                )?;

                builder.bbox = b.bbox;
            }
        }

        Some(())
    } else {
        // An empty glyph.
        None
    }
}

fn parse_variation_data<'a>(
    coordinates: &[NormalizedCoordinate],
    shared_tuple_records: &LazyArray16<F2DOT14>,
    points_len: u16,
    data: &'a [u8],
    tuples: &mut VariationTuples<'a>,
    work: &mut crate::work::OutlineWork,
) -> Option<()> {
    const SHARED_POINT_NUMBERS_FLAG: u16 = 0x8000;
    const COUNT_MASK: u16 = 0x0FFF;

    let mut main_stream = Stream::new(data);
    let tuple_variation_count = main_stream.read::<u16>()?;
    let data_offset = main_stream.read::<Offset16>()?;

    let has_shared_point_numbers = tuple_variation_count & SHARED_POINT_NUMBERS_FLAG != 0;
    let tuple_variation_count = tuple_variation_count & COUNT_MASK;

    if tuple_variation_count == 0 {
        return None;
    }

    tuples.reserve(tuple_variation_count);

    let mut serialized_stream = Stream::new_at(data, data_offset.to_usize())?;

    let mut shared_point_numbers = None;
    if has_shared_point_numbers {
        shared_point_numbers = PackedPointsIter::new(&mut serialized_stream)?;
    }

    parse_variation_tuples(
        tuple_variation_count,
        coordinates,
        shared_tuple_records,
        shared_point_numbers,
        points_len.checked_add(PHANTOM_POINTS_LEN as u16)?,
        main_stream,
        serialized_stream,
        tuples,
        work,
    )
}
