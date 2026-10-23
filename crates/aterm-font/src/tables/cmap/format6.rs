// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cmap/format6.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use core::convert::TryFrom;

use crate::GlyphId;
use crate::parser::{LazyArray16, Stream};

/// A [format
/// 6](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-6-trimmed-table-mapping)
/// subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable6<'a> {
    /// First character code of subrange.
    pub first_code_point: u16,
    /// Array of glyph indexes for character codes in the range.
    pub glyphs: LazyArray16<'a, GlyphId>,
}

impl<'a> Subtable6<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>(); // format
        s.skip::<u16>(); // length
        s.skip::<u16>(); // language
        let first_code_point = s.read::<u16>()?;
        let count = s.read::<u16>()?;
        let glyphs = s.read_array16::<GlyphId>(count)?;
        Some(Self {
            first_code_point,
            glyphs,
        })
    }

    /// Returns a glyph index for a code point.
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        let code_point = u16::try_from(code_point).ok()?;
        let idx = code_point.checked_sub(self.first_code_point)?;
        self.glyphs.get(idx)
    }

    /// Calls `f` for each codepoint defined in this table.
    pub fn codepoints(&self, mut f: impl FnMut(u32)) {
        for i in 0..self.glyphs.len() {
            if let Some(code_point) = self.first_code_point.checked_add(i) {
                f(u32::from(code_point));
            }
        }
    }
}
