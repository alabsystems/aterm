// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Rendering, text, and contrast gate models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// COLOUR-PRESENTATION GATE — a code point that defaults to TEXT presentation is
/// never resolved to the colour-emoji face. The abstract twin of aterm-render's
/// `select_face` (the real-code binding is aterm-render's
/// `select_face_never_colors_text_presentation` exhaustive test).
///
/// This is the model of the ⏺ (U+23FA) fix: `select_face` used to choose the
/// colour-emoji face for ANY code point the monochrome faces missed but Apple
/// Color Emoji covered — ignoring the Unicode `Emoji_Presentation` property.
/// U+23FA is `Emoji=Yes` but `Emoji_Presentation=No`, so it defaults to text; the
/// reference terminals gate the colour face on that property (iTerm2:
/// `emojiWithDefaultEmojiPresentation` membership; Ghostty:
/// `uucode.get(.is_emoji_presentation, cp)`), never on raw font coverage.
///
/// Scalar projection `<<wants_emoji, color>>`: `wants_emoji` = 1 iff the code
/// point has default emoji presentation (or an explicit VS16) — the only
/// legitimate trigger for colour; `color` = the gate's output (1 = resolved to
/// the colour face), RECOMPUTED from `wants_emoji` in the same step (face
/// selection is stateless / per-call, so the decision is never stale). The two
/// `Want*` actions spread the nondeterministic input — a default-emoji code point
/// vs a default-text one — over the reachable space.
///
/// `Buggy` gates the SHIPPED defect: with `Buggy = 0` (committed) `color` is set
/// ONLY when `wants_emoji`; with `Buggy = 1` it is set regardless (the old
/// coverage-only gate), so a default-TEXT code point gets `color = 1` and
/// `NoColorForText` is violated. Thus `ty` PROVES the gate (Buggy=0) and CATCHES
/// the real regression (Buggy=1 → counterexample). Exercises a constant-guarded
/// `if` update and a two-action disjunctive `Next`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn presentation_gate_model() -> Model {
    // color' = if (wants_emoji' OR Buggy) then 1 else 0. TLA primed semantics use
    // unprimed vars on the RHS, so substitute the LITERAL value each action is
    // about to assign to `wants_emoji` (1 for WantEmoji, 0 for WantText).
    let color_for = |wants_emoji_next: i64| {
        if_(
            gt(add(int(wants_emoji_next), cst("Buggy")), int(0)),
            int(1),
            int(0),
        )
    };
    Model {
        name: "PresentationGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "wants_emoji",
                init: 0,
            },
            StateVar {
                name: "color",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // The code point has default-emoji presentation (or VS16): colour allowed.
                name: "WantEmoji",
                guard: Some(le(var("wants_emoji"), int(0))),
                updates: vec![
                    Update {
                        var: "wants_emoji",
                        expr: int(1),
                    },
                    Update {
                        var: "color",
                        expr: color_for(1),
                    },
                ],
            },
            Action {
                // A default-text code point: colour must be withheld (the fix).
                name: "WantText",
                guard: Some(gt(var("wants_emoji"), int(0))),
                updates: vec![
                    Update {
                        var: "wants_emoji",
                        expr: int(0),
                    },
                    Update {
                        var: "color",
                        expr: color_for(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // A colour resolution implies the code point wanted emoji presentation.
            name: "NoColorForText",
            expr: le(var("color"), var("wants_emoji")),
        }],
    }
}

/// LIGATURE SLICING GATE (M4) — the conservative shaping-acceptance policy that
/// admits EXACTLY the two grid-mappable shape forms and rejects everything else.
/// The real decision is aterm-render's pure
/// [`aterm_render::ligature_shaping::classify_shape`] (the shaping seam in
/// `shape_ligature_run`); the Tier-1 binding is aterm-render's
/// `tests/ligature_slice.rs::classify_shape_lattice` (the SAME conservativeness
/// invariant, enumerated over the small-count lattice) and the `gate_*` kani
/// proofs.
///
/// The abstraction: a shape has `n_in` input cells and `n_out` output glyphs, and
/// a config flag `admit` (Cascadia N:1 admission, default OFF). The gate ACCEPTS
/// iff the shape is grid-mappable — either 1:1 (`n_out == n_in`, the shipping
/// Fira/JetBrains spacer form) or, WHEN `admit`, an N:1 collapse (`n_out == 1`,
/// `n_in >= 2`, the Cascadia form). `ty` PROVES `ConservativeAccept` at `Buggy=0`
/// (`accept` never exceeds the grid-mappable envelope — no non-mappable shape ever
/// reaches the blitter) over the whole bounded `(n_in, n_out, admit)` space, and
/// CATCHES the defect at `Buggy=1` (a gate that admits a collapse WITHOUT the
/// flag — dropping the `admit` guard — which draws a wide glyph the slicing path
/// is not wired for) -> counterexample. The N:1 tile arithmetic (slice at `cell_w`
/// boundaries) is NOT here — ty has no multiplication — it is the L0 lattice proof
/// in `tests/ligature_slice.rs` (the M4 rounding/scaling waiver).
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ligature_gate_model() -> Model {
    // Cap on n_in / n_out — enough to reach 1:1, N:1 collapse (N up to 4), partial
    // collapse (3:2), and expansion (2:3).
    let n = 4;
    // The two grid-mappable predicates over the UNPRIMED (picked) counts.
    let one_to_one = || and_(gt(var("n_in"), int(0)), eq(var("n_out"), var("n_in")));
    let collapse = || and_(eq(var("n_out"), int(1)), gt(var("n_in"), int(1)));
    // accept' = 1:1 ? 1 : (collapse AND (admit OR Buggy)) ? 1 : 0. Buggy weakens the
    // `admit` guard, reproducing the "admit collapse without the flag" defect.
    let admit_or_buggy = gt(add(var("admit"), cst("Buggy")), int(0));
    let accept_expr = if_(
        one_to_one(),
        int(1),
        if_(and_(collapse(), admit_or_buggy), int(1), int(0)),
    );
    Model {
        name: "LigatureGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "n_in",
                init: 0,
            },
            StateVar {
                name: "n_out",
                init: 0,
            },
            StateVar {
                name: "admit",
                init: 0,
            },
            StateVar {
                name: "accept",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Pick a shape: any (n_in, n_out) in 0..=N and any flag setting.
                name: "Pick",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "n_in",
                        expr: in_range(int(0), int(n)),
                    },
                    Update {
                        var: "n_out",
                        expr: in_range(int(0), int(n)),
                    },
                    Update {
                        var: "admit",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "accept",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Classify the picked shape (classify_shape); loop back so the state
                // space is the whole picked lattice with no deadlock.
                name: "Classify",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "accept",
                        expr: accept_expr,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // CONSERVATIVENESS: an accepted shape is ALWAYS grid-mappable — 1:1, or
            // an N:1 collapse WITH the flag. `accept <= grid_mappable`. Note the
            // envelope uses the TRUE `admit`, so a Buggy accept of an unflagged
            // collapse breaks it.
            name: "ConservativeAccept",
            expr: le(
                var("accept"),
                if_(
                    or_(one_to_one(), and_(collapse(), eq(var("admit"), int(1)))),
                    int(1),
                    int(0),
                ),
            ),
        }],
    }
}

/// HDR PRESENT GATE (M3 phase B) — the EDR ("HDR glow") present policy: when is
/// the swapchain `Rgba16Float`, when does the blit linear-decode, when does
/// the >1.0 aurora pass run. The real decisions are aterm-gpu's pure
/// `format_plan::hdr_swapchain_wants_f16` (the Attach seam,
/// `GpuRenderer::create_window_surface`) and `format_plan::hdr_present_plan`
/// (the Present seam, `present_input`); the Tier-1 binding is aterm-gpu's
/// `tests/hdr_gate.rs` exhaustive 2^3 enumeration of BOTH shipping functions
/// chained Attach→Present, asserting THESE invariants (a complete proof — the
/// domain is finite booleans). The float clamp laws the gate FEEDS (grid ≤ 1.0,
/// additive ≤ EDR headroom) are proven separately in `aterm_render::hdr`
/// (exhaustive sweeps + trust-mc harnesses) — ty carries the boolean policy,
/// per the derive-layer division of labour.
///
/// Scalar projection `<<hdr_glow, supports_f16, glow, attached, is_f16,
/// blit_hdr, boost>>`: `Observe` spreads the three input facts
/// nondeterministically (config opt-in, surface capability, aurora presence)
/// and resets the window lifecycle; `Attach` picks the swapchain format ONCE
/// (`is_f16' = hdr_glow ∧ supports_f16`); `Present` (repeatable) derives the
/// blit decode from the ACTUAL format (`blit_hdr' = is_f16` — the encode
/// follows the surface, so live config flips degrade safely) and the aurora
/// pass from all three (`boost' = hdr_glow ∧ is_f16 ∧ glow`).
///
/// `Buggy` gates the DEFECT CLASS the gate exists to exclude: with `Buggy = 1`
/// Attach picks f16 whenever the surface supports it — ignoring the config —
/// so an untouched default install lands on an EDR swapchain and
/// `SdrInvariance` (hdr_glow off ⇒ nothing HDR ever happens) is violated. Thus
/// `ty` PROVES the invariance (Buggy=0) and CATCHES the regression (Buggy=1 →
/// counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn hdr_present_gate_model() -> Model {
    // is_f16' at Attach: the shipping policy (hdr_glow AND supports_f16), or the
    // Buggy=1 defect (supports_f16 alone — config ignored).
    let attach_pick = if_(
        gt(cst("Buggy"), int(0)),
        var("supports_f16"),
        if_(
            and_(gt(var("hdr_glow"), int(0)), gt(var("supports_f16"), int(0))),
            int(1),
            int(0),
        ),
    );
    Model {
        name: "HdrPresentGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "hdr_glow",
                init: 0,
            },
            StateVar {
                name: "supports_f16",
                init: 0,
            },
            StateVar {
                name: "glow",
                init: 0,
            },
            StateVar {
                name: "attached",
                init: 0,
            },
            StateVar {
                name: "is_f16",
                init: 0,
            },
            StateVar {
                name: "blit_hdr",
                init: 0,
            },
            StateVar {
                name: "boost",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A window lifecycle begins: observe the config opt-in, the
                // surface's f16 capability, and whether aurora quads exist this
                // session (all 2^3 combinations); reset the derived state.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "hdr_glow",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "supports_f16",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "glow",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "attached",
                        expr: int(0),
                    },
                    Update {
                        var: "is_f16",
                        expr: int(0),
                    },
                    Update {
                        var: "blit_hdr",
                        expr: int(0),
                    },
                    Update {
                        var: "boost",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Surface attach: pick the swapchain format once
                // (format_plan::hdr_swapchain_wants_f16).
                name: "Attach",
                guard: Some(le(var("attached"), int(0))),
                updates: vec![
                    Update {
                        var: "attached",
                        expr: int(1),
                    },
                    Update {
                        var: "is_f16",
                        expr: attach_pick,
                    },
                ],
            },
            Action {
                // A present on the attached surface
                // (format_plan::hdr_present_plan): the blit decode follows the
                // ACTUAL format; the aurora pass needs config AND format AND glow.
                name: "Present",
                guard: Some(gt(var("attached"), int(0))),
                updates: vec![
                    Update {
                        var: "blit_hdr",
                        expr: var("is_f16"),
                    },
                    Update {
                        var: "boost",
                        expr: if_(
                            and_(
                                gt(var("hdr_glow"), int(0)),
                                and_(gt(var("is_f16"), int(0)), gt(var("glow"), int(0))),
                            ),
                            int(1),
                            int(0),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // SDR INVARIANCE (the M3 phase-B contract): hdr_glow off ⇒ no
                // f16 swapchain, no linear decode, no >1.0 aurora pass — the
                // present is byte-identical to pre-M3. hdr_glow = 0 =>
                // is_f16 = 0 /\ blit_hdr = 0 /\ boost = 0.
                name: "SdrInvariance",
                expr: or_(
                    gt(var("hdr_glow"), int(0)),
                    and_(
                        eq(var("is_f16"), int(0)),
                        and_(eq(var("blit_hdr"), int(0)), eq(var("boost"), int(0))),
                    ),
                ),
            },
            Invariant {
                // A >1.0 emission can only land on a float swapchain whose blit
                // decoded to linear: boost <= blit_hdr (and blit_hdr <= is_f16).
                name: "BoostNeedsLinearF16",
                expr: and_(
                    le(var("boost"), var("blit_hdr")),
                    le(var("blit_hdr"), var("is_f16")),
                ),
            },
            Invariant {
                // The EDR format is never picked without surface support —
                // holds even under Buggy=1 (whose violation is config, not caps).
                name: "F16NeedsSupport",
                expr: le(var("is_f16"), var("supports_f16")),
            },
        ],
    }
}

/// HDR RECONFIGURE RE-TAG (M3 lifecycle completion) — reconfiguring an f16
/// DX12 surface for resize, a live composite-alpha change, or Outdated/Lost
/// recovery recreates its swapchain and clears the DXGI colour-space tag.
/// Windows may also disable HDR without forcing any of those surface events, so
/// the same decision follows a bounded live validation. The surface may remain
/// f16 only when re-establishing/confirming scRGB succeeds. Otherwise the
/// transition atomically selects the retained SDR format and changes capture
/// metadata from extended-linear to sRGB before another present.
///
/// Scalar projection `<<stage, retagged, is_f16, capture_linear>>`: stage 0 is a
/// confirmed f16/scRGB surface. `RetagSucceeds` preserves that pairing;
/// `RetagFails` performs the required two-field fallback.
/// `EnterSdrFallback -> UpgradeSucceeds/UpgradeFails` models the symmetric
/// same-size Windows HDR-on path from a retained eligible SDR surface.
/// `Buggy=1` recreates both defects: ignore a failed live re-tag, or leave the
/// attempted upgrade f16 after its tag fails.
///
/// Tier-1 is `aterm-gpu/tests/hdr_gate.rs`: it drives the shipping
/// `hdr_reconfigure_plan`, projects both concrete outcomes onto these variables,
/// checks the real transitions against this model, and includes the old
/// ignore-failure policy as a negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn hdr_reconfigure_retag_model() -> Model {
    crate::ty_model! {
        HdrReconfigureRetag {
            const Buggy = 0;
            // 0 confirmed HDR, 1 retained eligible SDR, 2 recovery resolved.
            var stage = 0;
            var retagged = 0;
            var is_f16 = 1;
            var capture_linear = 1;

            action RetagSucceeds when (stage == 0) {
                stage = 2;
                retagged = 1;
                is_f16 = 1;
                capture_linear = 1;
            }

            action RetagFails when (stage == 0) {
                stage = 2;
                retagged = 0;
                is_f16 = if Buggy == 1 { 1 } else { 0 };
                capture_linear = if Buggy == 1 { 1 } else { 0 };
            }

            action EnterSdrFallback when (stage == 0) {
                stage = 1;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 0;
            }

            action UpgradeSucceeds when (stage == 1) {
                stage = 2;
                retagged = 1;
                is_f16 = 1;
                capture_linear = 1;
            }

            action UpgradeFails when (stage == 1) {
                stage = 2;
                retagged = 0;
                is_f16 = if Buggy == 1 { 1 } else { 0 };
                capture_linear = 0;
            }

            invariant FailedRetagFallsBackAtomically:
                if stage == 2 && retagged == 0 {
                    is_f16 == 0 && capture_linear == 0
                } else {
                    is_f16 <= 1
                };
            invariant ResolvedF16RequiresSuccessfulRetag:
                if stage == 2 && is_f16 == 1 {
                    retagged == 1
                } else {
                    retagged <= 1
                };
            invariant AwaitingUpgradeIsSdr:
                if stage == 1 {
                    is_f16 == 0 && capture_linear == 0
                } else {
                    stage == 0 || stage == 2
                };
            invariant CaptureMatchesSurfaceEncoding:
                capture_linear == is_f16;
            invariant ValuesBounded:
                stage <= 2 && retagged <= 1 &&
                is_f16 <= 1 && capture_linear <= 1;
        }
    }
}

/// CHROME FACE GATE — chrome typography to grid standard: which face draws a
/// chrome (tray/overlay) glyph. The real decision is aterm-gui's pure
/// `tray_raster::select_chrome_face`; the Tier-1 binding is that module's
/// exhaustive 2^3 enumeration test (`chrome_face_gate_exhaustive`), which
/// asserts THESE invariants on the shipping policy over its whole domain.
///
/// The chrome used to HARDCODE the embedded DejaVu for every glyph
/// (`tray_raster::font()`), even though the renderer had already resolved the
/// user's terminal face and discovered its real `-Bold` sibling. The fix: a
/// bold run whose real bold face covers the char takes the BOLD face; else a
/// char the user's primary covers takes the PRIMARY; the embedded DejaVu is
/// STRICTLY a coverage fallback (symbols like ⌘⇧⌃✓ the terminal face lacks).
///
/// Scalar projection `<<bold_run, bold_has, primary_has, resolved, pick>>`:
/// `Observe` spreads all 8 input combinations nondeterministically (three
/// `\in 0..1` updates — the full existential fan-out), then `Resolve` runs the
/// gate once over the observed facts; `pick` is its output (0 = embedded
/// fallback, 1 = primary, 2 = bold), meaningful while `resolved = 1`.
///
/// `Buggy` gates the SHIPPED defect: with `Buggy = 0` (committed) `Resolve`
/// applies the coverage policy; with `Buggy = 1` it pins `pick = 0` — the old
/// everything-is-DejaVu chrome — so a primary-covered char lands on the
/// embedded face and `EmbeddedOnlyAsCoverageFallback` is violated (and a
/// covered bold run loses its weight, violating `BoldHonoredWhenCovered`).
/// Thus `ty` PROVES the gate (Buggy=0) and CATCHES the regression (Buggy=1 →
/// counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn chrome_face_gate_model() -> Model {
    // pick' = IF Buggy THEN 0 (always the embedded face) ELSE the policy:
    // bold_run /\ bold_has -> 2; primary_has -> 1; else 0.
    let policy = if_(
        and_(gt(var("bold_run"), int(0)), gt(var("bold_has"), int(0))),
        int(2),
        if_(gt(var("primary_has"), int(0)), int(1), int(0)),
    );
    Model {
        name: "ChromeFaceGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "bold_run",
                init: 0,
            },
            StateVar {
                name: "bold_has",
                init: 0,
            },
            StateVar {
                name: "primary_has",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "pick",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A fresh glyph arrives: observe its coverage facts (all 2^3
                // combinations) and mark the pick stale until Resolve runs.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "bold_run",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "bold_has",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "primary_has",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "pick",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Run the gate once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "pick",
                        expr: if_(gt(cst("Buggy"), int(0)), int(0), policy),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // The embedded DejaVu is chosen ONLY when neither user face
                // covers the glyph: resolved /\ pick = 0 => ~primary_has /\
                // ~(bold_run /\ bold_has).
                name: "EmbeddedOnlyAsCoverageFallback",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        neq(var("pick"), int(0)),
                        and_(
                            eq(var("primary_has"), int(0)),
                            or_(le(var("bold_run"), int(0)), le(var("bold_has"), int(0))),
                        ),
                    ),
                ),
            },
            Invariant {
                // A bold run whose real bold face covers the char never
                // downgrades: resolved /\ bold_run /\ bold_has => pick = 2.
                name: "BoldHonoredWhenCovered",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        le(var("bold_run"), int(0)),
                        or_(le(var("bold_has"), int(0)), eq(var("pick"), int(2))),
                    ),
                ),
            },
        ],
    }
}

/// MOTION POLICY (W11) — reduced-motion totality: the abstract twin of
/// aterm-gui's pure `motion::MotionPolicy::resolve` + `amplitude` (the Tier-1
/// binding is aterm-gui's `reduced_motion_totality` test, which enumerates the
/// SAME 3×2×2 input domain × the full governed-effect set over the shipping
/// resolver — a complete proof, since the domain is finite).
///
/// The model abstracts the amplitude as ONE scalar over the whole governed set
/// (the per-effect arm is a constant 0 under Reduced), so a NEW governed effect
/// joins the proof through aterm-gui's `MotionEffect::ALL` + the exhaustive
/// `amplitude` match, not through an edit here — and a RETIRED one leaves the
/// same way (`MotionEffect::PkgProgressCard`, the floating progress card's
/// rainbow/sparkle/cat trim, went with the card on 2026-08-26; the status bars
/// that replaced it carry no time-driven decoration to govern).
///
/// Scalar projection `<<mode, sys, focused, resolved, policy, amp>>`: `Observe`
/// nondeterministically picks the three motion facts (`mode ∈ 0..2` =
/// auto|full|reduced, `sys`/`focused ∈ 0..1`) and marks the decision stale;
/// `Resolve` computes the policy (1 = Full ⟺ focused ∧ (full ∨ (auto ∧ ¬sys)))
/// and the animation amplitude (`amp ∈ {0,1}`, the integer twin of the f32
/// scalar every effect consumes) in the same step.
///
/// `Buggy` gates the PRE-W11 defect: the OS reduce-motion flag was never
/// queried (app_config.rs literally said "OS reduced-motion query is a future
/// refinement"), so with `Buggy = 1` the amplitude ignores `sys` under auto —
/// a focused auto-mode window keeps animating (`amp = 1`) while the policy is
/// Reduced, violating `ReducedImpliesZeroAmplitude`. `ty` PROVES the invariant
/// at Buggy=0 and CATCHES that regression at Buggy=1.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn motion_policy_model() -> Model {
    // Full ⟺ focused ∧ (mode = full ∨ (mode = auto ∧ ¬sys)).
    let full = and_(
        gt(var("focused"), int(0)),
        or_(
            eq(var("mode"), int(1)),
            and_(eq(var("mode"), int(0)), eq(var("sys"), int(0))),
        ),
    );
    // The COMMITTED amplitude equals the policy; the BUGGY one ignores `sys`
    // under auto (animate ⟺ focused ∧ mode ≠ reduced) — the shipped pre-fix
    // behavior.
    let buggy_full = and_(gt(var("focused"), int(0)), neq(var("mode"), int(2)));
    Model {
        name: "MotionPolicy",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "mode",
                init: 0,
            },
            StateVar {
                name: "sys",
                init: 0,
            },
            StateVar {
                name: "focused",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "policy",
                init: 0,
            },
            StateVar {
                name: "amp",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A frame begins: observe the three motion facts (all 3×2×2
                // combinations) and mark the decision stale until Resolve runs.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "mode",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "sys",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "focused",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "policy",
                        expr: int(0),
                    },
                    Update {
                        var: "amp",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Resolve the policy + amplitude once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "policy",
                        expr: if_(full.clone(), int(1), int(0)),
                    },
                    Update {
                        var: "amp",
                        expr: if_(
                            gt(cst("Buggy"), int(0)),
                            if_(buggy_full, int(1), int(0)),
                            if_(full, int(1), int(0)),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // THE W11 PROVE bullet: a Reduced policy has EXACTLY zero
                // animation amplitude — resolved /\ policy = 0 => amp = 0.
                name: "ReducedImpliesZeroAmplitude",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(gt(var("policy"), int(0)), eq(var("amp"), int(0))),
                ),
            },
            Invariant {
                // Unfocused demotion (W11b): resolved /\ ~focused => policy = 0.
                name: "UnfocusedIsReduced",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(gt(var("focused"), int(0)), eq(var("policy"), int(0))),
                ),
            },
            Invariant {
                // Non-vacuity twin: a Full policy animates at unit amplitude —
                // resolved /\ policy = 1 => amp = 1 (so the model cannot pass
                // with a constant-zero amplitude).
                name: "FullImpliesUnitAmplitude",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(le(var("policy"), int(0)), eq(var("amp"), int(1))),
                ),
            },
        ],
    }
}

/// Process-global serious-mode master gate.  Requested feature bits are kept
/// separate from their effective outputs so enabling the master can silence
/// every decorative/audio lane without destroying the user's configuration;
/// requests may even change while the gate is closed, and disabling serious
/// mode then reveals the latest requested vector exactly.
///
/// The eight requested bits cover the independently gated shipping families:
/// cursor trail/glow (including its sound), terminal BEL sound, sparkle/cat
/// decorations, matrix rain, stream fade, transient celebrations, native
/// Settings previews, and GPU bloom/shimmer. The retained level-up notice is a
/// second projection of the celebration request, ensuring already-materialized
/// celebration UI is hidden too. Static terminal content, cursor blink,
/// selection, and the visual bell are outside this model because serious mode
/// deliberately preserves them.
///
/// `Buggy=1` reproduces a partial master switch which forgets to mute the
/// cursor trail/audio lane.  The committed model proves both fail-closed
/// silence and exact restoration; the mutant must produce a counterexample.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serious_mode_model() -> Model {
    crate::ty_model! {
        SeriousMode {
            const Buggy = 0;
            var serious = 0;
            var want_trail = 1;
            var want_bell = 1;
            var want_sparkle = 1;
            var want_rain = 1;
            var want_fade = 1;
            var want_celebration = 1;
            var want_preview = 1;
            var want_gpu = 1;
            var trail = 1;
            var bell = 1;
            var sparkle = 1;
            var rain = 1;
            var fade = 1;
            var celebration = 1;
            var notice = 1;
            var preview = 1;
            var gpu = 1;
            action Enable when (serious == 0) {
                serious = 1;
                trail = if Buggy == 1 { want_trail } else { 0 };
                bell = 0;
                sparkle = 0;
                rain = 0;
                fade = 0;
                celebration = 0;
                notice = 0;
                preview = 0;
                gpu = 0;
            }
            action Disable when (serious == 1) {
                serious = 0;
                trail = want_trail;
                bell = want_bell;
                sparkle = want_sparkle;
                rain = want_rain;
                fade = want_fade;
                celebration = want_celebration;
                notice = want_celebration;
                preview = want_preview;
                gpu = want_gpu;
            }
            action ChangeTrail {
                want_trail = 1 - want_trail;
                trail = if serious == 1 { 0 } else { 1 - want_trail };
            }
            action ChangeBell {
                want_bell = 1 - want_bell;
                bell = if serious == 1 { 0 } else { 1 - want_bell };
            }
            action ChangeSparkle {
                want_sparkle = 1 - want_sparkle;
                sparkle = if serious == 1 { 0 } else { 1 - want_sparkle };
            }
            action ChangeRain {
                want_rain = 1 - want_rain;
                rain = if serious == 1 { 0 } else { 1 - want_rain };
            }
            action ChangeFade {
                want_fade = 1 - want_fade;
                fade = if serious == 1 { 0 } else { 1 - want_fade };
            }
            action ChangeCelebration {
                want_celebration = 1 - want_celebration;
                celebration = if serious == 1 { 0 } else { 1 - want_celebration };
                notice = if serious == 1 { 0 } else { 1 - want_celebration };
            }
            action ChangePreview {
                want_preview = 1 - want_preview;
                preview = if serious == 1 { 0 } else { 1 - want_preview };
            }
            action ChangeGpu {
                want_gpu = 1 - want_gpu;
                gpu = if serious == 1 { 0 } else { 1 - want_gpu };
            }
            invariant SeriousSilencesEverything:
                if serious == 1 {
                    trail == 0 && bell == 0 && sparkle == 0 &&
                    rain == 0 && fade == 0 && celebration == 0 &&
                    notice == 0 && preview == 0 && gpu == 0
                } else {
                    trail == want_trail && bell == want_bell &&
                    sparkle == want_sparkle && rain == want_rain &&
                    fade == want_fade && celebration == want_celebration &&
                    notice == want_celebration && preview == want_preview &&
                    gpu == want_gpu
                };
            invariant SeriousBitBounded: serious <= 1;
            invariant EffectiveBitsBounded:
                trail <= 1 && bell <= 1 && sparkle <= 1 &&
                rain <= 1 && fade <= 1 && celebration <= 1 &&
                notice <= 1 && preview <= 1 && gpu <= 1;
        }
    }
}

/// Bounded terminal incremental-search navigation.  This models the host-owned
/// Cmd-S/Cmd-R state machine, not the text-matching algorithm: opening captures
/// a viewport origin, a completed query publishes an ordered hit set, repeated
/// forward/backward chords move one ordinal with exact wrap, RET accepts the
/// selected hit, streaming output invalidates and deselects stale results before
/// one bounded refresh+repeat, and cancel restores the captured viewport.
/// `pty_writes` pins the critical terminal/TUI contract: these host chords never
/// reach the PTY.
///
/// `nav_work` is the deterministic work counter for a repeat transition over an
/// already materialized hit vector.  It is one regardless of hit count; query
/// construction has separate engine counters/benchmarks and is intentionally
/// not misrepresented as a wall-clock theorem here.
///
/// `Buggy=1` combines the two regressions the Tier-1 negative controls exercise:
/// opening leaks a byte to the PTY, and forward repeat scans `hits` entries while
/// stepping past the final ordinal instead of wrapping.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn emacs_search_navigation_model() -> Model {
    crate::ty_model! {
        EmacsSearchNavigation {
            const Buggy = 0;
            const Cap = 3;
            var active = 0;
            var forward = 1;
            var query = 0;
            var hits = 0;
            var current = 0;
            var origin = 0;
            var viewport = 0;
            var selection = 0;
            var dirty = 0;
            // 0 none/open, 1 cancelled, 2 accepted.
            var last_exit = 0;
            var pty_writes = 0;
            var nav_work = 0;
            action OpenForward when (active == 0) {
                active = 1;
                forward = 1;
                query = 0;
                hits = 0;
                current = 0;
                origin = viewport;
                selection = 0;
                dirty = 0;
                last_exit = 0;
                pty_writes = if Buggy == 1 { pty_writes + 1 } else { pty_writes };
                nav_work = 0;
            }
            action OpenBackward when (active == 0) {
                active = 1;
                forward = 0;
                query = 0;
                hits = 0;
                current = 0;
                origin = viewport;
                selection = 0;
                dirty = 0;
                last_exit = 0;
                pty_writes = if Buggy == 1 { pty_writes + 1 } else { pty_writes };
                nav_work = 0;
            }
            action PublishHit when (active == 1 && hits <= Cap - 1) {
                query = 1;
                hits = hits + 1;
                current = if forward == 1 { 0 } else { hits };
                viewport = if forward == 1 { 0 } else { hits };
                selection = 1;
                dirty = 0;
                last_exit = 0;
                nav_work = 0;
            }
            action PublishMiss when (active == 1) {
                query = 1;
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
                last_exit = 0;
                nav_work = 0;
            }
            action Output when (active == 1 && query == 1) {
                dirty = 1;
                selection = 0;
                nav_work = 0;
            }
            action RepeatForward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 1;
                current = if Buggy == 1 {
                    current + 1
                } else {
                    if current <= hits - 2 { current + 1 } else { 0 }
                };
                viewport = if Buggy == 1 {
                    current + 1
                } else {
                    if current <= hits - 2 { current + 1 } else { 0 }
                };
                selection = 1;
                last_exit = 0;
                nav_work = if Buggy == 1 { hits } else { 1 };
            }
            action RepeatBackward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                last_exit = 0;
                nav_work = 1;
            }
            action RefreshRepeatForward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 1;
                current = if current <= hits - 2 { current + 1 } else { 0 };
                viewport = if current <= hits - 2 { current + 1 } else { 0 };
                selection = 1;
                dirty = 0;
                nav_work = 1;
            }
            action RefreshRepeatBackward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                dirty = 0;
                nav_work = 1;
            }
            action RefreshMiss when (active == 1 && dirty == 1) {
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
                nav_work = 1;
            }
            action Cancel when (active == 1) {
                active = 0;
                viewport = origin;
                selection = 0;
                last_exit = 1;
                nav_work = 0;
            }
            action Accept when (active == 1 && dirty == 0) {
                active = 0;
                selection = if hits > 0 { 1 } else { 0 };
                last_exit = 2;
                nav_work = 0;
            }
            invariant NoPtyLeak: pty_writes == 0;
            invariant CurrentOrdinalBounded:
                if hits == 0 { current == 0 } else { 0 <= current && current <= hits - 1 };
            invariant HitCountBounded: hits <= Cap;
            invariant DirectionBounded: forward <= 1;
            invariant DirtyBounded: dirty <= 1;
            invariant StaleBatchNeverSelected:
                if dirty == 1 { selection == 0 } else { selection <= 1 };
            invariant CancelRestoresOrigin:
                if last_exit == 1 { viewport == origin && selection == 0 } else { viewport <= Cap };
            invariant AcceptKeepsHit:
                if last_exit == 2 && hits > 0 { selection == 1 } else { selection <= 1 };
            invariant RepeatWorkBounded: nav_work <= 1;
        }
    }
}

/// Independent non-vacuity twin for the already-materialized, non-truncated
/// match-vector step. Keeping this separate from the no-PTY-leak mutant ensures
/// `ty` must specifically catch linear-in-hit-count repeat work rather than
/// satisfying prove-and-catch with an earlier input-routing violation.
///
/// This is intentionally scoped to the host's cached vector step. Truncated
/// point lookup, cold snapshot/index construction, and matcher wall time are
/// bounded/measured by shipping-code counters and release tests, not claimed by
/// this abstract state machine.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn emacs_search_repeat_work_model() -> Model {
    crate::ty_model! {
        EmacsSearchRepeatWork {
            const Buggy = 0;
            const Cap = 3;
            var hits = 1;
            var current = 0;
            var work = 0;
            action AddHit when (hits <= Cap - 1) {
                hits = hits + 1;
            }
            action Step {
                current = if current <= hits - 2 { current + 1 } else { 0 };
                work = if Buggy == 1 { hits } else { 1 };
            }
            invariant CurrentOrdinalBounded: 0 <= current && current <= hits - 1;
            invariant RepeatWorkBounded: work <= 1;
        }
    }
}

/// SCROLL-GLIDE CONVERGENCE + POLICY SETTLEMENT (M1/W11) — the smooth-scroll
/// wheel glide's lifecycle discipline. Under Full motion, an armed glide makes
/// strict progress toward its target on every wake, disarms EXACTLY when it
/// lands there, and therefore wakes a BOUNDED number of times per arm (no
/// perpetual wake — the 0%-idle discipline). A Full→Reduced edge is stronger:
/// it atomically lands at the intended target and disarms, so Reduced owns no
/// glide deadline. The abstract twin of aterm-gui's `scroll_motion::Glide`
/// sampling + `App::settle_scroll_motion_at_target` reducer. The real-code
/// binding includes `glide_disarms_in_bounded_wakes`,
/// `glide_converges_monotonically_and_exactly`, and
/// `reduced_motion_settle_conforms_to_scroll_glide_model`.
///
/// Scalar projection `<<pos, target, armed, wakes, reduced>>` over a 0..N
/// position lane (N = 3): `Arm` starts a Full-policy glide at a nondeterministic
/// target (wake counter reset), `Retarget` redirects it mid-flight (a chained
/// wheel notch — the clock restarts, so the counter resets too), and `Wake` is
/// one Full-policy deadline firing: the position steps one unit toward the
/// target and the glide disarms iff it arrived. `SetReduced` is the accessibility
/// edge: position becomes target and armed becomes 0 in the same transition;
/// `SetFull` re-enables future arms. `wakes` saturates at N+1 so the state space
/// stays finite.
///
/// `Buggy` gates the policy-edge defect found by the settings audit: with
/// `Buggy = 0` (committed), every Full wake advances, at most N wakes elapse
/// before disarm (`BoundedWakes`), every disarmed glide sits at its target
/// (`DisarmedAtTarget`), and Reduced is both disarmed and landed
/// (`ReducedSettled`). With `Buggy = 1`, `SetReduced` retains the intermediate
/// position and armed deadline — the old whole-row sampling behavior — so
/// `ReducedSettled` yields a counterexample. Thus `ty` proves both ordinary
/// convergence and immediate Reduced settlement, and catches the actual stale-
/// deadline mutant.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn scroll_glide_model() -> Model {
    // One Full-policy wake's position step: toward the target by 1 (the
    // abstract ease tick).
    let step = || {
        if_(
            gt(var("pos"), var("target")),
            sub(var("pos"), int(1)),
            if_(
                gt(var("target"), var("pos")),
                add(var("pos"), int(1)),
                var("pos"),
            ),
        )
    };
    Model {
        name: "ScrollGlide",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "pos",
                init: 0,
            },
            StateVar {
                name: "target",
                init: 0,
            },
            StateVar {
                name: "armed",
                init: 0,
            },
            StateVar {
                name: "wakes",
                init: 0,
            },
            StateVar {
                name: "reduced",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A wheel notch arrives on an idle viewport: aim anywhere in
                // the 0..N lane and arm the glide (wake counter restarts).
                name: "Arm",
                guard: Some(and_(le(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "target",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "armed",
                        expr: int(1),
                    },
                    Update {
                        var: "wakes",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // A chained wheel notch mid-glide: redirect the target; the
                // ease clock restarts, so the per-arm wake budget does too.
                name: "Retarget",
                guard: Some(and_(gt(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "target",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "wakes",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // One Full-policy armed deadline fires: step toward the target,
                // count the wake (saturating at N+1 to keep the space finite),
                // and disarm iff the step LANDED on target.
                name: "Wake",
                guard: Some(and_(gt(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: step(),
                    },
                    Update {
                        var: "armed",
                        expr: if_(eq(step(), var("target")), int(0), int(1)),
                    },
                    Update {
                        var: "wakes",
                        expr: if_(
                            le(var("wakes"), int(3)),
                            add(var("wakes"), int(1)),
                            var("wakes"),
                        ),
                    },
                ],
            },
            Action {
                // Full→Reduced accessibility edge. Committed code lands and
                // disarms atomically. Buggy retains both the intermediate row and
                // its armed deadline, matching the audited scheduler defect.
                name: "SetReduced",
                guard: Some(le(var("reduced"), int(0))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: if_(gt(cst("Buggy"), int(0)), var("pos"), var("target")),
                    },
                    Update {
                        var: "armed",
                        expr: if_(gt(cst("Buggy"), int(0)), var("armed"), int(0)),
                    },
                    Update {
                        var: "reduced",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Restoring Full motion permits a later wheel gesture to arm.
                name: "SetFull",
                guard: Some(gt(var("reduced"), int(0))),
                updates: vec![Update {
                    var: "reduced",
                    expr: int(0),
                }],
            },
        ],
        invariants: vec![
            Invariant {
                // The no-perpetual-wake bound: one arm's ease is over within N
                // wakes (the farthest target is N cells away and every Full
                // wake advances).
                name: "BoundedWakes",
                expr: le(var("wakes"), int(3)),
            },
            Invariant {
                // Disarm happens EXACTLY at the target: armed = 0 => pos = target
                // (the glide never abandons the viewport short of where it aimed).
                name: "DisarmedAtTarget",
                expr: or_(gt(var("armed"), int(0)), eq(var("pos"), var("target"))),
            },
            Invariant {
                // Reduced motion has no retained ease/deadline and rests exactly
                // on the intended target in the same policy transition.
                name: "ReducedSettled",
                expr: or_(
                    le(var("reduced"), int(0)),
                    and_(le(var("armed"), int(0)), eq(var("pos"), var("target"))),
                ),
            },
        ],
    }
}

/// SUB-ROW SCROLL TRANSLATE PARTITION (M1b) — the chrome-exemption policy the
/// render-side sub-row translate enforces: a frame row is shifted by the
/// fractional-pixel residual IFF it lies in the terminal-content grid band
/// `[GridTop, GridBot)`. Chrome rows (the prepended tab strip below `GridTop`,
/// transient edge bars, and split dividers at/above `GridBot`) are PINNED. This is the
/// abstract twin of aterm-render's `scroll_translate::translate_grid_band_in_place`
/// (which writes ONLY the band's pixels); the Tier-1 binding is that module's
/// exhaustive `chrome_pixels_are_invariant` lattice test plus the real-renderer
/// `tests/scroll_frac_translate.rs::chrome_pixels_are_invariant_under_any_frac`.
///
/// Scalar projection over `row`: `Classify` spreads `row` across `0..GridBot+1`
/// (straddling the upper band boundary — the exact chrome/grid seam). The
/// invariant `ShiftOnlyInBand` states `shifted(row) <= in_band(row)` where
/// `in_band(row) = row ∈ [GridTop, GridBot)` and `shifted(row) = row ∈ [GridTop,
/// GridBot + Buggy)`: a shifted row is always in-band. `Buggy = 1` widens the
/// shift to `GridBot` inclusive — leaking the translate onto the first bottom-chrome
/// row (`row == GridBot`), the exact defect the band scissor exists to exclude —
/// so `ty` PROVES the policy at `Buggy = 0` and CATCHES the leak at `Buggy = 1`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn grid_translate_model() -> Model {
    // Indicator (0/1) that `row` lies in `[GridTop, hi)`: `GridTop <= row < hi`.
    let indicator = |hi: Expr| {
        if_(
            and_(le(cst("GridTop"), var("row")), gt(hi, var("row"))),
            int(1),
            int(0),
        )
    };
    Model {
        name: "GridTranslate",
        // GridTop=1 (row 0 is the tab strip), GridBot=3 (rows 3.. are bottom chrome).
        consts: vec![("Buggy", 0), ("GridTop", 1), ("GridBot", 3)],
        vars: vec![StateVar {
            name: "row",
            init: 0,
        }],
        fn_vars: vec![],
        actions: vec![Action {
            // Enumerate every frame row across the chrome/grid seam (0..GridBot+1).
            name: "Classify",
            guard: None,
            updates: vec![Update {
                var: "row",
                expr: in_range(int(0), add(cst("GridBot"), int(1))),
            }],
        }],
        invariants: vec![Invariant {
            // A shifted row is always a grid-band row: shifted(row) => in_band(row).
            // At Buggy=1 row==GridBot shifts (shifted=1) but is chrome (in_band=0).
            name: "ShiftOnlyInBand",
            expr: le(
                indicator(add(cst("GridBot"), cst("Buggy"))),
                indicator(cst("GridBot")),
            ),
        }],
    }
}

/// DECORATION BAND CONTAINMENT (cross-cutting theorem (c), W7) — the ordering
/// policy behind `aterm_render::deco::clamp_band` and every decoration WRITE
/// emitter (`underline_rects`, `strike_overline_rects`): a decoration band never
/// leaves its cell. The band `(top, thickness)` is clamped in a SPECIFIC ORDER —
/// thickness into `[1, cell_h]` FIRST, then top into `[0, cell_h − thickness]` —
/// which guarantees `top + thickness <= cell_h` for ANY raw input. This is the
/// abstract twin of that clamp; the Tier-1 bindings are the exhaustive lattice
/// tests `aterm-render/tests/deco_lines.rs::{resolved_bands_always_inside_the_cell,
/// decoration_writes_stay_within_the_run_band}` (the latter drives the real
/// emitters across every `UnderlineStyle`).
///
/// Purely additive (clamp = `min`/`max` via `if`, plus `+`/`−`/`<=`), so unlike
/// the resolver's per-em SCALING (which is `mul`, hence L0-only) the CONTAINMENT
/// ordering IS expressible in the `ty` Expr language. Scalar projection: pick any
/// `cell_h`, raw `thickness` and raw `top` on a bounded lattice (raw values may
/// exceed the cell — exercising the clamp), settle the thickness, then settle the
/// top against the thickness-aware bound. Safety `Contained`: `y + t <= cell_h`.
///
/// `Buggy = 1` reproduces the pre-clamp-order defect — the top is clamped against
/// the WHOLE cell (`cell_h`, ignoring the thickness) instead of `cell_h − t`, so a
/// low, thick band spills one or more rows past the cell bottom into the row below
/// (the "decoration escaped its cell" failure the emitters must never produce). So
/// `ty` PROVES `Contained` at `Buggy = 0` and CATCHES the spill at `Buggy = 1`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn deco_band_containment_model() -> Model {
    // clamp(raw_t, 1, cell_h): raw_t is >= 0 on the lattice, so `< 1` is `== 0`.
    let clamp_t = || {
        if_(
            le(var("raw_t"), int(0)),
            int(1),
            if_(gt(var("raw_t"), var("cell_h")), var("cell_h"), var("raw_t")),
        )
    };
    // The upper bound for the top: correct = cell_h - t (leaves room for the whole
    // stroke); Buggy = cell_h (ignores the thickness — the pre-fix order).
    let y_bound = || {
        if_(
            gt(cst("Buggy"), int(0)),
            var("cell_h"),
            sub(var("cell_h"), var("t")),
        )
    };
    // clamp(raw_top, 0, y_bound): raw_top >= 0, so only the upper clamp bites.
    let clamp_y = || if_(gt(var("raw_top"), y_bound()), y_bound(), var("raw_top"));
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(5)), body);
    Model {
        name: "DecoBandContainment",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "cell_h",
                init: 1,
            },
            StateVar {
                name: "raw_t",
                init: 0,
            },
            StateVar {
                name: "raw_top",
                init: 0,
            },
            StateVar { name: "t", init: 0 },
            StateVar { name: "y", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickCell",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "cell_h",
                        expr: in_range(int(1), int(5)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Raw thickness may exceed the cell (0 and > cell_h) — the clamp
                // must tame it either way.
                name: "PickThick",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "raw_t",
                        expr: in_range(int(0), int(7)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Raw top may point past the cell — the settle must pull it back.
                name: "PickTop",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "raw_top",
                        expr: in_range(int(0), int(7)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // Thickness FIRST — into [1, cell_h].
                name: "SettleThick",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "t",
                        expr: clamp_t(),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                // Top SECOND — against the (now committed) thickness-aware bound.
                name: "SettleTop",
                guard: Some(eq(var("phase"), int(4))),
                updates: vec![
                    Update {
                        var: "y",
                        expr: clamp_y(),
                    },
                    Update {
                        var: "phase",
                        expr: int(5),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // THE THEOREM: the settled band never leaves the cell bottom.
                // Buggy=1's cell_h bound lets a low thick band spill (y + t > cell_h).
                name: "Contained",
                expr: settled_implies(le(add(var("y"), var("t")), var("cell_h"))),
            },
            Invariant {
                // Always-true control (both Buggy values): the thickness clamp keeps
                // the stroke a visible, in-cell height — proves the model reaches a
                // settled non-degenerate band, so Contained is not checked vacuously.
                name: "ThicknessInCell",
                expr: or_(
                    neq(var("phase"), int(5)),
                    and_(le(int(1), var("t")), le(var("t"), var("cell_h"))),
                ),
            },
        ],
    }
}

/// STREAM-FADE BYPASS GATE (M2 "ink that dries") — streamed output may fade in
/// ONLY when no bypass holds; every bypass is a bypass TO INSTANT (exact
/// bytes). The abstract twin of aterm-gui's pure `stream_fade::fade_permitted`
/// (the Tier-1 binding is that module's exhaustive 2^5 `fade_gate_exhaustive`
/// test — a complete proof, since the domain is finite booleans — plus the
/// byte-identity pipeline test `bypass_is_byte_identical`).
///
/// Scalar projection `<<enabled, input_hot, alt_screen, scrolled_back,
/// reduced, resolved, fade>>`: `Observe` nondeterministically picks the five
/// gate facts (the full 2^5 fan-out) and marks the decision stale; `Resolve`
/// runs the gate once over the observed facts — `fade` = 1 iff the config is
/// on AND no bypass (keystroke echo in flight / alternate screen /
/// scrolled-back viewport / W11 Reduced motion) holds.
///
/// `Buggy` gates the taste defect the gate exists to prevent: with `Buggy = 1`
/// the `input_hot` fact is ignored, so a keystroke's echo fades in — typed
/// characters read as ADDED LATENCY — violating `InstantWhileTyping`. `ty`
/// PROVES all five bypass invariants (+ the non-vacuity twin) at Buggy=0 and
/// CATCHES the fading-echo regression at Buggy=1 (counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn stream_fade_gate_model() -> Model {
    // fade' = enabled ∧ ¬input_hot ∧ ¬alt ∧ ¬scrolled ∧ ¬reduced; under Buggy
    // the input_hot conjunct is dropped (the fading-keystroke-echo defect).
    let hot_clear = or_(le(var("input_hot"), int(0)), gt(cst("Buggy"), int(0)));
    let policy = if_(
        and_(
            gt(var("enabled"), int(0)),
            and_(
                hot_clear,
                and_(
                    le(var("alt_screen"), int(0)),
                    and_(le(var("scrolled_back"), int(0)), le(var("reduced"), int(0))),
                ),
            ),
        ),
        int(1),
        int(0),
    );
    // resolved ∧ <bypass> ⇒ fade = 0, in the or-form the checker consumes.
    let instant_when = |bypass: Expr| {
        or_(
            le(var("resolved"), int(0)),
            or_(bypass, eq(var("fade"), int(0))),
        )
    };
    Model {
        name: "StreamFadeGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "enabled",
                init: 0,
            },
            StateVar {
                name: "input_hot",
                init: 0,
            },
            StateVar {
                name: "alt_screen",
                init: 0,
            },
            StateVar {
                name: "scrolled_back",
                init: 0,
            },
            StateVar {
                name: "reduced",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
            StateVar {
                name: "fade",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A frame arrives: observe the five gate facts (all 2^5
                // combinations) and mark the decision stale until Resolve runs.
                name: "Observe",
                guard: None,
                updates: vec![
                    Update {
                        var: "enabled",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "input_hot",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "alt_screen",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "scrolled_back",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "reduced",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "resolved",
                        expr: int(0),
                    },
                    Update {
                        var: "fade",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Run the gate once over the observed facts.
                name: "Resolve",
                guard: Some(le(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    Update {
                        var: "fade",
                        expr: policy,
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // THE M2 taste gate: a keystroke echo in flight renders instant
                // — resolved ∧ input_hot ⇒ fade = 0. (Buggy=1 violates this.)
                name: "InstantWhileTyping",
                expr: instant_when(le(var("input_hot"), int(0))),
            },
            Invariant {
                // Full-screen programs never smear: resolved ∧ alt ⇒ fade = 0.
                name: "InstantOnAltScreen",
                expr: instant_when(le(var("alt_screen"), int(0))),
            },
            Invariant {
                // History is settled ink: resolved ∧ scrolled ⇒ fade = 0.
                name: "InstantScrolledBack",
                expr: instant_when(le(var("scrolled_back"), int(0))),
            },
            Invariant {
                // W11: resolved ∧ reduced-motion ⇒ fade = 0.
                name: "InstantUnderReducedMotion",
                expr: instant_when(le(var("reduced"), int(0))),
            },
            Invariant {
                // The master switch: resolved ∧ ¬enabled ⇒ fade = 0.
                name: "InstantWhenDisabled",
                expr: instant_when(gt(var("enabled"), int(0))),
            },
            Invariant {
                // Non-vacuity twin: with the config on and every bypass clear
                // the fade DOES run — the invariants cannot pass with a
                // constant-zero gate. resolved ∧ enabled ∧ all-clear ⇒ fade = 1.
                name: "FadesWhenPermitted",
                expr: or_(
                    le(var("resolved"), int(0)),
                    or_(
                        le(var("enabled"), int(0)),
                        or_(
                            gt(var("input_hot"), int(0)),
                            or_(
                                gt(var("alt_screen"), int(0)),
                                or_(
                                    gt(var("scrolled_back"), int(0)),
                                    or_(gt(var("reduced"), int(0)), eq(var("fade"), int(1))),
                                ),
                            ),
                        ),
                    ),
                ),
            },
        ],
    }
}

/// AA EDGE-HARDENING (the seam-tiling law) — an anti-aliased procedural glyph's
/// CELL-EDGE texel is always hard 0/MAX after the border-hardening pass. The
/// abstract twin of aterm-render's `procedural::Canvas::harden_edges` (the
/// real-code binding is aterm-render's `procedural_aa_edges.rs` exhaustive
/// size-lattice test — Tier-1).
///
/// This models the AA overhaul's safety property: diagonals/arcs/Powerline/
/// wedges rasterize at 4×4 subsamples per pixel (raw box-filter coverage
/// `raw ∈ 0..=16` here, standing for the 0..=255 byte), but a FRACTIONAL texel
/// on the cell boundary would compose a visible half-covered seam line between
/// adjacent cells (and blend differently on CPU vs GPU). So the rasterizer
/// hardens border texels: majority coverage (`raw >= 8`) becomes full, the
/// rest empty — interior texels keep their AA value.
///
/// Scalar projection `<<edge, raw, out, pc>>`: `Observe` nondeterministically
/// picks a texel position (`edge`) and its raw supersampled coverage (`raw`,
/// via `in_range` — this model exercises the nondeterministic-update path);
/// `Filter` computes the emitted byte. `Buggy` gates the defect: with
/// `Buggy = 0` (committed) an edge texel quantizes to {0, 16}; with `Buggy = 1`
/// the hardening pass is skipped (`out = raw`), so an edge texel keeps
/// fractional coverage and `EdgeTexelsHard` is violated. Thus `ty` PROVES the
/// seam-tiling law (Buggy=0) and CATCHES the soft-seam regression (Buggy=1 →
/// counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn aa_edge_hardening_model() -> Model {
    Model {
        name: "AaEdgeHarden",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "edge",
                init: 0,
            },
            StateVar {
                name: "raw",
                init: 0,
            },
            StateVar {
                name: "out",
                init: 0,
            },
            StateVar {
                name: "pc",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // A fresh texel: position (interior/edge) + raw AA coverage.
                // `out` resets so the invariant holds mid-pipeline.
                name: "Observe",
                guard: Some(eq(var("pc"), int(0))),
                updates: vec![
                    Update {
                        var: "edge",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "raw",
                        expr: in_range(int(0), int(16)),
                    },
                    Update {
                        var: "out",
                        expr: int(0),
                    },
                    Update {
                        var: "pc",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // The hardening pass: edge texels quantize (majority wins);
                // interior texels pass through. Buggy=1 skips the pass.
                name: "Filter",
                guard: Some(eq(var("pc"), int(1))),
                updates: vec![
                    Update {
                        var: "out",
                        expr: if_(
                            and_(eq(cst("Buggy"), int(0)), eq(var("edge"), int(1))),
                            if_(gt(var("raw"), int(7)), int(16), int(0)),
                            var("raw"),
                        ),
                    },
                    Update {
                        var: "pc",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // An edge texel is never fractional: out ∈ {0, 16} whenever edge=1.
            name: "EdgeTexelsHard",
            expr: or_(
                eq(var("edge"), int(0)),
                or_(eq(var("out"), int(0)), eq(var("out"), int(16))),
            ),
        }],
    }
}

/// SHADE DITHER PHASE (the uniform-period law) — the ░▒▓ dither pattern is a
/// function of ABSOLUTE framebuffer column parity, so a run of shade cells
/// tiles with a uniform 2-pixel period across every cell seam. The abstract
/// twin of aterm-render's `procedural::shade` + `shade_phase_key` (the
/// real-code binding is aterm-render's `shade_phase.rs` — Tier-1: the same
/// invariant over composed adjacent cells at cell_w = 9, plus the rendered
/// end-to-end frame).
///
/// This is the model of the odd-cell-width banding fix: shades used CELL-LOCAL
/// parity, so with `W = 9` every cell restarted its dither at local x = 0 and
/// the seam columns 8/9 held the SAME pattern — a doubled dither line at EVERY
/// seam. The fix keys each cell's pattern by the parity of its absolute pixel
/// origin (the phase), carried into the glyph key.
///
/// Scalar projection: the scanner walks absolute columns left-to-right across
/// consecutive `W`-wide cells. `parity` = absolute column parity; `local` =
/// cell-local column (0..W-1); `localpar` = its parity; `cellpar` = the
/// current cell's origin parity (the PHASE the fix feeds the rasterizer);
/// `lit` = the ░-row pattern at this column (lit on even pattern positions).
/// `Buggy` gates the defect: with `Buggy = 0` the phase is the cell origin's
/// parity (committed); with `Buggy = 1` the phase is 0 (cell-local dithering).
/// Invariant `UniformPeriod`: `lit + parity = 1` — the pattern IS the absolute
/// parity function, so two consecutive columns can never repeat (the doubled
/// line is impossible). `ty` PROVES it at Buggy=0 and CATCHES the doubled
/// seam line at Buggy=1 (violated at the first seam, absolute column 9).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn shade_phase_model() -> Model {
    // Expressions for the POST-step values, all over unprimed state (TLA
    // primed semantics): the step advances to absolute column x+1.
    let boundary = || eq(var("local"), sub(cst("W"), int(1)));
    let next_localpar = || if_(boundary(), int(0), sub(int(1), var("localpar")));
    let next_cellpar = || if_(boundary(), sub(int(1), var("parity")), var("cellpar"));
    // The phase the rasterizer is handed for the (possibly new) cell.
    let phase = || if_(gt(cst("Buggy"), int(0)), int(0), next_cellpar());
    Model {
        name: "ShadePhase",
        consts: vec![("Buggy", 0), ("W", 9)],
        vars: vec![
            StateVar {
                name: "parity",
                init: 0,
            },
            StateVar {
                name: "local",
                init: 0,
            },
            StateVar {
                name: "localpar",
                init: 0,
            },
            StateVar {
                name: "cellpar",
                init: 0,
            },
            // Column 0: local 0 + phase 0 → the ░ row is lit there.
            StateVar {
                name: "lit",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            // Advance one absolute column (wrapping into the next cell at the
            // W boundary — the seam the doubled line appeared at).
            name: "Advance",
            guard: None,
            updates: vec![
                Update {
                    var: "parity",
                    expr: sub(int(1), var("parity")),
                },
                Update {
                    var: "local",
                    expr: if_(boundary(), int(0), add(var("local"), int(1))),
                },
                Update {
                    var: "localpar",
                    expr: next_localpar(),
                },
                Update {
                    var: "cellpar",
                    expr: next_cellpar(),
                },
                Update {
                    // lit' = (local' + phase') is even ⟺ localpar' == phase'.
                    var: "lit",
                    expr: if_(eq(next_localpar(), phase()), int(1), int(0)),
                },
            ],
        }],
        invariants: vec![Invariant {
            // The pattern is the absolute-parity function: lit ⟺ even column.
            name: "UniformPeriod",
            expr: eq(add(var("lit"), var("parity")), int(1)),
        }],
    }
}

/// W6 STYLED-RUN FACE — the abstract twin of the pure styled-face policy
/// `aterm_render::resolve_styled_face` on the LIGATURE-RUN seam (the real code
/// seam is `Renderer::run_face_pick` / `row_glyph_plan`'s shaping closure /
/// `rasterize`'s MonoGid arm; Tier-1 is aterm-render's
/// `tests/styled_faces.rs`, which enumerates the policy's complete 2^6 input
/// space and binds the run routing to real rendered ink).
///
/// This is the model of the dilated-bold-ligature fix: `ligature_key` /
/// `rasterize` used to hard-code the PRIMARY face for every run gid, so a bold
/// `=>` rendered as the regular ligature glyph dilated by synthetic embolden —
/// even when a REAL bold face (injected `set_bold_font` or a discovered
/// `-Bold` sibling) was available.
///
/// Scalar projection `<<bold, have_real, synth_bold>>`: `bold` = the run's SGR
/// requests bold; `have_real` = a real bold face is available (injected or
/// slot 0); `synth_bold` = the run rasterizes as PRIMARY + synthetic dilation
/// (the output fact, recomputed from the inputs in the same step — face
/// resolution is stateless per call). Four `Case*` actions spread the whole
/// input square over the reachable space.
///
/// `Buggy` gates the shipped defect: with `Buggy = 0` (committed) synthesis is
/// used ONLY when no real face exists (`synth_bold = bold AND NOT have_real`);
/// with `Buggy = 1` availability is ignored (`synth_bold = bold` — the old
/// hard-coded Primary route), so a bold run with a real bold face present
/// still dilates and `RealBoldNeverDilated` is violated. `ty` PROVES the
/// routing (Buggy=0) and CATCHES the regression (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn styled_run_face_model() -> Model {
    // synth_bold' for the literal inputs (b, h) each Case action installs:
    // committed policy = b AND NOT h; Buggy = b (availability ignored).
    let synth_for = |b: i64, h: i64| {
        if_(
            gt(cst("Buggy"), int(0)),
            int(b),
            int(if b == 1 && h == 0 { 1 } else { 0 }),
        )
    };
    let case = |name: &'static str, b: i64, h: i64| Action {
        name,
        guard: None,
        updates: vec![
            Update {
                var: "bold",
                expr: int(b),
            },
            Update {
                var: "have_real",
                expr: int(h),
            },
            Update {
                var: "synth_bold",
                expr: synth_for(b, h),
            },
        ],
    };
    Model {
        name: "StyledRunFace",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "bold",
                init: 0,
            },
            StateVar {
                name: "have_real",
                init: 0,
            },
            StateVar {
                name: "synth_bold",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            case("CaseRegular", 0, 0),
            case("CaseRegularHave", 0, 1),
            case("CaseBoldNoFace", 1, 0),
            case("CaseBoldHave", 1, 1),
        ],
        invariants: vec![Invariant {
            // A real bold face present ⇒ the run is never drawn as synthetic
            // dilation: synth_bold + have_real <= 1 (they are mutually exclusive).
            name: "RealBoldNeverDilated",
            expr: le(add(var("synth_bold"), var("have_real")), int(1)),
        }],
    }
}

/// W6 FALLBACK-CHAIN PRECEDENCE — the abstract twin of the pure ordering law
/// `aterm_render::fallback_chain_order` (the real code seam is the
/// `set_config_fallback_fonts` / `set_config_symbol_font` /
/// `set_config_emoji_font` candidate-list builders, whose loaders take the
/// FIRST existing path; Tier-1 is aterm-render's `tests/styled_faces.rs`
/// presence-lattice test, which binds the real function's first-element class
/// to this model's `winner` for every presence combination).
///
/// This is the model of the invisible-env-knob fix: the fallback / symbol /
/// emoji fonts used to be configurable ONLY via `$ATERM_FALLBACK_FONT`-family
/// env vars. The TOML keys (`fallback_fonts` / `symbol_font` / `emoji_font`)
/// must STRICTLY OUTRANK the env compat alias, which outranks built-in
/// discovery (discovery always exists — the built-in candidate list is
/// non-empty).
///
/// Scalar projection `<<cfg_present, env_present, winner>>`: presence of a
/// config entry / env alias, and `winner` = the class of the chain's FIRST
/// candidate (1 = config, 2 = env, 3 = discovery), recomputed from the inputs
/// in the same step. Four `Case*` actions spread the input square.
///
/// `Buggy` gates the inverted precedence (the pre-W6 world view where the env
/// var was consulted first): with `Buggy = 1` the env alias outranks an
/// explicit config entry, violating `ConfigOutranksEnv`. `ty` PROVES the law
/// (Buggy=0) and CATCHES the inversion (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fallback_precedence_model() -> Model {
    // winner' for the literal inputs (c, e): committed = config > env >
    // discovery; Buggy = env > config > discovery.
    let winner_for = |c: i64, e: i64| {
        let committed = if c == 1 {
            1
        } else if e == 1 {
            2
        } else {
            3
        };
        let buggy = if e == 1 {
            2
        } else if c == 1 {
            1
        } else {
            3
        };
        if_(gt(cst("Buggy"), int(0)), int(buggy), int(committed))
    };
    let case = |name: &'static str, c: i64, e: i64| Action {
        name,
        guard: None,
        updates: vec![
            Update {
                var: "cfg_present",
                expr: int(c),
            },
            Update {
                var: "env_present",
                expr: int(e),
            },
            Update {
                var: "winner",
                expr: winner_for(c, e),
            },
        ],
    };
    Model {
        name: "FallbackPrecedence",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "cfg_present",
                init: 0,
            },
            StateVar {
                name: "env_present",
                init: 0,
            },
            // Init matches CaseNeither's recomputation (discovery wins when
            // nothing outranks it), so the initial state satisfies the invariant.
            StateVar {
                name: "winner",
                init: 3,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            case("CaseNeither", 0, 0),
            case("CaseCfgOnly", 1, 0),
            case("CaseEnvOnly", 0, 1),
            case("CaseBoth", 1, 1),
        ],
        invariants: vec![Invariant {
            // An explicit config entry always heads the chain.
            name: "ConfigOutranksEnv",
            expr: or_(le(var("cfg_present"), int(0)), eq(var("winner"), int(1))),
        }],
    }
}

/// W2 TEXT-BLEND GATE — the abstract twin of the texel-level policy
/// `aterm_render::correction_applies` (the real code seam; Tier-1 lives in
/// `aterm-render/tests/text_blending.rs`, which enumerates the gate's COMPLETE
/// `bool × u8 × bool` domain and binds it to the shipping byte-level
/// `blend_text` — output may differ from plain linear only when the gate is
/// open).
///
/// The linear-corrected mode remaps glyph coverage with
/// `a_corr = clamp((blend_l - bg_l) / (fg_l - bg_l), 0, 1)` — a DIVISION by
/// the fg/bg luminance gap. The remap is legal ONLY when (1) the mode is
/// corrected (`Linear` must stay byte-identical to the physical blend), (2)
/// the coverage texel is INTERIOR (`cov ∉ {0, 255}` — the endpoint-exactness
/// bytes are early returns that run before any float math), and (3) the
/// luminance gap is NON-DEGENERATE (`|fg_l - bg_l| >= TEXT_BLEND_EPS` — the
/// 0/0 form as fg_l → bg_l). `interior`/`degenerate` abstract the byte/float
/// facts to booleans; the exhaustive Tier-1 test grounds them in the full
/// concrete domain. The float-valued remap laws themselves (range/monotone/
/// identity-at-eps) need `*`//`pow` and so live in the Tier-1 lattice sweeps,
/// not here (the ty expression language is add/sub only).
///
/// `Buggy` gates the defect class the eps guard exists to forbid: with
/// `Buggy = 1` the gate ignores `degenerate` and the remap runs on a
/// near-zero luminance gap (the div-by-near-zero artifact), violating
/// `CorrectionGated` — so `ty` PROVES the gate (Buggy=0) and CATCHES the
/// unguarded variant (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn text_blend_gate_model() -> Model {
    // Two-phase probe: `Pick` chooses an arbitrary texel context (mode ×
    // interior × degeneracy) nondeterministically; `Fire` computes the gate
    // from the settled (unprimed) context — sidestepping TLA's primed-RHS
    // rule the same way presentation_gate substitutes literals.
    let gate = if_(
        and_(
            and_(gt(var("corrected"), int(0)), gt(var("interior"), int(0))),
            // Buggy=1 drops the degeneracy conjunct (the unguarded divide).
            or_(le(var("degenerate"), int(0)), gt(cst("Buggy"), int(0))),
        ),
        int(1),
        int(0),
    );
    Model {
        name: "TextBlendGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "corrected",
                init: 0,
            },
            StateVar {
                name: "interior",
                init: 0,
            },
            StateVar {
                name: "degenerate",
                init: 0,
            },
            StateVar {
                name: "applies",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "corrected",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "interior",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "degenerate",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "applies",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Fire",
                guard: Some(gt(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "applies",
                        expr: gate,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // The remap runs only in corrected mode, on an interior texel,
            // with a non-degenerate luminance gap:
            //   applies <= corrected  ∧  applies <= interior  ∧
            //   applies + degenerate <= 1.
            name: "CorrectionGated",
            expr: and_(
                and_(
                    le(var("applies"), var("corrected")),
                    le(var("applies"), var("interior")),
                ),
                le(add(var("applies"), var("degenerate")), int(1)),
            ),
        }],
    }
}

/// W5b MINIMUM-CONTRAST FLOOR — the abstract twin of
/// `aterm_render::floor_fg_contrast`'s delivery bound (the real code seam;
/// Tier-1 lives in `aterm-render/tests/contrast_floor.rs`, which checks the
/// same bound against an independent WCAG oracle, exhaustively over grayscale
/// and a dense RGB lattice).
///
/// Abstract contrast lattice `0..=4` (0 ≈ 1:1 … 4 ≈ 21:1). `Pick` chooses an
/// arbitrary scene — the incoming fg's contrast `c_fg`, the two poles'
/// achievable contrasts `cap_black`/`cap_white` (black/white against the same
/// bg), and the requested ratio `r`; `Fire` computes the floor's RESULT
/// contrast `c_out` from the settled (unprimed) scene, mirroring the shipping
/// structure: early-exit when `c_fg >= r`, else a step search toward the
/// chosen POLE — which reaches `r` iff that pole's cap admits it and lands on
/// the pole itself otherwise. The WCAG ratio/luminance arithmetic (division,
/// the sRGB EOTF) is not expressible in ty (no `*`/`/`); the Tier-1 lattice
/// test grounds the abstraction in the full concrete color space.
///
/// `FloorDelivers`: after a `Fire`, `c_out >= min(r, max(cap_black, cap_white))`
/// — the floor delivers the requested ratio whenever the background admits it,
/// and the best achievable contrast otherwise.
///
/// `Buggy` gates the shipped defect: the fallback pole was chosen by the
/// LUMINANCE MIDPOINT (`L(bg) > 0.5`), not by comparing the poles' contrasts —
/// for mid-luminance backgrounds (`0.18 < L <= 0.5`) that picks the WEAKER
/// pole. Abstractly, `Buggy = 1` makes the search chase the weaker cap, so a
/// scene with `r` above the weak cap but under the strong one violates the
/// bound — `ty` PROVES it (Buggy=0) and CATCHES the midpoint rule (Buggy=1 →
/// counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn contrast_floor_model() -> Model {
    let strong = || {
        if_(
            gt(var("cap_black"), var("cap_white")),
            var("cap_black"),
            var("cap_white"),
        )
    };
    let weak = || {
        if_(
            gt(var("cap_black"), var("cap_white")),
            var("cap_white"),
            var("cap_black"),
        )
    };
    // The pole the fallback search walks toward: the contrast argmax (the
    // fix); under Buggy the midpoint rule's worst case — the weaker pole.
    let chased = || if_(gt(cst("Buggy"), int(0)), weak(), strong());
    // c_out: the early exit keeps c_fg; else the tenth-step search returns the
    // first candidate meeting r (reachable iff the chased pole admits r, since
    // the final step IS the pole), else the pole itself.
    let c_out = if_(
        le(var("r"), var("c_fg")),
        var("c_fg"),
        if_(le(var("r"), chased()), var("r"), chased()),
    );
    // min(r, strong) — what the floor promises.
    let bound = if_(le(var("r"), strong()), var("r"), strong());
    Model {
        name: "ContrastFloor",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "c_fg",
                init: 0,
            },
            StateVar {
                name: "cap_black",
                init: 0,
            },
            StateVar {
                name: "cap_white",
                init: 0,
            },
            StateVar { name: "r", init: 0 },
            StateVar {
                name: "c_out",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "c_fg",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "cap_black",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "cap_white",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "r",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "c_out",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Fire",
                guard: Some(gt(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "c_out",
                        expr: c_out,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // Picked-but-unfired states (phase = 1) carry a stale c_out and are
            // exempt; every settled state satisfies the delivery bound.
            name: "FloorDelivers",
            expr: or_(gt(var("phase"), int(0)), le(bound, var("c_out"))),
        }],
    }
}

/// M5 VIBRANCY LEGIBILITY GUARANTEE — the abstract twin of the config-resolution
/// policy `aterm_gui::app_config::Config::effective_minimum_contrast` (the real
/// code seam; Tier-1 is `aterm-gui`'s exhaustive `vibrancy_contrast_guarantee`
/// lattice test, which drives the SHIPPING resolver over the opacity × configured
/// -contrast grid and asserts the SAME `NeverIllegible` bound).
///
/// The move: engaging translucent glass (`background_opacity < 1.0`) must
/// AUTOMATICALLY raise the effective per-cell minimum-contrast floor to the WCAG
/// AA threshold (4.5:1) — glass that cannot make text illegible, unlike iTerm2's
/// blur and ghostty's `background-blur`, which let text sink into the desktop.
///
/// Abstract contrast lattice `0..=6` (a compact tier ladder; the real WCAG 4.5:1
/// threshold maps to `Floor = 3`). `Pick` chooses whether the window is
/// translucent (`translucent ∈ {0,1}`, i.e. `background_opacity < 1.0`) and the
/// user's configured floor `base`; `Fire` resolves the EFFECTIVE floor from the
/// settled scene, mirroring the shipping structure — `max(base, Floor)` when
/// translucent, else `base` untouched. The clamp/parse arithmetic (`clamp
/// 1.0..=21.0`, the `< 1.0` test) is ordinary comparison, so this IS a ty-
/// expressible boolean/ordering policy (no `*`/`/` — unlike the contrast-floor
/// WCAG math itself, which stays a lattice test).
///
/// `NeverIllegible`: `translucent ⇒ effective >= Floor` — every settled
/// translucent scene clears the legibility floor regardless of how low the user
/// set (or left) `minimum_contrast`.
///
/// `Buggy` gates the defect class the guarantee exists to exclude: with
/// `Buggy = 1` the auto-floor is DROPPED (the resolver returns the raw `base`
/// even while translucent), so a user on default `minimum_contrast` (base below
/// `Floor`) running glass gets sub-4.5:1 text — `ty` PROVES the guarantee
/// (Buggy=0) and CATCHES the dropped floor (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vibrancy_contrast_model() -> Model {
    // max(base, Floor): the auto-engaged legibility floor for translucent glass.
    let maxf = || if_(gt(var("base"), cst("Floor")), var("base"), cst("Floor"));
    // The effective floor from the SETTLED (unprimed) scene: translucent windows
    // floor up to WCAG AA; opaque windows keep the user's configured value.
    // Buggy=1 drops the auto-floor (returns raw base even while translucent).
    let effective = if_(
        eq(var("translucent"), int(1)),
        if_(eq(cst("Buggy"), int(0)), maxf(), var("base")),
        var("base"),
    );
    Model {
        name: "VibrancyContrast",
        consts: vec![("Buggy", 0), ("Floor", 3)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "translucent",
                init: 0,
            },
            StateVar {
                name: "base",
                init: 0,
            },
            StateVar {
                name: "effective",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "translucent",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "base",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "effective",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Fire",
                guard: Some(gt(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "effective",
                        expr: effective,
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // Picked-but-unfired states (phase = 1) carry a stale effective and
            // are exempt; every settled translucent scene clears the floor.
            name: "NeverIllegible",
            expr: or_(
                gt(var("phase"), int(0)),
                or_(
                    eq(var("translucent"), int(0)),
                    le(cst("Floor"), var("effective")),
                ),
            ),
        }],
    }
}

/// W1 COMPOSITOR-STRETCH FIX — the abstract twin of the window-fit + padding-
/// absorption policy `aterm_render::pad_split` (the real code seam; Tier-1 lives
/// in `aterm-render/tests/pad_absorption.rs`, which drives THIS model's own
/// interpreter against the shipping function point-for-point over the model's
/// whole lattice, plus a lattice-exhaustive direct check).
///
/// The law: the swapchain is sized to the RAW window pixels; the maximal grid
/// (`cols = floor((w - 2*pad)/cell)`) plus per-edge pads that absorb the
/// `0..cell-1` remainder must tile the window EXACTLY (`pad_lo + cols*cell +
/// pad_hi == w`), keep the configured pad floor, and split the remainder
/// near-evenly (`pad_lo <= pad_hi <= pad_lo + 1`) — so the compositor never
/// rescales the frame and the bands never lean visibly to one side.
///
/// The `ty` expression language has no `*`/`/`, so the model builds `cols*cell`
/// by REPEATED ADDITION (`FitColumn` accumulates `acc += cell` while it fits;
/// maximality is the `Settle` guard) and computes `rem/2` by a bounded case
/// split (`rem <= 3` on this lattice). The full-bit-width arithmetic twin lives
/// in `aterm-render`'s `pad_split_kani` trust-mc harnesses.
///
/// `Buggy` gates the defect class the near-even law exists to forbid: with
/// `Buggy = 1` the WHOLE remainder lands on the trailing edge (`pad_lo` keeps
/// none of it), so a `rem >= 2` window violates `NearEvenSplit` — `ty` PROVES
/// the split (Buggy=0) and CATCHES the lopsided variant (Buggy=1).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn pad_absorption_model() -> Model {
    // usable == w - 2*pad; rem == usable - acc. All unprimed (evaluated at the
    // pre-state of the one Settle step that consumes them).
    let usable = || sub(sub(var("w"), var("pad")), var("pad"));
    let rem = || sub(usable(), var("acc"));
    // rem/2 without division: 0 for rem in {0,1}, 1 for {2,3} (cell <= 4 on
    // this lattice, so rem <= 3).
    let half = || if_(le(rem(), int(1)), int(0), int(1));
    // Buggy=1: the lopsided split — the leading pad keeps none of the remainder.
    let lo_share = || if_(gt(cst("Buggy"), int(0)), int(0), half());
    // Invariants hold vacuously until the split settles (phase 4).
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(4)), body);
    Model {
        name: "PadAbsorption",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "cell",
                init: 1,
            },
            StateVar {
                name: "pad",
                init: 0,
            },
            StateVar { name: "w", init: 0 },
            StateVar {
                name: "acc",
                init: 0,
            },
            StateVar {
                name: "pad_lo",
                init: 0,
            },
            StateVar {
                name: "pad_hi",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickCell",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "cell",
                        expr: in_range(int(1), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "PickPad",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // EVERY in-domain window on the lattice: w >= 2*pad + cell (at
                // least one padded cell fits), up to the exhaustive cap.
                name: "PickWin",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "w",
                        expr: in_range(add(add(var("pad"), var("pad")), var("cell")), int(14)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // cols*cell by repeated addition: fit one more column while it fits.
                name: "FitColumn",
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    le(add(var("acc"), var("cell")), usable()),
                )),
                updates: vec![Update {
                    var: "acc",
                    expr: add(var("acc"), var("cell")),
                }],
            },
            Action {
                // Maximality IS the guard: one more column would overflow the
                // usable extent. Absorb the remainder into the two pads.
                name: "Settle",
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    gt(add(var("acc"), var("cell")), usable()),
                )),
                updates: vec![
                    Update {
                        var: "pad_lo",
                        expr: add(var("pad"), lo_share()),
                    },
                    Update {
                        var: "pad_hi",
                        expr: add(var("pad"), sub(rem(), lo_share())),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // pad_lo + cols*cell + pad_hi == w: no pixel scaled or dropped.
                name: "ExactCover",
                expr: settled_implies(eq(
                    add(add(var("pad_lo"), var("acc")), var("pad_hi")),
                    var("w"),
                )),
            },
            Invariant {
                // Both pads keep the configured floor.
                name: "PadsFloor",
                expr: settled_implies(and_(
                    le(var("pad"), var("pad_lo")),
                    le(var("pad"), var("pad_hi")),
                )),
            },
            Invariant {
                // |pad_hi - pad_lo| <= 1, leaning to the trailing edge.
                name: "NearEvenSplit",
                expr: settled_implies(and_(
                    le(var("pad_lo"), var("pad_hi")),
                    le(var("pad_hi"), add(var("pad_lo"), int(1))),
                )),
            },
            Invariant {
                // The settled grid is maximal (restated as a state property).
                name: "Maximal",
                expr: settled_implies(gt(
                    add(var("acc"), var("cell")),
                    sub(sub(var("w"), var("pad")), var("pad")),
                )),
            },
        ],
    }
}

/// RAW-renderer asymmetric top-padding redistribution and cache identity.
///
/// This model deliberately describes the renderer's LEGACY RAW TRANSPORT
/// allocation, before a frontend crop: tightening only the raw top edge does
/// not change that allocation, so its raw bottom absorbs `pad - pad_top` and
/// `pad_top + raw_pad_bottom == 2*pad`. [`visible_pad_crop_model`] separately
/// proves the GUI-visible contract (`visible_bottom == pad`). The requested top
/// is clamped to the base pad. Because raw dimensions and terminal content can
/// remain byte-identical while the grid Y origin moves, CPU/GPU cache reuse must
/// key the effective `grid_top`, not dimensions alone.
///
/// The bounded lattice abstracts grid height away (it cancels from the exact
/// cover equation) but retains the independent head band in `grid_top`. `Buggy=1`
/// reproduces both defect classes: it accepts an unclamped top while leaving the
/// bottom at the old symmetric pad, and it reuses a valid cache solely because
/// dimensions/content match even when the cached grid origin differs.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn asymmetric_pad_layout_model() -> Model {
    let clamp_initial = || {
        if_(
            le(var("initial_request"), var("pad")),
            var("initial_request"),
            var("pad"),
        )
    };
    let clamp_changed = || {
        if_(
            le(var("changed_request"), var("pad")),
            var("changed_request"),
            var("pad"),
        )
    };
    let initial_top = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("initial_request"),
            clamp_initial(),
        )
    };
    let changed_top = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("changed_request"),
            clamp_changed(),
        )
    };
    let initial_bottom = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("pad"),
            sub(add(var("pad"), var("pad")), clamp_initial()),
        )
    };
    let changed_bottom = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("pad"),
            sub(add(var("pad"), var("pad")), clamp_changed()),
        )
    };
    let layouts_match = || eq(var("cached_grid_top"), var("grid_top"));
    let settled_layout = || gt(var("phase"), int(1));
    Model {
        name: "AsymmetricPadLayout",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "pad",
                init: 1,
            },
            StateVar {
                name: "head",
                init: 0,
            },
            StateVar {
                name: "initial_request",
                init: 0,
            },
            StateVar {
                name: "changed_request",
                init: 0,
            },
            StateVar {
                name: "pad_top",
                init: 0,
            },
            StateVar {
                name: "pad_bottom",
                init: 0,
            },
            StateVar {
                name: "grid_top",
                init: 0,
            },
            StateVar {
                name: "cached_grid_top",
                init: 0,
            },
            StateVar {
                name: "cache_valid",
                init: 0,
            },
            StateVar {
                name: "cache_hit",
                init: 0,
            },
            StateVar {
                name: "full_repaint",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickLayout",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "pad",
                        expr: in_range(int(1), int(3)),
                    },
                    Update {
                        var: "head",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "initial_request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "changed_request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "ApplyInitialTop",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad_top",
                        expr: initial_top(),
                    },
                    Update {
                        var: "pad_bottom",
                        expr: initial_bottom(),
                    },
                    Update {
                        var: "grid_top",
                        expr: add(var("head"), initial_top()),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                name: "PrimeLayoutCache",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "cached_grid_top",
                        expr: var("grid_top"),
                    },
                    Update {
                        var: "cache_valid",
                        expr: int(1),
                    },
                    Update {
                        var: "cache_hit",
                        expr: int(0),
                    },
                    Update {
                        var: "full_repaint",
                        expr: int(0),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                name: "ApplyChangedTop",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "pad_top",
                        expr: changed_top(),
                    },
                    Update {
                        var: "pad_bottom",
                        expr: changed_bottom(),
                    },
                    Update {
                        var: "grid_top",
                        expr: add(var("head"), changed_top()),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                name: "RenderWithLayoutCache",
                guard: Some(and_(
                    eq(var("phase"), int(4)),
                    eq(var("cache_valid"), int(1)),
                )),
                updates: vec![
                    Update {
                        var: "cache_hit",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(1),
                            if_(layouts_match(), int(1), int(0)),
                        ),
                    },
                    Update {
                        var: "full_repaint",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(0),
                            if_(layouts_match(), int(0), int(1)),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(5),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "ExactVerticalPadCover",
                expr: if_(
                    settled_layout(),
                    eq(
                        add(var("pad_top"), var("pad_bottom")),
                        add(var("pad"), var("pad")),
                    ),
                    le(
                        add(var("pad_top"), var("pad_bottom")),
                        add(var("pad"), var("pad")),
                    ),
                ),
            },
            Invariant {
                name: "TopPadIsBounded",
                expr: if_(
                    settled_layout(),
                    le(var("pad_top"), var("pad")),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "BottomAbsorbsFreedPixels",
                expr: if_(
                    settled_layout(),
                    and_(
                        le(var("pad"), var("pad_bottom")),
                        le(var("pad_bottom"), add(var("pad"), var("pad"))),
                    ),
                    eq(var("pad_bottom"), int(0)),
                ),
            },
            Invariant {
                name: "GridOriginTracksTopAndHead",
                expr: if_(
                    settled_layout(),
                    eq(var("grid_top"), add(var("head"), var("pad_top"))),
                    eq(var("grid_top"), int(0)),
                ),
            },
            Invariant {
                name: "LayoutChangeForcesFullRepaint",
                expr: if_(
                    and_(
                        eq(var("phase"), int(5)),
                        neq(var("cached_grid_top"), var("grid_top")),
                    ),
                    and_(
                        eq(var("cache_hit"), int(0)),
                        eq(var("full_repaint"), int(1)),
                    ),
                    le(var("cache_hit"), int(1)),
                ),
            },
            Invariant {
                name: "IdenticalLayoutMayReuseCache",
                expr: if_(
                    and_(eq(var("phase"), int(5)), layouts_match()),
                    and_(
                        eq(var("cache_hit"), int(1)),
                        eq(var("full_repaint"), int(0)),
                    ),
                    le(var("full_repaint"), int(1)),
                ),
            },
            Invariant {
                name: "CacheDecisionIsTotal",
                expr: if_(
                    eq(var("phase"), int(5)),
                    eq(add(var("cache_hit"), var("full_repaint")), int(1)),
                    eq(add(var("cache_hit"), var("full_repaint")), int(0)),
                ),
            },
        ],
    }
}

/// GUI-visible crop from the renderer's conserved raw padding allocation.
///
/// The raw renderer remains `grid + head + 2*pad`, with a tightened top and an
/// expanded raw bottom. The GUI removes exactly `pad - pad_top` source rows, so
/// its exposed frame is `grid + head + pad_top + pad`: the requested/clamped top
/// is preserved and the visible bottom is ALWAYS the base pad. `Buggy=1`
/// reproduces the shipped defect by exposing the uncropped raw frame/bottom.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn visible_pad_crop_model() -> Model {
    let resolved_top = || if_(le(var("request"), var("pad")), var("request"), var("pad"));
    let raw_height = || add(add(var("grid"), var("head")), add(var("pad"), var("pad")));
    let visible_height = || {
        add(
            add(var("grid"), var("head")),
            add(resolved_top(), var("pad")),
        )
    };
    let settled = || eq(var("phase"), int(2));
    Model {
        name: "VisiblePadCrop",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "pad",
                init: 0,
            },
            StateVar {
                name: "request",
                init: 0,
            },
            StateVar {
                name: "grid",
                init: 1,
            },
            StateVar {
                name: "head",
                init: 0,
            },
            StateVar {
                name: "pad_top",
                init: 0,
            },
            StateVar {
                name: "raw_pad_bottom",
                init: 0,
            },
            StateVar {
                name: "raw_height",
                init: 0,
            },
            StateVar {
                name: "visible_pad_top",
                init: 0,
            },
            StateVar {
                name: "visible_pad_bottom",
                init: 0,
            },
            StateVar {
                name: "visible_height",
                init: 0,
            },
            StateVar {
                name: "crop_total",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "ChooseGeometry",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "pad",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "request",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "grid",
                        expr: in_range(int(1), int(4)),
                    },
                    Update {
                        var: "head",
                        expr: in_range(int(0), int(2)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Crop",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "pad_top",
                        expr: resolved_top(),
                    },
                    Update {
                        var: "raw_pad_bottom",
                        expr: sub(add(var("pad"), var("pad")), resolved_top()),
                    },
                    Update {
                        var: "raw_height",
                        expr: raw_height(),
                    },
                    Update {
                        var: "visible_pad_top",
                        expr: resolved_top(),
                    },
                    Update {
                        var: "visible_pad_bottom",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            sub(add(var("pad"), var("pad")), resolved_top()),
                            var("pad"),
                        ),
                    },
                    Update {
                        var: "visible_height",
                        expr: if_(eq(cst("Buggy"), int(1)), raw_height(), visible_height()),
                    },
                    Update {
                        var: "crop_total",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(0),
                            sub(var("pad"), resolved_top()),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "TopIsClamped",
                expr: if_(
                    settled(),
                    and_(
                        le(var("pad_top"), var("pad")),
                        eq(var("visible_pad_top"), var("pad_top")),
                    ),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "RawTransportConservesTwoPads",
                expr: if_(
                    settled(),
                    and_(
                        eq(
                            add(var("pad_top"), var("raw_pad_bottom")),
                            add(var("pad"), var("pad")),
                        ),
                        eq(var("raw_height"), raw_height()),
                    ),
                    eq(var("raw_height"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleBottomIsBasePad",
                expr: if_(
                    settled(),
                    eq(var("visible_pad_bottom"), var("pad")),
                    eq(var("visible_pad_bottom"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleHeightUsesIndependentEdges",
                expr: if_(
                    settled(),
                    eq(var("visible_height"), visible_height()),
                    eq(var("visible_height"), int(0)),
                ),
            },
            Invariant {
                name: "CropDeletesOnlyRemovedTop",
                expr: if_(
                    settled(),
                    and_(
                        eq(var("crop_total"), sub(var("pad"), var("pad_top"))),
                        eq(
                            var("raw_height"),
                            add(var("visible_height"), var("crop_total")),
                        ),
                    ),
                    eq(var("crop_total"), int(0)),
                ),
            },
        ],
    }
}

/// W3 CT FRACTIONAL BEARING — the abstract twin of the CoreText sub-pixel
/// placement policy `aterm_render::ct_pen_and_bearing` (the real code seam is
/// `CtFont::rasterize`; Tier-1 lives in
/// `aterm-render/tests/ct_fractional_bearing.rs`, which sweeps the pure fn over
/// a dense dyadic lattice AND drives THIS model's own interpreter against it
/// point-for-point, plus the FFI binding test in aterm-render's lib tests).
///
/// The sin (audit sin 10): `CtFont::rasterize` pinned each ink box to an
/// integer bitmap origin (pen `= -b`) and reported `round(b)` bearings, so
/// every glyph sat up to 0.5px off its designed position per axis, error
/// varying glyph-to-glyph. The fix splits the padded ink-box origin `b` into
/// an integer bearing `floor(b)` plus a RETAINED in-bitmap phase
/// `b - floor(b) ∈ [0, 1)`.
///
/// `ty`'s Expr language has no `*`/`/`, so the model computes the floor
/// decomposition OPERATIONALLY, in eighth-px fixed point (SCALE = 8): after
/// `PickB` (any bearing `b ∈ [-24, 24]` eighths, i.e. ±3px) and `Latch`
/// (`rem = b`, `bearing = 0`), `StepDown`/`StepUp` move whole pixels (8 units)
/// between `rem` and `bearing` until `rem ∈ [0, 8)`, and `Report` publishes
/// the placement. Safety: `Decompose` — `bearing + rem == b` at every step
/// (integer bearing + retained phase IS the designed position, exactly) — and
/// `PhaseInUnit` — the reported phase sits in `[0, 1)`. `Buggy = 1` gates the
/// PRE-FIX placement into `Report` (bearing rounded to the nearest px, phase
/// pinned to 0): any `b` off the whole-px grid then violates `Decompose`, so
/// `ty` PROVES the law at `Buggy=0` and CATCHES the rounded-pin defect at
/// `Buggy=1` (counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ct_frac_bearing_model() -> Model {
    // The pre-fix report: round-to-nearest px (+8 iff rem >= 4) with the phase
    // discarded; the committed report keeps both untouched.
    let rounded = || {
        if_(
            gt(var("rem"), int(3)),
            add(var("bearing"), int(8)),
            var("bearing"),
        )
    };
    let report_bearing = if_(gt(cst("Buggy"), int(0)), rounded(), var("bearing"));
    let report_rem = if_(gt(cst("Buggy"), int(0)), int(0), var("rem"));
    // rem in [0, 8): the whole-px moves are exhausted, phase is sub-px.
    let rem_in_unit = || and_(le(int(0), var("rem")), le(var("rem"), int(7)));
    Model {
        name: "CtFracBearing",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar { name: "b", init: 0 },
            StateVar {
                name: "rem",
                init: 0,
            },
            StateVar {
                name: "bearing",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // EVERY designed bearing on the lattice, negatives included
                // (a left-side bearing can be negative).
                name: "PickB",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "b",
                        expr: in_range(int(-24), int(24)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Start the decomposition: the whole origin is still phase.
                name: "Latch",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: var("b"),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Move one whole px of positive origin into the bearing.
                name: "StepDown",
                guard: Some(and_(eq(var("phase"), int(2)), gt(var("rem"), int(7)))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: sub(var("rem"), int(8)),
                    },
                    Update {
                        var: "bearing",
                        expr: add(var("bearing"), int(8)),
                    },
                ],
            },
            Action {
                // Move one whole px of negative origin into the bearing
                // (floor, NOT truncation: -0.5px becomes bearing -1 + phase 0.5).
                name: "StepUp",
                guard: Some(and_(eq(var("phase"), int(2)), gt(int(0), var("rem")))),
                updates: vec![
                    Update {
                        var: "rem",
                        expr: add(var("rem"), int(8)),
                    },
                    Update {
                        var: "bearing",
                        expr: sub(var("bearing"), int(8)),
                    },
                ],
            },
            Action {
                // Publish the placement the raster/blit will use.
                name: "Report",
                guard: Some(and_(eq(var("phase"), int(2)), rem_in_unit())),
                updates: vec![
                    Update {
                        var: "bearing",
                        expr: report_bearing,
                    },
                    Update {
                        var: "rem",
                        expr: report_rem,
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // Exact reconstruction: integer bearing + retained phase is the
                // designed position at EVERY step after Latch (the pre-fix
                // round-and-pin loses up to half a px here).
                name: "Decompose",
                expr: or_(
                    le(var("phase"), int(1)),
                    eq(add(var("bearing"), var("rem")), var("b")),
                ),
            },
            Invariant {
                // The reported sub-px phase is in [0, 1) — scaled: rem in [0, 8).
                name: "PhaseInUnit",
                expr: or_(neq(var("phase"), int(3)), rem_in_unit()),
            },
        ],
    }
}

/// W4 CURSOR-CUT-OUT CLIP — the abstract twin of the block-cursor cut-out's
/// x-clip window (the real code seam is `aterm_render::clip_span`, which every
/// cut-out blit routes through, and `glyph_quad`'s x-clip on the GPU; Tier-1
/// lives in `aterm-render/tests/cursor_ink.rs`, whose exhaustive lattice tests
/// check the SAME partition/no-bleed law on the shipping fns and whose pixel
/// sweep enforces it end-to-end: the complement of the cursor rect is
/// byte-identical to the no-cursor frame).
///
/// The sin (audit sin 3): the cut-out re-blitted the ENTIRE glyph in cell-bg
/// with no horizontal clip, so a cursor on the '>' of a '=>' ligature painted
/// bg over the arrow's lead-cell ink, and on a wide CJK lead the full-glyph
/// re-blit erased the ideograph's right half. The fix clips every cut-out
/// paint to the cursor rect's x-span `[w0, w1)`: the visible cut-out slice is
/// the INTERSECTION `[max(g0, w0), min(g0+gw, w1))` of the glyph extent with
/// the window.
///
/// After the nondeterministic picks (`PickGlyph` → `PickWinLo` → `PickWinHi`,
/// every extent × window on the bounded lattice), `Slice` computes the
/// intersection with `min`/`max` as `if/else` (ty's Expr has no min/max
/// builtins). The left/right remainders `[g0, m0)` / `[m1, g0+gw)` tile the
/// extent around the middle BY CONSTRUCTION (the three lengths telescope to
/// `gw`), so the safety content is the two non-trivial laws: `SlicesOrdered`
/// — `g0 <= m0 <= m1 <= g0+gw`, the clamp keeps every slice inside the glyph
/// — and `CutoutInsideWindow` — a non-empty cut-out slice never exits the
/// window (NO-BLEED: nothing outside the cursor rect is ever repainted).
/// `Buggy = 1` gates the PRE-FIX slice into `Slice` (the whole glyph extent,
/// unclipped): any window that strictly clips then violates
/// `CutoutInsideWindow`, so `ty` PROVES the clip law at `Buggy=0` and CATCHES
/// the shipped bleed at `Buggy=1` (counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cutout_clip_model() -> Model {
    let min = |a: Expr, b: Expr| if_(le(a.clone(), b.clone()), a, b);
    let max = |a: Expr, b: Expr| if_(le(a.clone(), b.clone()), b, a);
    let end = || add(var("g0"), var("gw"));
    // Committed slice: clamp the window edge into the glyph extent. Buggy:
    // the unclipped pre-fix cut-out (the whole extent, window ignored).
    let clamped = |edge: &'static str| max(var("g0"), min(var(edge), end()));
    let buggy = || gt(cst("Buggy"), int(0));
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(4)), body);
    Model {
        name: "CursorCutoutClip",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "g0",
                init: 0,
            },
            StateVar {
                name: "gw",
                init: 0,
            },
            StateVar {
                name: "w0",
                init: 0,
            },
            StateVar {
                name: "w1",
                init: 0,
            },
            StateVar {
                name: "m0",
                init: 0,
            },
            StateVar {
                name: "m1",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Every glyph extent on the lattice: origin AND width range over
                // enough values that windows can hit the interior, an edge, or miss.
                name: "PickGlyph",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "g0",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "gw",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "PickWinLo",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "w0",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // w1 >= w0: a well-formed [w0, w1) window (the callers pass
                // x0 .. x0 + cur_w with cur_w >= 0).
                name: "PickWinHi",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "w1",
                        expr: in_range(var("w0"), int(6)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // The clip: middle slice = extent ∩ window (committed), or the
                // whole unclipped extent (Buggy — the pre-W4 cut-out).
                name: "Slice",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "m0",
                        expr: if_(buggy(), var("g0"), clamped("w0")),
                    },
                    Update {
                        var: "m1",
                        expr: if_(buggy(), end(), clamped("w1")),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // g0 <= m0 <= m1 <= g0+gw: the slice stays inside the glyph, so
                // the left/right remainders [g0,m0) / [m1,g0+gw) tile around it.
                name: "SlicesOrdered",
                expr: settled_implies(and_(
                    le(var("g0"), var("m0")),
                    and_(le(var("m0"), var("m1")), le(var("m1"), end())),
                )),
            },
            Invariant {
                // NO-BLEED: a non-empty cut-out slice never exits the window —
                // nothing outside the cursor rect is ever repainted.
                name: "CutoutInsideWindow",
                expr: settled_implies(or_(
                    le(var("m1"), var("m0")),
                    and_(le(var("w0"), var("m0")), le(var("m1"), var("w1"))),
                )),
            },
        ],
    }
}
