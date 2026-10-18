// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Terminal-core, presentation-loop, and capability-plumbing models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor is re-exported from this crate's root).

use aterm_spec::derive::*;

/// TERMINAL MODES — the DEC/ANSI mode-flag state machine the VT handler
/// maintains, and the reset discipline over it. The 28 actions are exactly the
/// `#[refines(machine="terminal_modes", action=…)]` set in
/// `aterm-core/src/terminal/handler_{dec,dec_refinement,esc,state,report}.rs`:
/// thirteen `Set*`/`Reset*` toggle pairs and the two resets (`SoftReset` =
/// DECSTR, `FullReset` = RIS). The DEC modes the handler does NOT model (VT52,
/// 132-column, reverse video, BiDi, …) are `#[spec_unmodeled(reason=…)]`
/// waivers, not actions here.
///
/// Every variable is a boolean FACT about a mode, and every action sets its own
/// variable to an exact value, which is what makes the projection from the real
/// `TerminalModes` exact rather than a witness. Three of them abstract a
/// multi-valued mode: `mouse_mode` is "some mouse tracking is on"
/// (`SetMouseMode` is X10/1000/1002/1003, `ResetMouseMode` tracking-off),
/// `sgr_mouse` is "the SGR 1006 encoding is in force" (the encodings that
/// replace it are `ResetSgrMouseEncoding` sites), and `cursor_style` is "the
/// cursor shape differs from the HOST's default" — `SetCursorStyle` lands on a
/// shape other than the host's, `RestoreCursorStyle` on the host's own, and
/// DECSCUSR and mode 12 (the blink bit of the shape) are sites of both, the
/// bytes deciding which. Both resets restore the host-configured shape, not
/// `CursorStyle::default()`.
///
/// **Invariant `ResetRestoresDefaults`.** Immediately after either reset
/// (`reset = 1`, a history variable every other action clears) the terminal is
/// usable again: cursor shown in the host shape, autowrap on, no insert /
/// origin / newline mode, no application cursor keys, no mouse capture or SGR
/// encoding, no focus or bracketed-paste reports, and NO synchronized-output
/// hold. `alt_screen` is deliberately outside it — DECSTR keeps the current
/// screen, RIS leaves the alternate one. `Buggy = 1` is a DECSTR that forgets
/// the synchronized-output hold: a program that soft-resets mid-frame then
/// leaves the host's present-hold armed and the screen frozen. `ty` proves the
/// invariant at `Buggy = 0` and catches that at `Buggy = 1`.
///
/// Tier-1: `aterm-core/src/terminal/terminal_modes_conformance.rs` drives every
/// anchored action through real `Terminal::process` bytes, projects the real
/// `TerminalModes` with `project_modes`, validates each step against this model,
/// and audits (per step) that the window entered the action's own anchor.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor — nested
// `vec!`/struct literals whose aggregate-operand count exceeds the
// VC-generation work budget, so its obligations are left Unknown
// fail-closed regardless. No runtime logic and no panic surface beyond
// the idiomatic allocs; the MODEL it returns is what `ty` machine-checks.
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

/// The bounded event-log ring as a derived model — the single source the spec in
/// `Evict.tla` hand-encodes, scalar-projected to `<<seq, lo>>`. `Push` advances
/// `seq` and evicts the oldest live event (`lo`) exactly when the live window
/// would exceed `Cap`. `MaxSeq` bounds the state space so `ty check` is
/// exhaustive + terminating; `Cap` is the ring capacity. Action name is `Push`
/// (not `Append`, which clashes with ty's Sequences builtin).
///
/// `Buggy = 1` evicts one push late, so the ring holds `Cap + 1` live events —
/// the shape of the alt-screen archive undercharge (`a4c4d3eaa`: a row charged
/// as `len + 32` let a 4 MiB budget hold 5.23 MiB, and admitted a fourth row into
/// a three-row budget, the negative control `alt_archive_tests.rs` binds against
/// this model). `LenBounded` is its counterexample.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn ring_model() -> Model {
    Model {
        name: "Ring",
        consts: vec![("MaxSeq", 6), ("Cap", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "lo",
                init: 1,
            },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Push",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))), // seq <= MaxSeq - 1
            updates: vec![
                Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                },
                Update {
                    var: "lo",
                    // IF (seq + 1) - lo + 1 > Cap + Buggy THEN lo + 1 ELSE lo
                    expr: if_(
                        gt(
                            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
                            add(cst("Cap"), cst("Buggy")),
                        ),
                        add(var("lo"), int(1)),
                        var("lo"),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "LenBounded",
            kind: InvariantKind::Claim,
            // seq - lo + 1 <= Cap
            expr: le(add(sub(var("seq"), var("lo")), int(1)), cst("Cap")),
        }],
    }
}

/// A second derived model — a writer/subscriber cursor — chosen because it
/// exercises derivation paths the ring does not: TWO actions (so `Next` is a
/// disjunction) and PARTIAL updates (so each action emits an `UNCHANGED` clause
/// for the variable it leaves alone). `Grow` advances the writer `seq`; `Deliver`
/// catches the reader `cursor` up to `seq`. Invariant: the reader never passes the
/// writer (`cursor <= seq`). This is the Subscribe/Kernel family in miniature, and
/// it proves the derivation engine generalizes beyond the single-action ring.
///
/// `Buggy = 1` is the off-by-one class a subscriber cursor can ship with (the
/// non-vacuity witness for `CursorBounded`, not a defect the code is known to have
/// had): a `Deliver` that parks the cursor one PAST the head it delivered, so the
/// next event written is silently skipped. `CursorBounded` catches it on the first
/// delivery. Tier-1: aterm-buffer's `tests/conformance_cursor.rs` drives the real
/// `Surface::subscribe`/`poll` — the library's read face, which no product crate
/// calls today — and reads the cursor's position back through the real `poll`
/// itself, which is what catches an overshoot `subscribe_model`'s bind (a
/// caller-tracked cursor) cannot see.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn cursor_model() -> Model {
    Model {
        name: "Cursor",
        consts: vec![("MaxSeq", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "cursor",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Grow", // writer appends; cursor is UNCHANGED
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                }],
            },
            Action {
                name: "Deliver", // reader catches up; seq is UNCHANGED
                guard: Some(gt(var("seq"), var("cursor"))),
                // cursor' = IF Buggy = 1 THEN seq + 1 ELSE seq  (Buggy overshoots)
                updates: vec![Update {
                    var: "cursor",
                    expr: if_(
                        eq(cst("Buggy"), int(1)),
                        add(var("seq"), int(1)),
                        var("seq"),
                    ),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "CursorBounded",
            kind: InvariantKind::Claim,
            expr: le(var("cursor"), var("seq")), // cursor <= seq
        }],
    }
}

/// A third derived model — the subscriber's NO-SILENT-LOSS / gap discipline, the
/// kernel family's most important correctness property: a reader that has fallen
/// behind the live ring window MUST receive a Gap (resync) and must NEVER be
/// silently delivered events as if nothing was lost. Scalar projection over
/// `<<seq, lo, cursor, lost>>`: `Grow` advances the writer and evicts the oldest
/// when over `Cap`; `PollGap` resyncs a fallen-behind reader (`lo > cursor + 1`);
/// `PollDeliver` delivers while the reader is still within the live window.
///
/// The `Buggy` constant flips `PollDeliver`'s guard: with `Buggy = 0` (committed)
/// it is correctly guarded and `lost` stays 0; with `Buggy = 1` it fires even when
/// the reader is behind, silently skipping evicted events — so `lost` becomes 1
/// and `NoSilentLoss` is violated. Thus `ty` both PROVES the property (Buggy=0)
/// and, via a `Buggy=1` cfg, shows it genuinely CATCHES the silent-loss bug.
/// Exercises the `Expr` disjunction (`\/`) and equality (`=`) operators.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn subscribe_model() -> Model {
    Model {
        name: "Subscribe",
        consts: vec![("MaxSeq", 4), ("Cap", 2), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "lo",
                init: 1,
            },
            StateVar {
                name: "cursor",
                init: 0,
            },
            StateVar {
                name: "lost",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Grow", // writer appends + evicts oldest when over Cap
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), int(1)),
                    },
                    Update {
                        var: "lo",
                        expr: if_(
                            gt(
                                add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
                                cst("Cap"),
                            ),
                            add(var("lo"), int(1)),
                            var("lo"),
                        ),
                    },
                ], // cursor, lost UNCHANGED
            },
            Action {
                name: "PollGap", // reader fell behind (lo > cursor + 1): resync, no loss
                guard: Some(gt(var("lo"), add(var("cursor"), int(1)))),
                updates: vec![Update {
                    var: "cursor",
                    expr: var("seq"),
                }], // seq, lo, lost UNCHANGED
            },
            Action {
                name: "PollDeliver", // deliver; correct iff the reader is still in window
                // Buggy = 1 \/ lo =< cursor + 1  (Buggy removes the in-window guard)
                guard: Some(or_(
                    eq(cst("Buggy"), int(1)),
                    le(var("lo"), add(var("cursor"), int(1))),
                )),
                updates: vec![
                    Update {
                        var: "cursor",
                        expr: var("seq"),
                    },
                    // lost' = IF lo > cursor + 1 THEN 1 ELSE lost  (records a silent skip)
                    Update {
                        var: "lost",
                        expr: if_(
                            gt(var("lo"), add(var("cursor"), int(1))),
                            int(1),
                            var("lost"),
                        ),
                    },
                ], // seq, lo UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "NoSilentLoss",
            kind: InvariantKind::Claim,
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}

/// Which historical defect a wake-latch model's `Buggy = 1` replays (see
/// [`damage_to_present_model`] and [`wake_heal_evidence_model`]: one machine,
/// held to three laws, each with the mutant that breaks it).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WakeLatchMutant {
    /// The shipped pre-2026-07-05 ONE-SHOT latch: no heal, no watchdog
    /// deadline and no re-post, so a lost wake starves its session's presents.
    OneShot,
    /// The pre-2026-09-29 AGE-ONLY heal: a latch older than `Expiry` is
    /// presumed lost whether or not its event is still queued behind a long
    /// frame — the false heal (a redraw of every window plus a duplicate wake).
    AgeOnly,
}

/// DAMAGE→PRESENT BOUNDED RESPONSE (the 2026-07-05 five-fps incident, proven
/// and caught). Abstract twin of the `Wake::Output` delivery spine, with the
/// event loop's TURN made explicit so a long frame is representable: PTY damage
/// arrives (`Damage*`), the coalescing latch (`spawn::gated_output_wake`) posts
/// at most one in-flight wake (`inflight`); a loop turn DRAINS the queued user
/// events (`TurnBegin` — a wake in flight is handled: the latch clears and the
/// damage presents) and may then stay BUSY for any number of ticks (a long
/// frame); it ends in `about_to_wait` (`TurnEnd`), where the lost-wake WATCHDOG
/// judges the latch, marks an armed one `seen`, and folds a `recheck` deadline
/// for it. A wake can be LOST without a handler pass (`Lose`; bounded by
/// `LostBudget`). Time (`Tick`) runs freely through a busy turn; an idle loop
/// turns at once for a queued wake and at a due recheck — and ONLY a `TurnEnd`
/// that saw the arm folds one (the house guarded-Tick encoding: time cannot
/// pass a due turn). The loop's first turn (winit's `Init`) is running at the
/// initial state.
///
/// A wake posted while the loop is IDLE and lost together with its wake-up is
/// seen by no `TurnEnd`, so no recheck exists for it: it is STRANDED. The
/// reader re-posts it (`DamageRepost`, `spawn::wake_stranded`) at the first
/// damage after its expiry, and never re-sends a wake a turn has passed or one
/// that may still be queued (`DamageCoalesced`).
///
/// The invariant `DamageBounded` is the property the incident violated: damage
/// waits at most `Expiry + 1` IDLE ticks for its present (`waited` counts only
/// ticks the main thread spent idle — its own long frames are its own latency,
/// not the latch's — and NOT a stranded wake's ticks, which end at the
/// session's next output after the expiry: if the program falls silent, at the
/// loop's next unrelated turn, a bound this model does not claim) — never the
/// process-lifetime starvation the user saw. `StrandedReposted`: no damage
/// arriving on a stranded wake past its expiry is swallowed without a re-post.
///
/// `Buggy = 0` (committed): the watchdog heals ON EVIDENCE — the latch is
/// still armed and a previous `TurnEnd` already saw it (`seen`: in the code,
/// its SENT stamp is older than the previous pass's), so the drain since must
/// have delivered it (`spawn::judge_output_wake`); the recheck it folds is one
/// expiry after the pass that saw it. `ty` PROVES both laws (the bound tight:
/// the worst case reaches exactly `Expiry + 1`); Tier-1-bound to the real
/// latch and watchdog by `spawn::latch_conforms_to_damage_to_present_model`
/// and `spawn::watchdog_conforms_to_damage_to_present_model`. `Buggy = 1`: the
/// original one-shot latch — no heal, no recheck, no re-post — and `ty` finds
/// Damage → Lose → TurnEnd → Tick* past the bound, the shipped defect, and a
/// stranded wake swallowing damage past its expiry.
///
/// Below the model's grain, stated: the model posts atomically with the arm.
/// The code arms, posts, then re-stamps the latch SENT with a clock read after
/// the post; an unsent arm is never judged lost nor re-posted
/// (`spawn::judge_output_wake`, `spawn::wake_stranded`, unit-tested by
/// `an_unsent_arm_is_never_presumed_lost`), which is what makes `seen` a
/// property of the code's stamps and not only of this model.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn damage_to_present_model() -> Model {
    wake_latch_model(WakeLatchMutant::OneShot)
}

/// NO FALSE HEAL (2026-09-29): the same machine as [`damage_to_present_model`],
/// held to the other half of the contract — the watchdog never heals a wake
/// that is merely LATE (still queued behind the main thread's turn). Measured
/// in the owner's live window before the fix: 163 "healed a lost output wake"
/// WARNs at 100–783 ms, each coinciding with a slow present of the same length
/// (`deadline=wake_heal`), at load ~150 — every one a wake still in the queue,
/// answered with a redraw of every window and a duplicate wake on a main thread
/// that was already behind. `falseHeal` latches a heal fired while its wake was
/// `inflight`. PROVES `NoFalseHeal` at `Buggy = 0` (the evidence rule: a
/// `TurnEnd` saw the arm, so the drain since delivered anything in flight),
/// CATCHES the age-only heal at `Buggy = 1` (Damage in the first turn, a busy
/// frame of `Expiry + 1` ticks, TurnEnd — the exact live trace).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn wake_heal_evidence_model() -> Model {
    wake_latch_model(WakeLatchMutant::AgeOnly)
}

// Skip (T2 vcgen-budget lane): a spec-model DATA constructor.
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
    // Keep the OLDEST unpresented damage's clock running: pending damage
    // arriving on pending damage does not restart `waited`.
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
                    // Pending damage waits at most Expiry+1 idle ticks — the
                    // bounded response the 5 fps incident violated (its bound
                    // was ∞).
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
                // Damage on an armed latch coalesces — a wake a turn has passed
                // is the watchdog's, and one that may be queued behind a long
                // frame must not be duplicated (the age re-send was the
                // duplicate-wake half of the 2026-09-29 false heals) — unless
                // it is stranded past its expiry. A swallowed stranded one is
                // booked in `missedRepost`.
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
                // The in-flight wake is DROPPED without a handler pass (the
                // incident's trigger) — while the loop runs, or while it sleeps
                // together with its wake-up. The latch stays armed.
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
                // A loop turn begins and drains the queued user events: a wake in
                // flight is handled (latch cleared) and its damage presented.
                // Enabled whenever idle: any event may turn the loop. The recheck
                // deadline is consumed; the turn's `TurnEnd` folds a new one.
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
                // `about_to_wait`: the watchdog judges the latch — heals (clear +
                // repaint every window) when the heal predicate holds; otherwise
                // an armed latch is now SEEN and folds a recheck one expiry out
                // (`now + Expiry + 1` ticks: the code's `now + expiry` ns, never
                // in the past).
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

/// OBSERVATION KERNEL — EARLIEST-ARMED IDLE DEADLINE (RFC L0). The abstract twin
/// of [`WatcherSet::next_deadline`](../../aterm_core/terminal/observe): the host
/// arms ONE `ControlFlow::WaitUntil`, and it must equal the MINIMUM of all
/// pending `IdleFor` deadlines so an earlier deadline is never missed (the
/// `BellFlash::deadline` discipline). Scalar projection `<<armed, minp>>` over
/// two deadline values (near = 1, far = 2; `4` is the unset sentinel): `ArmNear`
/// / `ArmFar` register a deadline; `armed` must track `minp = min` of everything
/// registered.
///
/// `Buggy = 0` (committed): `armed' = min(armed, v)`, so `armed = minp` always.
/// `Buggy = 1`: keep-first (`armed` set only while unset), so arming the FAR
/// deadline then the NEAR one leaves `armed = 2` while `minp = 1` — an earlier
/// wake is missed. `ty` PROVES `armed = minp` (Buggy=0) and CATCHES the
/// keep-first bug (Buggy=1 → counterexample). Two-action disjunctive `Next` with
/// nested `if` updates (the `cursor_model` family, plus a min computation).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
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

/// SURFACE COVERAGE — the one-dimensional abstract twin of the production GPU
/// present viewport/scissor. A raw window surface may be wider/taller than the
/// integer-cell renderer frame after font zoom; every present must cover the
/// WHOLE destination and resolve the remainder with the live terminal
/// background. Applying the law independently on x and y covers all four bands.
///
/// Correct (`Buggy=0`) presents through a full-surface viewport/scissor and marks
/// a non-empty band with the live background. Mutant (`Buggy=1`) reproduces the
/// deleted frame-sized viewport: only `frame` units are covered and the band is
/// left stale. Tier-1 drives the genuine GPU production seam at an odd +7px
/// destination and projects its exact pixels onto this model.
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

/// Abstract completeness gate for first-present phase publication: a valid
/// partition cannot publish until eight intervals are present. This deliberately
/// does NOT model timestamp identity, order, or duration arithmetic: the shipping
/// GUI's pure derivation checks those properties directly and its Tier-1 test
/// separately carries reordered-timestamp negative controls. Here each `Step`
/// means only "one more interval exists". `Buggy=1` admits an early publication
/// so Tier-0 must produce a counterexample rather than accepting a vacuous happy
/// trace.
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

/// PRESENT RETRY — bounded, future-only autonomous recovery after a dropped
/// surface transaction. `remaining` is the retry fuel for the current external
/// stimulus episode; `train` counts autonomous wakes since the last stimulus or
/// successful present; `retry=1` is one strictly-future deadline (`2` is kept
/// as the host negative-control value); `ready=1` means a redraw attempt may
/// run; `parked=1` means recovery waits for a new external stimulus; and
/// `bootstrap_available=1` is the one fresh-window permit for recovering an
/// initial `GpuOccluded` result.
///
/// Correct (`Buggy=0`): `DropBootstrapOccluded` is enabled only for the pristine
/// initial retry state, consumes its one-way permit, and arms one strictly-future
/// deadline. It cannot repeat after `Wake`, and neither external stimuli nor a
/// successful `Present` replenish the permit. Every `Wake` spends one unit of
/// fuel and, once empty, a further `Drop` parks without a deadline. `Stimulus`
/// begins a fresh bounded episode only when recovery is unresolved;
/// `ForcedStimulus` represents a real resize/expose that requests a frame even
/// from idle. `SynchronousStimulus` is the capture/internal twin: its caller
/// presents in the same main-thread operation, so it resets unresolved retry
/// state without creating a new OS redraw request, while preserving any request
/// that was already outstanding. An ordinary idle input stimulus is deliberately
/// not a transition, matching the production no-op. `Present` returns to
/// quiescent retry state.
/// Mutant (`Buggy=1`): wakes do not spend fuel, so the strictly-future
/// Drop→Wake train exceeds `RetryCap`. This catches unbounded autonomous
/// recovery independently of the separate past-deadline negative control in the
/// Tier-1 host test.
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

/// GPU DEVICE-LOSS ROUTING — a failed surface acquire on a latched dead device
/// must bypass transient retry and enter CPU fallback immediately. Retrying a
/// dead GPU repeatedly can only consume the bounded surface-retry train and
/// leave the last frame parked forever.
///
/// `route=1` is ordinary surface retry and `route=2` is GPU→CPU recovery.
/// Correct (`Buggy=0`) routes the lost observation to recovery. The historical
/// mutant (`Buggy=1`) routes it back into retry, so `LostUsesFallback` produces
/// a counterexample before the retry train could silently park.
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
/// Correct (`Buggy=0`) covers fresh, fuelled, and exhausted entry paths.
/// `Buggy=1` enables eight dead negative controls, one slip of the transaction
/// each, and each the first thing its own law sees. Three are the historical
/// mutant's defects:
/// * the fallback installed but the GPU recording left alive
///   (`LossStopsGpuRecording`);
/// * the failed-present path parks instead of arming its fresh retry
///   (`UnexhaustedFailureOwnsRetryOrDeliveredAttempt`);
/// * a failed-present entry that counts its frame again although the source
///   drop already did (`OneDropCountPerFrame`).
///
/// Five more are one line of the shipping transaction written wrong:
/// * the fuelled failed-present path refines its drop's accounting but keeps
///   the source surface's reason instead of `CpuAcquire`
///   (`FailedFallbackIsDiagnosed`);
/// * the exhausted path treats the failure as a fresh drop and arms a retry
///   past the fuel cap — a retry train with no end (`ExhaustedFailureIsParked`);
/// * `PresentRetry::take_due` hands the wake over without clearing the consumed
///   deadline, which then re-arms turn after turn (`DeliveredRetryHasNoDeadline`);
/// * the dropped-frame success path is classified `SourceReadyRequested`
///   without its `request_redraw`, freezing the window on the dead GPU's last
///   frame (`ReadyCpuOwnsRedrawUntilPresent`);
/// * the delivered retry wake is taken as the acknowledging present — the
///   acknowledgement-on-stimulus defect `RecoveryRedraw` pins, here before the
///   CPU target is even built (`CpuPresentWasReady`).
///
/// Why dead actions and not `Buggy` arms inside the entry actions: an inline
/// arm makes its slip on EVERY firing, so at `Buggy = 1` the correct fresh
/// retry would never exist and the wake-side slips could only fire after the
/// entry slip had already broken a law. No separate "a CPU present completes
/// the failure" law either. `fallback_failed` is a record only this model keeps
/// — nothing in aterm-gui is projected onto it — and only the `Succeed*`
/// entries and `BuildCpuAfterWake` clear it, the same steps that set the
/// `requested` every `PresentCpu` needs; so no modelled present can land over a
/// live record, and a law saying so would judge the model's own bookkeeping.
/// The one path that could, a parked failure presenting after the external
/// `PresentRetry::Stimulus`, is outside this model. The shipping reset is
/// `PresentRetry::on_presented`, and aterm-gui's success bind
/// (`successful_gpu_recovery_classifies_source_ready_only_after_redraw_request`)
/// asserts it leaves the default retry state after the acknowledging present.
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
/// Correct (`Buggy=0`) retains the acknowledgement bit across every replacement
/// request. `Buggy=1` enables two dead mutants, the two historical halves of the
/// frozen no-echo/app-owned-input window:
/// * `BuggyStimulusAcknowledges` clears the bit on the first stimulus, so a
///   second suppressed edge is never replaced (`OnlyPresentAcknowledgesRecovery`);
/// * `BuggyStimulusWithoutRedraw` reopens the gate without a `request_redraw`,
///   the gate-open/no-redraw reset (`RecoveryStimulusRequestsRedraw`).
///
/// Why dead actions, not a `Buggy` arm inside `Stimulus`: one arm can make only
/// one of the two slips, and a stimulus that both acknowledges and requests
/// nothing would leave no suppressed edge for the first slip to be caught on.
///
/// "A suppressed request leaves the episode unresolved" is not a separate law.
/// A present clears suppression, so a suppressed edge over a resolved episode
/// needs an acknowledgement without a present first — which
/// `OnlyPresentAcknowledgesRecovery` refuses at that step.
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

/// PREDICTIVE ECHO VISIBILITY — the bounded display/expiry policy shared by
/// native and web hosts. `app_owned=1` represents an application-owned input
/// composer (the observed Codex Kitty mode includes REPORT_EVENT_TYPES),
/// `slow=1` means Adaptive prediction has measured a useful RTT, and
/// `confirmed=1` is the current-line echo proof. `pending` is predictor state,
/// while `visible` and `erased` are the user-observable pixel events.
///
/// `Submit` is EVERY epoch boundary, not only the Enter key: a line terminator
/// in any spelling (`Predictor::note_line_submit`), any other press the host
/// could not predict (`note_unmodelled_input`), a Backspace with nothing of the
/// predictor's own to cancel, a declined glyph, the terminal moving rows under
/// the cursor (the `coords` generation), and the program moving the cursor off
/// the cell the last confirmed echo left it in (a prompt printed beside it).
/// Each ends the proof; `aterm-predict`'s
/// `every_epoch_boundary_is_the_models_submit` binds all six to this action.
/// (`Submit` also drops the abstract `pending`; the implementation keeps a
/// proven line's in-flight pixels across a terminator alone, until their echo
/// or expiry, which the model's single-guess abstraction does not separate.)
///
/// Correct (`Buggy=0`): an app-owned composer does not even arm speculative
/// state; fast Adaptive links may track a guess but never paint it, so expiry
/// is invisible. A session switch starts with no inherited slow-link estimate
/// (`Predictor::reset_session`, exported to both web hosts as
/// `predict_session_reset`). Mutant (`Buggy=1`) is the deleted immediate-display
/// policy plus retained cross-session RTT: confirmation alone paints, the Codex
/// gate is ignored, and a new pane can inherit slow eligibility. The mutant
/// makes the fast-link blink, app-owned ghost, and RTT leak reachable.
///
/// HYSTERESIS (`fast_streak` / `retracted`). The gate is a LATCH, not a
/// comparator: at `Buggy=0`, `ConfirmFast` closes it only after `Hide` decisive
/// fast turns, never on a SINGLE sample (the implementation requires
/// `FAST_SAMPLES_TO_HIDE` consecutive decisive fast turns, a smoothed estimate
/// that agrees, and an EMPTY pending set). A conformance test that fires
/// `ConfirmFast` only at an already closed latch leaves that unpinned.
/// `fast_streak` counts the
/// decisive fast turns (capped at `Hide`) and `retracted` marks the STEP on
/// which the gate went 1 -> 0, because the defect is a retraction taken on thin
/// evidence, not a steady state: `RetractOnlyOnSustainedFastEvidence` then makes
/// the single-sample close (and any close with pixels still in flight — the
/// blink itself) an outright counterexample. `ConfirmFastInFlight` is the same
/// fast turn taken while type-ahead is still on glass; at `Buggy=0` it may
/// advance the streak but never close, which is what keeps that clause of the
/// invariant non-vacuous.
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
pub fn predictive_echo_visibility_model() -> Model {
    aterm_spec::ty_model! {
        PredictiveEchoVisibility {
            const Buggy = 0;
            // FAST_SAMPLES_TO_HIDE: consecutive decisive fast turns required before
            // Adaptive may stop painting. Opening takes fewer (asymmetric on
            // purpose — closing is the destructive direction), which the abstract
            // `ConfirmSlow` folds into one step.
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
                visible = 0;
                erased = 0;
                // Dropping the estimate is not a RETRACTION: nothing was measured
                // that says this link is fast, so the streak restarts at zero and
                // the step carries no evidence obligation (`FreshSessionHasNoInheritedRtt`
                // is what covers this direction).
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
                fast_streak <= Hide && retracted <= 1 && owed <= 1 && class <= 1 &&
                edit_proven <= 1;
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
            // The gate may stop painting ONLY on sustained fast evidence with
            // nothing in flight. Stated about the retracting STEP because that is
            // where the damage is: a close is an erase of pixels the user is
            // looking at, so one lucky fast turn (or any turn with type-ahead still
            // pending) closing the gate IS the blink, whatever the steady state
            // looks like afterwards.
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
        }
    }
}

/// **ALT-SCREEN ARCHIVE POOL, CHARGED AGAINST THE PROCESS** — the retention
/// budget belongs to the machine's memory, so it is charged against EVERY live
/// archive at once.
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
/// * `Instances` — how many archives are joined. `CommitB` is admitted only
///   when `Instances > 1`, so `Instances = 1` collapses this onto the
///   single-archive theorem.
/// * `Local` — WHAT each archive consults when it evicts. `Local = 0`
///   (committed) is the shipping shape: a SHARE of one pool. `Local = 1` is the
///   pre-pool world, each archive keeping the whole `Total`.
/// * `Buggy` — WHICH archives count toward the share. `Buggy = 0` (committed)
///   counts an archive only while it is DRAWING on the pool — enabled AND
///   holding rows. `Buggy = 1` is the review blocker: an archive counts as soon
///   as it is joined, holding or not, so eight open tabs divide the pool by
///   eight while one of them runs `vim`.
///
/// # The two invariants have one victim each, and that is the point
///
/// `PoolBounded` is charged against the SUM, because the process's memory is.
/// It is stated over SETTLED archives — one whose retention is inside its
/// CURRENT allowance — because that is exactly what the code guarantees and no
/// more: an archive can only lower its OWN retention (reaching into seven other
/// terminals' mutexes from whichever thread opened a tab is how deadlocks are
/// written), so one that filled up while alone keeps those rows until its next
/// commit. The bound CONVERGES rather than holding at every instant, and the
/// antecedent is what says so. At `Local = 1` two individually-correct archives
/// break it at every corner — multiplication alone is the defect.
///
/// `IdleTakesNothing` says an archive holding nothing costs the archives that
/// do nothing: while `b` is empty, `a`'s allowance is the whole `Total`. This
/// is the invariant `Buggy = 1` violates, and it violates it immediately —
/// the idle tab takes its share the moment it joins. `Local` cannot damage it
/// (a local archive's allowance is `Total` by construction), which is what
/// makes the G5 attribution corner meaningful.
///
/// Sizes are the smallest that separate the cases: `Total = 4` rows with
/// `Half = 2`, because the IR has no division. Tier-1 binding: aterm-core's
/// `alt_archive_pool_conformance_real_archives_project_onto_model` drives the
/// REAL `AltArchive` pair through the same commits and projects every retention
/// onto this model.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor — the MODEL it
// returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_archive_pool_model() -> Model {
    aterm_spec::ty_model! {
        AltArchivePool {
            const Buggy = 0;      // 1 = a joined-but-empty archive still takes a share;
                                  // 2 = each archive keeps its own whole budget
            const Local = 0;      // 1 = each archive keeps its OWN whole budget
            const Instances = 2;  // archives joined to the pool
            const Total = 4;      // the pool, in rows
            const Half = 2;       // Total / 2 — the IR has no division
            var a = 0;            // rows retained by archive A
            var b = 0;            // rows retained by archive B
            var steps = 0;        // run bound

            // A commits one frame: the row goes in, then `evict` trims to what
            // A may retain RIGHT NOW. A counts ITSELF (it is drawing on the
            // pool), so the share is read with itself already in the divisor —
            // a share computed without it is one share too generous.
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

            // B is the SECOND archive and exists only in a multi-archive world.
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

            // B leaves the alt screen: its rows go, and with them its claim on
            // the pool. This is what makes `IdleTakesNothing` reachable from a
            // state where B once held something.
            action WipeB when (steps <= 9 && Instances > 1) {
                steps = steps + 1;
                b = 0;
            }

            // THE POOL IS ONE TOTAL. Charged against the sum, over SETTLED
            // archives only (see the doc): an archive over its current
            // allowance has not drawn since the allowance moved, and comes
            // within it on its next frame.
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

            // AN ARCHIVE HOLDING NOTHING COSTS THE ONES THAT DO NOTHING. While
            // B is empty, A may fill the whole pool.
            invariant IdleTakesNothing:
                (b > 0)
                || (Total <= (if (Local == 1 || Buggy == 2) { Total }
                              else { if (if Buggy == 1 { if Instances > 1 { 1 } else { 0 } }
                                         else { if b > 0 { 1 } else { 0 } }) > 0
                                     { Half } else { Total } }));
        }
    }
}

/// **WEARING A CAT IS A JOIN, SO TWO INSTANCES CONVERGE AND NEITHER LOSES ITS
/// PICK** — the sentence the kitty-collection merge ships in prose, checked.
///
/// The collection is replicated: every aterm instance keeps its own copy and
/// folds the others in. `KittyLog::merge_collectible` takes `max_ts` on the
/// favourite stamp, which makes the merge a JOIN on a semilattice — commutative,
/// idempotent, and order-independent. That is the whole reason a wear needs no
/// unpin and no tombstone to be reliable, and it was asserted in a commit
/// message with nothing behind it.
///
/// Two replicas, two collected cats, a logical clock, and the two flush
/// directions in either order. `Buggy = 1` is the obvious wrong merge — TAKE the
/// incoming value rather than the max, last-writer-by-ARRIVAL — which is what a
/// reader reaches for when a merge looks like an assignment.
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
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor — the MODEL it
// returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_pin_merge_model() -> Model {
    aterm_spec::ty_model! {
        KittyPinMerge {
            const Buggy = 0;  // 1 = a merge TAKES the incoming stamp, not the max
            var a1 = 0;       // replica A's favourite stamp for cat one
            var a2 = 0;       // replica A's favourite stamp for cat two
            var b1 = 0;       // replica B's, for cat one
            var b2 = 0;       // replica B's, for cat two
            var b1_hi = 0;    // the greatest stamp B held for cat one BEFORE now
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

            // A STAMP NEVER GOES BACKWARDS: a merge is a join, so no pin the
            // user made is ever silently dropped.
            invariant AMergeNeverLosesAPick: b1_hi <= b1 && b2_hi <= b2;
        }
    }
}
