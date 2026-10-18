// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Emoji strike selection and the premultiplied area filter, checked exhaustively.
//!
//! The strike-selection law (chosen from the available strikes; minimal adequate, else the
//! maximum) is checked against [`aterm_render::select_strike_ppem`] over every small strike
//! multiset. The area filter's laws involve products, which ty's `Expr` language lacks, so
//! the integer lattice here is their only check.

use aterm_render::{area_overlap, resample_rgba, select_strike_ppem};

/// The sRGB encoding of a linear-light value as a byte: the test's own piecewise OETF, so
/// the expectation shares no code with the implementation.
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

// Partition of unity.

/// Exhaustive over 1..=48 × 1..=48: each destination texel's footprint weights sum to
/// exactly `src` (normalized, exactly 1), and each source texel's mass sums to `dst`.
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
    // Non-vacuity: 3→2 splits the middle texel across both footprints.
    assert_eq!(area_overlap(0, 1, 3, 2), 1, "texel 1 of 3→2 leans into d=0");
    assert_eq!(
        area_overlap(1, 1, 3, 2),
        1,
        "…and into d=1 (split, not skipped)"
    );
}

/// A 1px black/white checkerboard minified 4x lands uniform linear-light mid-grey (the
/// sRGB encoding of 0.5, ~188) at every texel; the sRGB-space average (~128) and
/// skipped-tap extremes (0/255) are ruled out explicitly.
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
    // Negative controls: the gamma-space average and the skipped-tap extremes.
    assert!(
        (i32::from(first) - 128).abs() > 20,
        "output {first} is the too-dark sRGB-space average, not linear light"
    );
    assert!(
        first != 0 && first != 255,
        "output {first} is a skipped-tap extreme — texels were dropped"
    );
}

/// A solid image, opaque or semi-transparent, survives any ratio through both filter arms
/// within 1 LSB.
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

/// The fitted inline-image route (OSC 1337, Kitty with `c=`/`r=`) is the same resampler:
/// `decode_image_to_footprint` is byte-identical to `resample_rgba`, and the GPU image pass
/// decodes through the same path. `PixelExact` placements run no resampler
/// (`sixel_footprint_decode_is_pixel_exact` in `tests/inline_image.rs`).
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
        aterm_core::grid::extra::ImageScaling::Fit,
        None,
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

// Premultiply soundness: a=0 texels contribute nothing to RGB.

/// Resampling an image whose invisible texels carry a garish RGB payload is byte-identical
/// to resampling it with that payload zeroed, through both filter arms. Non-vacuity: the
/// same payload made opaque changes the output.
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
                // Invisible: magenta in `garish`, black in `zeroed`.
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
    // Non-vacuity: the invisible positions are inside the filter's reach.
    assert!(
        any_opaque_diff,
        "opaque twin never diverged — the a=0 texels were outside every footprint \
         and the soundness check proved nothing"
    );
}

/// One visible white texel in a field of invisible magenta, minified to one texel, yields
/// pure white at 1/16 coverage; a straight average would bleed magenta (the CBDT halo).
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

// Strike selection.

/// Exhaustive over strike multisets of size 0..=3 from ppems 1..=6 and targets 1..=8: the
/// choice is a member, the minimal adequate strike when one exists, else the maximum.
/// Asserted as properties, not by mirroring the implementation; both arms are reached.
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
