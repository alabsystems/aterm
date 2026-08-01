// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Session, kernel, fd-lifecycle, recording, and routing models — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// SPAWN-TIME LOCALE GUARANTEE — the child process aterm launches must run under a
/// UTF-8 `LC_CTYPE` whatever locale aterm inherited. `LC_CTYPE` is the POSIX
/// character-encoding category; under a non-UTF-8 one, locale-aware programs (emacs,
/// vim, python) re-encode multibyte terminal output to the ASCII codeset and emit a
/// literal `?` per character — the box-drawing-`?` bug. The real decision is
/// `aterm_pty::resolve_spawn_locale` (POSIX precedence `LC_ALL > LC_CTYPE > LANG`,
/// empty == unset); this is its abstract twin, with the real-code binding in
/// aterm-pty's `spawn_locale_conformance_*` test.
///
/// Scalar projection `<<present, eff_utf8, ctype, resolved>>`: `present` = any locale
/// var is set non-empty; `eff_utf8` = the effective inherited encoding is already
/// UTF-8; `ctype` = the child's resulting `LC_CTYPE` (1 = UTF-8) once `resolved`. The
/// two `Observe*` actions spread the nondeterministic input shape — nothing set; a
/// non-UTF-8 locale present; a UTF-8 locale present — and `Resolve` runs the fix once.
///
/// `Buggy` gates the SHIPPED defect: with `Buggy = 0` (committed) `Resolve` always
/// yields `ctype = 1`; with `Buggy = 1` it forces UTF-8 ONLY when nothing is present
/// (the old all-unset guard), so a present-but-non-UTF-8 locale leaves `ctype = 0`
/// and `ChildHasUtf8Ctype` is violated. Thus `ty` PROVES the guarantee (Buggy=0) and
/// CATCHES the real regression (Buggy=1 → counterexample). Exercises a nested `if`,
/// a constant-guarded update, and disjunction.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn spawn_locale_model() -> Model {
    Model {
        name: "SpawnLocale",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "present",
                init: 0,
            },
            StateVar {
                name: "eff_utf8",
                init: 0,
            },
            StateVar {
                name: "ctype",
                init: 0,
            },
            StateVar {
                name: "resolved",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "ObserveNonUtf8", // a non-UTF-8 locale is present (e.g. LANG=C)
                guard: Some(and_(
                    eq(var("resolved"), int(0)),
                    eq(var("present"), int(0)),
                )),
                updates: vec![Update {
                    var: "present",
                    expr: int(1),
                }], // eff_utf8 stays 0
            },
            Action {
                name: "ObserveUtf8", // a UTF-8 locale is present
                guard: Some(and_(
                    eq(var("resolved"), int(0)),
                    eq(var("present"), int(0)),
                )),
                updates: vec![
                    Update {
                        var: "present",
                        expr: int(1),
                    },
                    Update {
                        var: "eff_utf8",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Resolve", // run resolve_spawn_locale once; sets the child's LC_CTYPE
                guard: Some(eq(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
                    // Buggy=1: IF present=0 THEN 1 ELSE eff_utf8 (old all-unset guard
                    // leaves a present non-UTF-8 locale unfixed). Buggy=0: always 1.
                    Update {
                        var: "ctype",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            if_(eq(var("present"), int(0)), int(1), var("eff_utf8")),
                            int(1),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "ChildHasUtf8Ctype",
            // resolved => ctype = 1
            expr: or_(eq(var("resolved"), int(0)), eq(var("ctype"), int(1))),
        }],
    }
}

/// A fifth derived model — TRANSACTION ATOMICITY / no-lost-update under
/// optimistic concurrency (the `Transact` kernel-family property). A transaction
/// reads the head at `tbase` (`Begin`); concurrent `Write`s may advance `seq`. At
/// commit, the correct discipline is: commit only if no write intervened
/// (`seq = tbase`), otherwise ABORT — committing against a stale `tbase` would
/// clobber the concurrent write (a lost update). Scalar projection over
/// `<<seq, tbase, active, lost>>`, edits-per-txn `K`.
///
/// `Buggy` gates the bad path: with `Buggy = 0` (committed) a conflict can only
/// `Abort`, so `lost` stays 0; with `Buggy = 1` the txn may commit despite a
/// conflict (`seq' = tbase + K`, overwriting the intervening write) and sets
/// `lost = 1`. So `ty` proves `NoLostUpdate` (Buggy=0) and catches it (Buggy=1).
/// Exercises the `Expr` conjunction (`/\`) operator in guards.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn transact_model() -> Model {
    Model {
        name: "Transact",
        consts: vec![("MaxSeq", 4), ("K", 2), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "tbase",
                init: 0,
            },
            StateVar {
                name: "active",
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
                name: "Write", // a concurrent writer advances the committed head
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                }],
            },
            Action {
                name: "Begin", // a txn reads the current head as its base version
                guard: Some(eq(var("active"), int(0))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: int(1),
                    },
                    Update {
                        var: "tbase",
                        expr: var("seq"),
                    },
                ],
            },
            Action {
                name: "CommitClean", // no write intervened: commit K edits atomically
                guard: Some(and_(
                    and_(eq(var("active"), int(1)), eq(var("seq"), var("tbase"))),
                    le(var("seq"), sub(cst("MaxSeq"), cst("K"))),
                )),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), cst("K")),
                    },
                    Update {
                        var: "active",
                        expr: int(0),
                    },
                ],
            },
            Action {
                name: "Abort", // a write intervened (seq > tbase): correct path aborts
                guard: Some(and_(
                    eq(var("active"), int(1)),
                    gt(var("seq"), var("tbase")),
                )),
                updates: vec![Update {
                    var: "active",
                    expr: int(0),
                }],
            },
            Action {
                name: "BuggyCommit", // conflict committed anyway -> clobbers, lost update
                guard: Some(and_(
                    and_(
                        and_(eq(var("active"), int(1)), gt(var("seq"), var("tbase"))),
                        eq(cst("Buggy"), int(1)),
                    ),
                    le(var("tbase"), sub(cst("MaxSeq"), cst("K"))),
                )),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("tbase"), cst("K")),
                    },
                    Update {
                        var: "active",
                        expr: int(0),
                    },
                    Update {
                        var: "lost",
                        expr: int(1),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoLostUpdate",
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}

/// A fifth derived model — the event-log SPINE: gap-free, monotone, `seq == count`
/// (the `Kernel` family property). Each `Append` assigns the next contiguous seq
/// and bumps the count, so the head seq always equals the number of events — no
/// gaps, no duplicates. `Buggy` makes an append jump seq by 2 (a gap), so
/// `seq != count` and `SeqIsCount` is violated. ty proves it (Buggy=0) and catches
/// the gap (Buggy=1).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn kernel_model() -> Model {
    Model {
        name: "Kernel",
        consts: vec![("MaxSeq", 5), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "count",
                init: 0,
            },
        ],
        // Action `Emit` (not `Append`, which clashes with ty's Sequences builtin in
        // a single-action spec — see the ring's `Push`).
        fn_vars: vec![],
        actions: vec![Action {
            name: "Emit",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
            updates: vec![
                Update {
                    var: "count",
                    expr: add(var("count"), int(1)),
                },
                // seq' = IF Buggy = 1 THEN seq + 2 ELSE seq + 1   (Buggy opens a gap)
                Update {
                    var: "seq",
                    expr: if_(
                        eq(cst("Buggy"), int(1)),
                        add(var("seq"), int(2)),
                        add(var("seq"), int(1)),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "SeqIsCount",
            expr: eq(var("seq"), var("count")), // seq = count (gap-free, monotone spine)
        }],
    }
}

/// PROOF-CARRYING DYNAMIC SOFTWARE UPDATE — the QUIESCENCE-SAFETY obligation, Rung 0
/// of `docs/RFC-proof-carrying-dsu.md`.
///
/// The crux of applying an update to a RUNNING process (rather than at the next cold
/// launch): a live in-flight computation was begun under the OLD version's memory
/// layout; resuming it under the NEW version tears state. So an update may be applied
/// ONLY at a QUIESCENCE point — no request in flight. This model proves that a
/// quiescence-GATED apply never tears, and that dropping the gate (the `Buggy` mutant
/// = apply-mid-flight) is caught by the checker — so the safety precondition Trust
/// must discharge for any real DSU is itself formally pinned, not asserted in prose.
///
/// Scalar projection over `<<inflight, applied, torn, served>>`:
/// * `Begin`  — a request starts under the current version (`inflight := 1`), bounded
///   by `served <= MaxReq - 1`.
/// * `Finish` — the request completes cleanly (`inflight := 0`, `served += 1`).
/// * `ApplyQuiescent` — apply the update at a quiescence point (`applied = 0 /\
///   inflight = 0`): the transformer runs on SETTLED state, so nothing tears.
/// * `BuggyApplyInflight` — (`Buggy = 1`) apply while a request is in flight: the
///   in-flight computation, begun under the old layout, is now under new code → a
///   TEAR (`torn := 1`).
///
/// What is proven (the `Buggy`-convention shape shared with `transact_model` etc.):
/// `torn` is set ONLY by the mid-flight apply, so `NoTear` (`torn = 0`) holds over the
/// whole state space at `Buggy = 0` because the quiescence-gated `ApplyQuiescent` never
/// reaches an in-flight state — and `assert_proves_and_catches` REQUIRES a
/// counterexample at `Buggy = 1`, where the reachable `BuggyApplyInflight` tears state.
/// So the model proves the safety property that the mid-flight apply is FORBIDDEN (a
/// reachable defect the checker catches), not a liveness claim that requests complete.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn dsu_quiescence_model() -> Model {
    Model {
        name: "DsuQuiescence",
        consts: vec![("MaxReq", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "inflight", // a request is mid-processing under the current version
                init: 0,
            },
            StateVar {
                name: "applied", // the dynamic update has been applied
                init: 0,
            },
            StateVar {
                name: "torn", // SAFETY violation: an in-flight computation survived an apply
                init: 0,
            },
            StateVar {
                name: "served", // completed request cycles (bounds the state space)
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Begin", // start a request under the current version
                guard: Some(and_(
                    eq(var("inflight"), int(0)),
                    le(var("served"), sub(cst("MaxReq"), int(1))),
                )),
                updates: vec![Update {
                    var: "inflight",
                    expr: int(1),
                }],
            },
            Action {
                name: "Finish", // the request completes cleanly
                guard: Some(eq(var("inflight"), int(1))),
                updates: vec![
                    Update {
                        var: "inflight",
                        expr: int(0),
                    },
                    Update {
                        var: "served",
                        expr: add(var("served"), int(1)),
                    },
                ],
            },
            Action {
                name: "ApplyQuiescent", // apply ONLY at quiescence (no request in flight)
                guard: Some(and_(
                    eq(var("applied"), int(0)),
                    eq(var("inflight"), int(0)),
                )),
                updates: vec![Update {
                    var: "applied",
                    expr: int(1),
                }],
            },
            Action {
                name: "BuggyApplyInflight", // apply mid-flight -> tears the in-flight request
                guard: Some(and_(
                    and_(eq(var("applied"), int(0)), eq(var("inflight"), int(1))),
                    eq(cst("Buggy"), int(1)),
                )),
                updates: vec![
                    Update {
                        var: "applied",
                        expr: int(1),
                    },
                    Update {
                        var: "torn",
                        expr: int(1),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoTear",
            expr: eq(var("torn"), int(0)), // torn = 0
        }],
    }
}

/// PROOF-CARRYING DSU — the SNAPSHOT ROUND-TRIP obligation, Rung 1a of
/// `docs/RFC-proof-carrying-dsu.md`.
///
/// Strategy A (seamless re-exec) hands the session set to the new binary as a
/// serialized manifest; the new process must restore EXACTLY that set — no session
/// lost, none fabricated — or the update silently drops (or duplicates) a tab. This
/// models the round-trip `restore ∘ pack` as an identity on the session count:
/// `Pack` adds a session to the manifest; `Restore` faithfully re-materializes one;
/// the `Buggy` mutant (a transformer that drops a session across the handoff) sets
/// `lost`. `ty` checks the SAFETY invariant `NoLossNoFabricate` (`lost = 0 /\ restored
/// <= packed`): the no-fabricate half (`restored <= packed`) is a genuine non-vacuous
/// bound, and `lost = 0` holds at Buggy=0 because only `BuggyDrop` sets it — so, in the
/// repo's `Buggy`-convention, the model proves a session-dropping TRANSITION is
/// FORBIDDEN (caught at Buggy=1), not the liveness completion `restored == packed`. The
/// concrete no-loss round-trip is the REAL serde identity check
/// (`session_handoff_roundtrips_the_whole_set`, `session_store.rs`), which is what pins
/// the shipping serializer.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn handoff_roundtrip_model() -> Model {
    Model {
        name: "HandoffRoundTrip",
        consts: vec![("N", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "packed", // sessions written into the handoff manifest
                init: 0,
            },
            StateVar {
                name: "restored", // sessions re-materialized by the new process
                init: 0,
            },
            StateVar {
                name: "lost", // SAFETY violation: a session dropped across the round-trip
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pack", // serialize one more session into the manifest
                guard: Some(le(var("packed"), sub(cst("N"), int(1)))),
                updates: vec![Update {
                    var: "packed",
                    expr: add(var("packed"), int(1)),
                }],
            },
            Action {
                name: "Restore", // the new process faithfully restores a packed session
                guard: Some(gt(var("packed"), var("restored"))),
                updates: vec![Update {
                    var: "restored",
                    expr: add(var("restored"), int(1)),
                }],
            },
            Action {
                name: "BuggyDrop", // a lossy transformer drops a session across the handoff
                guard: Some(and_(
                    eq(cst("Buggy"), int(1)),
                    gt(var("packed"), var("restored")),
                )),
                updates: vec![Update {
                    var: "lost",
                    expr: int(1),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "NoLossNoFabricate",
            // lost = 0 /\ restored <= packed  (never drop a session, never invent one)
            expr: and_(eq(var("lost"), int(0)), le(var("restored"), var("packed"))),
        }],
    }
}

/// PROOF-CARRYING DSU — the FD-HANDOFF NO-LEAK obligation, Rung 1b of
/// `docs/RFC-proof-carrying-dsu.md`.
///
/// The seamless re-exec clears `FD_CLOEXEC` on each live PTY master so it SURVIVES the
/// exec (proven with real syscalls in `aterm-pty`'s
/// `cloexec_controls_master_survival_across_exec`). But a master whose CLOEXEC was
/// cleared and that is then NEITHER re-adopted NOR closed is a LEAK — a live, ungated
/// PTY channel dangling across the exec, defeating the cross-session isolation the
/// CLOEXEC default enforces. This models the incoming side's accounting: `adopted +
/// closed <= prepared` always (never over-account), and `leaked` is set ONLY by the
/// `BuggyDrop` mutant (adopt fails and the fd is dropped WITHOUT closing).
///
/// `ty` checks the SAFETY invariant `NoLeak` (`leaked = 0 /\ adopted + closed <=
/// prepared`) at Buggy=0 and CATCHES the dangling fd at Buggy=1. Per the repo's
/// `Buggy`-convention this proves the leaking TRANSITION is FORBIDDEN — NOT the liveness
/// completion that every prepared master is eventually accounted for (`adopted + closed
/// == prepared`), which a safety invariant cannot establish. A `prepared` master not yet
/// adopted/closed is a legal IN-FLIGHT state, not a leak.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fd_handoff_no_leak_model() -> Model {
    Model {
        name: "FdHandoffNoLeak",
        consts: vec![("N", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "prepared", // masters with CLOEXEC cleared, handed across the exec
                init: 0,
            },
            StateVar {
                name: "adopted", // re-adopted by the new process (reader thread + owner)
                init: 0,
            },
            StateVar {
                name: "closed", // deliberately closed (fell back to a fresh shell)
                init: 0,
            },
            StateVar {
                name: "leaked", // SAFETY violation: a cleared master neither adopted nor closed
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Prepare", // clear CLOEXEC on one more master (outgoing side)
                guard: Some(le(var("prepared"), sub(cst("N"), int(1)))),
                updates: vec![Update {
                    var: "prepared",
                    expr: add(var("prepared"), int(1)),
                }],
            },
            Action {
                name: "Adopt", // the new process re-adopts a prepared master (valid fd)
                guard: Some(gt(var("prepared"), add(var("adopted"), var("closed")))),
                updates: vec![Update {
                    var: "adopted",
                    expr: add(var("adopted"), int(1)),
                }],
            },
            Action {
                name: "CloseFallback", // a dead/invalid fd: close it + cold-restart the tab
                guard: Some(gt(var("prepared"), add(var("adopted"), var("closed")))),
                updates: vec![Update {
                    var: "closed",
                    expr: add(var("closed"), int(1)),
                }],
            },
            Action {
                name: "BuggyDrop", // adopt fails and the fd is dropped WITHOUT closing -> leak
                guard: Some(and_(
                    eq(cst("Buggy"), int(1)),
                    gt(var("prepared"), add(var("adopted"), var("closed"))),
                )),
                updates: vec![Update {
                    var: "leaked",
                    expr: int(1),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "NoLeak",
            // leaked = 0 /\ adopted + closed <= prepared (never leak; never over-account)
            expr: and_(
                eq(var("leaked"), int(0)),
                le(add(var("adopted"), var("closed")), var("prepared")),
            ),
        }],
    }
}

/// PROOF-CARRYING DSU — the SINGLE-USE HANDOFF NONCE obligation, Rung 1b (live wiring)
/// of `docs/RFC-proof-carrying-dsu.md`.
///
/// The seamless update-apply authenticates the inherited PTY-master fd map with a
/// single-use nonce STAMP minted into the per-user `0700` dir: the outgoing side writes
/// it (`mint_seamless_stamp`), the incoming boot PRESENTS the nonce it was handed and
/// CONSUMES the stamp (`consume_seamless_stamp`: read-then-UNLINK, constant-time compare)
/// BEFORE it trusts any fd. Single-use is the whole gate: a spoofed or REPLAYED
/// `ATERM_SEAMLESS_FDS` presented after one adoption must find no stamp and fail closed —
/// otherwise a same-uid adversary could re-present a stale fd map and be authorized twice.
///
/// This models that lifecycle mint → present → consume-ONCE → reject-replay: `Mint`
/// stamps the nonce; `Present` consumes it and authorizes. CORRECT (`Buggy = 0`): the
/// consume UNLINKS the stamp, so a second `Present` finds none and cannot fire — the nonce
/// authorizes at most once. The `Buggy = 1` mutant does NOT unlink (a replayable stamp),
/// so a second `Present` authorizes AGAIN, setting `replayed`. `ty` checks the SAFETY
/// invariant `NoReplay` (`accepted <= minted /\ replayed = 0`) — a presented nonce
/// authorizes at most as many times as one was minted (i.e. once), and no replay ever
/// authorizes — proving it at Buggy=0 and CATCHING the replayable-stamp bug at Buggy=1.
/// The concrete binding is the real read-then-unlink consume
/// (`control_auth::consume_seamless_stamp`), pinned by the `aterm-gui`
/// `seamless_stamp_is_single_use_and_fails_closed` conformance test (correct nonce → once;
/// replay → none; wrong nonce → fail closed — the negative control).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn seamless_nonce_model() -> Model {
    Model {
        name: "SeamlessNonce",
        consts: vec![("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "minted", // nonces stamped into the 0700 dir (outgoing mints one)
                init: 0,
            },
            StateVar {
                name: "stamp", // the single-use stamp is present on disk right now
                init: 0,
            },
            StateVar {
                name: "accepted", // successful authorizations (a present stamp consumed)
                init: 0,
            },
            StateVar {
                name: "replayed", // SAFETY violation: an authorization fired after consume
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Mint", // outgoing side writes the 0600 single-use stamp (exactly once)
                guard: Some(eq(var("minted"), int(0))),
                updates: vec![
                    Update {
                        var: "minted",
                        expr: add(var("minted"), int(1)),
                    },
                    Update {
                        var: "stamp",
                        expr: int(1),
                    },
                ], // accepted / replayed UNCHANGED
            },
            Action {
                name: "Present", // incoming boot presents the nonce + consumes the stamp
                guard: Some(eq(var("stamp"), int(1))),
                updates: vec![
                    Update {
                        var: "replayed",
                        // an authorization that fires AFTER one already succeeded is a replay
                        expr: if_(gt(var("accepted"), int(0)), int(1), var("replayed")),
                    },
                    Update {
                        var: "accepted",
                        expr: add(var("accepted"), int(1)),
                    },
                    Update {
                        var: "stamp",
                        // CORRECT (Buggy=0): consuming UNLINKS the stamp (single-use), so a
                        // second Present cannot fire. Buggy=1: the stamp is NOT unlinked (a
                        // replayable stamp) -> a second Present authorizes again -> replay.
                        expr: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoReplay",
            // accepted <= minted /\ replayed = 0: a presented nonce authorizes at most as
            // many times as one was minted (once), and no replay ever authorizes.
            expr: and_(
                le(var("accepted"), var("minted")),
                eq(var("replayed"), int(0)),
            ),
        }],
    }
}

/// A sixth derived model — SNAPSHOT isolation (the `Snapshot` family property): a
/// snapshot, once taken, is isolated from later writes; a write must NOT leak into
/// an active snapshot's view. Scalar projection over `<<seq, snapped, leaked>>`:
/// `Snap` activates a snapshot; `Write` advances the head and, in the BUGGY case
/// (`Buggy = 1 /\ snapped = 1`), leaks into the snapshot (`leaked = 1`). ty proves
/// `SnapshotIsolated` (Buggy=0) and catches the leak (Buggy=1).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn snapshot_model() -> Model {
    Model {
        name: "Snapshot",
        consts: vec![("MaxSeq", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "snapped",
                init: 0,
            },
            StateVar {
                name: "leaked",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Snap", // take a single snapshot of the current head
                guard: Some(eq(var("snapped"), int(0))),
                updates: vec![Update {
                    var: "snapped",
                    expr: int(1),
                }], // seq, leaked UNCHANGED
            },
            Action {
                name: "Write", // advance the head; must not leak into an active snapshot
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), int(1)),
                    },
                    // leaked' = IF Buggy = 1 /\ snapped = 1 THEN 1 ELSE leaked
                    Update {
                        var: "leaked",
                        expr: if_(
                            and_(eq(cst("Buggy"), int(1)), eq(var("snapped"), int(1))),
                            int(1),
                            var("leaked"),
                        ),
                    },
                ], // snapped UNCHANGED
            },
        ],
        invariants: vec![Invariant {
            name: "SnapshotIsolated",
            expr: eq(var("leaked"), int(0)), // leaked = 0
        }],
    }
}

/// The READ-IMAGE snapshot-SEQ protocol (REARCH A-3), authored via [`ty_model!`].
///
/// The engine's render snapshot ([`crate`]-external `Terminal::cell_frame_into`)
/// is stamped with the monotone `damage_epoch` as its `snapshot_seq`. This models
/// the temporal contract that stamp must obey, scalar-projected over
/// `<<epoch, snapped, snap_seq, torn>>`:
///
///   * `Damage` advances the live `epoch` (the engine bumps `damage_epoch` on
///     net-new grid damage) — the MONOTONE-SEQ driver.
///   * `ReadImage` captures `snap_seq := epoch` ATOMICALLY and activates the
///     snapshot (`snapped := 1`) — the "value-of-seq at snapshot time", filled
///     under the one lock (no torn read).
///   * `Write` advances `epoch` (more damage after the snapshot). In the BUGGY
///     case (`Buggy == 1 && snapped == 1`) the later write leaks into the active
///     snapshot, setting `torn := 1` — a retro-mutation / torn read.
///
/// Two invariants, both proven at the committed `Buggy = 0`:
///   1. `NoTornRead: torn = 0` — SNAPSHOT INTERNAL-CONSISTENCY: a write after the
///      capture never mutates the already-emitted snapshot. This is what the
///      `Buggy` flip violates (so `ty` catches it at `Buggy = 1`), exactly the
///      `snapshot_model` isolation discipline applied to the seq stamp.
///   2. `SeqIsStaleOrCurrent: snap_seq <= epoch` — the captured seq never exceeds
///      the live epoch, so it is MONOTONE (the epoch only grows) and STALENESS is
///      always DETECTABLE: a consumer observing `epoch > snap_seq` knows its
///      snapshot is behind. Holds for both `Buggy` values (it pins the capture to
///      `= epoch`; a wrong capture would violate it), so it is non-vacuous.
///
/// Conformance-bound to the REAL `Terminal::cell_frame` + `damage_epoch`/
/// `take_damage` path in `aterm-core/tests/conformance_read_image_seq.rs` (Tier-1),
/// with a negative control so the pass is never vacuous.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_image_seq_model() -> Model {
    crate::ty_model! {
        ReadImageSeq {
            const MaxSeq = 4;
            const Buggy = 0;
            // The live damage epoch (monotone). The snapshot-active latch. The
            // seq captured by the last ReadImage. The torn-read leak flag.
            var epoch = 0;
            var snapped = 0;
            var snap_seq = 0;
            var torn = 0;

            // More grid damage bumps the engine's damage_epoch.
            action Damage when (epoch <= MaxSeq - 1) {
                epoch = epoch + 1;
            }

            // read_image: capture snap_seq = damage_epoch atomically, activate.
            action ReadImage when (snapped == 0) {
                snap_seq = epoch;
                snapped = 1;
            }

            // A write after the snapshot advances the epoch; in the buggy case it
            // leaks into the active snapshot (a torn read).
            action Write when (epoch <= MaxSeq - 1) {
                epoch = epoch + 1;
                torn = if Buggy == 1 && snapped == 1 { 1 } else { torn };
            }

            // No torn read / snapshot internal-consistency (the catch at Buggy=1).
            invariant NoTornRead: torn == 0;
            // Monotone + staleness-detectable: the captured seq never exceeds the
            // live epoch, so epoch > snap_seq is an observable "snapshot is stale".
            invariant SeqIsStaleOrCurrent: snap_seq <= epoch;
        }
    }
}

/// The aterm session PTY-master FD-LIFECYCLE discipline as a DERIVED model
/// (initiative A7, WS-G/concurrency) — the drift-free, code-bound twin that
/// SUPERSEDES the hand-written `FdLifecycle.tla` (now quarantined to `specs/legacy/`,
/// exactly as the kernel-family specs were when their derived twins took over). It
/// is the SINGLE source of truth for the `SinkWriter` ownership state machine in
/// `aterm-session/src/sink.rs`:
///
///   * `SinkWriter` owns the PTY master fd via `Option<OwnedFd>` (sink.rs:53); the
///     fd closes EXACTLY when the last `Arc<SinkWriter>` clone drops, never
///     out-of-band (sink.rs:32-39).
///   * Raw-fd use is via [`master`](../../../aterm_session/sink/struct.SinkWriter.html#method.master)
///     (sink.rs:84) and [`write_frame`](../../../aterm_session/sink/struct.SinkWriter.html#method.write_frame)
///     (sink.rs:97).
///
/// Scalar projection over `<<clones, fdOpen, usedAfterClose>>`:
///
///   * `Clone` — `Arc::clone` adds a holder while a live clone exists and the bound
///     `MaxClones` is not reached (`clones > 0 /\ clones < MaxClones`); `clones += 1`.
///     Bound in source as a `#[spec_unmodeled]` waiver on `new_owned` (std
///     `Arc::clone` is pure RAII — no aterm method to anchor).
///   * `UseFd` — a holder uses the RAW master fd (`master()` / `write_frame`). Sound
///     while open; latches `usedAfterClose` if the fd is already closed
///     (`usedAfterClose' = usedAfterClose \/ ~fdOpen`). Bound to the REAL fd-use code
///     via `#[refines]` on `write_frame`/`master`.
///   * `DropClone` — dropping one clone. THE FIX closes the fd (via `OwnedFd::drop`)
///     EXACTLY when the last clone drops (`fdOpen' = (clones - 1 > 0)`); the modeled
///     DEFECT (`Buggy = 1`) is the pre-fix bare-`i32` `close()` that fires on EVERY
///     drop — an out-of-band close while siblings still hold + use the raw fd
///     (`fdOpen' = 0` regardless of the remaining count). Bound as a
///     `#[spec_unmodeled]` waiver (the `OwnedFd` `Drop` is RAII — no aterm method).
///
/// `Buggy` convention (single-source prove-AND-catch, like `subscribe_model` /
/// `kernel_model` / etc.): the defect rides INSIDE the always-live `DropClone` action
/// (its `fdOpen` update), NOT a separate Buggy-only action — so at `Buggy = 0` no
/// action is dead (the `--strict-vacuity` gate stays green). At the committed
/// `Buggy = 0` the close-on-last-drop discipline holds, so `ty` PROVES both
/// invariants over the whole bounded space; at `Buggy = 1` a drop closes the fd while
/// clones remain and a subsequent `UseFd` latches the use-after-close, making both
/// invariants reachable-false — `ty` finds the COUNTEREXAMPLE. So the derived model
/// hosts BOTH halves; the hand `.tla` is no longer a second registered source.
///
/// Invariants, both proven over the whole bounded space (`MaxClones = 3`, `Buggy=0`):
///   1. `NoUseAfterClose: ~usedAfterClose` — no holder ever uses the raw fd after it
///      closed (the use-after-close race the fix eliminates).
///   2. `ClosedImpliesNoClones: (~fdOpen) => (clones = 0)` — the fd is closed only
///      when no live holder remains (the `OwnedFd`-last-drop guarantee). Written
///      `fdOpen \/ clones = 0` because the builder DSL has no `=>`/`~`.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fd_lifecycle_model() -> Model {
    Model {
        name: "FdLifecycle",
        consts: vec![("MaxClones", 3), ("Buggy", 0)],
        vars: vec![
            // Live Arc<SinkWriter> clone count (the original owner starts holding it).
            StateVar {
                name: "clones",
                init: 1,
            },
            // Is the PTY master fd still open? (1 = open, 0 = closed.)
            StateVar {
                name: "fdOpen",
                init: 1,
            },
            // Latched: did a holder use the raw fd after it was closed? (the race.)
            StateVar {
                name: "usedAfterClose",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Clone", // Arc::clone: another party takes a holder.
                // clones > 0 /\ clones < MaxClones
                guard: Some(and_(
                    gt(var("clones"), int(0)),
                    le(var("clones"), sub(cst("MaxClones"), int(1))),
                )),
                updates: vec![Update {
                    var: "clones",
                    expr: add(var("clones"), int(1)),
                }],
            },
            Action {
                name: "UseFd", // a holder uses the RAW master fd (master()/write_frame).
                guard: Some(gt(var("clones"), int(0))),
                // usedAfterClose' = IF fdOpen = 0 THEN 1 ELSE usedAfterClose
                // (latches the use-after-close; fdOpen, clones UNCHANGED).
                updates: vec![Update {
                    var: "usedAfterClose",
                    expr: if_(eq(var("fdOpen"), int(0)), int(1), var("usedAfterClose")),
                }],
            },
            Action {
                name: "DropClone", // drop one clone; THE FIX closes the fd only on the last drop.
                guard: Some(gt(var("clones"), int(0))),
                updates: vec![
                    Update {
                        var: "clones",
                        expr: sub(var("clones"), int(1)),
                    },
                    // fdOpen' = IF Buggy = 1 THEN 0                       (DEFECT: bare close on
                    //                                                      EVERY drop, out-of-band)
                    //           ELSE IF clones - 1 > 0 THEN 1 ELSE 0      (FIX: close iff last holder)
                    // The defect rides this always-live action (no dead Buggy-only action), so
                    // --strict-vacuity stays green at Buggy=0.
                    Update {
                        var: "fdOpen",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            int(0),
                            if_(gt(sub(var("clones"), int(1)), int(0)), int(1), int(0)),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            // No party ever uses the raw master fd after it has been closed.
            Invariant {
                name: "NoUseAfterClose",
                expr: eq(var("usedAfterClose"), int(0)),
            },
            // The fd is closed only when no live holder remains (fdOpen \/ clones = 0).
            Invariant {
                name: "ClosedImpliesNoClones",
                expr: or_(eq(var("fdOpen"), int(1)), eq(var("clones"), int(0))),
            },
        ],
    }
}

/// A FAITHFUL per-element ring model with a function-valued live-set
/// `live: [1..MaxSeq -> BOOLEAN]` — the property the scalar `ring_model` cannot
/// express. It proves `EvictOldestContiguous`: the live region is EXACTLY the
/// contiguous window `[lo, seq]`, so eviction removes precisely the oldest event,
/// never a hole and never two. This is the function-valued twin of the
/// hand-written `Evict.tla`'s operational `live` discipline. Because it is
/// function-valued, it is Tier-0 `ty`-checked (TLA+ generation), not run through
/// the scalar interpreter.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn evict_full_model() -> Model {
    // (seq + 1) - lo + 1 > Cap : the eviction condition (over the pre-state seq).
    let evicting = || {
        gt(
            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
            cst("Cap"),
        )
    };
    Model {
        name: "EvictFull",
        consts: vec![("MaxSeq", 5), ("Cap", 3)],
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
        fn_vars: vec![FnVar {
            name: "live",
            range: "MaxSeq",
        }],
        actions: vec![Action {
            name: "Push",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
            updates: vec![
                Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                },
                Update {
                    var: "lo",
                    expr: if_(evicting(), add(var("lo"), int(1)), var("lo")),
                },
                Update {
                    var: "live",
                    // Evicting: rebuild the live-set as (old minus the evicted `lo`)
                    // plus the new event `seq+1`. Non-evicting: just mark `seq+1`.
                    expr: if_(
                        evicting(),
                        comprehension(
                            "n",
                            int(1),
                            cst("MaxSeq"),
                            if_(
                                eq(var("n"), add(var("seq"), int(1))),
                                bool_lit(true),
                                and_(fn_access("live", var("n")), neq(var("n"), var("lo"))),
                            ),
                        ),
                        except("live", add(var("seq"), int(1)), bool_lit(true)),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "EvictOldestContiguous",
            // \A n \in 1..MaxSeq : live[n] <=> (lo =< n /\ n =< seq)
            expr: forall(
                "n",
                int(1),
                cst("MaxSeq"),
                iff(
                    fn_access("live", var("n")),
                    and_(le(var("lo"), var("n")), le(var("n"), var("seq"))),
                ),
            ),
        }],
    }
}

/// NEW machine (HIERARCHICAL_SESSIONS.md Addendum B, B.8.2): TIER-RESIDENCY /
/// **spill-not-forget**.
///
/// This is deliberately **not** an extension of [`evict_full_model`]. That model
/// proves `EvictOldestContiguous` — an evicted seq is *definitively not live*.
/// The hydratable temporal buffer asserts the **opposite**: an evicted seq is
/// still *recoverable*. So this is a different state machine over a new
/// `resident_warm`/`resident_cold` projection layered on the same eviction spine.
///
/// `Push` is the eviction spine of `evict_full_model` with one behavioral change:
/// when it evicts the oldest seq `lo`, it **atomically spills** it to the warm
/// tier (`resident_warm[lo] := TRUE`) on the same step — modeling the recorder's
/// spill hook firing synchronously on the `pop_front` path, so there is never an
/// intermediate state where the evicted event is neither live nor resident.
/// `Demote` moves the whole warm tier to cold (both count as resident, so the
/// tier transition preserves recoverability; the DSL has no "pick one index", so
/// a whole-tier demotion is the faithful bounded abstraction).
///
/// Invariant **`NoSilentLoss`**: every recorded seq up to the head is live or
/// resident in some tier —
/// `\A n \in 1..MaxSeq : (n =< seq) => (live[n] \/ resident_warm[n] \/ resident_cold[n])`.
/// The implication is written `(n > seq) \/ R` because the builder DSL has no
/// `=>`/`~`.
///
/// Negative control: at `Buggy = 1`, `Push` **drops on evict without spilling**,
/// so the evicted seq becomes neither live nor resident and `NoSilentLoss` fails —
/// `ty` finds the counterexample. Thus the proof at `Buggy = 0` is non-vacuous.
///
/// Function-valued ⇒ Tier-0 `ty`-checked (TLA+ generation), not run through the
/// scalar interpreter. The keyframe-recoverability clause from the design
/// (`\E k : k =< n /\ keyframe_at[k] /\ resident(k)`) is intentionally **out of
/// scope here**: it needs an existential the derive DSL lacks and belongs to the
/// B.8.3 hydration-faithfulness model, where the keyframe→replay fold lives.
/// Residency (live ∨ warm ∨ cold) is the complete spill-not-forget property for
/// this machine.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tier_residency_model() -> Model {
    // (seq + 1) - lo + 1 > Cap : the eviction condition over the pre-state seq
    // (identical to `evict_full_model`'s spine).
    let evicting = || {
        gt(
            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
            cst("Cap"),
        )
    };
    Model {
        name: "TierResidency",
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
        ],
        fn_vars: vec![
            FnVar {
                name: "live",
                range: "MaxSeq",
            },
            FnVar {
                name: "resident_warm",
                range: "MaxSeq",
            },
            FnVar {
                name: "resident_cold",
                range: "MaxSeq",
            },
        ],
        actions: vec![
            Action {
                name: "Push",
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), int(1)),
                    },
                    Update {
                        var: "lo",
                        expr: if_(evicting(), add(var("lo"), int(1)), var("lo")),
                    },
                    Update {
                        var: "live",
                        // Same live-set discipline as evict_full: evicting rebuilds
                        // (old minus the evicted `lo`) plus the new event; otherwise
                        // just mark `seq+1`.
                        expr: if_(
                            evicting(),
                            comprehension(
                                "n",
                                int(1),
                                cst("MaxSeq"),
                                if_(
                                    eq(var("n"), add(var("seq"), int(1))),
                                    bool_lit(true),
                                    and_(fn_access("live", var("n")), neq(var("n"), var("lo"))),
                                ),
                            ),
                            except("live", add(var("seq"), int(1)), bool_lit(true)),
                        ),
                    },
                    Update {
                        var: "resident_warm",
                        // Spill-not-forget: on eviction the evicted `lo` lands in warm
                        // ATOMICALLY. The bug (Buggy=1) drops it — no spill — so the
                        // evicted seq is left in no tier and NoSilentLoss fails.
                        // Non-evicting Push leaves warm untouched.
                        expr: if_(
                            and_(evicting(), eq(cst("Buggy"), int(0))),
                            except("resident_warm", var("lo"), bool_lit(true)),
                            var("resident_warm"),
                        ),
                    },
                    // resident_cold UNCHANGED in Push (rendered automatically).
                ],
            },
            Action {
                name: "Demote",
                // warm -> cold for the whole tier: every warm seq becomes cold, and
                // both are "resident", so residency is preserved across the demotion.
                // Always enabled (a no-op self-loop when warm is empty — harmless for
                // invariant checking).
                guard: None,
                updates: vec![
                    Update {
                        var: "resident_cold",
                        expr: comprehension(
                            "n",
                            int(1),
                            cst("MaxSeq"),
                            or_(
                                fn_access("resident_cold", var("n")),
                                fn_access("resident_warm", var("n")),
                            ),
                        ),
                    },
                    Update {
                        var: "resident_warm",
                        expr: comprehension("n", int(1), cst("MaxSeq"), bool_lit(false)),
                    },
                    // seq, lo, live UNCHANGED.
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoSilentLoss",
            // \A n \in 1..MaxSeq : (n =< seq) => (live[n] \/ warm[n] \/ cold[n])
            // implication encoded as (n > seq) \/ R (DSL has no => / ~).
            expr: forall(
                "n",
                int(1),
                cst("MaxSeq"),
                or_(
                    gt(var("n"), var("seq")),
                    or_(
                        fn_access("live", var("n")),
                        or_(
                            fn_access("resident_warm", var("n")),
                            fn_access("resident_cold", var("n")),
                        ),
                    ),
                ),
            ),
        }],
    }
}

/// NEW machine (HIERARCHICAL_SESSIONS.md Addendum B, B.8.3): HYDRATION-FAITHFULNESS.
///
/// The centerpiece replay property: hydrating a recording at an instant and
/// folding events forward from a keyframe reproduces the LIVE engine state —
/// `P(replay@t) = P(live@t)`. This is deliberately **not** the rejected
/// "bookkeeping tautology" (`hydrated_seq = seq`, a counter copied to a counter);
/// the invariant compares two parallel FOLDS, so a dropped/omitted event makes
/// them diverge and `ty` finds the counterexample.
///
/// **Abstract projection `P`.** `ty` function-vars are boolean-valued (`to_tla`
/// seeds them all-FALSE), so `P` is a one-bit checksum and the fold is a running
/// XOR (parity) over the recorded events — `a # b` in TLA+. Parity is
/// history-dependent: dropping or omitting any event flips the tail, exactly the
/// faithfulness hazard. (A richer multi-bit `P` needs int-valued fn-vars, a DSL
/// extension tracked for B.8.4 Tier-1 binding; one-bit parity already makes the
/// drop/omit controls bite.)
///
/// **Why a cursor, not a one-shot Hydrate.** A TLA+ comprehension
/// `[n |-> f(replay[n-1], payload[n])]` reads the OLD `replay`, so it is NOT a
/// real left-fold. Instead `Hydrate` SEEDS `replay` from the keyframe
/// (`replay[n] = live[n]` for `n =< KF`) and `ReplayStep` folds replay forward
/// ONE index per step via a cursor `rt` (each step legitimately reads the prior
/// new `replay[rt]`) — a genuine keyframe-seed-then-forward-replay.
///
/// **Invariant `ReplayFaithful`:** `\A n : (n =< rt) => (replay[n] = live[n])`
/// (encoded `(n > rt) \/ (replay[n] <=> live[n])`; the DSL has no `=>`/`~`).
///
/// **Negative control.** At `Buggy = 1`, `ReplayStep` at `rt+1 == DROPAT` skips
/// applying the payload (parity stalls), so `replay[DROPAT..]` diverges from
/// `live` and `ReplayFaithful` is violated — `ty` proves faithfulness at
/// `Buggy = 0` and catches the silent drop at `Buggy = 1`. Because the fold
/// would be a no-op if the clock/payload were not an explicit recorded input,
/// this model is only authorable AFTER the B.4.2 Clock seam landed.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn recording_model() -> Model {
    Model {
        name: "Recording",
        // KF = keyframe seq (fixed); DROPAT = the replay index the bug drops.
        consts: vec![("MaxSeq", 4), ("KF", 2), ("DROPAT", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            }, // live head
            StateVar {
                name: "rt",
                init: 0,
            }, // replay cursor (how far replay has folded)
        ],
        fn_vars: vec![
            FnVar {
                name: "payload",
                range: "MaxSeq",
            }, // recorded events (TRUE once recorded)
            FnVar {
                name: "live",
                range: "MaxSeq",
            }, // live parity fold
            FnVar {
                name: "replay",
                range: "MaxSeq",
            }, // replay parity fold (from keyframe)
        ],
        actions: vec![
            // Record one event: payload[seq+1]=TRUE; live[seq+1] = live[seq] XOR TRUE
            // (base: live[1] = TRUE since live[0] does not exist).
            Action {
                name: "Record",
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
                updates: vec![
                    Update {
                        var: "seq",
                        expr: add(var("seq"), int(1)),
                    },
                    Update {
                        var: "payload",
                        expr: except("payload", add(var("seq"), int(1)), bool_lit(true)),
                    },
                    Update {
                        var: "live",
                        expr: except(
                            "live",
                            add(var("seq"), int(1)),
                            if_(
                                eq(var("seq"), int(0)),
                                bool_lit(true),
                                neq(fn_access("live", var("seq")), bool_lit(true)),
                            ),
                        ),
                    },
                    // replay UNCHANGED
                ],
            },
            // Hydrate: seed replay from the keyframe — replay[n] = live[n] for n =< KF,
            // FALSE above; set the replay cursor to KF. Guarded so KF is recorded.
            Action {
                name: "Hydrate",
                guard: Some(le(cst("KF"), var("seq"))),
                updates: vec![
                    Update {
                        var: "rt",
                        expr: cst("KF"),
                    },
                    Update {
                        var: "replay",
                        expr: comprehension(
                            "n",
                            int(1),
                            cst("MaxSeq"),
                            if_(
                                le(var("n"), cst("KF")),
                                fn_access("live", var("n")), // keyframe seed
                                bool_lit(false),
                            ),
                        ),
                    },
                    // seq, payload, live UNCHANGED
                ],
            },
            // ReplayStep: fold replay forward one index using the prior replay[rt].
            // replay[rt+1] = replay[rt] XOR payload[rt+1], EXCEPT the DROP bug skips it.
            Action {
                name: "ReplayStep",
                // KF =< rt: only after Hydrate has seeded the cursor at the keyframe
                // (rt is 0 pre-Hydrate; folding before a seed would read replay[0],
                // out of the 1..MaxSeq domain). rt+1 =< seq: stay within recorded events.
                guard: Some(and_(
                    le(cst("KF"), var("rt")),
                    le(add(var("rt"), int(1)), var("seq")),
                )),
                updates: vec![
                    Update {
                        var: "rt",
                        expr: add(var("rt"), int(1)),
                    },
                    Update {
                        var: "replay",
                        expr: except(
                            "replay",
                            add(var("rt"), int(1)),
                            if_(
                                and_(
                                    eq(cst("Buggy"), int(1)),
                                    eq(add(var("rt"), int(1)), cst("DROPAT")),
                                ),
                                fn_access("replay", var("rt")), // DROP: skip payload (parity stalls)
                                neq(
                                    fn_access("replay", var("rt")),
                                    fn_access("payload", add(var("rt"), int(1))),
                                ),
                            ),
                        ),
                    },
                    // seq, payload, live UNCHANGED
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "ReplayFaithful",
            // \A n in 1..MaxSeq : (n =< rt) => (replay[n] = live[n])
            // encoded (n > rt) \/ (replay[n] <=> live[n]); booleans use Iff.
            expr: forall(
                "n",
                int(1),
                cst("MaxSeq"),
                or_(
                    gt(var("n"), var("rt")),
                    iff(fn_access("replay", var("n")), fn_access("live", var("n"))),
                ),
            ),
        }],
    }
}

/// An ELEVENTH derived model — IN-PROCESS MULTI-WINDOW ROUTING (the GUI window
/// lifecycle the multi-window work builds: `App` holds `BTreeMap<WindowId,
/// WindowState>` with a `frontmost_window`; Cmd-N creates a window, closing the
/// last one exits the app). Scalar projection over `<<win_count, frontmost,
/// next_id, exited>>`: the number of live windows, the id of the frontmost window
/// (`0` == none), a MONOTONIC never-reused id source (the multi-window analogue
/// of `next_session_id`), and whether the app has exited. `MaxWin` bounds
/// concurrent windows and `MaxId` bounds total creations, keeping `ty`'s search
/// exhaustive + terminating.
///
/// `Buggy` gates the close-last-window path: with `Buggy = 0` (committed) closing
/// the LAST window sets `exited`, so exit and an empty window set stay in
/// lockstep; with `Buggy = 1` the last close fails to exit, reproducing the
/// "no windows left but the app is still running" defect. So `ty` both PROVES the
/// routing invariants (Buggy=0) and CATCHES the missed exit (Buggy=1 ->
/// counterexample on `ExitIffEmpty`).
///
/// Invariants:
///   ExitIffEmpty       — `exited = 1  <=>  win_count = 0` (close-last exits, and
///                        the app never exits while a window remains).
///   FrontmostLive      — `frontmost = 0  <=>  win_count = 0` (a non-empty set
///                        always has a real frontmost; an empty set has none).
///   FrontmostAllocated — `frontmost = 0  \/  frontmost < next_id` (the frontmost
///                        is never a future / unallocated / reused id — the
///                        never-reused property that makes a stale `Wake` for a
///                        closed window unable to address a live one).
///
/// SCOPE: this scalar projection tracks the frontmost's NULL-ness and ALLOCATION,
/// not which specific ids are live (that needs a per-element refinement / the
/// Tier-1 conformance bind to the real `App`). It is exactly the close→exit +
/// never-reuse safety core.
/// COALESCE: the streaming write fold must be a pure function of the byte log
/// regardless of how it is split across `process_at` calls — i.e. the fast
/// "bulk" lane and the reference "single-char" lane must agree on every cell.
///
/// This is a 2-SAFETY property (a relation between two runs over the SAME input),
/// which a plain single-execution invariant cannot state — which is exactly why
/// model-checking missed the wide-char-wrap-tail and ZWJ-join divergences that
/// shipped. It is encoded here by SELF-COMPOSITION (the same trick
/// `recording_model` uses for live-vs-replay parity): one machine folds the same
/// event stream down BOTH lanes and asserts they never diverge, lifting the
/// 2-safety to a 1-safety invariant `ty` can discharge. The `Buggy` convention
/// reproduces the real class: at `SKIPAT` the bulk lane drops the per-element
/// fixup the single lane applies (the wrap-tail blank / the ZWJ continuation),
/// so the lanes diverge and the invariant is violated.
///
/// Tier-1 binds this to the SHIPPING engine: `aterm-core/tests/replay_corpus_probe.rs`
/// drives the real `process_at` across every chunking of adversarial corpora and
/// asserts an identical `checkpoint()` — the concrete witness this model abstracts.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn coalesce_model() -> Model {
    Model {
        name: "Coalesce",
        // SKIPAT = the fold index at which the buggy bulk lane drops the fixup.
        consts: vec![("MaxSeq", 4), ("SKIPAT", 2), ("Buggy", 0)],
        vars: vec![StateVar {
            name: "seq",
            init: 0,
        }],
        fn_vars: vec![
            FnVar {
                name: "single",
                range: "MaxSeq",
            }, // reference (per-char) fold
            FnVar {
                name: "bulk",
                range: "MaxSeq",
            }, // fast (coalesced) fold
        ],
        actions: vec![Action {
            name: "Emit",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
            updates: vec![
                Update {
                    var: "seq",
                    expr: add(var("seq"), int(1)),
                },
                // Reference lane: each element flips parity (the per-element fixup).
                Update {
                    var: "single",
                    expr: except(
                        "single",
                        add(var("seq"), int(1)),
                        if_(
                            eq(var("seq"), int(0)),
                            bool_lit(true),
                            neq(fn_access("single", var("seq")), bool_lit(true)),
                        ),
                    ),
                },
                // Bulk lane: identical fold, EXCEPT the Buggy variant skips the
                // fixup at SKIPAT (copies the previous cell), diverging — exactly
                // the wrap-tail / ZWJ class. The skip branch only reads bulk[seq]
                // when seq+1 = SKIPAT (so seq >= 1); seq = 0 takes the else.
                Update {
                    var: "bulk",
                    expr: except(
                        "bulk",
                        add(var("seq"), int(1)),
                        if_(
                            and_(
                                eq(cst("Buggy"), int(1)),
                                eq(add(var("seq"), int(1)), cst("SKIPAT")),
                            ),
                            fn_access("bulk", var("seq")), // BUG: drop the fixup
                            if_(
                                eq(var("seq"), int(0)),
                                bool_lit(true),
                                neq(fn_access("bulk", var("seq")), bool_lit(true)),
                            ),
                        ),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "LanesAgree",
            // \A n in 1..MaxSeq : (n > seq) \/ (bulk[n] <=> single[n])
            expr: forall(
                "n",
                int(1),
                cst("MaxSeq"),
                or_(
                    gt(var("n"), var("seq")),
                    iff(fn_access("bulk", var("n")), fn_access("single", var("n"))),
                ),
            ),
        }],
    }
}

// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn window_routing_model() -> Model {
    Model {
        name: "WindowRouting",
        consts: vec![("MaxWin", 2), ("MaxId", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "win_count",
                init: 1,
            },
            StateVar {
                name: "frontmost",
                init: 1,
            },
            StateVar {
                name: "next_id",
                init: 2,
            },
            StateVar {
                name: "exited",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            // Cmd-N / Wake::CreateWindow: a new window takes the next monotonic id
            // and becomes frontmost. Bounded by MaxWin (concurrent) + MaxId (total).
            Action {
                name: "CreateWindow",
                guard: Some(and_(
                    and_(
                        le(var("win_count"), sub(cst("MaxWin"), int(1))),
                        le(var("next_id"), sub(cst("MaxId"), int(1))),
                    ),
                    eq(var("exited"), int(0)),
                )),
                updates: vec![
                    Update {
                        var: "win_count",
                        expr: add(var("win_count"), int(1)),
                    },
                    Update {
                        var: "frontmost",
                        expr: var("next_id"),
                    },
                    Update {
                        var: "next_id",
                        expr: add(var("next_id"), int(1)),
                    },
                ],
            },
            // CloseRequested / Cmd-W last tab: close a window. Closing the LAST one
            // exits the app (unless Buggy) and clears frontmost to none (0); a
            // surviving window keeps a valid, already-allocated frontmost id.
            Action {
                name: "CloseWindow",
                guard: Some(and_(
                    gt(var("win_count"), int(0)),
                    eq(var("exited"), int(0)),
                )),
                updates: vec![
                    Update {
                        var: "win_count",
                        expr: sub(var("win_count"), int(1)),
                    },
                    // exited' = IF this was the last window THEN (Buggy ? 0 : 1) ELSE exited
                    Update {
                        var: "exited",
                        expr: if_(
                            eq(sub(var("win_count"), int(1)), int(0)),
                            if_(eq(cst("Buggy"), int(1)), int(0), int(1)),
                            var("exited"),
                        ),
                    },
                    // frontmost' \in (IF empty THEN {0} ELSE the surviving allocated ids).
                    //
                    // Closing the FRONTMOST window must RE-POINT frontmost to a
                    // survivor — but WHICH survivor is NOT a function of the scalar
                    // projection (`win_count`, `frontmost`, `next_id`): it doesn't
                    // track which specific ids are live. The real app picks the
                    // lowest live `WindowId` (BTreeMap order); a different policy
                    // would pick another. The faithful abstraction is therefore
                    // NONDETERMINISTIC: `frontmost'` may be ANY already-allocated id
                    // `1..(next_id - 1)`. A survivor remaining means a CreateWindow
                    // has run (`next_id >= 3`), so that range is non-empty; when the
                    // LAST window closes the range collapses to `0..0 = {0}` (no
                    // frontmost). `ty` checks the whole `\in` fan-out exhaustively, so
                    // EVERY admissible re-point preserves FrontmostLive (frontmost > 0
                    // iff a window remains) and FrontmostAllocated (frontmost <
                    // next_id, never future/reused) — and the real app's lowest-id
                    // choice is one such admissible value, so Tier-1 conformance
                    // accepts it. This ADMITS the frontmost-with-a-survivor re-point
                    // the old `frontmost' = frontmost` over-pinned away WITHOUT
                    // over-committing to an unprojectable policy. ExitIffEmpty (the
                    // Buggy=1 catch) is independent of this update, so the proof at
                    // Buggy=0 and the counterexample at Buggy=1 both still hold.
                    Update {
                        var: "frontmost",
                        expr: in_range(
                            if_(eq(sub(var("win_count"), int(1)), int(0)), int(0), int(1)),
                            if_(
                                eq(sub(var("win_count"), int(1)), int(0)),
                                int(0),
                                sub(var("next_id"), int(1)),
                            ),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "ExitIffEmpty",
                // (exited=1 /\ win_count=0) \/ (exited=0 /\ win_count>0)
                expr: or_(
                    and_(eq(var("exited"), int(1)), eq(var("win_count"), int(0))),
                    and_(eq(var("exited"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostLive",
                // (frontmost=0 /\ win_count=0) \/ (frontmost>0 /\ win_count>0)
                expr: or_(
                    and_(eq(var("frontmost"), int(0)), eq(var("win_count"), int(0))),
                    and_(gt(var("frontmost"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostAllocated",
                // frontmost = 0 \/ frontmost < next_id (never a future/reused id)
                expr: or_(
                    eq(var("frontmost"), int(0)),
                    gt(var("next_id"), var("frontmost")),
                ),
            },
        ],
    }
}

// ===========================================================================
// Introspection / recursive-stacking models (aterm-gui control plane).
// These derive the SAFETY properties the lossless-introspection + cross-process
// proxy feature must hold, so `ty` proves them exhaustively over the bounded
// state space and CATCHES the audit's real defect classes (M1 dispatch gap,
// M2 relay teardown leak, S1 registry leak) under the `Buggy=1` convention —
// model+verify, not example-test. See docs/TRUST-introspection-audit-detection.md.
// ===========================================================================

/// DISPATCH COMPLETENESS (audit finding M1). The control router must route EVERY
/// forwardable verb class aimed at a REMOTE child to the cross-process forward —
/// never silently to the local path (where it answers `ERR no such session`).
///
/// `vc` enumerates the verb classes `0..MaxVc`; `decided` is the router verdict
/// (`0` undecided, `2` forward, `3` deny/local-miss). `Pick` chooses any class
/// nondeterministically (`ty` fans out the whole domain); `Route` applies the
/// routing table. Forwardable = `vc =< 3` (read/write/subscribe/feed-bin);
/// `vc = 4` is a non-forwardable owner verb. `Buggy = 1` drops the SUBSCRIBE
/// class (`vc = 2`) from the forward table — exactly M1, where the verb-first
/// `subscribe` grammar was missed by the selector-first planner.
///
/// Invariant `ForwardableRemoteAlwaysForwarded`: once decided, a forwardable
/// class is routed to forward (`decided = 2`). `ty` proves it at `Buggy = 0`
/// and returns the counterexample `vc = 2, decided = 3` at `Buggy = 1`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn dispatch_complete_model() -> Model {
    // verb classes 0..MaxVc; forwardable = vc =< 3 (read/write/subscribe/feed-bin),
    // vc=4 is a non-forwardable owner verb. Buggy drops the subscribe class (vc=2)
    // — the exact M1 grammar miss. forward=2, deny/local-miss=3.
    props::gated_completeness(props::Gated {
        name: "DispatchComplete",
        item: "vc",
        decided: "decided",
        domain_max: "MaxVc",
        domain_val: 4,
        fwd_hi: 3,
        drop: 2,
        good: 2,
        bad: 3,
        pick: "Pick",
        route: "Route",
        inv: "ForwardableRemoteAlwaysForwarded",
    })
}

/// RELAY TEARDOWN LIVENESS (audit finding M2). When the cross-process relay tears
/// down, BOTH read halves of BOTH sockets must be shut so a pump parked on a
/// CLONE of a local socket gets EOF and the worker thread joins (no thread/fd
/// leak). `child_read_open`/`client_read_open` model the two read halves a pump
/// blocks on; `done` gates the post-teardown check. `Teardown` shuts the halves:
/// the correct discipline (`shutdown(Both)`) closes the read halves; `Buggy = 1`
/// models the original `shutdown(Write)`-only, which leaves the read halves open.
///
/// Invariant `ReadersUnblockAfterTeardown`: after teardown, both read halves are
/// closed (so both pumps unblock). `ty` proves it at `Buggy = 0` and returns the
/// parked-reader counterexample (`done = 1` with a read half still open) at
/// `Buggy = 1`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn relay_teardown_model() -> Model {
    // shutdown(Both) closes BOTH read halves (Buggy shutdown(Write) leaves them
    // open → the parked-reader leak). The flags init OPEN (1); Teardown gates on
    // `done`. Invariant: not-yet-torn-down OR both read halves closed.
    props::teardown_clears(props::Teardown {
        name: "RelayTeardown",
        flags: vec!["child_read_open", "client_read_open"],
        gate: "done",
        act: "Teardown",
        inv: "ReadersUnblockAfterTeardown",
    })
}

/// PROXY REGISTRY LIFECYCLE (audit finding S1). A spawned child's `ProxyEntry`
/// must be deregistered on session close, so the process-wide table never grows
/// past the live-session count (no unbounded leak as tabs open/close). `live`
/// counts live sessions, `registered` counts retained entries, bounded by `MaxN`.
/// `Spawn` registers + adds a live session; `Close` removes a live session and —
/// correctly — its entry, but `Buggy = 1` models the original `Drop` that forgot
/// to deregister (the entry survives a closed session).
///
/// Invariant `NoRegistryLeak`: `registered =< live`. `ty` proves it at
/// `Buggy = 0` and catches the leak (`registered = live + 1`) at `Buggy = 1`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn proxy_registry_model() -> Model {
    props::lifecycle_no_leak(props::Lifecycle {
        name: "ProxyRegistry",
        live: "live",
        reg: "registered",
        max: "MaxN",
        max_val: 3,
        acquire: "Spawn",
        release: "Close",
        inv: "NoRegistryLeak",
    })
}

/// FORWARD-HANDSHAKE LIVENESS / DEADLOCK-FREEDOM — the real `drain_buffered` bug
/// (proxy.rs `drain_buffered`/`connect_and_relay`). This is the LIVENESS twin of
/// the safety models: it closes the gap the audit documented, where a blocking
/// I/O call deadlocks in a way no reachable-bad-STATE invariant can see.
///
/// The cross-process forward is a two-party request → relay → reply: the reply
/// bytes are ALREADY buffered past the request line, and the client is parked
/// awaiting the reply. Correct (`Buggy = 0`): the server relays the BUFFERED
/// bytes (`reader.buffer()`) and the client is always served — a work-complete
/// terminal that stutters via the `Done` self-loop, never a deadlock. `Buggy = 1`
/// models the shipped `fill_buf()` defect: the server insists on reading MORE
/// (`relayed > 0`) before the FIRST relay, but the client — blocked awaiting the
/// reply — sends nothing, so every action is disabled in a non-`Done` state: a
/// two-party all-parked WEDGE that `ty` reports as a DEADLOCK.
///
/// Checked with `CHECK_DEADLOCK TRUE` ([`Model::to_cfg_deadlock_with`]). The
/// `Done` self-loop is MANDATORY — without it `ty` flags the clean
/// `client_waiting = 0` terminal itself as a deadlock (stuttering does not count).
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn forward_handshake_model() -> Model {
    props::no_wedge(props::Wedge {
        name: "ForwardHandshake",
        buffered: "buffered",
        buffered_init: 1,
        relayed: "relayed",
        waiting: "client_waiting",
        relay: "Relay",
        recv: "ClientRecv",
        done: "Done",
        inv: "WaitingIsBool",
    })
}

/// TLS RELAY STARTUP LIVENESS. A peer's first application record can be
/// decrypted by the TLS handshake driver before `tls::relay` begins. The local
/// service is waiting for that request, while the peer is waiting for its reply.
/// Correct (`Buggy = 0`): `DrainBuffered` forwards the already-buffered request
/// without requiring another network read. `Buggy = 1` demands a fresh record
/// first, leaving both parties parked in the initial state.
///
/// This is deliberately separate from [`forward_handshake_model`]: the abstract
/// wedge is the same, but this model is Tier-1-bound to the genuine TLS relay by
/// `tls::tests::relay_round_trips_guarded_artifact_ack_before_request_half_close`.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tls_buffered_relay_model() -> Model {
    props::no_wedge(props::Wedge {
        name: "TlsBufferedRelay",
        buffered: "buffered",
        buffered_init: 1,
        relayed: "relayed",
        waiting: "service_waiting",
        relay: "DrainBuffered",
        recv: "ServiceRecv",
        done: "Done",
        inv: "WaitingIsBool",
    })
}

/// AUTHORIZATION SOUNDNESS — the trust core's central predicate (`decide_edge` /
/// `EdgeTable::authorize`): a presented token is PERMITTED only when ALL four
/// conjuncts hold — the token is in the table, its `dst` equals the resolved
/// target, its `op` equals the verb's required op, and its nonce equals the
/// target's current launch nonce. The capability-layer audit found every one of
/// these checked on every request; this model GUARDS that against regression. The
/// `Buggy` variant drops the `dst` conjunct — the confused-deputy escalation (a
/// token valid for one session authorizing a different target); dropping `op`,
/// `nonce`, or `token` instead would model op-confusion, replay, or forgery.
///
/// Invariant `PermitImpliesAllGuards`: a permit implies every guard truly held.
/// `ty` proves it and catches the dropped-conjunct disclosure.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn authorize_soundness_model() -> Model {
    props::conjunctive_authz(props::ConjunctiveAuthz {
        name: "AuthorizeSoundness",
        guards: vec!["token", "dst", "op", "nonce"],
        decided: "decided",
        pick: "Present",
        decide: "Authorize",
        drop: "dst",
        inv: "PermitImpliesAllGuards",
    })
}

/// NO TRANSITIVE AUTHORITY — the property that makes deep nesting SAFE and is the
/// reason `proxy_forward_plan` refuses to forward a chained `@a @b verb`: forwarding
/// requires OWNER scope (`if !matches!(scope, Scope::Owner) { return None }`), so a
/// connection that itself ARRIVED over a forward (and therefore carries only an
/// EDGE scope) cannot initiate a further forward. Authority does not COMPOSE: a
/// grandparent that owns a parent cannot borrow the parent's authority to reach a
/// grandchild — it would need a DIRECT edge to the grandchild (a delegation/grant).
/// This is the confused-deputy boundary the capability audit verified is closed.
///
/// Modeled as a single-guard instance of [`props::conjunctive_authz`] (reusing the
/// authorization-soundness class): a forward is permitted only when the connection
/// is Owner-scoped; `Buggy` waives that guard (the transitive escalation). Invariant
/// `ForwardImpliesOwner`: a permitted forward implies the connection was Owner.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn no_transitive_authority_model() -> Model {
    props::conjunctive_authz(props::ConjunctiveAuthz {
        name: "NoTransitiveAuthority",
        guards: vec!["owner"],
        decided: "forwarded",
        pick: "Arrive",
        decide: "Forward",
        drop: "owner",
        inv: "ForwardImpliesOwner",
    })
}
