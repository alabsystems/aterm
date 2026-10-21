// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-session models aterm-spec's own unit tests run the engine on (`transact_model`, `kernel_model`, `snapshot_model`, `evict_full_model`, `forward_handshake_model`),
//! re-exported from this crate's root like every other. aterm-spec `#[path]`-mounts this file
//! (`src/test_models.rs`), so it holds only these and names only `aterm_spec::…` paths: an edit
//! to the rest of this crate's models then rebuilds no aterm-spec test and does not seed
//! aterm-spec, and with it every crate that depends on aterm-spec, into `verify`'s change cone.

use aterm_spec::derive::*;

/// Transaction atomicity (no lost update under optimistic concurrency). A transaction reads the
/// head at `tbase` (`Begin`); concurrent `Write`s may advance `seq`; it commits only if no write
/// intervened (`seq = tbase`), else aborts, since committing against a stale `tbase` clobbers
/// the concurrent write. `Buggy = 1` may commit despite a conflict (`seq' = tbase + K`), setting
/// `lost` and breaking `NoLostUpdate`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
/// `tests/conformance_kernel.rs` drives every real edit kind, transactions and the eviction
/// regime, projecting `count` from the writer and `seq` from the seq stamped on each stored
/// event (every stored event is one `Emit`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Snapshot isolation: a snapshot, once taken, is isolated from later writes. `Snap` activates a
/// snapshot; `Write` advances the head and, under `Buggy = 1 /\ snapped = 1`, leaks into it
/// (`leaked = 1`), breaking `SnapshotIsolated`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Per-element ring with a function-valued live-set `live: [1..MaxSeq -> BOOLEAN]`, which the
/// scalar `ring_model` cannot express. `EvictOldestContiguous`: the live region is exactly the
/// contiguous window `[lo, seq]`, so eviction removes precisely the oldest event, never a hole
/// and never two. Function-valued, so Tier-0 `ty` only, not the scalar interpreter.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
        // `Buggy = 1` advances `lo` but keeps the evicted event live: a hole at the old end.
        consts: vec![("MaxSeq", 5), ("Cap", 3), ("Buggy", 0)],
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
                                and_(
                                    fn_access("live", var("n")),
                                    or_(neq(var("n"), var("lo")), eq(cst("Buggy"), int(1))),
                                ),
                            ),
                        ),
                        except("live", add(var("seq"), int(1)), bool_lit(true)),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "EvictOldestContiguous",
            kind: InvariantKind::Claim,
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

/// Forward-handshake liveness (the `drain_buffered` bug; `control::try_proxy_forward`:
/// `control_lanes::LaneReader::take_buffered`, then proxy.rs `relay_hop`): a blocking I/O
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
#[aterm_spec::model]
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
