// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/instance.rs and modified by
// the aterm project in 2026: no advance or side bearing is computed (aterm
// keeps advances linear), so the fitted outline alone is drawn.
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Autohinting state for a font instance.

use super::{
    DrawError, GlyphId, Target,
    fixed::F2Dot14,
    font::{HintFont, OutlineGlyph},
    hint::hint_outline,
    metrics::{Scale, ScaleFlags, UnscaledStyleMetricsSet, pix_round},
    outline::Outline,
    shape::Shaper,
    style::GlyphStyleMap,
};
use crate::OutlineSink;

/// The derived glyph styles and the lazily computed per-style metrics.
pub(crate) struct Instance {
    styles: GlyphStyleMap,
    metrics: UnscaledStyleMetricsSet,
    target: Target,
    is_italic: bool,
}

impl Instance {
    pub fn new(font: &HintFont, target: Target) -> Self {
        let shaper = Shaper::new(font);
        let styles = GlyphStyleMap::new(font.glyph_count(), &shaper);
        let metrics = UnscaledStyleMetricsSet::lazy(&styles);
        Self {
            styles,
            metrics,
            target,
            is_italic: font.is_italic(),
        }
    }

    pub fn draw(
        &self,
        font: &HintFont,
        ppem: f32,
        coords: &[F2Dot14],
        glyph: &OutlineGlyph,
        pen: &mut impl OutlineSink,
    ) -> Result<(), DrawError> {
        let glyph_id = match glyph {
            OutlineGlyph::Glyf(outline) => GlyphId::from(outline.glyph_id),
            OutlineGlyph::Cff(glyph_id) => *glyph_id,
        };
        let style = self
            .styles
            .style(glyph_id)
            .ok_or(DrawError::GlyphNotFound)?;
        let metrics = self
            .metrics
            .get(font, coords, &self.styles, glyph_id)
            .ok_or(DrawError::GlyphNotFound)?;
        let units_per_em = font.units_per_em();
        let scale = Scale::new(
            ppem,
            units_per_em,
            self.is_italic,
            self.target,
            metrics.style_class().script.group,
        );
        let mut outline = Outline::default();
        outline.fill(font, glyph, coords)?;
        let hinted_metrics = hint_outline(&mut outline, &metrics, &scale, Some(style));
        // The left side bearing shift FreeType applies to the fitted outline
        // (afloader.c); the advance it also fits is not wanted here.
        let mut pp1x = 0;
        let is_light = self.target.is_light();
        // <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afloader.c#L422>
        if !is_light {
            if let (true, Some(edge_metrics)) = (
                !scale.flags.contains(ScaleFlags::NO_ADVANCE),
                hinted_metrics.edge_metrics,
            ) {
                let old_lsb = edge_metrics.left_opos;
                let new_lsb = edge_metrics.left_pos;
                let mut pp1x_uh = new_lsb - old_lsb;
                if old_lsb < 24 {
                    pp1x_uh -= 8;
                }
                pp1x = pix_round(pp1x_uh);
                if pp1x >= new_lsb && old_lsb > 0 {
                    pp1x -= 64;
                }
            } else {
                pp1x = pix_round(pp1x);
            }
        } else {
            pp1x = pix_round(pp1x);
        }
        if pp1x != 0 {
            for point in &mut outline.points {
                point.x -= pp1x;
            }
        }
        outline.to_path(pen)
    }
}
