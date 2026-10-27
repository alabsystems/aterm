// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/kern.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! OpenType and Apple Advanced Typography `kern` table parser.

use crate::GlyphId;
use crate::parser::{FromData, LazyArray16, NumFrom, Offset, Offset16, Stream};

#[derive(Clone, Copy, Debug)]
struct OTCoverage(u8);

#[rustfmt::skip]
impl OTCoverage {
    #[inline] fn is_horizontal(self) -> bool { self.0 & (1 << 0) != 0 }
    #[inline] fn has_cross_stream(self) -> bool { self.0 & (1 << 2) != 0 }
}

impl FromData for OTCoverage {
    const SIZE: usize = 1;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.get(0).copied().map(OTCoverage)
    }
}

#[derive(Clone, Copy, Debug)]
struct AATCoverage(u8);

#[rustfmt::skip]
impl AATCoverage {
    #[inline] fn is_horizontal(self) -> bool { self.0 & (1 << 7) == 0 }
    #[inline] fn has_cross_stream(self) -> bool { self.0 & (1 << 6) != 0 }
    #[inline] fn is_variable(self) -> bool { self.0 & (1 << 5) != 0 }
}

impl FromData for AATCoverage {
    const SIZE: usize = 1;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.get(0).copied().map(AATCoverage)
    }
}

/// A kerning pair.
#[derive(Clone, Copy, Debug)]
pub struct KerningPair {
    /// Glyphs pair.
    pub pair: u32,
    /// Kerning value.
    pub value: i16,
}

impl KerningPair {
    /// Returns left glyph ID.
    #[inline]
    pub fn left(&self) -> GlyphId {
        GlyphId((self.pair >> 16) as u16)
    }

    /// Returns right glyph ID.
    #[inline]
    pub fn right(&self) -> GlyphId {
        GlyphId(self.pair as u16)
    }
}

impl FromData for KerningPair {
    const SIZE: usize = 6;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(KerningPair {
            pair: s.read::<u32>()?,
            value: s.read::<i16>()?,
        })
    }
}

/// A kerning subtable format.
#[allow(missing_docs)]
#[derive(Clone, Debug)]
pub enum Format<'a> {
    Format0(Subtable0<'a>),
    /// An AAT state machine.
    Format1,
    Format2(Subtable2<'a>),
    Format3(Subtable3<'a>),
}

/// A kerning subtable.
#[derive(Clone, Debug)]
pub struct Subtable<'a> {
    /// Indicates that subtable is for horizontal text.
    pub horizontal: bool,
    /// Indicates that subtable is variable.
    pub variable: bool,
    /// Indicates that subtable has a cross-stream values.
    pub has_cross_stream: bool,
    /// Indicates that subtable uses a state machine.
    pub has_state_machine: bool,
    /// Subtable format.
    pub format: Format<'a>,
}

impl<'a> Subtable<'a> {
    /// Returns kerning for a pair of glyphs.
    #[inline]
    pub fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        match self.format {
            Format::Format0(ref subtable) => subtable.glyphs_kerning(left, right),
            Format::Format2(ref subtable) => subtable.glyphs_kerning(left, right),
            Format::Format3(ref subtable) => subtable.glyphs_kerning(left, right),
            _ => None,
        }
    }
}

/// A list of subtables.
#[derive(Clone, Copy)]
pub struct Subtables<'a> {
    /// Indicates an Apple Advanced Typography format.
    is_aat: bool,
    /// The total number of tables.
    count: u32,
    /// Actual data.
    data: &'a [u8],
}

impl<'a> Subtables<'a> {
    /// Returns the number of subtables.
    pub fn len(&self) -> u32 {
        self.count
    }

    /// Checks if there are any subtables.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    pub(crate) fn shaper_iter(self) -> ShaperSubtablesIter<'a> {
        ShaperSubtablesIter {
            is_aat: self.is_aat,
            table_index: 0,
            number_of_tables: self.count,
            stream: Stream::new(self.data),
        }
    }
}

impl core::fmt::Debug for Subtables<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Subtables {{ ... }}")
    }
}

impl<'a> IntoIterator for Subtables<'a> {
    type Item = Subtable<'a>;
    type IntoIter = SubtablesIter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        SubtablesIter {
            is_aat: self.is_aat,
            table_index: 0,
            number_of_tables: self.count,
            stream: Stream::new(self.data),
        }
    }
}

/// An iterator over kerning subtables.
#[allow(missing_debug_implementations)]
#[derive(Clone, Default)]
pub struct SubtablesIter<'a> {
    /// Indicates an Apple Advanced Typography format.
    is_aat: bool,
    /// The current table index.
    table_index: u32,
    /// The total number of tables.
    number_of_tables: u32,
    /// Actual data.
    stream: Stream<'a>,
}

impl<'a> Iterator for SubtablesIter<'a> {
    type Item = Subtable<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.table_index == self.number_of_tables {
            return None;
        }

        if self.stream.at_end() {
            return None;
        }

        if self.is_aat {
            const HEADER_SIZE: u8 = 8;

            let table_len = self.stream.read::<u32>()?;
            let coverage = self.stream.read::<AATCoverage>()?;
            let format_id = self.stream.read::<u8>()?;
            self.stream.skip::<u16>(); // variation tuple index

            if format_id > 3 {
                return None;
            }

            let data_len = usize::num_from(table_len).checked_sub(usize::from(HEADER_SIZE))?;
            let data = self.stream.read_bytes(data_len)?;

            let format = match format_id {
                0 => Format::Format0(Subtable0::parse(data)?),
                1 => {
                    state_table_parses(data)?;
                    Format::Format1
                }
                2 => Format::Format2(Subtable2::parse(HEADER_SIZE, data)?),
                3 => Format::Format3(Subtable3::parse(data)?),
                _ => return None,
            };

            Some(Subtable {
                horizontal: coverage.is_horizontal(),
                variable: coverage.is_variable(),
                has_cross_stream: coverage.has_cross_stream(),
                has_state_machine: format_id == 1,
                format,
            })
        } else {
            const HEADER_SIZE: u8 = 6;

            self.stream.skip::<u16>(); // version
            let table_len = self.stream.read::<u16>()?;
            let format_id = self.stream.read::<u8>()?;
            let coverage = self.stream.read::<OTCoverage>()?;

            if format_id != 0 && format_id != 2 {
                return None;
            }

            let data_len = if self.number_of_tables == 1 {
                self.stream.tail()?.len()
            } else {
                usize::from(table_len).checked_sub(usize::from(HEADER_SIZE))?
            };

            let data = self.stream.read_bytes(data_len)?;

            let format = match format_id {
                0 => Format::Format0(Subtable0::parse(data)?),
                2 => Format::Format2(Subtable2::parse(HEADER_SIZE, data)?),
                _ => return None,
            };

            Some(Subtable {
                horizontal: coverage.is_horizontal(),
                variable: false, // Only AAT supports it.
                has_cross_stream: coverage.has_cross_stream(),
                has_state_machine: format_id == 1,
                format,
            })
        }
    }
}

pub(crate) struct ShaperSubtablesIter<'a> {
    is_aat: bool,
    table_index: u32,
    number_of_tables: u32,
    stream: Stream<'a>,
}

impl<'a> Iterator for ShaperSubtablesIter<'a> {
    type Item = (Subtable<'a>, Option<super::aat::StateTable<'a>>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.table_index == self.number_of_tables || self.stream.at_end() {
            return None;
        }

        if self.is_aat {
            const HEADER_SIZE: u8 = 8;

            let table_len = self.stream.read::<u32>()?;
            let coverage = self.stream.read::<AATCoverage>()?;
            let format_id = self.stream.read::<u8>()?;
            self.stream.skip::<u16>();

            if format_id > 3 {
                return None;
            }

            let data_len = usize::num_from(table_len).checked_sub(usize::from(HEADER_SIZE))?;
            let data = self.stream.read_bytes(data_len)?;

            let (format, state_table) = match format_id {
                0 => (Format::Format0(Subtable0::parse(data)?), None),
                1 => (Format::Format1, Some(super::aat::StateTable::parse(data)?)),
                2 => (Format::Format2(Subtable2::parse(HEADER_SIZE, data)?), None),
                3 => (Format::Format3(Subtable3::parse(data)?), None),
                _ => return None,
            };

            Some((
                Subtable {
                    horizontal: coverage.is_horizontal(),
                    variable: coverage.is_variable(),
                    has_cross_stream: coverage.has_cross_stream(),
                    has_state_machine: format_id == 1,
                    format,
                },
                state_table,
            ))
        } else {
            const HEADER_SIZE: u8 = 6;

            self.stream.skip::<u16>();
            let table_len = self.stream.read::<u16>()?;
            let format_id = self.stream.read::<u8>()?;
            let coverage = self.stream.read::<OTCoverage>()?;

            if format_id != 0 && format_id != 2 {
                return None;
            }

            let data_len = if self.number_of_tables == 1 {
                self.stream.tail()?.len()
            } else {
                usize::from(table_len).checked_sub(usize::from(HEADER_SIZE))?
            };

            let data = self.stream.read_bytes(data_len)?;

            let format = match format_id {
                0 => Format::Format0(Subtable0::parse(data)?),
                2 => Format::Format2(Subtable2::parse(HEADER_SIZE, data)?),
                _ => return None,
            };

            Some((
                Subtable {
                    horizontal: coverage.is_horizontal(),
                    variable: false,
                    has_cross_stream: coverage.has_cross_stream(),
                    has_state_machine: false,
                    format,
                },
                None,
            ))
        }
    }
}

/// Whether upstream's `aat::StateTable::parse` accepts `data`.
fn state_table_parses(data: &[u8]) -> Option<()> {
    let mut s = Stream::new(data);
    s.read::<u16>()?; // number_of_classes
    let class_table_offset = s.read::<Offset16>()?.to_usize();
    let state_array_offset = s.read::<Offset16>()?.to_usize();
    let entry_table_offset = s.read::<Offset16>()?.to_usize();

    let mut s = Stream::new_at(data, class_table_offset)?;
    s.read::<GlyphId>()?; // first_glyph
    let number_of_glyphs = s.read::<u16>()?;
    s.read_bytes(usize::from(number_of_glyphs))?;

    data.get(state_array_offset..)?;
    data.get(entry_table_offset..)?;
    Some(())
}

/// A format 0 subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable0<'a> {
    /// A list of kerning pairs.
    pub pairs: LazyArray16<'a, KerningPair>,
}

impl<'a> Subtable0<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let number_of_pairs = s.read::<u16>()?;
        s.advance(6); // search_range (u16) + entry_selector (u16) + range_shift (u16)
        let pairs = s.read_array16::<KerningPair>(number_of_pairs)?;
        Some(Self { pairs })
    }

    /// Returns kerning for a pair of glyphs.
    #[inline]
    pub fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let needle = u32::from(left.0) << 16 | u32::from(right.0);
        self.pairs
            .binary_search_by(|v| v.pair.cmp(&needle))
            .map(|(_, v)| v.value)
    }
}

/// A format 2 subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable2<'a> {
    data: &'a [u8],
    header_len: u8,
}

impl<'a> Subtable2<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(header_len: u8, data: &'a [u8]) -> Option<Self> {
        Some(Self { header_len, data })
    }

    /// Returns kerning for a pair of glyphs.
    pub fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let mut s = Stream::new(self.data);
        s.skip::<u16>(); // row_width

        let header_len = usize::from(self.header_len);
        let left_hand_table_offset = s.read::<Offset16>()?.to_usize().checked_sub(header_len)?;
        let right_hand_table_offset = s.read::<Offset16>()?.to_usize().checked_sub(header_len)?;
        let array_offset = s.read::<Offset16>()?.to_usize().checked_sub(header_len)?;

        let left_class = get_format2_class(left.0, left_hand_table_offset, self.data).unwrap_or(0);
        let right_class =
            get_format2_class(right.0, right_hand_table_offset, self.data).unwrap_or(0);

        if usize::from(left_class) < array_offset {
            return None;
        }

        let index = usize::from(left_class) + usize::from(right_class);
        let value_offset = index.checked_sub(header_len)?;
        Stream::read_at::<i16>(self.data, value_offset)
    }
}

pub(crate) fn get_format2_class(glyph_id: u16, offset: usize, data: &[u8]) -> Option<u16> {
    let mut s = Stream::new_at(data, offset)?;
    let first_glyph = s.read::<u16>()?;
    let index = glyph_id.checked_sub(first_glyph)?;

    let number_of_classes = s.read::<u16>()?;
    let classes = s.read_array16::<u16>(number_of_classes)?;
    classes.get(index)
}

/// A format 3 subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable3<'a> {
    data: &'a [u8],
}

impl<'a> Subtable3<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        Some(Self { data })
    }

    /// Returns kerning for a pair of glyphs.
    #[inline]
    pub fn glyphs_kerning(&self, left: GlyphId, right: GlyphId) -> Option<i16> {
        let mut s = Stream::new(self.data);
        let glyph_count = s.read::<u16>()?;
        let kerning_values_count = s.read::<u8>()?;
        let left_hand_classes_count = s.read::<u8>()?;
        let right_hand_classes_count = s.read::<u8>()?;
        s.skip::<u8>(); // reserved
        let indices_count =
            u16::from(left_hand_classes_count) * u16::from(right_hand_classes_count);

        let kerning_values = s.read_array16::<i16>(u16::from(kerning_values_count))?;
        let left_hand_classes = s.read_array16::<u8>(glyph_count)?;
        let right_hand_classes = s.read_array16::<u8>(glyph_count)?;
        let indices = s.read_array16::<u8>(indices_count)?;

        let left_class = left_hand_classes.get(left.0)?;
        let right_class = right_hand_classes.get(right.0)?;

        if left_class > left_hand_classes_count || right_class > right_hand_classes_count {
            return None;
        }

        let index =
            u16::from(left_class) * u16::from(right_hand_classes_count) + u16::from(right_class);
        let index = indices.get(index)?;
        kerning_values.get(u16::from(index))
    }
}

/// A [Kerning Table](https://docs.microsoft.com/en-us/typography/opentype/spec/kern).
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    /// A list of subtables.
    pub subtables: Subtables<'a>,
}

impl<'a> Table<'a> {
    /// Parses a table from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let version = s.read::<u16>()?;
        let subtables = if version == 0 {
            let count = s.read::<u16>()?;
            Subtables {
                is_aat: false,
                count: u32::from(count),
                data: s.tail()?,
            }
        } else {
            s.skip::<u16>(); // Skip the second part of u32 version.
            let count = s.read::<u32>()?;
            Subtables {
                is_aat: true,
                count,
                data: s.tail()?,
            }
        };

        Some(Self { subtables })
    }
}
