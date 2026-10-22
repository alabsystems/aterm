// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Terminal-core, presentation-loop, and capability-plumbing models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor is re-exported from this crate's root).

use aterm_spec::derive::*;

/// Terminal modes: the DEC/ANSI mode-flag state machine the VT handler maintains, and the
/// reset discipline over it. The 28 actions are exactly the
/// `#[refines(machine = "terminal_modes", action = …)]` set in
/// `aterm-core/src/terminal/handler_{dec,dec_refinement,esc,state,report}.rs`: thirteen
/// `Set*`/`Reset*` toggle pairs and the two resets (`SoftReset` = DECSTR, `FullReset` = RIS).
/// Unmodeled DEC modes (VT52, 132-column, reverse video, BiDi, …) are
/// `#[spec_unmodeled(reason = …)]` waivers.
///
/// Every variable is a boolean fact about a mode and every action sets its own variable
/// exactly, so the projection from `TerminalModes` is exact. Three abstract a multi-valued
/// mode: `mouse_mode` is "some mouse tracking is on" (X10/1000/1002/1003), `sgr_mouse` is
/// "SGR 1006 encoding is in force", and `cursor_style` is "the shape differs from the host's
/// default" (DECSCUSR and mode 12 are sites of both its actions, the bytes deciding which).
/// Both resets restore the host-configured shape, not `CursorStyle::default`.
///
/// `ResetRestoresDefaults`: right after either reset (`reset = 1`, a history variable every
/// other action clears) the cursor is shown in the host shape, autowrap is on, and insert,
/// origin and newline modes, application cursor keys, mouse capture, SGR encoding, focus and
/// bracketed-paste reports and the synchronized-output hold are off. `alt_screen` is outside
/// it: DECSTR keeps the current screen, RIS leaves the alternate one. `Buggy = 1` is a DECSTR
/// that forgets the synchronized-output hold, leaving the host's present-hold armed and the
/// screen frozen.
///
/// Tier 1: `aterm-core/src/terminal/terminal_modes_conformance.rs` drives every anchored
/// action through `Terminal::process`, projects with `project_modes`, validates each step
/// against this model, and checks that each step entered the action's own anchor.
// Skip: a spec-model data constructor whose nested literals exceed the VC-generation budget
// (its obligations stay Unknown, fail-closed); the model it returns is what `ty` checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn terminal_modes_model() -> Model {
    // (Set action, Reset action, mode) for every boolean toggle pair.
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
    // Every mode a reset restores, with its default (alt_screen is RIS-only).
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
    // Any non-reset action ends the "just reset" window.
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
/// ([`damage_to_present_model`], [`wake_heal_evidence_model`]) held to three laws, each with the
/// mutant that breaks it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WakeLatchMutant {
    /// A one-shot latch: no heal, no watchdog deadline and no re-post, so a lost wake starves
    /// its session's presents.
    OneShot,
    /// An age-only heal: a latch older than `Expiry` is presumed lost even while its event is
    /// still queued behind a long frame (a false heal).
    AgeOnly,
}

/// Damage→present bounded response: the abstract twin of the `Wake::Output` delivery spine,
/// with the event loop's turn explicit so a long frame is representable. PTY damage arrives
/// (`Damage*`) and the coalescing latch (`spawn::gated_output_wake`) posts at most one wake
/// (`inflight`). A turn drains the queued user events (`TurnBegin`: a wake in flight is
/// handled, clearing the latch and presenting the damage), may stay busy for any number of
/// ticks, and ends in `about_to_wait` (`TurnEnd`), where the lost-wake watchdog judges the
/// latch, marks an armed one `seen` and folds a `recheck` deadline for it. A wake can be lost
/// without a handler pass (`Lose`, bounded by `LostBudget`). Time (`Tick`) runs freely through
/// a busy turn; an idle loop turns at once for a queued wake and at a due recheck. The loop's
/// first turn (winit's `Init`) is running at the initial state.
///
/// A wake posted while the loop is idle and lost with its wake-up is seen by no `TurnEnd`, so
/// no recheck exists for it: it is stranded. The reader re-posts it (`DamageRepost`,
/// `spawn::wake_stranded`) at the first damage after its expiry, and never re-sends a wake a
/// turn has passed or one that may still be queued (`DamageCoalesced`).
///
/// `DamageBounded`: damage waits at most `Expiry + 1` idle ticks for its present. `waited`
/// counts only idle ticks (a long frame is the main thread's own latency) and not a stranded
/// wake's, which end at the session's next output after the expiry (if the program falls
/// silent, at the loop's next unrelated turn, a bound this model does not claim).
/// `StrandedReposted`: no damage arriving on a stranded wake past its expiry is swallowed
/// without a re-post.
///
/// The watchdog heals on evidence (`spawn::judge_output_wake`): the latch is still armed and a
/// previous `TurnEnd` saw it (`seen`; in the code, its sent stamp is older than the previous
/// pass's), so the drain since must have delivered it; the recheck it folds is one expiry
/// after that pass. The bound is tight (the worst case reaches `Expiry + 1`). Tier 1:
/// `spawn::latch_conforms_to_damage_to_present_model` and
/// `spawn::watchdog_conforms_to_damage_to_present_model`. `Buggy = 1` is the one-shot latch (no
/// heal, recheck or re-post): Damage → Lose → TurnEnd → Tick* passes the bound.
///
/// Below the model's grain: the model posts atomically with the arm, while the code arms,
/// posts, then re-stamps the latch sent with a clock read after the post. An unsent arm is
/// never judged lost nor re-posted (`an_unsent_arm_is_never_presumed_lost`), which makes `seen`
/// a property of the code's stamps.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn damage_to_present_model() -> Model {
    wake_latch_model(WakeLatchMutant::OneShot)
}

/// No false heal: the [`damage_to_present_model`] machine held to the other half of the
/// contract, that the watchdog never heals a wake that is merely late (still queued behind the
/// main thread's turn). A false heal answers with a redraw of every window and a duplicate wake
/// on a main thread already behind. `falseHeal` latches a heal fired while its wake was
/// `inflight`. `NoFalseHeal` holds under the evidence rule (a `TurnEnd` saw the arm, so the
/// drain since delivered anything in flight); `Buggy = 1` is the age-only heal (damage in the
/// first turn, a busy frame of `Expiry + 1` ticks, `TurnEnd`).
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
    // The committed heal: armed, and a previous turn's `about_to_wait` saw it.
    let proven_lost = and_(armed.clone(), eq(var("seen"), int(1)));
    // The age-only heal: armed, and older than the expiry.
    let aged = gt(sub(var("now"), var("latch")), cst("Expiry"));
    // A stranded wake past its expiry: armed, posted while the loop was idle
    // with no `TurnEnd` since (unseen, still idle), older than the expiry —
    // what the reader re-posts (`spawn::wake_stranded`).
    let stranded_due = and_(
        armed.clone(),
        and_(idle.clone(), and_(eq(var("seen"), int(0)), aged.clone())),
    );
    // Its complement on an armed latch: busy, or seen, or within the expiry.
    let not_stranded_due = or_(
        eq(var("busy"), int(1)),
        or_(
            eq(var("seen"), int(1)),
            le(sub(var("now"), var("latch")), cst("Expiry")),
        ),
    );
    // What each mutant keeps of the committed rules.
    let (heal, recheck_on, repost_guard, coalesce_guard) = match mutant {
        // Buggy=1: no heal, no watchdog deadline, no re-post.
        WakeLatchMutant::OneShot => (
            and_(fixed.clone(), proven_lost),
            fixed.clone(),
            and_(fixed, stranded_due.clone()),
            and_(armed.clone(), or_(mutated.clone(), not_stranded_due)),
        ),
        // Buggy=1: heal on age.
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
    // Damage arriving on pending damage does not restart `waited`: the oldest's clock runs.
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
    // A stranded wake: armed, unseen, not in flight, the loop idle.
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
                    // Pending damage waits at most Expiry+1 idle ticks.
                    name: "DamageBounded",
                    kind: InvariantKind::Claim,
                    expr: le(var("waited"), add(cst("Expiry"), int(1))),
                },
                Invariant {
                    // A stranded wake never swallows damage past its expiry.
                    name: "StrandedReposted",
                    kind: InvariantKind::Claim,
                    expr: eq(var("missedRepost"), int(0)),
                },
            ],
        ),
        WakeLatchMutant::AgeOnly => (
            "WakeHealOnlyWhenLost",
            vec![Invariant {
                // No heal ever fires on a wake still in the queue.
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
            // The loop's first turn (`Init`) is running.
            StateVar {
                name: "busy",
                init: 1,
            },
            var0("seen"),
            // The last `TurnEnd`'s instant: the code's pass stamp, which is also
            // what the readers see of an idle loop (`spawn::LoopIdle`).
            var0("drained"),
            var0("recheck"),
            var0("falseHeal"),
            var0("missedRepost"),
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                // First damage of an episode: arm the latch, post the wake.
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
                // Damage on an armed latch coalesces: a wake a turn has passed is the
                // watchdog's, and one possibly queued behind a long frame must not be
                // duplicated, unless it is stranded past its expiry. A swallowed stranded one
                // is booked in `missedRepost`.
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
                // A stranded wake past its expiry is re-posted by the reader
                // (a fresh post from the stranded arm; not a heal).
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
                // The in-flight wake is dropped without a handler pass, while the loop runs
                // or while it sleeps with its wake-up. The latch stays armed.
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
                // A turn begins (whenever idle: any event may turn the loop) and drains the
                // queued user events: a wake in flight is handled and its damage presented.
                // The recheck is consumed; the turn's `TurnEnd` folds a new one.
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
                // `about_to_wait`: the watchdog heals (clear + repaint every window) when the
                // heal predicate holds; otherwise an armed latch is now seen and folds a
                // recheck one expiry out (`now + Expiry + 1` ticks: the code's `now + expiry`
                // ns, never in the past).
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
                // Time passes: freely through a busy turn (a long frame); an idle
                // loop turns at once for a queued wake and at a due recheck.
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

/// Observation kernel, earliest-armed idle deadline: the abstract twin of
/// `WatcherSet::next_deadline`. The host arms one `ControlFlow::WaitUntil`, which must equal
/// the minimum of all pending `IdleFor` deadlines so an earlier one is never missed.
/// Projection `<<armed, minp>>` over two deadlines (near = 1, far = 2; 4 is unset):
/// `ArmNear`/`ArmFar` register one, and `armed` must track `minp`. `Buggy = 1` keeps the
/// first deadline armed, so far-then-near leaves `armed = 2` while `minp = 1`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn idle_deadline_model() -> Model {
    // min(a, b) == IF a > b THEN b ELSE a ; keep-first(a, v) == IF a = Unset THEN v ELSE a.
    let arm = |v: i64| -> Expr {
        if_(
            eq(cst("Buggy"), int(0)),
            if_(gt(var("armed"), int(v)), int(v), var("armed")), // min(armed, v)
            if_(eq(var("armed"), int(4)), int(v), var("armed")), // keep-first
        )
    };
    Model {
        name: "IdleDeadline",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "armed",
                init: 4,
            }, // 4 == unset sentinel (no pending deadline)
            StateVar {
                name: "minp",
                init: 4,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "ArmNear", // register the nearer deadline (value 1)
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
                name: "ArmFar", // register the farther deadline (value 2)
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
            expr: eq(var("armed"), var("minp")), // armed == min of pending deadlines
        }],
    }
}

/// Surface coverage along one axis (applied to x and y it covers all four bands): after font
/// zoom a raw window surface may exceed the integer-cell renderer frame, and every present must
/// cover the whole destination, filling the remainder with the live terminal background.
/// `Buggy = 1` is a frame-sized viewport: only `frame` units covered, the band left stale.
/// Tier 1 (`aterm-gpu/tests/present_real.rs`) drives the GPU present seam at an odd +7px
/// destination and projects its pixels onto this model.
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

/// First-present phase publication: a valid partition cannot publish before all eight
/// intervals exist. Each `Step` is "one more interval"; timestamp identity, order and duration
/// arithmetic are the GUI derivation's, checked by its Tier-1 test with reordered-timestamp
/// negative controls. `Buggy = 1` admits an early publication.
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

/// Present retry: bounded, future-only autonomous recovery after a dropped surface
/// transaction. `remaining` is the retry fuel for the current stimulus episode; `train` counts
/// autonomous wakes since the last stimulus or present; `retry = 1` is one strictly-future
/// deadline (`2` is the host test's negative-control value); `ready = 1` lets a redraw attempt
/// run; `parked = 1` waits for a new external stimulus; `bootstrap_available = 1` is the one
/// fresh-window permit to recover an initial `GpuOccluded` result.
///
/// `DropBootstrapOccluded` fires only from the pristine retry state and consumes its one-way
/// permit (neither stimuli nor a present replenish it). Every `Wake` spends one unit of fuel;
/// once it is gone a `Drop` parks with no deadline. `Stimulus` starts a fresh episode only while
/// recovery is unresolved; `ForcedStimulus` is a real resize/expose that requests a frame even
/// from idle; `SynchronousStimulus` is the capture/internal twin, whose caller presents in the
/// same main-thread operation, so it resets retry state without a new OS redraw request
/// (keeping one already outstanding). An ordinary idle input stimulus is no transition, as in
/// the code. `Buggy = 1`: wakes spend no fuel, so the Drop→Wake train passes `RetryCap`.
pub fn present_retry_model() -> Model {
    aterm_spec::ty_model! {
        PresentRetry {
            const Buggy = 0;
            const RetryCap = 5;
            var remaining = 5;
            var train = 0;
            var retry = 0;  // 0 none, 1 strictly future; 2 is the host negative control
            var ready = 1;  // one external/timeout-delivered present attempt
            var parked = 0;
            var outstanding = 0; // requested redraw not yet present/drop acknowledged
            var bootstrap_available = 1; // one pre-first-present GpuOccluded retry

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
            // Dial 2 arms the deadline in the past (the host's negative-control value).
            invariant RetryDeadlineIsStrictlyFuture: retry <= 1;
            invariant AutonomousTrainBound: train <= RetryCap;
            // Dial 3 parks with a deadline still armed.
            invariant ParkedHasNoDeadline:
                if parked == 1 { retry == 0 } else { 1 == 1 };
            // Dial 4 strands an exhausted attempt: no deadline and not parked.
            invariant ExhaustedAttemptParks:
                if remaining == 0 && ready == 0 { parked == 1 } else { 1 == 1 };
            // Dial 5 keeps a dropped redraw outstanding.
            invariant OutstandingRedrawHasOpenGate:
                if outstanding == 1 { ready == 1 && retry == 0 && parked == 0 }
                else { 1 == 1 };
        }
    }
}

/// GPU device-loss routing: a failed surface acquire on a latched dead device bypasses
/// transient retry and enters CPU fallback at once; retrying a dead GPU only burns the bounded
/// retry train and leaves the last frame parked. `route = 1` is surface retry, `route = 2`
/// GPU→CPU recovery; `Buggy = 1` routes the loss back into retry, breaking `LostUsesFallback`.
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

/// GPU DEVICE-LOSS RECOVERY — bounded safety for the whole host recovery
/// transaction, including the less-obvious notification-after-success path. A
/// lost device ends its recording; the first CPU-builder failure after a
/// successful present owns a typed, strictly-future retry; and exactly one
/// dropped-frame ledger entry remains. A source surface error may already own
/// that count, so recovery updates its disposition rather than incrementing it.
///
/// `Wake` then exposes the conditional recovery continuation: if the external
/// CPU builder/surface succeeds, `PresentCpu` completes it. This is deliberately
/// NOT an unconditional eventual-presentation claim—the environment may keep a
/// renderer unavailable. A failure entered after an already exhausted surface
/// retry train is allowed to park until the separately-modelled external
/// `PresentRetry::Stimulus` transition.
///
/// Dead actions, not `Buggy` arms in the entry actions: an inline arm slips on every firing, so
/// at `Buggy = 1` the correct fresh retry would never exist and the wake-side slips could fire
/// only after the entry slip broke a law. There is no "a CPU present completes the failure"
/// law: `fallback_failed` is this model's own record (nothing in aterm-gui projects onto it),
/// cleared only by the `Succeed*` entries and `BuildCpuAfterWake`, the steps that set the
/// `requested` every `PresentCpu` needs, so no modelled present lands over a live record and
/// such a law would judge only the model's bookkeeping. The shipping reset is
/// `PresentRetry::on_presented`, and
/// `successful_gpu_recovery_classifies_source_ready_only_after_redraw_request` asserts it leaves
/// the default retry state after the acknowledging present.
pub fn gpu_loss_recovery_model() -> Model {
    aterm_spec::ty_model! {
        GpuLossRecovery {
            const Buggy = 0;
            var path = 0;       // 0 idle, 1 loss after present, 2 loss after drop
            var fallback_failed = 0;
            var retry = 0;      // typed, strictly-future CPU recovery retry
            var delivered = 0;  // the retry wake was consumed
            var exhausted = 0;  // prior retry fuel was already empty
            var parked = 0;
            var cpu_ready = 0;
            var requested = 0;  // first CPU redraw is outstanding
            var cpu_presented = 0;
            var drops = 0;      // dropped-frame count for this host transaction
            var reason = 0;     // 0 none, 1 source surface, 2 CPU fallback
            var recording = 1;  // an in-flight GPU recording

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

/// RECOVERY REDRAW DELIVERY — an unresolved presentation-retry episode stays
/// unresolved while redraw requests are delivered or suppressed; only an
/// actual present acknowledges it. Merely reopening the gate once is not
/// sufficient: winit may suppress multiple requests after the host consumes a
/// deadline, including the first CPU-fallback redraw.
///
/// Dead actions, not a `Buggy` arm in `Stimulus`: one arm can make only one of the slips, and a
/// stimulus that both acknowledges and requests nothing leaves no suppressed edge to catch the
/// first on. "A suppressed request leaves the episode unresolved" needs no law of its own: a
/// present clears suppression, so reaching it over a resolved episode takes an acknowledgement
/// without a present, which `OnlyPresentAcknowledgesRecovery` refuses.
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
/// `app_owned` is an application-owned input composer (Kitty `REPORT_EVENT_TYPES`), `slow` that
/// Adaptive prediction measured a useful RTT, `confirmed` the current line's echo proof;
/// `pending` is predictor state, `visible` and `erased` the pixel events a user sees.
///
/// `Submit` is every epoch boundary, not only Enter: a line terminator in any spelling
/// (`Predictor::note_line_submit`), any other press the host could not predict
/// (`note_unmodelled_input`), a Backspace with nothing of the predictor's own to cancel, a
/// declined glyph, the terminal moving rows under the cursor (the `coords` generation), and the
/// program moving the cursor off the cell the last confirmed echo left it in. `aterm-predict`'s
/// `every_epoch_boundary_is_the_models_submit` binds all six to this action. (The code keeps a
/// proven line's in-flight pixels across a lone terminator until echo or expiry, which the
/// model's single-guess `pending` does not separate.)
///
/// At `Buggy = 0` an app-owned composer arms nothing, a fast Adaptive link may track a guess but
/// never paints it (so expiry is invisible), and a session switch starts with no inherited RTT
/// estimate (`Predictor::reset_session`; `predict_session_reset` on the web hosts). `Buggy = 1`
/// paints on confirmation alone, ignores the composer gate and keeps the RTT across sessions:
/// the fast-link blink, the app-owned ghost and the RTT leak.
///
/// The gate is a latch with hysteresis: `ConfirmFast` closes it only after `Hide` decisive fast
/// turns with nothing pending (the code also wants the smoothed RTT to agree). `fast_streak`
/// counts those turns (capped at `Hide`); `retracted` marks the step that closed the gate, so
/// `RetractOnlyOnSustainedFastEvidence` makes a single-sample close, or a close with pixels in
/// flight, a counterexample. `ConfirmFastInFlight` is a fast turn with type-ahead still on
/// glass: it may advance the streak but never close, which keeps that clause non-vacuous.
///
/// ATTRIBUTION (`owed`). Every boundary but one owes an echo the predictor no
/// longer tracks: the terminator's own, the unmodelled key's (a key refused at
/// the no-echo gate included), a declined glyph's, an erase's, the keystrokes
/// behind guesses a flush retired (the program moving the cursor owes none, and
/// the model over-approximates by owing at every `Submit`). Until that echo's
/// window passes (`OwedExpire`), a glyph matching a guess typed after it may BE
/// it: the guess's pixels retire (`Echo`), and that guess's own echo is owed in
/// turn (the implementation extends the window; the model's latch simply stays
/// up, and `OwedExpire` may fire at any step, which over-approximates), but no
/// turn may prove the epoch or vote on the link (`Confirm*` wait for
/// `owed == 0`). A PTY carries no `caused_by`, so this bound is what makes
/// positional proof exact. `ProofIsAttributable` refuses a proof standing while
/// an echo is owed; at `Buggy=1` a confirm turn takes the owed echo for the
/// guess's own (the deleted rule, under which `n`, `n`, ` ` proved the epoch one
/// cell behind and painted the space where the second `n` echoed). `owed` is the
/// latch from the owing step to the end of the window as it stood then;
/// `aterm-predict`'s `an_owed_echo_proves_nothing_until_its_window_passes` binds
/// it to the code. The implementation asks each guess only about echoes owed
/// from BEFORE it, so a proven line's tail, carried across its own terminator,
/// still votes on the link while that terminator's echo is owed; the model drops
/// `pending` at `Submit` and has no such turn, which the single-guess
/// abstraction does not separate.
///
/// EXPIRY OWES (`Expire`). An expired guess's key was sent all the same, and its
/// echo, if it comes, lands where the real cursor stands: on the cell the proof
/// names, ahead of anything typed next. So `Expire` owes it and clears
/// `confirmed`; at `Buggy=1` it keeps the proof (the expiry that only popped the
/// guess), which `ProofIsAttributable` refutes. Typed owes (a guess retired
/// without credit owes only its own glyph, `aterm-predict`'s `glyph_owes`) are
/// abstracted away: the model has no glyph identity, so they project to
/// `owed = 0`, and the safety theorem's directed cases pin them instead.
///
/// THE EDIT CLASS (`class`, `edit_proven`). An edit key (`←`, `→`) is armed as an
/// op of a state chain whose class needs its own proof: a motion displays only
/// once a motion of the same epoch has echoed exactly, on top of the glyph proof
/// (`aterm-predict`'s `proven`, the class ladder: a cooked tty echoes `^[[D`, which
/// only that proof tells apart). `KeyEdit` arms one (`class = 1`, pending like any
/// guess, so `ConfirmFast` cannot fire under it); `ConfirmEdit` is its credited
/// echo, which needs the glyph proof and no owed echo, and proves the class. Every
/// action that ends the glyph proof ends the class proof with it (a cooked `cat`
/// an Enter starts is a different program behind the same line). At `Buggy=1` an
/// edit key displays on the glyph proof alone and the class proof outlives
/// `Submit`: `EditVisibleNeedsClassProof` and `ClassProofRidesOnGlyphProof` refute
/// each. `aterm-predict`'s `an_edit_displays_only_after_its_class_is_proven` binds
/// it to the code.
///
/// THE LINE EDGE (`class = 2`, `witness`, `learned`). Home, End, Ctrl-A, Ctrl-E and
/// Delete are placed only on a WITNESSED line, one the predictor started at the
/// cell the shell's authenticated OSC 133 `B` names as its buffer's start
/// (`Witness`, which needs the glyph proof that started it), and shown only once
/// the key's binding is LEARNED in the editor context (`ConfirmLineEdge`, the
/// credited echo that teaches it: the code needs two, which the model folds into
/// one step as `ConfirmSlow` folds two slow samples). Until then `KeyLineEdge` is
/// a probe, pending and dark. `Contradict` is a divergence or an expiry the op
/// heads: the binding is unlearned and the chain flushed, owed, the proof void.
/// The witness goes with every boundary (a line ends with its epoch), and the
/// binding with the session: a session nothing is known about has learned
/// nothing (`FreshContextHasNoBinding`). At `Buggy=1` a line-edge key displays on
/// the glyph proof alone and a switch keeps the binding:
/// `LineEdgeNeedsWitnessAndBinding` and `FreshContextHasNoBinding` refute each.
/// `aterm-predict`'s `a_line_edge_key_displays_only_when_learned_at_a_witnessed_prompt`
/// binds it to the code.
///
/// THE FIRST KEY (`gate`, `first`, `witness_first`). The `B` vouches for the buffer
/// it found empty, which only the first key typed after it meets: a line is
/// witnessed only where the run that started it was the first key typed since a
/// line terminator pressed at an authenticated prompt (`SubmitAtPrompt`, the
/// code's `note_line_submit_at` with a prompt, opens the gate; every key, and every
/// other boundary but one, shuts it). The one is `RowsMoved`, the rows moving
/// under the screen (a scroll, the code's `observe_coordinates`): it flushes and
/// owes as `Submit` does, and lets no key past, so it keeps the gate as it stood.
/// `Key` records whether it was that first key
/// (`first`), and `Witness` needs it. At `Buggy=1` any run is witnessed:
/// `WitnessIsTheFirstKey` refutes it. The same conformance test binds it, with
/// `Rule::WitnessFirstKey` off as its negative control.
pub fn predictive_echo_visibility_model() -> Model {
    aterm_spec::ty_model! {
        PredictiveEchoVisibility {
            const Buggy = 0;
            // `FAST_SAMPLES_TO_HIDE`. Opening takes fewer samples (closing erases pixels),
            // which `ConfirmSlow` folds into one step.
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
            // 1: the pending op is an edit key of a class that needs its own proof
            // (a motion); 0: a glyph, or nothing pending.
            var class = 0;
            // That class has a credited echo this epoch.
            var edit_proven = 0;
            // The known line was started at the shell's authenticated buffer start.
            var witness = 0;
            // The line-edge key's binding is learned in this editor context.
            var learned = 0;
            // THE FIRST KEY's gate: no key since a line terminator pressed at an
            // authenticated prompt.
            var gate = 0;
            // The newest key was the first typed at its prompt.
            var first = 0;
            // The witnessed line was started by the first key typed at its prompt.
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
                // The first key typed at its prompt, if the gate was open; a key shuts it.
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
            // An edit key (a motion) armed as an op of a state chain: pending like
            // any guess, displayed only on the glyph proof, the benefit AND the
            // class proof (`Buggy`: on the glyph proof alone).
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
            // Its credited echo proves the class: only on the glyph proof, and only
            // with no owed echo that could be the screen's state instead (`Buggy`
            // waives both).
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
            // The glyph proof started a line at the shell's authenticated buffer
            // start: a witnessed line, where a line-edge key may be placed. Only a
            // run the prompt's first key started meets the buffer the `B` found
            // empty (`Buggy`: any run).
            action Witness when (
                witness == 0 && confirmed == 1 && app_owned == 0 && (first == 1 || Buggy == 1)
            ) {
                witness = 1;
                witness_first = first;
            }
            // A line-edge key (Home, End, Ctrl-A, Ctrl-E, Delete) on the witnessed
            // line: pending like any op, displayed only on the glyph proof, the
            // benefit, the witness AND a learned binding (`Buggy`: on the glyph
            // proof alone). Unlearned, it is a dark probe.
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
            // Its credited echo, the key's canonical effect, teaches the binding:
            // only on the glyph proof with no owed echo that could be the screen's
            // state instead, in a session that has measured its link.
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
            // A divergence or an expiry the line-edge op heads: the binding is
            // unlearned, and the chain flushed: owed, the proof and the line gone.
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
            // The expired guess's key was sent: its echo is owed, and it lands on
            // the proven cell when it comes, so the proof goes with the guess.
            // `Buggy` keeps the proof (the expiry that popped and nothing else).
            action Expire when (pending == 1) {
                pending = 0;
                erased = visible;
                visible = 0;
                retracted = 0;
                owed = 1;
                confirmed = if Buggy == 1 { confirmed } else { 0 };
                class = 0;
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                // The proof is void, and with it the line it witnessed.
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
                // The class proof is the epoch's: `Buggy` carries it into the next
                // line (a motion proven at the shell, shown in the `cat` Enter starts).
                edit_proven = if Buggy == 1 { edit_proven } else { 0 };
                // …and the witness is the line's, which ends with its epoch.
                witness = if Buggy == 1 { witness } else { 0 };
                // A boundary that is no line terminator at a prompt (a key not
                // modelled, a terminator where none was in front) shuts the gate.
                gate = 0;
                first = 0;
                erased = 0;
                retracted = 0;
                owed = 1;
            }
            // `Submit` by a line terminator pressed at an authenticated prompt
            // (`note_line_submit_at` with a prompt): the next key is the first
            // typed at whatever prompt follows, and the gate opens for it.
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
            // Rows moved under the screen (`aterm-predict`'s `observe_coordinates`:
            // the content-coordinate generation changed): every guess goes as at
            // `Submit`, owed, the proof and the line with them, but THE FIRST
            // KEY's gate and whether the newest key was the first stand as they
            // stood. A scroll lets no key past the predictor, so the next key it
            // is handed is still the first since the terminator that opened the
            // gate: a shell on the bottom row scrolls the screen with every
            // Enter's own output, before the next prompt's first key. (A host
            // reset may have let keys past, and is `Submit`.)
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
            // The owed echo's glitch window passes: whatever stands in a guess's
            // cell from now on is that guess's own echo or nobody's.
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
                // A session nothing is known about has learned no binding (`Buggy`
                // keeps the last session's: its line editor taught it, not this one's).
                learned = if Buggy == 1 { learned } else { 0 };
                witness = 0;
                // What was typed at the session just left says nothing of this one.
                gate = 0;
                first = 0;
                visible = 0;
                erased = 0;
                // Dropping the estimate is no retraction (nothing says the link is fast): the
                // streak restarts, and `FreshSessionHasNoInheritedRtt` covers this step.
                fast_streak = 0;
                retracted = 0;
                // The echoes in flight belong to the PTY just left. (This is the
                // switch to a session with no estimate, `reset_session`; a swap
                // back to a known session brings its owed echo back with its
                // estimate, which the model's single session does not separate.)
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
            // Stated about the closing step: a close erases pixels the user is looking at, so
            // one lucky fast turn, or a close with type-ahead pending, is the blink.
            invariant RetractOnlyOnSustainedFastEvidence:
                if retracted == 1 { Hide <= fast_streak && pending == 0 } else { 1 == 1 };
            // The proof is the guess's own echo, never one owed to a keystroke no
            // guess tracks: while such an echo may still be in flight, nothing
            // confirms.
            invariant ProofIsAttributable:
                if owed == 1 { confirmed == 0 } else { 1 == 1 };
            // An edit key displays only once its class is proven this epoch.
            invariant EditVisibleNeedsClassProof:
                if visible == 1 {
                    if class == 1 { edit_proven == 1 } else { 1 == 1 }
                } else {
                    1 == 1
                };
            // The class proof stands only on the glyph proof: whatever ends the one
            // ends the other.
            invariant ClassProofRidesOnGlyphProof:
                if edit_proven == 1 { confirmed == 1 } else { 1 == 1 };
            // A line-edge key displays only on a witnessed line, its binding learned.
            invariant LineEdgeNeedsWitnessAndBinding:
                if visible == 1 {
                    if class == 2 { learned == 1 && witness == 1 } else { 1 == 1 }
                } else {
                    1 == 1
                };
            // A session nothing is known about has learned nothing.
            invariant FreshContextHasNoBinding:
                if fresh == 1 { learned == 0 } else { 1 == 1 };
            // A witnessed line was started by the first key typed at its prompt.
            invariant WitnessIsTheFirstKey:
                if witness == 1 { witness_first == 1 } else { 1 == 1 };
        }
    }
}

/// NETWORK LISTENER SLOT DISPLACEMENT — `aterm-net`'s listener slots (the
/// fleet listener's, `slots::Slots`; the network drive's `serve` until it was
/// removed, 2026-10-15) keep at most `Cap` connections in flight, handshaking
/// or relaying, and at capacity let a newcomer take the slot of one that has
/// NOT authenticated (r13 NET-1, idle sockets renewed
/// against the handshake deadline used to hold every slot and lock token
/// holders out). Which slot is the address policy's (`slots::pick_displaced`,
/// unit-tested); this is the machine that policy runs on, and the law it must
/// never break: an admitted relay is never cut.
///
/// A connection holding a slot is `pending` (watchdog state `WD_RUNNING`), `authed`
/// (`WD_AUTHED`: relaying, or tearing down after a denied capability) or `cut` (`WD_FIRED` or
/// `WD_EVICTED`: socket force-closed, threads ending). One compare-and-swap out of `WD_RUNNING`
/// decides between the three claimants: the handshake (`Authenticate`,
/// `HandshakeWatchdog::finish`), the deadline (`Deadline`) and the listener (`Evict`,
/// `Evictor::evict`). `Evict` is the accept loop at capacity: it claims a pending connection,
/// then waits (`waiting`) for its threads to end (`Release`) before the newcomer takes the slot
/// (`Accept`), or gives up after `ACCEPT_POLL` (`Refuse`). `Close` ends a relay or a denied
/// connection. `arrivals` and `claims` are ghost counters for the exactly-once law.
///
/// `Buggy = 1` is four defects, each caught by its own invariant: `Accept` takes a slot the
/// displaced connection still holds (`InFlightBounded`); `Authenticate` stores `WD_AUTHED` over a
/// cut connection's claim (`ClaimExactlyOnce`); the deadline force-closes a connection its
/// handshake claimed (`AuthedNeverCut`); `Evict` stores `WD_EVICTED` over an authenticated
/// connection instead of a compare-and-swap (both of the last two). Each mutant fires only from
/// a state that has not slipped yet, so the `Buggy = 1` space stays finite.
///
/// Tier-1: aterm-net's `slots::tests::slots_conform_to_net_slot_displacement`
/// drives the real `Slots` and `HandshakeWatchdog` over real sockets at the
/// production capacity (`Cap` overridden to the fleet's `MAX_SLOTS`), projects
/// every slot's claim onto these counts after each step, and validates each
/// transition; every action is anchored by `#[refines]` in `slots.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn net_slot_displacement_model() -> Model {
    aterm_spec::ty_model! {
        NetSlotDisplacement {
            const Buggy = 0;
            const Cap = 2;
            // Bounds the connections the checker walks.
            const Arrivals = 4;
            var pending = 0;
            var authed = 0;
            var cut = 0;
            var waiting = 0;
            var arrivals = 0;
            var claims = 0;
            // An authenticated connection's socket was force-closed.
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

/// Alt-screen archive pool, charged against the process: the retention budget belongs to the
/// machine's memory, so it is charged against every live archive at once.
///
/// This is the same shape as `flash_limiter_window_model`: a correct theorem
/// about ONE archive ("it never retains more than its budget") stays true no
/// matter how many archives exist, which is exactly why it cannot see the
/// defect it is meant to rule out. Give every tab its own 4 MiB policy and each
/// archive independently satisfies its own bound while the PROCESS pays N times
/// over — measured at 8 × 4 MiB before the pool existed.
///
/// So this model carries TWO archives (`a`, `b`: rows retained) and three
/// dials:
///
/// `PoolBounded` is charged against the sum, over settled archives (retention inside the
/// current allowance), because that is what the code guarantees: an archive lowers only its
/// own retention (reaching into other terminals' mutexes from whichever thread opened a tab
/// invites deadlock), so one that filled up while alone keeps those rows until its next
/// commit. The bound converges rather than holding at every instant. `Local = 1` breaks it.
///
/// # The two invariants have one victim each, and that is the point
///
/// `Total = 4` rows with `Half = 2` (the IR has no division) are the smallest sizes that
/// separate the cases. Tier 1: aterm-core's
/// `alt_archive_pool_conformance_real_archives_project_onto_model` drives the real
/// `AltArchive` pair through the same commits and projects every retention onto this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_archive_pool_model() -> Model {
    aterm_spec::ty_model! {
        AltArchivePool {
            const Buggy = 0;      // 1 = a joined-but-empty archive still takes a share;
                                  // 2 = each archive keeps its own whole budget
            const Local = 0;      // 1 = each archive keeps its own whole budget
            const Instances = 2;  // archives joined to the pool
            const Total = 4;      // the pool, in rows
            const Half = 2;       // Total / 2 — the IR has no division
            var a = 0;            // rows retained by archive A
            var b = 0;            // rows retained by archive B
            var steps = 0;        // run bound

            // A commits one frame, then `evict` trims to what A may retain now. A counts
            // itself (it is drawing on the pool), so the share is read with itself in the
            // divisor.
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

            // B exists only in a multi-archive world.
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

            // B leaves the alt screen: its rows go, and with them its claim on the pool (so
            // `IdleTakesNothing` is reachable after B held something).
            action WipeB when (steps <= 9 && Instances > 1) {
                steps = steps + 1;
                b = 0;
            }

            // One total, charged against the sum over settled archives: one over its current
            // allowance has not drawn since the allowance moved, and comes within it on its
            // next frame.
            // Dial 2 is the `Local` corner as a mutant: each archive keeps its own whole
            // budget, so two settled archives hold twice the pool.
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

            // While B is empty, A may fill the whole pool.
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
///
/// Every aterm instance keeps its own copy of the kitty collection and folds the others in.
/// `merge_collectible` takes `max_ts` on the favourite stamp, a join on a semilattice
/// (commutative, idempotent, order-independent), which is why a wear needs no unpin and no
/// tombstone. The model has two replicas, two collected cats, a logical clock, and both flush
/// directions in either order. `Buggy = 1` takes the incoming value instead of the max (last
/// writer by arrival).
///
/// `AMergeNeverLosesAPick`: `b*_hi` records each stamp's value before the action that writes
/// it (updates read the pre-state), so `b*_hi <= b*` says a stamp never went backwards.
/// `Buggy = 1` breaks it when a flush carrying a stale row lands on a replica that has since
/// worn something, overwriting B's later pick with A's older one.
///
/// The invariant that IS stated carries its own victim, which is what keeps a
/// checked law honest:
///
/// * `AMergeNeverLosesAPick` is the CRDT property. `b*_hi` records each stamp's
///   value BEFORE the action that writes it (updates are simultaneous, so the
///   right-hand side reads the pre-state), so `b*_hi <= b*` says a stamp never
///   went BACKWARDS. `Buggy = 1` breaks it the moment a flush carrying a stale
///   row lands on a replica that has since worn something: B's own later pick is
///   overwritten by A's older one, which is a pin the user made and the machine
///   silently dropped.
///
/// CONVERGENCE ITSELF IS NOT STATED AS AN INVARIANT, deliberately. It follows
/// from the join and it is what the sentence in the commit message claims — but
/// NO mutant of a single `Buggy` dial can falsify it, because an assignment
/// merge converges too (just on the wrong value). This suite's own standard,
/// `verify::sweep_faults` (`aterm-models/tests/non_vacuity.rs`), calls an
/// invariant no mutant can break a GHOST and fails the build for it, having
/// caught two shipped ones already. So convergence is witnessed as a concrete
/// TRACE in the discharge test instead of dressed up as a checked law. The law
/// that carries the mutant is the one above, and it is the load-bearing half
/// anyway: a join that never goes backwards is what MAKES the ledgers converge.
///
/// Tier-1 binding: aterm-gui's `wear_and_merge_conform_to_the_kitty_pin_merge_model`
/// (`kitty_log.rs`) walks this model's whole reachable space on two REAL
/// `KittyLog` replicas — `KittyLog::wear` at each clock stamp, `merge_from` for
/// each flush — reading every stamp back out of the rows, and rejects the
/// assignment merge injected after the same real fold.
/// `a_wear_reaches_the_delta_or_it_does_not_survive_a_restart` pins the other
/// half: a wear reaches the delta this model flushes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_pin_merge_model() -> Model {
    aterm_spec::ty_model! {
        KittyPinMerge {
            const Buggy = 0;  // 1 = a merge takes the incoming stamp, not the max
            var a1 = 0;       // replica A's favourite stamp for cat one
            var a2 = 0;       // replica A's favourite stamp for cat two
            var b1 = 0;       // replica B's, for cat one
            var b2 = 0;       // replica B's, for cat two
            var b1_hi = 0;    // the greatest stamp B held for cat one before now
            var b2_hi = 0;
            var ab = 0;       // A's delta has reached B since the last wear
            var clock = 0;    // logical clock; each wear takes the next tick
            var steps = 0;    // run bound

            // The user on instance A wears cat one.
            action WearOneOnA when (steps <= 5 && clock <= 2) {
                steps = steps + 1;
                clock = clock + 1;
                a1 = clock + 1;
                ab = 0;
            }

            // The user on instance B wears cat two.
            action WearTwoOnB when (steps <= 5 && clock <= 2) {
                steps = steps + 1;
                clock = clock + 1;
                b2 = clock + 1;
                b2_hi = if b2 > b2_hi { b2 } else { b2_hi };
                ab = 0;
            }

            // A's delta lands in B's ledger.
            action FlushAToB when (steps <= 5) {
                steps = steps + 1;
                b1 = if Buggy == 1 { a1 } else { (if a1 > b1 { a1 } else { b1 }) };
                b2 = if Buggy == 1 { a2 } else { (if a2 > b2 { a2 } else { b2 }) };
                b1_hi = if b1 > b1_hi { b1 } else { b1_hi };
                b2_hi = if b2 > b2_hi { b2 } else { b2_hi };
                ab = 1;
            }

            // And B's lands in A's.
            action FlushBToA when (steps <= 5 && ab == 1) {
                steps = steps + 1;
                a1 = if Buggy == 1 { b1 } else { (if b1 > a1 { b1 } else { a1 }) };
                a2 = if Buggy == 1 { b2 } else { (if b2 > a2 { b2 } else { a2 }) };
            }

            // A stamp never goes backwards, so no pin the user made is silently dropped.
            invariant AMergeNeverLosesAPick: b1_hi <= b1 && b2_hi <= b2;
        }
    }
}
