// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/mod.rs, src/outline/hint.rs and
// src/outline/error.rs, and modified by the aterm project in 2026.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Automatic glyph hinter (ported from skrifa 0.46.2 / FreeType autofit).

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

/// 32-bit glyph identifier.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub(crate) struct GlyphId(u32);

impl GlyphId {
    /// Create a glyph identifier.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Return the identifier as a `u32`.
    pub const fn to_u32(self) -> u32 {
        self.0
    }
}

impl From<u16> for GlyphId {
    fn from(id: u16) -> Self {
        Self(u32::from(id))
    }
}

/// Error returned when a glyph cannot be drawn hinted.
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
    /// The size is past the scale the hinter fits at
    /// ([`metrics::MAX_SCALE_26_6`]).
    ScaleOutOfRange,
}

/// Smooth hinting target mode.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// Antialiased grayscale fitted in both axes (`FT_LOAD_TARGET_NORMAL`).
    Normal,
    /// Antialiased fitted vertically only (`FT_LOAD_TARGET_LIGHT`).
    Light,
    /// Horizontal LCD subpixel target (`FT_LOAD_TARGET_LCD`).
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

/// Automatic hinter state for a font face at one size and variation instance.
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
    /// Analyze face `index` of `data` for hinting at `ppem` and `coords` under `target`.
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
        if !metrics::scale_fits(ppem, font.units_per_em()) {
            return None;
        }
        let coords = font.location(settings);
        let instance = instance::Instance::new(&font, target);
        Some(Self {
            ppem,
            coords,
            instance,
        })
    }

    /// Load, grid-fit, and emit the outline of `glyph_id` into `sink` in pixel coordinates.
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
