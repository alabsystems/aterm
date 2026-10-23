// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/charset.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use super::StringId;
use crate::GlyphId;
use crate::parser::{FromData, LazyArray16, Stream};
use core::num::NonZeroU16;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Format1Range {
    first: StringId,
    left: u8,
}

impl FromData for Format1Range {
    const SIZE: usize = 3;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(Format1Range {
            first: s.read::<StringId>()?,
            left: s.read::<u8>()?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Format2Range {
    first: StringId,
    left: u16,
}

impl FromData for Format2Range {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(Format2Range {
            first: s.read::<StringId>()?,
            left: s.read::<u16>()?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Charset<'a> {
    ISOAdobe,
    Expert,
    ExpertSubset,
    Format0(LazyArray16<'a, StringId>),
    Format1(LazyArray16<'a, Format1Range>),
    Format2(LazyArray16<'a, Format2Range>),
}

impl Charset<'_> {
    pub fn sid_to_gid(&self, sid: StringId) -> Option<GlyphId> {
        if sid.0 == 0 {
            return Some(GlyphId(0));
        }

        match self {
            Charset::ISOAdobe | Charset::Expert | Charset::ExpertSubset => None,
            Charset::Format0(array) => {
                // First glyph is omitted, so we have to add 1.
                array
                    .into_iter()
                    .position(|n| n == sid)
                    .map(|n| GlyphId(n as u16 + 1))
            }
            Charset::Format1(array) => {
                let mut glyph_id = GlyphId(1);
                for range in *array {
                    let last = u32::from(range.first.0) + u32::from(range.left);
                    if range.first <= sid && u32::from(sid.0) <= last {
                        glyph_id.0 += sid.0 - range.first.0;
                        return Some(glyph_id);
                    }

                    glyph_id.0 += u16::from(range.left) + 1;
                }

                None
            }
            Charset::Format2(array) => {
                let mut glyph_id = GlyphId(1);
                for range in *array {
                    let last = u32::from(range.first.0) + u32::from(range.left);
                    if sid >= range.first && u32::from(sid.0) <= last {
                        glyph_id.0 += sid.0 - range.first.0;
                        return Some(glyph_id);
                    }

                    glyph_id.0 += range.left + 1;
                }

                None
            }
        }
    }
}

pub(crate) fn parse_charset<'a>(
    number_of_glyphs: NonZeroU16,
    s: &mut Stream<'a>,
) -> Option<Charset<'a>> {
    let format = s.read::<u8>()?;
    match format {
        0 => Some(Charset::Format0(
            s.read_array16::<StringId>(number_of_glyphs.get() - 1)?,
        )),
        1 => {
            let mut count = 0;
            {
                let mut s = s.clone();
                let mut total_left = number_of_glyphs.get() - 1;
                while total_left > 0 {
                    s.skip::<StringId>(); // first
                    let left = s.read::<u8>()?;
                    total_left = total_left.checked_sub(u16::from(left) + 1)?;
                    count += 1;
                }
            }

            s.read_array16::<Format1Range>(count).map(Charset::Format1)
        }
        2 => {
            let mut count = 0;
            {
                let mut s = s.clone();
                let mut total_left = number_of_glyphs.get() - 1;
                while total_left > 0 {
                    s.skip::<StringId>(); // first
                    let left = s.read::<u16>()?.checked_add(1)?;
                    total_left = total_left.checked_sub(left)?;
                    count += 1;
                }
            }

            s.read_array16::<Format2Range>(count).map(Charset::Format2)
        }
        _ => None,
    }
}
