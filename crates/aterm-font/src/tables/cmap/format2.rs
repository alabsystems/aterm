// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cmap/format2.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

use core::convert::TryFrom;

use crate::GlyphId;
use crate::parser::{FromData, LazyArray16, Stream};

#[derive(Clone, Copy)]
struct SubHeaderRecord {
    first_code: u16,
    entry_count: u16,
    id_delta: i16,
    id_range_offset: u16,
}

impl FromData for SubHeaderRecord {
    const SIZE: usize = 8;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(SubHeaderRecord {
            first_code: s.read::<u16>()?,
            entry_count: s.read::<u16>()?,
            id_delta: s.read::<i16>()?,
            id_range_offset: s.read::<u16>()?,
        })
    }
}

/// A [format
/// 2](https://docs.microsoft.com/en-us/typography/opentype/spec/cmap#format-2-high-byte-mapping-through-table)
/// subtable.
#[derive(Clone, Copy)]
pub struct Subtable2<'a> {
    sub_header_keys: LazyArray16<'a, u16>,
    sub_headers_offset: usize,
    sub_headers: LazyArray16<'a, SubHeaderRecord>,
    data: &'a [u8],
}

impl<'a> Subtable2<'a> {
    /// Parses a subtable from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        s.skip::<u16>();
        s.skip::<u16>();
        s.skip::<u16>();
        let sub_header_keys = s.read_array16::<u16>(256)?;
        let sub_headers_count = sub_header_keys.into_iter().map(|n| n / 8).max()? + 1;

        let sub_headers_offset = s.offset();
        let sub_headers = s.read_array16::<SubHeaderRecord>(sub_headers_count)?;

        Some(Self {
            sub_header_keys,
            sub_headers_offset,
            sub_headers,
            data,
        })
    }

    /// Returns a glyph index for a code point.
    pub fn glyph_index(&self, code_point: u32) -> Option<GlyphId> {
        let code_point = u16::try_from(code_point).ok()?;
        let high_byte = code_point >> 8;
        let low_byte = code_point & 0x00FF;

        let i = if code_point < 0xff {
            0
        } else {
            self.sub_header_keys.get(high_byte)? / 8
        };

        let sub_header = self.sub_headers.get(i)?;

        let first_code = sub_header.first_code;
        let range_end = first_code.checked_add(sub_header.entry_count)?;
        if low_byte < first_code || low_byte >= range_end {
            return None;
        }

        let index_offset = usize::from(low_byte.checked_sub(first_code)?) * u16::SIZE;

        let offset = self.sub_headers_offset + SubHeaderRecord::SIZE * usize::from(i + 1)
            - u16::SIZE
            + usize::from(sub_header.id_range_offset)
            + index_offset;

        let glyph: u16 = Stream::read_at(self.data, offset)?;
        if glyph == 0 {
            return None;
        }

        u16::try_from((i32::from(glyph) + i32::from(sub_header.id_delta)) % 65536)
            .ok()
            .map(GlyphId)
    }

    /// Calls `f` for each codepoint defined in this table.
    pub fn codepoints(&self, f: impl FnMut(u32)) {
        let _ = self.codepoints_inner(f);
    }

    #[inline]
    fn codepoints_inner(&self, mut f: impl FnMut(u32)) -> Option<()> {
        for first_byte in 0u16..256 {
            let i = self.sub_header_keys.get(first_byte)? / 8;
            let sub_header = self.sub_headers.get(i)?;
            let first_code = sub_header.first_code;

            if i == 0 {
                let range_end = first_code.checked_add(sub_header.entry_count)?;
                if first_byte >= first_code && first_byte < range_end {
                    f(u32::from(first_byte));
                }
            } else {
                let base = first_code.checked_add(first_byte << 8)?;
                for k in 0..sub_header.entry_count {
                    let code_point = base.checked_add(k)?;
                    f(u32::from(code_point));
                }
            }
        }

        Some(())
    }
}

impl core::fmt::Debug for Subtable2<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Subtable2 {{ ... }}")
    }
}
