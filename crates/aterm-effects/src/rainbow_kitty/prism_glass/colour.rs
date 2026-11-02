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
    luminance_rgb([(c >> 16) as u8, (c >> 8) as u8, c as u8])
}

/// [`luminance`] of the channels `[r, g, b]`.
#[inline]
#[must_use]
pub fn luminance_rgb([r, g, b]: [u8; 3]) -> f64 {
    let lin = linear_levels();
    0.2126 * lin[usize::from(r)] + 0.7152 * lin[usize::from(g)] + 0.0722 * lin[usize::from(b)]
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

/// The judge's solid ink: a glyph's pixels that read at least this share of its extreme's ratio
/// against its clean ground, so a frame equal to its twin reads close to what the clean glyph
/// reads (the antialiased rim reads less on any page, and on paper a pixel nine tenths of the way
/// to the ink in luminance is still only 6:1). The one source of the trail judge's metric:
/// `aterm-lab trail-contrast` and `trail-census` read their glyphs by it, aterm-render's
/// `glass_letters_read` reads the glass's letters by it, the letter guard keeps a light's letters
/// by it, and the glass is solved to it ([`seen_y`]).
pub const SOLID_SHARE: f64 = 0.85;

/// The widest ring of untouched ground a solid ink pixel's local ground is read from, px
/// (Chebyshev, 1 then this far out): the trail judge's ring ([`SOLID_SHARE`]'s readers').
pub const RING_REACH: i64 = 2;

/// A cell whose extreme reads under this ratio against its ground holds no glyph: the trail
/// judge's blank ([`SOLID_SHARE`]'s readers').
pub const BLANK_BELOW: f64 = 1.5;

/// How far toward its page, as a share of linear light, the darkest pixel of the thinnest glyph
/// the window's face lays may lie: the glass is solved for a face that lays it no further, and
/// draws only where the window's face does (`host::PageFacts::glass`, read off the face's raster
/// by `aterm_render::GlyphInk::thin_share`, `docs/design/PRISM-GLASS.md` §15.8 amendment 53), so
/// every letter's solid ink keeps its bar. The judge reads a glyph's solid ink against its own
/// extreme, so a glyph none of whose pixels reaches the full ink reads under the bar a full ink
/// keeps. The solve's share was measured by aterm-render's `glass_letters_read` from the frame
/// (§15.8 amendment 49): SF Mono, macOS's primary, at 24 px (a 12 pt window at scale 2) leaves
/// its `"` and `%` 0.0044 to 0.0069 short of the full ink by page as the census reads them. The
/// raster's own share, which the page reads, is 0 there (every printable glyph lays a pixel at
/// full coverage), at most 0.0036 at 21, 23 and 25 px, 0 from 27 to 48 px, 0.0046 to 0.0074 by
/// page at 22 px, and over the share at 26 px (0.0090 to 0.0141: its `_` lays no row at the full
/// ink, 5.09 by the census on a 5.25 bar), at 20 px (0.0108 to 0.0169) and at every size under
/// (0.063 and more); the bundled JetBrains Mono's is 0 at 24 px and over 0.14 at 12 and 14 px.
/// A share that covers 26 px's `_` leaves Default Light's glass mud (`light-mud-census`), so the
/// glass is solved for the faces that keep this share and draws on no other.
pub const THIN_INK_SHARE: f64 = 0.007;

/// How far the dimmest solid ink of a glyph, its ink of luminance `ink_y` on a page of luminance
/// `page_y`, lies from the ink toward its ground, as a share of linear light, when its darkest
/// pixel lies `edge` of the way (`0`: a glyph that lays its full ink): the renderer lays a
/// glyph's edge as the ink and its ground mixed in linear light, so a pixel that far toward the
/// page reads [`SOLID_SHARE`] of the darkest pixel's ratio there, the least the judge counts as
/// solid. `0` when the ink is not the darker.
#[inline]
#[must_use]
pub fn rim_share(ink_y: f64, page_y: f64, edge: f64) -> f64 {
    if page_y <= ink_y {
        return 0.0;
    }
    ((ink_y + 0.05) * (1.0 / SOLID_SHARE - 1.0) / (page_y - ink_y) + edge / SOLID_SHARE)
        .clamp(0.0, 1.0)
}

/// What a glyph whose darkest pixel lies `edge` of the way to the page reads over a ground of
/// luminance `ground_y`, its ink of luminance `ink_y` set on a page of luminance `page_y`, as the
/// judge reads it (`aterm-lab trail-census`'s glyph minimum): its dimmest solid ink,
/// [`rim_share`] of the way to the ground, against the ground.
#[inline]
#[must_use]
pub fn seen_edge_y(ink_y: f64, page_y: f64, ground_y: f64, edge: f64) -> f64 {
    let rim = ink_y + rim_share(ink_y, page_y, edge) * (ground_y - ink_y);
    contrast_y(rim, ground_y)
}

/// What the window's thinnest glyph reads over a ground of luminance `ground_y`, its ink of
/// luminance `ink_y` set on a page of luminance `page_y`: [`seen_edge_y`] at
/// [`THIN_INK_SHARE`], the glyph as it is seen. Every bar the glass keeps is read here.
#[inline]
#[must_use]
pub fn seen_y(ink_y: f64, page_y: f64, ground_y: f64) -> f64 {
    seen_edge_y(ink_y, page_y, ground_y, THIN_INK_SHARE)
}

/// [`seen_y`] of three colours: the ink `ink` set on `page`, over `ground`.
#[must_use]
pub fn seen(ink: u32, page: u32, ground: u32) -> f64 {
    seen_y(luminance(ink), luminance(page), luminance(ground))
}

/// The ground luminance at which [`seen_y`] reaches `thr` for an ink of luminance `ink_y` on a
/// page of luminance `page_y`: a lighter ground reads more, a darker one less (`seen_y` rises
/// with the ground over the ink). `INFINITY` when no ground reaches it.
#[inline]
#[must_use]
pub fn seen_floor_y(ink_y: f64, page_y: f64, thr: f64) -> f64 {
    let s = rim_share(ink_y, page_y, THIN_INK_SHARE);
    let room = 1.0 - thr * s;
    if room <= 0.0 {
        return f64::INFINITY;
    }
    (thr * (ink_y * (1.0 - s) + 0.05) - 0.05) / room
}
