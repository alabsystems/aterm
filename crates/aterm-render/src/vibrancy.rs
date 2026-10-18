// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vibrancy alpha policy: the configured `background_opacity` is multiplied into the
//! background-quad alpha only. Glyph ink, decorations and inline images stay opaque over
//! their own cell fills, so the desktop shows through the terminal's background without
//! the text sinking into the wallpaper.
//!
//! For every opacity `o ∈ [0, 1]`, [`bg_quad_alpha`] is `round(clamp(o) * 255)`: below
//! 255 exactly when `o < 1` and monotone in `o`. `tests/vibrancy_ink.rs` checks this over
//! the opacity lattice (the arithmetic has a `*`, so it cannot be a ty model). The
//! companion guarantee, that translucency engages the WCAG-AA contrast floor, is applied
//! here: [`effective_minimum_contrast`] is the law and `Renderer::effective_minimum_contrast`
//! blits with it, so native, CPU-web and GPU-web floor identically. Its ty model is
//! `aterm_models_render::vibrancy_contrast_model`, bound by `tests/vibrancy_contrast.rs`.

/// The alpha a background quad carries under `opacity`: `round(clamp01(opacity) * 255)`.
/// `>= 1.0` or non-finite → `255` (solid, the default); `0.0` → `0` (clear glass).
/// Multiplied into the bg-quad stream only.
#[must_use]
#[inline]
pub fn bg_quad_alpha(opacity: f32) -> u8 {
    if !opacity.is_finite() || opacity >= 1.0 {
        return 255;
    }
    if opacity <= 0.0 {
        return 0;
    }
    // Round-to-nearest; opacity ∈ (0,1) here, so the product ∈ (0,255).
    (opacity * 255.0 + 0.5) as u8
}

/// Whether `opacity` describes a translucent window (`< 1.0`): the one test that gates
/// the WCAG-AA legibility floor ([`effective_minimum_contrast`], applied by
/// `Renderer::effective_minimum_contrast`) and selects the vibrancy present path.
/// Non-finite is treated as solid.
#[must_use]
#[inline]
pub fn is_translucent(opacity: f32) -> bool {
    opacity.is_finite() && opacity < 1.0
}

/// WCAG AA (4.5:1): the per-cell contrast floor a translucent frame always clears. The
/// twin of `Floor` in `aterm_models_render::vibrancy_contrast_model`.
pub const CONTRAST_FLOOR: f32 = 4.5;

/// The per-cell minimum contrast a frame actually floors glyphs to:
/// `max(configured, CONTRAST_FLOOR)` while [`is_translucent`]`(opacity)`, else `configured`
/// unchanged. A NaN `configured` under glass yields [`CONTRAST_FLOOR`] (`f32::max` ignores
/// NaN). Idempotent: `effective(effective(c, o), o) == effective(c, o)`, so a host that
/// already passes the effective value changes nothing.
#[must_use]
#[inline]
pub fn effective_minimum_contrast(configured: f32, opacity: f32) -> f32 {
    if is_translucent(opacity) {
        configured.max(CONTRAST_FLOOR)
    } else {
        configured
    }
}
