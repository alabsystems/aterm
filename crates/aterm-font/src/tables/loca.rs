// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/loca.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! An [Index to Location Table](https://docs.microsoft.com/en-us/typography/opentype/spec/loca).

use core::convert::TryFrom;
use core::num::NonZeroU16;
use core::ops::Range;

use crate::parser::{LazyArray16, NumFrom, Stream};
use crate::{GlyphId, IndexToLocationFormat};

/// An [Index to Location Table](https://docs.microsoft.com/en-us/typography/opentype/spec/loca).
#[derive(Clone, Copy, Debug)]
pub enum Table<'a> {
    /// Short offsets.
    Short(LazyArray16<'a, u16>),
    /// Long offsets.
    Long(LazyArray16<'a, u32>),
}

impl<'a> Table<'a> {
    /// Parses a table from raw data.
    pub fn parse(
        number_of_glyphs: NonZeroU16,
        format: IndexToLocationFormat,
        data: &'a [u8],
    ) -> Option<Self> {
        let mut total = if number_of_glyphs.get() == u16::MAX {
            number_of_glyphs.get()
        } else {
            number_of_glyphs.get() + 1
        };

        let actual_total = match format {
            IndexToLocationFormat::Short => data.len() / 2,
            IndexToLocationFormat::Long => data.len() / 4,
        };
        let actual_total = u16::try_from(actual_total).ok()?;
        total = total.min(actual_total);

        let mut s = Stream::new(data);
        match format {
            IndexToLocationFormat::Short => Some(Table::Short(s.read_array16::<u16>(total)?)),
            IndexToLocationFormat::Long => Some(Table::Long(s.read_array16::<u32>(total)?)),
        }
    }

    /// Returns the number of offsets.
    #[inline]
    pub const fn len(&self) -> u16 {
        match self {
            Table::Short(array) => array.len(),
            Table::Long(array) => array.len(),
        }
    }

    /// Checks if there are any offsets.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns glyph's range in the `glyf` table.
    #[inline]
    pub fn glyph_range(&self, glyph_id: GlyphId) -> Option<Range<usize>> {
        let glyph_id = glyph_id.0;
        if glyph_id == u16::MAX {
            return None;
        }

        if glyph_id + 1 >= self.len() {
            return None;
        }

        let range = match self {
            Table::Short(array) => {
                usize::from(array.get(glyph_id)?) * 2..usize::from(array.get(glyph_id + 1)?) * 2
            }
            Table::Long(array) => {
                usize::num_from(array.get(glyph_id)?)..usize::num_from(array.get(glyph_id + 1)?)
            }
        };

        if range.start >= range.end {
            None
        } else {
            Some(range)
        }
    }
}
