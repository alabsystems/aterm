// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! W10 (emoji strike selection + premultiplied area filter): the always-on
//! proofs of the three resampling laws, over exhaustive/lattice domains.
//!
//! ## Two-tier proof (strike selection)
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `StrikeSelection` derived model (`aterm_spec::derive::strike_selection_model`)
//!   carries `ChosenFromAvailable` + `ChosenAdequateMinimal` +
//!   `ChosenMaxWhenNoneAdequate`. `cargo test -p aterm-spec`
//!   (`derived_strike_selection_proves_and_catches_largest_strike_bias`) runs the
//!   REAL `ty` binary over the bounded state space: it PROVES the law at `Buggy=0`
//!   and CATCHES the pre-W10 always-largest (`u16::MAX`) request at `Buggy=1`.
//! * **Tier-1 (concrete, this file)** — the SAME law checked against the shipping
//!   [`aterm_render::select_strike_ppem`] by exhaustive enumeration of every
//!   strike multiset over a ppem lattice, for every target — a complete proof
//!   over the enumerated domain, with non-vacuity controls (both the adequate
//!   and the fallback arm are reached) and the Apple strike ladder spot-checked.
//!
//! ## Arithmetic laws (partition of unity, premultiply soundness)
//!
//! Products (`d*src`) are outside `ty`'s Expr language (no `*`), so the area
//! filter's laws have no derived model (documented WAIVER on
//! [`aterm_render::area_overlap`]): the always-on proof weight is the exhaustive
//! integer lattice here, redundantly deepened by the config-free `#[kani::proof]`
//! harnesses in aterm-render (`resample_kani`, a trust-mc `verify.sh --full`
//! lane) over the bit-precise bounded domain.

use aterm_render::{area_overlap, resample_rgba, select_strike_ppem};

/// The sRGB encoding of a linear-light value, as a byte — the test's own
/// independent transfer function (the proper piecewise OETF, not a LUT), so the
/// expectation does not share code with the implementation under test.
fn srgb_encode(l: f64) -> u8 {
    let c = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Tiny deterministic LCG so the soundness sweep is reproducible.
struct Lcg(u64);
impl Lcg {
    fn next_u8(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u8
    }
}

// ---------------------------------------------------------------------------
// PROVE (1): partition of unity — the checker theorem's weight half.
// ---------------------------------------------------------------------------

/// For every (src, dst) pair on the lattice and every destination texel `d`,
/// the integer footprint weights sum to EXACTLY `src` (so the `1/src`-
/// normalized weights sum to exactly 1: the filter can neither gain nor lose
/// energy). Dually, every source texel's mass is fully distributed
/// (`Σ_d == dst`). Exhaustive over 1..=48 × 1..=48 — covers odd/even, prime,
/// degenerate-1, up- and downscale; no sampling.
#[test]
fn area_weights_partition_of_unity_lattice() {
    for src in 1usize..=48 {
        for dst in 1usize..=48 {
            for d in 0..dst {
                let sum: u64 = (0..src).map(|s| area_overlap(d, s, src, dst)).sum();
                assert_eq!(
                    sum, src as u64,
                    "footprint weights must sum to src (src={src}, dst={dst}, d={d})"
                );
            }
            for s in 0..src {
                let sum: u64 = (0..dst).map(|d| area_overlap(d, s, src, dst)).sum();
                assert_eq!(
                    sum, dst as u64,
                    "texel mass must be fully distributed (src={src}, dst={dst}, s={s})"
                );
            }
        }
    }
    // NON-VACUOUS: weights are not trivially zero/constant — a 3→2 minification
    // splits the middle texel across both footprints (a genuine fractional edge).
    assert_eq!(area_overlap(0, 1, 3, 2), 1, "texel 1 of 3→2 leans into d=0");
    assert_eq!(
        area_overlap(1, 1, 3, 2),
        1,
        "…and into d=1 (split, not skipped)"
    );
}

/// THE CHECKER THEOREM, end to end: a 1px black/white opaque checkerboard
/// minified 4x must land UNIFORM linear-light mid-grey — every output texel the
/// sRGB encoding of linear 0.5 (~188), identical across the frame. The pre-W10
/// 2x2-tap bilinear filter sampled a phase-dependent subset of texels (0, 255,
/// or an arbitrary mix — the flag/keycap shimmer), and an sRGB-space average
/// would land at ~128 — both are ruled out explicitly.
#[test]
fn checkerboard_minified_4x_lands_uniform_linear_midgrey() {
    let (sw, sh) = (8usize, 8usize);
    let mut src = vec![0u8; sw * sh * 4];
    for y in 0..sh {
        for x in 0..sw {
            let v = if (x + y) % 2 == 0 { 255 } else { 0 };
            let i = (y * sw + x) * 4;
            src[i] = v;
            src[i + 1] = v;
            src[i + 2] = v;
            src[i + 3] = 255;
        }
    }
    let out = resample_rgba(&src, sw, sh, 2, 2);
    let expected = srgb_encode(0.5); // 188: the LINEAR average of black+white
    let first = out[0];
    for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
        for (c, &v) in px.iter().enumerate().take(3) {
            assert!(
                (i32::from(v) - i32::from(expected)).abs() <= 1,
                "texel {i} channel {c}: got {v}, want the sRGB encoding of linear 0.5 ({expected}±1)"
            );
            assert_eq!(
                v, first,
                "texel {i} must match texel 0 — UNIFORM, no phase shimmer"
            );
        }
        assert_eq!(px[3], 255, "an opaque source stays opaque");
    }
    // Negative controls: the gamma-space average (128) and the tap-skipping
    // extremes (0/255) — what the pre-W10 filter produced — are far away.
    assert!(
        (i32::from(first) - 128).abs() > 20,
        "output {first} is the too-dark sRGB-space average, not linear light"
    );
    assert!(
        first != 0 && first != 255,
        "output {first} is a skipped-tap extreme — texels were dropped"
    );
}

/// A solid image survives ANY ratio (up, mild down, heavy down — BOTH filter
/// arms) within 1 LSB: the end-to-end corollary of partition of unity through
/// the premultiply/unpremultiply bracket, including a semi-transparent alpha.
#[test]
fn solid_image_survives_any_ratio() {
    let cases: [(usize, usize, usize, usize); 6] = [
        (8, 8, 2, 2),  // heavy minification → area arm
        (9, 7, 3, 3),  // heavy, odd/prime dims → area arm
        (8, 8, 5, 5),  // mild minification → bilinear arm
        (4, 4, 4, 4),  // identity → bilinear arm
        (3, 3, 7, 9),  // upscale → bilinear arm
        (16, 2, 2, 2), // anisotropic heavy-x → area arm
    ];
    for rgba in [[37u8, 200, 90, 255], [10u8, 20, 30, 128]] {
        for (sw, sh, dw, dh) in cases {
            let src: Vec<u8> = rgba.iter().copied().cycle().take(sw * sh * 4).collect();
            let out = resample_rgba(&src, sw, sh, dw, dh);
            for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
                for c in 0..4 {
                    assert!(
                        (i32::from(px[c]) - i32::from(rgba[c])).abs() <= 1,
                        "solid {rgba:?} drifted at texel {i} ch {c}: {} \
                         ({sw}x{sh} → {dw}x{dh})",
                        px[c]
                    );
                }
            }
        }
    }
}

/// W10 (d): the sixel/OSC-1337 inline-image route goes through the SAME
/// upgraded resampler — a raw-RGBA checkerboard minified 4x into its footprint
/// lands the identical uniform linear mid-grey (byte-identical to
/// `resample_rgba`), not the old tap-skipping bilinear. The GPU image pass
/// calls this same `decode_image_to_footprint`, so parity holds by
/// construction.
#[test]
fn inline_image_footprint_uses_the_same_resampler() {
    use aterm_core::grid::extra::ImageFormat;
    let (sw, sh) = (8usize, 8usize);
    let mut src = vec![0u8; sw * sh * 4];
    for y in 0..sh {
        for x in 0..sw {
            let v = if (x + y) % 2 == 0 { 255 } else { 0 };
            let i = (y * sw + x) * 4;
            src[i] = v;
            src[i + 1] = v;
            src[i + 2] = v;
            src[i + 3] = 255;
        }
    }
    let out = aterm_render::decode_image_to_footprint(
        &src,
        ImageFormat::RawRgba8 {
            width: sw as u16,
            height: sh as u16,
        },
        2,
        2,
    )
    .expect("raw RGBA resamples");
    assert_eq!(
        out,
        resample_rgba(&src, sw, sh, 2, 2),
        "the inline-image route must be the one upgraded resampler"
    );
    let expected = srgb_encode(0.5);
    assert!(
        (i32::from(out[0]) - i32::from(expected)).abs() <= 1,
        "sixel/OSC-1337 minification must average in linear light (got {}, want {expected}±1)",
        out[0]
    );
}

// ---------------------------------------------------------------------------
// PROVE (2): premultiply soundness — a=0 texels contribute zero to RGB out.
// ---------------------------------------------------------------------------

/// Texels with `a == 0` contribute NOTHING: resampling an image whose invisible
/// texels carry a garish RGB payload is byte-identical to resampling the same
/// image with that payload zeroed — through BOTH filter arms. Non-vacuity: the
/// same payload made OPAQUE does change the output (the filter genuinely reads
/// those positions), and a straight (non-premultiplied) average WOULD have bled
/// the payload (checked on a hand-built worst case).
#[test]
fn invisible_texels_contribute_nothing() {
    let cases: [(usize, usize, usize, usize); 4] = [
        (16, 16, 4, 4), // area arm
        (9, 5, 3, 2),   // area arm, odd dims
        (7, 5, 6, 4),   // bilinear arm
        (5, 5, 5, 5),   // bilinear identity
    ];
    let mut any_opaque_diff = false;
    for (sw, sh, dw, dh) in cases {
        let n = sw * sh;
        let mut lcg = Lcg(0x5710_ee75 ^ (n as u64) << 8);
        let mut garish = vec![0u8; n * 4];
        let mut zeroed = vec![0u8; n * 4];
        let mut opaque = vec![0u8; n * 4];
        for i in 0..n {
            let visible = i % 3 != 0; // every third texel invisible
            let rgb = [lcg.next_u8(), lcg.next_u8(), lcg.next_u8()];
            let base = i * 4;
            if visible {
                garish[base..base + 3].copy_from_slice(&rgb);
                zeroed[base..base + 3].copy_from_slice(&rgb);
                opaque[base..base + 3].copy_from_slice(&rgb);
                garish[base + 3] = 255;
                zeroed[base + 3] = 255;
                opaque[base + 3] = 255;
            } else {
                // Invisible: garish carries magenta, zeroed carries black —
                // if a=0 texels leak, these two frames diverge.
                garish[base] = 255;
                garish[base + 2] = 255;
                opaque[base] = 255;
                opaque[base + 2] = 255;
                opaque[base + 3] = 255; // the non-vacuity twin: visible magenta
            }
        }
        let out_garish = resample_rgba(&garish, sw, sh, dw, dh);
        let out_zeroed = resample_rgba(&zeroed, sw, sh, dw, dh);
        assert_eq!(
            out_garish, out_zeroed,
            "a=0 RGB payload bled into the output ({sw}x{sh} → {dw}x{dh})"
        );
        if resample_rgba(&opaque, sw, sh, dw, dh) != out_garish {
            any_opaque_diff = true;
        }
    }
    // NON-VACUOUS: the invisible positions are genuinely inside the filter's
    // reach — making them opaque changes at least one case's output.
    assert!(
        any_opaque_diff,
        "opaque twin never diverged — the a=0 texels were outside every footprint \
         and the soundness check proved nothing"
    );
}

/// Exhaustive over the full channel domain: EVERY possible RGB payload under
/// `a == 0` maps to fully-zero output (premultiplication annihilates it — the
/// per-texel half of the soundness law, complete because a channel byte has
/// exactly 256 values and channels are independent). Non-vacuity: the same
/// payload at `a == 255` survives.
#[test]
fn zero_alpha_annihilates_every_payload() {
    for v in 0u16..=255 {
        let v = v as u8;
        for src in [[v, 0, 0, 0], [0, v, 0, 0], [0, 0, v, 0], [v, v, v, 0]] {
            assert_eq!(
                resample_rgba(&src, 1, 1, 1, 1),
                [0, 0, 0, 0],
                "a=0 payload {src:?} leaked"
            );
        }
    }
    assert_eq!(
        resample_rgba(&[9, 8, 7, 255], 1, 1, 1, 1),
        [9, 8, 7, 255],
        "an opaque texel must survive the identity resample exactly"
    );
}

/// The CBDT-halo worst case, pinned: ONE visible white texel in a field of
/// invisible saturated magenta, minified to a single output texel. A straight
/// (non-premultiplied) average bleeds magenta into the result; the
/// premultiplied bracket must yield pure white at the source's 1/16 coverage.
#[test]
fn halo_worst_case_stays_payload_free() {
    let (sw, sh) = (4usize, 4usize);
    let mut src = vec![0u8; sw * sh * 4];
    for i in 0..sw * sh {
        src[i * 4] = 255; // magenta payload…
        src[i * 4 + 2] = 255;
        // …fully transparent (a stays 0)
    }
    // one visible white texel
    src[0] = 255;
    src[1] = 255;
    src[2] = 255;
    src[3] = 255;
    let out = resample_rgba(&src, sw, sh, 1, 1);
    assert_eq!(
        &out[0..3],
        &[255, 255, 255],
        "unpremultiplied colour must be the visible texel's white — no magenta halo"
    );
    let a = f64::from(out[3]) / 255.0;
    assert!(
        (a - 1.0 / 16.0).abs() < 1.0 / 255.0 + 1e-6,
        "coverage must be the visible texel's exact footprint share (got {a})"
    );
}

// ---------------------------------------------------------------------------
// PROVE (3): the strike-selection law (Tier-1 of the StrikeSelection model).
// ---------------------------------------------------------------------------

/// Exhaustive over every strike multiset of size 0..=3 drawn from ppems 1..=6
/// and every target 1..=8: the choice is a member, adequate-and-minimal when
/// an adequate strike exists, else the maximum — the SAME law the `ty`-checked
/// `StrikeSelection` model carries, asserted property-wise (not by mirroring
/// the implementation). Non-vacuity: both law arms are reached.
#[test]
fn strike_selection_law_exhaustive() {
    let (mut hits_adequate, mut hits_fallback) = (0usize, 0usize);
    // 0 encodes "slot unused" so sizes 0..=3 are all enumerated.
    for a in 0u16..=6 {
        for b in 0u16..=6 {
            for c in 0u16..=6 {
                let strikes: Vec<u16> = [a, b, c].into_iter().filter(|&s| s != 0).collect();
                for target in 1u16..=8 {
                    match select_strike_ppem(&strikes, target) {
                        None => assert!(
                            strikes.is_empty(),
                            "None from a non-empty strike set {strikes:?}"
                        ),
                        Some(ch) => {
                            assert!(
                                strikes.contains(&ch),
                                "{ch} is not an available strike of {strikes:?}"
                            );
                            if strikes.iter().any(|&s| s >= target) {
                                hits_adequate += 1;
                                assert!(
                                    ch >= target,
                                    "adequate strike exists in {strikes:?} but {ch} < {target}"
                                );
                                assert!(
                                    strikes.iter().filter(|&&s| s >= target).all(|&s| ch <= s),
                                    "{ch} is not minimal among adequate {strikes:?} (t={target})"
                                );
                            } else {
                                hits_fallback += 1;
                                assert!(
                                    strikes.iter().all(|&s| s <= ch),
                                    "{ch} is not the maximum of inadequate {strikes:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(hits_adequate > 0, "the adequate arm was never exercised");
    assert!(
        hits_fallback > 0,
        "the fallback (max) arm was never exercised"
    );
}

/// The real Apple ladder (20/32/40/48/64/96/160), spot-checked: typical cell
/// heights land on the hand-tuned small strikes, and the ZWJ dead-zone shape
/// (a composite carried ONLY by the large strikes) walks UP to the smallest
/// carrier — never all the way to 160 when 64 suffices, never `None`.
#[test]
fn apple_strike_ladder_and_dead_zone() {
    const APPLE: [u16; 7] = [20, 32, 40, 48, 64, 96, 160];
    assert_eq!(select_strike_ppem(&APPLE, 17), Some(20));
    assert_eq!(select_strike_ppem(&APPLE, 22), Some(32));
    assert_eq!(
        select_strike_ppem(&APPLE, 40),
        Some(40),
        "exact hit is exact"
    );
    assert_eq!(select_strike_ppem(&APPLE, 41), Some(48));
    assert_eq!(select_strike_ppem(&APPLE, 100), Some(160));
    assert_eq!(
        select_strike_ppem(&APPLE, 300),
        Some(160),
        "beyond the ladder: the largest strike (the old u16::MAX behaviour)"
    );
    // A composite ZWJ glyph absent from the small strikes (the sbix ppem
    // dead-zone): the carrying set is what `pick_glyph_raster` feeds here.
    assert_eq!(
        select_strike_ppem(&[64, 96, 160], 40),
        Some(64),
        "dead-zone walk stops at the SMALLEST carrier, not the 160px master"
    );
    assert_eq!(
        select_strike_ppem(&[], 40),
        None,
        "no carrier → COLR/mono fallback"
    );
}
