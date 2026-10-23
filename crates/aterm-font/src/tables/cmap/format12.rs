// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cmap/format12.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use core::convert::TryFrom;

use crate::GlyphId;
use crate::parser::{FromData, LazyArray32, Stream};

#[derive(Clone, Copy)]
pub struct SequentialMapGroup {
    pub start_char_code: u32,
    pub end_char_code: u32,
    pub start_glyph_id: u32,
}

impl FromData for SequentialMapGroup {
    const SIZE: usize = 12;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(SequentialMapGroup {
            start_char_code: s.read::<u32>()?,
            end_char_code: s.read::<u32>()?,
            start_glyph_id: s.read::<u32>()?,
        })
    }
}

/// A [format 12](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-12-segmented-coverage)
/// subtable.
#[derive(Clone, Copy)]
pub struct Subtable12<'a> {
    groups: LazyArray32<'a, SequentialMapGroup>,
}

impl<'a> Subtable12<'a> {
    /// Parses a subtable from raw data.
    #[inline]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>(); // format
        s.skip::<u16>(); // reserved
        s.skip::<u32>(); // length
        s.skip::<u32>(); // language
        let count = s.read::<u32>()?;
        let groups = s.read_array32::<SequentialMapGroup>(count)?;
        Some(Self { groups })
    }

    /// Returns a glyph index for a code point.
    #[inline]
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        let (_, group) = self.groups.binary_search_by(|range| {
            use core::cmp::Ordering;

            if range.start_char_code > code_point {
                Ordering::Greater
            } else if range.end_char_code < code_point {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })?;

        let id = group
            .start_glyph_id
            .checked_add(code_point)?
            .checked_sub(group.start_char_code)?;
        u16::try_from(id).ok().map(GlyphId)
    }

    /// Calls `f` for each code point a group maps, capped at U+10FFFF and [`super::CODE_POINTS`].
    pub fn codepoints(&self, mut f: impl FnMut(u32)) {
        let mut left = super::CODE_POINTS;
        for group in self.groups {
            for code_point in group.start_char_code..=group.end_char_code.min(super::MAX_CODE_POINT)
            {
                if left == 0 {
                    return;
                }
                left -= 1;
                f(code_point);
            }
        }
    }
}

impl core::fmt::Debug for Subtable12<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Subtable12 {{ ... }}")
    }
}
