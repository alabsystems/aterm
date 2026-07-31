// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Font and glyph geometry gate models (fallback, variable-font, strike selection) — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// W7 DECO PHASE — the abstract twin of the underline pattern-phase law: the
/// dot/dash/wave phase at a pixel is a pure function of ABSOLUTE x, so
/// crossing a CELL SEAM never resets it. The real code seam is
/// `aterm_render::deco::{dotted_on, dashed_on, squarewave_up,
/// undercurl_tile_col}` + the rect emission in
/// `aterm_render::underline_rects_into`; Tier-1 is aterm-render's
/// `tests/deco_lines.rs::pattern_rects_are_partition_invariant`, which
/// enumerates every cell partition of a run over a size lattice and proves the
/// emitted pixel coverage identical to the whole-run emission (with the old
/// per-cell dash law as a failing negative control).
///
/// This is the model of the phase-restart defect: the historical emission
/// derived its pattern phase from the CELL origin (dash/dot stepping restarted
/// at every `x0`, the curly square wave restarted every cell), so patterns
/// visibly re-synced at every cell boundary.
///
/// Scalar projection `<<x, phase>>`: `x` = the ghost pattern phase a pure
/// function of absolute x would carry (a counter wrapping at `Period`);
/// `phase` = the phase counter the EMISSION actually uses. `Step` advances one
/// pixel inside a cell (both counters tick); `Seam` advances the pixel that
/// crosses a cell boundary — the committed law ticks `phase` like any other
/// pixel, the `Buggy=1` variant RESETS it to 0 (the old per-cell phasing).
///
/// Invariant `PhasePure`: `phase = x` — the emission phase is exactly the
/// absolute-x ghost, wherever the seams fall. `ty` proves it at `Buggy=0` and
/// produces the seam-reset counterexample at `Buggy=1`
/// (tests/derived_ring_ty.rs::derived_deco_phase_proves_and_catches_seam_reset).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn deco_phase_model() -> Model {
    // Both counters wrap at Period = 4 (any pattern period >= 2 exhibits the
    // defect; 4 keeps the state space tiny while distinguishing reset from wrap).
    let wrap = |v: &'static str| if_(gt(var(v), int(2)), int(0), add(var(v), int(1)));
    Model {
        name: "DecoPhase",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar { name: "x", init: 0 },
            StateVar {
                name: "phase",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // One pixel forward, interior of a cell.
                name: "Step",
                guard: None,
                updates: vec![
                    Update {
                        var: "x",
                        expr: wrap("x"),
                    },
                    Update {
                        var: "phase",
                        expr: wrap("phase"),
                    },
                ],
            },
            Action {
                // One pixel forward, CROSSING a cell seam: the committed law
                // treats it like any pixel; the buggy law restarts the phase
                // at the new cell's origin.
                name: "Seam",
                guard: None,
                updates: vec![
                    Update {
                        var: "x",
                        expr: wrap("x"),
                    },
                    Update {
                        var: "phase",
                        expr: if_(gt(cst("Buggy"), int(0)), int(0), wrap("phase")),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // The emission phase IS the absolute-x ghost: pattern value at a
            // pixel cannot depend on how the run was cut into cells.
            name: "PhasePure",
            expr: eq(var("phase"), var("x")),
        }],
    }
}

/// W8 FALLBACK METRIC NORMALIZATION (fallback harmony) — the abstract twin of
/// `aterm_render::fallback_cjk_scale` / `fallback_xheight_scale`'s clamp law
/// (the real code seam: `Renderer::fallback_face_scale`, which sizes every
/// fallback-face raster). Tier-1 lives in
/// `aterm-render/tests/fallback_harmony.rs`, which sweeps the real `f32`
/// functions over a dense lattice + degenerate inputs.
///
/// The ratio `target/actual` is abstracted to an integer percentage `r` (the
/// division itself is outside ty's Expr language — no `*`/`/` — exactly the
/// contrast-floor precedent); `Fire` computes the shipped clamp `out =
/// min(115, max(85, r))` from the settled scene. Bounds are the CJK ±15%
/// clamp ×100; the x-height clamp is the same law with different constants,
/// carried by the same proof.
///
/// Invariants: `ScaleInBounds` — the computed scale NEVER leaves the clamp
/// interval, so a fallback face is never distorted more than the bound; and
/// `ScaleExactInRange` — an in-interval ratio passes through UNCHANGED (the
/// normalization is exact whenever achievable). `Buggy = 1` ships the raw
/// ratio (the pre-W8 "rasterize at the primary's px, whatever that does"
/// behaviour, where the effective ratio was unbounded) — `ty` PROVES the
/// clamp (Buggy=0) and CATCHES the unclamped scale (Buggy=1 → r=0 violates
/// the lower bound).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fallback_scale_clamp_model() -> Model {
    let clamp = if_(
        gt(int(85), var("r")),
        int(85),
        if_(gt(var("r"), int(115)), int(115), var("r")),
    );
    Model {
        name: "FallbackScaleClamp",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "r",
                init: 100,
            },
            StateVar {
                name: "out",
                init: 100,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "r",
                        expr: in_range(int(0), int(300)),
                    },
                    Update {
                        var: "out",
                        expr: int(100),
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
                        var: "out",
                        expr: if_(gt(cst("Buggy"), int(0)), var("r"), clamp),
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "ScaleInBounds",
                expr: and_(le(int(85), var("out")), le(var("out"), int(115))),
            },
            Invariant {
                // Settled states only (phase = 0): an in-interval ratio is exact.
                name: "ScaleExactInRange",
                expr: or_(
                    gt(var("phase"), int(0)),
                    or_(
                        gt(int(85), var("r")),
                        or_(gt(var("r"), int(115)), eq(var("out"), var("r"))),
                    ),
                ),
            },
        ],
    }
}

/// W8 WIDE-GLYPH CENTRING — the abstract twin of
/// `aterm_render::wide_center_offset`'s balance law (the real code seam:
/// `Renderer::harmonize_fallback_raster`, which centres a wide fallback
/// glyph's advance box in its 2-cell box). Tier-1 lives in
/// `aterm-render/tests/fallback_harmony.rs`, which proves EXHAUSTIVELY that
/// the shipped fn satisfies the floor CHARACTERIZATION `2*off <= gap <=
/// 2*off + 1` over the whole bounded domain; this model proves the
/// characterization IMPLIES the balance law — floor division itself is
/// outside ty's Expr language, so the proof is split at that exact seam.
///
/// `Pick` chooses an arbitrary gap (2-cell box minus scaled advance) and a
/// candidate offset; `Fire` accepts the offset — gated on the floor
/// characterization when correct, UNGATED when `Buggy = 1` (the pre-W8
/// left-bias shipped `off = 0` regardless of gap) — and publishes the two
/// margins. `MarginsBalance`: a checked state's margins differ by at most 1px.
/// `ty` PROVES it (Buggy=0) and CATCHES the left-bias (Buggy=1: `off = 0`,
/// `gap = 2` → margins 0 vs 2).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn wide_center_model() -> Model {
    let characterized = and_(
        le(add(var("off"), var("off")), var("gap")),
        le(var("gap"), add(add(var("off"), var("off")), int(1))),
    );
    Model {
        name: "WideCentre",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "gap",
                init: 0,
            },
            StateVar {
                name: "off",
                init: 0,
            },
            StateVar {
                name: "left",
                init: 0,
            },
            StateVar {
                name: "right",
                init: 0,
            },
            StateVar {
                name: "checked",
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
                        var: "gap",
                        expr: in_range(int(0), int(16)),
                    },
                    Update {
                        var: "off",
                        expr: in_range(int(0), int(8)),
                    },
                    Update {
                        var: "left",
                        expr: int(0),
                    },
                    Update {
                        var: "right",
                        expr: int(0),
                    },
                    Update {
                        var: "checked",
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
                guard: Some(and_(
                    gt(var("phase"), int(0)),
                    // Correct: only a floor-characterized offset is the policy's
                    // output. Buggy: ANY offset ships (the pre-W8 `off = 0`
                    // left-bias is one witness).
                    or_(gt(cst("Buggy"), int(0)), characterized),
                )),
                updates: vec![
                    Update {
                        var: "left",
                        expr: var("off"),
                    },
                    Update {
                        var: "right",
                        expr: sub(var("gap"), var("off")),
                    },
                    Update {
                        var: "checked",
                        expr: int(1),
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "MarginsBalance",
            expr: or_(
                le(var("checked"), int(0)),
                and_(
                    le(sub(var("left"), var("right")), int(1)),
                    le(sub(var("right"), var("left")), int(1)),
                ),
            ),
        }],
    }
}

/// W8 FALLBACK ROW-BAND CLIP — the abstract twin of
/// `aterm_render::clamp_to_row_band` (the real code seam:
/// `Renderer::harmonize_fallback_raster`, which trims a fallback glyph's
/// coverage rows so no fallback blit can paint outside its cell row band —
/// on the CPU blit and the GPU atlas quad alike, since they share the
/// trimmed bytes + placement). Tier-1 lives in
/// `aterm-render/tests/fallback_harmony.rs` (exhaustive lattice over
/// top/height/band including all-negative and overflow-past-band shapes).
///
/// `Pick` chooses an arbitrary ink placement — cell-relative `top` (may be
/// negative: ascender overshoot), bitmap `h`, band height `band`; `Fire`
/// computes the shipped trim `skip = min(max(0, -top), h)` and
/// `keep = min(h - skip, band - (top + skip))` (0 when past the band), or
/// the pre-W8 identity (`skip = 0`, `keep = h`) when `Buggy = 1`.
/// `KeptRowsInBand`: a checked state only ever TRIMS (`skip + keep <= h`)
/// and every kept row lies inside `[0, band)`. `ty` PROVES it (Buggy=0) and
/// CATCHES the unclipped blit (Buggy=1: `top = -1`, `h = 1` paints above the
/// band).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fallback_band_clip_model() -> Model {
    // `skip = min(max(0, -top), h)` — inlined everywhere it is needed (updates
    // evaluate on the unprimed state, so `keep` cannot read `skip'`).
    let skip = || {
        if_(
            gt(int(0), var("top")),
            if_(
                gt(sub(int(0), var("top")), var("h")),
                var("h"),
                sub(int(0), var("top")),
            ),
            int(0),
        )
    };
    // `keep = if top+skip >= band { 0 } else { min(h - skip, band - (top+skip)) }`
    let new_top = || add(var("top"), skip());
    let keep = if_(
        le(var("band"), new_top()),
        int(0),
        if_(
            gt(sub(var("h"), skip()), sub(var("band"), new_top())),
            sub(var("band"), new_top()),
            sub(var("h"), skip()),
        ),
    );
    Model {
        name: "FallbackBandClip",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "top",
                init: 0,
            },
            StateVar { name: "h", init: 0 },
            StateVar {
                name: "band",
                init: 0,
            },
            StateVar {
                name: "skip",
                init: 0,
            },
            StateVar {
                name: "keep",
                init: 0,
            },
            StateVar {
                name: "checked",
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
                        var: "top",
                        expr: in_range(int(-8), int(8)),
                    },
                    Update {
                        var: "h",
                        expr: in_range(int(0), int(8)),
                    },
                    Update {
                        var: "band",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "skip",
                        expr: int(0),
                    },
                    Update {
                        var: "keep",
                        expr: int(0),
                    },
                    Update {
                        var: "checked",
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
                        var: "skip",
                        expr: if_(gt(cst("Buggy"), int(0)), int(0), skip()),
                    },
                    Update {
                        var: "keep",
                        expr: if_(gt(cst("Buggy"), int(0)), var("h"), keep.clone()),
                    },
                    Update {
                        var: "checked",
                        expr: int(1),
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "KeptRowsInBand",
            expr: or_(
                le(var("checked"), int(0)),
                and_(
                    // Only ever trims: the kept window is a sub-range of the rows.
                    le(add(var("skip"), var("keep")), var("h")),
                    // Nothing kept, or every kept row lies inside [0, band).
                    or_(
                        le(var("keep"), int(0)),
                        and_(
                            le(int(0), add(var("top"), var("skip"))),
                            le(add(add(var("top"), var("skip")), var("keep")), var("band")),
                        ),
                    ),
                ),
            ),
        }],
    }
}

/// W9 VARIABLE-FONT AXIS CLAMP — the abstract twin of
/// `aterm_render::variation::clamp_axis`, the resolution law every W9
/// coordinate flows through (named-instance coords, the `wght=400` default
/// pull, `font_variation`/`font_weight` requests, the bold instance, the
/// dark nudge). The real code seam: `Renderer::compute_variations` →
/// `CtFont::new` / `rb_variations` / `varied_metrics_px`. Tier-1 lives in
/// `aterm-render/tests/variation_instantiation.rs`, which sweeps the real
/// `f32` function over a bounds lattice INCLUDING NaN/±∞ (floats are outside
/// ty's Expr language — the fallback-scale-clamp precedent).
///
/// `Pick` chooses an arbitrary normalized axis (`min <= def <= max`, the
/// ttf-parser parse-time guarantee) and an arbitrary request from a WIDER
/// range; `Fire` publishes the shipped clamp `out = min(max, max(min, req))`
/// — or the RAW request when `Buggy = 1` (the pre-W9 behaviour: no
/// instantiation layer at all, so whatever value arrived was the instance).
///
/// Invariants: `CoordInBounds` — a resolved coordinate NEVER leaves its axis
/// bounds (so `CTFontDescriptorCreateCopyWithVariation`, rustybuzz and
/// ttf-parser always receive a valid design-space value — resolution is
/// TOTAL); `CoordExactInRange` — an in-bounds request passes through
/// unchanged. `ty` PROVES both (Buggy=0) and CATCHES the unclamped pass-through
/// (Buggy=1: e.g. req=-2 under min=0 escapes the axis).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vf_axis_clamp_model() -> Model {
    let clamp = if_(
        gt(var("min"), var("req")),
        var("min"),
        if_(gt(var("req"), var("max")), var("max"), var("req")),
    );
    Model {
        name: "VfAxisClamp",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "min",
                init: 0,
            },
            StateVar {
                name: "def",
                init: 0,
            },
            StateVar {
                name: "max",
                init: 0,
            },
            StateVar {
                name: "req",
                init: 0,
            },
            StateVar {
                name: "out",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // An arbitrary axis + request scene. Normalization
                // (min <= def <= max) is enforced by Fire's guard, mirroring
                // ttf-parser's parse-time guarantee.
                name: "Pick",
                guard: Some(le(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "min",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "def",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "max",
                        expr: in_range(int(0), int(4)),
                    },
                    Update {
                        var: "req",
                        expr: in_range(int(-2), int(6)),
                    },
                    Update {
                        var: "out",
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
                guard: Some(and_(
                    gt(var("phase"), int(0)),
                    and_(le(var("min"), var("def")), le(var("def"), var("max"))),
                )),
                updates: vec![
                    Update {
                        var: "out",
                        expr: if_(gt(cst("Buggy"), int(0)), var("req"), clamp),
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
                // Resolved states only (phase = 2): the coordinate is on-axis.
                name: "CoordInBounds",
                expr: or_(
                    neq(var("phase"), int(2)),
                    and_(le(var("min"), var("out")), le(var("out"), var("max"))),
                ),
            },
            Invariant {
                // An in-bounds request resolves exactly.
                name: "CoordExactInRange",
                expr: or_(
                    neq(var("phase"), int(2)),
                    or_(
                        gt(var("min"), var("req")),
                        or_(gt(var("req"), var("max")), eq(var("out"), var("req"))),
                    ),
                ),
            },
        ],
    }
}

/// W9 DARK-NUDGE SAFETY GATE — the abstract twin of
/// `aterm_render::variation::dark_nudge_permitted`, the checked PRECONDITION
/// for the theme-polarity weight nudge (`font_weight_dark_nudge`). The real
/// code seam: `Renderer::compute_variations`' nudge branch, which measures
/// the `'M'` advance at the nudged coords and at the `fvar` default instance
/// and admits the nudge ONLY when they agree within 0.25px (monospace
/// variable fonts hold advances constant across `wght` — the property that
/// makes a weight nudge grid-safe). Tier-1 lives in
/// `aterm-render/tests/variation_instantiation.rs` (exhaustive advance
/// lattice incl. the NaN/∞ failed-measurement rows, plus the live-renderer
/// geometry-stability binding on SF Mono).
///
/// Advances are abstracted to integer QUARTER-px, so the 0.25px tolerance is
/// exactly `diff <= 1` (|·| via the two-sided `if`, the ty idiom for abs —
/// no floats in the Expr language). `Pick` chooses the two measured
/// advances; `Fire` applies the gate — or, when `Buggy = 1`, applies the
/// nudge UNCONDITIONALLY (the unguarded moonshot a review would rightly
/// reject: on a non-advance-stable VF it would silently change every cell's
/// designed advance).
///
/// Invariant `NudgeOnlyWhenInvariant`: a state where the nudge was applied
/// has `|adv_nudged − adv_default| <= 1` quarter-px. `ty` PROVES it
/// (Buggy=0) and CATCHES the unconditional nudge (Buggy=1: adv 0 vs 6
/// still nudges).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn vf_nudge_gate_model() -> Model {
    let diff = if_(
        gt(var("adv_n"), var("adv_d")),
        sub(var("adv_n"), var("adv_d")),
        sub(var("adv_d"), var("adv_n")),
    );
    Model {
        name: "VfNudgeGate",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "adv_n",
                init: 0,
            },
            StateVar {
                name: "adv_d",
                init: 0,
            },
            StateVar {
                name: "applied",
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
                        var: "adv_n",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "adv_d",
                        expr: in_range(int(0), int(6)),
                    },
                    Update {
                        var: "applied",
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
                        var: "applied",
                        expr: if_(
                            gt(cst("Buggy"), int(0)),
                            int(1),
                            if_(le(diff.clone(), int(1)), int(1), int(0)),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // An applied nudge implies advance invariance held.
            name: "NudgeOnlyWhenInvariant",
            expr: or_(le(var("applied"), int(0)), le(diff, int(1))),
        }],
    }
}

/// W10 EMOJI STRIKE SELECTION — the abstract twin of the pure strike policy
/// `aterm_render::select_strike_ppem` (the real code seam:
/// `pick_glyph_raster`, which walks the sbix strikes that actually carry a
/// glyph — the ZWJ ppem dead-zone defense — and applies this law to the
/// carrying set; Tier-1 is aterm-render's `tests/emoji_resample.rs`, which
/// enumerates the policy over an exhaustive strike lattice, plus the
/// real-face conformance sweep in aterm-render's in-module tests).
///
/// This is the model of the always-largest-strike fix: emoji used to
/// rasterize from the 160px master regardless of cell size (a `u16::MAX`
/// request), then minify 4–8x through a 2x2-tap filter that skips most
/// source texels — Apple's hand-tuned small strikes were never used.
///
/// Scalar projection `<<s1, s2, s3, t, chosen>>`: three nondeterministic
/// strike ppems (`0` = that strike does not carry the glyph), the target box
/// size, and the policy's pick (`0` = no strike carries the glyph). `Pick`
/// draws the whole space; `Fire` computes `chosen` with the shipped fold —
/// at `Buggy = 0` the min-adequate-else-max law, at `Buggy = 1` the pre-fix
/// always-largest pick. Invariants (settled states, `phase = 0`):
/// `ChosenFromAvailable` (the pick is a real strike), `ChosenAdequateMinimal`
/// (an adequate strike exists ⇒ the pick is adequate and `=<` every adequate
/// strike — the clause the old code violates: strikes {1,2}, target 1 → old
/// picks 2, law demands 1 → `ty` CATCHES Buggy=1), and
/// `ChosenMaxWhenNoneAdequate`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn strike_selection_model() -> Model {
    // The shipped fold: keep candidate `x` (a carrying strike, so != 0) over
    // incumbent `c` iff
    //   c = 0 (nothing kept yet), or
    //   x adequate (t =< x) and (c inadequate or x < c) — a tighter adequate, or
    //   both inadequate and x > c — a larger last-resort.
    let law_step = |x: &'static str, c: Expr| {
        if_(
            and_(
                neq(var(x), int(0)),
                or_(
                    eq(c.clone(), int(0)),
                    or_(
                        and_(
                            le(var("t"), var(x)),
                            or_(gt(var("t"), c.clone()), gt(c.clone(), var(x))),
                        ),
                        and_(gt(var("t"), c.clone()), gt(var(x), c.clone())),
                    ),
                ),
            ),
            var(x),
            c,
        )
    };
    let law_fold = law_step("s3", law_step("s2", law_step("s1", int(0))));
    // Pre-fix (`Buggy = 1`): the largest carrying strike, regardless of target.
    let max_step = |x: &'static str, c: Expr| if_(gt(var(x), c.clone()), var(x), c);
    let buggy_fold = max_step("s3", max_step("s2", max_step("s1", int(0))));
    // Per-strike minimality clause: strike i inadequate, or the pick is
    // adequate and no larger than it.
    let minimal_clause = |x: &'static str| {
        or_(
            gt(var("t"), var(x)),
            and_(le(var("t"), var("chosen")), le(var("chosen"), var(x))),
        )
    };
    // Invariants apply to SETTLED states only (`phase = 0`, after `Fire`);
    // this clause discharges the mid-`Pick` state.
    let mid_pick = gt(var("phase"), int(0));
    let any_adequate = or_(
        le(var("t"), var("s1")),
        or_(le(var("t"), var("s2")), le(var("t"), var("s3"))),
    );
    Model {
        name: "StrikeSelection",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "s1",
                init: 0,
            },
            StateVar {
                name: "s2",
                init: 0,
            },
            StateVar {
                name: "s3",
                init: 0,
            },
            StateVar { name: "t", init: 1 },
            StateVar {
                name: "chosen",
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
                        var: "s1",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "s2",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "s3",
                        expr: in_range(int(0), int(3)),
                    },
                    Update {
                        var: "t",
                        expr: in_range(int(1), int(3)),
                    },
                    Update {
                        var: "chosen",
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
                        var: "chosen",
                        expr: if_(gt(cst("Buggy"), int(0)), buggy_fold, law_fold),
                    },
                    Update {
                        var: "phase",
                        expr: int(0),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                // The pick is one of the (possibly absent, = 0) strikes.
                name: "ChosenFromAvailable",
                expr: or_(
                    mid_pick.clone(),
                    or_(
                        eq(var("chosen"), var("s1")),
                        or_(eq(var("chosen"), var("s2")), eq(var("chosen"), var("s3"))),
                    ),
                ),
            },
            Invariant {
                // Every adequate strike bounds the pick from above, and the
                // pick itself is adequate whenever any adequate strike exists.
                name: "ChosenAdequateMinimal",
                expr: or_(
                    mid_pick.clone(),
                    and_(
                        minimal_clause("s1"),
                        and_(minimal_clause("s2"), minimal_clause("s3")),
                    ),
                ),
            },
            Invariant {
                // No adequate strike: the pick dominates every carrying strike.
                name: "ChosenMaxWhenNoneAdequate",
                expr: or_(
                    mid_pick,
                    or_(
                        any_adequate,
                        and_(
                            le(var("s1"), var("chosen")),
                            and_(le(var("s2"), var("chosen")), le(var("s3"), var("chosen"))),
                        ),
                    ),
                ),
            },
        ],
    }
}
