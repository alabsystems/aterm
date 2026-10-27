// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/ankr.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Apple Advanced Typography `ankr` (Anchor Point) table parser.

use core::num::NonZeroU16;

use super::aat;
use crate::GlyphId;
use crate::parser::{FromData, LazyArray32, Offset, Offset32, Stream};

/// An anchor point.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(crate) struct Point {
    pub(crate) x: i16,
    pub(crate) y: i16,
}

impl FromData for Point {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(Point {
            x: s.read::<i16>()?,
            y: s.read::<i16>()?,
        })
    }
}

/// An AAT Anchor Point Table (`ankr`).
#[derive(Clone)]
pub(crate) struct Table<'a> {
    lookup: aat::Lookup<'a>,
    glyphs_data: &'a [u8],
}

impl core::fmt::Debug for Table<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Table {{ ... }}")
    }
}

impl<'a> Table<'a> {
    /// Parses a table from raw data (`number_of_glyphs` is from `maxp`).
    pub(crate) fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let version = s.read::<u16>()?;
        if version != 0 {
            return None;
        }
        s.skip::<u16>(); // reserved

        let lookup_offset = s.read::<Offset32>()?.to_usize();
        let glyphs_offset = s.read::<Offset32>()?.to_usize();
        let lookup_table = data.get(lookup_offset..)?;
        let glyphs_data = data.get(glyphs_offset..)?;

        Some(Table {
            lookup: aat::Lookup::parse(number_of_glyphs, lookup_table)?,
            glyphs_data,
        })
    }

    /// Returns a list of anchor points for `glyph_id`.
    pub(crate) fn points(&self, glyph_id: GlyphId) -> Option<LazyArray32<'a, Point>> {
        let offset = self.lookup.value(glyph_id)?;
        let mut s = Stream::new_at(self.glyphs_data, usize::from(offset))?;
        let number_of_points = s.read::<u32>()?;
        s.read_array32::<Point>(number_of_points)
    }
}
