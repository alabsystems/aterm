// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The meter's hue (docs/DESIGN-unified-messages-2026-09-21.md): which of the theme's
//! own colours a band meter's fill wears, decided on plain sRGB bytes so every host
//! paints the same bar.
//!
//! The fill is the theme's cursor. Most schemes set the cursor to their foreground, a
//! near-grey ([`NEAR_GREY_CHROMA`]), and there the bar borrows the theme's ANSI blue or
//! cyan so it keeps colour. Blue is taken when one of the band's word inks (the
//! terminal's and the band's grounds and the band's value ink) reads on it at WCAG AA;
//! else cyan when cyan does; else whichever the words read better on (the host still
//! floors each word to AA on the fill). A candidate under [`BORROW_CHROMA`] is never
//! borrowed, so a monochrome palette keeps its cursor.
//!
//! A hue the band's 3:1 floor would darken into brown keeps its own pastel and a darker
//! edge line carries the boundary ([`keeps_pastel`], [`pastel_edge`]).

use crate::ink::contrast;

/// A cursor at an `OkLCh` chroma under this is near-grey: its bar borrows the theme's
/// blue or cyan. The middle of the widest gap in the built-in schemes' cursor chromas
/// (0.057 → 0.104).
pub const NEAR_GREY_CHROMA: f64 = 0.08;

/// The least `OkLCh` chroma a borrowed blue or cyan must have: half the
/// near-grey line, a hue the eye reads as a colour (Nord's desaturated frost
/// blue, 0.059, is one; the grey slots of a monochrome palette are not).
pub const BORROW_CHROMA: f64 = 0.04;

/// WCAG AA for text: what the band's words need on a fill.
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
    /// The hue a meter's fill wears for a theme whose cursor is `cursor` and whose ANSI
    /// blue and cyan are `blue` and `cyan`, where `words` are the inks the band writes on
    /// a fill.
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

    /// This hue's colour among the three.
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

/// Whether a metered row's outlined Primary borrows the theme's ANSI blue: its ring and
/// label wear the meter's hue floored to AA on the outline's inside, and a warm hue
/// floored that far reads brown. The blue is borrowed when it has colour of its own
/// ([`BORROW_CHROMA`]) and does not read brown itself; else the outline keeps the meter.
///
/// `floored` is the label as the host floors it; `blue_floored` the blue floored the
/// same way.
#[inline]
#[must_use]
pub fn outline_borrows_blue(floored: [u8; 3], blue: [u8; 3], blue_floored: [u8; 3]) -> bool {
    reads_brown(floored) && chroma(blue) >= BORROW_CHROMA && !reads_brown(blue_floored)
}

/// Whether a meter fill keeps its own pastel: the band's 3:1 non-text floor would darken
/// `hue` (the theme's cursor, or the blue or cyan it borrows) into brown, so the fill
/// stays as the theme gives it and a darker edge line ([`pastel_edge`]) carries the
/// boundary. True exactly when the floor moved a hue that does not read brown
/// ([`reads_brown`]) into one that does (`floored`, as the host floors it on the band);
/// otherwise the floored fill stands byte for byte. Catppuccin Latte's rosewater is the
/// one built-in case.
#[inline]
#[must_use]
pub fn keeps_pastel(hue: [u8; 3], floored: [u8; 3]) -> bool {
    floored != hue && reads_brown(floored) && !reads_brown(hue)
}

/// The contrast a pastel fill's edge line stands from the track: the WCAG 1.4.11
/// non-text floor.
pub const EDGE_CONTRAST: f64 = 3.0;

/// The edge line of a pastel fill ([`keeps_pastel`]): `fill` mixed in linear light toward
/// black (toward white when the fill is the lighter of the two) by the least amount that
/// stands it [`EDGE_CONTRAST`]:1 from `track`; `fill` itself where it already does.
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

/// The `OkLCh` lightness, chroma and hue (degrees, `[0, 360)`) of an sRGB
/// colour (Björn Ottosson's `OkLab`).
#[inline]
#[must_use]
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "Björn Ottosson's published `OkLab` matrices, digit for digit, in his names"
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

/// The Euclidean `OkLab` distance between two sRGB colours: how far apart
/// they look, lightness and colour together.
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

/// The WCAG relative luminance of an sRGB colour, with a fused weighted sum.
///
/// The band's side decisions (which end of the scale a word or a fill sits toward) read
/// this; every contrast ratio is [`contrast`], `aterm_types::Rgb`'s unfused arithmetic.
#[inline]
#[must_use]
pub fn luminance(c: [u8; 3]) -> f64 {
    let [r, g, b] = c.map(lin);
    0.2126f64.mul_add(r, 0.7152f64.mul_add(g, 0.0722 * b))
}

/// An sRGB byte in linear light — the one copy the palette, the band's
/// resolver ([`crate::ink`]) and the hosts' tests read.
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
mod tests {
    use super::*;

    const DARK_WORDS: [[u8; 3]; 2] = [[0x28, 0x2a, 0x36], [0xf8, 0xf8, 0xf2]];

    /// A cursor with a colour keeps it; a grey one borrows.
    #[test]
    fn a_coloured_cursor_keeps_its_hue_and_a_grey_one_borrows() {
        let (blue, cyan) = ([0x26, 0x8b, 0xd2], [0x2a, 0xa1, 0x98]);
        for cursor in [[0x50, 0xfa, 0x7b], [0x7a, 0xa2, 0xf7], [0xdc, 0x8a, 0x78]] {
            assert_eq!(
                MeterHue::pick(cursor, blue, cyan, &DARK_WORDS),
                MeterHue::Cursor
            );
        }
        for cursor in [[0x65, 0x7b, 0x83], [0xf8, 0xf8, 0xf2], [0x80, 0x80, 0x80]] {
            assert_ne!(
                MeterHue::pick(cursor, blue, cyan, &DARK_WORDS),
                MeterHue::Cursor
            );
        }
    }

    /// Blue while the band's words read on it; cyan where only cyan carries
    /// them; the better of the two where neither does; never a grey.
    #[test]
    fn blue_unless_only_cyan_carries_the_words() {
        let grey = [0x83, 0x94, 0x96];
        let words = [[0x00, 0x2b, 0x36], [0x93, 0xa1, 0xa1]];
        // Solarized Dark: its blue reads 4.08:1 at best, its cyan 4.75:1.
        assert_eq!(
            MeterHue::pick(grey, [0x26, 0x8b, 0xd2], [0x2a, 0xa1, 0x98], &words),
            MeterHue::Cyan
        );
        // A blue the words read on is taken whatever the cyan.
        assert_eq!(
            MeterHue::pick(grey, [0x61, 0xaf, 0xef], [0x56, 0xb6, 0xc2], &words),
            MeterHue::Blue
        );
        // Neither carries them: the better one.
        let light = [[0xfd, 0xf6, 0xe3], [0x58, 0x6e, 0x75]];
        assert_eq!(
            MeterHue::pick(
                [0x65, 0x7b, 0x83],
                [0x26, 0x8b, 0xd2],
                [0x2a, 0xa1, 0x98],
                &light
            ),
            MeterHue::Blue
        );
        // Nord's frost blue is desaturated (0.059) but a colour: borrowed.
        assert_eq!(
            MeterHue::pick(
                [0xec, 0xef, 0xf4],
                [0x81, 0xa1, 0xc1],
                [0x88, 0xc0, 0xd0],
                &[[0x2e, 0x34, 0x40], [0xd8, 0xde, 0xe9]]
            ),
            MeterHue::Blue
        );
        // A grey blue is never borrowed.
        assert_eq!(
            MeterHue::pick(grey, [0x70, 0x70, 0x70], [0x2a, 0xa1, 0x98], &words),
            MeterHue::Cyan
        );
        assert_eq!(
            MeterHue::pick(grey, [0x70, 0x70, 0x70], [0x71, 0x71, 0x71], &words),
            MeterHue::Cursor
        );
    }

    /// A warm hue gone dark reads brown; the same hue light, a cool hue dark, or a grey
    /// does not.
    #[test]
    fn a_dark_warm_hue_reads_brown_and_borrows_blue() {
        assert!(reads_brown([0x8b, 0x4a, 0x2b]), "a saddle brown");
        assert!(reads_brown([0x9a, 0x55, 0x45]), "a terracotta label");
        assert!(!reads_brown([0xdc, 0x8a, 0x78]), "the rosewater itself");
        assert!(!reads_brown([0x1e, 0x66, 0xf5]), "Latte's blue");
        assert!(!reads_brown([0x40, 0x40, 0x40]), "a grey");
        assert!(!reads_brown([0x1a, 0x7f, 0x37]), "a dark green");
        let (l, c, h) = oklch([0xff, 0x00, 0x00]);
        assert!((l - 0.628).abs() < 0.01 && (c - 0.258).abs() < 0.01 && (h - 29.2).abs() < 0.5);
        let blue = [0x1e, 0x66, 0xf5];
        assert!(outline_borrows_blue([0x9a, 0x55, 0x45], blue, blue));
        assert!(!outline_borrows_blue([0x1e, 0x66, 0xf5], blue, blue));
        assert!(!outline_borrows_blue(
            [0x9a, 0x55, 0x45],
            [0x70, 0x70, 0x70],
            [0x70, 0x70, 0x70]
        ));
    }

    /// Latte's rosewater, floored to brown on its light band, keeps its pastel; an
    /// unfloored hue, a cool hue and an already-brown hue do not. The edge is the same
    /// hue deepened to the least 3:1 from the track.
    #[test]
    fn a_warm_pastel_floored_brown_keeps_its_fill_and_takes_a_darker_edge() {
        let rosewater = [0xdc, 0x8a, 0x78];
        let brick = [0xb0, 0x6e, 0x60];
        assert!(keeps_pastel(rosewater, brick), "Latte's brick bar");
        assert!(!keeps_pastel(rosewater, rosewater), "not floored");
        assert!(
            !keeps_pastel([0x1e, 0x66, 0xf5], [0x1a, 0x57, 0xd0]),
            "a blue floored stays blue"
        );
        assert!(
            !keeps_pastel([0x8b, 0x4a, 0x2b], [0x6f, 0x3b, 0x22]),
            "a brown was never a pastel"
        );
        let track = [0xdf, 0xd8, 0xdc];
        let edge = pastel_edge(rosewater, track);
        assert!(contrast(edge, track) >= EDGE_CONTRAST, "{edge:?}");
        assert!(
            contrast(edge, track) < EDGE_CONTRAST + 0.1,
            "the least move"
        );
        assert!(contrast(edge, rosewater) >= 1.4, "darker than the fill");
        assert!(
            (oklch(edge).2 - oklch(rosewater).2).abs() < 3.0,
            "the same hue"
        );
        // A fill that already stands 3:1 is its own edge.
        let deep = [0x90, 0x50, 0x40];
        assert_eq!(pastel_edge(deep, track), deep);
        // On a dark track the edge lightens.
        let dark = [0x20, 0x20, 0x28];
        let light = pastel_edge([0x60, 0x40, 0x40], dark);
        assert!(luminance(light) > luminance([0x60, 0x40, 0x40]));
        assert!(contrast(light, dark) >= EDGE_CONTRAST);
    }

    /// The chroma is `OkLCh`'s: grey is zero, the sRGB primaries are vivid.
    #[test]
    fn chroma_reads_oklch() {
        assert!(chroma([0x80, 0x80, 0x80]) < 1e-3);
        assert!(chroma([0, 0, 255]) > 0.3);
        assert!(near_grey([0x65, 0x7b, 0x83]), "Solarized Light's cursor");
        assert!(
            !near_grey([0xdc, 0x8a, 0x78]),
            "Catppuccin Latte's rosewater"
        );
    }
}
