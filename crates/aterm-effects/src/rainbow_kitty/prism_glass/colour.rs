// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The solver's colour arithmetic, in f64 (`docs/design/PRISM-GLASS.md` §1): OKLCH over
//! aterm_types' published OkLab matrices, the sRGB gamut edge by bisection, rounding to 8 bits,
//! and WCAG contrast. Every table entry is solved here and then stored as its 8-bit hex, which is
//! authoritative: every bar is read back off the hex, never off the OKLCH it came from.

use std::sync::OnceLock;

use aterm_types::Rgb;
use aterm_types::scheme_palette256::oklab_to_linear;
use aterm_types::scheme_signature::oklab;

/// Bisection steps toward the gamut edge's chroma, from `[0, 0.5]`: far under a step of the
/// 8-bit grid.
const EDGE_STEPS: u32 = 34;

/// Ternary-search steps toward a hue's cusp lightness.
const CUSP_STEPS: u32 = 80;

/// A colour in OKLCH: lightness, chroma and hue (degrees in `[0, 360)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// `0x00RRGGBB` as aterm_types' colour.
#[inline]
const fn rgb(c: u32) -> Rgb {
    Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// The OKLCH of an 8-bit colour.
#[must_use]
pub fn lch(c: u32) -> Lch {
    let (l, a, b) = oklab(rgb(c));
    Lch {
        l,
        c: a.hypot(b),
        h: b.atan2(a).to_degrees().rem_euclid(360.0),
    }
}

/// The linear sRGB of an OKLCH point, possibly outside `[0, 1]`.
#[inline]
fn linear(l: f64, c: f64, h: f64) -> (f64, f64, f64) {
    let (s, co) = h.to_radians().sin_cos();
    oklab_to_linear((l, c * co, c * s))
}

/// Whether an OKLCH point lies inside the sRGB gamut.
#[inline]
fn in_gamut(l: f64, c: f64, h: f64) -> bool {
    let (r, g, b) = linear(l, c, h);
    [r, g, b].iter().all(|v| (0.0..=1.0).contains(v))
}

/// The largest chroma at lightness `l` and hue `h` that sRGB holds: the gamut edge.
#[must_use]
pub fn gamut_edge(l: f64, h: f64) -> f64 {
    if !in_gamut(l, 0.0, h) {
        return 0.0;
    }
    let (mut lo, mut hi) = (0.0_f64, 0.5_f64);
    for _ in 0..EDGE_STEPS {
        let mid = 0.5 * (lo + hi);
        if in_gamut(l, mid, h) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The lightness at which hue `h` holds its most chroma: its cusp on the gamut edge.
#[must_use]
pub fn cusp_l(h: f64) -> f64 {
    let (mut lo, mut hi) = (0.05_f64, 0.999_f64);
    for _ in 0..CUSP_STEPS {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if gamut_edge(m1, h) < gamut_edge(m2, h) {
            lo = m1;
        } else {
            hi = m2;
        }
    }
    0.5 * (lo + hi)
}

/// One linear channel encoded to sRGB and rounded half up to 8 bits, clamped to the gamut.
#[inline]
fn encode(v: f64) -> u32 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u32
}

/// An OKLCH point as `0x00RRGGBB`.
#[must_use]
pub fn quantise(l: f64, c: f64, h: f64) -> u32 {
    let (r, g, b) = linear(l, c, h);
    (encode(r) << 16) | (encode(g) << 8) | encode(b)
}

/// The colour at lightness `l` and hue `h` whose chroma is the gamut edge, capped at `cap`.
#[must_use]
pub fn edge_ink(l: f64, h: f64, cap: f64) -> u32 {
    quantise(l, gamut_edge(l, h).min(cap), h)
}

/// WCAG's linear light of each 8-bit sRGB level.
#[must_use]
pub fn linear_levels() -> &'static [f64; 256] {
    static LEVELS: OnceLock<[f64; 256]> = OnceLock::new();
    LEVELS.get_or_init(|| {
        let mut out = [0.0; 256];
        for (v, slot) in out.iter_mut().enumerate() {
            let s = v as f64 / 255.0;
            *slot = if s <= 0.040_45 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
        }
        out
    })
}

/// WCAG relative luminance of `0x00RRGGBB`.
#[must_use]
pub fn luminance(c: u32) -> f64 {
    let lin = linear_levels();
    0.2126 * lin[((c >> 16) & 0xff) as usize]
        + 0.7152 * lin[((c >> 8) & 0xff) as usize]
        + 0.0722 * lin[(c & 0xff) as usize]
}

/// The WCAG contrast of two luminances.
#[inline]
#[must_use]
pub fn contrast_y(a: f64, b: f64) -> f64 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The WCAG contrast of two colours.
#[must_use]
pub fn contrast(a: u32, b: u32) -> f64 {
    contrast_y(luminance(a), luminance(b))
}
