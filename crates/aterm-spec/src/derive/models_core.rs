// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Terminal-core, presentation-loop, and capability-plumbing models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// TERMINAL MODES — the DEC/ANSI/keyboard mode-flag state machine that the VT
/// handler maintains (TRUST_NATIVE_TLA, Phase 0: resolves the dangling
/// `terminal_modes` machine the `#[refines]` anchors in
/// `aterm-core/src/terminal/handler_{dec,esc,state,report}*.rs` point at).
///
/// Each modelled mode is a bounded scalar (booleans as `{0,1}`; the multi-valued
/// `mouse_mode`/`mouse_encoding`/`cursor_style` as small bounded ints). The 26
/// actions are exactly the `#[refines(machine="terminal_modes", action=…)]` set:
/// the `Set*`/`Reset*` toggle pairs, the multi-valued setters
/// (`SetMouseMode`/`SetSgrMouseEncoding`/`SetCursorStyle`), and the two reset
/// actions (`SoftReset` = DECSTR, `FullReset` = RIS) which return the modes to
/// known defaults. (The DEC modes the handler explicitly does NOT model —
/// VT52/132-col/reverse-video/BiDi/… — are `#[spec_unmodeled(reason=…)]` waivers,
/// not actions here; that is the deliberate "modelled vs. waived" split.)
///
/// **Invariant `ModesValid`.** Every mode stays inside its valid domain under ANY
/// interleaving of the 26 actions: the booleans never leave `{0,1}`, and the
/// multi-valued modes never leave their enum range. This is the contract the
/// handler genuinely maintains — `TerminalModes` fields are `bool`/small `enum`,
/// so a mode is never an out-of-range / torn value, and a reset always lands on a
/// valid default. It is non-vacuous: `ty` enumerates the full action fan-out from
/// `Init`, so a setter that pushed a mode out of range (or a reset that left a
/// stale out-of-range value) would be caught.
///
/// SCOPE: this scalar model captures mode *validity* and the reset discipline (the
/// set/reset/RIS/DECSTR contract), not per-mode rendering semantics — those live in
/// the engine and, where bounded, in the other derived models. It exists so the
/// `terminal_modes` anchors RESOLVE (obligation 4) and are fully bound-or-waived
/// (obligation 3): the smallest sound model that makes the cross-reference real.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor — nested
// `vec!`/struct literals whose aggregate-operand count exceeds the
// VC-generation work budget, so its obligations are left Unknown
// fail-closed regardless. No runtime logic and no panic surface beyond
// the idiomatic allocs; the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn terminal_modes_model() -> Model {
    crate::ty_model! {
        TerminalModes {
            // Boolean modes (0/1). DECSTR (SoftReset) / RIS (FullReset) defaults
            // are encoded in the two reset actions below.
            var app_cursor_keys = 0;
            var origin_mode = 0;
            var auto_wrap = 1;          // DECAWM defaults ON
            var cursor_visible = 1;     // DECTCEM defaults ON
            var focus_reporting = 0;
            var sync_output = 0;
            var insert_mode = 0;
            var new_line_mode = 0;
            var alt_screen = 0;
            var bracketed_paste = 0;
            // Multi-valued modes (small bounded enums). mouse_mode: 0=off..4; the
            // mouse coordinate encoding sgr flag: 0/1; cursor_style: 0..6 (DECSCUSR).
            var mouse_mode = 0;
            var sgr_mouse = 0;
            var cursor_style = 0;

            action SetApplicationCursorKeys { app_cursor_keys = 1; }
            action ResetApplicationCursorKeys { app_cursor_keys = 0; }
            action SetOriginMode { origin_mode = 1; }
            action ResetOriginMode { origin_mode = 0; }
            action SetAutoWrap { auto_wrap = 1; }
            action ResetAutoWrap { auto_wrap = 0; }
            action SetCursorVisible { cursor_visible = 1; }
            action ResetCursorVisible { cursor_visible = 0; }
            action SetFocusReporting { focus_reporting = 1; }
            action ResetFocusReporting { focus_reporting = 0; }
            action SetSynchronizedOutput { sync_output = 1; }
            action ResetSynchronizedOutput { sync_output = 0; }
            action SetInsertMode { insert_mode = 1; }
            action ResetInsertMode { insert_mode = 0; }
            action SetNewLineMode { new_line_mode = 1; }
            action ResetNewLineMode { new_line_mode = 0; }
            action SetAlternateScreen { alt_screen = 1; }
            action ResetAlternateScreen { alt_screen = 0; }
            action SetBracketedPaste { bracketed_paste = 1; }
            action ResetBracketedPaste { bracketed_paste = 0; }
            // Multi-valued setters: enter a representative valid value in range.
            // (The real handler picks among X10/Normal/ButtonEvent/AnyEvent etc;
            // the bounded abstraction is "any in-range mode", here a fixed witness.)
            action SetMouseMode { mouse_mode = 1; }
            action SetSgrMouseEncoding { sgr_mouse = 1; }
            action ResetSgrMouseEncoding { sgr_mouse = 0; }
            action SetCursorStyle { cursor_style = 6; }

            // SoftReset (DECSTR): return modes to their soft defaults — cursor
            // visible, autowrap on, everything else off; mouse/encoding/style cleared.
            action SoftReset {
                app_cursor_keys = 0;
                origin_mode = 0;
                auto_wrap = 1;
                cursor_visible = 1;
                focus_reporting = 0;
                sync_output = 0;
                insert_mode = 0;
                new_line_mode = 0;
                bracketed_paste = 0;
                mouse_mode = 0;
                sgr_mouse = 0;
                cursor_style = 0;
            }
            // FullReset (RIS): hard reset — everything to power-on defaults
            // (including leaving the alternate screen).
            action FullReset {
                app_cursor_keys = 0;
                origin_mode = 0;
                auto_wrap = 1;
                cursor_visible = 1;
                focus_reporting = 0;
                sync_output = 0;
                insert_mode = 0;
                new_line_mode = 0;
                alt_screen = 0;
                bracketed_paste = 0;
                mouse_mode = 0;
                sgr_mouse = 0;
                cursor_style = 0;
            }

            // Every mode stays in its valid domain under any action interleaving.
            invariant ModesValid:
                app_cursor_keys <= 1 && origin_mode <= 1 && auto_wrap <= 1
                && cursor_visible <= 1 && focus_reporting <= 1 && sync_output <= 1
                && insert_mode <= 1 && new_line_mode <= 1 && alt_screen <= 1
                && bracketed_paste <= 1 && mouse_mode <= 4 && sgr_mouse <= 1
                && cursor_style <= 6;
        }
    }
}

/// The bounded event-log ring as a derived model — the single source the spec in
/// `Evict.tla` hand-encodes, scalar-projected to `<<seq, lo>>`. `Push` advances
/// `seq` and evicts the oldest live event (`lo`) exactly when the live window
/// would exceed `Cap`. `MaxSeq` bounds the state space so `ty check` is
/// exhaustive + terminating; `Cap` is the ring capacity. Action name is `Push`
/// (not `Append`, which clashes with ty's Sequences builtin).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn ring_model() -> Model {
    Model {
        name: "Ring",
        consts: vec![("MaxSeq", 6), ("Cap", 3)],
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
                    // IF (seq + 1) - lo + 1 > Cap THEN lo + 1 ELSE lo
                    expr: if_(
                        gt(
                            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
                            cst("Cap"),
                        ),
                        add(var("lo"), int(1)),
                        var("lo"),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "LenBounded",
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
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_model() -> Model {
    Model {
        name: "Cursor",
        consts: vec![("MaxSeq", 4)],
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
                updates: vec![Update {
                    var: "cursor",
                    expr: var("seq"),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "CursorBounded",
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
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}

/// OBSERVATION KERNEL — NO-SILENT-LOSS LATCH (RFC "The Reactive Surface", L0).
/// The abstract twin of `aterm-core`'s [`WatcherSet`](../../aterm_core/terminal/observe)
/// no-silent-loss invariant, bound to the real engine by
/// `aterm-core/tests/conformance_observe.rs`.
///
/// A surface predicate can be **transiently** true — a row matched then
/// overwritten, a block completed then superseded — across two coalesced
/// consumer wakes. The kernel must latch the predicate AT THE SEAM where it
/// became true (`post_process`), not on the later, coalescing wake that sees
/// only the LATEST state. Scalar projection `<<truth, latched, lost>>`: `Rise`
/// makes the predicate true (the CORRECT kernel latches immediately; the buggy
/// one defers to a wake), `Fall` clears the transient (recording a silent loss
/// if it was never latched), `Wake` is the coalescing consumer that can latch
/// only while `truth` still holds.
///
/// `Buggy = 0` (committed): `Rise` latches at the seam, so `Fall` never loses and
/// `NoSilentLoss` holds. `Buggy = 1`: `Rise` defers, so a `Rise`→`Fall` with no
/// intervening `Wake` silently drops the event → `lost = 1`. Thus `ty` PROVES the
/// latch (Buggy=0) and CATCHES the coalescing-loss bug (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn watcher_latch_model() -> Model {
    Model {
        name: "WatcherLatch",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "truth",
                init: 0,
            },
            StateVar {
                name: "latched",
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
                name: "Rise", // predicate becomes true at a processed batch
                guard: Some(eq(var("truth"), int(0))),
                updates: vec![
                    Update {
                        var: "truth",
                        expr: int(1),
                    },
                    Update {
                        var: "latched",
                        // CORRECT (Buggy=0): latch AT THE SEAM. Buggy=1: defer.
                        expr: if_(eq(cst("Buggy"), int(0)), int(1), var("latched")),
                    },
                ], // lost UNCHANGED
            },
            Action {
                name: "Fall", // the transient clears
                guard: Some(eq(var("truth"), int(1))),
                updates: vec![
                    Update {
                        var: "truth",
                        expr: int(0),
                    },
                    Update {
                        var: "lost",
                        // never latched + buggy deferral => a true was silently lost
                        expr: if_(
                            and_(eq(cst("Buggy"), int(1)), eq(var("latched"), int(0))),
                            int(1),
                            var("lost"),
                        ),
                    },
                ], // latched UNCHANGED
            },
            Action {
                name: "Wake", // coalescing consumer: latches only while truth holds
                guard: Some(eq(var("truth"), int(1))),
                updates: vec![Update {
                    var: "latched",
                    expr: int(1),
                }], // truth, lost UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "NoSilentLoss",
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}

/// DAMAGE→PRESENT BOUNDED RESPONSE (the 2026-07-05 five-fps incident, proven
/// and caught). Abstract twin of the `Wake::Output` delivery spine: PTY damage
/// arrives (`Damage*`), the coalescing latch (`spawn::gated_output_wake`) posts
/// at most one in-flight wake (`inflight`), the handler pass presents and
/// clears (`Present` — `main.rs`'s `Wake::Output` arm + `redraw_window`), and a
/// wake can be LOST without a handler pass (`Lose` — the event dropped around
/// startup; bounded by `LostBudget`, losses are rare events). Time (`Tick`)
/// follows the house convention: it cannot pass a DUE heal (the enabled-action-
/// fires-before-the-deadline encoding that keeps liveness checkable as safety).
///
/// The invariant `DamageBounded` is the property the incident violated:
/// pending damage is presented within `Expiry + 1` ticks — never the
/// process-lifetime starvation (presents pinned to the 5 Hz focus timers) the
/// user saw.
///
/// `Buggy = 0` (committed): the latch SELF-EXPIRES via TWO code paths that
/// together make the state-enabled `DamageHeals` faithful — the next output
/// burst re-arms a stale arm (`spawn::gated_output_wake`, streaming case), and
/// the `about_to_wait` WATCHDOG heals a stale arm at its expiry instant even
/// with no further output (final-burst case; it folds the expiry into the
/// event-loop wait, which is what licenses this model's Tick being blocked at
/// a due heal). Both use exactly `WAKE_LATCH_EXPIRY_NS`. `ty` PROVES the bound
/// (tight: the worst case reaches exactly `Expiry + 1`); the guard algebra is
/// Tier-1-bound to the real latch by
/// `spawn::latch_conforms_to_damage_to_present_model` (with a one-shot
/// negative control). `Buggy = 1`: the original
/// one-shot latch — a stale arm coalesces FOREVER (`DamageCoalesced`'s
/// `Buggy = 1` disjunct), no heal exists, and `ty` finds the counterexample:
/// Damage → Lose → Tick* → pending damage older than the bound, i.e. the
/// shipped defect.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn damage_to_present_model() -> Model {
    // now - latch > Expiry, with the latch armed and no wake in flight: the
    // stale-arm state the self-expiry heals and the one-shot latch never leaves.
    let stale = and_(
        neq(var("latch"), int(0)),
        and_(
            eq(var("inflight"), int(0)),
            gt(sub(var("now"), var("latch")), cst("Expiry")),
        ),
    );
    Model {
        name: "DamageToPresent",
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
            StateVar {
                name: "pending",
                init: 0,
            },
            StateVar {
                name: "damageAt",
                init: 0,
            },
            StateVar {
                name: "latch",
                init: 0,
            },
            StateVar {
                name: "inflight",
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
                // First damage of an episode: arm the latch, wake in flight.
                name: "DamageFresh",
                guard: Some(eq(var("latch"), int(0))),
                updates: vec![
                    Update {
                        var: "pending",
                        expr: int(1),
                    },
                    Update {
                        var: "damageAt",
                        // Keep the OLDEST unpresented damage (the honest bound).
                        expr: if_(eq(var("pending"), int(1)), var("damageAt"), var("now")),
                    },
                    Update {
                        var: "latch",
                        expr: var("now"),
                    },
                    Update {
                        var: "inflight",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // Later damage while armed-and-fresh (or armed-and-in-flight)
                // coalesces — no new wake. Buggy=1 ALSO coalesces on a stale arm:
                // the one-shot latch suppressing re-sends forever.
                name: "DamageCoalesced",
                guard: Some(and_(
                    neq(var("latch"), int(0)),
                    or_(
                        eq(var("inflight"), int(1)),
                        or_(
                            le(sub(var("now"), var("latch")), cst("Expiry")),
                            eq(cst("Buggy"), int(1)),
                        ),
                    ),
                )),
                updates: vec![
                    Update {
                        var: "pending",
                        expr: int(1),
                    },
                    Update {
                        var: "damageAt",
                        expr: if_(eq(var("pending"), int(1)), var("damageAt"), var("now")),
                    },
                ],
            },
            Action {
                // THE FIX (Buggy=0 only): a burst arriving on a STALE arm re-arms
                // and re-sends — `gated_output_wake`'s self-expiry.
                name: "DamageHeals",
                guard: Some(and_(eq(cst("Buggy"), int(0)), stale.clone())),
                updates: vec![
                    Update {
                        var: "pending",
                        expr: int(1),
                    },
                    Update {
                        var: "damageAt",
                        expr: if_(eq(var("pending"), int(1)), var("damageAt"), var("now")),
                    },
                    Update {
                        var: "latch",
                        expr: var("now"),
                    },
                    Update {
                        var: "inflight",
                        expr: int(1),
                    },
                ],
            },
            Action {
                // The in-flight wake is DROPPED without a handler pass (the
                // incident's trigger). The latch stays armed — that is the bug
                // surface. Rare: bounded by LostBudget.
                name: "Lose",
                guard: Some(and_(
                    eq(var("inflight"), int(1)),
                    le(add(var("lost"), int(1)), cst("LostBudget")),
                )),
                updates: vec![
                    Update {
                        var: "inflight",
                        expr: int(0),
                    },
                    Update {
                        var: "lost",
                        expr: add(var("lost"), int(1)),
                    },
                ],
            },
            Action {
                // Handler pass: clear the latch, present the accumulated damage.
                name: "Present",
                guard: Some(eq(var("inflight"), int(1))),
                updates: vec![
                    Update {
                        var: "pending",
                        expr: int(0),
                    },
                    Update {
                        var: "damageAt",
                        expr: int(0),
                    },
                    Update {
                        var: "latch",
                        expr: int(0),
                    },
                    Update {
                        var: "inflight",
                        expr: int(0),
                    },
                ],
            },
            Action {
                // Time passes — but never past a wake in flight (handler passes
                // are sub-tick) and never past a DUE heal (Buggy=0), the standard
                // guarded-Tick encoding of "the enabled action fires in time".
                // Under Buggy=1 no heal exists, so time runs freely past the
                // stale arm — exposing the starvation to the invariant.
                name: "Tick",
                guard: Some(and_(
                    gt(cst("MaxTime"), var("now")),
                    and_(
                        eq(var("inflight"), int(0)),
                        or_(
                            eq(cst("Buggy"), int(1)),
                            or_(
                                eq(var("pending"), int(0)),
                                // ¬stale (under inflight=0): the arm is clear or
                                // still fresh — a due heal blocks the tick.
                                or_(
                                    eq(var("latch"), int(0)),
                                    le(sub(var("now"), var("latch")), cst("Expiry")),
                                ),
                            ),
                        ),
                    ),
                )),
                updates: vec![Update {
                    var: "now",
                    expr: add(var("now"), int(1)),
                }],
            },
        ],
        invariants: vec![Invariant {
            // Pending damage is presented within Expiry+1 ticks — the bounded
            // response the 5 fps incident violated (its bound was ∞).
            name: "DamageBounded",
            expr: or_(
                eq(var("pending"), int(0)),
                le(sub(var("now"), var("damageAt")), add(cst("Expiry"), int(1))),
            ),
        }],
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
    crate::ty_model! {
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

            invariant FrameFitsSurface: frame <= Surface;
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

/// The first successful present publishes a valid phase partition only after
/// all eight exclusive Rust-main → present milestones have been observed in
/// order. The shipping GUI binds each `Step` to one adjacent timestamp interval
/// and enables `Publish` only when `derive_startup_phases` validates the exact
/// sum. `Buggy=1` admits an early valid publication so Tier-0 must produce a
/// counterexample rather than accepting a vacuous happy trace.
pub fn startup_phase_publication_model() -> Model {
    crate::ty_model! {
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

            invariant PhaseBounded: phase <= PhaseCount;
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
    crate::ty_model! {
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
                retry = if remaining > 0 { 1 } else { 0 };
                parked = if remaining > 0 { 0 } else { 1 };
                outstanding = 0;
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
                retry = 0;
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

            invariant Bounds:
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
    crate::ty_model! {
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

            invariant RouteRange: route <= 2;
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
/// Correct (`Buggy=0`) covers fresh, fuelled, and exhausted entry paths. The
/// historical mutant leaves a GPU recording alive, omits the fresh retry, and
/// double-counts the failed-present path; each defect is independently rejected.
pub fn gpu_loss_recovery_model() -> Model {
    crate::ty_model! {
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
                retry = if Buggy == 1 { 0 } else { 1 };
                delivered = 0;
                exhausted = 0;
                parked = if Buggy == 1 { 1 } else { 0 };
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = 1;
                reason = 2;
                recording = if Buggy == 1 { 1 } else { 0 };
            }
            action FailFallbackAfterDropWithFuel {
                path = 2;
                fallback_failed = 1;
                retry = if Buggy == 1 { 0 } else { 1 };
                delivered = 0;
                exhausted = 0;
                parked = if Buggy == 1 { 1 } else { 0 };
                cpu_ready = 0;
                requested = 0;
                cpu_presented = 0;
                drops = if Buggy == 1 { 2 } else { 1 };
                reason = 2;
                recording = if Buggy == 1 { 1 } else { 0 };
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
                drops = if Buggy == 1 { 2 } else { 1 };
                reason = 2;
                recording = if Buggy == 1 { 1 } else { 0 };
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
                recording = if Buggy == 1 { 1 } else { 0 };
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
                recording = if Buggy == 1 { 1 } else { 0 };
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

            invariant Bounds:
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
            invariant CpuPresentCompletesFailure:
                if cpu_presented == 1 { fallback_failed == 0 } else { 1 == 1 };
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
/// request. The historical mutant clears it on the first stimulus, reproducing
/// the frozen no-echo/app-owned-input window after a second suppressed edge.
pub fn recovery_redraw_model() -> Model {
    crate::ty_model! {
        RecoveryRedraw {
            const Buggy = 0;
            var unresolved = 1;
            var stimulated = 0;
            var requested = 0;
            var suppressed = 0;
            var presented = 0;

            action Stimulus when (unresolved == 1) {
                unresolved = if Buggy == 1 { 0 } else { 1 };
                stimulated = 1;
                requested = 1;
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

            invariant Bounds:
                unresolved <= 1 && stimulated <= 1 && requested <= 1 &&
                suppressed <= 1 && presented <= 1;
            invariant RecoveryStimulusRequestsRedraw:
                if stimulated == 1 { requested == 1 } else { 1 == 1 };
            invariant SuppressedRequestRemainsUnresolved:
                if suppressed == 1 { unresolved == 1 } else { 1 == 1 };
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
/// comparator, and this model must say so: `ConfirmFast` used to set `slow = 0`
/// outright, which is the SINGLE-SAMPLE close the implementation deliberately
/// replaced (`FAST_SAMPLES_TO_HIDE` consecutive decisive fast turns, a smoothed
/// estimate that agrees, and an EMPTY pending set). The old abstraction only
/// looked sound because the conformance test fired `ConfirmFast` at an already
/// closed latch — so the one property the model exists to pin, "speculation
/// never blinks off on one lucky turn", was unpinned. `fast_streak` counts the
/// decisive fast turns (capped at `Hide`) and `retracted` marks the STEP on
/// which the gate went 1 -> 0, because the defect is a retraction taken on thin
/// evidence, not a steady state: `RetractOnlyOnSustainedFastEvidence` then makes
/// the single-sample close (and any close with pixels still in flight — the
/// blink itself) an outright counterexample. `ConfirmFastInFlight` is the same
/// fast turn taken while type-ahead is still on glass; at `Buggy=0` it may
/// advance the streak but never close, which is what keeps that clause of the
/// invariant non-vacuous.
pub fn predictive_echo_visibility_model() -> Model {
    crate::ty_model! {
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

            action ConfirmFast when (app_owned == 0 && pending == 0) {
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
            action ConfirmFastInFlight when (app_owned == 0 && pending == 1) {
                fast_streak = if fast_streak <= Hide - 1 { fast_streak + 1 } else { Hide };
                slow = if Buggy == 1 { 0 } else { slow };
                retracted = if Buggy == 1 { slow } else { 0 };
                visible = if Buggy == 1 { 0 } else { visible };
                erased = if Buggy == 1 { visible } else { 0 };
                fresh = 0;
                confirmed = 1;
            }
            action ConfirmSlow when (app_owned == 0 && pending == 0) {
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
            action Expire when (pending == 1) {
                pending = 0;
                erased = visible;
                visible = 0;
                retracted = 0;
            }
            action Echo when (pending == 1) {
                pending = 0;
                visible = 0;
                erased = 0;
                retracted = 0;
            }
            action EnterComposer {
                app_owned = 1;
                pending = if Buggy == 1 { pending } else { 0 };
                visible = if Buggy == 1 { visible } else { 0 };
                confirmed = if Buggy == 1 { confirmed } else { 0 };
                erased = 0;
                retracted = 0;
            }
            action LeaveComposer {
                app_owned = 0;
                pending = 0;
                visible = 0;
                confirmed = 0;
                erased = 0;
                retracted = 0;
            }
            action Submit {
                pending = 0;
                visible = 0;
                confirmed = 0;
                erased = 0;
                retracted = 0;
            }
            action SwitchSession {
                app_owned = 0;
                slow = if Buggy == 1 { slow } else { 0 };
                fresh = 1;
                confirmed = 0;
                pending = 0;
                visible = 0;
                erased = 0;
                // Dropping the estimate is not a RETRACTION: nothing was measured
                // that says this link is fast, so the streak restarts at zero and
                // the step carries no evidence obligation (`FreshSessionHasNoInheritedRtt`
                // is what covers this direction).
                fast_streak = 0;
                retracted = 0;
            }

            invariant Bounds:
                app_owned <= 1 && slow <= 1 && confirmed <= 1 &&
                pending <= 1 && visible <= 1 && erased <= 1 && fresh <= 1 &&
                fast_streak <= Hide && retracted <= 1;
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
        }
    }
}

/// SELF-REFLECTION FEEDBACK GOVERNOR — FAIL-CLOSED (RFC "The Reactive Surface",
/// R4 / L2). The abstract twin of `aterm-agent`'s
/// [`SelfGovernor`](../../aterm_agent/struct.SelfGovernor.html): once the
/// circuit-breaker trips on sustained self-induced churn, NO self-write may
/// proceed — the storm backstop that `await-idle` alone cannot provide (a
/// self-write that produces output keeps `content_seq` advancing, so quiescence
/// never settles). This models the BREAKER condition of the real
/// `SelfGovernor::allow_self_write` gate — the latching, hardest-to-reason-about
/// one; the gate's other two fail-closed conditions (self-write disabled, or the
/// token bucket empty) are non-latching and covered by `SelfGovernor`'s unit
/// tests. Scalar projection `<<tripped, wrote_while_tripped>>`: `Trip` latches the
/// breaker; `Write` proceeds only while NOT tripped (the correct gate) and records
/// a violation if it ever fires while tripped.
///
/// `Buggy = 0` (committed): `Write` is guarded on `tripped = 0`, so a write never
/// happens after a trip and `FailClosed` holds. `Buggy = 1`: the guard drops, so
/// a `Trip`→`Write` lets a self-write through the tripped breaker →
/// `wrote_while_tripped = 1`. Thus `ty` PROVES the backstop (Buggy=0) and CATCHES
/// the breaker-bypass bug (Buggy=1 → counterexample). This is the `edge_gate`
/// FailClosed shape (`decision <= granted`).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn self_governor_model() -> Model {
    Model {
        name: "SelfGovernor",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "tripped",
                init: 0,
            },
            StateVar {
                name: "wrote_while_tripped",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Trip", // sustained self-churn trips the breaker (latching)
                guard: Some(eq(var("tripped"), int(0))),
                updates: vec![Update {
                    var: "tripped",
                    expr: int(1),
                }], // wrote_while_tripped UNCHANGED
            },
            Action {
                name: "Write", // a self-write attempt
                // CORRECT (Buggy=0): only when NOT tripped. Buggy=1: drop the gate.
                guard: Some(or_(eq(cst("Buggy"), int(1)), eq(var("tripped"), int(0)))),
                updates: vec![Update {
                    var: "wrote_while_tripped",
                    // a write that fired while tripped is a fail-OPEN violation
                    expr: if_(
                        eq(var("tripped"), int(1)),
                        int(1),
                        var("wrote_while_tripped"),
                    ),
                }], // tripped UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "FailClosed",
            expr: eq(var("wrote_while_tripped"), int(0)), // no write survived a trip
        }],
    }
}

/// SELF-FEED FLOOR — NO-OVERDRAFT (RFC D3). The abstract twin of `aterm-gui`'s
/// [`inject_floor`](../../aterm_gui/inject_floor) token bucket: the un-bypassable
/// control-layer backstop that bounds self-targeted input injection so a raw
/// client cannot drive a feedback storm. Scalar projection `<<tokens, over>>`
/// over a bucket of capacity `Cap`: `Refill` adds a token (capped); `Write`
/// admits an injection only with a spare token (the correct gate) and records an
/// overdraft if it ever admits at zero.
///
/// `Buggy = 0` (committed): `Write` is guarded on `tokens > 0`, so it never
/// overdraws and `NoOverdraft` holds (and `tokens <= Cap` from the capped
/// refill). `Buggy = 1`: the guard drops, so a `Write` at `tokens = 0` injects
/// past the floor → `over = 1`. `ty` PROVES the bound (Buggy=0) and CATCHES the
/// overdraft bug (Buggy=1 → counterexample). The bounded-ring / token-bucket
/// shape (`ring_model` family).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn inject_floor_model() -> Model {
    Model {
        name: "InjectFloor",
        consts: vec![("Cap", 2), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "tokens",
                init: 2,
            }, // starts full (= Cap)
            StateVar {
                name: "over",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Refill", // continuous refill, capped at Cap
                guard: Some(le(var("tokens"), sub(cst("Cap"), int(1)))),
                updates: vec![Update {
                    var: "tokens",
                    expr: add(var("tokens"), int(1)),
                }], // over UNCHANGED
            },
            Action {
                name: "Write", // a self-targeted injection attempt
                // CORRECT (Buggy=0): only with a spare token. Buggy=1: drop the gate.
                guard: Some(or_(eq(cst("Buggy"), int(1)), gt(var("tokens"), int(0)))),
                updates: vec![
                    Update {
                        var: "tokens",
                        expr: if_(
                            gt(var("tokens"), int(0)),
                            sub(var("tokens"), int(1)),
                            var("tokens"),
                        ),
                    },
                    Update {
                        var: "over",
                        // admitted at zero tokens => overdraft (floor bypassed)
                        expr: if_(eq(var("tokens"), int(0)), int(1), var("over")),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "NoOverdraft",
                expr: eq(var("over"), int(0)), // never admitted past an empty bucket
            },
            Invariant {
                name: "BoundedTokens",
                expr: le(var("tokens"), cst("Cap")), // the bucket never exceeds Cap
            },
        ],
    }
}

/// **No-mint-reachability** (`ATERM_DESIGN §5.4`, `AUDIT.md:186-201`). An UNTRUSTED
/// actor — a parser / control handler / extension reached from a PTY, socket, or
/// extension boundary — must never reach `Top` authority; i.e. NO path from
/// untrusted input reaches a capability MINT. The trusted launcher mints `Top`
/// exactly once at process entry; untrusted code may only `Receive` an explicitly
/// delegated, scoped capability STRICTLY BELOW `Top` (a grant, never the mint).
///
/// The `Buggy` convention encodes the pre-§5.4 reality: at `Buggy = 1` the mint is
/// reachable from untrusted code (any `unsafe { Authority::root_authority() }` an
/// untrusted path could execute), and `ty` must drive the untrusted actor to `Top`,
/// violating `NoUntrustedTop`. At `Buggy = 0` the mint is launcher-only and the
/// invariant holds. Bound to real code by the `mint_sites_are_launcher_only`
/// source-scan conformance test (Tier-1): the sealed constructor is named in exactly
/// one product location and no engine crate can reach it.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn mint_reachability_model() -> Model {
    Model {
        name: "MintReachability",
        // Top = the mint level: the authority that can `grant` ANY capability.
        consts: vec![("Top", 2), ("Buggy", 0)],
        vars: vec![StateVar {
            // the untrusted actor's authority: 0=None, 1=Trusted(delegated), 2=Top(minted)
            name: "untrusted",
            init: 0,
        }],
        fn_vars: vec![],
        actions: vec![
            Action {
                // untrusted RECEIVES an explicitly-delegated scoped cap — capped BELOW Top
                name: "Receive",
                guard: Some(eq(var("untrusted"), int(0))),
                updates: vec![Update {
                    var: "untrusted",
                    expr: sub(cst("Top"), int(1)), // Trusted = Top - 1 (a grant is never a mint)
                }],
            },
            Action {
                // reaching Top authority = constructing a root Authority (the MINT).
                name: "Mint",
                // CORRECT (Buggy=0): launcher-only — NEVER enabled for the untrusted
                // actor. Buggy=1: the mint is reachable from untrusted code (the
                // pre-§5.4 `unsafe fn root_authority()` any in-process path could call).
                guard: Some(eq(cst("Buggy"), int(1))),
                updates: vec![Update {
                    var: "untrusted",
                    expr: cst("Top"),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "NoUntrustedTop",
            // untrusted authority never reaches the mint level
            expr: le(var("untrusted"), sub(cst("Top"), int(1))),
        }],
    }
}

/// NETWORK CAPABILITY — CHANNEL-BOUND, NO REPLAY (RFC D4 / L3). The abstract twin
/// of `aterm-net`'s [`channel_bind`](../../aterm_net/fn.channel_bind.html): an
/// edge token captured on one connection must NOT authorize on another. The local
/// fabric's same-uid `SO_PEERCRED` check has no network analog, so the token is
/// bound to the connection's exporter (`presented = H(token, exporter)`); a
/// replay on a different channel presents a value computed over the WRONG
/// exporter. Scalar projection `<<captured, accepted_replay>>`: `Capture` records
/// the channel-A presented value an adversary observed; `ReplayOnB` presents it on
/// channel B.
///
/// `Buggy = 0` (committed): the verifier checks the binding against the CURRENT
/// channel, so the cross-channel replay is rejected and `accepted_replay` stays 0.
/// `Buggy = 1`: the verifier ignores the channel (accepts a bare token), so the
/// replay succeeds → `accepted_replay = 1`. `ty` PROVES no-replay (Buggy=0) and
/// CATCHES the channel-unbound bug (Buggy=1 → counterexample).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn channel_bind_model() -> Model {
    Model {
        name: "ChannelBind",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "captured",
                init: 0,
            },
            StateVar {
                name: "accepted_replay",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Capture", // adversary records channel-A's presented value
                guard: Some(eq(var("captured"), int(0))),
                updates: vec![Update {
                    var: "captured",
                    expr: int(1),
                }], // accepted_replay UNCHANGED
            },
            Action {
                name: "ReplayOnB", // present the captured A-value on channel B
                guard: Some(eq(var("captured"), int(1))),
                updates: vec![Update {
                    var: "accepted_replay",
                    // channel-bound verifier (Buggy=0) rejects; unbound (Buggy=1) accepts
                    expr: if_(eq(cst("Buggy"), int(1)), int(1), var("accepted_replay")),
                }], // captured UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "NoReplay",
            expr: eq(var("accepted_replay"), int(0)), // a cross-channel replay never authorizes
        }],
    }
}

/// NETWORK-CAPABILITY GRANT SOUNDNESS — the L3 listener's `verify_capability`
/// decision (`aterm-net`): a dialer is GRANTED only when BOTH conjuncts hold — the
/// presented `(src, op)` names a capability the host minted (`lookup` returns a
/// token, not `None`), AND the presented tag is the channel-bound
/// `HMAC-SHA256(token, exporter)` for THIS TLS session (`verify_presented`
/// succeeds in constant time). The real-code binding is aterm-net's
/// `capability_handshake_grants_valid_and_denies_unknown_replay_and_forgery` test
/// (valid → grant; unknown / cross-channel replay / wrong-token forgery → deny).
///
/// Where [`channel_bind_model`] proves the binding PRIMITIVE (a captured tag never
/// replays on another channel), this proves the GRANT decision that consumes it:
/// the two are the primitive and its caller. Modeled as a two-guard
/// [`props::conjunctive_authz`]: `known` = the lookup hit, `bound` = the
/// channel-binding HMAC verified. `Buggy` WAIVES the `bound` guard — the exact
/// "grant a known `(src, op)` without actually checking the HMAC over the session
/// exporter" regression, which would accept a forged or cross-channel-replayed
/// tag. Invariant `GrantImpliesKnownAndBound`: a grant implies BOTH the lookup hit
/// AND the binding verified. `ty` proves it at Buggy=0 and catches the
/// dropped-binding disclosure at Buggy=1.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn net_capability_grant_model() -> Model {
    props::conjunctive_authz(props::ConjunctiveAuthz {
        name: "NetCapabilityGrant",
        guards: vec!["known", "bound"],
        decided: "granted",
        pick: "Present",
        decide: "Verify",
        drop: "bound",
        inv: "GrantImpliesKnownAndBound",
    })
}

/// NETWORK DIAL-AFTER-GRANT ORDERING — the L3 listener dials its LOCAL control
/// socket only AFTER the channel-bound capability is granted (`aterm-net`'s
/// `accept_and_relay`: `verify_capability(...)?` runs to a grant BEFORE
/// `connect_local()`). So a denied dialer NEVER reaches the local socket — the
/// confused-deputy boundary at the network edge, the network twin of
/// [`no_transitive_authority_model`]. The real-code binding is aterm-net's
/// `a_denied_capability_never_reaches_the_local_socket` test (a forged capability
/// is rejected AND `connect_local` is never called).
///
/// Modeled as a [`props::happens_before`] latch pair: `granted` (set by `Verify`)
/// must precede `local_dialed` (set by `DialLocal`, guarded on `granted`). `Buggy`
/// lets the dial race ahead of the grant — dialing the local socket for an
/// unverified peer. Invariant `DialImpliesGranted`: `local_dialed = 0 \/ granted =
/// 1`. `ty` proves it at Buggy=0 and catches the premature dial at Buggy=1.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn net_dial_after_grant_model() -> Model {
    props::happens_before(props::Ordering {
        name: "NetDialAfterGrant",
        a: "granted",
        a_act: "Verify",
        b: "local_dialed",
        b_act: "DialLocal",
        inv: "DialImpliesGranted",
    })
}
