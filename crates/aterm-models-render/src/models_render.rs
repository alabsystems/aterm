// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rendering, text, and contrast gate models.

use aterm_spec::derive::*;

/// Two native leaves' retained rasters across full-native, split, and terminal routes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_leaf_cache_lifetime_model() -> Model {
    aterm_spec::ty_model! {
        NativeLeafCacheLifetime {
            const Buggy = 0;
            var first_visible = 0;
            var second_visible = 0;
            var first_cached = 0;
            var second_cached = 0;
            action BindTerminal {
                first_visible = 0;
                second_visible = 0;
                first_cached = if Buggy == 1 { first_cached } else { 0 };
                second_cached = if Buggy == 1 { second_cached } else { 0 };
            }
            action BindFirst {
                first_visible = 1;
                second_visible = 0;
                second_cached = if Buggy == 1 { second_cached } else { 0 };
            }
            action BindSecond {
                first_visible = 0;
                second_visible = 1;
                first_cached = if Buggy == 1 { first_cached } else { 0 };
            }
            action BindSplit {
                first_visible = 1;
                second_visible = 1;
            }
            action Paint {
                first_cached = if first_visible == 1 { 1 } else { first_cached };
                second_cached = if second_visible == 1 { 1 } else { second_cached };
            }
            invariant OnlyVisibleLeavesRetainRasters:
                first_cached <= first_visible && second_cached <= second_visible;
        }
    }
}

/// Composed cursor frame reading a requested ribbon witness row from the latest cache.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn composed_witness_generation_model() -> Model {
    aterm_spec::ty_model! {
        ComposedWitnessGeneration {
            const Buggy = 0;
            var changed = 0;
            var captured = 0;
            var admitted = 0;
            action Capture when (captured == 0 && changed == 0) { captured = 1; }
            action Mutate when (changed == 0 && admitted == 0) { changed = 1; }
            action ReadFresh when (admitted == 0 && (changed == 0 || Buggy == 1)) {
                admitted = 1;
            }
            action ReadCaptured when (admitted == 0 && (captured == 1 || Buggy == 1)) {
                admitted = 2;
            }
            invariant CoherentFreshRead:
                admitted == 0 || admitted == 2 || changed == 0;
            invariant CapturedReadOwnsSnapshot:
                admitted == 0 || admitted == 1 || captured == 1;
        }
    }
}

/// Demand-driven native drawable acquisition state machine.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metal_drawable_acquire_model() -> Model {
    aterm_spec::ty_model! {
        MetalDrawableAcquire {
            const Buggy = 0;
            const MaxRequests = 2;
            const MaxEpoch = 2;
            var closed = 0;
            var pending = 0;
            var active = 0;
            var ready = 0;
            var epoch = 0;
            var request_epoch = 0;
            var retiring = 0;
            var retired = 0;
            var requests = 0;
            var completed = 0;
            var published = 0;
            var wakes = 0;
            var invalid_accept = 0;
            var main_wait = 0;
            action Request when (
                closed == 0 && requests <= MaxRequests - 1 &&
                (pending == 0 || Buggy == 1)
            ) {
                pending = pending + 1;
                active = active + 1;
                request_epoch = epoch;
                requests = requests + 1;
            }
            action Complete when (active > 0) {
                active = active - 1;
                completed = if Buggy == 3 { completed + 2 } else { completed + 1 };
                ready = if closed == 0 { ready + 1 } else { ready };
                pending = if closed == 0 { pending } else { pending - 1 };
                published = if closed == 0 { published + 1 } else { published };
                wakes = if closed == 0 { wakes + 1 } else { wakes };
            }
            action Take when (closed == 0 && ready > 0) {
                ready = ready - 1;
                pending = if Buggy == 2 { pending } else { pending - 1 };
                published = if Buggy == 4 { published + 1 } else { published };
            }
            action Invalidate when (epoch <= MaxEpoch - 1) {
                epoch = epoch + 1;
            }
            action AcceptResult when (
                Buggy == 1 || (closed == 0 && request_epoch == epoch)
            ) {
                invalid_accept = if closed == 0 && request_epoch == epoch {
                    invalid_accept
                } else { 1 };
            }
            action MainProgress {
                main_wait = if Buggy == 1 && pending > 0 { 1 } else { main_wait };
            }
            action Close when (closed == 0) {
                closed = 1;
                retiring = 1;
                pending = if Buggy == 6 { active + ready } else { active };
                ready = if Buggy == 6 { ready } else { 0 };
                wakes = if Buggy == 5 { wakes + 1 } else { wakes };
            }
            action Retire when (
                retiring == 1 && (active == 0 || Buggy == 1)
            ) {
                retiring = 0;
                retired = 1;
            }
            invariant OneOutstanding: pending <= 1;
            invariant OutstandingAccounted: pending == active + ready;
            invariant CallsBounded: requests <= MaxRequests && completed <= requests;
            invariant PublicationsBounded: published <= completed;
            invariant OnlyPublicationWakes: wakes == published;
            invariant ClosedHasNoReadyResult: closed == 0 || ready == 0;
            invariant RetirementFollowsAcquisition: retired == 0 || active == 0;
            invariant NoInvalidAcceptance: invalid_accept == 0;
            invariant NoCallerWait: main_wait == 0;
        }
    }
}

/// Ligature slicing gate for shaping-acceptance policy.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ligature_gate_model() -> Model {
    let n = 4;
    let one_to_one = || and_(gt(var("n_in"), int(0)), eq(var("n_out"), var("n_in")));
    let collapse = || and_(eq(var("n_out"), int(1)), gt(var("n_in"), int(1)));
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
            name: "ConservativeAccept",
            kind: InvariantKind::Claim,
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

/// HDR reconfigure color-space re-tag model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn hdr_reconfigure_retag_model() -> Model {
    aterm_spec::ty_model! {
        HdrReconfigureRetag {
            const Buggy = 0;
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
                is_f16 = 0;
                capture_linear = 0;
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
                is_f16 = 0;
                capture_linear = 0;
            }

            action BuggyRetagFailureIgnored when (Buggy == 1 && stage == 0) {
                stage = 2;
                retagged = 0;
                is_f16 = 1;
                capture_linear = 1;
            }

            action BuggyUpgradeFailureKeepsF16 when (Buggy == 1 && stage == 1) {
                stage = 2;
                retagged = 0;
                is_f16 = 1;
                capture_linear = 0;
            }

            action BuggyEscapeKeepsLinearCapture when (Buggy == 1 && stage == 0) {
                stage = 1;
                retagged = 0;
                is_f16 = 0;
                capture_linear = 1;
            }

            invariant ResolvedF16RequiresSuccessfulRetag:
                if stage == 2 && is_f16 == 1 {
                    retagged == 1
                } else {
                    retagged <= 1
                };
            invariant CaptureMatchesSurfaceEncoding:
                capture_linear == is_f16;
            bound ValuesBounded:
                stage <= 2 && retagged <= 1 &&
                is_f16 <= 1 && capture_linear <= 1;
        }
    }
}

/// Process-global serious-mode master gate model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn serious_mode_model() -> Model {
    aterm_spec::ty_model! {
        SeriousMode {
            const Buggy = 0;
            var serious = 0;
            var want_trail = 1;
            var want_bell = 1;
            var want_sparkle = 1;
            var want_rain = 1;
            var want_celebration = 1;
            var want_preview = 1;
            var want_gpu = 1;
            var trail = 1;
            var bell = 1;
            var sparkle = 1;
            var rain = 1;
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
                    rain == 0 && celebration == 0 &&
                    notice == 0 && preview == 0 && gpu == 0
                } else {
                    trail == want_trail && bell == want_bell &&
                    sparkle == want_sparkle && rain == want_rain &&
                    celebration == want_celebration &&
                    notice == want_celebration && preview == want_preview &&
                    gpu == want_gpu
                };
            bound SeriousBitBounded: serious <= 1;
            bound EffectiveBitsBounded:
                trail <= 1 && bell <= 1 && sparkle <= 1 &&
                rain <= 1 && celebration <= 1 &&
                notice <= 1 && preview <= 1 && gpu <= 1;
        }
    }
}

/// Bounded terminal incremental-search navigation state machine.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn emacs_search_navigation_model() -> Model {
    aterm_spec::ty_model! {
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
            var last_exit = 0;
            var pty_writes = 0;
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
                pty_writes = if Buggy == 1 { 1 } else { pty_writes };
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
                pty_writes = if Buggy == 1 { 1 } else { pty_writes };
            }
            action PublishHit when (active == 1 && hits <= Cap - 1) {
                query = 1;
                hits = hits + 1;
                current = if forward == 1 { 0 } else { hits };
                viewport = if forward == 1 { 0 } else { hits };
                selection = 1;
                dirty = 0;
                last_exit = 0;
            }
            action PublishMiss when (active == 1) {
                query = 1;
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
                last_exit = 0;
            }
            action Output when (active == 1 && query == 1) {
                dirty = 1;
                selection = if Buggy == 1 { selection } else { 0 };
            }
            action RepeatForward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 1;
                current = if current <= hits - 2 || (Buggy == 1 && current == hits - 1) {
                    current + 1
                } else {
                    0
                };
                viewport = if current <= hits - 2 || (Buggy == 1 && current == hits - 1) {
                    current + 1
                } else {
                    0
                };
                selection = 1;
                last_exit = 0;
            }
            action RepeatBackward when (active == 1 && hits > 0 && dirty == 0) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                last_exit = 0;
            }
            action RefreshRepeatForward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 1;
                current = if current <= hits - 2 { current + 1 } else { 0 };
                viewport = if current <= hits - 2 { current + 1 } else { 0 };
                selection = 1;
                dirty = 0;
            }
            action RefreshRepeatBackward when (active == 1 && hits > 0 && dirty == 1) {
                forward = 0;
                current = if current > 0 { current - 1 } else { hits - 1 };
                viewport = if current > 0 { current - 1 } else { hits - 1 };
                selection = 1;
                dirty = 0;
            }
            action RefreshMiss when (active == 1 && dirty == 1) {
                hits = 0;
                current = 0;
                selection = 0;
                dirty = 0;
            }
            action Cancel when (active == 1) {
                active = 0;
                viewport = if Buggy == 1 { viewport } else { origin };
                selection = 0;
                last_exit = 1;
            }
            action Accept when (active == 1 && dirty == 0) {
                active = 0;
                selection = if hits > 0 && Buggy == 0 { 1 } else { 0 };
                last_exit = 2;
            }
            invariant NoPtyLeak: pty_writes == 0;
            invariant CurrentOrdinalBounded:
                if hits == 0 { current == 0 } else { 0 <= current && current <= hits - 1 };
            bound HitCountBounded: hits <= Cap;
            bound DirectionBounded: forward <= 1;
            bound DirtyBounded: dirty <= 1;
            invariant StaleBatchNeverSelected:
                if dirty == 1 { selection == 0 } else { selection <= 1 };
            invariant CancelRestoresOrigin:
                if last_exit == 1 { viewport == origin && selection == 0 } else { viewport <= Cap };
            invariant AcceptKeepsHit:
                if last_exit == 2 && hits > 0 { selection == 1 } else { selection <= 1 };
        }
    }
}

/// Scroll-glide convergence and policy settlement model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn scroll_glide_model() -> Model {
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
            StateVar {
                name: "aim",
                init: 0,
            },
            StateVar {
                name: "drift",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
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
                    Update {
                        var: "aim",
                        expr: int(0),
                    },
                    Update {
                        var: "drift",
                        expr: int(0),
                    },
                ],
            },
            Action {
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
                    Update {
                        var: "aim",
                        expr: int(0),
                    },
                    Update {
                        var: "drift",
                        expr: int(0),
                    },
                ],
            },
            Action {
                name: "Output",
                guard: Some(and_(
                    and_(gt(var("armed"), int(0)), le(var("reduced"), int(0))),
                    and_(
                        and_(gt(var("pos"), int(0)), le(var("pos"), int(2))),
                        and_(gt(var("target"), int(0)), le(var("target"), int(2))),
                    ),
                )),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: add(var("pos"), int(1)),
                    },
                    Update {
                        var: "target",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("target"),
                            add(var("target"), int(1)),
                        ),
                    },
                    Update {
                        var: "aim",
                        expr: if_(eq(var("drift"), int(0)), var("target"), var("aim")),
                    },
                    Update {
                        var: "drift",
                        expr: add(var("drift"), int(1)),
                    },
                ],
            },
            Action {
                name: "Wake",
                guard: Some(and_(gt(var("armed"), int(0)), le(var("reduced"), int(0)))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: step(),
                    },
                    Update {
                        var: "armed",
                        expr: if_(
                            or_(
                                eq(step(), var("target")),
                                and_(
                                    eq(cst("Buggy"), int(2)),
                                    and_(
                                        le(sub(step(), var("target")), int(1)),
                                        le(sub(var("target"), step()), int(1)),
                                    ),
                                ),
                            ),
                            int(0),
                            int(1),
                        ),
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
                name: "SetReduced",
                guard: Some(le(var("reduced"), int(0))),
                updates: vec![
                    Update {
                        var: "pos",
                        expr: if_(eq(cst("Buggy"), int(1)), var("pos"), var("target")),
                    },
                    Update {
                        var: "armed",
                        expr: if_(eq(cst("Buggy"), int(1)), var("armed"), int(0)),
                    },
                    Update {
                        var: "reduced",
                        expr: int(1),
                    },
                ],
            },
            Action {
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
                name: "BoundedWakes",
                kind: InvariantKind::Claim,
                expr: le(var("wakes"), int(3)),
            },
            Invariant {
                name: "DisarmedAtTarget",
                kind: InvariantKind::Claim,
                expr: or_(gt(var("armed"), int(0)), eq(var("pos"), var("target"))),
            },
            Invariant {
                name: "ReducedSettled",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("reduced"), int(0)),
                    and_(le(var("armed"), int(0)), eq(var("pos"), var("target"))),
                ),
            },
            Invariant {
                name: "TargetIsAnchored",
                kind: InvariantKind::Claim,
                expr: or_(
                    or_(le(var("armed"), int(0)), le(var("drift"), int(0))),
                    eq(var("target"), add(var("aim"), var("drift"))),
                ),
            },
        ],
    }
}

/// Decoration band cell-containment model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn deco_band_containment_model() -> Model {
    let clamp_t = || {
        if_(
            le(var("raw_t"), int(0)),
            if_(gt(cst("Buggy"), int(0)), int(0), int(1)),
            if_(gt(var("raw_t"), var("cell_h")), var("cell_h"), var("raw_t")),
        )
    };
    let y_bound = || {
        if_(
            gt(cst("Buggy"), int(0)),
            var("cell_h"),
            sub(var("cell_h"), var("t")),
        )
    };
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
                name: "Contained",
                kind: InvariantKind::Claim,
                expr: settled_implies(le(add(var("y"), var("t")), var("cell_h"))),
            },
            Invariant {
                name: "ThicknessInCell",
                kind: InvariantKind::Claim,
                expr: or_(
                    neq(var("phase"), int(5)),
                    and_(le(int(1), var("t")), le(var("t"), var("cell_h"))),
                ),
            },
        ],
    }
}

/// Translucent vibrancy contrast-floor model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn vibrancy_contrast_model() -> Model {
    let maxf = || if_(gt(var("base"), cst("Floor")), var("base"), cst("Floor"));
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
            name: "NeverIllegible",
            kind: InvariantKind::Claim,
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

/// Window fit and symmetric padding absorption model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn pad_absorption_model() -> Model {
    let usable = || sub(sub(var("w"), var("pad")), var("pad"));
    let rem = || sub(usable(), var("acc"));
    let half = || if_(le(rem(), int(1)), int(0), int(1));
    let lo_share = || if_(eq(cst("Buggy"), int(1)), int(0), half());
    let dial = |k: i64| eq(cst("Buggy"), int(k));
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
                name: "Settle",
                guard: Some(and_(
                    eq(var("phase"), int(3)),
                    or_(gt(add(var("acc"), var("cell")), usable()), dial(4)),
                )),
                updates: vec![
                    Update {
                        var: "pad_lo",
                        expr: if_(dial(3), lo_share(), add(var("pad"), lo_share())),
                    },
                    Update {
                        var: "pad_hi",
                        expr: if_(dial(2), var("pad"), add(var("pad"), sub(rem(), lo_share()))),
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
                name: "ExactCover",
                kind: InvariantKind::Claim,
                expr: settled_implies(eq(
                    add(add(var("pad_lo"), var("acc")), var("pad_hi")),
                    var("w"),
                )),
            },
            Invariant {
                name: "PadsFloor",
                kind: InvariantKind::Claim,
                expr: settled_implies(and_(
                    le(var("pad"), var("pad_lo")),
                    le(var("pad"), var("pad_hi")),
                )),
            },
            Invariant {
                name: "NearEvenSplit",
                kind: InvariantKind::Claim,
                expr: settled_implies(and_(
                    le(var("pad_lo"), var("pad_hi")),
                    le(var("pad_hi"), add(var("pad_lo"), int(1))),
                )),
            },
            Invariant {
                name: "Maximal",
                kind: InvariantKind::Claim,
                expr: settled_implies(gt(
                    add(var("acc"), var("cell")),
                    sub(sub(var("w"), var("pad")), var("pad")),
                )),
            },
        ],
    }
}

/// Raw-renderer asymmetric top-padding redistribution and cache identity model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
    let non_absorbing = || and_(eq(cst("Buggy"), int(1)), eq(var("top_fault"), int(1)));
    let initial_bottom = || {
        if_(
            eq(cst("Buggy"), int(1)),
            var("pad"),
            sub(add(var("pad"), var("pad")), clamp_initial()),
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
            StateVar {
                name: "gate_fault",
                init: 0,
            },
            StateVar {
                name: "top_fault",
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
                        var: "gate_fault",
                        expr: in_range(int(0), cst("Buggy")),
                    },
                    Update {
                        var: "top_fault",
                        expr: in_range(int(0), cst("Buggy")),
                    },
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
                        expr: if_(non_absorbing(), var("changed_request"), clamp_changed()),
                    },
                    Update {
                        var: "pad_bottom",
                        expr: if_(
                            non_absorbing(),
                            var("pad"),
                            sub(add(var("pad"), var("pad")), changed_top()),
                        ),
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
                            if_(eq(var("gate_fault"), int(1)), int(0), int(1)),
                            if_(layouts_match(), int(1), int(0)),
                        ),
                    },
                    Update {
                        var: "full_repaint",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            if_(eq(var("gate_fault"), int(1)), int(1), int(0)),
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
                kind: InvariantKind::Claim,
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
                kind: InvariantKind::Claim,
                expr: if_(
                    settled_layout(),
                    le(var("pad_top"), var("pad")),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "GridOriginTracksTopAndHead",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled_layout(),
                    eq(var("grid_top"), add(var("head"), var("pad_top"))),
                    eq(var("grid_top"), int(0)),
                ),
            },
            Invariant {
                name: "LayoutChangeForcesFullRepaint",
                kind: InvariantKind::Claim,
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
                kind: InvariantKind::Claim,
                expr: if_(
                    and_(eq(var("phase"), int(5)), layouts_match()),
                    and_(
                        eq(var("cache_hit"), int(1)),
                        eq(var("full_repaint"), int(0)),
                    ),
                    le(var("full_repaint"), int(1)),
                ),
            },
        ],
    }
}

/// GUI-visible crop from the renderer's conserved raw padding allocation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
                        expr: if_(eq(cst("Buggy"), int(1)), var("request"), resolved_top()),
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
                name: "VisibleTopMatchesRendererTop",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_pad_top"), var("pad_top")),
                    eq(var("pad_top"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleBottomIsBasePad",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_pad_bottom"), var("pad")),
                    eq(var("visible_pad_bottom"), int(0)),
                ),
            },
            Invariant {
                name: "VisibleHeightUsesIndependentEdges",
                kind: InvariantKind::Claim,
                expr: if_(
                    settled(),
                    eq(var("visible_height"), visible_height()),
                    eq(var("visible_height"), int(0)),
                ),
            },
            Invariant {
                name: "CropDeletesOnlyRemovedTop",
                kind: InvariantKind::Claim,
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

/// CoreText sub-pixel fractional bearing decomposition model.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ct_frac_bearing_model() -> Model {
    let rounded = || {
        if_(
            gt(var("rem"), int(3)),
            add(var("bearing"), int(8)),
            var("bearing"),
        )
    };
    let report_bearing = if_(eq(cst("Buggy"), int(1)), rounded(), var("bearing"));
    let report_rem = if_(eq(cst("Buggy"), int(1)), int(0), var("rem"));
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
                name: "Report",
                guard: Some(and_(
                    eq(var("phase"), int(2)),
                    or_(rem_in_unit(), eq(cst("Buggy"), int(2))),
                )),
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
                name: "Decompose",
                kind: InvariantKind::Claim,
                expr: or_(
                    le(var("phase"), int(1)),
                    eq(add(var("bearing"), var("rem")), var("b")),
                ),
            },
            Invariant {
                name: "PhaseInUnit",
                kind: InvariantKind::Claim,
                expr: or_(neq(var("phase"), int(3)), rem_in_unit()),
            },
        ],
    }
}
