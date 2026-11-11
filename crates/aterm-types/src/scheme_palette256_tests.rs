// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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

/// Spec `scheme-generated-palette`: the cube's eight corners are the
/// theme's anchors and bright row, byte for byte, and a light theme's
/// white corner is its page while its black corner stays dark ink.
/// Negative control: with the flag off, slot 196 is xterm's `#FF0000`,
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
    let generated = generated_extended_palette(&scheme.ansi, scheme.foreground, scheme.background);
    generated[216..].iter().map(|c| lightness(*c)).collect()
}

fn rising(ls: &[f64]) -> bool {
    ls.windows(2).all(|w| w[0] < w[1])
}

/// Spec `scheme-generated-palette`: the grey ramp rises in lightness from
/// the dark anchor to the light one on every theme, light ones included,
/// and swapping `color0` and `color15` leaves it unchanged. NEGATIVE
/// CONTROL: the naive `color0` → `color15` ramp on Aterm Day (whose
/// bright white is its darkest ink) does not rise, so the test sees
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
