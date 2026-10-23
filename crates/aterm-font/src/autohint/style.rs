// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/style.rs and modified by the
// aterm project in 2026: the generated script data included from
// styles_generated.rs; the digit bit (it fed only the fitted advance) and the
// precomputed-metrics style iterator dropped. Its tests need fontations' test
// fonts and are not carried.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Styles, scripts and glyph style mapping.

use super::GlyphId;
use super::metrics::BlueZones;
use super::shape::{ShaperCoverageKind, VisitedLookupSet};
use crate::Tag;

/// Defines the script and style associated with a single glyph.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct GlyphStyle(pub(super) u16);

impl GlyphStyle {
    const STYLE_INDEX_MASK: u16 = 0xFF;
    const UNASSIGNED: u16 = Self::STYLE_INDEX_MASK;
    const NON_BASE: u16 = 0x100;
    const FROM_GSUB_OUTPUT: u16 = 0x8000;

    /// Returns true if this glyph doesn't have an assigned style.
    pub const fn is_unassigned(self) -> bool {
        self.0 & Self::STYLE_INDEX_MASK == Self::UNASSIGNED
    }

    /// Returns true if this glyph is a non-base (usually a mark).
    pub const fn is_non_base(self) -> bool {
        self.0 & Self::NON_BASE != 0
    }

    /// Returns the associated style class for this glyph.
    pub fn style_class(self) -> Option<&'static StyleClass> {
        StyleClass::from_index(self.style_index()?)
    }

    /// Returns the index of the style class for this class.
    pub const fn style_index(self) -> Option<u16> {
        let ix = self.0 & Self::STYLE_INDEX_MASK;
        if ix != Self::UNASSIGNED {
            Some(ix)
        } else {
            None
        }
    }

    fn maybe_assign(&mut self, other: Self) {
        // Note that we keep the extra bits because FreeType allows
        // setting the NON_BASE bit to an already mapped style.
        if other.0 & Self::STYLE_INDEX_MASK <= self.0 & Self::STYLE_INDEX_MASK {
            self.0 = (self.0 & !Self::STYLE_INDEX_MASK) | other.0;
        }
    }

    pub(super) fn set_from_gsub_output(&mut self) {
        self.0 |= Self::FROM_GSUB_OUTPUT
    }

    pub(super) fn clear_from_gsub(&mut self) {
        self.0 &= !Self::FROM_GSUB_OUTPUT;
    }

    /// Assign a style if we've been marked as GSUB output _and_
    /// we don't currently have an assigned style.
    pub(super) fn maybe_assign_gsub_output_style(&mut self, style: &StyleClass) -> bool {
        let style_ix = style.index as u16;
        if self.0 & Self::FROM_GSUB_OUTPUT != 0 && self.is_unassigned() {
            self.clear_from_gsub();
            self.0 = (self.0 & !Self::STYLE_INDEX_MASK) | style_ix;
            true
        } else {
            false
        }
    }
}

impl Default for GlyphStyle {
    fn default() -> Self {
        Self(Self::UNASSIGNED)
    }
}

/// Sentinel for unused styles in [`GlyphStyleMap::metrics_map`].
const UNMAPPED_STYLE: u8 = 0xFF;

/// Maps glyph identifiers to glyph styles.
#[derive(Debug)]
pub(crate) struct GlyphStyleMap {
    /// List of styles, indexed by glyph id.
    styles: Vec<GlyphStyle>,
    /// Maps an actual style class index into a compacted index for the
    /// metrics table.
    metrics_map: [u8; MAX_STYLES],
    /// Number of metrics styles in use.
    metrics_count: u8,
}

impl GlyphStyleMap {
    /// Computes a new glyph style map for the given glyph count and character map.
    pub fn new(glyph_count: u32, shaper: &Shaper) -> Self {
        let lookup_count = shaper.lookup_count() as usize;
        let mut bytes = vec![0u8; lookup_count.div_ceil(8)];
        let map = Self::new_inner(glyph_count, shaper, VisitedLookupSet::new(&mut bytes));
        if !shaper.refused() {
            return map;
        }
        Self::new_inner(
            glyph_count,
            &shaper.without_gsub(),
            VisitedLookupSet::new(&mut []),
        )
    }

    fn new_inner(glyph_count: u32, shaper: &Shaper, mut visited_set: VisitedLookupSet) -> Self {
        let mut map = Self {
            styles: vec![GlyphStyle::default(); glyph_count as usize],
            metrics_map: [UNMAPPED_STYLE; MAX_STYLES],
            metrics_count: 0,
        };
        for style in super::style::STYLE_CLASSES {
            if style.feature.is_some()
                && shaper.compute_coverage(
                    style,
                    ShaperCoverageKind::Script,
                    &mut map.styles,
                    &mut visited_set,
                )
            {
                map.use_style(style.index);
            }
        }
        let mut last_range: Option<(usize, StyleRange)> = None;
        for (ch, gid) in shaper.charmap().mappings() {
            let Some(style) = map.styles.get_mut(gid.to_u32() as usize) else {
                continue;
            };
            if let Some(last) = last_range {
                if last.1.contains(ch) {
                    style.maybe_assign(last.1.style);
                    continue;
                }
            }
            let ix = match STYLE_RANGES.binary_search_by(|x| x.first.cmp(&ch)) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let Some(range) = STYLE_RANGES.get(ix).copied() else {
                continue;
            };
            if range.contains(ch) {
                style.maybe_assign(range.style);
                if let Some(style_ix) = range.style.style_index() {
                    map.use_style(style_ix as usize);
                }
                last_range = Some((ix, range));
            }
        }
        for style in super::style::STYLE_CLASSES {
            if style.feature.is_none()
                && shaper.compute_coverage(
                    style,
                    ShaperCoverageKind::Script,
                    &mut map.styles,
                    &mut visited_set,
                )
            {
                map.use_style(style.index);
            }
        }
        let default_style = &STYLE_CLASSES[StyleClass::LATN];
        if shaper.compute_coverage(
            default_style,
            ShaperCoverageKind::Default,
            &mut map.styles,
            &mut visited_set,
        ) {
            map.use_style(default_style.index);
        }
        let mut need_hani = false;
        for style in map.styles.iter_mut() {
            if style.is_unassigned() {
                style.0 &= !GlyphStyle::STYLE_INDEX_MASK;
                style.0 |= StyleClass::HANI as u16;
                need_hani = true;
            }
        }
        if need_hani {
            map.use_style(StyleClass::HANI);
        }
        map
    }

    pub fn style(&self, glyph_id: GlyphId) -> Option<GlyphStyle> {
        self.styles.get(glyph_id.to_u32() as usize).copied()
    }

    /// Returns a compacted metrics index for the given glyph style.
    pub fn metrics_index(&self, style: GlyphStyle) -> Option<usize> {
        let ix = style.style_index()? as usize;
        let metrics_ix = *self.metrics_map.get(ix)? as usize;
        if metrics_ix != UNMAPPED_STYLE as usize {
            Some(metrics_ix)
        } else {
            None
        }
    }

    /// Returns the required size of the compacted metrics array.
    pub fn metrics_count(&self) -> usize {
        self.metrics_count as usize
    }

    fn use_style(&mut self, style_ix: usize) {
        let mapped = &mut self.metrics_map[style_ix];
        if *mapped == UNMAPPED_STYLE {
            *mapped = self.metrics_count;
            self.metrics_count += 1;
        }
    }
}

impl Default for GlyphStyleMap {
    fn default() -> Self {
        Self {
            styles: Default::default(),
            metrics_map: [UNMAPPED_STYLE; MAX_STYLES],
            metrics_count: 0,
        }
    }
}

/// Determines which algorithms the autohinter will use while generating
/// metrics and processing a glyph outline.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub enum ScriptGroup {
    /// All scripts that are not CJK or Indic.
    #[default]
    Default,
    Cjk,
    Indic,
}

/// Defines the basic properties for each script supported by the autohinter.
#[derive(Clone, Debug)]
pub struct ScriptClass {
    /// The name of the script class.
    #[allow(dead_code)]
    pub name: &'static str,
    /// Group that defines how glyphs belonging to this script are hinted.
    pub group: ScriptGroup,
    /// Unicode tag for the script.
    pub tag: Tag,
    /// True if outline edges are processed top to bottom.
    pub hint_top_to_bottom: bool,
    /// Characters used to define standard width and height of stems.
    pub std_chars: &'static str,
    /// "Blue" characters used to define alignment zones.
    pub blues: &'static [(&'static str, BlueZones)],
}

/// Defines the basic properties for each style supported by the autohinter.
#[derive(Clone, Debug)]
pub struct StyleClass {
    /// The name of the style class.
    #[allow(dead_code)]
    pub name: &'static str,
    /// Index of self in the STYLE_CLASSES array.
    pub index: usize,
    /// Associated Unicode script.
    pub script: &'static ScriptClass,
    /// OpenType feature tag for styles that derive coverage from layout tables.
    #[allow(unused)]
    pub feature: Option<Tag>,
}

impl StyleClass {
    pub(crate) fn from_index(index: u16) -> Option<&'static StyleClass> {
        STYLE_CLASSES.get(index as usize)
    }
}

/// Associates a basic glyph style with a range of codepoints.
#[derive(Copy, Clone, Debug)]
pub(super) struct StyleRange {
    pub first: u32,
    pub last: u32,
    pub style: GlyphStyle,
}

impl StyleRange {
    pub fn contains(&self, ch: u32) -> bool {
        (self.first..=self.last).contains(&ch)
    }
}

const fn base_range(first: u32, last: u32, style_index: u16) -> StyleRange {
    StyleRange {
        first,
        last,
        style: GlyphStyle(style_index),
    }
}

const fn non_base_range(first: u32, last: u32, style_index: u16) -> StyleRange {
    StyleRange {
        first,
        last,
        style: GlyphStyle(style_index | GlyphStyle::NON_BASE),
    }
}

const MAX_STYLES: usize = STYLE_CLASSES.len();

use super::shape::Shaper;

include!("styles_generated.rs");
