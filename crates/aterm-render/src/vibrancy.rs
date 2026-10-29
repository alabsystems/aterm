// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vibrancy alpha policy: `background_opacity` scales only background-quad alpha
//! and engages the WCAG-AA contrast floor.

/// Background-quad alpha under `opacity`: `round(clamp01(opacity) * 255)` capped at `254` below
/// `1.0`, or `255` when non-finite or `>= 1.0`.
#[must_use]
#[inline]
pub const fn bg_quad_alpha(opacity: f32) -> u8 {
    if !opacity.is_finite() || opacity >= 1.0 {
        return 255;
    }
    if opacity <= 0.0 {
        return 0;
    }
    let rounded = (opacity * 255.0 + 0.5) as u8;
    if rounded == 255 { 254 } else { rounded }
}

/// Whether `opacity` describes a translucent window (`opacity.is_finite() && opacity < 1.0`).
#[must_use]
#[inline]
pub const fn is_translucent(opacity: f32) -> bool {
    opacity.is_finite() && opacity < 1.0
}

/// WCAG AA (4.5:1) per-cell contrast floor for translucent frames.
pub const CONTRAST_FLOOR: f32 = 4.5;

/// Effective per-cell minimum contrast (`max(configured, CONTRAST_FLOOR)` when translucent).
#[must_use]
#[inline]
pub const fn effective_minimum_contrast(configured: f32, opacity: f32) -> f32 {
    if is_translucent(opacity) {
        configured.max(CONTRAST_FLOOR)
    } else {
        configured
    }
}
