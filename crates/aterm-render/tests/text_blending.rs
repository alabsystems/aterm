// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The text-blend seam: [`aterm_render::blend_text`] and its perceptual alpha
//! remap [`aterm_render::correct_alpha`].
//!
//! This file checks the correction gate's consequences on the shipping
//! byte-level seam (endpoint texels are never remapped; a degenerate luminance
//! gap reduces to plain linear). The remap's float laws (range, monotonicity,
//! degenerate guard, endpoints) are beyond the ty expression language (no
//! multiplication or `pow`), so they are swept exhaustively here.

use aterm_render::{TEXT_BLEND_EPS, blend_text, correct_alpha};

/// Reference sRGB EOTF (gamma → linear), the proper piecewise curve —
/// duplicated from the crate so the fixtures don't trust the code under test.
fn s2l(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Reference BT.709 relative luminance of a packed 0x00RRGGBB pixel, in
/// linear light.
fn ref_luminance(rgb: u32) -> f32 {
    let ch = |sh: u32| s2l(((rgb >> sh) & 0xff) as f32 / 255.0);
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// A deliberate colour lattice: channel extremes, near-extremes, and midtones,
/// mixed across channels — the fg/bg pairs the endpoint sweep runs over.
fn colour_lattice() -> Vec<u32> {
    let bytes = [0u32, 1, 63, 127, 128, 191, 254, 255];
    let mut v: Vec<u32> = bytes.iter().map(|&b| (b << 16) | (b << 8) | b).collect();
    // Chromatic corners and colours the renderer blends.
    v.extend_from_slice(&[
        0x00ff_0000,
        0x0000_ff00,
        0x0000_00ff,
        0x00ff_ff00,
        0x0000_ffff,
        0x00ff_00ff,
        0x0011_1318, // Theme::default().bg
        0x00c8_ccd4,
        0x007f_0040,
        0x0040_7f00,
    ]);
    v
}

/// `t == 0` returns the destination bytes and `t == 255` the fg bytes, exactly,
/// in both modes for every lattice pair, with dirty high bytes masked. The
/// early returns run before any float math.
#[test]
fn endpoint_exactness_both_modes() {
    let lattice = colour_lattice();
    let mut cases = 0usize;
    for &fg in &lattice {
        for &bg in &lattice {
            // Dirty high bytes must be masked away, never leak through.
            let (fg_dirty, bg_dirty) = (fg | 0xaa00_0000, bg | 0x5500_0000);
            for corrected in [false, true] {
                assert_eq!(
                    blend_text(bg_dirty, fg_dirty, bg_dirty, 0, corrected),
                    bg & 0x00ff_ffff,
                    "t=0 must return dst exactly (fg={fg:#08x} bg={bg:#08x} corrected={corrected})"
                );
                assert_eq!(
                    blend_text(bg_dirty, fg_dirty, fg_dirty, 255, corrected),
                    fg & 0x00ff_ffff,
                    "t=255 must return fg exactly (fg={fg:#08x} bg={bg:#08x} corrected={corrected})"
                );
                cases += 2;
            }
        }
    }
    assert!(cases > 1000, "endpoint sweep too thin ({cases} cases)");
}

/// A 50%-coverage white texel on black lands at ~sRGB 128 in the default
/// corrected mode (the apparent weight of a gamma-space, CoreText-like blend)
/// and at ~sRGB 188 in pure linear, the negative control.
#[test]
fn golden_half_coverage_white_on_black() {
    let corrected = blend_text(0x0000_0000, 0x00ff_ffff, 0x0000_0000, 128, true);
    let linear = blend_text(0x0000_0000, 0x00ff_ffff, 0x0000_0000, 128, false);
    for sh in [16u32, 8, 0] {
        let c = (corrected >> sh) & 0xff;
        let l = (linear >> sh) & 0xff;
        assert!(
            (126..=130).contains(&c),
            "corrected 50% white-on-black must be ~sRGB 128, got {c} (pixel {corrected:#08x})"
        );
        assert!(
            (186..=190).contains(&l),
            "pure-linear 50% white-on-black must be ~sRGB 188, got {l} (pixel {linear:#08x})"
        );
    }
}

/// The remapped alpha is in `[0, 1]` and nondecreasing in raw coverage for all
/// 33×33 luminance pairs (i/32) × 256 coverage steps, anchored at both ends,
/// and it moves midtones (50% white-on-black by more than 0.1).
#[test]
fn corrected_alpha_range_monotone_and_anchored() {
    for i in 0..=32u32 {
        for j in 0..=32u32 {
            let fg_l = i as f32 / 32.0;
            let bg_l = j as f32 / 32.0;
            let mut prev = -1.0f32;
            for t in 0..=255u32 {
                let a = t as f32 / 255.0;
                let ac = correct_alpha(fg_l, bg_l, a);
                assert!(
                    (0.0..=1.0).contains(&ac),
                    "a_corr out of [0,1]: {ac} @ fg_l={fg_l} bg_l={bg_l} a={a}"
                );
                assert!(
                    ac >= prev,
                    "a_corr not monotone: {ac} < {prev} @ fg_l={fg_l} bg_l={bg_l} a={a}"
                );
                prev = ac;
            }
            // Identity at the ends, up to the sRGB round-trip's float error
            // (`blend_text`'s early returns make the bytes exact).
            assert!(
                correct_alpha(fg_l, bg_l, 0.0).abs() < 1e-3,
                "a=0 must map near 0 @ fg_l={fg_l} bg_l={bg_l}"
            );
            assert!(
                (correct_alpha(fg_l, bg_l, 1.0) - 1.0).abs() < 1e-3,
                "a=1 must map near 1 @ fg_l={fg_l} bg_l={bg_l}"
            );
        }
    }
    // Non-vacuity: 50% white-on-black moves from 0.502 to ~0.216.
    let moved = correct_alpha(1.0, 0.0, 0.5);
    assert!(
        (0.5 - moved) > 0.1,
        "remap must genuinely reweight midtones (got {moved})"
    );
}

/// A luminance gap under [`TEXT_BLEND_EPS`] returns the raw coverage bit for
/// bit (no division runs), so corrected mode then equals plain linear at every
/// coverage, including pure red against the gray of equal luminance.
#[test]
fn degenerate_guard_reduces_to_linear() {
    // Float level: |fg_l - bg_l| < eps ⇒ identity, bit-for-bit.
    for i in 0..=64u32 {
        let l = i as f32 / 64.0;
        let l_near = (l + TEXT_BLEND_EPS * 0.99).min(1.0);
        for t in 0..=255u32 {
            let a = t as f32 / 255.0;
            assert_eq!(
                correct_alpha(l, l, a).to_bits(),
                a.to_bits(),
                "equal luminances must return a unchanged"
            );
            if (l_near - l).abs() < TEXT_BLEND_EPS {
                assert_eq!(
                    correct_alpha(l, l_near, a).to_bits(),
                    a.to_bits(),
                    "near-equal luminances (within eps) must return a unchanged"
                );
            }
        }
    }
    // Byte level: pure red vs its equal-luminance gray, checked degenerate by
    // the independent reference.
    let (red, gray) = (0x00ff_0000u32, 0x007f_7f7fu32);
    assert!(
        (ref_luminance(red) - ref_luminance(gray)).abs() < TEXT_BLEND_EPS,
        "fixture: red/gray pair must be luminance-degenerate"
    );
    for t in 0..=255u8 {
        assert_eq!(
            blend_text(gray, red, gray, t, true),
            blend_text(gray, red, gray, t, false),
            "degenerate pair must blend identically in both modes (t={t})"
        );
    }
}
