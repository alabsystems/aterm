// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Font and glyph geometry models (fallback, variable font, strike selection), re-exported
//! from `crate::derive`.

use super::*;

/// Decoration phase: the dot/dash/wave phase of an underline at a pixel is a pure function of
/// absolute x, so crossing a cell seam never resets it. The code is
/// `aterm_render::deco::{dotted_on, dashed_on, squarewave_up, undercurl_tile_col}` and the rect
/// emission in `aterm_render::underline_rects_into`; Tier-1 is aterm-render's
/// `tests/deco_lines.rs::pattern_rects_are_partition_invariant`, which checks every cell
/// partition of a run over a size lattice emits the whole run's pixel coverage, with a per-cell
/// dash law as a failing negative control.
///
/// Projection `<<x, phase>>`: `x` is the ghost phase a pure function of absolute x carries (a
/// counter wrapping at 4), `phase` the one the emission uses. `Step` advances a pixel inside a
/// cell; `Seam` advances across a cell boundary, ticking `phase` like any pixel, or resetting it
/// at `Buggy=1` (per-cell phasing). `PhasePure`: `phase = x`
/// (`tests/derived_ring_ty.rs::derived_deco_phase_proves_and_catches_seam_reset`).
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn deco_phase_model() -> Model {
    // Both counters wrap at 4: any period >= 2 shows the defect, and 4 tells reset from wrap.
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
                // One pixel forward across a cell seam; `Buggy` restarts the phase there.
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
            // The emission phase is the absolute-x ghost: the pattern at a pixel cannot
            // depend on how the run was cut into cells.
            name: "PhasePure",
            kind: InvariantKind::Claim,
            expr: eq(var("phase"), var("x")),
        }],
    }
}

/// Fallback metric normalization: the abstract twin of `aterm_render::fallback_cjk_scale` /
/// `fallback_xheight_scale`'s clamp law (its seam, `Renderer::fallback_face_scale`, sizes every
/// fallback-face raster). Tier-1: `aterm-render/tests/fallback_harmony.rs` sweeps the real `f32`
/// functions over a dense lattice and degenerate inputs.
///
/// The ratio `target/actual` is an integer percentage `r` (the Expr language has no `*` or `/`);
/// `Fire` computes `out = min(115, max(85, r))`, the CJK ±15% clamp ×100. The x-height clamp is
/// the same law with other constants. `ScaleInBounds`: the scale never leaves the interval.
/// `ScaleExactInRange`: an in-interval ratio passes through unchanged. `Buggy = 1` ships the raw
/// ratio (`r = 0` breaks the lower bound).
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                kind: InvariantKind::Claim,
                expr: and_(le(int(85), var("out")), le(var("out"), int(115))),
            },
            Invariant {
                // Settled states only (phase = 0): an in-interval ratio is exact.
                name: "ScaleExactInRange",
                kind: InvariantKind::Uncaught,
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

/// Wide-glyph centring: the abstract twin of `aterm_render::wide_center_offset`'s balance law
/// (its seam, `Renderer::harmonize_fallback_raster`, centres a wide fallback glyph's advance in
/// its 2-cell box). Floor division is outside the Expr language, so the proof splits there:
/// Tier-1 (`aterm-render/tests/fallback_harmony.rs`) proves exhaustively that the shipped fn
/// meets `2*off <= gap <= 2*off + 1`, and this model proves that characterization implies the
/// margins differ by at most 1px (`MarginsBalance`).
///
/// `Pick` chooses a gap (2-cell box minus scaled advance) and a candidate offset; `Fire`
/// accepts it, gated on the characterization, or ungated at `Buggy = 1` (`off = 0`, `gap = 2`
/// gives margins 0 and 2).
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                    // Only a floor-characterized offset is the policy's output; `Buggy`
                    // ships any (`off = 0` is one witness).
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
            kind: InvariantKind::Claim,
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

/// Fallback band clip: the abstract twin of both band clamps, `aterm_render::clamp_to_row_band`
/// (band = the cell height) and `clamp_to_col_band` (band = the cell box plus a half-cell
/// bleed), one-line wrappers over `clamp_to_band`. The arithmetic is axis-free, so one model
/// covers both.
///
/// The seam is `Renderer::harmonize_fallback_raster`: stage (f) trims a fallback glyph's
/// coverage rows to its cell row band, and stage (h) trims its columns as the backstop under
/// the (g) condense, so a symbol glyph far wider than a cell (STIX Two Math's 1.499 em arrows,
/// U+27F5..U+27FC) cannot bury its neighbours. Both apply to the CPU blit and the GPU atlas
/// quad, which share the trimmed bytes and placement. Tier-1:
/// `aterm-render/tests/fallback_harmony.rs` (an exhaustive pos/len/band lattice including
/// all-negative and past-band shapes, a sweep pinning the two wrappers to one core, and the
/// law binding (g) to (h)).
///
/// `Pick` chooses an ink placement: cell-relative `top` (negative for ascender overshoot),
/// bitmap `h`, band height `band`. `Fire` computes `skip = min(max(0, -top), h)` and
/// `keep = min(h - skip, band - (top + skip))` (0 past the band), or the identity
/// (`skip = 0`, `keep = h`) at `Buggy = 1`. `KeptRowsInBand`: a checked state only trims
/// (`skip + keep <= h`) and every kept row lies in `[0, band)`.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn fallback_band_clip_model() -> Model {
    // `skip = min(max(0, -top), h)`, inlined where needed: updates read the unprimed state,
    // so `keep` cannot read `skip'`.
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
            kind: InvariantKind::Claim,
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

/// Variable-font axis clamp: the abstract twin of `aterm_render::variation::clamp_axis`, which
/// every coordinate flows through (named-instance coords, the `wght=400` default pull,
/// `font_variation`/`font_weight` requests, the bold instance, the dark nudge). Its seam is
/// `Renderer::compute_variations`, feeding `CtFont::new`, `compute_shaping_variations` and
/// `varied_metrics_px`. Tier-1: `aterm-render/tests/variation_instantiation.rs` sweeps the real
/// `f32` function over a bounds lattice including NaN/±∞.
///
/// `Pick` chooses a normalized axis (`min <= def <= max`) and a request from a wider range;
/// `Fire` publishes `out = min(max, max(min, req))`, or the raw request at `Buggy = 1`.
/// `CoordInBounds`: a resolved coordinate never leaves its axis, so CoreText and aterm-font's
/// reader and shaper always get a valid design-space value. `CoordExactInRange`: an in-bounds request
/// passes through unchanged.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                // An arbitrary axis and request; Fire's guard enforces min <= def <= max,
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
                kind: InvariantKind::Claim,
                expr: or_(
                    neq(var("phase"), int(2)),
                    and_(le(var("min"), var("out")), le(var("out"), var("max"))),
                ),
            },
            Invariant {
                // An in-bounds request resolves exactly.
                name: "CoordExactInRange",
                kind: InvariantKind::Uncaught,
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

/// Dark-nudge safety gate: the abstract twin of `aterm_render::variation::dark_nudge_permitted`,
/// the precondition for the theme-polarity weight nudge (`font_weight_dark_nudge`). Its seam is
/// `Renderer::compute_variations`' nudge branch, which measures the `'M'` advance at the nudged
/// coords and at the `fvar` default and admits the nudge only within
/// `DARK_NUDGE_ADVANCE_TOL_PX` (0.25px): monospace variable fonts hold advances constant across
/// `wght`, which is what makes a weight nudge grid-safe. Tier-1:
/// `aterm-render/tests/variation_instantiation.rs` (an advance lattice including NaN/∞ failed
/// measurements, plus a live-renderer geometry check on SF Mono).
///
/// Advances are integer quarter-px, so the tolerance is `diff <= 1` (abs via a two-sided `if`;
/// the Expr language has no floats). `Buggy = 1` nudges unconditionally, which on a font whose
/// advance moves with `wght` changes every cell's advance. `NudgeOnlyWhenInvariant`: an applied
/// nudge has `|adv_nudged − adv_default| <= 1` quarter-px.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Claim,
            expr: or_(le(var("applied"), int(0)), le(diff, int(1))),
        }],
    }
}

/// Emoji strike selection: the abstract twin of `aterm_render::select_strike_ppem`. Its seam,
/// `pick_glyph_raster`, walks the sbix strikes that carry the glyph (the ZWJ ppem dead-zone
/// defense) and applies this law to that set. Tier-1 is aterm-render's `tests/emoji_resample.rs`
/// (an exhaustive strike lattice) plus a real-face sweep in its in-module tests.
///
/// Projection `<<s1, s2, s3, t, chosen>>`: three nondeterministic strike ppems (`0` = does not
/// carry the glyph), the target box size, and the pick (`0` = none carries it). `Fire` computes
/// `chosen` with the shipped fold, min-adequate-else-max; `Buggy = 1` always takes the largest
/// strike (rasterizing from the master and minifying, so the small strikes go unused).
/// Invariants (settled states): `ChosenFromAvailable`, `ChosenAdequateMinimal` (if an adequate
/// strike exists the pick is adequate and `=<` every adequate strike; strikes {1,2} at target 1
/// refute `Buggy = 1`), and `ChosenMaxWhenNoneAdequate`.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
    // `Buggy = 1`: the largest carrying strike, regardless of target.
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
    // Invariants hold at settled states (`phase = 0`); this discharges the mid-`Pick` state.
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
                kind: InvariantKind::Uncaught,
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
                kind: InvariantKind::Claim,
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
                kind: InvariantKind::Uncaught,
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
