// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session, kernel, fd-lifecycle, recording and routing models, re-exported from
//! `crate::derive`. Each constructor is `trust::skip`: it is model data, and the model it
//! returns is what `ty` checks.

use super::*;

/// One fresh read in the momentum wait: accept a satisfied floor, time out an unmet reading
/// past the backstop, otherwise arm one crossing. A latched timer is only a reason to re-read,
/// never permission to ignore the backstop. `Buggy=1` re-parks an expired reading. Tier-1
/// drives `control_session::momentum_read_outcome`; separate wait regressions bind the timer to
/// the real snapping momentum metric.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn momentum_wait_read_model() -> Model {
    crate::ty_model! {
        MomentumWaitRead {
            const Buggy = 0;
            var read = 0;
            var met = 0;
            var expired = 0;
            var decision = 0; // 1=below, 2=deadline, 3=park

            action ReadLive when (read == 0) {
                read = 1; met = 0; expired = 0;
            }
            action ReadExpired when (read == 0) {
                read = 1; met = 0; expired = 1;
            }
            action ReadBelow when (read == 0) {
                read = 1; met = 1; expired = 0;
            }
            action ReadBelowExpired when (read == 0) {
                read = 1; met = 1; expired = 1;
            }
            action Below when (read == 1 && met == 1) {
                decision = 1;
            }
            action Deadline when (read == 1 && met == 0 && expired == 1) {
                decision = 2;
            }
            action Park when (read == 1 && met == 0 && (expired == 0 || Buggy == 1)) {
                decision = 3;
            }
            invariant ExpiredReadingCannotRepark:
                if expired == 1 { decision <= 2 } else { decision <= 3 };
        }
    }
}

/// Spawn-time locale: the child aterm launches runs under a UTF-8 `LC_CTYPE` whatever locale
/// aterm inherited. Under a non-UTF-8 `LC_CTYPE`, locale-aware programs (emacs, vim, python)
/// re-encode multibyte output to ASCII and print `?` per character. The real decision is
/// `aterm_pty::resolve_spawn_locale` (POSIX precedence `LC_ALL > LC_CTYPE > LANG`, empty ==
/// unset); Tier-1 is aterm-pty's `spawn_locale_conformance_*` test.
///
/// `present`: any locale var is set non-empty; `eff_utf8`: the effective inherited encoding is
/// already UTF-8; `ctype`: the child's resulting `LC_CTYPE` (1 = UTF-8) once `resolved`. The
/// `Observe*` actions spread the input shape and `Resolve` runs once. `Buggy = 1` forces UTF-8
/// only when nothing is set, so a present non-UTF-8 locale breaks `ChildHasUtf8Ctype`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                    // Buggy=1: IF present=0 THEN 1 ELSE eff_utf8 (an all-unset guard leaves a
                    // present non-UTF-8 locale unfixed). Buggy=0: always 1.
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
            kind: InvariantKind::Claim,
            // resolved => ctype = 1
            expr: or_(eq(var("resolved"), int(0)), eq(var("ctype"), int(1))),
        }],
    }
}

/// Transaction atomicity (no lost update under optimistic concurrency). A transaction reads the
/// head at `tbase` (`Begin`); concurrent `Write`s may advance `seq`; it commits only if no write
/// intervened (`seq = tbase`), else aborts, since committing against a stale `tbase` clobbers
/// the concurrent write. `Buggy = 1` may commit despite a conflict (`seq' = tbase + K`), setting
/// `lost` and breaking `NoLostUpdate`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Claim,
            expr: eq(var("lost"), int(0)), // lost = 0
        }],
    }
}

/// The event-log spine: gap-free, monotone, `seq == count`. Each `Emit` assigns the next
/// contiguous seq and bumps the count, so the head seq equals the number of events; `Buggy`
/// jumps seq by 2 (a gap), breaking `SeqIsCount`. Tier-1: aterm-buffer's
/// `tests/conformance_kernel.rs` drives every real edit kind, transactions, the `append_at`
/// spill seam and the eviction regime, projecting `count` from the writer and `seq` from the
/// seq stamped on each stored event (every stored event is one `Emit`).
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Claim,
            expr: eq(var("seq"), var("count")), // seq = count (gap-free, monotone spine)
        }],
    }
}

/// Proof-carrying DSU, the quiescence-safety obligation (Rung 0 of
/// `docs/RFC-proof-carrying-dsu.md`). A computation begun under the old version's memory layout
/// tears if resumed under the new one, so a running process may take an update only at a
/// quiescence point, with no request in flight.
///
/// * `Begin` — a request starts (`inflight := 1`), bounded by `served <= MaxReq - 1`.
/// * `Finish` — it completes (`inflight := 0`, `served += 1`).
/// * `ApplyQuiescent` — apply at quiescence (`applied = 0 /\ inflight = 0`): the transformer
///   runs on settled state.
/// * `BuggyApplyInflight` — (`Buggy = 1`) apply mid-flight: a tear (`torn := 1`).
///
/// `NoTear` (`torn = 0`) holds at `Buggy = 0` and fails at `Buggy = 1`: the mid-flight apply is
/// forbidden; request completion is not claimed.
///
/// Tier-1: in the overlap handoff a request is a slice of PTY output the reader drained from
/// the kernel and has not yet folded into the engine, and the apply is the handoff proceeding
/// past `park_all_readers`, which it does only when every `spawn::park_reader` returns `true`.
/// aterm-gui's `spawn::dsu_quiescence_conformance` attaches the real reader (gather + parse) to
/// a real pseudo-terminal, holds the engine busy on the terminal lock so a drained slice stays
/// in flight, and checks every step (the reader stopping, `park_reader` acking) against this
/// model; an ack with a drained slice unapplied is the step it rejects.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                name: "ApplyQuiescent", // apply only at quiescence (no request in flight)
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
            kind: InvariantKind::Claim,
            expr: eq(var("torn"), int(0)), // torn = 0
        }],
    }
}

/// Proof-carrying DSU, the snapshot round-trip obligation (Rung 1a of
/// `docs/RFC-proof-carrying-dsu.md`). Seamless re-exec hands the session set to the new binary
/// as a serialized manifest, and the new process must restore exactly that set, or the update
/// silently drops or duplicates a tab. `Pack` adds a session to the manifest, `Restore`
/// re-materializes one, and `BuggyDrop` (a transformer that drops one) sets `lost`.
/// `NoLossNoFabricate` (`lost = 0 /\ restored <= packed`) proves a session-dropping transition
/// forbidden, not the completion `restored == packed`; the concrete round-trip is the real serde
/// identity check (`session_handoff_roundtrips_the_whole_set`, `session_store.rs`).
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Claim,
            // lost = 0 /\ restored <= packed  (never drop a session, never invent one)
            expr: and_(eq(var("lost"), int(0)), le(var("restored"), var("packed"))),
        }],
    }
}

/// Proof-carrying DSU, the fd-handoff no-leak obligation (Rung 1b of
/// `docs/RFC-proof-carrying-dsu.md`). Seamless re-exec clears `FD_CLOEXEC` on each live PTY
/// master so it survives the exec (aterm-pty's `cloexec_controls_master_survival_across_exec`);
/// a cleared master then neither re-adopted nor closed is a live, ungated PTY channel dangling
/// across the exec. The incoming side's accounting: `adopted + closed <= prepared` always, and
/// only `BuggyDrop` (adopt fails, the fd dropped unclosed) sets `leaked`. `NoLeak` forbids the
/// leaking transition; eventual accounting (`adopted + closed == prepared`) is liveness and not
/// claimed — a prepared master not yet adopted or closed is a legal in-flight state.
///
/// Tier-1: two aterm-gui tests classify every handed master by a pooled session owning it
/// (`Adopt`) or its witness reading EOF (`CloseFallback`), never `BuggyDrop`:
/// `app_restore::tests::handed_off_masters_are_adopted_or_closed_never_leaked` hands socketpair
/// "masters" through the real orphan net (`App::adopt_orphan_shells_as_tabs`) with and without a
/// window to adopt into, adopting through `spawn::HandedMaster::into_sink`; and
/// `seamless::tests::a_refused_handoff_closes_every_master_it_was_handed` has the real
/// `take_incoming` refuse a staged two-PTY handoff after claiming one master.
/// `spawn::HandedMaster` owns an un-adopted master and closes it on drop, which also covers a
/// spawn that fails after taking the handle (not driven: it needs a live event loop).
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                name: "BuggyDrop", // adopt fails and the fd is dropped unclosed -> leak
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
            kind: InvariantKind::Claim,
            // leaked = 0 /\ adopted + closed <= prepared (never leak; never over-account)
            expr: and_(
                eq(var("leaked"), int(0)),
                le(add(var("adopted"), var("closed")), var("prepared")),
            ),
        }],
    }
}

/// Proof-carrying DSU, the single-use handoff nonce obligation (Rung 1b of
/// `docs/RFC-proof-carrying-dsu.md`). A spoofed or replayed `ATERM_SEAMLESS_FDS` presented after
/// one adoption must fail closed, or a same-uid adversary could re-present a stale fd map and be
/// authorized twice. `Mint` stamps a nonce; `Present` consumes it and authorizes. At
/// `Buggy = 0` the consume removes the stamp, so a second `Present` cannot fire; `Buggy = 1`
/// leaves a replayable stamp. `NoReplay`: `accepted <= minted /\ replayed = 0`.
///
/// The on-disk stamp this modelled is gone, and with it the Tier-1 bind. The property now rides
/// `ATERM_SEAMLESS_NONCE`: `handoff_rendezvous::claim_incoming` consumes the rendezvous names
/// from the environment whether or not it succeeds, and refuses a grant naming another attempt.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                        // an authorization after one already succeeded is a replay
                        expr: if_(gt(var("accepted"), int(0)), int(1), var("replayed")),
                    },
                    Update {
                        var: "accepted",
                        expr: add(var("accepted"), int(1)),
                    },
                    Update {
                        var: "stamp",
                        // Buggy=0: consuming unlinks the stamp, so a second Present cannot
                        // fire. Buggy=1: the stamp stays, and a second Present replays.
                        expr: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoReplay",
            kind: InvariantKind::Claim,
            // accepted <= minted /\ replayed = 0: a presented nonce authorizes at most as
            // many times as one was minted (once), and no replay ever authorizes.
            expr: and_(
                le(var("accepted"), var("minted")),
                eq(var("replayed"), int(0)),
            ),
        }],
    }
}

/// Snapshot isolation: a snapshot, once taken, is isolated from later writes. `Snap` activates a
/// snapshot; `Write` advances the head and, under `Buggy = 1 /\ snapped = 1`, leaks into it
/// (`leaked = 1`), breaking `SnapshotIsolated`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Claim,
            expr: eq(var("leaked"), int(0)), // leaked = 0
        }],
    }
}

/// The read-image snapshot-seq protocol. The engine's render snapshot
/// (`Terminal::cell_frame_into`, outside this crate) is stamped with the monotone
/// `damage_epoch` as its `snapshot_seq`:
///
/// * `Damage` advances the live `epoch` (net-new grid damage bumps `damage_epoch`).
/// * `ReadImage` captures `snap_seq := epoch` atomically under the one lock and activates the
///   snapshot (`snapped := 1`).
/// * `Write` advances `epoch` after the snapshot; under `Buggy == 1 && snapped == 1` the write
///   leaks into the active snapshot (`torn := 1`).
///
/// Two invariants:
/// 1. `NoTornRead: torn = 0` — a write after the capture never mutates the emitted snapshot
///    ([`snapshot_model`]'s isolation applied to the seq stamp).
/// 2. `SeqIsStaleOrCurrent: snap_seq <= epoch` — the captured seq never exceeds the live epoch,
///    so staleness is always detectable (`epoch > snap_seq`). `snap_seq` is the engine's stamp
///    (`engine_fill_seq`), which only lags a write. The scratch's `snapshot_seq` can run ahead
///    (every host mutator bumps it: stream fade, prediction ghosts, the find bar, the Claude
///    footer); `Buggy = 1` stamps the capture one ahead, that host-bumped value read as the
///    engine's, so one engine advance lands the epoch on it and a stale frame passes as current
///    — the collision `effect_only_snapshot_reusable`'s `engine_fill_seq == snapshot_seq` clause
///    refuses.
///
/// Tier-1: `aterm-core/tests/conformance_read_image_seq.rs` binds the real
/// `Terminal::cell_frame` and `damage_epoch`/`take_damage` path, with a negative control per law.
#[must_use]
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
            // Buggy: the stamp is a host-bumped seq, one ahead of the engine's.
            action ReadImage when (snapped == 0) {
                snap_seq = if Buggy == 1 { epoch + 1 } else { epoch };
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

/// The session PTY-master fd lifecycle: the code-bound model of the `SinkWriter` ownership state
/// machine in `aterm-session/src/sink.rs`. `SinkWriter` owns the master as `Option<OwnedFd>`;
/// the fd closes exactly when the last `Arc<SinkWriter>` clone drops, never out of band, and
/// raw-fd use goes through `master()` and `write_frame`.
///
/// * `Clone` — `Arc::clone` adds a holder while one exists and `MaxClones` is not reached; bound
///   by a `spec_unmodeled` waiver on `new_owned` (`Arc::clone` has no aterm method to anchor).
/// * `UseFd` — a holder uses the raw fd (`master()` / `write_frame`), latching `usedAfterClose`
///   if it is already closed; bound by `#[refines]` on `write_frame`/`master`.
/// * `DropClone` — drop one clone; the fd closes (via `OwnedFd::drop`) exactly on the last drop
///   (`fdOpen' = (clones - 1 > 0)`). `Buggy = 1` closes on every drop, out of band while
///   siblings still hold and use the raw fd. Bound by a `spec_unmodeled` waiver.
///
/// The defect rides the always-live `DropClone` rather than a Buggy-only action, so no action
/// is dead at `Buggy = 0` (`--strict-vacuity`). At `Buggy = 1` a drop closes the fd while clones
/// remain and a later `UseFd` latches the use-after-close.
///
/// 1. `NoUseAfterClose: ~usedAfterClose` — no holder uses the raw fd after it closed.
/// 2. `ClosedImpliesNoClones: (~fdOpen) => (clones = 0)` — the fd closes only with no live
///    holder, written `fdOpen \/ clones = 0` (the DSL has no `=>`/`~`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                name: "DropClone", // drop one clone; the fd closes only on the last drop.
                guard: Some(gt(var("clones"), int(0))),
                updates: vec![
                    Update {
                        var: "clones",
                        expr: sub(var("clones"), int(1)),
                    },
                    // fdOpen' = IF Buggy = 1 THEN 0 (close on every drop)
                    //           ELSE IF clones - 1 > 0 THEN 1 ELSE 0 (close iff last holder).
                    // The defect rides this always-live action, so --strict-vacuity holds at
                    // Buggy=0.
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
                kind: InvariantKind::Claim,
                expr: eq(var("usedAfterClose"), int(0)),
            },
            // The fd is closed only when no live holder remains (fdOpen \/ clones = 0).
            Invariant {
                name: "ClosedImpliesNoClones",
                kind: InvariantKind::Claim,
                expr: or_(eq(var("fdOpen"), int(1)), eq(var("clones"), int(0))),
            },
        ],
    }
}

/// Per-element ring with a function-valued live-set `live: [1..MaxSeq -> BOOLEAN]`, which the
/// scalar `ring_model` cannot express. `EvictOldestContiguous`: the live region is exactly the
/// contiguous window `[lo, seq]`, so eviction removes precisely the oldest event, never a hole
/// and never two. Function-valued, so Tier-0 `ty` only, not the scalar interpreter.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
            kind: InvariantKind::Uncaught,
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

/// Tier residency, spill-not-forget (HIERARCHICAL_SESSIONS.md Addendum B, B.8.2). Not an
/// extension of [`evict_full_model`], which proves an evicted seq is not live: here an evicted
/// seq stays recoverable, through `resident_warm`/`resident_cold` layered on the same eviction
/// spine. `Push` evicts the oldest seq `lo` and spills it to warm in the same step (the
/// recorder's spill hook fires synchronously on the `pop_front` path, so the evicted event is
/// never neither live nor resident). `Demote` moves the whole warm tier to cold (the DSL cannot
/// pick one index).
///
/// `NoSilentLoss`: every recorded seq up to the head is live or resident,
/// `\A n \in 1..MaxSeq : (n =< seq) => (live[n] \/ resident_warm[n] \/ resident_cold[n])`,
/// written `(n > seq) \/ R`. `Buggy = 1` drops on evict without spilling. Function-valued, so
/// Tier-0 `ty` only. Keyframe recoverability is out of scope (it needs an existential the DSL
/// lacks): re-keyframing keeps the latest instant reachable, and a needed base that aged out
/// answers `ERR temporal unreachable` rather than a wrong screen (aterm-gui `temporal.rs`'s
/// `rekeyframing_keeps_the_latest_reachable_past_the_budget`,
/// `replay_degrades_to_none_when_needed_blobs_were_evicted`).
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                        // atomically. Buggy=1 drops it, so it is in no tier and NoSilentLoss
                        // fails. A non-evicting Push leaves warm untouched.
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
                // warm -> cold for the whole tier; both are resident, so residency is
                // preserved. Always enabled (a no-op self-loop when warm is empty).
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
            kind: InvariantKind::Claim,
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

/// Hydration faithfulness (HIERARCHICAL_SESSIONS.md Addendum B, B.8.3): hydrating a recording
/// at an instant and folding events forward from a keyframe reproduces the live engine state,
/// `P(replay@t) = P(live@t)`. The invariant compares two parallel folds, not a counter copied
/// to a counter, so a dropped or omitted event makes them diverge.
///
/// `ty` function-vars are boolean (`to_tla` seeds them all-FALSE), so `P` is a one-bit checksum
/// and the fold a running XOR over the recorded events (`a # b`); parity is history-dependent,
/// so dropping any event flips the tail. A TLA+ comprehension `[n |-> f(replay[n-1], ...)]`
/// reads the old `replay` and is not a left fold, so `Hydrate` seeds `replay` from the keyframe
/// (`replay[n] = live[n]` for `n =< KF`) and `ReplayStep` folds one index per step via the
/// cursor `rt`.
///
/// `ReplayFaithful`: `\A n : (n =< rt) => (replay[n] = live[n])`, encoded
/// `(n > rt) \/ (replay[n] <=> live[n])`. At `Buggy = 1`, `ReplayStep` at `rt+1 == DROPAT` skips
/// the payload (parity stalls) and `replay[DROPAT..]` diverges.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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
                // KF =< rt: only after Hydrate seeded the cursor (rt is 0 before, and folding
                // then would read replay[0], outside 1..MaxSeq). rt+1 =< seq: within recorded
                // events.
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
                                fn_access("replay", var("rt")), // drop: parity stalls
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
            kind: InvariantKind::Claim,
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

/// Coalescing: the streaming write fold is a pure function of the byte log however it is split
/// across `process_at` calls, so the bulk lane (a coalesced run through the batched writers) and
/// the single-char lane (one byte per call, every glyph through the per-glyph writer) agree on
/// every cell after every element. This 2-safety property (a relation between two runs over
/// one input) is lifted by self-composition to the 1-safety `LanesAgree`; the model keeps only
/// the composition's verdict — `seq`, the elements folded, and `diverged`, whether the screens
/// differ — and the two lanes are the Tier-1 bind's. `Buggy` makes the bulk lane drop at
/// `SKIPAT` a per-element fixup the single lane applies (the wrap-tail blank, the ZWJ
/// continuation).
///
/// Tier-1: `aterm-core/tests/replay_corpus_probe.rs` drives the shipping `process_at` down both
/// lanes over every adversarial corpus, projects `diverged` from the lanes' real `checkpoint()`s
/// after every parser-ground prefix (a record ending mid-escape or mid-UTF-8 extends the element
/// into the next, since a checkpoint is defined only at ground), and checks each element as
/// this model's `Emit`, so a divergence at any grounded prefix is a rejected step.
#[cfg_attr(trust_verify, trust::skip)]
pub fn coalesce_model() -> Model {
    crate::ty_model! {
        Coalesce {
            const MaxSeq = 4;
            const SKIPAT = 2;   // the element at which the buggy bulk lane drops the fixup
            const Buggy = 0;
            var seq = 0;        // elements folded down both lanes
            var diverged = 0;   // the two lanes' screens differ
            action Emit when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                diverged = if Buggy == 1 && seq + 1 == SKIPAT { 1 } else { diverged };
            }
            invariant LanesAgree: diverged == 0;
        }
    }
}

/// In-process multi-window routing: `App` holds `BTreeMap<WindowId, WindowState>` with a
/// `frontmost_window`; Cmd-N creates a window and closing the last one exits the app. Over
/// `<<win_count, frontmost, next_id, exited>>`: live windows, the frontmost id (`0` = none), a
/// monotonic never-reused id source, and whether the app exited. `MaxWin` bounds concurrent
/// windows and `MaxId` total creations.
///
/// `Buggy = 1` has one slip per law in the real `App` seams (Tier-1: `aterm-gui`'s
/// `window_routing_conformance`):
/// * the last close does not exit (`ExitIffEmpty`);
/// * `close_window_logical` skips its frontmost re-point, so the last close leaves
///   `frontmost_window` naming the dropped window (`FrontmostLive`). Only the last close is
///   caught, where the real app exits anyway (`CloseOutcome::Exit`), so this pins bookkeeping;
///   the harmful form (the front window closes beside a survivor and keeps the dead id) passes
///   this projection, and the real `structural_invariant_violation` oracle, debug-asserted by
///   `close_window_logical`, refuses it;
/// * `insert_logical_window` bumps `next_window_id` before reading it, so the new frontmost
///   carries the id the next create mints again (`FrontmostAllocated`).
///
/// `ConnectedCreateWindow` (the `spawn connected= place=window` mint) is CreateWindow with the
/// hosted-origin precondition (`win_count > 0`); `RaiseSession` (`raise <sid>` / map Enter)
/// re-points `frontmost` over the allocated ids. Both add transitions, never state.
///
/// * `ExitIffEmpty` — `exited = 1 <=> win_count = 0`.
/// * `FrontmostLive` — `frontmost = 0 <=> win_count = 0`.
/// * `FrontmostAllocated` — `frontmost = 0 \/ frontmost < next_id`: never a future or reused
///   id, so a stale `Wake` for a closed window cannot address a live one.
///
/// The projection tracks the frontmost's null-ness and allocation, not which ids are live
/// (that is the Tier-1 bind's).
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn window_routing_model() -> Model {
    // The value `frontmost` takes when the last window closes: none (0), or — the
    // skipped re-point — the id of the window just closed.
    let emptied = || eq(sub(var("win_count"), int(1)), int(0));
    let last_close_frontmost = || if_(eq(cst("Buggy"), int(1)), var("frontmost"), int(0));
    // The id a create mints for the new frontmost window: `next_id`, or — read
    // after the bump — the one the next create will mint.
    let minted = || add(var("next_id"), cst("Buggy"));
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
                        expr: minted(),
                    },
                    Update {
                        var: "next_id",
                        expr: add(var("next_id"), int(1)),
                    },
                ],
            },
            // CloseRequested / Cmd-W last tab: closing the last window exits the app (unless
            // Buggy) and clears frontmost to 0; a survivor keeps an allocated frontmost id.
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
                    // Which survivor a frontmost close re-points to is not a function of the
                    // projection (it does not track which ids are live; the real app picks the
                    // lowest live `WindowId`), so `frontmost'` may be any allocated id
                    // `1..(next_id - 1)`, non-empty while a survivor remains (`next_id >= 3`)
                    // and collapsing to `{0}` on the last close. Every admissible re-point
                    // preserves FrontmostLive and FrontmostAllocated, and the real choice is
                    // one of them.
                    //
                    // The in_range must be the top-level RHS (the renderer emits
                    // `frontmost' \in lo..hi` only there), so Buggy's skipped re-point is
                    // folded into the bounds of the emptied case.
                    Update {
                        var: "frontmost",
                        expr: in_range(
                            if_(emptied(), last_close_frontmost(), int(1)),
                            if_(
                                emptied(),
                                last_close_frontmost(),
                                sub(var("next_id"), int(1)),
                            ),
                        ),
                    },
                ],
            },
            // `spawn connected= place=window`: CreateWindow with a live, hosted origin session
            // (`of=<origin>`), which the projection states as `win_count > 0` (headless
            // `place=window` is refused before any create). ExitIffEmpty already implies it;
            // stating it keeps the precondition in the spec. Same bounds and updates.
            Action {
                name: "ConnectedCreateWindow",
                guard: Some(and_(
                    and_(
                        and_(
                            le(var("win_count"), sub(cst("MaxWin"), int(1))),
                            le(var("next_id"), sub(cst("MaxId"), int(1))),
                        ),
                        eq(var("exited"), int(0)),
                    ),
                    gt(var("win_count"), int(0)),
                )),
                updates: vec![
                    Update {
                        var: "win_count",
                        expr: add(var("win_count"), int(1)),
                    },
                    Update {
                        var: "frontmost",
                        expr: minted(),
                    },
                    Update {
                        var: "next_id",
                        expr: add(var("next_id"), int(1)),
                    },
                ],
            },
            // `raise <sid>` / map Enter / menu Show: focus routing re-points `frontmost` to the
            // window hosting the raised session; nothing is created or destroyed. Which window
            // hosts it is not in the projection, so this is CloseWindow's nondeterministic
            // re-point over the allocated ids `1..(next_id - 1)`, of which the real choice (and
            // the headless stutter) is one value. A live window implies `next_id >= 2`, so the
            // range is non-empty. Only `frontmost` moves: the act adds edges, never states.
            Action {
                name: "RaiseSession",
                guard: Some(and_(
                    gt(var("win_count"), int(0)),
                    eq(var("exited"), int(0)),
                )),
                updates: vec![Update {
                    var: "frontmost",
                    expr: in_range(int(1), sub(var("next_id"), int(1))),
                }],
            },
        ],
        invariants: vec![
            Invariant {
                name: "ExitIffEmpty",
                kind: InvariantKind::Claim,
                // (exited=1 /\ win_count=0) \/ (exited=0 /\ win_count>0)
                expr: or_(
                    and_(eq(var("exited"), int(1)), eq(var("win_count"), int(0))),
                    and_(eq(var("exited"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostLive",
                kind: InvariantKind::Claim,
                // (frontmost=0 /\ win_count=0) \/ (frontmost>0 /\ win_count>0)
                expr: or_(
                    and_(eq(var("frontmost"), int(0)), eq(var("win_count"), int(0))),
                    and_(gt(var("frontmost"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostAllocated",
                kind: InvariantKind::Claim,
                // frontmost = 0 \/ frontmost < next_id (never a future/reused id)
                expr: or_(
                    eq(var("frontmost"), int(0)),
                    gt(var("next_id"), var("frontmost")),
                ),
            },
        ],
    }
}

// Introspection / recursive-stacking models (the aterm-gui control plane and cross-process
// proxy). See docs/TRUST-introspection-audit-detection.md.

/// Dispatch completeness: the control router routes every forwardable verb class aimed at a
/// remote child to the cross-process forward, never to the local path (where it answers
/// `ERR no such session`). `vc` enumerates the classes `0..MaxVc`; `decided` is the verdict (`0`
/// undecided, `2` forward, `3` deny/local-miss). Forwardable is `vc =< 3`
/// (read/write/subscribe/feed-bin); `vc = 4` is a non-forwardable owner verb. `Buggy = 1` drops
/// the subscribe class (`vc = 2`), as a selector-first planner misses the verb-first
/// `subscribe` grammar. `ForwardableRemoteAlwaysForwarded`: once decided, a forwardable class
/// is forwarded.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn dispatch_complete_model() -> Model {
    // forward=2, deny/local-miss=3; Buggy drops the subscribe class (vc=2).
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

/// Relay teardown: when the cross-process relay tears down, both read halves of both sockets
/// are shut, so a pump parked on a clone of a local socket gets EOF and its worker joins (no
/// thread/fd leak). `child_read_open`/`client_read_open` are the two read halves a pump blocks
/// on; `Teardown` hangs up each socket (`aterm_uds::hangup::hang_up`: its write half, then its
/// read half), closing them. `Buggy = 1` shuts only `Write`, leaving the read halves open. The
/// worker also polls before each read and stops on a flag set before every hang-up of the
/// child, so a lost wake costs one poll, not a parked thread. `ReadersUnblockAfterTeardown`:
/// after teardown both read halves are closed.
///
/// Tier-1: aterm-gui's `proxy::tests::relay_teardown_conforms_to_readers_unblock_after_teardown`
/// tears the real `relay_bidirectional` down on an injected send fault while both peers stay
/// open, observes each read half on a clone of the relay's local socket, and checks the result
/// as this model's `Teardown`; a `shutdown(Write)`-only teardown leaves both halves open, the
/// step only `Buggy = 1` admits.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
pub fn relay_teardown_model() -> Model {
    // A hang-up closes both read halves; Buggy's shutdown(Write) leaves them open. The flags
    // init open (1); Teardown gates on `done`.
    props::teardown_clears(props::Teardown {
        name: "RelayTeardown",
        flags: vec!["child_read_open", "client_read_open"],
        gate: "done",
        act: "Teardown",
        inv: "ReadersUnblockAfterTeardown",
    })
}

/// Proxy registry lifecycle: a spawned child's `ProxyEntry` is deregistered on session close,
/// so the process-wide table never outgrows the live-session count. `live` counts live
/// sessions, `registered` retained entries, bounded by `MaxN`; `Spawn` registers and adds a
/// session, `Close` removes one and its entry. `Buggy = 1` is a `Drop` that forgets to
/// deregister, once, from a table not yet leaked (keeping the space finite).
/// `NoRegistryLeak`: `registered =< live`.
///
/// Tier-1: aterm-gui's `session_pool_tests::session_close_conforms_to_proxy_registry_no_leak`
/// drives the real provisioning (`spawn::provision_child_proxy`, whose `proxy::ChildProxy` the
/// session keeps) and the real session drop through a spawn/close script that fills `MaxN` and
/// drains it, projecting the pool and `proxy::proxies()` onto `live`/`registered` at every
/// step. Storing the `ChildProxy` is `spawn_session`'s, which needs a live event loop; the test
/// stores it the same way, and the undriven half is structural: an unstored `ChildProxy` drops,
/// deregistering.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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

/// Forward-handshake liveness (proxy.rs `drain_buffered`/`connect_and_relay`): a blocking I/O
/// deadlock no bad-state invariant can see. The cross-process forward is request → relay →
/// reply: the reply bytes are already buffered past the request line, and the client is parked
/// awaiting them. At `Buggy = 0` the server relays the buffered bytes (`reader.buffer()`) and
/// the client is always served, ending in the `Done` self-loop. `Buggy = 1` reads more
/// (`fill_buf()`) before the first relay, but the client sends nothing while it waits, so every
/// action is disabled outside `Done`: a deadlock.
///
/// Checked with `CHECK_DEADLOCK TRUE` ([`Model::to_cfg_deadlock_with`]); the `Done` self-loop is
/// required, or the clean `client_waiting = 0` terminal itself reads as a deadlock.
///
/// Tier-1: aterm-gui's `control::tests::try_proxy_forward_conforms_to_forward_handshake` sends
/// one request line through the real `try_proxy_forward` to a stand-in child socket and checks
/// the observed `Relay` (the child receives the handshake) and `ClientRecv` (the client is
/// answered) against this model; a forward that demands a fresh client read first is reported
/// parked in the state only `Buggy = 1` wedges in.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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

/// TLS relay startup liveness. The TLS handshake driver can decrypt a peer's first application
/// record before `tls::relay` begins; the local service waits for that request while the peer
/// waits for its reply. `DrainBuffered` forwards the already-buffered request without another
/// network read; `Buggy = 1` demands a fresh record first, parking both parties. Separate from
/// [`forward_handshake_model`] (the same wedge) because each binds its own relay: this one the
/// TLS relay, via `tls::tests::relay_round_trips_guarded_artifact_ack_before_request_half_close`.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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

/// Authorization soundness: the trust core's central predicate (`decide_edge` /
/// `EdgeTable::authorize`) permits a presented token only when all four conjuncts hold — the
/// token is in the table, its `dst` equals the resolved target, its `op` the verb's required op,
/// and its nonce the target's current launch nonce. `Buggy` drops the `dst` conjunct (the
/// confused deputy: a token for one session authorizing another); dropping `op`, `nonce` or
/// `token` would model op-confusion, replay or forgery. `PermitImpliesAllGuards`: a permit
/// implies every guard held.
///
/// Tier-1: aterm-session's `tests/conformance_authorize.rs` drives the real `decide_edge` over
/// all sixteen guard combinations and `EdgeTable::authorize` over the eight with `op` held (it
/// takes no op, and its callers compare the one it returns to nothing), validates each real
/// `Present`/`Authorize` step against this model, and replays them against `Buggy = 1`, which
/// must reject the shipping gate's denial of the wrong-destination presentation.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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

/// No transitive authority: the reason `proxy_forward_plan` refuses to forward a chained
/// `@a @b verb`. Forwarding requires owner-class scope (`scope.is_owner_class()`: the instance
/// token, or the fabric bridge connection, which is Owner plus the two bridge verbs), so a
/// connection that arrived over a forward, and so carries only an edge scope, cannot forward
/// further. A forwarded connection authenticates at the child socket with a token and resolves
/// to `Owner` or `Edge` there, never `Bridge`: that scope exists only on an fd the instance
/// handed its own child, never a relayed one. So a grandparent that owns a parent cannot borrow
/// the parent's authority to reach a grandchild; it needs a direct edge (a delegation/grant).
///
/// A single-guard instance of [`props::conjunctive_authz`]: a forward is permitted only when the
/// connection is Owner-scoped; `Buggy` waives the guard. `ForwardImpliesOwner`: a permitted
/// forward implies the connection was Owner.
///
/// Tier-1: aterm-gui's `control::tests::proxy_forward_plan_conforms_to_no_transitive_authority`
/// drives the real `proxy_forward_plan` across read, write, signal and subscribe lines aimed at
/// a forwardable child for an Owner connection, a bridge one, and one that arrived over the
/// forward (whose scope is what the child's connect-time auth, `control::first_line_scope`,
/// makes of the Owner plan's exact first line), stating `owner` from each scope rather than
/// from `is_owner_class`, and replays the steps against `Buggy = 1`, which must part from the
/// real router on every arrived line.
#[cfg_attr(trust_verify, trust::skip)]
#[crate::model]
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

/// One live holder per session id: two holders mean `@<sid>` can deliver a keystroke into a
/// human's window.
///
/// A pane's shell exports `ATERM_SESSION_ID`, the identity the outer aterm preminted for the
/// inner aterm that pane may launch. The export deliberately outlives the launch it was minted
/// for — a child aterm that exits must relaunch in the same shell under its original identity,
/// or every edge the outer minted for it dies with the first launch — so a launch must tell a
/// relaunch (the previous holder gone: a transfer) from a second simultaneous launch (the
/// previous holder live: a duplicate). Otherwise two instances answer to one id, `@<sid>`
/// resolves through one discovery entry, and a driver reading a sid from a listing can land a
/// keystroke in the other instance's window.
///
/// * `Instances` — launches from one shell. `Launch` is admitted only below it, so
///   `Instances = 1` collapses this onto the single-launch case where nothing can go wrong.
/// * `Local` — what a launch consults. `Local = 0` is the shipping rule: the kernel arbitrates,
///   via a `flock` held for the process's life and a live foreign `graph/<sid>` entry.
///   `Local = 1` decides from the launch's own premint alone, blind to other holders: each
///   launch is individually "correct" and the id is held N times, breaking the invariant at
///   every corner.
/// * `Buggy` — consult the claim lock but not the live entry. That is invisible until a
///   seamless update, whose successor keeps its ids and cannot inherit the predecessor's lock,
///   so the lock goes free while the successor's own entry says the id is held. It needs
///   `Handoff = 1` to bite, which is why the entry gate exists beside the lock.
/// * `Unmarked` — the handoff window. On a fixed-path socket the predecessor exits (its lock
///   dies) before the successor binds and publishes its entry, and between the two neither gate
///   answers. (A per-process socket's successor publishes before the Commit instead, where the
///   question is who answers: [`handoff_address_owner_model`].) The predecessor names its
///   successor at Commit (`claims/<sid>.successor`, a third gate a launch consults), and the
///   successor retires the marker once its entry is published. `Unmarked = 1` exits with no
///   marker, and a launch in the window duplicates.
///
/// The lock dying with its holder is what lets a relaunch adopt, and why the claim file is never
/// unlinked (unlinking a claim a peer holds open lets two processes both hold one). `Exit`
/// models that release, so a "fix" that never adopts would not reproduce the relaunch trace.
///
/// Tier-1: aterm-gui's
/// `identity_claim::session_id_claim_conformance_real_adoption_projects_onto_model` and
/// `identity_claim::the_handoff_window_is_held_by_the_successor_marker` drive the real
/// `claim_for_adoption` through every arm, the window included.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_id_claim_model() -> Model {
    crate::ty_model! {
        SessionIdClaim {
            const Buggy = 0;      // 1 = consult the claim lock but not the live entry
            const Local = 0;      // 1 = each launch adopts on its own premint read alone
            const Instances = 2;  // launches from one shell (the export outlives the launch)
            const Handoff = 0;    // 1 = a seamless-update successor takes the id over
            const Unmarked = 0;   // 1 = the predecessor exits without naming its successor
            // Seamless updates hand ids over in the real code.
            scenario Handoff = 1;
            var holders = 0;      // live instances answering to the id
            var lock = 0;         // the claim flock is held by a live process
            var entry = 0;        // a live process has published graph/<sid>
            var window = 0;       // predecessor gone, successor not yet published
            var marker = 0;       // claims/<sid>.successor names the live successor
            var steps = 0;        // run bound

            // A launch reads the surviving premint and tries to adopt it.
            action Launch when (steps <= 5 && Instances > holders) {
                steps = steps + 1;
                holders = if Local == 1 { holders + 1 }
                          else { if lock == 0 && (Buggy == 1 || entry == 0) && marker == 0
                                 { holders + 1 } else { holders } };
                lock = if Local == 1 { 1 }
                       else { if lock == 0 && (Buggy == 1 || entry == 0) && marker == 0
                              { 1 } else { lock } };
            }

            // The holder exits and the lock dies with it — what lets a relaunch adopt, and why
            // nothing is ever unlinked. A successor dying in the window takes its marker's
            // authority with it (a marker naming a dead pid reads as no marker).
            action Exit when (steps <= 5 && holders > 0) {
                steps = steps + 1;
                holders = holders - 1;
                lock = if holders > 1 { 1 } else { 0 };
                entry = if holders > 1 { entry } else { 0 };
                window = if holders > 1 { window } else { 0 };
                marker = if holders > 1 { marker } else { 0 };
            }

            // A seamless-update successor takes the id over: a transfer, not a second holder.
            // The predecessor names it at Commit and exits; its lock dies with it, and nothing
            // of the successor's is published yet.
            action PredecessorExits
                when (steps <= 5 && Handoff == 1 && holders == 1 && entry == 0 && window == 0)
            {
                steps = steps + 1;
                lock = 0;
                window = 1;
                marker = if Unmarked == 1 { 0 } else { 1 };
            }

            // The successor binds and publishes its own entry, which now holds the id, and
            // retires the marker. It re-attempts the claim too, but the model takes the worst
            // case (it may not get it), so the entry gate stays load-bearing.
            action SuccessorPublishes when (steps <= 5 && window == 1 && holders > 0) {
                steps = steps + 1;
                entry = 1;
                window = 0;
                marker = 0;
            }

            // An address that resolves at all resolves to exactly one place.
            invariant AtMostOneHolder: holders <= 1;
        }
    }
}

/// Who answers an adopted `@<sid>` while a seamless update decides (aterm-gui
/// `control::publish_when_committed`, and `control::ambiguous_sid_refusal` over
/// `identity_claim::live_holder`).
///
/// `graph/<sid>` is the address every `@<sid>` client resolves. The dispatch refuses an id this
/// process hosts when that entry names another live process: two holders of one address, so a
/// keystroke could land in a stranger's window. A successor on a per-process socket that
/// published its carried ids at boot, before the predecessor's Commit, would make the entry
/// refuse the predecessor's own supervisor (`ERR ambiguous session id … also served by pid
/// <successor>`) while the predecessor still owns the sessions. Two layers close it:
///
/// * The order: a successor publishes the carried entries only once the Commit has released
///   its gate, so while this process owns a session its address names no process it could be
///   handed to — `CandidateNamedOnlyAfterCommit`.
/// * The exemption: should an entry name this process's own candidate early anyway, it is the
///   transfer being decided, not a second holder — the one pid this process's handoff lane
///   registered, for as long as the attempt decides — `RefusesExactlyAStranger`.
///
/// `cand`: this process's candidate, 0 none yet, 1 alive, 2 proven dead (a rejection reaps
/// before the lane lets go of it); `registered`: the lane's registration names it (`Launch`
/// creates and registers it before any descriptor reaches it; `Reject` reaps, then drops the
/// registration); `entry`: whom `graph/<sid>` names, 0 this process, 1 the candidate, 2 a live
/// stranger (an unrelated instance holding the same id); `pred`: this process still owns the
/// session (0 after the Commit's `_exit`); `asked`: the last request answered, 0 none, else 1 +
/// the entry it met; `refused`: whether it was refused.
///
/// * `Buggy = 1` — publish at boot and refuse any live foreign entry, the candidate's included:
///   `Launch` → `SuccessorPublishes` names the candidate while this process owns the session,
///   and a `Request` there refuses its own session.
/// * `Early = 1` — publish at boot with the exemption in place. `CandidateNamedOnlyAfterCommit`
///   is caught; the exemption alone still keeps `RefusesExactlyAStranger`, checked with the
///   order's invariant set aside, so the second layer is proved on its own.
/// * `Blanket = 1` — an exemption keyed on "an attempt is deciding" rather than on which pid: a
///   stranger publishing the same id during an update is served (`StrangerPublishes` →
///   `Request`).
///
/// A request after a `Reject` meets a dead candidate's entry (only under `Early`), which no
/// rule refuses (a dead pid is no holder); one after the Commit never reaches this process. In
/// the fork lane the registration follows `spawn` by microseconds; the model takes the launched
/// lane's order, where the registration comes at the dial, before the grant gives the successor
/// any id to publish.
///
/// Tier-1: aterm-gui's
/// `control::publish_when_committed_conformance::a_candidates_entries_name_it_only_after_the_commit`
/// drives the real gated publication (`SuccessorPublishes` at the Commit, never before);
/// `control::tests::an_id_handed_to_this_processs_update_candidate_is_served_here_until_commit`
/// the real `ambiguous_sid_refusal` and `resolve_explicit` through every entry and registration
/// state; and `identity_claim::tests::a_handoff_candidate_registration_is_exactly_its_own` the
/// registration's life.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn handoff_address_owner_model() -> Model {
    crate::ty_model! {
        HandoffAddressOwner {
            const Buggy = 0;      // 1 = publish at boot and refuse every live foreign entry
            const Early = 0;      // 1 = the successor publishes at boot (the exemption kept)
            const Blanket = 0;    // 1 = exempt every entry while an attempt decides
            var cand = 0;         // 0 none, 1 alive, 2 proven dead
            var registered = 0;   // the lane's registration names the candidate
            var entry = 0;        // graph/<sid> names: 0 us, 1 the candidate, 2 a stranger
            var pred = 1;         // this process still owns the session
            var asked = 0;        // the last request: 0 none, else 1 + the entry it met
            var refused = 0;      // and whether it was refused
            var steps = 0;        // run bound

            // The lane makes its candidate and registers it, before the grant.
            action Launch when (pred == 1 && cand == 0) {
                cand = 1;
                registered = 1;
            }

            // The candidate publishes the carried ids' entries once the Commit has
            // released its gate (`Early`/`Buggy`: as soon as it has booted).
            action SuccessorPublishes when (cand == 1 && entry == 0
                                            && (pred == 0 || Early == 1 || Buggy == 1)) {
                entry = 1;
            }

            // An unrelated live instance publishes the same id.
            action StrangerPublishes when (entry <= 1) {
                entry = 2;
            }

            // This process's own supervisor asks for its session.
            action Request when (pred == 1 && steps <= 3) {
                steps = steps + 1;
                asked = entry + 1;
                refused = if entry == 2 {
                    if Blanket == 1 && registered == 1 { 0 } else { 1 }
                } else {
                    if entry == 1 && cand == 1 && (Buggy == 1 || registered == 0) { 1 } else { 0 }
                };
            }

            // The Commit: this process `_exit`s, its registration with it.
            action Commit when (pred == 1 && cand == 1) {
                pred = 0;
            }

            // A rejection: the candidate is reaped, then the lane lets go.
            action Reject when (pred == 1 && cand == 1) {
                cand = 2;
                registered = 0;
            }

            // The order: while this process owns the session, its address names no candidate
            // of this process's — one server per address at every instant.
            invariant CandidateNamedOnlyAfterCommit: pred == 0 || entry == 0 || entry == 2;

            // The exemption: refuse exactly the entries a stranger holds — never the transfer
            // this process is deciding, always a real duplicate.
            invariant RefusesExactlyAStranger: if asked == 3 { refused == 1 } else { refused == 0 };
        }
    }
}
