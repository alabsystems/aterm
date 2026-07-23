// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! W2 (linear-corrected weight compensation) — machine-checked invariants for
//! the text-blend seam: [`aterm_render::blend_text`], its perceptual alpha
//! remap [`aterm_render::correct_alpha`], and the texel-level gate
//! [`aterm_render::correction_applies`].
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `TextBlendGate` derived model (`aterm_spec::derive::text_blend_gate_model`)
//!   carries the `CorrectionGated` invariant: the remap fires ONLY in
//!   corrected mode, on an interior (non-endpoint) coverage texel, with a
//!   non-degenerate luminance gap. `cargo test -p aterm-spec`
//!   (`derived_text_blend_gate_proves_and_catches_degenerate_divide`) runs the
//!   real `ty` binary over the bounded state space: it PROVES the invariant at
//!   `Buggy=0` and CATCHES the ungated (div-by-near-zero) variant at `Buggy=1`.
//! * **Tier-1 (concrete, this file)** — the gate's domain is tiny
//!   (`bool × u8 × bool`), so we enumerate it COMPLETELY, and additionally
//!   bind it to the shipping byte-level seam: whenever `blend_text` in some
//!   mode differs from plain linear, `correction_applies` must hold for that
//!   texel — over every coverage byte and both degenerate and non-degenerate
//!   colour pairs, with non-vacuity controls.
//!
//! The FLOAT-VALUED laws of the remap (range, monotonicity, degenerate guard,
//! endpoint anchoring) cannot be stated in the ty expression language (no
//! multiplication/`pow`), so — per the repo's box-drawing-rounding precedent —
//! they are proven here by deliberate exhaustive lattice sweeps under plain
//! `cargo test`.

use aterm_render::{TEXT_BLEND_EPS, blend_text, correct_alpha, correction_applies};

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
    // Chromatic corners + the theme-ish defaults the renderer actually blends.
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

/// PROVE (1) endpoint exactness: `t == 0` leaves the destination bytes
/// untouched and `t == 255` yields the exact fg bytes, in BOTH modes, for
/// every fg/bg pair on the lattice — including pixels with a dirty high byte
/// (the blit's framebuffer mask). The early returns run before any float
/// math, so this is bit-precise, not merely within a tolerance.
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

/// GOLDEN (the audit's sin-2 number): a 50%-coverage white texel on black must
/// land at ~sRGB 128 in the default linear-corrected mode — the apparent
/// weight of a gamma-space (CoreText-like) blend — while pure linear puts it
/// at ~sRGB 188 (the thin/washed dark-mode text this item fixes). The linear
/// assertion doubles as the NEGATIVE CONTROL: it shows the pre-fix behaviour
/// really is ~188, so the corrected assertion is non-vacuous.
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

/// PROVE (2) the remapped alpha is in `[0, 1]` and MONOTONE NONDECREASING in
/// the raw coverage for any fg/bg luminance pair, over an exhaustive lattice:
/// all 33×33 luminance pairs (i/32, including equal and near-equal) × all 256
/// coverage steps. Also anchors the endpoints (`a=0 → ~0`, `a=1 → ~1`) and
/// includes the NON-VACUITY control: the remap genuinely moves midtone
/// coverage (white-on-black 50% shifts by more than 0.1).
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
            // Endpoint anchors: identity at the ends up to the sRGB round-trip's
            // float error (the BYTE-exact endpoints are blend_text's early
            // returns, proven in endpoint_exactness_both_modes).
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
    // Non-vacuity: the remap is not the identity — 50% white-on-black moves
    // from 0.502 to ~0.216 (the whole point of the mode).
    let moved = correct_alpha(1.0, 0.0, 0.5);
    assert!(
        (0.5 - moved) > 0.1,
        "remap must genuinely reweight midtones (got {moved})"
    );
}

/// PROVE (3) the degenerate guard: a luminance gap under [`TEXT_BLEND_EPS`]
/// returns the raw coverage BIT-EXACTLY (no division ever runs), and at the
/// byte level the corrected mode is then IDENTICAL to plain linear for every
/// coverage value. Uses both trivially-equal pairs and a genuinely tricky one
/// — pure red vs the gray of equal luminance — validated degenerate by an
/// independent reference luminance.
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
    // Byte level: pure red vs its equal-luminance gray. Guard the fixture with
    // the independent reference so the pair really is inside the eps window.
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

/// Tier-1 CONFORMANCE BIND for the `TextBlendGate` ty model: (a) the pure gate
/// itself, enumerated over its COMPLETE `bool × u8 × bool` domain, satisfies
/// the model's `CorrectionGated` invariant; (b) the shipping byte-level seam
/// obeys the gate — whenever `blend_text` differs from plain linear, the gate
/// is open for that texel. With positive (non-vacuity) and negative controls.
#[test]
fn correction_gate_conformance() {
    // (a) Complete enumeration of the gate: the SAME invariant the ty model
    // carries (applies ⇒ corrected ∧ interior ∧ ¬degenerate).
    let mut opened = 0usize;
    for corrected in [false, true] {
        for cov in 0..=255u16 {
            for degenerate in [false, true] {
                let applies = correction_applies(corrected, cov as u8, degenerate);
                if applies {
                    assert!(
                        corrected && cov != 0 && cov != 255 && !degenerate,
                        "gate opened outside its domain: corrected={corrected} cov={cov} degenerate={degenerate}"
                    );
                    opened += 1;
                }
            }
        }
    }
    // Non-vacuity: the gate does open (for every interior texel exactly once).
    assert_eq!(opened, 254, "gate must open for all 254 interior coverages");

    // (b) Bind to the real seam: `blend_text` may differ from linear ONLY when
    // the gate is open. White-on-black is the non-degenerate pair; red vs its
    // equal-luminance gray is the degenerate one (validated above).
    let pairs = [
        (0x00ff_ffffu32, 0x0000_0000u32, false), // fg, bg, degenerate
        (0x00ff_0000, 0x007f_7f7f, true),
    ];
    let mut differing = 0usize;
    for &(fg, bg, degenerate) in &pairs {
        assert_eq!(
            (ref_luminance(fg) - ref_luminance(bg)).abs() < TEXT_BLEND_EPS,
            degenerate,
            "fixture degeneracy must match the reference luminances"
        );
        for corrected in [false, true] {
            for t in 0..=255u16 {
                let t = t as u8;
                if blend_text(bg, fg, bg, t, corrected) != blend_text(bg, fg, bg, t, false) {
                    assert!(
                        correction_applies(corrected, t, degenerate),
                        "output changed with the gate closed: fg={fg:#08x} bg={bg:#08x} t={t} corrected={corrected}"
                    );
                    differing += 1;
                }
            }
        }
    }
    // Positive control: corrected mode really rewrites midtone texels on the
    // non-degenerate pair (an all-equal sweep would prove nothing).
    assert!(
        differing > 100,
        "corrected mode must visibly reweight the white-on-black ramp ({differing} texels differed)"
    );
}
