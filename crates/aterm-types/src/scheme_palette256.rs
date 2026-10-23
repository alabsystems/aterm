// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The 256-colour palette a theme derives from its own colours.

use crate::Rgb;
use crate::scheme_signature::oklab;

/// xterm's six cube levels per channel (`0` then `55 + 40·v`).
pub const XTERM_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// How many slots the derivation fills: 16 through 255.
pub const EXTENDED_SLOTS: usize = 240;

/// bright yellow.
const CORNER_SLOTS: [usize; 8] = [0, 12, 10, 14, 9, 13, 11, 15];

/// The derivation's dark and light anchors.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn palette_anchors(ansi: &[Rgb; 16], foreground: Rgb, background: Rgb) -> (Rgb, Rgb) {
    let l = |c: Rgb| oklab(c).0;
    let dark = if l(ansi[15]) < l(ansi[0]) {
        ansi[15]
    } else {
        ansi[0]
    };
    let mut light = ansi[0];
    for c in [ansi[7], ansi[15], foreground, background] {
        if l(c) > l(light) {
            light = c;
        }
    }
    (dark, light)
}

/// Slots 16-255 derived from a theme's colours: element `k` is slot `16 + k`.
#[must_use]
pub fn generated_extended_palette(
    ansi: &[Rgb; 16],
    foreground: Rgb,
    background: Rgb,
) -> [Rgb; EXTENDED_SLOTS] {
    let (dark, light) = palette_anchors(ansi, foreground, background);
    let mut corners: [Rgb; 8] = CORNER_SLOTS.map(|slot| ansi[slot & 0xF]);
    corners[0] = dark;
    corners[7] = light;
    let labs = corners.map(oklab);
    let mut out = [Rgb::new(0, 0, 0); EXTENDED_SLOTS];
    for (k, slot) in out.iter_mut().enumerate() {
        *slot = if k < 216 {
            let (r, g, b) = (k / 36, (k % 36) / 6, k % 6);
            let edge = |v: usize| v == 0 || v == 5;
            if edge(r) && edge(g) && edge(b) {
                let bits =
                    (usize::from(r == 5) << 2) | (usize::from(g == 5) << 1) | usize::from(b == 5);
                corners[bits & 7]
            } else {
                cube_slot(&labs, [r % 6, g % 6, b % 6])
            }
        } else {
            grey_slot(&labs[0], &labs[7], (k - 216) % 24)
        };
    }
    out
}

/// A cube level's interpolation fraction.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn level_t(v: usize) -> f64 {
    f64::from(XTERM_CUBE_LEVELS[v % 6]) / 255.0
}

/// The trilinear `OkLab` blend of the eight corners at cube levels `rgb`.
#[cfg_attr(trust_verify, trust::skip)]
fn cube_slot(labs: &[(f64, f64, f64); 8], rgb: [usize; 3]) -> Rgb {
    let t = rgb.map(level_t);
    let mut acc = (0.0, 0.0, 0.0);
    for (bits, lab) in labs.iter().enumerate() {
        let weight = |axis: usize, bit: usize| {
            if bits >> bit & 1 == 1 {
                t[axis]
            } else {
                1.0 - t[axis]
            }
        };
        let w = weight(0, 2) * weight(1, 1) * weight(2, 0);
        acc.0 += w * lab.0;
        acc.1 += w * lab.1;
        acc.2 += w * lab.2;
    }
    oklab_to_rgb(acc)
}

/// Grey slot `232 + i`: `ansi[0]` to `ansi[15]` at xterm's grey level.
#[cfg_attr(trust_verify, trust::skip)]
fn grey_slot(black: &(f64, f64, f64), white: &(f64, f64, f64), i: usize) -> Rgb {
    let level = 8.0 + 10.0 * f64::from(u8::try_from(i).unwrap_or(23));
    let t = level / 255.0;
    let mix = |a: f64, b: f64| a + (b - a) * t;
    oklab_to_rgb((
        mix(black.0, white.0),
        mix(black.1, white.1),
        mix(black.2, white.2),
    ))
}

/// Linear sRGB of an `OkLab` colour (Björn Ottosson's published inverse).
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "the published OkLab inverse matrices, digit for digit"
)]
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn oklab_to_linear(lab: (f64, f64, f64)) -> (f64, f64, f64) {
    let (l, a, b) = lab;
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    (
        4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
        -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
        -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3,
    )
}

/// gamut: float noise on a colour that round-trips, far under half a step.
const GAMUT_EPSILON: f64 = 1e-7;

/// Bisection steps toward the largest in-gamut chroma.
const CHROMA_STEPS: u32 = 20;

/// then encoded and rounded half up.
#[cfg_attr(trust_verify, trust::skip)]
fn oklab_to_rgb(lab: (f64, f64, f64)) -> Rgb {
    let in_gamut = |c: (f64, f64, f64)| {
        [c.0, c.1, c.2]
            .iter()
            .all(|v| (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(v))
    };
    let at = |scale: f64| oklab_to_linear((lab.0, lab.1 * scale, lab.2 * scale));
    let mut linear = at(1.0);
    if !in_gamut(linear) {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..CHROMA_STEPS {
            let mid = (lo + hi) / 2.0;
            if in_gamut(at(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        linear = at(lo);
    }
    let encode = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to [0, 255] first"
        )]
        let byte = (s * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
        byte
    };
    Rgb::new(encode(linear.0), encode(linear.1), encode(linear.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorPalette;
    use crate::scheme::{ColorScheme, builtin, builtin_names};

    fn hand_made() -> [ColorScheme; 2] {
        let warm = ColorScheme {
            name: "warm".into(),
            ansi: [
                Rgb::new(0x1c, 0x17, 0x14),
                Rgb::new(0xb0, 0x40, 0x30),
                Rgb::new(0x60, 0x90, 0x40),
                Rgb::new(0xc0, 0x90, 0x30),
                Rgb::new(0x40, 0x60, 0xa0),
                Rgb::new(0x90, 0x50, 0x90),
                Rgb::new(0x40, 0x90, 0x90),
                Rgb::new(0xc8, 0xc0, 0xb0),
                Rgb::new(0x50, 0x48, 0x40),
                Rgb::new(0xe8, 0x60, 0x50),
                Rgb::new(0x80, 0xc0, 0x60),
                Rgb::new(0xf0, 0xc0, 0x50),
                Rgb::new(0x60, 0x90, 0xe0),
                Rgb::new(0xc0, 0x80, 0xd0),
                Rgb::new(0x60, 0xc8, 0xc8),
                Rgb::new(0xfa, 0xf4, 0xea),
            ],
            ..ColorScheme::default()
        };
        let mut saturated = ColorScheme {
            name: "saturated".into(),
            ..ColorScheme::default()
        };
        saturated.ansi[0] = Rgb::new(0x20, 0x20, 0x20);
        saturated.ansi[9] = Rgb::new(0xff, 0x00, 0x00);
        saturated.ansi[10] = Rgb::new(0x00, 0xff, 0x00);
        saturated.ansi[11] = Rgb::new(0xff, 0xff, 0x00);
        saturated.ansi[12] = Rgb::new(0x00, 0x00, 0xff);
        saturated.ansi[13] = Rgb::new(0xff, 0x00, 0xff);
        saturated.ansi[14] = Rgb::new(0x00, 0xff, 0xff);
        saturated.ansi[15] = Rgb::new(0xf0, 0xf0, 0xf0);
        [warm, saturated]
    }

    fn every_scheme() -> Vec<ColorScheme> {
        let mut all: Vec<ColorScheme> = builtin_names()
            .iter()
            .map(|name| builtin(name).expect("a built-in name resolves"))
            .collect();
        all.extend(hand_made());
        all
    }

    /// The cube's colour corners and the ANSI slot each is.
    const COLOUR_CORNERS: [(u8, usize); 6] =
        [(196, 9), (46, 10), (226, 11), (21, 12), (201, 13), (51, 14)];

    /// not the theme's bright red.
    #[test]
    fn generated_corners_are_the_theme_anchors() {
        for mut scheme in every_scheme() {
            scheme.palette_generate = true;
            let palette = scheme.to_color_palette();
            let (dark, light) = palette_anchors(&scheme.ansi, scheme.foreground, scheme.background);
            assert_eq!(palette.get(16), dark, "{}", scheme.name);
            assert_eq!(palette.get(231), light, "{}", scheme.name);
            for (slot, anchor) in COLOUR_CORNERS {
                assert_eq!(
                    palette.get(slot),
                    scheme.ansi[anchor],
                    "{} slot {slot}",
                    scheme.name
                );
            }
            scheme.palette_generate = false;
            let plain = scheme.to_color_palette();
            assert_eq!(
                plain.get(196),
                ColorPalette::default_color(196),
                "{}",
                scheme.name
            );
        }
        let night = builtin("Aterm Night").expect("a signature theme");
        assert_eq!(
            palette_anchors(&night.ansi, night.foreground, night.background).0,
            night.ansi[0]
        );
        let day = builtin("Aterm Day").expect("a signature theme");
        let (dark, light) = palette_anchors(&day.ansi, day.foreground, day.background);
        assert_eq!(light, day.background);
        assert!(lightness(dark) < 0.4, "{dark:?}");
        let [warm, _] = hand_made();
        assert_ne!(warm.ansi[9], ColorPalette::default_color(196));
    }

    fn lightness(c: Rgb) -> f64 {
        oklab(c).0
    }

    fn ramp_ls(scheme: &ColorScheme) -> Vec<f64> {
        let generated =
            generated_extended_palette(&scheme.ansi, scheme.foreground, scheme.background);
        generated[216..].iter().map(|c| lightness(*c)).collect()
    }

    fn rising(ls: &[f64]) -> bool {
        ls.windows(2).all(|w| w[0] < w[1])
    }

    /// direction.
    #[test]
    fn grey_ramp_follows_black_to_white() {
        let mut checked = 0;
        for scheme in every_scheme() {
            let ls = ramp_ls(&scheme);
            assert!(rising(&ls), "{}: {ls:?}", scheme.name);
            let mut swapped = scheme.clone();
            swapped.ansi.swap(0, 15);
            assert_eq!(ramp_ls(&swapped), ls, "{}", scheme.name);
            checked += 1;
        }
        assert!(checked > 4);
        let day = builtin("Aterm Day").expect("a signature theme");
        let (black, white) = (oklab(day.ansi[0]), oklab(day.ansi[15]));
        let naive: Vec<f64> = (0..24)
            .map(|i| lightness(grey_slot(&black, &white, i)))
            .collect();
        assert!(!rising(&naive), "{naive:?}");
    }

    /// Spec `scheme-generated-palette`: the derivation is deterministic.
    #[test]
    fn generated_palette_is_deterministic() {
        for mut scheme in every_scheme() {
            scheme.palette_generate = true;
            let once =
                || generated_extended_palette(&scheme.ansi, scheme.foreground, scheme.background);
            assert_eq!(once(), once());
            let (a, b) = (scheme.to_color_palette(), scheme.to_color_palette());
            for i in 0..=255u8 {
                assert_eq!(a.get(i), b.get(i), "{} slot {i}", scheme.name);
            }
        }
    }
}
