// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CROSS-CUTTING THEOREM (b) — WEIGHT INVARIANCE ACROSS POLARITY.
//!
//! The W2 linear-corrected coverage pipeline ([`aterm_render::blend_text`] with
//! `corrected = true`, the DEFAULT `text_blending = "linear-corrected"`) exists
//! so a glyph's APPARENT STROKE WEIGHT does not depend on the theme polarity: a
//! mid-coverage edge texel of light text on a dark background must read as the
//! same fraction of ink as the mirror-image dark text on a light background.
//! Pre-W2 (pure linear-light alpha-over) that symmetry was broken — 50%
//! coverage displayed at ~sRGB 188 in BOTH polarities, so on dark it read as
//! ~74% ink and on light as ~26% ink: dark-mode text looked systematically
//! bolder than the same face on a light theme.
//!
//! This is the theorem NO single audit item owns: W2 owns the remap's float
//! laws (range / monotone / degenerate guard, in `text_blending.rs`); THIS file
//! composes them into the end-user guarantee — *perceived weight is invariant
//! under fg↔bg swap* — over an exhaustive coverage × polarity × contrast
//! lattice, driving the SHIPPING blend.
//!
//! ## Why an L0 lattice test (ty-waiver)
//!
//! The perceived-weight metric is `(result − bg) / (fg − bg)` in sRGB space and
//! the pipeline itself is piecewise-`powf` sRGB (de)linearization — neither the
//! division nor the gamma curve is expressible in the `ty` Expr language (no
//! `*` / `/` / `pow`). Per the repo's box-drawing-rounding precedent, the
//! machine check for such an arithmetic law is an exhaustive/dense lattice test
//! under plain `cargo test`. The BOOLEAN gate that turns the remap on (`Linear`
//! vs `LinearCorrected`, endpoint exactness) is the part carried by `ty`
//! (`text_blend_gate_model`); this file carries the SCALING invariant.

use aterm_render::blend_text;

/// Grayscale theme poles `(lo, hi)` as sRGB BYTES, `hi − lo` the contrast.
/// Grayscale keeps the per-channel blend equal to its luminance (all channels
/// share the byte), so the perceived-weight metric is exact and the fg↔bg mirror
/// is clean. Contrast spans generous (0/255) down to a modest 160.
const POLES: &[(u32, u32)] = &[(0, 255), (16, 240), (24, 216), (40, 200)];

/// A grayscale sRGB PIXEL `0x00GGGGGG` from a channel byte.
fn gray(b: u32) -> u32 {
    (b << 16) | (b << 8) | b
}

/// The (shared) channel byte of a grayscale pixel.
fn chan(px: u32) -> u32 {
    px & 0xff
}

/// Perceived weight of a blend `result` between `bg` and `fg` (all sRGB bytes):
/// the fraction of the bg→fg travel the result reached, in sRGB (perceptual)
/// space. `1.0` = full ink, `0.0` = bare background.
fn perceived_weight(result: u32, bg: u32, fg: u32) -> f32 {
    (result as f32 - bg as f32) / (fg as f32 - bg as f32)
}

/// THE THEOREM: for every coverage and every contrast, the corrected pipeline's
/// perceived weight in a DARK theme (light ink `hi` on dark bg `lo`) equals the
/// perceived weight of the MIRRORED LIGHT theme (dark ink `lo` on light bg `hi`)
/// within a tight, contrast-scaled tolerance — and both track the input
/// coverage. The apparent boldness of a face is polarity-independent.
#[test]
fn corrected_perceived_weight_is_polarity_invariant() {
    let mut mid_weight_seen = false; // non-vacuity: a genuinely mid blend occurred
    let mut checked = 0u64;
    for &(lo, hi) in POLES {
        let contrast = (hi - lo) as f32;
        // One byte of rounding on each side is ±1/contrast of weight; allow that
        // plus a hair for the two independent roundings and the LUT step.
        let tol = 3.0 / contrast + 0.004;
        for t in 1u32..=254 {
            let a = t as f32 / 255.0;
            // Dark theme: bright ink over dark cell.
            let dark = chan(blend_text(gray(lo), gray(hi), gray(lo), t as u8, true));
            let w_dark = perceived_weight(dark, lo, hi);
            // Light theme: the exact fg↔bg mirror — dark ink over bright cell.
            let light = chan(blend_text(gray(hi), gray(lo), gray(hi), t as u8, true));
            let w_light = perceived_weight(light, hi, lo);

            assert!(
                (w_dark - w_light).abs() <= tol,
                "polarity broke weight: poles=({lo},{hi}) t={t} \
                 w_dark={w_dark:.4} w_light={w_light:.4} tol={tol:.4}"
            );
            // Both track the perceptual coverage (that IS the point of the remap:
            // the sRGB-space blend position equals the coverage), so neither
            // polarity is a washed-out or over-inked outlier.
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

/// NEGATIVE CONTROL — the pre-W2 defect. The UNCORRECTED linear-light pipeline
/// (`corrected = false`) is polarity-ASYMMETRIC at mid coverage: the identical
/// 50% texel reads far bolder on dark than on light. Reproduce that gap, then
/// show the corrected pipeline collapses it — so the test genuinely discriminates
/// the fix from the bug.
#[test]
fn uncorrected_pipeline_is_polarity_asymmetric() {
    let (lo, hi) = (0u32, 255u32);
    let t = 128u8;

    // Uncorrected: physical linear-light midpoint re-encodes to ~sRGB 188 in
    // both polarities, so the perceived weights sit on opposite sides of 0.5.
    let dark_lin = chan(blend_text(gray(lo), gray(hi), gray(lo), t, false));
    let light_lin = chan(blend_text(gray(hi), gray(lo), gray(hi), t, false));
    let gap_lin = (perceived_weight(dark_lin, lo, hi) - perceived_weight(light_lin, hi, lo)).abs();
    assert!(
        gap_lin > 0.30,
        "the pre-fix defect must be reproduced: uncorrected polarity gap {gap_lin:.4} \
         should be large (dark={dark_lin} light={light_lin})"
    );

    // Corrected: the same swap is invariant — the gap all but vanishes.
    let dark_c = chan(blend_text(gray(lo), gray(hi), gray(lo), t, true));
    let light_c = chan(blend_text(gray(hi), gray(lo), gray(hi), t, true));
    let gap_c = (perceived_weight(dark_c, lo, hi) - perceived_weight(light_c, hi, lo)).abs();
    assert!(
        gap_c < 0.02,
        "corrected polarity gap {gap_c:.4} must be near zero (dark={dark_c} light={light_c})"
    );
    assert!(
        gap_c < gap_lin,
        "the correction must strictly reduce the polarity gap ({gap_c:.4} vs {gap_lin:.4})"
    );
}
