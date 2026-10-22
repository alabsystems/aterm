// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/face.rs and src/hb/ot_layout_common.rs
// and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! The face the shaper reads: an [`crate::Face`] (with its variation
//! instance already set), the `cmap` subtable upstream prefers, and the
//! `GSUB` lookups, all parsed the first time a shape reaches one, within one
//! budget for the face ([`LayoutWork`]).

use std::cell::OnceCell;

use super::buffer::glyph_props;
use super::digest::Digest;
use crate::gsub::SubstitutionSubtable;
use crate::opentype_layout::{Coverage, LayoutTable, LookupIndex};
use crate::work::LayoutWork;
use crate::{Face, GlyphId, PlatformId};

const WINDOWS_SYMBOL_ENCODING: u16 = 0;
const WINDOWS_UNICODE_BMP_ENCODING: u16 = 1;
const WINDOWS_UNICODE_FULL_ENCODING: u16 = 10;
const UNICODE_1_0_ENCODING: u16 = 0;
const UNICODE_1_1_ENCODING: u16 = 1;
const UNICODE_ISO_ENCODING: u16 = 2;
const UNICODE_2_0_BMP_ENCODING: u16 = 3;
const UNICODE_2_0_FULL_ENCODING: u16 = 4;
const UNICODE_FULL_ENCODING: u16 = 6;

/// lookup_props: the lookup flags in the low 16 bits and, when the lookup
/// uses one, the mark filtering set in the high 16.
pub(crate) mod lookup_flags {
    pub(crate) const IGNORE_FLAGS: u16 = 0x000E;
    pub(crate) const USE_MARK_FILTERING_SET: u16 = 0x0010;
    pub(crate) const MARK_ATTACHMENT_TYPE_MASK: u16 = 0xFF00;
}

/// One `GSUB` lookup, parsed: its subtables, their coverage digest, whether
/// it runs backwards (every subtable is a reverse chain), and its props.
pub(crate) struct SubstLookup<'a> {
    pub(crate) subtables: Vec<SubstitutionSubtable<'a>>,
    pub(crate) digest: Digest,
    pub(crate) reverse: bool,
    pub(crate) props: u32,
}

/// A face set up for shaping. Building one parses nothing but the `cmap`
/// directory; the `GSUB` lookups are parsed together when a shape first
/// reaches one.
pub struct ShapeFace<'a> {
    pub(crate) face: Face<'a>,
    cmap_subtable: Option<u16>,
    /// Every `GSUB` lookup (`None` for one that does not parse), or `None`
    /// for the whole table once the parse runs past its budget
    /// ([`LayoutWork`]): `GSUB` is then dropped for the face.
    lookups: OnceCell<Option<Vec<Option<SubstLookup<'a>>>>>,
}

impl<'a> ShapeFace<'a> {
    /// Wraps `face`, at whatever variation instance it was given.
    #[inline]
    #[must_use]
    pub fn new(face: Face<'a>) -> Self {
        let cmap_subtable = find_best_cmap_subtable(&face);
        ShapeFace {
            face,
            cmap_subtable,
            lookups: OnceCell::new(),
        }
    }

    /// Parses face `index` of `data` and wraps it at its default instance.
    #[inline]
    #[must_use]
    pub fn from_slice(data: &'a [u8], index: u32) -> Option<Self> {
        Face::parse(data, index).ok().map(Self::new)
    }

    /// The face read.
    #[inline]
    #[must_use]
    pub const fn face(&self) -> &Face<'a> {
        &self.face
    }

    #[inline]
    pub(crate) const fn gsub(&self) -> Option<&LayoutTable<'a>> {
        self.face.tables().gsub.as_ref()
    }

    /// Lookup `index`; `None` when it does not parse, and for every lookup
    /// when the face's `GSUB` is refused.
    #[inline]
    pub(crate) fn gsub_lookup(&self, index: LookupIndex) -> Option<&SubstLookup<'a>> {
        self.gsub_lookups()?.get(usize::from(index))?.as_ref()
    }

    /// Every `GSUB` lookup, parsed on the first call, the lookups no feature
    /// names (reached only through a nested lookup) included. So the budget
    /// refuses a table before any of it applies, and every shape of the face
    /// reads the same lookups: parsing only what a plan names would leave a
    /// nested lookup to run past the budget mid-shape, after the lookups
    /// before it had applied. Each lookup is parsed once: one traversal.
    fn gsub_lookups(&self) -> Option<&[Option<SubstLookup<'a>>]> {
        self.lookups
            .get_or_init(|| {
                let gsub = self.gsub()?;
                let len = self
                    .face
                    .raw_face()
                    .table(crate::Tag::from_bytes(b"GSUB"))
                    .map_or(0, <[u8]>::len);
                let work = LayoutWork::for_table(len, 1);
                let mut lookups = Vec::with_capacity(usize::from(gsub.lookups.len()));
                for index in 0..gsub.lookups.len() {
                    lookups.push(match gsub.lookups.get(index) {
                        Some(lookup) => Some(SubstLookup::parse(lookup, &work)?),
                        None => None,
                    });
                }
                Some(lookups)
            })
            .as_deref()
    }

    #[inline]
    pub(crate) fn has_glyph(&self, c: u32) -> bool {
        self.get_nominal_glyph(c).is_some()
    }

    /// The glyph `c` maps to through the preferred `cmap` subtable.
    #[inline]
    pub(crate) fn get_nominal_glyph(&self, mut c: u32) -> Option<GlyphId> {
        let index = self.cmap_subtable?;
        let subtable = self.face.tables().cmap?.subtables.get(index)?;
        if subtable.platform_id == PlatformId::Macintosh && c > 0x7F {
            c = unicode_to_macroman(c);
        }
        match subtable.glyph_index(c) {
            Some(gid) => Some(gid),
            None => {
                // A Windows symbol font duplicates U+F000..F0FF at
                // U+0000..00FF, as Windows does.
                if subtable.platform_id == PlatformId::Windows
                    && subtable.encoding_id == WINDOWS_SYMBOL_ENCODING
                    && c <= 0x00FF
                {
                    return self.get_nominal_glyph(0xF000 + c);
                }
                None
            }
        }
    }

    #[inline]
    pub(crate) fn glyph_variation_index(&self, c: char, vs: char) -> Option<GlyphId> {
        self.face.glyph_variation_index(c, vs)
    }

    /// The horizontal advance at the face's instance, in font units.
    #[inline]
    pub(crate) fn glyph_h_advance(&self, glyph: GlyphId) -> i32 {
        let face = &self.face;
        if face.is_variable()
            && face.has_non_default_variation_coordinates()
            && face.tables().hvar.is_none()
            && face.tables().vvar.is_none()
            && face.glyph_phantom_points(glyph).is_none()
        {
            return match face.glyph_bounding_box(glyph) {
                Some(bbox) => i32::from(bbox.x_max) + i32::from(bbox.x_min),
                None => 0,
            };
        }
        if face.tables().hmtx.is_some() {
            i32::from(face.glyph_hor_advance(glyph).unwrap_or(0))
        } else {
            i32::from(face.units_per_em())
        }
    }

    /// `GDEF`'s class for `glyph` as glyph props (0 with no `GDEF`).
    #[inline]
    pub(crate) fn glyph_props(&self, glyph: GlyphId) -> u16 {
        use crate::gdef::GlyphClass;
        let Some(table) = self.face.tables().gdef else {
            return 0;
        };
        match table.glyph_class(glyph) {
            Some(GlyphClass::Base) => glyph_props::BASE_GLYPH,
            Some(GlyphClass::Ligature) => glyph_props::LIGATURE,
            Some(GlyphClass::Mark) => {
                let class = table.glyph_mark_attachment_class(glyph);
                (class << 8) | glyph_props::MARK
            }
            _ => 0,
        }
    }

    #[inline]
    pub(crate) fn has_glyph_classes(&self) -> bool {
        self.face
            .tables()
            .gdef
            .is_some_and(|table| table.has_glyph_classes())
    }

    #[inline]
    pub(crate) fn is_mark_glyph(&self, glyph: GlyphId, set_index: u16) -> bool {
        self.face
            .tables()
            .gdef
            .is_some_and(|table| table.is_mark_glyph(glyph, Some(set_index)))
    }
}

impl<'a> SubstLookup<'a> {
    /// The lookup's subtables and their digest, each subtable and coverage
    /// entry spending one unit of `work`; `None` when it runs out.
    fn parse(lookup: crate::opentype_layout::Lookup<'a>, work: &LayoutWork) -> Option<Self> {
        let mut subtables: Vec<SubstitutionSubtable<'a>> = Vec::new();
        let mut digest = Digest::new();
        for subtable in lookup.subtables.into_iter::<SubstitutionSubtable>() {
            let coverage = subtable.coverage();
            work.spend(1 + coverage_len(&coverage))?;
            collect_coverage(&coverage, &mut digest);
            subtables.push(subtable);
        }
        let reverse = !subtables.is_empty() && subtables.iter().all(|s| s.is_reverse());
        let mut props = u32::from(lookup.flags.0);
        if let Some(set) = lookup.mark_filtering_set {
            props |= u32::from(set) << 16;
        }
        Some(SubstLookup {
            subtables,
            digest,
            reverse,
            props,
        })
    }
}

/// How many entries (glyphs or ranges) `coverage` lists.
#[inline]
const fn coverage_len(coverage: &Coverage<'_>) -> u64 {
    match *coverage {
        Coverage::Format1 { glyphs } => glyphs.len() as u64,
        Coverage::Format2 { records } => records.len() as u64,
    }
}

fn collect_coverage(coverage: &Coverage<'_>, digest: &mut Digest) {
    match *coverage {
        Coverage::Format1 { glyphs } => {
            for g in glyphs {
                digest.add(g);
            }
        }
        Coverage::Format2 { records } => {
            for record in records {
                digest.add_range(record.start, record.end);
            }
        }
    }
}

fn find_best_cmap_subtable(face: &Face) -> Option<u16> {
    // A symbol subtable first (harfbuzz#1918), then the 32-bit subtables,
    // the 16-bit ones, and MacRoman last.
    find_cmap_subtable(face, PlatformId::Windows, WINDOWS_SYMBOL_ENCODING)
        .or_else(|| find_cmap_subtable(face, PlatformId::Windows, WINDOWS_UNICODE_FULL_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_FULL_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_2_0_FULL_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Windows, WINDOWS_UNICODE_BMP_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_2_0_BMP_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_ISO_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_1_1_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Unicode, UNICODE_1_0_ENCODING))
        .or_else(|| find_cmap_subtable(face, PlatformId::Macintosh, 0))
}

fn find_cmap_subtable(face: &Face, platform_id: PlatformId, encoding_id: u16) -> Option<u16> {
    for (i, subtable) in face.tables().cmap?.subtables.into_iter().enumerate() {
        if subtable.platform_id == platform_id && subtable.encoding_id == encoding_id {
            return u16::try_from(i).ok();
        }
    }
    None
}

#[rustfmt::skip]
static UNICODE_TO_MACROMAN: [u16; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1,
    0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8,
    0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC,
    0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF,
    0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211,
    0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8,
    0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153,
    0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
    0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1,
    0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

fn unicode_to_macroman(c: u32) -> u32 {
    // Upstream truncates the code point to 16 bits before the search.
    let u = c as u16;
    match UNICODE_TO_MACROMAN.iter().position(|&m| m == u) {
        Some(index) => 0x80 + index as u32,
        None => 0,
    }
}
