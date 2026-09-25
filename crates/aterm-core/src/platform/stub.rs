// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stub implementations of platform traits.
//!
//! [`StubTextShaper`] is the one stub left: a 1:1 glyph mapping for hosts
//! without real shaping.

#[allow(
    clippy::wildcard_imports,
    reason = "internal stub module re-uses parent traits"
)]
use super::*;

// =============================================================================
// Stub Text Shaper (production)
// =============================================================================

/// Stub text shaper for testing.
///
/// Returns simple 1:1 glyph mapping without actual shaping.
pub struct StubTextShaper;

impl TextShaper for StubTextShaper {
    fn shape(&self, run: &TextRun, font: &FontData) -> Vec<ShapedGlyph> {
        // Simple 1:1 mapping without real shaping
        run.text
            .char_indices()
            .map(|(byte_idx, c)| {
                let cluster = u32::try_from(byte_idx).unwrap_or(u32::MAX);
                ShapedGlyph {
                    font_id: ShapedGlyph::FONT_PRIMARY,
                    glyph_id: c as u32,
                    cluster,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    x_advance: font.metrics.cell_width,
                    y_advance: 0.0,
                }
            })
            .collect()
    }
}
