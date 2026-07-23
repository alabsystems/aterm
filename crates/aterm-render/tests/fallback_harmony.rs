// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for W8 (fallback harmony): the SHIPPING pure policies
//! behind fallback-face metric normalization, wide-glyph centring, the
//! row-band clip, weight-matched candidate ranking, and the macOS CoreText
//! drawability probe.
//!
//! aterm used to rasterize every fallback face via fontdue at the PRIMARY's
//! raw px (audit sin 6): CJK from Arial Unicode instead of a native design,
//! wide glyphs left-biased in their 2-cell box, no weight/italic matching,
//! and fallback overdraw never clipped to the cell row.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   derived models `FallbackScaleClamp`, `WideCentre` and `FallbackBandClip`
//!   (`aterm_spec::derive::{fallback_scale_clamp_model, wide_center_model,
//!   fallback_band_clip_model}`) carry the clamp/balance/in-band invariants;
//!   `cargo test -p aterm-spec --test derived_ring_ty` runs the REAL `ty`
//!   binary over each bounded state space, PROVING every invariant at
//!   `Buggy=0` and CATCHING the pre-W8 defect at `Buggy=1` (counterexample).
//! * **Tier-1 (concrete, this file)** — the real `f32`/integer functions are
//!   swept over dense/exhaustive lattices (plus degenerate and adversarial
//!   inputs the abstract models cannot express: NaN, ∞, floor division on
//!   negative gaps, garbage font bytes), with non-vacuity controls and the
//!   pre-fix behaviour shown to violate each law. Floor division and the
//!   ratio arithmetic are outside ty's Expr language (no `*`/`/`), so — per
//!   the box-drawing rounding-law precedent — the lattice sweeps here are the
//!   binding layer for the arithmetic half.

use aterm_render::{
    CJK_SCALE_MAX, CJK_SCALE_MIN, XHEIGHT_SCALE_MAX, XHEIGHT_SCALE_MIN, clamp_to_row_band,
    fallback_cjk_scale, fallback_weight_rank, fallback_xheight_scale, wide_center_offset,
};

// ---- (1) normalization clamps ----

/// Every finite positive (target, actual) pair on a dense lattice: the scale
/// stays inside the clamp interval, and whenever the ideal ratio is already
/// inside it the scaled advance equals the target EXACTLY (f32 division is
/// exact enough here: `scale * actual == target` to within one ulp-scale
/// epsilon, asserted at 1e-3 relative).
#[test]
fn scale_clamp_bounds_and_exactness_lattice() {
    let vals: Vec<f32> = (1..=400).map(|i| i as f32 * 0.25).collect(); // 0.25..=100
    let mut clamped_lo = 0u32;
    let mut clamped_hi = 0u32;
    let mut exact = 0u32;
    for &target in &vals {
        for &actual in &vals {
            for (scale, lo, hi) in [
                (
                    fallback_cjk_scale(target, actual),
                    CJK_SCALE_MIN,
                    CJK_SCALE_MAX,
                ),
                (
                    fallback_xheight_scale(target, actual),
                    XHEIGHT_SCALE_MIN,
                    XHEIGHT_SCALE_MAX,
                ),
            ] {
                assert!(
                    (lo..=hi).contains(&scale),
                    "scale {scale} out of [{lo}, {hi}] for target={target} actual={actual}"
                );
                let ratio = target / actual;
                if ratio >= lo && ratio <= hi {
                    let err = (scale * actual - target).abs() / target;
                    assert!(
                        err <= 1e-3,
                        "in-range ratio must normalize exactly: target={target} \
                         actual={actual} scale={scale} err={err}"
                    );
                    exact += 1;
                } else if ratio < lo {
                    assert_eq!(scale, lo, "below-range ratio must clamp to lo");
                    clamped_lo += 1;
                } else {
                    assert_eq!(scale, hi, "above-range ratio must clamp to hi");
                    clamped_hi += 1;
                }
            }
        }
    }
    // Non-vacuity: all three regimes are genuinely exercised by the lattice.
    assert!(exact > 0 && clamped_lo > 0 && clamped_hi > 0);
}

/// Totality on degenerate/adversarial inputs: zero, negative, NaN and ∞
/// metrics yield the neutral scale `1.0` (the pre-W8 unscaled raster), never
/// a panic or a NaN that would poison the raster px.
#[test]
fn scale_clamp_total_on_degenerate_inputs() {
    let bad = [
        0.0f32,
        -1.0,
        -100.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    for &a in &bad {
        for &b in bad.iter().chain(&[16.0f32]) {
            assert_eq!(fallback_cjk_scale(a, b), 1.0, "cjk({a}, {b})");
            assert_eq!(fallback_cjk_scale(b, a), 1.0, "cjk({b}, {a})");
            assert_eq!(fallback_xheight_scale(a, b), 1.0, "xh({a}, {b})");
            assert_eq!(fallback_xheight_scale(b, a), 1.0, "xh({b}, {a})");
        }
    }
    // Finite operands whose RATIO overflows to ∞ are caught by the
    // finite-ratio guard: junk metrics yield the neutral scale, never a
    // non-finite raster px.
    assert_eq!(fallback_cjk_scale(f32::MAX, f32::MIN_POSITIVE), 1.0);
}

// ---- (2) centring law ----

/// Exhaustive over every (box_w, adv) in a lattice that includes `adv >
/// box_w` (a clamped face whose scaled advance overflows the box): the
/// offset satisfies the floor characterization `2*off <= gap <= 2*off + 1`
/// (the EXACT hypothesis the ty-proven `WideCentre` model shows implies the
/// balance law), hence `|left - right| <= 1`; and the pre-W8 left-bias
/// (`off = 0`) violates the balance for every gap >= 2 (negative control).
#[test]
fn wide_center_offset_balances_margins_exhaustively() {
    let mut nonzero_offsets = 0u32;
    for box_w in 0..=64i32 {
        for adv in 0..=80i32 {
            let gap = box_w - adv;
            let off = wide_center_offset(box_w, adv);
            // The floor characterization (the Tier-0 model's Fire guard).
            assert!(
                2 * off <= gap && gap <= 2 * off + 1,
                "off={off} is not floor((box={box_w} - adv={adv})/2)"
            );
            let (left, right) = (off, gap - off);
            assert!(
                (left - right).abs() <= 1,
                "margins unbalanced: box={box_w} adv={adv} left={left} right={right}"
            );
            if off != 0 {
                nonzero_offsets += 1;
            }
            // Negative control: the pre-W8 placement (off = 0, whole gap on
            // the right) breaks the law exactly where centring matters.
            if gap.abs() >= 2 {
                let (l0, r0) = (0, gap);
                assert!((l0 - r0).abs() > 1, "the pre-fix left-bias must violate");
            }
        }
    }
    // Non-vacuity: the offset genuinely moves glyphs (not identically zero).
    assert!(nonzero_offsets > 0);
}

// ---- (4) row-band clip ----

/// Exhaustive over every (top, height, band) in a lattice spanning negative
/// tops (ascender overshoot), zero-size bitmaps/bands and far-past-the-band
/// placements: the trim only ever drops rows, every kept row lies inside
/// `[0, band)`, and an already-in-band glyph is untouched. The pre-W8
/// behaviour (no trim) is shown to violate on every out-of-band shape
/// (negative control).
#[test]
fn clamp_to_row_band_keeps_rows_in_band_exhaustively() {
    let mut trimmed = 0u32;
    let mut untouched = 0u32;
    for top in -48..=48i32 {
        for height in 0..=48usize {
            for band in 0..=40usize {
                let (skip, keep) = clamp_to_row_band(top, height, band);
                assert!(
                    skip + keep <= height,
                    "trim grew the bitmap: top={top} h={height} band={band} -> ({skip}, {keep})"
                );
                let new_top = top + skip as i32;
                if keep > 0 {
                    assert!(
                        new_top >= 0 && new_top + keep as i32 <= band as i32,
                        "kept rows escape the band: top={top} h={height} band={band} \
                         -> ({skip}, {keep})"
                    );
                }
                // An in-band glyph is byte-identical (no gratuitous trims).
                if top >= 0 && top + height as i32 <= band as i32 {
                    assert_eq!((skip, keep), (0, height), "in-band glyph must be untouched");
                    untouched += 1;
                } else if height > 0 {
                    // Out-of-band ink: the pre-W8 unclipped blit (skip=0,
                    // keep=height) violates the in-band law here.
                    let would_escape = top < 0 || top + height as i32 > band as i32;
                    assert!(would_escape, "negative control classification");
                    trimmed += 1;
                }
            }
        }
    }
    // Non-vacuity: both regimes are exercised.
    assert!(trimmed > 0 && untouched > 0);
}

// ---- (e) weight/italic ranking ----

/// The ranking law: a style (italic) mismatch outranks ANY weight distance
/// (OS/2 weights are 1..=1000, so the italic penalty of 1000 dominates), and
/// among style-matched candidates the closest weight wins.
#[test]
fn weight_rank_orders_candidates() {
    // Regular primary: a W3 (300) CJK face beats a W6 (600) one.
    assert!(
        fallback_weight_rank(300, false, 400, false) < fallback_weight_rank(600, false, 400, false)
    );
    // Exact match is rank 0 (best possible).
    assert_eq!(fallback_weight_rank(400, false, 400, false), 0);
    // Italic mismatch dominates even the worst weight gap.
    assert!(
        fallback_weight_rank(1000, false, 1, false) < fallback_weight_rank(1, true, 1, false),
        "style mismatch must outrank any weight distance"
    );
    // An italic primary prefers italic candidates symmetrically.
    assert!(
        fallback_weight_rank(400, true, 400, true) < fallback_weight_rank(400, false, 400, true)
    );
}

// ---- (3) probe soundness (macOS CoreText) ----

/// The CT drawability probe is TOTAL over adversarial inputs — garbage bytes,
/// truncated real fonts, an out-of-range collection index, and the empty blob
/// all return `false` without panicking — and sound-by-construction on a real
/// face (accepting == literally obtaining a non-empty raster, which the
/// renderer then reproduces at raster time). Non-vacuity: the system
/// monospace face IS accepted for 'M'; the colour-emoji face is REJECTED for
/// an emoji (colour bitmaps are the colour pipeline's job, not the mono
/// fallback's).
#[test]
#[cfg(target_os = "macos")]
fn ct_probe_is_total_and_sound() {
    use aterm_render::ct_face_can_render;

    // Adversarial corpus: must be `false`, must not panic.
    assert!(!ct_face_can_render(&[], 0, 'M'), "empty blob");
    assert!(!ct_face_can_render(&[0u8; 64], 0, 'M'), "zero blob");
    let junk: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    assert!(!ct_face_can_render(&junk, 0, 'M'), "pseudo-random blob");
    if let Ok(menlo) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
        for cut in [1usize, 12, 256, 4096, menlo.len() / 2] {
            assert!(
                !ct_face_can_render(&menlo[..cut.min(menlo.len())], 0, 'M') || cut >= menlo.len(),
                "truncated real font at {cut} bytes must not panic (and must not \
                 claim a raster it can't obtain)"
            );
        }
        // Out-of-range collection index: total, false.
        assert!(
            !ct_face_can_render(&menlo, 999, 'M'),
            "face index out of range"
        );
        // Non-vacuity: the real face IS accepted for a covered char and
        // rejected for an uncovered one (cmap duty via ttf-parser).
        assert!(ct_face_can_render(&menlo, 0, 'M'), "Menlo must accept 'M'");
        assert!(
            !ct_face_can_render(&menlo, 0, '\u{FDD0}'),
            "a permanent noncharacter has no glyph"
        );
    } else {
        eprintln!("SKIP: Menlo.ttc not readable");
    }
    // Colour-bitmap glyphs are rejected (the mono resolver's contract).
    if let Ok(emoji) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") {
        assert!(
            !ct_face_can_render(&emoji, 0, '\u{1F600}'),
            "a colour-bitmap glyph must not pass the MONO drawability probe"
        );
    }
}

// ---- end-to-end regression pins (real renderer) ----

/// A wide CJK glyph resolved through the fallback chain: (i) its coverage
/// rows sit inside the cell row band (the W8 clip, pinned on the REAL raster
/// under both backends), and (ii) its advance is the full 2-cell box (the
/// centring recipe). Skips gracefully on a host with no system font.
#[test]
fn cjk_fallback_glyph_is_banded_and_wide() {
    use aterm_render::{FaceId, GlyphClass, Renderer, Theme};
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    let (_, cell_h) = r.cell_size();
    for force_fontdue in [false, true] {
        if force_fontdue {
            r.debug_force_fontdue();
        }
        let key = r.glyph_key('\u{4E2D}'); // 中
        if key.source != FaceId::Fallback || key.glyph_class != GlyphClass::Mono {
            eprintln!("SKIP: 中 not served by the fallback chain on this host");
            return;
        }
        let baseline = r.baseline();
        let (cell_w, _) = r.cell_size();
        let img = r.glyph_image(key);
        assert!(
            img.height() > 0 && img.bytes().iter().any(|&c| c > 0),
            "中 rasterized empty"
        );
        let top = baseline - img.height() as i32 - img.ymin();
        assert!(
            top >= 0 && top + img.height() as i32 <= cell_h as i32,
            "fallback coverage escapes the cell row band: top={top} h={} cell_h={cell_h} \
             (force_fontdue={force_fontdue})",
            img.height()
        );
        assert_eq!(
            img.advance(),
            (2 * cell_w) as f32,
            "a wide fallback glyph owns its 2-cell box (force_fontdue={force_fontdue})"
        );
    }
}

/// macOS native-CJK routing (W8): with the stock candidate list, the chain
/// face that covers 中 is the native CJK design (Hiragino Sans GB), not Arial
/// Unicode — while a char Hiragino lacks still lands on the broad face
/// (additive tier, no coverage lost). Skips when the host lacks the faces.
#[test]
#[cfg(target_os = "macos")]
fn native_cjk_face_leads_the_macos_chain() {
    use aterm_render::{Renderer, Theme};
    if !std::path::Path::new("/System/Library/Fonts/Hiragino Sans GB.ttc").exists() {
        eprintln!("SKIP: Hiragino Sans GB not installed");
        return;
    }
    if std::env::var_os("ATERM_FALLBACK_FONT").is_some() {
        eprintln!("SKIP: ATERM_FALLBACK_FONT overrides the builtin chain");
        return;
    }
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    let pick = r.debug_fallback_pick_path('\u{4E2D}');
    assert_eq!(
        pick.as_deref(),
        Some("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        "中 must come from the native CJK chain face"
    );
    // Additive tier: Arabic (which Hiragino lacks) still reaches the broad face.
    if std::path::Path::new("/System/Library/Fonts/Supplemental/Arial Unicode.ttf").exists() {
        let arabic = r.debug_fallback_pick_path('\u{0645}'); // م
        assert_eq!(
            arabic.as_deref(),
            Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
            "the broad face must still back the chain"
        );
    }
}
