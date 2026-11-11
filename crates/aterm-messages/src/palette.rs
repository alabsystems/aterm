// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Band meter fill hue selection and `OkLCh` colour perceptual helpers.
//!
//! Uses the theme cursor colour when chromatic, or borrows a chromatic ANSI blue/cyan
//! ([`BORROW_CHROMA`]) when the cursor is near-grey ([`NEAR_GREY_CHROMA`]).

use crate::ink::contrast;

/// `OkLCh` chroma under which a cursor is near-grey for colour borrowing.
pub const NEAR_GREY_CHROMA: f64 = 0.08;

/// Minimum `OkLCh` chroma required to borrow ANSI blue or cyan.
pub const BORROW_CHROMA: f64 = 0.04;

const WORD_AA: f64 = 4.5;

/// Which of the theme's colours a meter's fill wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeterHue {
    /// The theme's cursor — every scheme whose cursor has a colour.
    Cursor,
    /// The theme's ANSI blue (slot 4).
    Blue,
    /// The theme's ANSI cyan (slot 6).
    Cyan,
}

impl MeterHue {
    /// Select the meter fill hue from `cursor`, `blue`, and `cyan` for label inks `words`.
    #[must_use]
    pub fn pick(cursor: [u8; 3], blue: [u8; 3], cyan: [u8; 3], words: &[[u8; 3]]) -> Self {
        if !near_grey(cursor) {
            return Self::Cursor;
        }
        let best = |fill: [u8; 3]| {
            words
                .iter()
                .map(|&w| contrast(w, fill))
                .fold(0.0f64, f64::max)
        };
        let (blue_ok, cyan_ok) = (chroma(blue) >= BORROW_CHROMA, chroma(cyan) >= BORROW_CHROMA);
        match (blue_ok, cyan_ok) {
            (false, false) => Self::Cursor,
            (true, false) => Self::Blue,
            (false, true) => Self::Cyan,
            (true, true) => {
                let (b, c) = (best(blue), best(cyan));
                if b >= WORD_AA || (c < WORD_AA && b >= c) {
                    Self::Blue
                } else {
                    Self::Cyan
                }
            }
        }
    }

    /// Resolve this hue to its RGB colour.
    #[inline]
    #[must_use]
    pub const fn of(self, cursor: [u8; 3], blue: [u8; 3], cyan: [u8; 3]) -> [u8; 3] {
        match self {
            Self::Cursor => cursor,
            Self::Blue => blue,
            Self::Cyan => cyan,
        }
    }
}

/// Return `true` when `floored` reads brown, `blue` is chromatic, and `blue_floored`
/// does not read brown.
#[inline]
#[must_use]
pub fn outline_borrows_blue(floored: [u8; 3], blue: [u8; 3], blue_floored: [u8; 3]) -> bool {
    reads_brown(floored) && chroma(blue) >= BORROW_CHROMA && !reads_brown(blue_floored)
}

/// Return `true` when flooring `hue` darkens a non-brown pastel into brown.
#[inline]
#[must_use]
pub fn keeps_pastel(hue: [u8; 3], floored: [u8; 3]) -> bool {
    floored != hue && reads_brown(floored) && !reads_brown(hue)
}

/// WCAG 1.4.11 non-text contrast floor (`3.0:1`) for a pastel fill's edge line.
pub const EDGE_CONTRAST: f64 = 3.0;

/// Darken or lighten `fill` in linear light until it reaches [`EDGE_CONTRAST`] against `track`.
#[must_use]
pub fn pastel_edge(fill: [u8; 3], track: [u8; 3]) -> [u8; 3] {
    if contrast(fill, track) >= EDGE_CONTRAST {
        return fill;
    }
    let away = if luminance(track) > luminance(fill) {
        0.0
    } else {
        1.0
    };
    let at = |s: f64| fill.map(|v| enc(lin(v).mul_add(1.0 - s, away * s)));
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = f64::midpoint(lo, hi);
        if contrast(at(mid), track) >= EDGE_CONTRAST {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

/// The lower end of the warm hues that darken into brown (orange through olive), in
/// `OkLCh` degrees.
pub(crate) const WARM_HUE_MIN: f64 = 20.0;
/// The upper end of those warm hues.
pub(crate) const WARM_HUE_MAX: f64 = 110.0;
/// Below this `OkLCh` lightness a warm hue reads brown, not orange or gold.
pub(crate) const BROWN_LIGHTNESS: f64 = 0.62;

/// Whether `c` reads brown: a warm `OkLCh` hue, dark, with colour.
#[inline]
#[must_use]
pub fn reads_brown(c: [u8; 3]) -> bool {
    let (l, ch, h) = oklch(c);
    ch >= BORROW_CHROMA && (WARM_HUE_MIN..=WARM_HUE_MAX).contains(&h) && l < BROWN_LIGHTNESS
}

/// Whether `c` is near-grey: its `OkLCh` chroma under [`NEAR_GREY_CHROMA`].
#[inline]
#[must_use]
pub fn near_grey(c: [u8; 3]) -> bool {
    chroma(c) < NEAR_GREY_CHROMA
}

/// The `OkLCh` chroma of an sRGB colour (Björn Ottosson's `OkLab`).
#[inline]
#[must_use]
pub fn chroma(c: [u8; 3]) -> f64 {
    oklch(c).1
}

/// Convert an sRGB colour to `OkLCh` `(lightness, chroma, hue_degrees)`.
#[inline]
#[must_use]
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "Björn Ottosson's published `OkLab` matrices, digit for digit, and variable names"
)]
pub fn oklch(c: [u8; 3]) -> (f64, f64, f64) {
    let [r, g, b] = c.map(lin);
    let l = 0.0514459929f64.mul_add(b, 0.4122214708f64.mul_add(r, 0.5363325363 * g));
    let m = 0.1073969566f64.mul_add(b, 0.2119034982f64.mul_add(r, 0.6806995451 * g));
    let s = 0.6299787005f64.mul_add(b, 0.0883024619f64.mul_add(r, 0.2817188376 * g));
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    let big_l = 0.0040720468f64.mul_add(-s, 0.2104542553f64.mul_add(l, 0.7936177850 * m));
    let a = 0.4505937099f64.mul_add(s, 1.9779984951f64.mul_add(l, -2.4285922050 * m));
    let bb = (-0.8086757660f64).mul_add(s, 0.0259040371f64.mul_add(l, 0.7827717662 * m));
    let h = bb.atan2(a).to_degrees().rem_euclid(360.0);
    (big_l, a.hypot(bb), h)
}

/// Euclidean `OkLab` distance between two sRGB colours.
#[inline]
#[must_use]
pub fn oklab_distance(a: [u8; 3], b: [u8; 3]) -> f64 {
    let lab = |c: [u8; 3]| {
        let (l, ch, h) = oklch(c);
        let (sin, cos) = h.to_radians().sin_cos();
        [l, ch * cos, ch * sin]
    };
    let (p, q) = (lab(a), lab(b));
    (p[0] - q[0]).hypot(p[1] - q[1]).hypot(p[2] - q[2])
}

/// WCAG relative luminance of an sRGB colour.
#[inline]
#[must_use]
pub fn luminance(c: [u8; 3]) -> f64 {
    let [r, g, b] = c.map(lin);
    0.2126f64.mul_add(r, 0.7152f64.mul_add(g, 0.0722 * b))
}

/// Convert an sRGB channel byte to linear light.
#[inline]
#[must_use]
pub fn lin(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light back to an sRGB byte.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to 0..=255 before the cast"
)]
pub(crate) fn enc(l: f64) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055f64.mul_add(l.powf(1.0 / 2.4), -0.055)
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
