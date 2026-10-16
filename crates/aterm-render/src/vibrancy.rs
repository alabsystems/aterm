// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vibrancy alpha policy: the configured `background_opacity` is multiplied into the
//! background-quad alpha only. Glyph ink, decorations and inline images stay opaque over
//! their own cell fills, so the desktop shows through the terminal's background without
//! the text sinking into the wallpaper.
//!
//! For every opacity `o ∈ [0, 1]`, [`ink_quad_alpha`] is 255, and [`bg_quad_alpha`] is
//! `round(clamp(o) * 255)`: below 255 exactly when `o < 1` and monotone in `o`.
//! `tests/vibrancy_ink.rs` checks both over the opacity lattice, with a negative control
//! (the arithmetic has a `*`, so it cannot be a ty model). The companion guarantee, that
//! translucency engages the WCAG-AA contrast floor, is the ty model
//! `aterm_spec::derive::vibrancy_contrast_model`.

/// The fully opaque alpha every ink stream (glyph coverage, decorations, inline images)
/// carries regardless of `background_opacity`.
pub const INK_ALPHA: u8 = 255;

/// The alpha an ink quad carries under `opacity`: always [`INK_ALPHA`]. It takes
/// `opacity` so the independence is explicit and the proof is non-vacuous.
#[must_use]
#[inline]
pub fn ink_quad_alpha(_opacity: f32) -> u8 {
    INK_ALPHA
}

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
/// the WCAG-AA legibility floor (in `aterm-gui`) and selects the vibrancy present path.
/// Non-finite is treated as solid.
#[must_use]
#[inline]
pub fn is_translucent(opacity: f32) -> bool {
    opacity.is_finite() && opacity < 1.0
}
