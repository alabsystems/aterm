// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! W3 (fractional-bearing CoreText rasters): proofs of the sub-pixel placement
//! policy [`aterm_render::ct_pen_and_bearing`] and the phase-headroom extent
//! [`aterm_render::ct_padded_extent`] — the pure fns `CtFont::rasterize`
//! (macOS) applies to every glyph.
//!
//! The sin being fixed (audit sin 10): `ct_rasterize` pinned each ink box to an
//! integer bitmap origin (pen `= -b`) and stored `round(b)` bearings, so every
//! glyph sat up to 0.5px off its designed position in both axes, error varying
//! glyph-to-glyph. The fix rasterizes the outline at its true sub-pixel offset
//! (pen `= -floor(b)`) and reports `floor(b)` bearings — the phase is RETAINED
//! inside the bitmap and the integer bearing places it back exactly. The
//! monospace invariant (cell origins are always integer device px) means each
//! glyph has exactly ONE correct phase: one raster per glyph, zero new cache
//! keys, zero atlas growth.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `CtFracBearing` derived model (`aterm_spec::derive::ct_frac_bearing_model`)
//!   carries `Decompose` (bearing + retained phase == designed position,
//!   exactly) and `PhaseInUnit` (reported phase in `[0, 1)`).
//!   `cargo test -p aterm-spec`
//!   (`derived_ct_frac_bearing_proves_and_catches_rounded_pin`) runs the REAL
//!   `ty` binary over the whole bounded eighth-px lattice: it PROVES both at
//!   `Buggy=0` and CATCHES the pre-fix round-and-pin placement at `Buggy=1`
//!   (counterexample).
//! * **Tier-1 (concrete, this file)** — the same invariants checked directly
//!   against the shipping pure fns over a dyadic lattice far denser than the
//!   model's (plus adversarial float specials), PLUS a point-for-point
//!   conformance drive of the model's own executable interpreter against
//!   `ct_pen_and_bearing` over the model's entire domain (model ↔ code can't
//!   drift). The FFI seam itself is bound on real Menlo ink by
//!   `coretext_placement_binds_to_the_pure_policy` in aterm-render's lib tests.
//!
//! ## Honest float scope (the ty waiver)
//!
//! `ty`'s Expr language is integer-only (no `*`, `/`, floats), so the
//! FLOAT-EXACTNESS half of the invariant cannot be a ty model; per the repo's
//! box-drawing rounding-law precedent it is carried here by a dyadic lattice on
//! which every asserted equality is EXACT by representability (1/64 steps —
//! finer than any real bearing distinction that survives 8-bit coverage), plus
//! ulp-bounded adversarial specials. The one genuine float boundary — a `b`
//! within one ulp below an integer saturates the computed phase to exactly
//! `1.0` — is asserted below and absorbed by the extent's `+1` headroom.
//!
//! Scope: the CoreText path only. The portable fontdue path (the test-stable
//! default; `ATERM_RASTERIZER=fontdue`) is untouched, so the CPU/GPU parity
//! suites are unaffected — and on macOS, where the parity tests do exercise
//! CoreText, both backends pull the SAME `glyph_image` bytes/placement, so they
//! move together by construction.

use aterm_render::{ct_padded_extent, ct_pen_and_bearing};
use aterm_spec::derive::ct_frac_bearing_model;

/// THE FLOOR LAW, exhaustive over the dyadic 1/64-px lattice spanning ±64px
/// (every value exactly representable, so every equality below is EXACT — no
/// rounding excuses): `min <= b < min + 1`, the pen is the negated integer
/// part with no rounding, the in-bitmap phase is `b - min ∈ [0, 1)`, and
/// integer bearing + phase reconstructs the designed position bit-exactly.
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
        // PROVE (1): bearing + phase reconstructs the designed position EXACTLY.
        assert_eq!(
            minf + phase,
            b,
            "floor(b) + (b - floor(b)) must reconstruct b exactly at b={b}"
        );
        if phase > 0.0 {
            nonzero_phase_seen += 1;
        }
    }
    // NON-VACUITY: fractional bearings (the case the pre-fix pin destroyed)
    // dominate the lattice.
    assert!(
        nonzero_phase_seen > 4000,
        "the lattice must reach fractional phases ({nonzero_phase_seen})"
    );
}

/// Adversarial float specials: signed zero, exact halves, values one ulp off an
/// integer, a typical fractional px, and a large-magnitude bearing. The floor
/// law and the integer pen stay EXACT everywhere; the reconstruction is exact
/// except within one ulp of an integer from below, where the computed phase
/// saturates to exactly `1.0` (documented; the extent's `+1` absorbs it) and
/// the reconstruction error is bounded by one epsilon — sub-attopixel.
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
    // NON-VACUITY: the one genuine float boundary (phase saturating to exactly
    // 1.0) is reached — this is precisely what the extent's +1 must absorb.
    assert!(
        saturated_seen > 0,
        "a b one half-ulp below an integer must saturate the phase to 1.0"
    );
}

/// PROVE (2): the bitmap NEVER clips for any fractional phase — over an ink
/// lattice of 1/64-px steps up to 48px, both pad levels (1 = plain, 2 =
/// `font_thicken`) and every phase in [0, 1] (1.0 included, the saturated
/// boundary above): `phase + ink + 2*pad <= extent`, with at least one full px
/// of headroom over the padded ink box (`extent >= ink + 2*pad + 1`), and the
/// thickened box stays exactly 1px-per-side larger than the plain one (the
/// relation `coretext_rasterizes_real_coverage` pins on real ink). All values
/// are dyadic, so every comparison is exact.
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
            let old = (ink + 2.0 * pad).ceil(); // the pre-fix extent (no +1)
            for q in 0i64..=64 {
                let phase = q as f64 / 64.0;
                // No clipping at ANY phase: the padded ink's trailing edge stays
                // inside the bitmap.
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
    // NEGATIVE CONTROL: without the +1 (the pre-fix `ceil(ink + 2*pad)` box) a
    // near-1 phase genuinely clips the trailing edge — the +1 is load-bearing.
    assert!(
        old_extent_would_clip > 0,
        "the pre-fix extent must clip for some (ink, phase) — otherwise the +1 proves nothing"
    );
}

/// NEGATIVE CONTROL for the placement: the pre-fix policy (`pen = -b`, bearing
/// `round(b)`, phase pinned to 0) genuinely mis-places glyphs — on the same
/// lattice where the fixed policy reconstructs EXACTLY (asserted above), the
/// old reported position misses the designed one by >= 0.25px on a quarter of
/// the lattice, and the old pen destroys the phase (a non-integer translate
/// pinned the ink to the bitmap grid).
#[test]
fn old_round_and_pin_placement_is_rejected() {
    let mut half_px_error_seen = 0usize;
    let mut phase_destroyed_seen = 0usize;
    for k in -4096i64..=4096 {
        let b = k as f64 / 64.0;
        let (pen, min) = ct_pen_and_bearing(b);
        // Fixed: exact at every lattice point (re-asserted for the comparison).
        assert_eq!(f64::from(min) + (b + pen), b);
        // Pre-fix reported position = round(b) with the phase discarded.
        if (b.round() - b).abs() >= 0.25 {
            half_px_error_seen += 1;
        }
        // Pre-fix pen -b: off the integer px grid, the raster translate was
        // fractional — i.e. the outline got re-pinned, losing its phase.
        if -b != (-b).floor() {
            phase_destroyed_seen += 1;
        }
    }
    assert!(
        half_px_error_seen > 2000,
        "the >=0.25px error class must be reached ({half_px_error_seen})"
    );
    assert!(
        phase_destroyed_seen > 4000,
        "the old pen must be non-integer somewhere ({phase_destroyed_seen})"
    );
}

/// Tier-1 MODEL ↔ CODE conformance: drive the `CtFracBearing` model's own
/// executable interpreter (`Model::fire` — the same semantics `ty` checks) over
/// its ENTIRE bounded domain (eighth-px bearings in ±3px) and assert the
/// reported `(bearing, rem)` equals the shipping `ct_pen_and_bearing` at every
/// point. The abstract twin and the real policy cannot drift.
#[test]
fn pen_and_bearing_conforms_to_the_ty_checked_model() {
    let m = ct_frac_bearing_model();
    let mut negative_seen = 0usize;
    let mut fractional_seen = 0usize;
    for b_e in -24i64..=24 {
        // Enter the model at phase 2 (PickB/Latch are the nondeterministic
        // enumeration this loop performs: rem = b, bearing = 0).
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
    // NON-VACUITY: negative bearings (floor != truncate) and off-grid bearings
    // (the class the pre-fix round-and-pin got wrong) were both walked.
    assert!(negative_seen > 0 && fractional_seen > 0);
}
