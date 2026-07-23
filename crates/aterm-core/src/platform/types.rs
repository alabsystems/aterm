// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Platform data types for fonts and text shaping.
//!
//! These are pure data structures consumed by [`super::FontProvider`],
//! [`super::TextShaper`], and the rendering pipeline.

// =============================================================================
// Font Types
// =============================================================================

/// Describes a font to load.
#[derive(Debug, Clone, PartialEq)]
pub struct FontDescriptor {
    /// Font family name (e.g., "SF Mono", "Menlo").
    pub family: String,
    /// Font size in points.
    pub size: f32,
    /// Font weight (400 = normal, 700 = bold).
    pub weight: u16,
    /// Whether the font is italic.
    pub italic: bool,
}

/// Default: "SF Mono" at 14.0pt.
///
/// NOTE: this default does NOT drive rendering. The on-screen face is chosen
/// by `aterm-gui`'s `effective_font_family` ($ATERM_FONT > config
/// `font_family` > Monaco on macOS) resolved against `aterm-render`'s
/// `FONT_CANDIDATES`, at the GUI's `FONT_PX` (12.0) — none of which read this
/// value. It only (a) seeds `Terminal`'s write-only `font` bookkeeping field
/// (compared into `ConfigChange::Font`, which no production code consumes)
/// and (b) provides the size that `StubFontProvider` scales its mock test
/// metrics from. It is not mechanically synced with the GUI/render defaults
/// and has drifted before (Menlo here vs Monaco in the GUI; 14.0 here vs the
/// GUI's 12.0 today).
impl Default for FontDescriptor {
    fn default() -> Self {
        Self {
            family: String::from("SF Mono"),
            size: 14.0,
            weight: 400,
            italic: false,
        }
    }
}

impl FontDescriptor {
    /// Set the font weight.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_weight(mut self, weight: u16) -> Self {
        self.weight = weight;
        self
    }

    /// Set italic style.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    /// Create a bold variant.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn bold(&self) -> Self {
        Self {
            family: self.family.clone(),
            size: self.size,
            weight: 700,
            italic: self.italic,
        }
    }

    /// Create an italic variant.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn italic(&self) -> Self {
        Self {
            family: self.family.clone(),
            size: self.size,
            weight: self.weight,
            italic: true,
        }
    }
}

/// Loaded font data.
#[derive(Debug, Clone)]
pub struct FontData {
    /// The font descriptor this data corresponds to.
    pub descriptor: FontDescriptor,
    /// Platform-specific font handle or raw font bytes.
    pub data: FontDataKind,
    /// Font metrics.
    pub metrics: FontMetrics,
}

/// Platform-specific font data.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FontDataKind {
    /// Platform-specific handle (opaque identifier).
    Handle(u64),
}

/// Font metrics for layout calculations.
#[derive(Debug, Clone, Copy, Default)]
pub struct FontMetrics {
    /// Line height in pixels.
    pub line_height: f32,
    /// Character advance width (for monospace).
    pub cell_width: f32,
    /// Ascender height above baseline.
    pub ascent: f32,
    /// Descender depth below baseline.
    pub descent: f32,
    /// Leading (extra line spacing).
    pub leading: f32,
    /// Underline position below baseline.
    pub underline_position: f32,
    /// Underline thickness.
    pub underline_thickness: f32,
}

// =============================================================================
// Text Shaping Types
// =============================================================================

/// A run of text with consistent styling for shaping.
#[derive(Debug, Clone, Default)]
pub struct TextRun {
    /// The text to shape.
    pub text: String,
    /// Starting column in the terminal grid.
    pub start_column: usize,
    /// Row in the terminal grid (for cursor-disabled ligature check).
    pub row: usize,
    /// Optional text shaping configuration override.
    /// If None, uses renderer's default config.
    pub config: Option<crate::text_shaping_config::TextShapingConfig>,
    /// Cursor position if visible: (row, col).
    /// Used for cursor-disabled ligature mode.
    pub cursor: Option<(usize, usize)>,
}

/// A shaped glyph ready for rendering.
///
/// Matches Core Text CTRunGetGlyphs + CTRunGetPositions output.
/// The `font_id` field identifies which font provided this glyph (for fallback).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ShapedGlyph {
    /// Font ID (identifies which font provided this glyph).
    /// 0 = primary font, u32::MAX = invalid. Matches FFI FontHandle values.
    pub font_id: u32,
    /// Font-specific glyph index (from Core Text / HarfBuzz).
    pub glyph_id: u32,
    /// Cluster index (maps back to original text byte offset).
    pub cluster: u32,
    /// X offset from pen position (pixels, can be fractional).
    pub x_offset: f32,
    /// Y offset from baseline (pixels, can be fractional).
    pub y_offset: f32,
    /// Horizontal advance: cursor movement after glyph (pixels).
    pub x_advance: f32,
    /// Vertical advance: usually 0 for horizontal text (pixels).
    pub y_advance: f32,
}

impl Default for ShapedGlyph {
    fn default() -> Self {
        Self {
            font_id: Self::FONT_PRIMARY,
            glyph_id: 0,
            cluster: 0,
            x_offset: 0.0,
            y_offset: 0.0,
            x_advance: 0.0,
            y_advance: 0.0,
        }
    }
}

impl ShapedGlyph {
    /// Font ID for primary font (matches FontHandle::PRIMARY)
    pub const FONT_PRIMARY: u32 = 0;
}
