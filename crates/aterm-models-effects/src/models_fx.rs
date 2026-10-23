// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlay-effect, rain, and kitty-persistence models.

use aterm_spec::derive::*;

/// Cell-observation snapshot protocol model for prism wake.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_streak_attribution_model() -> Model {
    aterm_spec::ty_model! {
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

/// Rainbow kitty blink-flare admission model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_idle_twinkle_model() -> Model {
    aterm_spec::ty_model! {
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
                flare_seq = if Buggy == 2 { flare_seq + 2 } else { flare_seq + 1 };
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
            invariant FlareSeqBounded: flare_seq <= steps;
            bound StepsBounded: steps <= MaxSteps;
        }
    }
}

/// Sparse-frame sampling model for the rainbow kitty finger-lift exit lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_exit_sampling_model() -> Model {
    aterm_spec::ty_model! {
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

/// Content-present and effect-timer ownership model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_present_rebase_model() -> Model {
    aterm_spec::ty_model! {
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

/// Focus and typed-wake demotion settle-present model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_presentability_settle_model() -> Model {
    aterm_spec::ty_model! {
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
                pending = 0; effect_active = 0; pixels_lit = if Buggy == 2 { 1 } else { 0 };
                settle_owed = 0; settles = 1; steps = steps + 1;
            }
            invariant LitPixelsHaveSettleRoute:
                if presentable == 0 && pixels_lit == 1 {
                    pending == 1 && settle_owed == 1
                } else { pending <= 1 };
            invariant SettledPixelsAreClear:
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

/// Phase-locked brisk effect cadence model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn effect_phase_lock_model() -> Model {
    aterm_spec::ty_model! {
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
                fresh = if Buggy == 3 { fresh } else { 0 };
            }
            action Fire when (pending == 1 && deadline <= now) {
                pending = 0;
                anchor = deadline;
                rendering = 1;
                phase_locked = 0;
                fresh = if Buggy == 3 { fresh } else { 0 };
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
                        if Buggy == 2 { now + Interval } else { anchor + Interval }
                    } else {
                        now + Interval
                    }
                };
                pending = 1;
                rendering = if Buggy == 4 { 1 } else { 0 };
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
            bound NowBounded: now <= MaxTime;
            bound BaseBounded: schedule_base <= MaxTime;
            bound DeadlineBounded: deadline <= MaxTime + Interval;
        }
    }
}

/// Cross-component episode delivery model for prism wake.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_streak_episode_delivery_model() -> Model {
    aterm_spec::ty_model! {
        OutputStreakEpisodeDelivery {
            const Buggy = 0;
            var phase = 0;
            var shimmer = 0;
            var settle = 0;
            var wake = 0;
            var human_gap = 0;
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
                (audio_checked == 1 || Buggy == 2) && human_gap == 0 && next_human == 0
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
            invariant NextHumanFollowsCheckedOutput:
                if next_human == 1 {
                    audio_checked == 1
                } else {
                    next_human == 0
                };
        }
    }
}

/// Acceptance-ordered publication model for GUI output-echo receipts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_echo_receipt_publication_model() -> Model {
    aterm_spec::ty_model! {
        OutputEchoReceiptPublication {
            const Buggy = 0;
            var accepted = 0;
            var older_done = 0;
            var newer_done = 0;
            var published_order = 0;
            var accepted_shadow = 0;
            var boundary_shadow = 0;
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
            invariant PublishFollowsAcceptance: published_order <= accepted;
            bound StateBounded:
                accepted <= 2 && older_done <= 1 && newer_done <= 1 &&
                published_order <= 2 && accepted_shadow <= 1 &&
                boundary_shadow <= 1 && sampled <= 1 && sample_order <= 2 &&
                sample_accepted <= 1 && sample_boundary <= 1;
        }
    }
}

/// Phosphor rain lifecycle model (`{0 Idle, 1 Raining, 2 Draining}`), following [`nova_phase_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rain_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        RainLifecycle {
            const Buggy = 0;
            const DrainBound = 30;
            const MaxActs = 2;
            const MaxSteps = 64;
            var state = 0;
            var drained = 0;
            var steps = 0;
            var acts = 0;
            var rains = 0;
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
            action Rearm when (Buggy == 1 && state == 0 && steps <= MaxSteps - 1) {
                state = 1;
                drained = 0;
                rains = rains + 1;
                steps = steps + 1;
            }
            invariant NoUnlicensedRain: rains <= acts;
            invariant CanReachIdle:
                (if state == 2 { steps + DrainBound } else { 0 }) <= MaxSteps + drained;
            bound StateBounded: state <= 2;
            bound DrainBounded: drained <= DrainBound;
        }
    }
}

/// Phosphor rain band-containment damage model (`compute_dirty_rows`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn rain_band_containment_model() -> Model {
    let tail = || sub(var("head"), var("l"));
    let head_visible = || le(var("head"), sub(cst("Viewport"), int(1)));
    let last_lit = || if_(head_visible(), var("head"), sub(cst("Viewport"), int(1)));
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
    let diff_last = || {
        if_(
            and_(eq(var("mt"), int(1)), eq(cst("Buggy"), int(0))),
            last_lit(),
            if_(head_visible(), var("head"), tail()),
        )
    };
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
            name: "Contained",
            kind: InvariantKind::Claim,
            expr: or_(neq(var("phase"), int(5)), le(var("changed"), var("marked"))),
        }],
    }
}

/// Collectible glyph-key set cardinality model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_collectibles_model() -> Model {
    aterm_spec::ty_model! {
        KittyCollectibles {
            const Buggy = 0;
            const RosterCap = 36;
            const MaxSightings = 37;
            var unlocked = 0;
            var discoveries = 0;
            var sightings = 0;
            var duplicates = 0;
            action Unlock when (
                unlocked <= RosterCap - 1 && sightings <= MaxSightings - 1
            ) {
                unlocked = if Buggy == 2 { unlocked } else { unlocked + 1 };
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
            bound SightingsBounded: sightings <= MaxSightings;
        }
    }
}

/// Bidirectional rollback-safe collectible persistence model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_sidecar_durability_model() -> Model {
    aterm_spec::ty_model! {
        KittySidecarDurability {
            const Buggy = 0;
            const Cap = 3;
            const EventCap = 6;
            var known = 0;
            var base = 0;
            var sidecar = 0;
            var pending = 0;
            var pending_events = 0;
            var durable = 0;
            var events = 0;
            var base_events = 0;
            var sidecar_events = 0;
            var mirror_events = 0;
            var durable_events = 0;
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
                pending_events = if Buggy == 5 { pending_events + 2 } else { pending_events + 1 };
                durable = durable + 1;
                events = events + 1;
                base_events = base_events + 1;
                durable_events = durable_events + 1;
            }
            action OldRepeat when (base > 0 && events <= EventCap - 1) {
                pending = if Buggy == 4 { pending + 1 } else { pending };
                events = events + 1;
                base_events = base_events + 1;
                pending_events = pending_events + 1;
                durable_events = durable_events + 1;
            }
            action OldRepeatReset when (
                known > 0 && base == 0 && events <= EventCap - 1
            ) {
                base = if Buggy == 2 { 2 } else { 1 };
                pending_events = pending_events + 1;
                events = events + 1;
                base_events = 1;
                durable_events = durable_events + 1;
            }
            action Reconcile when (pending + pending_events > 0) {
                sidecar = if Buggy == 3 { sidecar + pending + pending } else { sidecar + pending };
                pending = 0;
                sidecar_events = if Buggy == 3 {
                    sidecar_events + pending_events + pending_events
                } else {
                    sidecar_events + pending_events
                };
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
            bound KnownBounded: known <= Cap;
            bound EventsBounded: events <= EventCap;
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

/// Nonblocking Kitty Log flush-worker lifecycle model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_flush_worker_model() -> Model {
    aterm_spec::ty_model! {
        KittyFlushWorker {
            const Buggy = 0;
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
                (retries <= RetryCap - 1 || (Buggy == 2 && retries <= RetryCap))
            ) {
                retries = retries + 1;
            }
            action BeginExit when (exiting == 0) {
                exiting = 1;
            }
            action OfferTail when (
                (exiting == 1 || Buggy == 3) && host_tail == 1 && exit_lane == 0 &&
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
                pending > 0 && (exiting == 1 || Buggy == 5) &&
                (joined == 0 || Buggy == 6) && detached == 0 &&
                stalled == 0 && (retries <= RetryCap - 1 || Buggy == 6)
            ) {
                stalled = 1;
            }
            action TickDeadline when (
                stalled == 1 && detached == 0 && deadline <= DeadlineCap - 1
            ) {
                deadline = deadline + 1;
            }
            action Detach when (
                (exiting == 1 || Buggy == 5) && stalled == 1 && detached == 0 &&
                (deadline == DeadlineCap || Buggy == 7)
            ) {
                detached = 1;
            }
            action Join when (
                (exiting == 1 || Buggy == 4) && joined == 0 && detached == 0 && stalled == 0 &&
                normal_lane == 0 && (host_tail == 0 || Buggy == 8) && exit_lane == 0 &&
                (pending == 0 || retries == RetryCap)
            ) {
                joined = 1;
            }
            bound BatchBounded: accepted <= BatchCap;
            bound NormalLaneBounded: normal_lane <= 1;
            bound HostTailBounded: host_tail <= 1;
            bound ExitLaneBounded: exit_lane <= 1;
            invariant RetryBounded: retries <= RetryCap;
            bound DeadlineBounded: deadline <= DeadlineCap;
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
