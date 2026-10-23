// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Terminal-core, presentation-loop, and capability-plumbing models.

use aterm_spec::derive::*;

/// Terminal modes: the DEC/ANSI mode-flag state machine the VT handler maintains, and the reset discipline over it.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn terminal_modes_model() -> Model {
    const TOGGLES: [(&str, &str, &str); 13] = [
        (
            "SetApplicationCursorKeys",
            "ResetApplicationCursorKeys",
            "app_cursor_keys",
        ),
        ("SetOriginMode", "ResetOriginMode", "origin_mode"),
        ("SetAutoWrap", "ResetAutoWrap", "auto_wrap"),
        ("SetCursorVisible", "ResetCursorVisible", "cursor_visible"),
        (
            "SetFocusReporting",
            "ResetFocusReporting",
            "focus_reporting",
        ),
        (
            "SetSynchronizedOutput",
            "ResetSynchronizedOutput",
            "sync_output",
        ),
        ("SetInsertMode", "ResetInsertMode", "insert_mode"),
        ("SetNewLineMode", "ResetNewLineMode", "new_line_mode"),
        ("SetAlternateScreen", "ResetAlternateScreen", "alt_screen"),
        (
            "SetBracketedPaste",
            "ResetBracketedPaste",
            "bracketed_paste",
        ),
        ("SetSgrMouseEncoding", "ResetSgrMouseEncoding", "sgr_mouse"),
        ("SetMouseMode", "ResetMouseMode", "mouse_mode"),
        ("SetCursorStyle", "RestoreCursorStyle", "cursor_style"),
    ];
    const RESET_DEFAULTS: [(&str, i64); 12] = [
        ("app_cursor_keys", 0),
        ("origin_mode", 0),
        ("auto_wrap", 1),
        ("cursor_visible", 1),
        ("focus_reporting", 0),
        ("sync_output", 0),
        ("insert_mode", 0),
        ("new_line_mode", 0),
        ("bracketed_paste", 0),
        ("mouse_mode", 0),
        ("sgr_mouse", 0),
        ("cursor_style", 0),
    ];
    let set = |mode: &'static str, expr: Expr| Update { var: mode, expr };
    let plain = |name: &'static str, mode: &'static str, expr: Expr| Action {
        name,
        guard: None,
        updates: vec![set(mode, expr), set("reset", int(0))],
    };
    let reset = |name: &'static str, leave_alt: bool| {
        let mut updates: Vec<Update> = RESET_DEFAULTS
            .iter()
            .map(|&(mode, default)| {
                // Buggy: DECSTR forgets the synchronized-output hold.
                if mode == "sync_output" && name == "SoftReset" {
                    set(mode, if_(eq(cst("Buggy"), int(1)), var(mode), int(default)))
                } else {
                    set(mode, int(default))
                }
            })
            .collect();
        if leave_alt {
            updates.push(set("alt_screen", int(0)));
        }
        updates.push(set("reset", int(1)));
        Action {
            name,
            guard: None,
            updates,
        }
    };

    let mut actions = Vec::new();
    for (on, off, mode) in TOGGLES {
        actions.push(plain(on, mode, int(1)));
        actions.push(plain(off, mode, int(0)));
    }
    actions.push(reset("SoftReset", false));
    actions.push(reset("FullReset", true));

    let mut vars: Vec<StateVar> = TOGGLES
        .iter()
        .map(|&(_, _, name)| StateVar {
            name,
            init: RESET_DEFAULTS
                .iter()
                .find(|(mode, _)| *mode == name)
                .map_or(0, |&(_, default)| default),
        })
        .collect();
    vars.push(StateVar {
        name: "reset",
        init: 0,
    });

    let restored = RESET_DEFAULTS
        .iter()
        .map(|&(mode, default)| eq(var(mode), int(default)))
        .reduce(and_)
        .unwrap_or_else(|| bool_lit(true));
    Model {
        name: "TerminalModes",
        consts: vec![("Buggy", 0)],
        vars,
        fn_vars: vec![],
        actions,
        invariants: vec![Invariant {
            name: "ResetRestoresDefaults",
            kind: InvariantKind::Claim,
            expr: or_(eq(var("reset"), int(0)), restored),
        }],
    }
}

/// Which defect a wake-latch model's `Buggy = 1` replays: one machine
#[derive(Clone, Copy, PartialEq, Eq)]
enum WakeLatchMutant {
    /// A one-shot latch: no heal, no watchdog deadline and no re-post, so a lost wake starves its session's presents.
    OneShot,
    /// An age-only heal: a latch older than `Expiry` is presumed lost even while its event is still queued behind a long frame (a false heal).
    AgeOnly,
}

/// Damage→present bounded response.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn damage_to_present_model() -> Model {
    wake_latch_model(WakeLatchMutant::OneShot)
}

/// No false heal: the [`damage_to_present_model`] machine held to the other half of the contract, that the watchdog never heals a wake that is merely late (still queued behind the main thread's turn).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn wake_heal_evidence_model() -> Model {
    wake_latch_model(WakeLatchMutant::AgeOnly)
}

#[cfg_attr(trust_verify, trust::skip)]
fn wake_latch_model(mutant: WakeLatchMutant) -> Model {
    let armed = neq(var("latch"), int(0));
    let fixed = eq(cst("Buggy"), int(0));
    let mutated = eq(cst("Buggy"), int(1));
    let idle = eq(var("busy"), int(0));
    let proven_lost = and_(armed.clone(), eq(var("seen"), int(1)));
    let aged = gt(sub(var("now"), var("latch")), cst("Expiry"));
    let stranded_due = and_(
        armed.clone(),
        and_(idle.clone(), and_(eq(var("seen"), int(0)), aged.clone())),
    );
    let not_stranded_due = or_(
        eq(var("busy"), int(1)),
        or_(
            eq(var("seen"), int(1)),
            le(sub(var("now"), var("latch")), cst("Expiry")),
        ),
    );
    let (heal, recheck_on, repost_guard, coalesce_guard) = match mutant {
        WakeLatchMutant::OneShot => (
            and_(fixed.clone(), proven_lost),
            fixed.clone(),
            and_(fixed, stranded_due.clone()),
            and_(armed.clone(), or_(mutated.clone(), not_stranded_due)),
        ),
        WakeLatchMutant::AgeOnly => (
            or_(
                and_(fixed, proven_lost),
                and_(mutated, and_(armed.clone(), aged)),
            ),
            bool_lit(true),
            stranded_due.clone(),
            and_(armed.clone(), not_stranded_due),
        ),
    };
    let damage = Update {
        var: "pending",
        expr: int(1),
    };
    let clear_if = |cond: Expr, v: &'static str| Update {
        var: v,
        expr: if_(cond, int(0), var(v)),
    };
    let set = |v: &'static str, e: Expr| Update { var: v, expr: e };
    let wake_queued = eq(var("inflight"), int(1));
    let stranded = and_(
        armed.clone(),
        and_(
            eq(var("seen"), int(0)),
            and_(eq(var("inflight"), int(0)), idle.clone()),
        ),
    );
    let (name, invariants) = match mutant {
        WakeLatchMutant::OneShot => (
            "DamageToPresent",
            vec![
                Invariant {
                    name: "DamageBounded",
                    kind: InvariantKind::Claim,
                    expr: le(var("waited"), add(cst("Expiry"), int(1))),
                },
                Invariant {
                    name: "StrandedReposted",
                    kind: InvariantKind::Claim,
                    expr: eq(var("missedRepost"), int(0)),
                },
            ],
        ),
        WakeLatchMutant::AgeOnly => (
            "WakeHealOnlyWhenLost",
            vec![Invariant {
                name: "NoFalseHeal",
                kind: InvariantKind::Claim,
                expr: eq(var("falseHeal"), int(0)),
            }],
        ),
    };
    let var0 = |name: &'static str| StateVar { name, init: 0 };
    Model {
        name,
        consts: vec![
            ("MaxTime", 8),
            ("Expiry", 3),
            ("LostBudget", 1),
            ("Buggy", 0),
        ],
        vars: vec![
            StateVar {
                name: "now",
                init: 1,
            },
            var0("pending"),
            var0("waited"),
            var0("latch"),
            var0("inflight"),
            var0("lost"),
            StateVar {
                name: "busy",
                init: 1,
            },
            var0("seen"),
            var0("drained"),
            var0("recheck"),
            var0("falseHeal"),
            var0("missedRepost"),
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "DamageFresh",
                guard: Some(eq(var("latch"), int(0))),
                updates: vec![
                    damage.clone(),
                    set("latch", var("now")),
                    set("inflight", int(1)),
                    set("seen", int(0)),
                ],
            },
            Action {
                name: "DamageCoalesced",
                guard: Some(coalesce_guard),
                updates: vec![
                    damage.clone(),
                    set(
                        "missedRepost",
                        if_(stranded_due.clone(), int(1), var("missedRepost")),
                    ),
                ],
            },
            Action {
                name: "DamageRepost",
                guard: Some(repost_guard),
                updates: vec![
                    damage,
                    set("latch", var("now")),
                    set("inflight", int(1)),
                    set("seen", int(0)),
                ],
            },
            Action {
                name: "Lose",
                guard: Some(and_(
                    wake_queued.clone(),
                    le(add(var("lost"), int(1)), cst("LostBudget")),
                )),
                updates: vec![
                    set("inflight", int(0)),
                    set("lost", add(var("lost"), int(1))),
                ],
            },
            Action {
                name: "TurnBegin",
                guard: Some(idle.clone()),
                updates: vec![
                    set("busy", int(1)),
                    clear_if(wake_queued.clone(), "pending"),
                    clear_if(wake_queued.clone(), "waited"),
                    clear_if(wake_queued.clone(), "latch"),
                    clear_if(wake_queued.clone(), "inflight"),
                    clear_if(wake_queued.clone(), "seen"),
                    set("recheck", int(0)),
                ],
            },
            Action {
                name: "TurnEnd",
                guard: Some(eq(var("busy"), int(1))),
                updates: vec![
                    set("busy", int(0)),
                    set("drained", var("now")),
                    clear_if(heal.clone(), "pending"),
                    clear_if(heal.clone(), "waited"),
                    clear_if(heal.clone(), "latch"),
                    set(
                        "seen",
                        if_(heal.clone(), int(0), if_(armed.clone(), int(1), int(0))),
                    ),
                    set(
                        "recheck",
                        if_(
                            heal.clone(),
                            int(0),
                            if_(
                                and_(recheck_on, armed.clone()),
                                add(var("now"), add(cst("Expiry"), int(1))),
                                int(0),
                            ),
                        ),
                    ),
                    set(
                        "falseHeal",
                        if_(and_(heal, wake_queued.clone()), int(1), var("falseHeal")),
                    ),
                ],
            },
            Action {
                name: "Tick",
                guard: Some(and_(
                    gt(cst("MaxTime"), var("now")),
                    or_(
                        eq(var("busy"), int(1)),
                        and_(
                            eq(var("inflight"), int(0)),
                            or_(
                                eq(var("latch"), int(0)),
                                or_(eq(var("recheck"), int(0)), gt(var("recheck"), var("now"))),
                            ),
                        ),
                    ),
                )),
                updates: vec![
                    set("now", add(var("now"), int(1))),
                    set(
                        "waited",
                        if_(
                            stranded,
                            var("waited"),
                            if_(
                                and_(idle, eq(var("pending"), int(1))),
                                add(var("waited"), int(1)),
                                var("waited"),
                            ),
                        ),
                    ),
                ],
            },
        ],
        invariants,
    }
}

/// Observation kernel, earliest-armed idle deadline: the abstract twin of `WatcherSet::next_deadline`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn idle_deadline_model() -> Model {
    let arm = |v: i64| -> Expr {
        if_(
            eq(cst("Buggy"), int(0)),
            if_(gt(var("armed"), int(v)), int(v), var("armed")),
            if_(eq(var("armed"), int(4)), int(v), var("armed")),
        )
    };
    Model {
        name: "IdleDeadline",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "armed",
                init: 4,
            },
            StateVar {
                name: "minp",
                init: 4,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "ArmNear",
                guard: Some(gt(var("minp"), int(1))),
                updates: vec![
                    Update {
                        var: "minp",
                        expr: if_(gt(var("minp"), int(1)), int(1), var("minp")),
                    },
                    Update {
                        var: "armed",
                        expr: arm(1),
                    },
                ],
            },
            Action {
                name: "ArmFar",
                guard: Some(gt(var("minp"), int(2))),
                updates: vec![
                    Update {
                        var: "minp",
                        expr: if_(gt(var("minp"), int(2)), int(2), var("minp")),
                    },
                    Update {
                        var: "armed",
                        expr: arm(2),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "EarliestArmed",
            kind: InvariantKind::Claim,
            expr: eq(var("armed"), var("minp")),
        }],
    }
}

/// Surface coverage along one axis (applied to x and y it covers all four bands).
pub fn surface_coverage_model() -> Model {
    aterm_spec::ty_model! {
        SurfaceCoverage {
            const Buggy = 0;
            const Surface = 5;
            var frame = 3;
            var covered = 0;
            var band_live = 0;
            var presented = 0;

            action Zoom when (frame <= Surface - 1) {
                frame = frame + 1;
                covered = 0;
                band_live = 0;
                presented = 0;
            }
            action Present {
                covered = if Buggy == 1 { frame } else { Surface };
                band_live = if Buggy == 1 { 0 } else { 1 };
                presented = 1;
            }

            bound FrameFitsSurface: frame <= Surface;
            invariant PresentCoversSurface:
                if presented == 1 { covered == Surface } else { 1 == 1 };
            invariant RemainderUsesLiveBackground:
                if presented == 1 && frame <= Surface - 1 {
                    band_live == 1
                } else {
                    1 == 1
                };
        }
    }
}

/// First-present phase publication: a valid partition cannot publish before all eight intervals exist.
pub fn startup_phase_publication_model() -> Model {
    aterm_spec::ty_model! {
        StartupPhasePublication {
            const Buggy = 0;
            const PhaseCount = 8;
            var phase = 0;
            var published = 0;

            action Step when (phase <= PhaseCount - 1) {
                phase = phase + 1;
            }
            action Publish when (
                published == 0 && (phase == PhaseCount || Buggy == 1)
            ) {
                published = 1;
            }

            bound PhaseBounded: phase <= PhaseCount;
            invariant CompleteBeforeValidPublication:
                if published == 1 { phase == PhaseCount } else { 1 == 1 };
        }
    }
}

/// Present retry: bounded, future-only autonomous recovery after a dropped surface transaction.
pub fn present_retry_model() -> Model {
    aterm_spec::ty_model! {
        PresentRetry {
            const Buggy = 0;
            const RetryCap = 5;
            var remaining = 5;
            var train = 0;
            var retry = 0;
            var ready = 1;
            var parked = 0;
            var outstanding = 0;
            var bootstrap_available = 1;

            action Drop when (ready == 1) {
                ready = 0;
                retry = if remaining > 0 { if Buggy == 2 { 2 } else { 1 } } else { 0 };
                parked = if remaining > 0 { 0 } else { if Buggy == 4 { 0 } else { 1 } };
                outstanding = if Buggy == 5 { outstanding } else { 0 };
            }
            action DropBootstrapOccluded when (
                bootstrap_available == 1 && remaining == RetryCap && train == 0 &&
                retry == 0 && ready == 1 && parked == 0 && outstanding == 0
            ) {
                ready = 0;
                retry = 1;
                parked = 0;
                outstanding = 0;
                bootstrap_available = 0;
            }
            action DropPersistent when (ready == 1) {
                ready = 0;
                retry = if Buggy == 3 { 1 } else { 0 };
                parked = 1;
                outstanding = 0;
            }
            action Wake when (retry > 0) {
                retry = 0;
                ready = 1;
                remaining = if Buggy == 1 { remaining } else { remaining - 1 };
                train = train + 1;
                parked = 0;
                outstanding = 1;
            }
            action Present when (ready == 1) {
                remaining = RetryCap;
                train = 0;
                retry = 0;
                ready = 1;
                parked = 0;
                outstanding = 0;
                bootstrap_available = 0;
            }
            action Stimulus when (
                remaining <= RetryCap - 1 || train > 0 || retry > 0 || ready == 0 ||
                parked == 1 || outstanding == 1)
            {
                remaining = RetryCap;
                train = 0;
                retry = 0;
                ready = 1;
                parked = 0;
                outstanding = 1;
            }
            action SynchronousStimulus when (
                remaining <= RetryCap - 1 || train > 0 || retry > 0 || ready == 0 ||
                parked == 1 || outstanding == 1)
            {
                remaining = RetryCap;
                train = 0;
                retry = 0;
                ready = 1;
                parked = 0;
            }
            action ForcedStimulus {
                remaining = RetryCap;
                train = 0;
                retry = 0;
                ready = 1;
                parked = 0;
                outstanding = 1;
            }

            bound Bounds:
                remaining <= RetryCap && retry <= 2 && ready <= 1 && parked <= 1 &&
                outstanding <= 1 && bootstrap_available <= 1;
            invariant RetryDeadlineIsStrictlyFuture: retry <= 1;
            invariant AutonomousTrainBound: train <= RetryCap;
            invariant ParkedHasNoDeadline:
                if parked == 1 { retry == 0 } else { 1 == 1 };
            invariant ExhaustedAttemptParks:
                if remaining == 0 && ready == 0 { parked == 1 } else { 1 == 1 };
            invariant OutstandingRedrawHasOpenGate:
                if outstanding == 1 { ready == 1 && retry == 0 && parked == 0 }
                else { 1 == 1 };
        }
    }
}

/// GPU device-loss routing.
pub fn gpu_loss_route_model() -> Model {
    aterm_spec::ty_model! {
        GpuLossRoute {
            const Buggy = 0;
            var lost = 0;
            var route = 0;

            action FailHealthy {
                lost = 0;
                route = 1;
            }
            action FailLost {
                lost = 1;
                route = if Buggy == 1 { 1 } else { 2 };
            }
            action Reset {
                lost = 0;
                route = 0;
            }

            bound RouteRange: route <= 2;
            invariant LostUsesFallback:
                if lost == 1 { route == 2 } else { 1 == 1 };
        }
    }
}

/// GPU DEVICE-LOSS RECOVERY.
pub fn gpu_loss_recovery_model() -> Model {
    aterm_spec::ty_model! {
        GpuLossRecovery {
            const Buggy = 0;
            var path = 0;
            var fallback_failed = 0;
            var retry = 0;
            var delivered = 0;
            var exhausted = 0;
            var parked = 0;
            var cpu_ready = 0;
            var requested = 0;
            var cpu_presented = 0;
            var drops = 0;
            var reason = 0;
            var recording = 1;

            action FailFallbackAfterPresent {
                path = 1;
                fallback_failed = 1;
                retry = 1;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = 0;
            }
            action FailFallbackAfterDropWithFuel {
                path = 2;
                fallback_failed = 1;
                retry = 1;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = 0;
            }
            action FailFallbackAfterDropExhausted {
                path = 2;
                fallback_failed = 1;
                retry = 0;
                delivered = 0;
                exhausted = 1;
                parked = 1;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = 0;
            }
            action SucceedFallbackAfterPresent {
                path = 1;
                fallback_failed = 0;
                retry = 0;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 1;
                requested = 1;
                cpu_presented = 0;
                drops = 0;
                reason = 0;
                recording = 0;
            }
            action SucceedFallbackAfterDrop {
                path = 2;
                fallback_failed = 0;
                retry = 0;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 1;
                requested = 1;
                cpu_presented = 0;
                drops = 1;
                reason = 1;
                recording = 0;
            }
            action Wake when (retry == 1) {
                retry = 0;
                delivered = 1;
            }
            action BuildCpuAfterWake when (delivered == 1) {
                delivered = 0;
                fallback_failed = 0;
                cpu_ready = 1;
                requested = 1;
            }
            action PresentCpu when (requested == 1) {
                requested = 0;
                cpu_presented = 1;
            }
            action BuggyLossKeepsRecording when (Buggy == 1) {
                path = 1;
                fallback_failed = 0;
                retry = 0;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 1;
                requested = 1;
                cpu_presented = 0;
                drops = 0;
                reason = 0;
                recording = 1;
            }
            action BuggyFailedPresentOmitsRetry when (Buggy == 1) {
                path = 1;
                fallback_failed = 1;
                retry = 0;
                delivered = 0;
                exhausted = 0;
                parked = 1;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = 0;
            }
            action BuggyDropCountedTwice when (Buggy == 1) {
                path = 2;
                fallback_failed = 1;
                retry = 1;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 2;
                reason = 2;
                recording = 0;
            }
            action BuggyFuelledDropKeepsSourceReason when (Buggy == 1) {
                path = 2;
                fallback_failed = 1;
                retry = 1;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 1;
                recording = 0;
            }
            action BuggyExhaustedArmsRetry when (Buggy == 1) {
                path = 2;
                fallback_failed = 1;
                retry = 1;
                delivered = 0;
                exhausted = 1;
                parked = 0;
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = 0;
            }
            action BuggyWakeKeepsDeadline when (Buggy == 1 && retry == 1) {
                retry = 1;
                delivered = 1;
            }
            action BuggyReadyWithoutRedraw when (Buggy == 1) {
                path = 2;
                fallback_failed = 0;
                retry = 0;
                delivered = 0;
                exhausted = 0;
                parked = 0;
                cpu_ready = 1;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 1;
                recording = 0;
            }
            action BuggyPresentOnWake when (
                Buggy == 1 && delivered == 1 && requested == 0
            ) {
                requested = 0;
                cpu_presented = 1;
            }

            bound Bounds:
                path <= 2 && fallback_failed <= 1 && retry <= 1 && delivered <= 1 &&
                exhausted <= 1 && parked <= 1 && cpu_ready <= 1 && requested <= 1 &&
                cpu_presented <= 1 &&
                drops <= 2 && reason <= 2 && recording <= 1;
            invariant LossStopsGpuRecording:
                if path > 0 { recording == 0 } else { 1 == 1 };
            invariant UnexhaustedFailureOwnsRetryOrDeliveredAttempt:
                if fallback_failed == 1 && exhausted == 0 && delivered == 0 {
                    retry == 1
                } else { 1 == 1 };
            invariant ExhaustedFailureIsParked:
                if exhausted == 1 { parked == 1 && retry == 0 } else { 1 == 1 };
            invariant DeliveredRetryHasNoDeadline:
                if delivered == 1 { retry == 0 } else { 1 == 1 };
            invariant OneDropCountPerFrame: drops <= 1;
            invariant FailedFallbackIsDiagnosed:
                if fallback_failed == 1 { reason == 2 } else { 1 == 1 };
            invariant ReadyCpuOwnsRedrawUntilPresent:
                if cpu_ready == 1 && cpu_presented == 0 { requested == 1 } else { 1 == 1 };
            invariant CpuPresentWasReady:
                if cpu_presented == 1 { cpu_ready == 1 } else { 1 == 1 };
        }
    }
}

/// RECOVERY REDRAW DELIVERY.
pub fn recovery_redraw_model() -> Model {
    aterm_spec::ty_model! {
        RecoveryRedraw {
            const Buggy = 0;
            var unresolved = 1;
            var stimulated = 0;
            var requested = 0;
            var suppressed = 0;
            var presented = 0;

            action Stimulus when (unresolved == 1) {
                stimulated = 1;
                requested = 1;
                suppressed = 0;
            }
            action BuggyStimulusAcknowledges when (
                Buggy == 1 && unresolved == 1
            ) {
                unresolved = 0;
                stimulated = 1;
                requested = 1;
                suppressed = 0;
            }
            action BuggyStimulusWithoutRedraw when (
                Buggy == 1 && unresolved == 1
            ) {
                stimulated = 1;
                requested = 0;
                suppressed = 0;
            }
            action Suppress when (requested == 1) {
                stimulated = 0;
                requested = 0;
                suppressed = 1;
            }
            action Present when (requested == 1) {
                unresolved = 0;
                stimulated = 0;
                requested = 0;
                suppressed = 0;
                presented = 1;
            }

            bound Bounds:
                unresolved <= 1 && stimulated <= 1 && requested <= 1 &&
                suppressed <= 1 && presented <= 1;
            invariant RecoveryStimulusRequestsRedraw:
                if stimulated == 1 { requested == 1 } else { 1 == 1 };
            invariant OnlyPresentAcknowledgesRecovery:
                if unresolved == 0 { presented == 1 } else { 1 == 1 };
        }
    }
}

/// Predictive echo visibility: the bounded display/expiry policy native and web hosts share.
pub fn predictive_echo_visibility_model() -> Model {
    aterm_spec::ty_model! {
        PredictiveEchoVisibility {
            const Buggy = 0;
            const Hide = 3;
            var app_owned = 0;
            var slow = 0;
            var confirmed = 0;
            var pending = 0;
            var visible = 0;
            var erased = 0;
            var fresh = 1;
            var fast_streak = 0;
            var retracted = 0;
            var owed = 0;
            var class = 0;
            var edit_proven = 0;
            var witness = 0;
            var learned = 0;
            var gate = 0;
            var first = 0;
            var witness_first = 0;

            action ConfirmFast when (app_owned == 0 && pending == 0 && (owed == 0 || Buggy == 1)) {
                fast_streak = if fast_streak <= Hide - 1 { fast_streak + 1 } else { Hide };
                slow = if Buggy == 1 { 0 }
                       else if Hide <= fast_streak + 1 { 0 }
                       else { slow };
                retracted = if Buggy == 1 { slow }
                            else if slow == 1 && Hide <= fast_streak + 1 { 1 }
                            else { 0 };
                fresh = 0;
                confirmed = 1;
                visible = 0;
                erased = 0;
            }
            action ConfirmFastInFlight when (
                app_owned == 0 && pending == 1 && (owed == 0 || Buggy == 1)
            ) {
                fast_streak = if fast_streak <= Hide - 1 { fast_streak + 1 } else { Hide };
                slow = if Buggy == 1 { 0 } else { slow };
                retracted = if Buggy == 1 { slow } else { 0 };
                visible = if Buggy == 1 { 0 } else { visible };
                erased = if Buggy == 1 { visible } else { 0 };
                fresh = 0;
                confirmed = 1;
            }
            action ConfirmSlow when (app_owned == 0 && pending == 0 && (owed == 0 || Buggy == 1)) {
                slow = 1;
                fast_streak = 0;
                retracted = 0;
                fresh = 0;
                confirmed = 1;
                visible = 0;
                erased = 0;
            }
            action Key when (pending == 0) {
                pending = if app_owned == 1 && Buggy == 0 { 0 } else { 1 };
                class = 0;
                first = gate;
                gate = 0;
                visible = if Buggy == 1 {
                    confirmed
                } else if app_owned == 0 && confirmed == 1 && slow == 1 {
                    1
                } else {
                    0
                };
                erased = 0;
                retracted = 0;
            }
            action KeyEdit when (pending == 0) {
                gate = 0;
                pending = if app_owned == 1 && Buggy == 0 { 0 } else { 1 };
                class = if app_owned == 1 && Buggy == 0 { 0 } else { 1 };
                visible = if Buggy == 1 {
                    confirmed
                } else if app_owned == 0 && confirmed == 1 && slow == 1 && edit_proven == 1 {
                    1
                } else {
                    0
                };
                erased = 0;
                retracted = 0;
            }
            action ConfirmEdit when (
                pending == 1 && class == 1 && app_owned == 0
                    && (owed == 0 || Buggy == 1) && (confirmed == 1 || Buggy == 1)
            ) {
                edit_proven = 1;
                pending = 0;
                class = 0;
                visible = 0;
                erased = 0;
                retracted = 0;
            }
            action Witness when (
                witness == 0 && confirmed == 1 && app_owned == 0 && (first == 1 || Buggy == 1)
            ) {
                witness = 1;
                witness_first = first;
            }
            action KeyLineEdge when (pending == 0 && witness == 1) {
                gate = 0;
                pending = if app_owned == 1 && Buggy == 0 { 0 } else { 1 };
                class = if app_owned == 1 && Buggy == 0 { 0 } else { 2 };
                visible = if Buggy == 1 {
                    confirmed
                } else if app_owned == 0 && confirmed == 1 && slow == 1 && learned == 1 {
                    1
                } else {
                    0
                };
                erased = 0;
                retracted = 0;
            }
            action ConfirmLineEdge when (
                pending == 1 && class == 2 && app_owned == 0
                    && (owed == 0 || Buggy == 1) && (confirmed == 1 || Buggy == 1)
            ) {
                learned = 1;
                pending = 0;
                class = 0;
                visible = 0;
                erased = 0;
                retracted = 0;
            }
            action Contradict when (pending == 1 && class == 2) {
                learned = 0;
                pending = 0;
                class = 0;
                erased = visible;
                visible = 0;
                retracted = 0;
                owed = 1;
                confirmed = 0;
                edit_proven = 0;
                witness = 0;
            }
            action Expire when (pending == 1) {
                pending = 0;
                erased = visible;
                visible = 0;
                retracted = 0;
                owed = 1;
                confirmed = if Buggy == 1 { confirmed } else { 0 };
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = if Buggy == 1 { witness } else { 0 };
            }
            action Echo when (pending == 1) {
                pending = 0;
                visible = 0;
                erased = 0;
                retracted = 0;
                class = 0;
            }
            action EnterComposer {
                app_owned = 1;
                pending = if Buggy == 1 { pending } else { 0 };
                visible = if Buggy == 1 { visible } else { 0 };
                confirmed = if Buggy == 1 { confirmed } else { 0 };
                class = if Buggy == 1 { class } else { 0 };
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = if Buggy == 1 { witness } else { 0 };
                erased = 0;
                retracted = 0;
            }
            action LeaveComposer {
                app_owned = 0;
                pending = 0;
                visible = 0;
                confirmed = 0;
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = 0;
                erased = 0;
                retracted = 0;
            }
            action Submit {
                pending = 0;
                visible = 0;
                confirmed = 0;
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = if Buggy == 1 { witness } else { 0 };
                gate = 0;
                first = 0;
                erased = 0;
                retracted = 0;
                owed = 1;
            }
            action SubmitAtPrompt {
                pending = 0;
                visible = 0;
                confirmed = 0;
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = if Buggy == 1 { witness } else { 0 };
                gate = 1;
                first = 0;
                erased = 0;
                retracted = 0;
                owed = 1;
            }
            action RowsMoved {
                pending = 0;
                visible = 0;
                confirmed = 0;
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                witness = if Buggy == 1 { witness } else { 0 };
                erased = 0;
                retracted = 0;
                owed = 1;
            }
            action OwedExpire when (owed == 1) {
                owed = 0;
                retracted = 0;
            }
            action SwitchSession {
                app_owned = 0;
                slow = if Buggy == 1 { slow } else { 0 };
                fresh = 1;
                confirmed = 0;
                pending = 0;
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                learned = if Buggy == 1 { learned } else { 0 };
                witness = 0;
                gate = 0;
                first = 0;
                visible = 0;
                erased = 0;
                fast_streak = 0;
                retracted = 0;
                owed = 0;
            }

            bound Bounds:
                app_owned <= 1 && slow <= 1 && confirmed <= 1 &&
                pending <= 1 && visible <= 1 && erased <= 1 && fresh <= 1 &&
                fast_streak <= Hide && retracted <= 1 && owed <= 1 && class <= 2 &&
                edit_proven <= 1 && witness <= 1 && learned <= 1 && gate <= 1 && first <= 1 &&
                witness_first <= 1;
            invariant AppOwnedHasNoPrediction:
                if app_owned == 1 { pending == 0 && visible == 0 } else { 1 == 1 };
            invariant FastAdaptiveNeverPaints:
                if slow == 0 { visible == 0 } else { 1 == 1 };
            invariant InvisibleExpiryCannotErase:
                if slow == 0 || app_owned == 1 { erased == 0 } else { 1 == 1 };
            invariant VisibleNeedsProofAndBenefit:
                if visible == 1 {
                    app_owned == 0 && confirmed == 1 && slow == 1 && pending == 1
                } else {
                    1 == 1
                };
            invariant FreshSessionHasNoInheritedRtt:
                if fresh == 1 { slow == 0 } else { 1 == 1 };
            invariant RetractOnlyOnSustainedFastEvidence:
                if retracted == 1 { Hide <= fast_streak && pending == 0 } else { 1 == 1 };
            invariant ProofIsAttributable:
                if owed == 1 { confirmed == 0 } else { 1 == 1 };
            invariant EditVisibleNeedsClassProof:
                if visible == 1 {
                    if class == 1 { edit_proven == 1 } else { 1 == 1 }
                } else {
                    1 == 1
                };
            invariant ClassProofRidesOnGlyphProof:
                if edit_proven == 1 { confirmed == 1 } else { 1 == 1 };
            invariant LineEdgeNeedsWitnessAndBinding:
                if visible == 1 {
                    if class == 2 { learned == 1 && witness == 1 } else { 1 == 1 }
                } else {
                    1 == 1
                };
            invariant FreshContextHasNoBinding:
                if fresh == 1 { learned == 0 } else { 1 == 1 };
            invariant WitnessIsTheFirstKey:
                if witness == 1 { witness_first == 1 } else { 1 == 1 };
        }
    }
}

/// NETWORK LISTENER SLOT DISPLACEMENT.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn net_slot_displacement_model() -> Model {
    aterm_spec::ty_model! {
        NetSlotDisplacement {
            const Buggy = 0;
            const Cap = 2;
            const Arrivals = 4;
            var pending = 0;
            var authed = 0;
            var cut = 0;
            var waiting = 0;
            var arrivals = 0;
            var claims = 0;
            var authed_cut = 0;
            action Accept when (
                arrivals <= Arrivals - 1 &&
                pending + authed + cut <= if Buggy == 1 { Cap } else { Cap - 1 }
            ) {
                pending = pending + 1;
                arrivals = arrivals + 1;
                waiting = 0;
            }
            action Authenticate when (
                pending > 0 || (Buggy == 1 && cut > 0 && claims + pending == arrivals)
            ) {
                pending = if pending > 0 { pending - 1 } else { pending };
                cut = if pending > 0 { cut } else { cut - 1 };
                authed = authed + 1;
                claims = claims + 1;
            }
            action Deadline when (pending > 0 || (Buggy == 1 && authed > 0 && authed_cut == 0)) {
                pending = if pending > 0 { pending - 1 } else { pending };
                cut = if pending > 0 { cut + 1 } else { cut };
                claims = if pending > 0 { claims + 1 } else { claims };
                authed_cut = if pending > 0 { authed_cut } else { 1 };
            }
            action Evict when (
                waiting == 0 && Cap <= pending + authed + cut &&
                (pending > 0 || (Buggy == 1 && authed > 0 && claims + pending == arrivals))
            ) {
                pending = if pending > 0 { pending - 1 } else { pending };
                authed = if pending > 0 { authed } else { authed - 1 };
                authed_cut = if pending > 0 { authed_cut } else { 1 };
                cut = cut + 1;
                claims = claims + 1;
                waiting = 1;
            }
            action Release when (cut > 0) {
                cut = cut - 1;
            }
            action Close when (authed > 0) {
                authed = authed - 1;
            }
            action Refuse when (waiting == 1) {
                waiting = 0;
            }
            invariant InFlightBounded: pending + authed + cut <= Cap;
            invariant ClaimExactlyOnce: claims + pending == arrivals;
            invariant AuthedNeverCut: authed_cut == 0;
        }
    }
}

/// Alt-screen archive pool, charged against the process.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_archive_pool_model() -> Model {
    aterm_spec::ty_model! {
        AltArchivePool {
            const Buggy = 0;
            const Local = 0;
            const Instances = 2;
            const Total = 4;
            const Half = 2;
            var a = 0;
            var b = 0;
            var steps = 0;

            action CommitA when (steps <= 9) {
                steps = steps + 1;
                a = if (Local == 1 || Buggy == 2) {
                        (if a + 1 > Total { Total } else { a + 1 })
                    } else {
                        (if (if Buggy == 1 { if Instances > 1 { 1 } else { 0 } }
                             else { if b > 0 { 1 } else { 0 } }) > 0
                         { (if a + 1 > Half { Half } else { a + 1 }) }
                         else { (if a + 1 > Total { Total } else { a + 1 }) })
                    };
            }

            action CommitB when (steps <= 9 && Instances > 1) {
                steps = steps + 1;
                b = if (Local == 1 || Buggy == 2) {
                        (if b + 1 > Total { Total } else { b + 1 })
                    } else {
                        (if (if Buggy == 1 { 1 }
                             else { if a > 0 { 1 } else { 0 } }) > 0
                         { (if b + 1 > Half { Half } else { b + 1 }) }
                         else { (if b + 1 > Total { Total } else { b + 1 }) })
                    };
            }

            action WipeB when (steps <= 9 && Instances > 1) {
                steps = steps + 1;
                b = 0;
            }

            invariant PoolBounded:
                (a > (if (Local == 1 || Buggy == 2) { Total }
                      else { if (if Buggy == 1 { if Instances > 1 { 1 } else { 0 } }
                                 else { if b > 0 { 1 } else { 0 } }) > 0
                             { Half } else { Total } }))
                || (b > (if (Local == 1 || Buggy == 2) { Total }
                         else { if (if Buggy == 1 { 1 }
                                    else { if a > 0 { 1 } else { 0 } }) > 0
                                { Half } else { Total } }))
                || (a + b <= Total);

            invariant IdleTakesNothing:
                (b > 0)
                || (Total <= (if (Local == 1 || Buggy == 2) { Total }
                              else { if (if Buggy == 1 { if Instances > 1 { 1 } else { 0 } }
                                         else { if b > 0 { 1 } else { 0 } }) > 0
                                     { Half } else { Total } }));
        }
    }
}

/// Wearing a cat is a join, so two instances converge and neither loses its pick.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_pin_merge_model() -> Model {
    aterm_spec::ty_model! {
        KittyPinMerge {
            const Buggy = 0;
            var a1 = 0;
            var a2 = 0;
            var b1 = 0;
            var b2 = 0;
            var b1_hi = 0;
            var b2_hi = 0;
            var ab = 0;
            var clock = 0;
            var steps = 0;

            action WearOneOnA when (steps <= 5 && clock <= 2) {
                steps = steps + 1;
                clock = clock + 1;
                a1 = clock + 1;
                ab = 0;
            }

            action WearTwoOnB when (steps <= 5 && clock <= 2) {
                steps = steps + 1;
                clock = clock + 1;
                b2 = clock + 1;
                b2_hi = if b2 > b2_hi { b2 } else { b2_hi };
                ab = 0;
            }

            action FlushAToB when (steps <= 5) {
                steps = steps + 1;
                b1 = if Buggy == 1 { a1 } else { (if a1 > b1 { a1 } else { b1 }) };
                b2 = if Buggy == 1 { a2 } else { (if a2 > b2 { a2 } else { b2 }) };
                b1_hi = if b1 > b1_hi { b1 } else { b1_hi };
                b2_hi = if b2 > b2_hi { b2 } else { b2_hi };
                ab = 1;
            }

            action FlushBToA when (steps <= 5 && ab == 1) {
                steps = steps + 1;
                a1 = if Buggy == 1 { b1 } else { (if b1 > a1 { b1 } else { a1 }) };
                a2 = if Buggy == 1 { b2 } else { (if b2 > a2 { b2 } else { a2 }) };
            }

            invariant AMergeNeverLosesAPick: b1_hi <= b1 && b2_hi <= b2;
        }
    }
}
