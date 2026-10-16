// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/mod.rs, src/outline/hint.rs
// (`HintingInstance`, `Target`, `SmoothMode`) and src/outline/error.rs
// (`DrawError`), and modified by the aterm project in 2026: the automatic
// hinter alone (no TrueType interpreter, no CFF hinter, no hint plans), over
// aterm-font's reader, for the three smooth targets aterm renders.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! The automatic hinter: FreeType's autofit, as skrifa 0.46.2 ports it,
//! carved for aterm's Linux and Windows grid fitting (TF.4 of
//! docs/AUDIT-third-party-2026-10-15.md).
//!
//! [`HintingInstance::new`] analyses a face once per size and variation
//! instance: which writing system each glyph belongs to (its `cmap` range,
//! or the `GSUB` lookups that reach it), and lazily, per writing system, the
//! standard stem widths and the alignment ("blue") zones measured from that
//! script's reference characters. [`HintingInstance::draw`] then loads one
//! glyph's outline in font units, finds its segments and edges, snaps them
//! to the pixel grid against those zones and widths, interpolates the other
//! points, and hands the fitted outline to an [`OutlineSink`] in pixels, y
//! up.
//!
//! What is carved, and from where (each file names its upstream file):
//!
//! - the hinter itself: skrifa's `outline/autohint/` (`instance.rs`,
//!   `outline.rs`, `style.rs` and its generated script data, `shape.rs`,
//!   `metrics/`, `topo/`, `hint/`), less the hint-plan recorder and the
//!   ahead-of-time (ttfautohint) quirks;
//! - FreeType's path decomposition (`path.rs`);
//! - the outline the hinter starts from, as FreeType loads it unscaled
//!   (`glyf.rs`, `deltas.rs`, `tuple.rs`): integer font units, `gvar` deltas
//!   in 16.16 rounded to a unit, composite transforms in fixed point;
//! - the location in 16.16 (`location.rs`) and the one `cmap` subtable
//!   FreeType picks (`charmap.rs`).
//!
//! The rest comes from aterm-font's own reader: the `GSUB` lookups the
//! style coverage walks, and CFF and CFF2 outlines, which are taken from
//! the charstring interpreter here and truncated to font units as FreeType
//! does (`font.rs`).
//!
//! The answers are skrifa's: the TF.1 hinting corpus
//! (`crates/aterm-render/tests/fixtures/font-oracle/hinting.txt`, skrifa
//! 0.46.2's grid-fitted outlines and aterm's masks of them) replays bit for
//! bit (`tests/hint_oracle.rs`). Where it can answer differently:
//!
//! - a CFF or CFF2 point whose 16.16 value and aterm-font's `f32` value
//!   truncate to different font units, and a CFF `FontMatrix` (skrifa
//!   applies one; aterm-font's interpreter does not);
//! - `avar` version 2's item variation deltas (version 1's segment maps
//!   apply) and `VARC` glyphs, which skrifa reads and aterm-font does not;
//! - a malformed `GSUB` subtable aterm-font's reader rejects as a whole
//!   where read-fonts reads part of it.

mod charmap;
mod deltas;
mod fixed;
mod font;
mod glyf;
mod hint;
mod instance;
mod location;
mod metrics;
mod outline;
mod path;
mod shape;
mod style;
#[cfg(test)]
mod tests;
mod topo;
mod tuple;
mod unscaled;

use crate::OutlineSink;

/// A glyph identifier, 32 bits wide as a `cmap` format 12 subtable can name
/// one (skrifa's `GlyphId`).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub(crate) struct GlyphId(u32);

impl GlyphId {
    /// A glyph identifier.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The identifier as a `u32`.
    pub const fn to_u32(self) -> u32 {
        self.0
    }
}

impl From<u16> for GlyphId {
    fn from(id: u16) -> Self {
        Self(u32::from(id))
    }
}

/// Why a glyph could not be drawn hinted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawError {
    /// The glyph is not in the font (or has no style).
    GlyphNotFound,
    /// A loader buffer was too small: malformed data.
    InsufficientMemory,
    /// A composite nests or fans out past the limits.
    RecursionLimitExceeded,
    /// More than 65,535 points.
    TooManyPoints,
    /// A composite anchors to a point it does not have.
    InvalidAnchorPoint,
    /// The outline data could not be read.
    Read,
    /// The fitted points do not make a path.
    ToPath,
}

/// The rendering a hinting instance fits for: FreeType's smooth targets.
///
/// None of them hints advances back to the caller (aterm keeps advances
/// linear), and none applies the font's own instructions.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// Antialiased grayscale: stems and horizontal features fitted in both
    /// directions (FreeType's `FT_LOAD_TARGET_NORMAL`).
    Normal,
    /// Antialiased, fitted vertically only (`FT_LOAD_TARGET_LIGHT`, the
    /// desktop `hintslight` look).
    Light,
    /// Horizontal LCD subpixels: vertical stems snapped, no horizontal
    /// fitting of Latin outlines (`FT_LOAD_TARGET_LCD`).
    Lcd,
}

impl Target {
    pub(crate) fn is_light(self) -> bool {
        self == Self::Light
    }

    pub(crate) fn is_lcd(self) -> bool {
        self == Self::Lcd
    }
}

/// The automatic hinter at one size and variation instance of one face.
///
/// Built once per face, size and instance ([`Self::new`]), then asked for
/// each glyph ([`Self::draw`]) with the same face data. Glyph styles are
/// computed up front. Writing-system metrics are cached lazily; warm draws
/// take a read lock to clone cached metrics.
pub struct HintingInstance {
    ppem: f32,
    coords: Vec<fixed::F2Dot14>,
    instance: instance::Instance,
}

impl core::fmt::Debug for HintingInstance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HintingInstance")
            .field("ppem", &self.ppem)
            .field("coords", &self.coords)
            .finish_non_exhaustive()
    }
}

impl HintingInstance {
    /// Analyses face `index` of `data` for hinting at `ppem` pixels per em,
    /// at the variation instance of the user-space `(tag, value)` settings
    /// (an empty slice is the default instance; settings for axes the face
    /// lacks are ignored).
    ///
    /// `None` when the face cannot be read, has neither `glyf` nor CFF
    /// outlines, or `ppem` is not finite and positive.
    pub fn new(
        data: &[u8],
        index: u32,
        ppem: f32,
        settings: &[(u32, f32)],
        target: Target,
    ) -> Option<Self> {
        if !ppem.is_finite() || ppem <= 0.0 {
            return None;
        }
        let font = font::HintFont::new(data, index)?;
        let coords = font.location(settings);
        let instance = instance::Instance::new(&font, target);
        Some(Self {
            ppem,
            coords,
            instance,
        })
    }

    /// Draws glyph `glyph_id` of the face this instance was built for, grid
    /// fitted, into `sink` in pixels (y up): contours of lines and
    /// quadratic or cubic curves, each closed.
    ///
    /// `data` and `index` must name the same face [`Self::new`] read.
    /// `None` when the glyph is not in the face or its outline cannot be
    /// read; nothing has then been drawn, so a caller can fall back to the
    /// unhinted outline.
    pub fn draw(
        &self,
        data: &[u8],
        index: u32,
        glyph_id: u16,
        sink: &mut dyn OutlineSink,
    ) -> Option<()> {
        let font = font::HintFont::new(data, index)?;
        let glyph = font.outline(GlyphId::from(glyph_id))?;
        let mut recorded = path::Recorded::default();
        self.instance
            .draw(&font, self.ppem, &self.coords, &glyph, &mut recorded)
            .ok()?;
        recorded.replay(sink);
        Some(())
    }
}

/// All constants are defined based on a UPEM of 2048 (FreeType aflatin.h).
fn derived_constant(units_per_em: i32, value: i32) -> i32 {
    value * units_per_em / 2048
}
