// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Fractional-bearing CoreText rasters: the sub-pixel placement policy
//! [`aterm_render::ct_pen_and_bearing`] and the phase-headroom extent
//! [`aterm_render::ct_padded_extent`], the pure fns `CtFont::rasterize` (macOS) applies
//! to every glyph.
//!
//! Each outline is rasterized at its true sub-pixel offset (pen `= -floor(b)`) with
//! `floor(b)` bearings: the phase stays inside the bitmap and the integer bearing places
//! it back exactly. Cell origins are integer device px, so each glyph has exactly one
//! correct phase: one raster per glyph, no new cache keys.
//!
//! The ty model `aterm_models_render::ct_frac_bearing_model` carries `Decompose`
//! (bearing + phase == designed position) and `PhaseInUnit` (phase in `[0, 1)`) and
//! catches a round-and-pin placement at `Buggy=1`
//! (aterm-spec's `unregistered_bound_models_prove_and_catch`). ty's Expr language is
//! integer-only, so the float half lives here: a dyadic lattice on which every asserted
//! equality is exact (1/64 steps), ulp-bounded specials, and a point-for-point drive of
//! the model's interpreter against `ct_pen_and_bearing`. The one float boundary, a `b`
//! within one ulp below an integer saturating the phase to `1.0`, is absorbed by the
//! extent's `+1`. The FFI seam is bound on real Menlo ink by
//! `coretext_placement_binds_to_the_pure_policy` in the lib tests.
//!
//! The policy is the CoreText path's alone; on macOS both backends take the same
//! `glyph_image` bytes and placement.

use aterm_models_render::ct_frac_bearing_model;
use aterm_render::{ct_padded_extent, ct_pen_and_bearing};

/// The floor law, exhaustive over the 1/64-px lattice in ±64px (every value exactly
/// representable): `min <= b < min + 1`, the pen is `-min` with no rounding, the phase is
/// `b - min ∈ [0, 1)`, and bearing + phase reconstructs `b` bit-exactly.
#[test]
fn pen_and_bearing_floor_law_exact_on_the_dyadic_lattice() {
    let mut nonzero_phase_seen = 0usize;
    for k in -4096i64..=4096 {
        let b = k as f64 / 64.0;
        let (pen, min) = ct_pen_and_bearing(b);
        let minf = f64::from(min);
        // Floor law: min <= b < min + 1 (exact comparisons).
        assert!(
            minf <= b && b < minf + 1.0,
            "floor law violated at b={b} (min={min})"
        );
        // The raster translate is the pure integer -min: no rounding at all.
        assert_eq!(pen, -minf, "pen must be the negated integer part at b={b}");
        // The outline's in-bitmap phase (what CG draws at) is b - min in [0,1).
        let phase = b + pen;
        assert!(
            (0.0..1.0).contains(&phase),
            "phase must be in [0,1) at b={b} (phase={phase})"
        );
        // Bearing + phase reconstructs the designed position exactly.
        assert_eq!(
            minf + phase,
            b,
            "floor(b) + (b - floor(b)) must reconstruct b exactly at b={b}"
        );
        if phase > 0.0 {
            nonzero_phase_seen += 1;
        }
    }
    // Non-vacuity: fractional bearings dominate the lattice.
    assert!(
        nonzero_phase_seen > 4000,
        "the lattice must reach fractional phases ({nonzero_phase_seen})"
    );
}

/// Float specials (signed zero, halves, one ulp off an integer, a large magnitude): the
/// floor law and the integer pen stay exact; within one ulp below an integer the phase
/// saturates to exactly `1.0` and the reconstruction is off by at most one epsilon.
#[test]
fn pen_and_bearing_floor_law_adversarial_specials() {
    let specials = [
        0.0,
        -0.0,
        0.5,
        -0.5,
        1.0 - f64::EPSILON / 2.0, // largest f64 below 1
        -(f64::EPSILON / 4.0),    // one "half-ulp" below 0: phase saturates to 1.0
        -1.0 - f64::EPSILON,      // just below -1
        13.7,
        -2.3,
        1e6 + 0.3,
        f64::EPSILON,
    ];
    let mut saturated_seen = 0usize;
    for b in specials {
        let (pen, min) = ct_pen_and_bearing(b);
        let minf = f64::from(min);
        assert!(
            minf <= b && b < minf + 1.0,
            "floor law violated at b={b:e} (min={min})"
        );
        assert_eq!(pen, -minf, "pen must be exact at b={b:e}");
        let phase = b + pen;
        assert!(
            (0.0..=1.0).contains(&phase),
            "phase must be in [0,1] at b={b:e} (phase={phase})"
        );
        assert!(
            (minf + phase - b).abs() <= f64::EPSILON,
            "reconstruction must be within one ulp at b={b:e}"
        );
        if phase == 1.0 {
            saturated_seen += 1;
        }
    }
    // Non-vacuity: the phase does saturate to 1.0, which the extent's +1 absorbs.
    assert!(
        saturated_seen > 0,
        "a b one half-ulp below an integer must saturate the phase to 1.0"
    );
}

/// The bitmap never clips: over 1/64-px ink widths up to 48px, both pad levels (1 plain,
/// 2 `font_thicken`) and every phase in [0, 1], `phase + ink + 2*pad <= extent`, with a
/// full px of headroom over the padded ink box, and the thickened box exactly 1px per
/// side larger (as `coretext_placement_binds_to_the_pure_policy` pins on real ink).
#[test]
fn padded_extent_absorbs_every_phase_without_clipping() {
    let mut old_extent_would_clip = 0usize;
    for j in 1i64..=3072 {
        let ink = j as f64 / 64.0;
        assert_eq!(
            ct_padded_extent(ink, 2.0),
            ct_padded_extent(ink, 1.0) + 2,
            "font_thicken must widen the raster box by exactly 1px per side (ink={ink})"
        );
        for pad in [1.0f64, 2.0] {
            let ext = ct_padded_extent(ink, pad) as f64;
            // Headroom: one full px over the padded ink box (>= ink width + 1).
            assert!(
                ext >= ink + 2.0 * pad + 1.0,
                "extent must keep 1px of phase headroom (ink={ink} pad={pad} ext={ext})"
            );
            let old = (ink + 2.0 * pad).ceil(); // the extent without the +1
            for q in 0i64..=64 {
                let phase = q as f64 / 64.0;
                // No clipping at any phase: the padded ink's trailing edge fits.
                assert!(
                    phase + ink + 2.0 * pad <= ext,
                    "fractional ink must never clip (ink={ink} pad={pad} phase={phase})"
                );
                if phase + ink + 2.0 * pad > old {
                    old_extent_would_clip += 1;
                }
            }
        }
    }
    // Negative control: without the +1 (`ceil(ink + 2*pad)`) a near-1 phase clips.
    assert!(
        old_extent_would_clip > 0,
        "the pre-fix extent must clip for some (ink, phase) — otherwise the +1 proves nothing"
    );
}

/// Model ↔ code conformance: the `CtFracBearing` model's own interpreter (`Model::fire`,
/// the semantics `ty` checks), over its whole domain (eighth-px bearings in ±3px),
/// reports the same `(bearing, rem)` as `ct_pen_and_bearing` at every point.
#[test]
fn pen_and_bearing_conforms_to_the_ty_checked_model() {
    let m = ct_frac_bearing_model();
    let mut negative_seen = 0usize;
    let mut fractional_seen = 0usize;
    for b_e in -24i64..=24 {
        // Enter the model at phase 2 (this loop does PickB/Latch's enumeration:
        // rem = b, bearing = 0).
        let mut state = m.init_state();
        state.insert("phase", 2);
        state.insert("b", b_e);
        state.insert("rem", b_e);
        state.insert("bearing", 0);
        let mut steps = 0usize;
        while !m.action_enabled("Report", &state) {
            let step = if m.action_enabled("StepDown", &state) {
                "StepDown"
            } else {
                "StepUp"
            };
            assert!(m.fire(step, &mut state), "{step} must fire (b={b_e})");
            steps += 1;
            assert!(
                steps <= 3,
                "±3px decomposes in <=3 whole-px moves (b={b_e})"
            );
        }
        assert!(m.fire("Report", &mut state), "Report must fire (b={b_e})");
        // The committed (Buggy=0) model invariants hold at the reported state.
        assert!(
            m.check_invariant("Decompose", &state),
            "Decompose (b={b_e})"
        );
        assert!(
            m.check_invariant("PhaseInUnit", &state),
            "PhaseInUnit (b={b_e})"
        );
        // Point-for-point agreement with the shipping pure fn (eighth-px scale).
        let (pen, min) = ct_pen_and_bearing(b_e as f64 / 8.0);
        assert_eq!(
            state["bearing"],
            8 * i64::from(min),
            "model bearing must equal 8*floor(b/8) (b={b_e})"
        );
        assert_eq!(
            state["rem"],
            b_e - 8 * i64::from(min),
            "model phase must equal the retained remainder (b={b_e})"
        );
        assert_eq!(
            state["rem"] as f64,
            (b_e as f64 / 8.0 + pen) * 8.0,
            "model phase must equal the code's in-bitmap phase, scaled (b={b_e})"
        );
        if b_e < 0 {
            negative_seen += 1;
        }
        if b_e.rem_euclid(8) != 0 {
            fractional_seen += 1;
        }
    }
    // Non-vacuity: negative bearings (floor != truncate) and off-grid bearings walked.
    assert!(negative_seen > 0 && fractional_seen > 0);
}
