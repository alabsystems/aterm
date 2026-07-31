// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Glyph-key/metrics, overlay-effect, rain, and kitty-persistence models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// GLYPH-KEY INJECTIVITY (W12) — px is part of every [`aterm_render::GlyphKey`] by
/// construction, so a cache/atlas lookup for a glyph at one pixel size can NEVER
/// collide with the SAME glyph rasterized at a different size. This underwrites the
/// mixed-DPI refactor: one shared glyph cache safely hosts every window's size at
/// once precisely because the key's identity separates the sizes.
///
/// The abstraction: two lookups for a glyph that agrees on every OTHER field
/// (source / class / code point / style) but differs in pixel size (`PxA != PxB`).
/// The cache ADDRESS of a key is its identity; the px is one component of it
/// (`Base + px` — ty has no multiplication, so the folded 26.6 quantization of the
/// real key is modelled additively as "px contributes to the address"). The gate
/// PROVES `NoCollision` at `Buggy=0` (the two addresses stay distinct — different
/// size ⇒ different slot) over the bounded space, and CATCHES the defect at
/// `Buggy=1` (a key that DROPS px — the pre-enabler world where one cache could not
/// host two sizes — so the two lookups alias the same slot) → counterexample. The
/// Tier-1 binding to the real derived `Eq`/`Hash` is aterm-render's
/// `tests/glyph_key_injectivity.rs` (the SAME injectivity, enumerated over real
/// keys at two sizes).
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn key_injectivity_model() -> Model {
    // A key's cache address folds a shared base (source/class/ch/style) with its px
    // component. The correct key keeps px; the Buggy key drops it (the two sizes
    // then alias). `Buggy=1` substitutes 0 for the px component of BOTH keys.
    let addr_for =
        |px: &'static str| add(cst("Base"), if_(eq(cst("Buggy"), int(1)), int(0), cst(px)));
    Model {
        name: "KeyInjectivity",
        consts: vec![("Base", 10), ("PxA", 1), ("PxB", 2), ("Buggy", 0)],
        vars: vec![
            // Init the two addresses DISTINCT so `Init` itself satisfies the
            // invariant; `Compute` then re-derives them from the key policy.
            StateVar {
                name: "addr_a",
                init: 0,
            },
            StateVar {
                name: "addr_b",
                init: 1,
            },
            StateVar {
                name: "done",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            // Resolve the cache address of each key from its (shared base, own px).
            name: "Compute",
            guard: Some(eq(var("done"), int(0))),
            updates: vec![
                Update {
                    var: "addr_a",
                    expr: addr_for("PxA"),
                },
                Update {
                    var: "addr_b",
                    expr: addr_for("PxB"),
                },
                Update {
                    var: "done",
                    expr: int(1),
                },
            ],
        }],
        invariants: vec![Invariant {
            // Two keys differing only in px map to DIFFERENT cache slots.
            name: "NoCollision",
            expr: neq(var("addr_a"), var("addr_b")),
        }],
    }
}

/// PER-WINDOW METRIC CONSISTENCY (W12) — every draw of window `w` must use metrics
/// derived from `w`'s OWN scale factor, never a shared "most-recently-scaled wins"
/// global. This is the safety property behind retiring the single shared-backend
/// font size/pad: scaling one window (moving it to a different-DPI display) must
/// not change how ANOTHER window renders.
///
/// The abstraction: two windows, each with its own `scale` token (`{1,2}`), plus a
/// shared backend metric `g` (the process-global the pre-fix code rescales on every
/// window's `ScaleFactorChanged` / attach). A `DrawWin*` renders a window; the
/// correct code sources that window's OWN `scale`, the `Buggy=1` code sources the
/// shared `g` (the SHARED-BACKEND LIMITATION). ty PROVES `PerWindowConsistent` at
/// `Buggy=0` (a drawn window's rendered metric always equals its own scale, for ALL
/// interleavings of scale-changes and draws), and CATCHES the defect at `Buggy=1`:
/// scale window 2 to hi-DPI (`g := 2`), then redraw window 1 → it renders at `g=2`
/// though its own scale is 1 → counterexample. The actual `round(FONT_PX·scale)` /
/// `round(PAD·scale)` arithmetic is NOT here (ty has no multiplication) — those
/// per-window laws are the L0 lattice tests (`font_px_for_scale` / `pad_for_scale`,
/// and the W1 per-window padding lattice).
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn per_window_metrics_model() -> Model {
    // A window's rendered metric: its own scale (correct) or the shared global
    // (Buggy — the most-recently-scaled backend wins).
    let render_from = |scale: &'static str| if_(eq(cst("Buggy"), int(1)), var("g"), var(scale));
    Model {
        name: "PerWindowMetrics",
        consts: vec![("Buggy", 0)],
        vars: vec![
            // The shared backend metric (the global the pre-fix path rescales).
            StateVar { name: "g", init: 1 },
            // Each window's own display scale token (1 = lo-DPI, 2 = hi-DPI).
            StateVar {
                name: "scale1",
                init: 1,
            },
            StateVar {
                name: "scale2",
                init: 1,
            },
            // The metric each window was last DRAWN with (0 = not yet drawn).
            StateVar {
                name: "rendered1",
                init: 0,
            },
            StateVar {
                name: "rendered2",
                init: 0,
            },
            StateVar {
                name: "drawn1",
                init: 0,
            },
            StateVar {
                name: "drawn2",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // Window 1 moves to a hi-DPI display: the shared backend rescales to it.
            // A scale change INVALIDATES the window's on-screen frame (winit re-grids
            // and repaints), so `drawn1 := 0` — the invariant tracks only the CURRENT
            // frame, and the window is not "drawn at the new scale" until redrawn.
            Action {
                name: "ScaleWin1Hi",
                guard: Some(eq(var("scale1"), int(1))),
                updates: vec![
                    Update {
                        var: "scale1",
                        expr: int(2),
                    },
                    Update {
                        var: "g",
                        expr: int(2),
                    },
                    Update {
                        var: "drawn1",
                        expr: int(0),
                    },
                ],
            },
            // Window 2 moves to a hi-DPI display: same shared rescale (the clobber),
            // invalidating window 2's own frame.
            Action {
                name: "ScaleWin2Hi",
                guard: Some(eq(var("scale2"), int(1))),
                updates: vec![
                    Update {
                        var: "scale2",
                        expr: int(2),
                    },
                    Update {
                        var: "g",
                        expr: int(2),
                    },
                    Update {
                        var: "drawn2",
                        expr: int(0),
                    },
                ],
            },
            // Render window 1 — from its own scale (correct) or the shared g (Buggy).
            Action {
                name: "DrawWin1",
                guard: None,
                updates: vec![
                    Update {
                        var: "rendered1",
                        expr: render_from("scale1"),
                    },
                    Update {
                        var: "drawn1",
                        expr: int(1),
                    },
                ],
            },
            // Render window 2 — from its own scale (correct) or the shared g (Buggy).
            Action {
                name: "DrawWin2",
                guard: None,
                updates: vec![
                    Update {
                        var: "rendered2",
                        expr: render_from("scale2"),
                    },
                    Update {
                        var: "drawn2",
                        expr: int(1),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // Every DRAWN window rendered at its OWN scale — no cross-window clobber.
            name: "PerWindowConsistent",
            expr: and_(
                or_(
                    eq(var("drawn1"), int(0)),
                    eq(var("rendered1"), var("scale1")),
                ),
                or_(
                    eq(var("drawn2"), int(0)),
                    eq(var("rendered2"), var("scale2")),
                ),
            ),
        }],
    }
}

/// Nyan blink-flare admission. A charged cursor may translate a terminal blink
/// edge into one bounded twinkle; a settled cursor must leave the ordinary
/// blink alone. `Buggy=1` reproduces the regression where every idle blink
/// creates a fresh effect episode, pinning recurring animation wakes forever.
/// The flare carries a two-tick countdown and a fuel invariant: whenever it is
/// armed, the remaining bounded state-space budget is sufficient to execute
/// `Age` and `Finish`. The buggy idle restart also corrupts that countdown,
/// making the termination obligation independently non-vacuous.
///
/// Tier-1 binding: `cursor_rainbow::idle_blink_transition_conforms_to_model`
/// drives the shipping admission decision through a reachable charged flare,
/// cooling, and an idle blink while that flare is still active. It projects
/// the real animator's `twinkle_seq`: an idle restart is therefore observable
/// even when the coarse `twinkle` boolean remains `1` on both sides.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nyan_idle_twinkle_model() -> Model {
    crate::ty_model! {
        NyanIdleTwinkle {
            const Buggy = 0;
            const MaxSteps = 6;
            const FlareTicks = 2;
            var charged = 0;
            var twinkle = 0;
            var remaining = 0;
            var flare_seq = 0;
            var idle_restarts = 0;
            var steps = 0;
            action Charge when (
                charged == 0 && steps + remaining <= MaxSteps - 1
            ) {
                charged = 1;
                steps = steps + 1;
            }
            action Cool when (
                charged == 1 && steps + remaining <= MaxSteps - 1
            ) {
                charged = 0;
                steps = steps + 1;
            }
            action BlinkCharged when (
                charged == 1 && steps <= MaxSteps - FlareTicks - 1
            ) {
                twinkle = 1;
                remaining = FlareTicks;
                flare_seq = flare_seq + 1;
                steps = steps + 1;
            }
            action BlinkIdle when (
                charged == 0 && steps + remaining <= MaxSteps - 1
            ) {
                twinkle = if Buggy == 1 { 1 } else { twinkle };
                remaining = if Buggy == 1 { MaxSteps } else { remaining };
                flare_seq = if Buggy == 1 { flare_seq + 1 } else { flare_seq };
                idle_restarts = if Buggy == 1 {
                    idle_restarts + 1
                } else {
                    idle_restarts
                };
                steps = steps + 1;
            }
            action Age when (
                twinkle == 1 && remaining > 1 && steps <= MaxSteps - 1
            ) {
                remaining = remaining - 1;
                steps = steps + 1;
            }
            action Finish when (
                twinkle == 1 && remaining == 1 && steps <= MaxSteps - 1
            ) {
                twinkle = 0;
                remaining = 0;
                steps = steps + 1;
            }
            invariant CanFinish:
                if twinkle == 1 {
                    steps + remaining <= MaxSteps
                } else {
                    remaining == 0
                };
            invariant IdleBlinkSilent: idle_restarts == 0;
            invariant TwinkleBounded: twinkle <= 1;
            invariant RemainingBounded: remaining <= FlareTicks;
            invariant FlareSeqBounded: flare_seq <= steps;
            invariant StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Sparse-frame sampling for the Nyan finger-lift exit lifecycle.
///
/// Logical time may pass the complete `grace + reach + retract` deadline
/// without any callback. On the first later observation, healthy code samples
/// the settled state and emits/arms nothing. `Buggy = 1` reproduces the former
/// callback-relative implementation: it births reach cells and starts the
/// retract clock at the observation itself, resurrecting visible motion.
///
/// Tier-1 binding:
/// `cursor_glow::tests::nyan_single_late_tick_does_not_resurrect_exit_swoosh`
/// drives the real animator from a live typing ribbon to one five-second-late
/// tick and projects its fingerprint and scheduler state onto this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nyan_exit_sampling_model() -> Model {
    crate::ty_model! {
        NyanExitSampling {
            const Buggy = 0;
            var logical_done = 0;
            var sampled = 0;
            var visible = 1;
            var active = 1;
            action ElapseDone when (logical_done == 0) {
                logical_done = 1;
            }
            action ObserveDone when (logical_done == 1 && sampled == 0) {
                sampled = 1;
                visible = if Buggy == 1 { 1 } else { 0 };
                active = if Buggy == 1 { 1 } else { 0 };
            }
            invariant SettledSampleHasNoLight:
                if logical_done == 1 && sampled == 1 { visible == 0 } else { visible <= 1 };
            invariant SettledSampleDisarms:
                if logical_done == 1 && sampled == 1 { active == 0 } else { active <= 1 };
            invariant SampleBounded: sampled <= 1;
        }
    }
}

/// Content-present/effect-timer ownership. A useful present consumes any
/// already-armed animation deadline and becomes the next cadence anchor, so an
/// immediately following stale timer cannot create a frame doublet. `Buggy=1`
/// leaves the deadline armed—the shipped regression.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_present_rebase_model() -> Model {
    crate::ty_model! {
        EffectPresentRebase {
            const Buggy = 0;
            const MaxSteps = 5;
            var pending = 0;
            var anchor = 0;
            var after_content = 0;
            var steps = 0;
            action Arm when (pending == 0 && after_content == 0 && steps <= MaxSteps - 1) {
                pending = 1;
                steps = steps + 1;
            }
            action ContentPresent when (pending == 1 && steps <= MaxSteps - 1) {
                pending = if Buggy == 1 { 1 } else { 0 };
                anchor = if Buggy == 1 { anchor } else { steps + 1 };
                after_content = 1;
                steps = steps + 1;
            }
            action NextTurn when (after_content == 1 && steps <= MaxSteps - 1) {
                after_content = 0;
                steps = steps + 1;
            }
            invariant NoImmediateTimerDoublet: after_content + pending <= 1;
            invariant ContentAnchored:
                if after_content == 1 { anchor == steps } else { after_content == 0 };
            invariant AnchorBounded: anchor <= MaxSteps;
            invariant StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Phase-locked brisk effect cadence with a two-tick interval. Timer fire owns
/// the cadence anchor; sub-interval wake/render cost must not slide the next
/// deadline. Late delivery and arbitrarily overloaded rendering are reachable:
/// once the next phase slot has already passed, re-arm starts one fresh full
/// interval with no catch-up burst. `Buggy=1` reproduces both failure modes:
/// completion-relative scheduling while the next phase slot is still live,
/// and an already-due catch-up deadline after an overloaded frame.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_phase_lock_model() -> Model {
    crate::ty_model! {
        EffectPhaseLock {
            const Buggy = 0;
            const Interval = 2;
            const MaxTime = 8;
            var now = 0;
            var anchor = 0;
            var schedule_base = 0;
            var deadline = 2;
            var pending = 1;
            var rendering = 0;
            var phase_locked = 1;
            var fresh = 1;
            action Tick when (pending == 1 && now <= MaxTime - 1) {
                now = now + 1;
                fresh = 0;
            }
            action Fire when (pending == 1 && deadline <= now) {
                pending = 0;
                anchor = deadline;
                rendering = 1;
                phase_locked = 0;
                fresh = 0;
            }
            action RenderCost when (rendering == 1 && now <= MaxTime - 1) {
                now = now + 1;
            }
            action Rearm when (rendering == 1) {
                schedule_base = if Buggy == 1 {
                    if now <= anchor + Interval - 1 { now } else { anchor }
                } else {
                    if now <= anchor + Interval - 1 { anchor } else { now }
                };
                deadline = if Buggy == 1 {
                    if now <= anchor + Interval - 1 {
                        now + Interval
                    } else {
                        anchor + Interval
                    }
                } else {
                    if now <= anchor + Interval - 1 {
                        anchor + Interval
                    } else {
                        now + Interval
                    }
                };
                pending = 1;
                rendering = 0;
                phase_locked = if now <= anchor + Interval - 1 { 1 } else { 0 };
                fresh = 1;
            }
            invariant DeadlineFromBase: deadline == schedule_base + Interval;
            invariant BriskPhaseLocked:
                if pending == 1 && phase_locked == 1 {
                    schedule_base == anchor
                } else {
                    schedule_base <= MaxTime
                };
            invariant FreshDeadlineFuture:
                if fresh == 1 { now <= deadline - 1 } else { fresh == 0 };
            invariant FreshIsPending: fresh <= pending;
            invariant StateOwned: pending + rendering == 1;
            invariant NowBounded: now <= MaxTime;
            invariant BaseBounded: schedule_base <= MaxTime;
            invariant DeadlineBounded: deadline <= MaxTime + Interval;
        }
    }
}

/// PHOSPHOR rain lifecycle (docs/matrix-rain-design.md §5/§10), authored via
/// [`ty_model!`] in the [`nova_phase_model`] shape: `{0 Idle, 1 Raining,
/// 2 Draining}`. `Activity` (a host activity event — enable, a content-seq
/// delta, a licensed refocus resumption) is the ONLY entry into Raining;
/// `StartDrain` (idle sleep / unfocus) opens the mandatory drain; `DrainTick`
/// consumes the drain fuel one engine tick at a time and lands Idle at EXACTLY
/// `DrainBound = 30` ticks — the engine's `DRAIN_TICKS` (the field decays `L`
/// per tick, so 30 ticks empties every column REGARDLESS of geometry: the
/// design's "no configuration animates forever").
///
/// Invariants: `NoUnlicensedRain: rains <= acts` — every Raining entry is
/// paid for by an activity event (the phantom-replay class: a drained pane
/// must never relight on cmd-tab alone); `CanReachIdle` is the fuel invariant
/// in the [`nova_phase_model`] `CanSettle` idiom — while Draining, the unspent
/// step budget `MaxSteps − steps` always covers the `DrainBound − drained`
/// ticks left to Idle, so no reachable drain is ever stranded by the
/// finiteness guard (`MaxSteps = 64` is TIGHT: two full episodes are
/// `2·(Activity + StartDrain + 30·DrainTick) = 64` steps — a 63-step budget
/// fails this very invariant at `Buggy = 0`); `StateBounded`/`DrainBounded`
/// are the always-true structural controls.
///
/// `Buggy = 1` enables `Rearm`: Idle re-enters Raining WITHOUT an activity
/// event (`rains` grows past `acts`) — `ty` PROVES all four invariants at
/// `Buggy = 0` over the whole bounded space and CATCHES the unlicensed
/// relight at `Buggy = 1` (counterexample on `NoUnlicensedRain`).
///
/// Tier-1 binding: aterm-effects'
/// `rain_lifecycle_conformance_real_engine_projects_onto_model` drives the
/// REAL `MatrixRain` through an enable → rain → idle-sleep-drain → resume
/// script, projecting `(state, fuel) = (lifecycle, drain_ticks)` onto this
/// model tick-for-tick, with the phantom-relight negative-control twin.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn rain_lifecycle_model() -> Model {
    crate::ty_model! {
        RainLifecycle {
            const Buggy = 0;
            const DrainBound = 30;  // DRAIN_TICKS: the geometry-independent drain
            const MaxActs = 2;      // bounded activity events (finiteness)
            const MaxSteps = 64;    // 2 full episodes: 2·(1 + 1 + 30) — every drain can finish
            var state = 0;   // 0 Idle, 1 Raining, 2 Draining
            var drained = 0; // drain ticks consumed THIS drain (the spent fuel)
            var steps = 0;   // guard-bounds every walk (finiteness)
            var acts = 0;    // activity events observed
            var rains = 0;   // entries into Raining
            action Activity when (acts <= MaxActs - 1 && steps <= MaxSteps - 1) {
                acts = acts + 1;
                rains = rains + (if state == 1 { 0 } else { 1 });
                state = 1;
                drained = 0;
                steps = steps + 1;
            }
            action StartDrain when (state == 1 && steps <= MaxSteps - 1) {
                state = 2;
                drained = 0;
                steps = steps + 1;
            }
            action DrainTick when (state == 2
                && drained <= DrainBound - 1 && steps <= MaxSteps - 1)
            {
                drained = drained + 1;
                state = if drained == DrainBound - 1 { 0 } else { 2 };
                steps = steps + 1;
            }
            // The defect class: a drained pane relights with NO activity event
            // (phantom replay). Only expressible at Buggy = 1.
            action Rearm when (Buggy == 1 && state == 0 && steps <= MaxSteps - 1) {
                state = 1;
                drained = 0;
                rains = rains + 1;
                steps = steps + 1;
            }
            invariant NoUnlicensedRain: rains <= acts;
            // Fuel: the remaining step budget always covers the remaining
            // drain, so Draining ALWAYS reaches Idle within DrainBound ticks
            // of the last activity event (nova CanSettle idiom).
            invariant CanReachIdle:
                (if state == 2 { steps + DrainBound } else { 0 }) <= MaxSteps + drained;
            invariant StateBounded: state <= 2;
            invariant DrainBounded: drained <= DrainBound;
        }
    }
}

/// PHOSPHOR rain band containment (docs/matrix-rain-design.md §7/§10) — the
/// damage law behind `aterm_render::compute_dirty_rows`' per-row merge-diff
/// for `rain_quads`: EVERY emitted quad whose bytes changed this tick lies in
/// a marked dirty row, INCLUDING the mutation-tick case where the glyph hash
/// window rolls and every lit trail cell changes at once (not just the
/// stepped head/tail edges). Hand-built in the [`deco_band_containment_model`]
/// shape (nondeterministic `in_range` picks need the `Expr` builders; the
/// `ty_model!` grammar has none): a phased pick walk chooses a head row, a
/// trail length, whether this is a mutation tick, and a probe row anywhere in
/// the lit band (head-inclusive down to the just-expired tail).
///
/// `changed(r)`: the head (newly lit), the expired tail (its quad vanished —
/// the prev∪cur half of the merge-diff), or — on a mutation tick — ANY lit
/// row (the glyph swap rewrites the whole band). `marked(r)`: head + tail
/// always; the whole band only when mutation marking is on (`Buggy = 0`).
/// Safety `Contained`: `changed(r) <= marked(r)` (indicator order, the
/// [`grid_translate_model`] idiom). `Buggy = 1` skips mutation marking, so a
/// strictly-interior trail row changes UNMARKED on a mutation tick — the
/// exact stale-glyph ghost the renderer's no-ghost byte-equality test pins —
/// and `ty` catches it. `StepEdgesMarked` is the always-true non-vacuity
/// control (the stepped edges are marked at BOTH `Buggy` values).
///
/// Tier-1 binding: aterm-render's dirty-row merge-diff + cached-vs-fresh
/// no-ghost byte-equality tests (`tests/rain_render.rs`) drive the shipping
/// `compute_dirty_rows` over real emission, mutation ticks included.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn rain_band_containment_model() -> Model {
    // Indicator that the probe row is a stepped EDGE (head or expired tail).
    let edge = || {
        or_(
            eq(var("r"), var("head")),
            eq(var("r"), sub(var("head"), var("l"))),
        )
    };
    // A quad at `r` changed this tick: a stepped edge, or (mutation tick) any
    // lit row — `r` is picked inside the band, so lit is structural.
    let changed = || if_(or_(edge(), eq(var("mt"), int(1))), int(1), int(0));
    // The dirty-row marker: edges always; the whole band only when mutation
    // marking is on (Buggy = 0 — the merge-diff sees every changed slice).
    let marked = || {
        if_(
            or_(
                edge(),
                and_(eq(var("mt"), int(1)), eq(cst("Buggy"), int(0))),
            ),
            int(1),
            int(0),
        )
    };
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(4)), body);
    Model {
        name: "RainBandContainment",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "head",
                init: 2,
            },
            StateVar { name: "l", init: 2 },
            StateVar {
                name: "mt",
                init: 0,
            },
            StateVar { name: "r", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "PickHead",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "head",
                        expr: in_range(int(2), int(8)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Trail length >= 2 so a strictly-interior row exists (the
                // mutation-only change the Buggy marker misses); <= head keeps
                // the expired tail on-screen.
                name: "PickTrail",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "l",
                        expr: in_range(int(2), var("head")),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Mutation tick or plain stepping tick — BOTH cases quantified.
                name: "PickTick",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "mt",
                        expr: in_range(int(0), int(1)),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // The probe: any row of the lit band, expired tail included.
                name: "PickRow",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "r",
                        expr: in_range(sub(var("head"), var("l")), var("head")),
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
                // THE THEOREM: a changed quad row is always a marked dirty row.
                // Buggy=1 leaves a mutation-tick interior row unmarked (ghost).
                name: "Contained",
                expr: settled_implies(le(changed(), marked())),
            },
            Invariant {
                // Always-true control (both Buggy values): the stepped edges
                // are marked — the walk settles and the marker is never empty,
                // so Contained is not checked vacuously.
                name: "StepEdgesMarked",
                expr: settled_implies(le(if_(edge(), int(1), int(0)), marked())),
            },
        ],
    }
}

/// PHOSPHOR rain ignition floor (docs/matrix-rain-design.md §4/§10) — the
/// structural flash-safety theorem: a column's head passes any given cell at
/// most once per second, because the cycle length carries the runtime
/// G-EXTENSION `G = max(G_natural, ceil(1000/(p·tick_ms)) − rows − L)`
/// (`aterm_effects::matrix_rain::field::col_params`), which forces
/// `C·p·tick_ms = (rows + L + G)·p·tick_ms >= 1000 ms` however small the
/// grid. Hand-built with `rows` as a nondeterministic model VARIABLE
/// (`in_range` needs the `Expr` builders): the walk picks a small grid
/// (`rows ∈ 3..=8` — exactly the regime where the natural gap alone is too
/// short), a step period `p ∈ 2..=5` (the engine's pre-knob range; 2 is the
/// fastest, worst-case column), a clamped trail `L ∈ 3..=rows`, and a natural
/// gap `G_natural ∈ 1..=rows`, then settles `C`. `tick_ms` is pinned at the
/// default 33 ms (30 fps); the runtime recomputes the same ceil for every
/// configured rate. `ty` has no `*`, so the product is encoded as literal
/// repeated addition (`C·66/99/132/165` branched on `p`) and the per-`p`
/// floor table `{16, 11, 8, 7}` is VERIFIED against `>= 1000` by the checker
/// itself, not assumed.
///
/// `Buggy = 1` drops the G-extension (`G = G_natural` — the pre-fix field): a
/// 3-row grid at `p = 2` cycles in `7·2·33 = 462 ms`, the head re-flashes the
/// same cell twice a second, and `ty` produces the counterexample.
/// `CycleExceedsViewport` is the always-true non-vacuity control (the cycle
/// always clears the viewport plus trail at BOTH `Buggy` values).
///
/// Tier-1 binding: aterm-effects' field tests drive the REAL
/// `col_params` over the same small-grid lattice and assert
/// `c * p * tick_ms >= 1000` per column, with the engine's flash-floor
/// behavior pinned by the matrix_rain unit suite.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn rain_ignition_model() -> Model {
    // `C · k` by literal repeated addition (the grammar has no `*`).
    let c_times = |k: i64| {
        let mut acc = var("c");
        for _ in 1..k {
            acc = add(acc, var("c"));
        }
        acc
    };
    // `C·p·33` — branch on the picked period (p ∈ 2..=5 ⇒ 66/99/132/165).
    let product = || {
        if_(
            eq(var("p"), int(2)),
            c_times(66),
            if_(
                eq(var("p"), int(3)),
                c_times(99),
                if_(eq(var("p"), int(4)), c_times(132), c_times(165)),
            ),
        )
    };
    // ceil(1000 / (p·33)) per period — the runtime flash floor. The table is
    // not trusted: HeadPassFloor re-verifies `floor·p·33 >= 1000` numerically.
    let floor_rows = || {
        if_(
            eq(var("p"), int(2)),
            int(16),
            if_(
                eq(var("p"), int(3)),
                int(11),
                if_(eq(var("p"), int(4)), int(8), int(7)),
            ),
        )
    };
    // The G-extension: G = max(G_natural, floor − rows − L); Buggy drops it.
    let g_final = || {
        let ext = || sub(floor_rows(), add(var("rows"), var("l")));
        if_(
            eq(cst("Buggy"), int(1)),
            var("g0"),
            if_(gt(ext(), var("g0")), ext(), var("g0")),
        )
    };
    let settled_implies = |body: Expr| or_(neq(var("phase"), int(5)), body);
    Model {
        name: "RainIgnition",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "phase",
                init: 0,
            },
            StateVar {
                name: "rows",
                init: 3,
            },
            StateVar { name: "p", init: 2 },
            StateVar { name: "l", init: 3 },
            StateVar {
                name: "g0",
                init: 1,
            },
            StateVar { name: "c", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Small grids: exactly where the natural gap alone breaks the
                // floor (a 50-row grid satisfies it without the extension).
                name: "PickRows",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "rows",
                        expr: in_range(int(3), int(8)),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "PickPeriod",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "p",
                        expr: in_range(int(2), int(5)),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // The engine clamp: L ∈ [3, rows].
                name: "PickTrail",
                guard: Some(eq(var("phase"), int(2))),
                updates: vec![
                    Update {
                        var: "l",
                        expr: in_range(int(3), var("rows")),
                    },
                    Update {
                        var: "phase",
                        expr: int(3),
                    },
                ],
            },
            Action {
                // The natural (hash-derived) gap before the extension: >= 1.
                name: "PickGap",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "g0",
                        expr: in_range(int(1), var("rows")),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                // C = rows + L + G, with (Buggy=0) or without (Buggy=1) the
                // G-extension.
                name: "Settle",
                guard: Some(eq(var("phase"), int(4))),
                updates: vec![
                    Update {
                        var: "c",
                        expr: add(add(var("rows"), var("l")), g_final()),
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
                // THE THEOREM: the per-cell head-pass period is >= 1 s.
                // Buggy=1: rows=3, L=3, G=1, p=2 ⇒ 7·66 = 462 < 1000.
                name: "HeadPassFloor",
                expr: settled_implies(le(int(1000), product())),
            },
            Invariant {
                // Always-true control (both Buggy values): the settled cycle
                // clears viewport + minimum trail + a live gap, so the floor
                // theorem is checked on non-degenerate cycles, not vacuously.
                name: "CycleExceedsViewport",
                expr: settled_implies(le(add(var("rows"), int(4)), var("c"))),
            },
        ],
    }
}

/// Generated cat-art collectible set. Each accepted semantic glyph key is
/// inserted at most once, so the unlocked/discovery count grows monotonically
/// to the finite generated roster and a repeated sighting changes only the
/// encounter counters. `RosterCap` is pinned to `GLYPH_IDS.len()` by the
/// Tier-1 shipping-code conformance test in `aterm-gui::kitty_log`.
///
/// `Buggy = 1` reproduces a set implemented as an append-only event list: a
/// repeated key incorrectly grows both `unlocked` and `discoveries`. That
/// immediately violates `DuplicateIdempotent` and, with enough repeats, also
/// exceeds `RosterBound`, giving the prove-and-catch obligation real teeth.
/// Monotonicity is structural: neither action contains a decrement, and the
/// only growth action in the healthy machine is `Unlock`.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_collectibles_model() -> Model {
    crate::ty_model! {
        KittyCollectibles {
            const Buggy = 0;
            const RosterCap = 36;
            // One encounter beyond a full roster exercises duplicate handling.
            const MaxSightings = 37;
            var unlocked = 0;    // cardinality of the semantic glyph-key set
            var discoveries = 0; // successful unique insertions
            var sightings = 0;   // accepted-key encounters, unique or repeated
            var duplicates = 0;  // encounters whose key was already present
            action Unlock when (
                unlocked <= RosterCap - 1 && sightings <= MaxSightings - 1
            ) {
                unlocked = unlocked + 1;
                discoveries = discoveries + 1;
                sightings = sightings + 1;
            }
            action Repeat when (
                unlocked > 0 && sightings <= MaxSightings - 1
            ) {
                unlocked = if Buggy == 1 { unlocked + 1 } else { unlocked };
                discoveries = if Buggy == 1 { discoveries + 1 } else { discoveries };
                sightings = sightings + 1;
                duplicates = duplicates + 1;
            }
            invariant RosterBound: unlocked <= RosterCap;
            invariant DiscoverySetAgreement: discoveries == unlocked;
            invariant DuplicateIdempotent: discoveries + duplicates == sightings;
            invariant SightingsBounded: sightings <= MaxSightings;
        }
    }
}

/// Bidirectional rollback-safe collectible persistence. Current `Discover`
/// writes the authoritative sidecar and its embedded mirror. A
/// collectible-aware rollback can `OldDiscover` a new semantic key or
/// `OldRepeat` an existing key into that mirror; `OldRepeatReset` covers an
/// existing key detectably recreated after a pre-collectibles mirror erasure
/// (its `first_seen` is strictly after the baseline window). `Reconcile`
/// imports only the positive event delta identified against the stored mirror
/// baseline. Thus a replicated row is never counted twice. A pre-collectibles
/// `OldRewrite` may erase the embedded replica only after it is reconciled;
/// `RestoreMirror` reconstructs it from the sidecar for another rollback.
///
/// `Buggy = 1` models the former base-only current writer. `Discover` appears
/// healthy to the live process, but leaves both sidecar key and event counts
/// behind. `Discover -> OldRewrite` then loses the unlock and violates both
/// `NoUnlockRollback` and `NoCountRollback`. At `Buggy = 0`, both invariants are
/// inductive over every bounded interleaving of current discovery, rollback
/// discovery/repeat, reconciliation, destructive rewrite, and mirror restore.
/// Same-timestamp/clock-regression ambiguity is deliberately outside the count
/// model and documented by the runtime ledger; the unlock-set invariant does
/// not depend on timestamps.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_sidecar_durability_model() -> Model {
    crate::ty_model! {
        KittySidecarDurability {
            const Buggy = 0;
            const Cap = 3;
            const EventCap = 6;
            var known = 0;          // logical unique-key high-water mark
            var base = 0;           // keys in rollback-readable mirror
            var sidecar = 0;        // keys in authoritative sidecar
            var pending = 0;        // old-build keys not reconciled yet
            var pending_events = 0; // old-build encounter deltas awaiting import
            var durable = 0;        // keys visible to the active/reloaded build
            var events = 0;         // logical encounter count
            var base_events = 0;    // encounter count in embedded replica
            var sidecar_events = 0; // authoritative encounter count
            var mirror_events = 0;  // last embedded baseline in sidecar
            var durable_events = 0; // encounter count visible after reload
            action Discover when (
                known <= Cap - 1 && events <= EventCap - 1 &&
                pending == 0 && pending_events == 0 &&
                base == sidecar && base_events == mirror_events
            ) {
                known = known + 1;
                base = base + 1;
                sidecar = if Buggy == 1 { sidecar } else { sidecar + 1 };
                durable = durable + 1;
                events = events + 1;
                base_events = base_events + 1;
                sidecar_events = if Buggy == 1 {
                    sidecar_events
                } else {
                    sidecar_events + 1
                };
                mirror_events = mirror_events + 1;
                durable_events = durable_events + 1;
            }
            action OldDiscover when (known <= Cap - 1 && events <= EventCap - 1) {
                known = known + 1;
                base = base + 1;
                pending = pending + 1;
                pending_events = pending_events + 1;
                durable = durable + 1;
                events = events + 1;
                base_events = base_events + 1;
                durable_events = durable_events + 1;
            }
            action OldRepeat when (base > 0 && events <= EventCap - 1) {
                events = events + 1;
                base_events = base_events + 1;
                pending_events = pending_events + 1;
                durable_events = durable_events + 1;
            }
            action OldRepeatReset when (
                known > 0 && base == 0 && events <= EventCap - 1
            ) {
                base = 1;
                pending_events = pending_events + 1;
                events = events + 1;
                base_events = 1;
                durable_events = durable_events + 1;
            }
            action Reconcile when (pending + pending_events > 0) {
                sidecar = sidecar + pending;
                pending = 0;
                sidecar_events = sidecar_events + pending_events;
                pending_events = 0;
                base = known;
                base_events = events;
                mirror_events = events;
                durable = known;
                durable_events = events;
            }
            action OldRewrite when (
                known > 0 && pending == 0 && pending_events == 0
            ) {
                base = 0;
                base_events = 0;
                durable = sidecar;
                durable_events = sidecar_events;
            }
            action RestoreMirror when (base == 0 && sidecar > 0) {
                base = sidecar;
                base_events = sidecar_events;
                mirror_events = sidecar_events;
                durable = sidecar;
                durable_events = sidecar_events;
            }
            invariant KnownBounded: known <= Cap;
            invariant EventsBounded: events <= EventCap;
            invariant BaseWithinKnown: base <= known;
            invariant SidecarWithinKnown: sidecar <= known;
            invariant PendingWithinKnown: pending <= known;
            invariant PendingEventsWithinEvents: pending_events <= events;
            invariant SidecarEventsWithinEvents: sidecar_events <= events;
            invariant NoUnlockRollback: durable == known;
            invariant NoCountRollback: durable_events == events;
        }
    }
}

/// Nonblocking Kitty Log flush-worker lifecycle. The ordinary capacity-one
/// lane, a host-retained tail, the shutdown-only exit lane, the worker pending
/// accumulator, and durable storage form one ownership equation. A tail
/// retained while the ordinary lane is full moves only through
/// `BeginExit → OfferTail → DrainNormal → AbsorbTail`; it can never disappear
/// merely because normal delivery was saturated.
///
/// Active lock contention is a stuttering observation: it leaves the worker
/// accumulator eligible for a later flush without consuming the terminal
/// budget. Only after exit begins and both delivery lanes drain does exit lock
/// contention advance the finite retry counter. An ordinary filesystem call
/// that never returns advances the event-loop-owned deadline and detaches at
/// `DeadlineCap`. Detachment
/// retains the conceptual pending batch: best-effort observability may lose a
/// crash window, but the UI thread never pretends it persisted. `Buggy = 1`
/// reproduces the retired one-lane exit path: `OfferTail` clears the host tail
/// while the full ordinary lane prevents ownership from reaching the exit lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_flush_worker_model() -> Model {
    crate::ty_model! {
        KittyFlushWorker {
            const Buggy = 0;
            // A slow worker may own one pending batch while the capacity-one
            // ordinary lane holds a second and the host retains a third tail.
            const BatchCap = 3;
            const RetryCap = 4;
            const DeadlineCap = 2;
            var accepted = 0;
            var normal_lane = 0;
            var host_tail = 0;
            var exit_lane = 0;
            var pending = 0;
            var persisted = 0;
            var exiting = 0;
            var retries = 0;
            var joined = 0;
            var stalled = 0;
            var deadline = 0;
            var detached = 0;
            action QueueNormal when (
                exiting == 0 && normal_lane == 0 && accepted <= BatchCap - 1
            ) {
                accepted = accepted + 1;
                normal_lane = 1;
            }
            action RetainTailOnFull when (
                exiting == 0 && normal_lane == 1 && host_tail == 0 &&
                accepted <= BatchCap - 1
            ) {
                accepted = accepted + 1;
                host_tail = 1;
            }
            action DrainNormal when (
                normal_lane == 1 && joined == 0 && detached == 0 && stalled == 0
            ) {
                normal_lane = 0;
                pending = pending + 1;
            }
            action Flush when (
                pending > 0 && joined == 0 && detached == 0 && stalled == 0 &&
                (exiting == 0 || retries <= RetryCap - 1)
            ) {
                persisted = persisted + pending;
                pending = 0;
            }
            action Contend when (
                pending > 0 && joined == 0 && detached == 0 && stalled == 0 &&
                exiting == 1 && normal_lane == 0 && host_tail == 0 && exit_lane == 0 &&
                retries <= RetryCap - 1
            ) {
                retries = retries + 1;
            }
            action BeginExit when (exiting == 0) {
                exiting = 1;
            }
            action OfferTail when (
                exiting == 1 && host_tail == 1 && exit_lane == 0 &&
                joined == 0 && detached == 0
            ) {
                host_tail = 0;
                exit_lane = if Buggy == 1 { 0 } else { 1 };
            }
            action AbsorbTail when (
                exiting == 1 && normal_lane == 0 && exit_lane == 1 &&
                joined == 0 && detached == 0 && stalled == 0
            ) {
                exit_lane = 0;
                pending = pending + 1;
            }
            action StallIo when (
                pending > 0 && exiting == 1 && joined == 0 && detached == 0 &&
                stalled == 0 && retries <= RetryCap - 1
            ) {
                stalled = 1;
            }
            action TickDeadline when (
                stalled == 1 && detached == 0 && deadline <= DeadlineCap - 1
            ) {
                deadline = deadline + 1;
            }
            action Detach when (
                exiting == 1 && stalled == 1 && detached == 0 &&
                deadline == DeadlineCap
            ) {
                detached = 1;
            }
            action Join when (
                exiting == 1 && joined == 0 && detached == 0 && stalled == 0 &&
                normal_lane == 0 && host_tail == 0 && exit_lane == 0 &&
                (pending == 0 || retries == RetryCap)
            ) {
                joined = 1;
            }
            invariant BatchBounded: accepted <= BatchCap;
            invariant NormalLaneBounded: normal_lane <= 1;
            invariant HostTailBounded: host_tail <= 1;
            invariant ExitLaneBounded: exit_lane <= 1;
            invariant RetryBounded: retries <= RetryCap;
            invariant DeadlineBounded: deadline <= DeadlineCap;
            invariant AcceptedConserved:
                accepted == normal_lane + host_tail + exit_lane + pending + persisted;
            invariant ExitLaneOnlyAfterExit: exit_lane <= exiting;
            invariant JoinedOnlyAfterExit: joined <= exiting;
            invariant StalledOnlyDuringExit: stalled <= exiting;
            invariant DetachedOnlyAfterExit: detached <= exiting;
            invariant OneExitDisposition: joined + detached <= 1;
            invariant DetachedOnlyAtDeadline:
                if detached == 1 {
                    deadline == DeadlineCap
                } else {
                    1 == 1
                };
            invariant JoinedHasFiniteOutcome:
                if joined == 1 {
                    normal_lane == 0 && host_tail == 0 && exit_lane == 0 &&
                    (pending == 0 || retries == RetryCap)
                } else {
                    1 == 1
                };
        }
    }
}

// -----------------------------------------------------------------------------
// Native tab-app platform models (`docs/NATIVE_TAB_APPS_DESIGN.md`, section 8).
//
// These are Tier-0 obligations for the platform contracts.  They intentionally
// live in the drift-free derived lane: the Rust declaration below is both the
// executable bounded machine and the source of the TLA+ checked by `ty`.
// Tier-1 status is model-specific and lives beside each genuine shipping seam;
// for example, control admission is bound in aterm-gui's
// `control_connection_conformance` test rather than duplicated here.
