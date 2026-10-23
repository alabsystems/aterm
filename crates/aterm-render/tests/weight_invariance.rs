// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Verifies that linear-corrected text blending preserves perceived stroke weight across theme
//! polarities.

use aterm_render::blend_text;

const POLES: &[(u32, u32)] = &[(0, 255), (16, 240), (24, 216), (40, 200)];

fn gray(b: u32) -> u32 {
    (b << 16) | (b << 8) | b
}

fn chan(px: u32) -> u32 {
    px & 0xff
}

fn perceived_weight(result: u32, bg: u32, fg: u32) -> f32 {
    (result as f32 - bg as f32) / (fg as f32 - bg as f32)
}

#[test]
fn corrected_perceived_weight_is_polarity_invariant() {
    let mut mid_weight_seen = false; // non-vacuity: a genuinely mid blend occurred
    let mut checked = 0u64;
    for &(lo, hi) in POLES {
        let contrast = (hi - lo) as f32;
        let tol = 3.0 / contrast + 0.004;
        for t in 1u32..=254 {
            let a = t as f32 / 255.0;
            let dark = chan(blend_text(gray(lo), gray(hi), gray(lo), t as u8, true));
            let w_dark = perceived_weight(dark, lo, hi);
            let light = chan(blend_text(gray(hi), gray(lo), gray(hi), t as u8, true));
            let w_light = perceived_weight(light, hi, lo);

            assert!(
                (w_dark - w_light).abs() <= tol,
                "polarity broke weight: poles=({lo},{hi}) t={t} \
                 w_dark={w_dark:.4} w_light={w_light:.4} tol={tol:.4}"
            );
            assert!(
                (w_dark - a).abs() <= tol && (w_light - a).abs() <= tol,
                "corrected weight must track coverage a={a:.4}: poles=({lo},{hi}) \
                 t={t} w_dark={w_dark:.4} w_light={w_light:.4}"
            );
            if t == 128 {
                assert!(
                    (0.35..=0.65).contains(&w_dark),
                    "non-vacuity: half-coverage must be a genuinely MID weight, \
                     got {w_dark:.4} at poles=({lo},{hi})"
                );
                mid_weight_seen = true;
            }
            checked += 1;
        }
    }
    assert!(mid_weight_seen, "non-vacuity control never fired");
    assert!(checked >= 1000, "lattice must be dense ({checked})");
}
