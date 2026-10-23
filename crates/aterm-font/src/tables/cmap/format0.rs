// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cmap/format0.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use crate::GlyphId;
use crate::parser::{NumFrom, Stream};

/// A format 0 `cmap` subtable.
#[derive(Clone, Copy, Debug)]
pub struct Subtable0<'a> {
    /// Just a list of 256 8bit glyph IDs.
    pub glyph_ids: &'a [u8],
}

impl<'a> Subtable0<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>();
        s.skip::<u16>();
        s.skip::<u16>();
        let glyph_ids = s.read_bytes(256)?;
        Some(Self { glyph_ids })
    }

    /// Returns a glyph index for a code point.
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        let glyph_id = *self.glyph_ids.get(usize::num_from(code_point))?;
        if glyph_id != 0 {
            Some(GlyphId(u16::from(glyph_id)))
        } else {
            None
        }
    }

    /// Calls `f` for each codepoint defined in this table.
    pub fn codepoints(&self, mut f: impl FnMut(u32)) {
        for (i, glyph_id) in self.glyph_ids.iter().enumerate() {
            if *glyph_id != 0 {
                f(i as u32);
            }
        }
    }
}
