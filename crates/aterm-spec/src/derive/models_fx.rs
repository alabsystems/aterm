// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Glyph-key and metric, overlay-effect, rain and kitty-persistence models, re-exported from
//! `crate::derive`.

use super::*;

/// The cell-observation snapshot protocol for prism wake. Token and glyph identity are each
/// quotiented to two values: `OtherRow` flips only the token; `Echo` consumes a glyph change
/// without licensing it; `Rebase` forgets both. `GeometryBaseline` covers a reflow token and
/// `GeometryWithoutToken` a host resize arriving before another terminal token; both establish
/// history silently. Tier-1 (`aterm-effects/tests/output_streak_attribution.rs`) drives the real
/// Terminal VT and `OutputStreak`, including an echo followed by unrelated output, which a kept
/// pre-echo snapshot or a trusted global token would fail, and checks geometry and span bounds
/// against emitted quads.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_streak_attribution_model() -> Model {
    crate::ty_model! {
        OutputStreakAttribution {
            const Buggy = 0;
            var based = 0;
            var token = 0;
            var snapshot = 0;
            var fresh = 0;
            var echo = 0;
            var moved = 0;
            var licensed = 0;
            var geometry_changed = 0;

            action Baseline when (based == 0) {
                based = 1; token = 0; snapshot = 1;
                fresh = 0; echo = 0; moved = 0; licensed = 0;
                geometry_changed = 0;
            }
            action StableToken when (based == 1) {
                fresh = 0; echo = 0; moved = 0; licensed = 0;
                geometry_changed = 0;
            }
            action OtherRow when (based == 1) {
                token = 1 - token;
                fresh = 0; echo = 0; moved = 0;
                licensed = if Buggy == 1 { 1 } else { 0 };
                geometry_changed = 0;
            }
            action ChangedRun when (based == 1) {
                token = 1 - token; snapshot = 1 - snapshot;
                fresh = 1; echo = 0; moved = 0; licensed = 1;
                geometry_changed = 0;
            }
            action Echo when (based == 1) {
                token = 1 - token; snapshot = 1 - snapshot;
                fresh = 1; echo = 1; moved = 0; licensed = 0;
                geometry_changed = 0;
            }
            action Relocated when (based == 1) {
                token = 1 - token;
                fresh = 1; echo = 0; moved = 1; licensed = 0;
                geometry_changed = 0;
            }
            action GeometryBaseline when (based == 1) {
                token = 1 - token; snapshot = 1 - snapshot;
                fresh = if Buggy == 1 { 1 } else { 0 };
                echo = 0; moved = 0;
                licensed = if Buggy == 1 { 1 } else { 0 };
                geometry_changed = 1;
            }
            action GeometryWithoutToken when (based == 1) {
                fresh = 0; echo = 0; moved = 0; licensed = 0;
                geometry_changed = 1;
            }
            action Rebase when (based == 1) {
                based = 0; token = 0; snapshot = 0;
                fresh = 0; echo = 0; moved = 0; licensed = 0;
                geometry_changed = 0;
            }
            bound Bounded:
                based <= 1 && token <= 1 && snapshot <= 1 &&
                fresh <= 1 && echo <= 1 && moved <= 1 && licensed <= 1 &&
                geometry_changed <= 1;
            invariant LicensedRequiresFreshUndiscountedInk:
                if licensed == 1 {
                    based == 1 && fresh == 1 && echo == 0 && moved == 0
                } else { licensed == 0 };
            invariant GeometryBaselineIsSilent:
                if geometry_changed == 1 { licensed == 0 } else { licensed <= 1 };
        }
    }
}

/// Glyph-key injectivity: px is part of every [`aterm_render::GlyphKey`], so a lookup for a
/// glyph at one pixel size never collides with the same glyph at another, which is what lets
/// one shared glyph cache host every window's size at once.
///
/// Two lookups agree on every other field and differ in px (`PxA != PxB`). A key's address is
/// `Base + px` (the real key's 26.6 quantization modelled additively, as ty has no
/// multiplication). `NoCollision`: the two addresses stay distinct; `Buggy=1` drops px from the
/// key. Tier-1 against the real derived `Eq`/`Hash`: aterm-render's
/// `tests/glyph_key_injectivity.rs`.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn key_injectivity_model() -> Model {
    // A key's address folds a shared base (source/class/ch/style) with its px; `Buggy`
    // drops the px, so the two sizes alias.
    let addr_for =
        |px: &'static str| add(cst("Base"), if_(eq(cst("Buggy"), int(1)), int(0), cst(px)));
    Model {
        name: "KeyInjectivity",
        consts: vec![("Base", 10), ("PxA", 1), ("PxB", 2), ("Buggy", 0)],
        vars: vec![
            // Distinct at Init so `Init` satisfies the invariant; `Compute` re-derives them.
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
            // Two keys differing only in px map to different cache slots.
            name: "NoCollision",
            kind: InvariantKind::Claim,
            expr: neq(var("addr_a"), var("addr_b")),
        }],
    }
}

/// Per-window metric consistency: every draw of window `w` uses metrics from `w`'s own scale
/// factor, never a shared "most recently scaled wins" global, so moving one window to a
/// different-DPI display does not change how another renders.
///
/// Two windows, each with a `scale` token (`{1,2}`), and a shared backend metric `g` that every
/// window's `ScaleFactorChanged` rescales. `DrawWin*` renders a window from its own `scale`, or
/// from `g` at `Buggy=1` (scale window 2 to hi-DPI, redraw window 1: it renders at 2). The
/// `round(FONT_PX·scale)` / `round(PAD·scale)` arithmetic is not here (no multiplication); it
/// is the lattice tests' (`font_px_for_scale` / `pad_for_scale`).
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn per_window_metrics_model() -> Model {
    // `Buggy` renders from the shared global: the most recently scaled window wins.
    let render_from = |scale: &'static str| if_(eq(cst("Buggy"), int(1)), var("g"), var(scale));
    Model {
        name: "PerWindowMetrics",
        consts: vec![("Buggy", 0)],
        vars: vec![
            // The shared backend metric.
            StateVar { name: "g", init: 1 },
            // Each window's own display scale (1 = lo-DPI, 2 = hi-DPI).
            StateVar {
                name: "scale1",
                init: 1,
            },
            StateVar {
                name: "scale2",
                init: 1,
            },
            // The metric each window was last drawn with.
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
            // Window 1 moves to a hi-DPI display: the shared backend rescales to it. The
            // scale change invalidates the window's frame, so it is undrawn until redrawn.
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
            // Window 2 moves to a hi-DPI display: the same shared rescale.
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
            // Every drawn window rendered at its own scale.
            name: "PerWindowConsistent",
            kind: InvariantKind::Claim,
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

/// Rainbow kitty blink-flare admission. A charged cursor may turn a terminal blink edge into one
/// bounded twinkle; a settled cursor leaves the ordinary blink alone. `Buggy=1` starts a fresh
/// episode at every idle blink, pinning animation wakes for ever. The flare carries a two-tick
/// countdown with a fuel invariant (while armed, the remaining budget covers `Age` and
/// `Finish`), which the buggy restart also breaks.
///
/// Tier-1: `cursor_rainbow::idle_blink_transition_conforms_to_model` drives the shipping
/// admission through a charged flare, cooling, and an idle blink while the flare is active,
/// projecting the animator's `twinkle_seq` so a restart shows even when `twinkle` stays `1`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_idle_twinkle_model() -> Model {
    crate::ty_model! {
        RainbowIdleTwinkle {
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
            bound TwinkleBounded: twinkle <= 1;
            invariant RemainingBounded: remaining <= FlareTicks;
            uncaught FlareSeqBounded: flare_seq <= steps;
            bound StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Sparse-frame sampling for the rainbow kitty finger-lift exit lifecycle.
///
/// Logical time may pass the whole `grace + reach + retract` deadline without a callback; the
/// first later observation samples the settled state and emits and arms nothing. `Buggy = 1` is
/// callback-relative: it births reach cells and starts the retract clock at the observation,
/// resurrecting visible motion.
///
/// Tier-1: aterm-effects' `rainbow_kitty::tests::a_late_tick_samples_the_settled_exit_swoosh`
/// drives the real engine from a live ribbon (a single key, a slow word, a flow run) through
/// 5 s, 30 s and 10 min of silence to one late tick, projects what that frame painted and what
/// the engine then asks the host for, and refuses the engine with `Buggy = 1` injected
/// (`Ribbon::restart_unobserved_swooshes`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_exit_sampling_model() -> Model {
    crate::ty_model! {
        RainbowExitSampling {
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
            bound SampleBounded: sampled <= 1;
        }
    }
}

/// Content-present/effect-timer ownership. A useful present consumes any armed animation
/// deadline and becomes the next cadence anchor, so a stale timer right after it cannot create a
/// frame doublet. `Buggy=1` leaves the deadline armed.
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
            bound AnchorBounded: anchor <= MaxSteps;
            bound StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Focus/typed-wake demotion keeps the armed effect deadline until one settle present erases the
/// last animated raster. An unrelated event-loop turn may see the wake expired before that
/// deadline fires; dropping the slot there (`Buggy=1`) strands lit pixels on an unfocused window.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_presentability_settle_model() -> Model {
    crate::ty_model! {
        EffectPresentabilitySettle {
            const Buggy = 0;
            const MaxSteps = 3;
            var presentable = 1;
            var effect_active = 1;
            var pixels_lit = 1;
            var pending = 1;
            var settle_owed = 0;
            var settles = 0;
            var steps = 0;
            action ExpireWake when (
                presentable == 1 && effect_active == 1 && pixels_lit == 1
                    && pending == 1 && steps <= MaxSteps - 1
            ) {
                presentable = 0; settle_owed = 1; steps = steps + 1;
            }
            action UnrelatedPark when (
                presentable == 0 && settle_owed == 1 && pending == 1
                    && steps <= MaxSteps - 1
            ) {
                pending = if Buggy == 1 { 0 } else { 1 };
                steps = steps + 1;
            }
            action PaintSettle when (
                presentable == 0 && settle_owed == 1 && pending == 1
                    && steps <= MaxSteps - 1
            ) {
                pending = 0; effect_active = 0; pixels_lit = 0;
                settle_owed = 0; settles = 1; steps = steps + 1;
            }
            invariant LitPixelsHaveSettleRoute:
                if presentable == 0 && pixels_lit == 1 {
                    pending == 1 && settle_owed == 1
                } else { pending <= 1 };
            uncaught SettledPixelsAreClear:
                if settles == 1 {
                    pixels_lit == 0 && effect_active == 0 && settle_owed == 0
                } else { settles == 0 };
            bound StateBounded:
                presentable <= 1 && effect_active <= 1 && pixels_lit <= 1
                    && pending <= 1 && settle_owed <= 1 && settles <= 1;
            bound StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Phase-locked brisk effect cadence with a two-tick interval. Timer fire owns the cadence
/// anchor, so sub-interval wake and render cost do not slide the next deadline. Late delivery
/// and overloaded rendering are reachable: once the next phase slot has passed, re-arm starts
/// one fresh full interval with no catch-up burst. `Buggy=1` schedules from completion while
/// the next slot is live and arms an already-due catch-up deadline after an overloaded frame.
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
            uncaught DeadlineFromBase: deadline == schedule_base + Interval;
            invariant BriskPhaseLocked:
                if pending == 1 && phase_locked == 1 {
                    schedule_base == anchor
                } else {
                    schedule_base <= MaxTime
                };
            invariant FreshDeadlineFuture:
                if fresh == 1 { now <= deadline - 1 } else { fresh == 0 };
            uncaught FreshIsPending: fresh <= pending;
            uncaught StateOwned: pending + rendering == 1;
            bound NowBounded: now <= MaxTime;
            bound BaseBounded: schedule_base <= MaxTime;
            bound DeadlineBounded: deadline <= MaxTime + Interval;
        }
    }
}

/// Prism wake's cross-component episode delivery: one opening cue survives the synth's shared
/// keystroke gap, the host parks only after arming the analytic settle crossing, and that one
/// wake closes the episode. Output cues never claim the human voice gap, so the next authored
/// key stays admissible. The three `Buggy*` actions thin the sole Shimmer right after Enter,
/// park an open episode without its settle wake, and let an output cue take the next human
/// slot.
///
/// Tier-1 (`aterm-effects/tests/output_streak_conformance.rs`) drives `OutputStreak`'s analytic
/// deadline and `TrailSynth`'s admission surface; the native and web hosts' folds of that
/// deadline are host-level unit tests, and neither event loop is modelled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_streak_episode_delivery_model() -> Model {
    crate::ty_model! {
        OutputStreakEpisodeDelivery {
            const Buggy = 0;
            // 0 Idle, 1 Visible, 2 Parked/open, 3 Settled.
            var phase = 0;
            var shimmer = 0;
            var settle = 0;
            var wake = 0;
            // 1 while a human voice owns the shared min-gap.
            var human_gap = 0;
            // An output cue was delivered while the human slot was open; the next human
            // attempt checks that it stayed open.
            var audio_checked = 0;
            var next_human = 0;

            action HumanVoice when (phase == 0 && human_gap == 0) {
                human_gap = 1;
            }
            action OpenOutput when (phase == 0 && human_gap == 1) {
                phase = 1;
                shimmer = 1;
            }
            action RetireVisuals when (phase == 1) {
                phase = 2;
                wake = 1;
            }
            action SettleAtWake when (phase == 2 && wake == 1) {
                phase = 3;
                wake = 0;
                settle = 1;
            }
            action HumanGapElapses when (human_gap == 1) {
                human_gap = 0;
            }
            action CheckOutputDoesNotClaimHuman when (
                phase > 0 && human_gap == 0 && audio_checked == 0
            ) {
                audio_checked = 1;
            }
            action NextHuman when (
                audio_checked == 1 && human_gap == 0 && next_human == 0
            ) {
                next_human = 1;
                human_gap = 1;
            }
            action BuggyThinOpening when (
                Buggy == 1 && phase == 0 && human_gap == 1
            ) {
                phase = 1;
                shimmer = 0;
            }
            action BuggyRetireWithoutWake when (Buggy == 1 && phase == 1) {
                phase = 2;
                wake = 0;
            }
            action BuggyClaimHumanGap when (
                Buggy == 1 && phase > 0 && human_gap == 0 &&
                audio_checked == 0
            ) {
                audio_checked = 1;
                human_gap = 1;
            }

            bound Bounds:
                phase <= 3 && shimmer <= 1 && settle <= 1 && wake <= 1 &&
                human_gap <= 1 && audio_checked <= 1 && next_human <= 1;
            invariant OpeningCueSurvivesSharedGap:
                if phase > 0 { shimmer == 1 } else { shimmer == 0 };
            invariant ParkedEpisodeOwnsSettleWake:
                if phase == 2 { wake == 1 } else { wake <= 1 };
            invariant SettledEpisodeClosesExactlyOnce:
                if phase == 3 {
                    shimmer == 1 && settle == 1 && wake == 0
                } else {
                    settle == 0
                };
            invariant OutputNeverClaimsHumanGap:
                if audio_checked == 1 && next_human == 0 {
                    human_gap == 0
                } else {
                    human_gap <= 1
                };
            uncaught NextHumanFollowsCheckedOutput:
                if next_human == 1 {
                    audio_checked == 1
                } else {
                    next_human == 0
                };
        }
    }
}

/// Acceptance-ordered publication of the GUI's output-echo evidence.
///
/// A sink receipt is ordered where its bytes enter the direct/spill serializer, but its writer
/// may be descheduled before publishing it to the render-time tracker. Receipt 1 is an older
/// echoable input and receipt 2 the newer Enter turn boundary; their completions may publish in
/// either order. The newer boundary stays authoritative when the older completion arrives last,
/// and a sample sees one whole published timestamp tuple, never fields from both versions.
///
/// `aterm_session::sink::AcceptedOrder` exposes no number: `1` and `2` are the relative classes
/// Tier-1's real `older < newer` comparison establishes. Timestamps project to presence bits.
///
/// `Buggy=1` adds three independent defects: `PublishOlderEcho` overwrites a newer publication
/// (completion order wins); `Sample` mixes the boundary's order with the older echo's
/// timestamps (independent atomics instead of one mutex-owned record); and `PublishOlderEcho`
/// fires before the sink accepted the echo, an order minted at the write attempt
/// (`InputEpoch`'s shape) rather than at acceptance, which `OutputEchoWrite::finish` refuses by
/// publishing only on `receipt.accepted_order()` (`PublishFollowsAcceptance`). Tier-0 has an
/// explicit trace per mutant so one counterexample cannot mask another.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_echo_receipt_publication_model() -> Model {
    crate::ty_model! {
        OutputEchoReceiptPublication {
            const Buggy = 0;
            // Sink acceptance high-water: 0 none, 1 older echo, 2 newer Enter.
            var accepted = 0;
            var older_done = 0;
            var newer_done = 0;
            // Tracker high-water: 0 none, 1 older echo, 2 newer Enter.
            var published_order = 0;
            // Timestamp-presence projection of the one mutex-owned record.
            var accepted_shadow = 0;
            var boundary_shadow = 0;
            // One render-time sample of that record.
            var sampled = 0;
            var sample_order = 0;
            var sample_accepted = 0;
            var sample_boundary = 0;

            action AcceptOlderEcho when (accepted == 0) {
                accepted = 1;
            }
            action AcceptNewerBoundary when (accepted == 1) {
                accepted = 2;
            }
            action PublishOlderEcho when (
                (accepted > 0 || Buggy == 1) && older_done == 0
            ) {
                older_done = 1;
                published_order = if Buggy == 1 || published_order <= 0 {
                    1
                } else {
                    published_order
                };
                accepted_shadow = if Buggy == 1 || published_order <= 0 {
                    1
                } else {
                    accepted_shadow
                };
            }
            action PublishNewerBoundary when (accepted > 1 && newer_done == 0) {
                newer_done = 1;
                published_order = 2;
                accepted_shadow = 0;
                boundary_shadow = 1;
            }
            action Sample when (published_order > 0 && sampled == 0) {
                sampled = 1;
                sample_order = published_order;
                // `Buggy`: the sampler sees the boundary's store between the older
                // timestamp loads.
                sample_accepted = if Buggy == 1 && older_done == 1 &&
                    newer_done == 1 && published_order == 2 {
                    1
                } else {
                    accepted_shadow
                };
                sample_boundary = if Buggy == 1 && older_done == 1 &&
                    newer_done == 1 && published_order == 2 {
                    0
                } else {
                    boundary_shadow
                };
            }

            invariant LatestCompletedOrderWins:
                published_order == if newer_done == 1 {
                    2
                } else if older_done == 1 {
                    1
                } else {
                    0
                };
            invariant BoundaryRetiresOlderEcho:
                if newer_done == 1 {
                    published_order == 2 && accepted_shadow == 0 &&
                    boundary_shadow == 1
                } else {
                    boundary_shadow == 0
                };
            invariant SampleIsOnePublishedVersion:
                if sampled == 1 {
                    if sample_order == 1 {
                        sample_accepted == 1 && sample_boundary == 0
                    } else {
                        sample_order == 2 && sample_accepted == 0 &&
                        sample_boundary == 1
                    }
                } else {
                    sample_order == 0 && sample_accepted == 0 &&
                    sample_boundary == 0
                };
            // The tracker never publishes an order the sink has not accepted.
            invariant PublishFollowsAcceptance: published_order <= accepted;
            bound StateBounded:
                accepted <= 2 && older_done <= 1 && newer_done <= 1 &&
                published_order <= 2 && accepted_shadow <= 1 &&
                boundary_shadow <= 1 && sampled <= 1 && sample_order <= 2 &&
                sample_accepted <= 1 && sample_boundary <= 1;
        }
    }
}

/// Phosphor rain lifecycle (docs/matrix-rain-design.md §5/§10), in the [`nova_phase_model`]
/// shape: `{0 Idle, 1 Raining, 2 Draining}`. `Activity` (enable, a content-seq delta, a
/// licensed refocus resumption) is the only entry into Raining; `StartDrain` (idle sleep or
/// unfocus) opens the mandatory drain; `DrainTick` lands Idle at exactly `DrainBound = 30`
/// ticks, the engine's `DRAIN_TICKS` (the field decays `L` per tick, so 30 ticks empty every
/// column whatever the geometry).
///
/// `NoUnlicensedRain: rains <= acts`: every Raining entry is paid for by an activity event, so a
/// drained pane never relights on cmd-tab alone. `CanReachIdle` is the fuel invariant: while
/// Draining, `MaxSteps − steps` covers the `DrainBound − drained` ticks left. `MaxSteps = 64` is
/// tight: two full episodes take `2·(1 + 1 + 30)` steps, and 63 fails it at `Buggy = 0`.
/// `StateBounded`/`DrainBounded` are structural controls. `Buggy = 1` enables `Rearm`, which
/// re-enters Raining from Idle without an activity event.
///
/// Tier-1: aterm-effects' `rain_lifecycle_conformance_real_engine_projects_onto_model` drives
/// the real `MatrixRain` through enable, rain, idle-sleep drain and resume, projecting
/// `(lifecycle, drain_ticks)` onto this model tick for tick, with a phantom-relight negative
/// control.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
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
            // A drained pane relights with no activity event (phantom replay).
            action Rearm when (Buggy == 1 && state == 0 && steps <= MaxSteps - 1) {
                state = 1;
                drained = 0;
                rains = rains + 1;
                steps = steps + 1;
            }
            invariant NoUnlicensedRain: rains <= acts;
            // Fuel: the step budget always covers the remaining drain (nova `CanSettle`).
            invariant CanReachIdle:
                (if state == 2 { steps + DrainBound } else { 0 }) <= MaxSteps + drained;
            bound StateBounded: state <= 2;
            bound DrainBounded: drained <= DrainBound;
        }
    }
}

/// Phosphor rain band containment (docs/matrix-rain-design.md §7/§10): the damage law behind
/// `aterm_render::compute_dirty_rows` for `rain_quads`. Every emitted quad whose bytes changed
/// this tick lies in a marked dirty row, including a mutation tick, where the glyph hash window
/// rolls and every lit trail cell changes at once. Hand-built in the
/// [`deco_band_containment_model`] shape (the `in_range` picks need the `Expr` builders): a
/// phased walk picks one column's head row, its trail length, whether this is a mutation tick,
/// and a probe row anywhere in the band's visible part (the just-expired tail included); then
/// `Mark` records what the tick did to that row and what the marker decided.
///
/// The column lives in a `Viewport`-row window and its head may have run off the bottom
/// (`head >= Viewport`) while the tail stays on screen. The marker has two parts:
///
/// * the per-row merge-diff marks every row whose quad slice differs: the stepped head, the
///   expired tail (its quad vanished), and on a mutation tick every visible lit row;
/// * the scissor-band fill then marks every row carrying a current quad inside the band
///   `[first, last]` of what the merge-diff marked.
///
/// With both edges on screen the fill covers the whole band and would mask a merge-diff that
/// missed the mutation. Once the head has left, the fill band collapses onto the tail and the
/// mutation-changed interior rows are marked by the merge-diff or by nothing. `Buggy = 1` is a
/// merge-diff that sees the stepped edges and not the glyph swap: an interior row changes
/// unmarked under an off-screen head, the stale-glyph ghost.
///
/// Safety `Contained`: `changed <= marked` once the row is marked (indicator order, the
/// [`grid_translate_model`] idiom).
///
/// Tier-1: `aterm-render/tests/rain_render.rs::rain_marking_conforms_to_the_rain_band_containment_model`
/// feeds the shipping `compute_dirty_rows` one column's band for every point of this lattice and
/// requires each row's verdict to be the model's one `Mark` successor.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn rain_band_containment_model() -> Model {
    let tail = || sub(var("head"), var("l"));
    let head_visible = || le(var("head"), sub(cst("Viewport"), int(1)));
    // The last visible lit row: the head, or the viewport's bottom row.
    let last_lit = || if_(head_visible(), var("head"), sub(cst("Viewport"), int(1)));
    // A quad at `r` changed this tick: a stepped edge, or (mutation tick) any
    // lit row — `r` is picked inside the visible band, so lit is structural.
    let changed = || {
        if_(
            or_(
                or_(eq(var("r"), var("head")), eq(var("r"), tail())),
                eq(var("mt"), int(1)),
            ),
            int(1),
            int(0),
        )
    };
    // The last row the merge-diff marks (its first is the expired tail): every visible lit row
    // on a mutation tick (not under Buggy, whose diff sees only the stepped edges), else the
    // head if on screen, else the tail.
    let diff_last = || {
        if_(
            and_(eq(var("mt"), int(1)), eq(cst("Buggy"), int(0))),
            last_lit(),
            if_(head_visible(), var("head"), tail()),
        )
    };
    // The merge-diff's own rows plus the scissor-band fill: every row in
    // `[tail, diff_last]` is either the tail or carries a current quad.
    let marked = || if_(le(var("r"), diff_last()), int(1), int(0));
    Model {
        name: "RainBandContainment",
        consts: vec![("Buggy", 0), ("Viewport", 6)],
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
            StateVar {
                name: "changed",
                init: 0,
            },
            StateVar {
                name: "marked",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // Up to two rows past the viewport's bottom: the head may have
                // left the screen.
                name: "PickHead",
                guard: Some(eq(var("phase"), int(0))),
                updates: vec![
                    Update {
                        var: "head",
                        expr: in_range(int(2), add(cst("Viewport"), int(2))),
                    },
                    Update {
                        var: "phase",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // l >= 2 so a strictly interior row exists (what the Buggy marker misses);
                // l <= head keeps the expired tail at or below the top row, and the lower bound
                // keeps it on screen when the head has left.
                name: "PickTrail",
                guard: Some(eq(var("phase"), int(1))),
                updates: vec![
                    Update {
                        var: "l",
                        expr: in_range(
                            if_(
                                gt(sub(var("head"), sub(cst("Viewport"), int(1))), int(2)),
                                sub(var("head"), sub(cst("Viewport"), int(1))),
                                int(2),
                            ),
                            var("head"),
                        ),
                    },
                    Update {
                        var: "phase",
                        expr: int(2),
                    },
                ],
            },
            Action {
                // Mutation tick or plain stepping tick, both quantified.
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
                // The probe: any visible row of the band, expired tail included.
                name: "PickRow",
                guard: Some(eq(var("phase"), int(3))),
                updates: vec![
                    Update {
                        var: "r",
                        expr: in_range(tail(), last_lit()),
                    },
                    Update {
                        var: "phase",
                        expr: int(4),
                    },
                ],
            },
            Action {
                // What the tick did to the probed row, and the marker's verdict.
                name: "Mark",
                guard: Some(eq(var("phase"), int(4))),
                updates: vec![
                    Update {
                        var: "changed",
                        expr: changed(),
                    },
                    Update {
                        var: "marked",
                        expr: marked(),
                    },
                    Update {
                        var: "phase",
                        expr: int(5),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            // A changed quad row is always a marked dirty row; Buggy=1 leaves a mutation-tick
            // interior row unmarked.
            name: "Contained",
            kind: InvariantKind::Claim,
            expr: or_(neq(var("phase"), int(5)), le(var("changed"), var("marked"))),
        }],
    }
}

/// Phosphor rain ignition floor (docs/matrix-rain-design.md §4/§10): a column's head passes a
/// cell at most once per second, because the cycle carries the G-extension
/// `G = max(G_natural, ceil(1000/(p·tick_ms)) − rows − L)`
/// (`aterm_effects::matrix_rain::field::col_params`), so `(rows + L + G)·p·tick_ms >= 1000 ms`
/// however small the grid.
///
/// Hand-built with `rows` a nondeterministic variable: the walk picks `rows ∈ 3..=8` (where the
/// natural gap alone is too short), `p ∈ 2..=5` (2 is the fastest column), a clamped trail
/// `L ∈ 3..=rows` and `G_natural ∈ 1..=rows`, then settles `C`. `tick_ms` is pinned at 33 ms. With
/// no `*`, the product is repeated addition branched on `p`, and the checker verifies the floor
/// table `{16, 11, 8, 7}` against `>= 1000`. `Buggy = 1` drops the G-extension (a 3-row grid at
/// `p = 2` cycles in 462 ms). `CycleExceedsViewport` is the always-true non-vacuity control.
///
/// Tier-1: `matrix_rain`'s `rain_ignition_conformance_real_col_params_hold_the_flash_floor`
/// drives the real `col_params` over the same lattice and asserts `c * p * tick_ms >= 1000`.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
    // ceil(1000 / (p·33)) per period, the runtime flash floor; `HeadPassFloor` re-checks
    // `floor·p·33 >= 1000` rather than trusting the table.
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
                // Small grids, where the natural gap alone breaks the floor.
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
                // C = rows + L + G, with the G-extension unless Buggy.
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
                // The per-cell head-pass period is >= 1 s.
                // Buggy=1: rows=3, L=3, G=1, p=2 ⇒ 7·66 = 462 < 1000.
                name: "HeadPassFloor",
                kind: InvariantKind::Claim,
                expr: settled_implies(le(int(1000), product())),
            },
            Invariant {
                // Always-true control: the settled cycle clears viewport, minimum trail and a
                // live gap, so the floor is checked on non-degenerate cycles.
                name: "CycleExceedsViewport",
                kind: InvariantKind::Uncaught,
                expr: settled_implies(le(add(var("rows"), int(4)), var("c"))),
            },
        ],
    }
}

/// Generated cat-art collectible set. Each accepted semantic glyph key is inserted at most once,
/// so the unlocked count grows monotonically to the finite roster and a repeated sighting
/// changes only the encounter counters. `RosterCap` is pinned to `GLYPH_IDS.len()` by the Tier-1
/// conformance test in `aterm-gui::kitty_log`. `Buggy = 1` is an append-only event list: a
/// repeated key grows `unlocked` and `discoveries`, breaking `DuplicateIdempotent` and, with
/// enough repeats, `RosterBound`. Monotonicity is structural: no action decrements.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
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
            uncaught DiscoverySetAgreement: discoveries == unlocked;
            invariant DuplicateIdempotent: discoveries + duplicates == sightings;
            uncaught SightingsBounded: sightings <= MaxSightings;
        }
    }
}

/// Bidirectional rollback-safe collectible persistence. `Discover` writes the authoritative
/// sidecar and its embedded mirror. A collectible-aware rollback build can `OldDiscover` a new
/// key or `OldRepeat` an existing one into that mirror; `OldRepeatReset` is an existing key
/// detectably recreated after a pre-collectibles build erased the mirror (its `first_seen` is
/// after the baseline window). `Reconcile` imports only the positive delta against the stored
/// mirror baseline, so a replicated row is never counted twice. A pre-collectibles `OldRewrite`
/// may erase the replica only once it is reconciled; `RestoreMirror` rebuilds it from the
/// sidecar.
///
/// `Buggy = 1` is a base-only writer: `Discover` looks healthy to the live process but leaves the
/// sidecar's key and event counts behind, so `Discover -> OldRewrite` loses the unlock
/// (`NoUnlockRollback`, `NoCountRollback`). Same-timestamp and clock-regression ambiguity is
/// outside the count model (the runtime ledger documents it); the unlock-set invariant does not
/// depend on timestamps.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
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
            uncaught KnownBounded: known <= Cap;
            uncaught EventsBounded: events <= EventCap;
            uncaught BaseWithinKnown: base <= known;
            uncaught SidecarWithinKnown: sidecar <= known;
            uncaught PendingWithinKnown: pending <= known;
            uncaught PendingEventsWithinEvents: pending_events <= events;
            uncaught SidecarEventsWithinEvents: sidecar_events <= events;
            invariant NoUnlockRollback: durable == known;
            invariant NoCountRollback: durable_events == events;
        }
    }
}

/// Nonblocking Kitty Log flush-worker lifecycle. The capacity-one ordinary lane, a host-retained
/// tail, the shutdown-only exit lane, the worker's pending accumulator and durable storage form
/// one ownership equation (`AcceptedConserved`). A tail retained while the ordinary lane is full
/// moves only through `BeginExit → OfferTail → DrainNormal → AbsorbTail`; saturated delivery
/// never drops it.
///
/// Active lock contention is a stuttering observation that leaves the batch eligible for a later
/// flush without spending the exit budget; only after exit begins and both lanes drain does
/// contention advance the finite retry counter. A filesystem call that never returns advances
/// the event-loop-owned deadline and detaches at `DeadlineCap`, keeping the pending batch: a
/// crash window may be lost, but the UI thread never claims it persisted. `Buggy = 1` is a
/// one-lane exit: `OfferTail` clears the host tail while the full ordinary lane keeps it from
/// the exit lane.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_flush_worker_model() -> Model {
    crate::ty_model! {
        KittyFlushWorker {
            const Buggy = 0;
            // A slow worker may own one pending batch while the capacity-one ordinary lane
            // holds a second and the host retains a third tail.
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
            uncaught BatchBounded: accepted <= BatchCap;
            bound NormalLaneBounded: normal_lane <= 1;
            bound HostTailBounded: host_tail <= 1;
            bound ExitLaneBounded: exit_lane <= 1;
            uncaught RetryBounded: retries <= RetryCap;
            bound DeadlineBounded: deadline <= DeadlineCap;
            invariant AcceptedConserved:
                accepted == normal_lane + host_tail + exit_lane + pending + persisted;
            uncaught ExitLaneOnlyAfterExit: exit_lane <= exiting;
            uncaught JoinedOnlyAfterExit: joined <= exiting;
            uncaught StalledOnlyDuringExit: stalled <= exiting;
            uncaught DetachedOnlyAfterExit: detached <= exiting;
            uncaught OneExitDisposition: joined + detached <= 1;
            uncaught DetachedOnlyAtDeadline:
                if detached == 1 {
                    deadline == DeadlineCap
                } else {
                    1 == 1
                };
            uncaught JoinedHasFiniteOutcome:
                if joined == 1 {
                    normal_lane == 0 && host_tail == 0 && exit_lane == 0 &&
                    (pending == 0 || retries == RetryCap)
                } else {
                    1 == 1
                };
        }
    }
}
