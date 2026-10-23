// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WCAG 2.0 contrast ratio tests for `Rgb::contrast()`.

use aterm_types::Rgb;

#[test]
fn contrast_known_values() {
    let ratio = Rgb::new(255, 255, 255).contrast(Rgb::new(0, 0, 0));
    assert!((ratio - 21.0).abs() < 0.01, "white/black: {ratio}");
    let ratio = Rgb::new(128, 64, 32).contrast(Rgb::new(128, 64, 32));
    assert!((ratio - 1.0).abs() < 1e-10, "same color: {ratio}");
    let (a, b) = (Rgb::new(200, 100, 50), Rgb::new(50, 100, 200));
    assert!(
        (a.contrast(b) - b.contrast(a)).abs() < 1e-10,
        "contrast is symmetric"
    );
}

#[test]
fn contrast_wcag_aa_boundary() {
    let white = Rgb::new(255, 255, 255);
    let ratio76 = white.contrast(Rgb::new(0x76, 0x76, 0x76));
    assert!(ratio76 >= 4.5, "#767676 on white passes AA ({ratio76})");
    let ratio77 = white.contrast(Rgb::new(0x77, 0x77, 0x77));
    assert!(ratio77 < 4.5, "#777777 on white fails AA ({ratio77})");
}

fn luminance_via_contrast(c: Rgb) -> f64 {
    c.contrast(Rgb::new(0, 0, 0)) * 0.05 - 0.05
}

#[test]
fn luminance_reference_values() {
    for (what, rgb, want, tolerance) in [
        ("black", Rgb::new(0, 0, 0), 0.0, 1e-10),
        ("white", Rgb::new(255, 255, 255), 1.0, 1e-10),
        ("mid gray", Rgb::new(128, 128, 128), 0.215_860_5, 0.001),
        ("red", Rgb::new(255, 0, 0), 0.2126, 1e-4),
        ("green", Rgb::new(0, 255, 0), 0.7152, 1e-4),
        ("blue", Rgb::new(0, 0, 255), 0.0722, 1e-4),
    ] {
        let lum = luminance_via_contrast(rgb);
        assert!((lum - want).abs() < tolerance, "{what}: {lum} vs {want}");
    }
}

#[test]
fn luminance_monotonic_for_grays() {
    let mut prev = 0.0_f64;
    for v in 1..=255u8 {
        let lum = luminance_via_contrast(Rgb::new(v, v, v));
        assert!(
            lum > prev,
            "luminance should increase: sRGB {v} gave {lum}, previous was {prev}"
        );
        prev = lum;
    }
}
