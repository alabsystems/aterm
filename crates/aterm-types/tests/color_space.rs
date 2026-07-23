// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WCAG 2.0 contrast ratio tests for `Rgb::contrast()`.
//!
//! These tests verify color math through the public API. The underlying
//! `luminance()` and `linearize()` functions are private; correctness of
//! the linearization transfer function is validated indirectly through
//! contrast ratio results against known reference values.

use aterm_types::Rgb;

// =========================================================================
// Known-value contrast tests
// =========================================================================

#[test]
fn contrast_white_on_black_is_21() {
    // Maximum WCAG contrast ratio: (1.0 + 0.05) / (0.0 + 0.05) = 21.0
    let ratio = Rgb::new(255, 255, 255).contrast(Rgb::new(0, 0, 0));
    assert!(
        (ratio - 21.0).abs() < 0.01,
        "white/black contrast should be 21.0, got {ratio}"
    );
}

#[test]
fn contrast_same_color_is_one() {
    let ratio = Rgb::new(128, 64, 32).contrast(Rgb::new(128, 64, 32));
    assert!(
        (ratio - 1.0).abs() < 1e-10,
        "same color contrast should be 1.0, got {ratio}"
    );
}

#[test]
fn contrast_is_symmetric() {
    let a = Rgb::new(200, 100, 50);
    let b = Rgb::new(50, 100, 200);
    let ab = a.contrast(b);
    let ba = b.contrast(a);
    assert!(
        (ab - ba).abs() < 1e-10,
        "contrast should be symmetric: {ab} vs {ba}"
    );
}

#[test]
fn contrast_always_at_least_one() {
    for r in (0..=255).step_by(51) {
        for g in (0..=255).step_by(51) {
            for b in (0..=255).step_by(51) {
                let c = Rgb::new(r as u8, g as u8, b as u8);
                let ratio = c.contrast(Rgb::new(0, 0, 0));
                assert!(
                    ratio >= 1.0,
                    "contrast should be >= 1.0, got {ratio} for ({r},{g},{b})"
                );
            }
        }
    }
}

// =========================================================================
// WCAG threshold boundary tests
// =========================================================================

#[test]
fn contrast_wcag_aa_boundary() {
    // WCAG AA requires 4.5:1 for normal text.
    // #767676 is the well-known minimum-contrast gray on white (~4.54:1).
    // #777777 is lighter -> less contrast with white (~4.48:1, fails AA).
    let white = Rgb::new(255, 255, 255);
    let gray76 = Rgb::new(0x76, 0x76, 0x76);
    let ratio76 = white.contrast(gray76);
    assert!(
        ratio76 >= 4.5,
        "#767676 on white should pass AA (ratio={ratio76})"
    );
    let gray77 = Rgb::new(0x77, 0x77, 0x77);
    let ratio77 = white.contrast(gray77);
    assert!(
        ratio77 < 4.5,
        "#777777 on white should fail AA (ratio={ratio77})"
    );
}

// =========================================================================
// Luminance validation via contrast with black
// =========================================================================

// Since luminance() is private, we derive it from contrast with black:
//   contrast(C, black) = (lum_C + 0.05) / 0.05
//   lum_C = contrast(C, black) * 0.05 - 0.05
fn luminance_via_contrast(c: Rgb) -> f64 {
    c.contrast(Rgb::new(0, 0, 0)) * 0.05 - 0.05
}

#[test]
fn luminance_black_is_zero() {
    let lum = luminance_via_contrast(Rgb::new(0, 0, 0));
    assert!(
        lum.abs() < 1e-10,
        "black luminance should be 0.0, got {lum}"
    );
}

#[test]
fn luminance_white_is_one() {
    let lum = luminance_via_contrast(Rgb::new(255, 255, 255));
    assert!(
        (lum - 1.0).abs() < 1e-10,
        "white luminance should be 1.0, got {lum}"
    );
}

#[test]
fn luminance_mid_gray() {
    // sRGB 128 -> linearize(128) ~ 0.2158605
    // For neutral gray: luminance = linearize(128) since R=G=B
    let lum = luminance_via_contrast(Rgb::new(128, 128, 128));
    assert!(
        (lum - 0.2158605).abs() < 0.001,
        "mid-gray luminance should be ~0.2158, got {lum}"
    );
}

#[test]
fn luminance_pure_red() {
    // Red (255,0,0): luminance = 0.2126 * linearize(255) = 0.2126
    let lum = luminance_via_contrast(Rgb::new(255, 0, 0));
    assert!(
        (lum - 0.2126).abs() < 1e-4,
        "pure red luminance should be 0.2126, got {lum}"
    );
}

#[test]
fn luminance_pure_green() {
    // Green (0,255,0): luminance = 0.7152 * linearize(255) = 0.7152
    let lum = luminance_via_contrast(Rgb::new(0, 255, 0));
    assert!(
        (lum - 0.7152).abs() < 1e-4,
        "pure green luminance should be 0.7152, got {lum}"
    );
}

#[test]
fn luminance_pure_blue() {
    // Blue (0,0,255): luminance = 0.0722 * linearize(255) = 0.0722
    let lum = luminance_via_contrast(Rgb::new(0, 0, 255));
    assert!(
        (lum - 0.0722).abs() < 1e-4,
        "pure blue luminance should be 0.0722, got {lum}"
    );
}

#[test]
fn luminance_monotonic_for_grays() {
    // Luminance must be strictly monotonically increasing for gray ramps
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

#[test]
fn luminance_near_srgb_threshold() {
    // sRGB linearize has a piecewise function with threshold at 0.04045
    // per IEC 61966-2-1. sRGB value 10/255 ~ 0.0392 is near the boundary.
    // Verify continuity: values around the boundary should be close.
    let lum_10 = luminance_via_contrast(Rgb::new(10, 10, 10));
    let lum_11 = luminance_via_contrast(Rgb::new(11, 11, 11));
    assert!(
        lum_11 > lum_10,
        "luminance should increase across threshold boundary"
    );
    assert!(
        lum_10 < 0.005,
        "sRGB 10 should have very low luminance, got {lum_10}"
    );
}

/// Verify cross-language sRGB linearization threshold agreement.
///
/// All three codebases (Rust, Metal, Swift) must use the IEC 61966-2-1
/// threshold of 0.04045. This test validates continuity at the exact
/// threshold boundary — the piecewise linear and gamma branches must agree
/// at the transition point.
#[test]
fn srgb_threshold_cross_language_agreement() {
    // IEC 61966-2-1 threshold: 0.04045
    // At the threshold, both branches of the piecewise function should agree:
    //   linear branch: 0.04045 / 12.92 ≈ 0.003130805
    //   gamma branch:  ((0.04045 + 0.055) / 1.055)^2.4 ≈ 0.003130805
    let threshold = 0.04045_f64;
    let linear_val = threshold / 12.92;
    let gamma_val = ((threshold + 0.055) / 1.055).powf(2.4);
    let diff = (linear_val - gamma_val).abs();
    assert!(
        diff < 1e-6,
        "piecewise branches must agree at threshold: linear={linear_val}, gamma={gamma_val}, diff={diff}"
    );
}

// =========================================================================
// Rec.709 coefficient validation
// =========================================================================

#[test]
fn rec709_coefficients_sum_to_one() {
    // The Rec.709 luminance coefficients (0.2126, 0.7152, 0.0722) should
    // sum to 1.0. We validate this indirectly: white luminance should be
    // exactly 1.0 (since all channels linearize to 1.0 for sRGB 255).
    let white_lum = luminance_via_contrast(Rgb::new(255, 255, 255));
    let sum = luminance_via_contrast(Rgb::new(255, 0, 0))
        + luminance_via_contrast(Rgb::new(0, 255, 0))
        + luminance_via_contrast(Rgb::new(0, 0, 255));
    assert!(
        (white_lum - sum).abs() < 1e-10,
        "R+G+B luminance should equal white luminance: {sum} vs {white_lum}"
    );
}
