// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Engine corpus models mounted by `aterm-spec`'s unit tests (`src/test_models.rs`).

use aterm_spec::derive::*;

/// Transaction atomicity (no lost update under optimistic concurrency).
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

/// The event-log spine: gap-free, monotone, `seq == count`.
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
        fn_vars: vec![],
        actions: vec![Action {
            name: "Emit",
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int(1)))),
            updates: vec![
                Update {
                    var: "count",
                    expr: add(var("count"), int(1)),
                },
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

/// Snapshot isolation: a snapshot, once taken, is isolated from later writes.
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

/// Per-element ring with a function-valued live-set `live: [1..MaxSeq -> BOOLEAN]`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn evict_full_model() -> Model {
    let evicting = || {
        gt(
            add(sub(add(var("seq"), int(1)), var("lo")), int(1)),
            cst("Cap"),
        )
    };
    Model {
        name: "EvictFull",
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

/// Forward-handshake liveness model (`drain_buffered` blocking I/O deadlock).
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
