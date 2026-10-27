// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/kerx.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Apple Advanced Typography `kerx` (Extended Kerning) table parser.

use core::num::NonZeroU16;

use super::aat;
use crate::GlyphId;
use crate::kern::KerningPair;
use crate::parser::{FromData, LazyArray32, NumFrom, Offset, Offset32, Stream};

const HEADER_SIZE: usize = 12;

/// A format 0 subtable (ordered list of kerning pairs).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Subtable0<'a> {
    pub(crate) pairs: LazyArray32<'a, KerningPair>,
}

impl<'a> Subtable0<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let number_of_pairs = s.read::<u32>()?;
        s.advance(12);
        let pairs = s.read_array32::<KerningPair>(number_of_pairs)?;
        Some(Self { pairs })
    }

    #[inline]
    pub(crate) fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let needle = u32::from(left.0) << 16 | u32::from(right.0);
        self.pairs
            .binary_search_by(|v| v.pair.cmp(&needle))
            .map(|(_, v)| v.value)
    }
}

/// A state machine entry payload for `kerx` format 1 and format 4.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EntryData {
    pub(crate) action_index: u16,
}

impl FromData for EntryData {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(EntryData {
            action_index: s.read::<u16>()?,
        })
    }
}

/// A format 1 subtable (state table for contextual kerning).
#[derive(Clone)]
pub(crate) struct Subtable1<'a> {
    pub(crate) state_table: aat::ExtendedStateTable<'a, EntryData>,
    actions_data: &'a [u8],
}

impl<'a> Subtable1<'a> {
    fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let state_table = aat::ExtendedStateTable::parse(number_of_glyphs, &mut s)?;
        let actions_offset = s.read::<Offset32>()?;
        let actions_data = data.get(actions_offset.to_usize()..)?;

        Some(Subtable1 {
            state_table,
            actions_data,
        })
    }

    #[inline]
    pub(crate) fn glyphs_kerning(&self, action_index: u16) -> Option<i16> {
        Stream::read_at(
            self.actions_data,
            usize::from(action_index).checked_mul(i16::SIZE)?,
        )
    }
}

impl core::fmt::Debug for Subtable1<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Subtable1 {{ ... }}")
    }
}

/// A format 2 subtable (simple `n x m` array of kerning values).
#[derive(Clone, Copy)]
pub(crate) struct Subtable2<'a>(&'a [u8]);

impl<'a> Subtable2<'a> {
    pub(crate) fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let mut s = Stream::new(self.0);
        s.skip::<u32>(); // row_width

        let left_hand_table_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;
        let right_hand_table_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;
        let array_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;

        let left_class =
            crate::kern::get_format2_class(left.0, left_hand_table_offset, self.0).unwrap_or(0);
        let right_class =
            crate::kern::get_format2_class(right.0, right_hand_table_offset, self.0).unwrap_or(0);

        if usize::from(left_class) < array_offset {
            return None;
        }

        let index = usize::from(left_class).checked_add(usize::from(right_class))?;
        let value_offset = index.checked_sub(HEADER_SIZE)?;
        Stream::read_at::<i16>(self.0, value_offset)
    }
}

impl core::fmt::Debug for Subtable2<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Subtable2 {{ ... }}")
    }
}

/// Anchor point pairs used by [`Subtable4`].
#[derive(Clone, Copy)]
pub(crate) struct AnchorPoints<'a>(&'a [u8]);

impl AnchorPoints<'_> {
    /// Returns `(mark_anchor_point, current_anchor_point)` at `action_index`.
    pub(crate) fn get(&self, action_index: u16) -> Option<(u16, u16)> {
        let offset = usize::from(action_index).checked_mul(u16::SIZE * 2)?;
        let mut s = Stream::new_at(self.0, offset)?;
        Some((s.read::<u16>()?, s.read::<u16>()?))
    }
}

impl core::fmt::Debug for AnchorPoints<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AnchorPoints {{ ... }}")
    }
}

/// A format 4 subtable (state table for anchor point positioning).
#[derive(Clone)]
pub(crate) struct Subtable4<'a> {
    pub(crate) state_table: aat::ExtendedStateTable<'a, EntryData>,
    pub(crate) anchor_points: AnchorPoints<'a>,
}

impl<'a> Subtable4<'a> {
    fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let state_table = aat::ExtendedStateTable::parse(number_of_glyphs, &mut s)?;
        let flags = s.read::<u32>()?;
        let action_type = ((flags & 0xC000_0000) >> 30) as u8;
        let points_offset = usize::num_from(flags & 0x00FF_FFFF);

        if action_type != 1 {
            return None;
        }

        Some(Self {
            state_table,
            anchor_points: AnchorPoints(data.get(points_offset..)?),
        })
    }
}

impl core::fmt::Debug for Subtable4<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Subtable4 {{ ... }}")
    }
}

/// A format 6 subtable (index-based `n x m` array of kerning values).
#[derive(Clone, Copy)]
pub(crate) struct Subtable6<'a> {
    data: &'a [u8],
    number_of_glyphs: NonZeroU16,
}

impl<'a> Subtable6<'a> {
    fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Self {
        Subtable6 {
            number_of_glyphs,
            data,
        }
    }

    pub(crate) fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let mut s = Stream::new(self.data);
        let flags = s.read::<u32>()?;
        s.skip::<u16>(); // row_count
        s.skip::<u16>(); // col_count
        let row_index_table_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;
        let column_index_table_offset =
            s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;
        let kerning_array_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;
        let kerning_vector_offset = s.read::<Offset32>()?.to_usize().checked_sub(HEADER_SIZE)?;

        let row_index_table_data = self.data.get(row_index_table_offset..)?;
        let column_index_table_data = self.data.get(column_index_table_offset..)?;
        let kerning_array_data = self.data.get(kerning_array_offset..)?;
        let kerning_vector_data = self.data.get(kerning_vector_offset..)?;

        let l = aat::Lookup::parse(self.number_of_glyphs, row_index_table_data)?
            .value(left)
            .unwrap_or(0);
        let r = aat::Lookup::parse(self.number_of_glyphs, column_index_table_data)?
            .value(right)
            .unwrap_or(0);
        let pair_index = usize::from(l).checked_add(usize::from(r))?;

        if flags & 0x0000_0001 != 0 {
            let array_offset = pair_index.checked_mul(i32::SIZE)?;
            let vector_offset: u32 = Stream::read_at(kerning_array_data, array_offset)?;
            Stream::read_at(kerning_vector_data, usize::num_from(vector_offset))
        } else {
            let array_offset = pair_index.checked_mul(i16::SIZE)?;
            let vector_offset: u16 = Stream::read_at(kerning_array_data, array_offset)?;
            Stream::read_at(kerning_vector_data, usize::from(vector_offset))
        }
    }
}

impl core::fmt::Debug for Subtable6<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Subtable6 {{ ... }}")
    }
}

/// An extended kerning subtable format.
#[derive(Clone, Debug)]
pub(crate) enum Format<'a> {
    Format0(Subtable0<'a>),
    Format1(Subtable1<'a>),
    Format2(Subtable2<'a>),
    Format4(Subtable4<'a>),
    Format6(Subtable6<'a>),
}

/// An extended kerning subtable.
#[derive(Clone, Debug)]
pub(crate) struct Subtable<'a> {
    pub(crate) horizontal: bool,
    pub(crate) variable: bool,
    pub(crate) has_cross_stream: bool,
    pub(crate) has_state_machine: bool,
    pub(crate) tuple_count: u32,
    pub(crate) format: Format<'a>,
}

impl<'a> Subtable<'a> {
    #[inline]
    pub(crate) fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        match self.format {
            Format::Format0(ref subtable) => subtable.glyphs_kerning(left, right),
            Format::Format1(_) => None,
            Format::Format2(ref subtable) => subtable.glyphs_kerning(left, right),
            Format::Format4(_) => None,
            Format::Format6(ref subtable) => subtable.glyphs_kerning(left, right),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Coverage(u8);

impl Coverage {
    #[inline]
    const fn is_horizontal(self) -> bool {
        self.0 & (1 << 7) == 0
    }

    #[inline]
    const fn has_cross_stream(self) -> bool {
        self.0 & (1 << 6) != 0
    }

    #[inline]
    const fn is_variable(self) -> bool {
        self.0 & (1 << 5) != 0
    }
}

/// A list of extended kerning subtables.
#[derive(Clone, Copy)]
pub(crate) struct Subtables<'a> {
    number_of_glyphs: NonZeroU16,
    number_of_tables: u32,
    data: &'a [u8],
}

impl core::fmt::Debug for Subtables<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Subtables {{ ... }}")
    }
}

impl<'a> IntoIterator for Subtables<'a> {
    type Item = Subtable<'a>;
    type IntoIter = SubtablesIter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        SubtablesIter {
            number_of_glyphs: self.number_of_glyphs,
            table_index: 0,
            number_of_tables: self.number_of_tables,
            stream: Stream::new(self.data),
        }
    }
}

/// An iterator over extended kerning subtables.
#[derive(Clone)]
pub(crate) struct SubtablesIter<'a> {
    number_of_glyphs: NonZeroU16,
    table_index: u32,
    number_of_tables: u32,
    stream: Stream<'a>,
}

impl<'a> Iterator for SubtablesIter<'a> {
    type Item = Subtable<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.table_index == self.number_of_tables || self.stream.at_end() {
            return None;
        }

        let s = &mut self.stream;
        let table_len = s.read::<u32>()?;
        let coverage = Coverage(s.read::<u8>()?);
        s.skip::<u16>(); // unused
        let raw_format = s.read::<u8>()?;
        let tuple_count = s.read::<u32>()?;

        let data_len = usize::num_from(table_len).checked_sub(HEADER_SIZE)?;
        let data = s.read_bytes(data_len)?;

        let format = match raw_format {
            0 => Subtable0::parse(data).map(Format::Format0)?,
            1 => Subtable1::parse(self.number_of_glyphs, data).map(Format::Format1)?,
            2 => Format::Format2(Subtable2(data)),
            4 => Subtable4::parse(self.number_of_glyphs, data).map(Format::Format4)?,
            6 => Format::Format6(Subtable6::parse(self.number_of_glyphs, data)),
            _ => return None,
        };

        self.table_index += 1;

        Some(Subtable {
            horizontal: coverage.is_horizontal(),
            variable: coverage.is_variable(),
            has_cross_stream: coverage.has_cross_stream(),
            has_state_machine: raw_format == 1 || raw_format == 4,
            tuple_count,
            format,
        })
    }
}

/// An AAT Extended Kerning Table (`kerx`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Table<'a> {
    pub(crate) subtables: Subtables<'a>,
}

impl<'a> Table<'a> {
    /// Parses a table from raw data (`number_of_glyphs` is from `maxp`).
    pub(crate) fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>(); // version
        s.skip::<u16>(); // padding
        let number_of_tables = s.read::<u32>()?;
        let subtables = Subtables {
            number_of_glyphs,
            number_of_tables,
            data: s.tail()?,
        };
        Some(Table { subtables })
    }
}
