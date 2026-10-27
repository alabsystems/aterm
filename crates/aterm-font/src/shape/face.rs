// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/face.rs and src/hb/ot_layout_common.rs
// and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Font face wrapper for shaping with cached `cmap`, `GSUB`, and `GPOS` lookups.

use std::cell::OnceCell;

use super::buffer::glyph_props;
use super::digest::Digest;
use crate::gpos::PositioningSubtable;
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

/// Lookup property flags in the low 16 bits and mark filtering set in the high 16 bits.
pub(crate) mod lookup_flags {
    pub(crate) const RIGHT_TO_LEFT: u16 = 0x0001;
    pub(crate) const IGNORE_MARKS: u16 = 0x0008;
    pub(crate) const IGNORE_FLAGS: u16 = 0x000E;
    pub(crate) const USE_MARK_FILTERING_SET: u16 = 0x0010;
    pub(crate) const MARK_ATTACHMENT_TYPE_MASK: u16 = 0xFF00;
}

/// Parsed `GSUB` lookup with subtables, coverage digest, direction, and properties.
pub(crate) struct SubstLookup<'a> {
    pub(crate) subtables: Vec<SubstitutionSubtable<'a>>,
    pub(crate) digest: Digest,
    pub(crate) reverse: bool,
    pub(crate) props: u32,
}

/// Glyph bounding metrics in font design units, following HarfBuzz's `hb_glyph_extents_t` sign convention (`height` is negative for non-empty glyphs).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct GlyphExtents {
    pub(crate) x_bearing: i32,
    pub(crate) y_bearing: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// Parsed `GPOS` lookup with subtables, coverage digest, and properties.
pub(crate) struct PosLookup<'a> {
    pub(crate) subtables: Vec<PositioningSubtable<'a>>,
    pub(crate) digest: Digest,
    pub(crate) props: u32,
}

/// A font face prepared for shaping, with `GSUB` and `GPOS` lookups parsed lazily on first use.
pub struct ShapeFace<'a> {
    pub(crate) face: Face<'a>,
    cmap_subtable: Option<u16>,
    /// Every `GSUB` lookup (`None` for one that does not parse), or `None`
    /// for the whole table once the parse runs past its budget
    /// ([`LayoutWork`]): `GSUB` is then dropped for the face.
    lookups: OnceCell<Option<Vec<Option<SubstLookup<'a>>>>>,
    /// Every `GPOS` lookup (`None` for one that does not parse), or `None`
    /// for the whole table once the parse runs past its budget
    /// ([`LayoutWork`]): `GPOS` is then dropped for the face.
    pos_lookups: OnceCell<Option<Vec<Option<PosLookup<'a>>>>>,
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
            pos_lookups: OnceCell::new(),
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

    #[inline]
    pub(crate) const fn gpos(&self) -> Option<&LayoutTable<'a>> {
        self.face.tables().gpos.as_ref()
    }

    /// Returns parsed `GSUB` lookup `index`, or `None` if unparseable or refused.
    #[inline]
    pub(crate) fn gsub_lookup(&self, index: LookupIndex) -> Option<&SubstLookup<'a>> {
        self.gsub_lookups()?.get(usize::from(index))?.as_ref()
    }

    /// Returns parsed `GPOS` lookup `index`, or `None` if unparseable or refused.
    #[inline]
    pub(crate) fn gpos_lookup(&self, index: LookupIndex) -> Option<&PosLookup<'a>> {
        self.gpos_lookups()?.get(usize::from(index))?.as_ref()
    }

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

    fn gpos_lookups(&self) -> Option<&[Option<PosLookup<'a>>]> {
        self.pos_lookups
            .get_or_init(|| {
                let gpos = self.gpos()?;
                let len = self
                    .face
                    .raw_face()
                    .table(crate::Tag::from_bytes(b"GPOS"))
                    .map_or(0, <[u8]>::len);
                let work = LayoutWork::for_table(len, 1);
                let mut lookups = Vec::with_capacity(usize::from(gpos.lookups.len()));
                for index in 0..gpos.lookups.len() {
                    lookups.push(match gpos.lookups.get(index) {
                        Some(lookup) => Some(PosLookup::parse(lookup, &work)?),
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

    pub(crate) fn glyph_extents(&self, glyph: GlyphId) -> Option<GlyphExtents> {
        if let Some(img) = self.face.glyph_raster_image(glyph, u16::MAX) {
            if img.format == crate::RasterImageFormat::PNG && img.pixels_per_em != 0 {
                let scale = f32::from(self.face.units_per_em()) / f32::from(img.pixels_per_em);
                return Some(GlyphExtents {
                    x_bearing: (f32::from(img.x) * scale).round() as i32,
                    y_bearing: ((f32::from(img.y) + f32::from(img.height)) * scale).round() as i32,
                    width: (f32::from(img.width) * scale).round() as i32,
                    height: (-f32::from(img.height) * scale).round() as i32,
                });
            }
        } else if let Some(ref colr) = self.face.tables().colr {
            if colr.is_simple() {
                return None;
            }
            if let Some(clip_box) = colr.clip_box(glyph, self.face.variation_coordinates()) {
                return Some(GlyphExtents {
                    x_bearing: clip_box.x_min.round() as i32,
                    y_bearing: clip_box.y_max.round() as i32,
                    width: (clip_box.x_max - clip_box.x_min).round() as i32,
                    height: (clip_box.y_min - clip_box.y_max).round() as i32,
                });
            }
            let mut painter = PaintExtents::new(&self.face);
            self.face.paint_color_glyph(
                glyph,
                0,
                crate::RgbaColor::new(0, 0, 0, 0),
                &mut painter,
            )?;
            return Some(painter.extents());
        }

        if let Some(ref glyf) = self.face.tables().glyf {
            return Some(match glyf.bbox(glyph) {
                Some(bbox) => GlyphExtents {
                    x_bearing: i32::from(bbox.x_min),
                    y_bearing: i32::from(bbox.y_max),
                    width: i32::from(bbox.x_max) - i32::from(bbox.x_min),
                    height: i32::from(bbox.y_min) - i32::from(bbox.y_max),
                },
                None => GlyphExtents::default(),
            });
        }

        None
    }
}

#[derive(Clone, Copy)]
struct FloatExtents {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

impl FloatExtents {
    #[inline]
    fn is_empty(self) -> bool {
        self.x_min >= self.x_max || self.y_min >= self.y_max
    }

    #[inline]
    fn is_void(self) -> bool {
        self.x_min > self.x_max
    }

    #[inline]
    fn union(&mut self, other: Self) {
        self.x_min = self.x_min.min(other.x_min);
        self.y_min = self.y_min.min(other.y_min);
        self.x_max = self.x_max.max(other.x_max);
        self.y_max = self.y_max.max(other.y_max);
    }

    #[inline]
    fn intersect(&mut self, other: Self) {
        self.x_min = self.x_min.max(other.x_min);
        self.y_min = self.y_min.max(other.y_min);
        self.x_max = self.x_max.min(other.x_max);
        self.y_max = self.y_max.min(other.y_max);
    }

    fn transform(&mut self, ts: crate::Transform) {
        let mut qx = [self.x_min, self.x_min, self.x_max, self.x_max];
        let mut qy = [self.y_min, self.y_max, self.y_min, self.y_max];
        for (x, y) in qx.iter_mut().zip(&mut qy) {
            let (ox, oy) = (*x, *y);
            *x = ts.a * ox + ts.c * oy + ts.e;
            *y = ts.b * ox + ts.d * oy + ts.f;
        }
        self.x_min = qx[0];
        self.x_max = qx[0];
        self.y_min = qy[0];
        self.y_max = qy[0];
        for i in 1..4 {
            self.x_min = self.x_min.min(qx[i]);
            self.y_min = self.y_min.min(qy[i]);
            self.x_max = self.x_max.max(qx[i]);
            self.y_max = self.y_max.max(qy[i]);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundsStatus {
    Empty,
    Bounded,
    Unbounded,
}

#[derive(Clone, Copy)]
struct PaintBounds {
    status: BoundsStatus,
    extents: FloatExtents,
}

impl Default for PaintBounds {
    fn default() -> Self {
        Self::from_extents(FloatExtents {
            x_min: 0.0,
            y_min: 0.0,
            x_max: 0.0,
            y_max: 0.0,
        })
    }
}

impl PaintBounds {
    fn from_extents(extents: FloatExtents) -> Self {
        let status = if extents.is_empty() {
            BoundsStatus::Empty
        } else {
            BoundsStatus::Bounded
        };
        Self { status, extents }
    }

    fn from_status(status: BoundsStatus) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    fn union(&mut self, other: Self) {
        if other.status == BoundsStatus::Unbounded {
            self.status = BoundsStatus::Unbounded;
        } else if other.status == BoundsStatus::Bounded {
            if self.status == BoundsStatus::Empty {
                *self = other;
            } else if self.status == BoundsStatus::Bounded {
                self.extents.union(other.extents);
            }
        }
    }

    fn intersect(&mut self, other: Self) {
        if other.status == BoundsStatus::Empty {
            self.status = BoundsStatus::Empty;
        } else if other.status == BoundsStatus::Bounded {
            if self.status == BoundsStatus::Unbounded {
                *self = other;
            } else if self.status == BoundsStatus::Bounded {
                self.extents.intersect(other.extents);
                if self.extents.is_empty() {
                    self.status = BoundsStatus::Empty;
                }
            }
        }
    }
}

struct PaintExtents<'a> {
    clips: Vec<PaintBounds>,
    groups: Vec<PaintBounds>,
    transforms: Vec<crate::Transform>,
    composite_modes: Vec<crate::colr::CompositeMode>,
    face: &'a Face<'a>,
    current_glyph: GlyphId,
}

impl<'a> PaintExtents<'a> {
    fn new(face: &'a Face<'a>) -> Self {
        Self {
            clips: vec![PaintBounds::from_status(BoundsStatus::Unbounded)],
            groups: vec![PaintBounds::from_status(BoundsStatus::Empty)],
            transforms: vec![crate::Transform::default()],
            composite_modes: vec![crate::colr::CompositeMode::SourceOver],
            face,
            current_glyph: GlyphId(0),
        }
    }

    fn extents(&self) -> GlyphExtents {
        let e = self.groups.last().copied().unwrap_or_default().extents;
        if e.is_void() {
            GlyphExtents::default()
        } else {
            GlyphExtents {
                x_bearing: e.x_min as i32,
                y_bearing: e.y_max as i32,
                width: (e.x_max - e.x_min) as i32,
                height: (e.y_min - e.y_max) as i32,
            }
        }
    }

    fn push_clip_extents(&mut self, mut extents: FloatExtents) {
        if let Some(&ts) = self.transforms.last() {
            extents.transform(ts);
        }
        self.clips.push(PaintBounds::from_extents(extents));
    }
}

impl crate::colr::Painter<'_> for PaintExtents<'_> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        self.current_glyph = glyph_id;
    }

    fn paint(&mut self, _: crate::colr::Paint<'_>) {
        if let (Some(&clip), Some(group)) = (self.clips.last(), self.groups.last_mut()) {
            group.union(clip);
        }
    }

    fn push_clip(&mut self) {
        if let Some(bbox) = self.face.glyph_bounding_box(self.current_glyph) {
            self.push_clip_extents(FloatExtents {
                x_min: f32::from(bbox.x_min),
                y_min: f32::from(bbox.y_min),
                x_max: f32::from(bbox.x_max),
                y_max: f32::from(bbox.y_max),
            });
        }
    }

    fn push_clip_box(&mut self, clipbox: crate::colr::ClipBox) {
        self.push_clip_extents(FloatExtents {
            x_min: clipbox.x_min,
            y_min: clipbox.y_min,
            x_max: clipbox.x_max,
            y_max: clipbox.y_max,
        });
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, mode: crate::colr::CompositeMode) {
        self.composite_modes.push(mode);
        self.groups.push(PaintBounds::default());
    }

    fn pop_layer(&mut self) {
        use crate::colr::CompositeMode;
        if let (Some(mode), Some(src)) = (self.composite_modes.pop(), self.groups.pop())
            && let Some(backdrop) = self.groups.last_mut()
        {
            match mode {
                CompositeMode::Clear => backdrop.status = BoundsStatus::Empty,
                CompositeMode::Source | CompositeMode::SourceOut => *backdrop = src,
                CompositeMode::Destination | CompositeMode::DestinationOut => {}
                CompositeMode::SourceIn | CompositeMode::DestinationIn => backdrop.intersect(src),
                _ => backdrop.union(src),
            }
        }
    }

    fn push_transform(&mut self, transform: crate::Transform) {
        let base = self.transforms.last().copied().unwrap_or_default();
        self.transforms
            .push(crate::Transform::combine(base, transform));
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

impl<'a> SubstLookup<'a> {
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

impl<'a> PosLookup<'a> {
    fn parse(lookup: crate::opentype_layout::Lookup<'a>, work: &LayoutWork) -> Option<Self> {
        let mut subtables: Vec<PositioningSubtable<'a>> = Vec::new();
        let mut digest = Digest::new();
        for subtable in lookup.subtables.into_iter::<PositioningSubtable>() {
            let coverage = subtable.coverage();
            work.spend(1 + coverage_len(&coverage))?;
            collect_coverage(&coverage, &mut digest);
            subtables.push(subtable);
        }
        let mut props = u32::from(lookup.flags.0);
        if let Some(set) = lookup.mark_filtering_set {
            props |= u32::from(set) << 16;
        }
        Some(PosLookup {
            subtables,
            digest,
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
    let u = c as u16;
    match UNICODE_TO_MACROMAN.iter().position(|&m| m == u) {
        Some(index) => 0x80 + index as u32,
        None => 0,
    }
}
