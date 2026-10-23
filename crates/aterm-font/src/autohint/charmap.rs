// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/charmap.rs and read-fonts 0.43.2
// src/tables/cmap.rs and generated/generated_cmap.rs, and modified by the
// aterm project in 2026: the one `cmap` subtable skrifa picks (symbol, else
// the widest Unicode one, formats 4, 12 and 13), its lookup and its
// enumeration, every read checked; variation sequences dropped.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Autohinter character map: single-subtable lookup and enumeration (formats 4, 12, and 13).

use super::GlyphId;
use core::ops::Range;

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let b = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let b = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A run of `count` big-endian u16 (or i16) values, empty when it runs
/// past the data.
fn u16_array(data: &[u8], start: usize, count: usize) -> &[u8] {
    data.get(start..)
        .and_then(|d| d.get(..count * 2))
        .unwrap_or_default()
}

/// A format 4 subtable: segment arrays over the BMP.
#[derive(Clone, Copy)]
struct Cmap4<'a> {
    end_code: &'a [u8],
    start_code: &'a [u8],
    id_delta: &'a [u8],
    id_range_offsets: &'a [u8],
    glyph_id_array: &'a [u8],
}

impl<'a> Cmap4<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let seg_count = usize::from(read_u16(data, 6)?) / 2;
        let end_start = 14;
        let start_start = end_start + seg_count * 2 + 2;
        let delta_start = start_start + seg_count * 2;
        let offsets_start = delta_start + seg_count * 2;
        let glyphs_start = offsets_start + seg_count * 2;
        let glyph_id_array = data
            .get(glyphs_start..)
            .map(|rest| &rest[..rest.len() / 2 * 2])
            .unwrap_or_default();
        Some(Self {
            end_code: u16_array(data, end_start, seg_count),
            start_code: u16_array(data, start_start, seg_count),
            id_delta: u16_array(data, delta_start, seg_count),
            id_range_offsets: u16_array(data, offsets_start, seg_count),
            glyph_id_array,
        })
    }

    /// The glyph for `codepoint`, binary searching the header's
    /// `seg_count` segments (each array read is checked).
    fn map_codepoint(&self, codepoint: u32, seg_count: usize) -> Option<u32> {
        if codepoint > 0xFFFF {
            return None;
        }
        let codepoint = codepoint as u16;
        let mut lo = 0;
        let mut hi = seg_count;
        while lo < hi {
            let i = (lo + hi) / 2;
            let start_code = read_u16(self.start_code, i * 2)?;
            if codepoint < start_code {
                hi = i;
            } else if codepoint > read_u16(self.end_code, i * 2)? {
                lo = i + 1;
            } else {
                return self.lookup_glyph_id(codepoint, i, start_code);
            }
        }
        None
    }

    /// The final phase of a lookup, shared with the iterator.
    fn lookup_glyph_id(&self, codepoint: u16, index: usize, start_code: u16) -> Option<u32> {
        let delta = i32::from(read_u16(self.id_delta, index * 2)? as i16);
        let range_offset = usize::from(read_u16(self.id_range_offsets, index * 2)?);
        if range_offset == 0 {
            return Some(u32::from((i32::from(codepoint) + delta) as u16));
        }
        let mut offset = range_offset / 2 + usize::from(codepoint.wrapping_sub(start_code));
        offset = offset.saturating_sub(self.id_range_offsets.len() / 2 - index);
        let gid = read_u16(self.glyph_id_array, offset.checked_mul(2)?)?;
        (gid != 0).then_some(u32::from((i32::from(gid) + delta) as u16))
    }

    /// `[start, end]` of segment `index`, as an exclusive range.
    fn code_range(&self, index: usize) -> Option<Range<u32>> {
        let start = u32::from(read_u16(self.start_code, index * 2)?);
        let end = u32::from(read_u16(self.end_code, index * 2)?);
        Some(start..end + 1)
    }
}

/// A format 12 or 13 subtable: 32-bit groups.
#[derive(Clone, Copy)]
struct Cmap1213<'a> {
    groups: &'a [u8],
    /// Format 13: every character of a group maps to its one glyph.
    constant: bool,
}

impl<'a> Cmap1213<'a> {
    fn new(data: &'a [u8], constant: bool) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let count = read_u32(data, 12)? as usize;
        let groups = data
            .get(16..)
            .and_then(|d| d.get(..count.checked_mul(12)?))
            .unwrap_or_default();
        Some(Self { groups, constant })
    }

    fn len(&self) -> usize {
        self.groups.len() / 12
    }

    /// `(start char, end char, start or constant glyph)` of group `i`.
    fn group(&self, i: usize) -> Option<(u32, u32, u32)> {
        let at = i.checked_mul(12)?;
        Some((
            read_u32(self.groups, at)?,
            read_u32(self.groups, at + 4)?,
            read_u32(self.groups, at + 8)?,
        ))
    }

    fn compute_glyph_id(&self, codepoint: u32, start_char_code: u32, ref_glyph_id: u32) -> u32 {
        if self.constant {
            ref_glyph_id
        } else {
            ref_glyph_id.wrapping_add(codepoint.wrapping_sub(start_char_code))
        }
    }

    fn map_codepoint(&self, codepoint: u32) -> Option<u32> {
        let mut lo = 0;
        let mut hi = self.len();
        while lo < hi {
            let i = (lo + hi) / 2;
            let (start, end, glyph) = self.group(i)?;
            if codepoint < start {
                hi = i;
            } else if codepoint > end {
                lo = i + 1;
            } else {
                return Some(self.compute_glyph_id(codepoint, start, glyph));
            }
        }
        None
    }

    /// Group `index` as an iteration range, cut to the valid characters and
    /// (for format 12) to the glyphs the font has.
    fn iter_group(&self, index: usize, glyph_count: u32) -> Option<IterGroup> {
        let (start_code, end, start_glyph_id) = self.group(index)?;
        let max_char = char::MAX as u64;
        let end_code = u64::from(end) + 1;
        let end_code = if self.constant {
            end_code.min(max_char)
        } else {
            u64::from(glyph_count)
                .saturating_sub(u64::from(start_glyph_id))
                .saturating_add(u64::from(start_code))
                .min(end_code.min(max_char))
        };
        Some(IterGroup {
            range: u64::from(start_code)..end_code,
            start_code,
            ref_glyph_id: start_glyph_id,
        })
    }
}

#[derive(Clone, Debug)]
struct IterGroup {
    range: Range<u64>,
    start_code: u32,
    ref_glyph_id: u32,
}

#[derive(Clone, Copy)]
enum Subtable<'a> {
    Format4(Cmap4<'a>, usize),
    Format1213(Cmap1213<'a>),
}

impl<'a> Subtable<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        match read_u16(data, 0)? {
            4 => {
                let cmap4 = Cmap4::new(data)?;
                let seg_count = usize::from(read_u16(data, 6)?) / 2;
                Some(Self::Format4(cmap4, seg_count))
            }
            12 => Cmap1213::new(data, false).map(Self::Format1213),
            13 => Cmap1213::new(data, true).map(Self::Format1213),
            _ => None,
        }
    }

    fn map_impl(&self, codepoint: u32) -> Option<u32> {
        let gid = match self {
            Self::Format4(cmap4, seg_count) => cmap4.map_codepoint(codepoint, *seg_count),
            Self::Format1213(cmap) => cmap.map_codepoint(codepoint),
        }?;
        (gid != 0).then_some(gid)
    }
}

/// The character map: one subtable, and whether it is a symbol one.
#[derive(Clone, Copy, Default)]
pub(crate) struct Charmap<'a> {
    subtable: Option<Subtable<'a>>,
    is_symbol: bool,
    glyph_count: u32,
}

impl<'a> Charmap<'a> {
    /// Creates a character map from `cmap` with `glyph_count` from `maxp` (or `u16::MAX`).
    pub(crate) fn new(cmap: Option<&'a [u8]>, glyph_count: u32) -> Self {
        let Some(cmap) = cmap.filter(|c| c.len() >= 4) else {
            return Self::default();
        };
        const PLATFORM_UNICODE: u16 = 0;
        const PLATFORM_ISO: u16 = 2;
        const PLATFORM_WINDOWS: u16 = 3;
        const ENCODING_MS_SYMBOL: u16 = 0;
        const ENCODING_MS_UNICODE_CS: u16 = 1;
        const ENCODING_APPLE_ID_UNICODE_32: u16 = 4;
        const ENCODING_APPLE_ID_VARIANT_SELECTOR: u16 = 5;
        const ENCODING_MS_ID_UCS_4: u16 = 10;
        let mut kind = 0u8;
        let mut chosen = Self {
            glyph_count,
            ..Self::default()
        };
        let num_tables = usize::from(read_u16(cmap, 2).unwrap_or_default());
        let records = cmap
            .get(4..)
            .and_then(|d| d.get(..num_tables * 8))
            .unwrap_or_default();
        for record in records.as_chunks::<8>().0.iter().rev() {
            let platform = read_u16(record, 0).unwrap_or_default();
            let encoding = read_u16(record, 2).unwrap_or_default();
            let record_kind = match (platform, encoding) {
                (PLATFORM_UNICODE, ENCODING_APPLE_ID_VARIANT_SELECTOR) => continue,
                (PLATFORM_WINDOWS, ENCODING_MS_SYMBOL) => 3,
                (PLATFORM_WINDOWS, ENCODING_MS_ID_UCS_4)
                | (PLATFORM_UNICODE, ENCODING_APPLE_ID_UNICODE_32) => 2,
                (PLATFORM_ISO, _)
                | (PLATFORM_UNICODE, _)
                | (PLATFORM_WINDOWS, ENCODING_MS_UNICODE_CS) => 1,
                _ => continue,
            };
            let offset = read_u32(record, 4).unwrap_or_default() as usize;
            if offset == 0 {
                continue;
            }
            let Some(subtable) = cmap.get(offset..).and_then(Subtable::new) else {
                continue;
            };
            if record_kind > kind {
                kind = record_kind;
                chosen.subtable = Some(subtable);
                chosen.is_symbol = record_kind == 3;
            }
        }
        chosen
    }

    /// True if the chosen subtable is a symbol one.
    pub(crate) fn is_symbol(&self) -> bool {
        self.subtable.is_some() && self.is_symbol
    }

    /// Maps `ch` to a non-`.notdef` glyph, falling back to `U+F000..=U+F0FF` for symbol subtables.
    pub(crate) fn map(&self, ch: impl Into<u32>) -> Option<GlyphId> {
        let subtable = self.subtable.as_ref()?;
        let codepoint = ch.into();
        subtable
            .map_impl(codepoint)
            .or_else(|| {
                if self.is_symbol && codepoint <= 0x00FF {
                    subtable.map_impl(codepoint + 0xF000)
                } else {
                    None
                }
            })
            .map(GlyphId::new)
    }

    /// Iterates over all non-`.notdef` `(codepoint, glyph)` pairs in subtable order.
    pub(crate) fn mappings(&self) -> Mappings<'a> {
        let inner = match self.subtable {
            Some(Subtable::Format4(cmap4, _)) => {
                let cur_range = cmap4.code_range(0).unwrap_or_default();
                MappingsInner::Format4 {
                    subtable: cmap4,
                    cur_start_code: cur_range.start as u16,
                    cur_range,
                    cur_range_ix: 0,
                }
            }
            Some(Subtable::Format1213(cmap)) => MappingsInner::Format1213 {
                subtable: cmap,
                cur_group: cmap.iter_group(0, self.glyph_count),
                cur_group_ix: 0,
                glyph_count: self.glyph_count,
            },
            None => MappingsInner::None,
        };
        Mappings(inner)
    }
}

/// The (character, glyph) pairs of a [`Charmap`].
pub(crate) struct Mappings<'a>(MappingsInner<'a>);

enum MappingsInner<'a> {
    None,
    Format4 {
        subtable: Cmap4<'a>,
        cur_range: Range<u32>,
        cur_start_code: u16,
        cur_range_ix: usize,
    },
    Format1213 {
        subtable: Cmap1213<'a>,
        cur_group: Option<IterGroup>,
        cur_group_ix: usize,
        glyph_count: u32,
    },
}

impl Iterator for Mappings<'_> {
    type Item = (u32, GlyphId);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let item = match &mut self.0 {
                MappingsInner::None => None,
                MappingsInner::Format4 {
                    subtable,
                    cur_range,
                    cur_start_code,
                    cur_range_ix,
                } => loop {
                    if let Some(codepoint) = cur_range.next() {
                        let Some(glyph_id) = subtable.lookup_glyph_id(
                            codepoint as u16,
                            *cur_range_ix,
                            *cur_start_code,
                        ) else {
                            continue;
                        };
                        break Some((codepoint, glyph_id));
                    }
                    *cur_range_ix += 1;
                    let Some(next_range) = subtable.code_range(*cur_range_ix) else {
                        break None;
                    };
                    // Clamp overlapping/out-of-order segments while keeping `start_code` for lookup (fontations#1100).
                    let start_code = next_range.start as u16;
                    *cur_range =
                        next_range.start.max(cur_range.end)..next_range.end.max(cur_range.end);
                    *cur_start_code = start_code;
                },
                MappingsInner::Format1213 {
                    subtable,
                    cur_group,
                    cur_group_ix,
                    glyph_count,
                } => loop {
                    let Some(group) = cur_group.as_mut() else {
                        break None;
                    };
                    if let Some(codepoint) = group.range.next() {
                        let codepoint = codepoint as u32;
                        let glyph_id = subtable.compute_glyph_id(
                            codepoint,
                            group.start_code,
                            group.ref_glyph_id,
                        );
                        break Some((codepoint, glyph_id));
                    }
                    *cur_group_ix += 1;
                    let Some(mut next_group) = subtable.iter_group(*cur_group_ix, *glyph_count)
                    else {
                        break None;
                    };
                    if next_group.range.start < group.range.end {
                        next_group.range.start = group.range.end;
                    }
                    next_group.range.end = next_group.range.end.max(group.range.end);
                    *cur_group = Some(next_group);
                },
            }?;
            if item.1 != 0 {
                return Some((item.0, GlyphId::new(item.1)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmap_with(records: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&(records.len() as u16).to_be_bytes());
        let mut offset = 4 + records.len() * 8;
        let mut bodies = Vec::new();
        for (platform, encoding, body) in records {
            t.extend_from_slice(&platform.to_be_bytes());
            t.extend_from_slice(&encoding.to_be_bytes());
            t.extend_from_slice(&(offset as u32).to_be_bytes());
            offset += body.len();
            bodies.extend_from_slice(body);
        }
        t.extend_from_slice(&bodies);
        t
    }

    /// A format 12 subtable of `(start, end, glyph)` groups.
    fn format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&12u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&((16 + groups.len() * 12) as u32).to_be_bytes());
        t.extend_from_slice(&0u32.to_be_bytes());
        t.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (s, e, g) in groups {
            for v in [s, e, g] {
                t.extend_from_slice(&v.to_be_bytes());
            }
        }
        t
    }

    /// A format 4 subtable of `(start, end, delta)` segments plus the
    /// 0xFFFF terminator.
    fn format4(segments: &[(u16, u16, i16)]) -> Vec<u8> {
        let mut segs: Vec<(u16, u16, i16)> = segments.to_vec();
        segs.push((0xFFFF, 0xFFFF, 1));
        let n = segs.len() as u16;
        let mut t = Vec::new();
        for v in [4u16, 0, 0, n * 2, 0, 0, 0] {
            t.extend_from_slice(&v.to_be_bytes());
        }
        for s in &segs {
            t.extend_from_slice(&s.1.to_be_bytes());
        }
        t.extend_from_slice(&0u16.to_be_bytes());
        for s in &segs {
            t.extend_from_slice(&s.0.to_be_bytes());
        }
        for s in &segs {
            t.extend_from_slice(&s.2.to_be_bytes());
        }
        for _ in &segs {
            t.extend_from_slice(&0u16.to_be_bytes());
        }
        let len = t.len() as u16;
        t[2..4].copy_from_slice(&len.to_be_bytes());
        t
    }

    /// The UCS-4 subtable wins over the BMP one, whatever their order, and
    /// only it answers.
    #[test]
    fn picks_the_full_unicode_subtable() {
        let bmp = format4(&[(0x41, 0x42, 1)]);
        let full = format12(&[(0x41, 0x41, 7), (0x1F600, 0x1F601, 9)]);
        let cmap = cmap_with(&[(3, 1, bmp.clone()), (3, 10, full.clone())]);
        let map = Charmap::new(Some(&cmap), 100);
        assert_eq!(map.map('A'), Some(GlyphId::new(7)));
        assert_eq!(map.map('B'), None);
        assert_eq!(map.map(0x1F601u32), Some(GlyphId::new(10)));
        let cmap = cmap_with(&[(3, 10, full), (3, 1, bmp)]);
        assert_eq!(
            Charmap::new(Some(&cmap), 100).map('A'),
            Some(GlyphId::new(7))
        );
    }

    /// A symbol subtable wins, and answers Latin-1 from the F0xx block.
    #[test]
    fn symbol_subtable_maps_the_f000_block() {
        let sym = format4(&[(0xF041, 0xF041, 0x0FC4)]);
        let bmp = format4(&[(0x41, 0x41, 1)]);
        let cmap = cmap_with(&[(3, 1, bmp), (3, 0, sym)]);
        let map = Charmap::new(Some(&cmap), 100);
        assert!(map.is_symbol());
        assert_eq!(map.map('A'), Some(GlyphId::new(5)));
    }

    /// Enumeration skips `.notdef`, cuts format 12 groups to the glyph
    /// count, and never steps a backwards group back.
    #[test]
    fn mappings_follow_read_fonts() {
        let full = format12(&[(0x20, 0x22, 0), (0x30, 0x39, 95), (0x31, 0x36, 1)]);
        let cmap = cmap_with(&[(3, 10, full)]);
        let pairs: Vec<_> = Charmap::new(Some(&cmap), 100)
            .mappings()
            .map(|(c, g)| (c, g.to_u32()))
            .collect();
        assert_eq!(
            pairs,
            [
                (0x21, 1),
                (0x22, 2),
                (0x30, 95),
                (0x31, 96),
                (0x32, 97),
                (0x33, 98),
                (0x34, 99),
                (0x35, 5),
                (0x36, 6)
            ]
        );
        let bmp = format4(&[(0x41, 0x43, 1)]);
        let cmap = cmap_with(&[(0, 3, bmp)]);
        let pairs: Vec<_> = Charmap::new(Some(&cmap), 100)
            .mappings()
            .map(|(c, g)| (c, g.to_u32()))
            .collect();
        assert_eq!(pairs[..3], [(0x41, 0x42), (0x42, 0x43), (0x43, 0x44)]);
    }
}
