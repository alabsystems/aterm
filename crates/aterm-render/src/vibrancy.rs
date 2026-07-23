// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TRUE VIBRANCY alpha policy (M5) — the pure, backend-agnostic rule for HOW the
//! configured `background_opacity` reaches the compositor: it is multiplied into
//! the BACKGROUND-QUAD alpha ONLY. Glyph ink, font-metric decorations, and inline
//! images stay fully opaque over their own cell fills, so the desktop shows
//! through the *background* of the terminal without ever bleeding through the
//! *text* — the failure mode iTerm2's blur and ghostty's `background-blur` both
//! ship (text sinks into the wallpaper).
//!
//! # Invariant (proven — the ink-opacity PROVE bullet)
//!
//! For EVERY opacity setting `o ∈ [0, 1]`:
//!
//! * [`ink_quad_alpha`] `== 255` (fully opaque) — the glyph / decoration / image
//!   streams never inherit the background's translucency.
//! * [`bg_quad_alpha`] is `round(clamp(o) * 255)`: it is `< 255` exactly when the
//!   window is translucent (`o < 1`), `== 255` at `o == 1` (solid — the byte-
//!   identical default), and monotonically non-decreasing in `o`.
//!
//! These two facts are exhaustively checked over the opacity lattice in
//! `aterm-render/tests/vibrancy_ink.rs` (the Tier-1 L0 proof, with a non-vacuity
//! control and a negative control reproducing the pre-fix "ink goes translucent
//! too" defect). The arithmetic (`o * 255`) has a `*`, so it cannot be a `ty`
//! model — the lattice test is the documented waiver, mirroring the box-drawing
//! rounding law. The COMPANION guarantee — that translucency auto-engages the
//! WCAG-AA contrast floor — is the ordering policy `ty` DOES carry
//! (`aterm_spec::derive::vibrancy_contrast_model`).

/// The fully-opaque alpha every INK stream (glyph coverage, decorations, inline
/// images) carries regardless of `background_opacity`. A named constant so the
/// invariant "ink never goes translucent" is stated once and reused by the
/// present paths and the proof.
pub const INK_ALPHA: u8 = 255;

/// The alpha an INK quad carries under `opacity` — always [`INK_ALPHA`]. Takes
/// `opacity` so the "independent of the opacity setting" property is explicit
/// (and the proof is non-vacuous over the whole lattice).
#[must_use]
#[inline]
pub fn ink_quad_alpha(_opacity: f32) -> u8 {
    INK_ALPHA
}

/// The alpha a BACKGROUND quad carries under `opacity`: `round(clamp01(opacity) *
/// 255)`. `1.0` (and any `>= 1.0`, or a non-finite value) → `255` — solid, the
/// byte-identical default; `0.0` → `0` — fully transparent glass. Multiply this
/// into the theme background's alpha channel in the bg-quad stream ONLY.
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

/// Whether `opacity` describes a TRANSLUCENT window (`< 1.0`) — the single test
/// that both gates the WCAG-AA legibility floor (in `aterm-gui`) and selects the
/// vibrancy present path. Non-finite is treated as solid (fail-safe to opaque).
#[must_use]
#[inline]
pub fn is_translucent(opacity: f32) -> bool {
    opacity.is_finite() && opacity < 1.0
}
