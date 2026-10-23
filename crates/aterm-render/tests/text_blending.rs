// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for [`aterm_render::blend_text`] and [`aterm_render::correct_alpha`].

use aterm_render::{TEXT_BLEND_EPS, blend_text, correct_alpha};

fn s2l(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn ref_luminance(rgb: u32) -> f32 {
    let ch = |sh: u32| s2l(((rgb >> sh) & 0xff) as f32 / 255.0);
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

fn colour_lattice() -> Vec<u32> {
    let bytes = [0u32, 1, 63, 127, 128, 191, 254, 255];
    let mut v: Vec<u32> = bytes.iter().map(|&b| (b << 16) | (b << 8) | b).collect();
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

#[test]
fn endpoint_exactness_both_modes() {
    let lattice = colour_lattice();
    let mut cases = 0usize;
    for &fg in &lattice {
        for &bg in &lattice {
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
    let moved = correct_alpha(1.0, 0.0, 0.5);
    assert!(
        (0.5 - moved) > 0.1,
        "remap must genuinely reweight midtones (got {moved})"
    );
}

#[test]
fn degenerate_guard_reduces_to_linear() {
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
