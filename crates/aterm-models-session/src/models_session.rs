// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session, kernel, fd-lifecycle, recording, and routing models.

use aterm_spec::derive::*;

/// Momentum wait model: accept a satisfied floor, time out an unmet reading past the backstop,
/// or arm one crossing (`Buggy = 1` re-parks an expired reading).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn momentum_wait_read_model() -> Model {
    aterm_spec::ty_model! {
        MomentumWaitRead {
            const Buggy = 0;
            var read = 0;
            var met = 0;
            var expired = 0;
            var decision = 0;

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

/// Spawn-time locale model: `resolve_spawn_locale` ensures the child runs under a UTF-8
/// `LC_CTYPE` regardless of inherited locale (`Buggy = 1` fixes only when all vars are unset).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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
                name: "ObserveNonUtf8",
                guard: Some(and_(
                    eq(var("resolved"), int(0)),
                    eq(var("present"), int(0)),
                )),
                updates: vec![Update {
                    var: "present",
                    expr: int(1),
                }],
            },
            Action {
                name: "ObserveUtf8",
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
                name: "Resolve",
                guard: Some(eq(var("resolved"), int(0))),
                updates: vec![
                    Update {
                        var: "resolved",
                        expr: int(1),
                    },
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
            expr: or_(eq(var("resolved"), int(0)), eq(var("ctype"), int(1))),
        }],
    }
}

/// Dynamic software update quiescence model: an update applies only when no request is in flight
/// (`Buggy = 1` applies mid-flight and sets `torn`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn dsu_quiescence_model() -> Model {
    Model {
        name: "DsuQuiescence",
        consts: vec![("MaxReq", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "inflight",
                init: 0,
            },
            StateVar {
                name: "applied",
                init: 0,
            },
            StateVar {
                name: "torn",
                init: 0,
            },
            StateVar {
                name: "served",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Begin",
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
                name: "Finish",
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
                name: "ApplyQuiescent",
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
                name: "BuggyApplyInflight",
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
            expr: eq(var("torn"), int(0)),
        }],
    }
}

/// Seamless re-exec fd-handoff model: every prepared PTY master is either re-adopted or closed
/// (`Buggy = 1` drops a failed adoption unclosed and sets `leaked`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn fd_handoff_no_leak_model() -> Model {
    Model {
        name: "FdHandoffNoLeak",
        consts: vec![("N", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "prepared",
                init: 0,
            },
            StateVar {
                name: "adopted",
                init: 0,
            },
            StateVar {
                name: "closed",
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
                name: "Prepare",
                guard: Some(le(var("prepared"), sub(cst("N"), int(1)))),
                updates: vec![Update {
                    var: "prepared",
                    expr: add(var("prepared"), int(1)),
                }],
            },
            Action {
                name: "Adopt",
                guard: Some(gt(var("prepared"), add(var("adopted"), var("closed")))),
                updates: vec![Update {
                    var: "adopted",
                    expr: add(var("adopted"), int(1)),
                }],
            },
            Action {
                name: "CloseFallback",
                guard: Some(gt(var("prepared"), add(var("adopted"), var("closed")))),
                updates: vec![Update {
                    var: "closed",
                    expr: add(var("closed"), int(1)),
                }],
            },
            Action {
                name: "BuggyDrop",
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
            expr: and_(
                eq(var("leaked"), int(0)),
                le(add(var("adopted"), var("closed")), var("prepared")),
            ),
        }],
    }
}

/// Render snapshot sequence model: `ReadImage` captures `snap_seq := epoch` atomically so post-read
/// writes do not tear the snapshot and `snap_seq <= epoch` (`Buggy = 1` stamps one ahead and leaks
/// subsequent writes).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_image_seq_model() -> Model {
    aterm_spec::ty_model! {
        ReadImageSeq {
            const MaxSeq = 4;
            const Buggy = 0;
            var epoch = 0;
            var snapped = 0;
            var snap_seq = 0;
            var torn = 0;

            action Damage when (epoch <= MaxSeq - 1) {
                epoch = epoch + 1;
            }

            action ReadImage when (snapped == 0) {
                snap_seq = if Buggy == 1 { epoch + 1 } else { epoch };
                snapped = 1;
            }

            action Write when (epoch <= MaxSeq - 1) {
                epoch = epoch + 1;
                torn = if Buggy == 1 && snapped == 1 { 1 } else { torn };
            }

            invariant NoTornRead: torn == 0;
            invariant SeqIsStaleOrCurrent: snap_seq <= epoch;
        }
    }
}

/// `SinkWriter` PTY master fd lifecycle model: the fd closes only when the last `Arc<SinkWriter>`
/// clone drops, preventing use-after-close (`Buggy = 1` closes on every drop).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn fd_lifecycle_model() -> Model {
    Model {
        name: "FdLifecycle",
        consts: vec![("MaxClones", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "clones",
                init: 1,
            },
            StateVar {
                name: "fdOpen",
                init: 1,
            },
            StateVar {
                name: "usedAfterClose",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Clone",
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
                name: "UseFd",
                guard: Some(gt(var("clones"), int(0))),
                updates: vec![Update {
                    var: "usedAfterClose",
                    expr: if_(eq(var("fdOpen"), int(0)), int(1), var("usedAfterClose")),
                }],
            },
            Action {
                name: "DropClone",
                guard: Some(gt(var("clones"), int(0))),
                updates: vec![
                    Update {
                        var: "clones",
                        expr: sub(var("clones"), int(1)),
                    },
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
            Invariant {
                name: "NoUseAfterClose",
                kind: InvariantKind::Claim,
                expr: eq(var("usedAfterClose"), int(0)),
            },
            Invariant {
                name: "ClosedImpliesNoClones",
                kind: InvariantKind::Claim,
                expr: or_(eq(var("fdOpen"), int(1)), eq(var("clones"), int(0))),
            },
        ],
    }
}

/// Tier residency model: `Push` spills an evicted event to the warm tier atomically and `Demote`
/// moves warm to cold, so every recorded seq up to `seq` remains live or resident (`Buggy = 1`
/// drops on eviction without spilling).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn tier_residency_model() -> Model {
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
                        expr: if_(
                            and_(evicting(), eq(cst("Buggy"), int(0))),
                            except("resident_warm", var("lo"), bool_lit(true)),
                            var("resident_warm"),
                        ),
                    },
                ],
            },
            Action {
                name: "Demote",
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
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "NoSilentLoss",
            kind: InvariantKind::Claim,
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

/// Recording hydration faithfulness model: seeding `replay` from keyframe `KF` and folding forward
/// step-by-step via XOR parity reproduces `live` (`Buggy = 1` drops the payload at `DROPAT`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn recording_model() -> Model {
    Model {
        name: "Recording",
        consts: vec![("MaxSeq", 4), ("KF", 2), ("DROPAT", 3), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "seq",
                init: 0,
            },
            StateVar {
                name: "rt",
                init: 0,
            },
        ],
        fn_vars: vec![
            FnVar {
                name: "payload",
                range: "MaxSeq",
            },
            FnVar {
                name: "live",
                range: "MaxSeq",
            },
            FnVar {
                name: "replay",
                range: "MaxSeq",
            },
        ],
        actions: vec![
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
                ],
            },
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
                                fn_access("live", var("n")),
                                bool_lit(false),
                            ),
                        ),
                    },
                ],
            },
            Action {
                name: "ReplayStep",
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
                                fn_access("replay", var("rt")),
                                neq(
                                    fn_access("replay", var("rt")),
                                    fn_access("payload", add(var("rt"), int(1))),
                                ),
                            ),
                        ),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "ReplayFaithful",
            kind: InvariantKind::Claim,
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

/// Streaming write coalescing model: bulk and single-byte `process_at` lanes agree after every
/// element (`Buggy = 1` drops a per-element fixup at `SKIPAT`).
#[cfg_attr(trust_verify, trust::skip)]
pub fn coalesce_model() -> Model {
    aterm_spec::ty_model! {
        Coalesce {
            const MaxSeq = 4;
            const SKIPAT = 2;
            const Buggy = 0;
            var seq = 0;
            var diverged = 0;
            action Emit when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                diverged = if Buggy == 1 && seq + 1 == SKIPAT { 1 } else { diverged };
            }
            invariant LanesAgree: diverged == 0;
        }
    }
}

/// Multi-window routing model: tracks `win_count`, `frontmost`, monotonic `next_id`, and `exited`
/// across window creation, close, and session raise (`Buggy = 1` skips exit on last close, skips
/// frontmost re-point, and reads `next_id` after bumping).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn window_routing_model() -> Model {
    let emptied = || eq(sub(var("win_count"), int(1)), int(0));
    let last_close_frontmost = || if_(eq(cst("Buggy"), int(1)), var("frontmost"), int(0));
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
                    Update {
                        var: "exited",
                        expr: if_(
                            eq(sub(var("win_count"), int(1)), int(0)),
                            if_(eq(cst("Buggy"), int(1)), int(0), int(1)),
                            var("exited"),
                        ),
                    },
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
                expr: or_(
                    and_(eq(var("exited"), int(1)), eq(var("win_count"), int(0))),
                    and_(eq(var("exited"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostLive",
                kind: InvariantKind::Claim,
                expr: or_(
                    and_(eq(var("frontmost"), int(0)), eq(var("win_count"), int(0))),
                    and_(gt(var("frontmost"), int(0)), gt(var("win_count"), int(0))),
                ),
            },
            Invariant {
                name: "FrontmostAllocated",
                kind: InvariantKind::Claim,
                expr: or_(
                    eq(var("frontmost"), int(0)),
                    gt(var("next_id"), var("frontmost")),
                ),
            },
        ],
    }
}

/// Control router dispatch completeness model: every forwardable verb class (`vc <= 3`) aimed at a
/// remote child is forwarded rather than denied locally (`Buggy = 1` drops `vc = 2`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn dispatch_complete_model() -> Model {
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

/// Cross-process relay teardown model: `Teardown` closes both read halves so blocked reader pumps
/// observe EOF and join (`Buggy = 1` shuts only `Write`, leaving read halves open).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn relay_teardown_model() -> Model {
    props::teardown_clears(props::Teardown {
        name: "RelayTeardown",
        flags: vec!["child_read_open", "client_read_open"],
        gate: "done",
        act: "Teardown",
        inv: "ReadersUnblockAfterTeardown",
    })
}

/// Child proxy registry lifecycle model: retained edge-token files are retired on session close so
/// `registered <= live` (`Buggy = 1` drops a session without retiring its file).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Cross-process forward handshake liveness model: the server drains already-buffered bytes before
/// reading more so the waiting client is always served (`Buggy = 1` demands a fresh read first and
/// deadlocks).
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

/// TLS relay startup liveness model: `DrainBuffered` forwards application bytes decrypted during
/// the TLS handshake without waiting for another network read (`Buggy = 1` deadlocks waiting for a
/// fresh record).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Edge authorization soundness model: a presented token is permitted only when table membership,
/// destination, operation, and launch nonce all match (`Buggy = 1` omits the destination check).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Cross-process forwarding authority model: forwarding is permitted only for owner-class scope,
/// preventing transitive authority across session edges (`Buggy = 1` waives the owner guard).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
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

/// Session id claim exclusivity model: at most one live instance holds `@<sid>` across normal
/// launches, exits, and seamless handoffs via the claim lock, live graph entry, and successor
/// marker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_id_claim_model() -> Model {
    aterm_spec::ty_model! {
        SessionIdClaim {
            const Buggy = 0;
            const Local = 0;
            const Instances = 2;
            const Handoff = 0;
            const Unmarked = 0;
            scenario Handoff = 1;
            var holders = 0;
            var lock = 0;
            var entry = 0;
            var window = 0;
            var marker = 0;
            var steps = 0;

            action Launch when (steps <= 5 && Instances > holders) {
                steps = steps + 1;
                holders = if Local == 1 { holders + 1 }
                          else { if lock == 0 && (Buggy == 1 || entry == 0) && marker == 0
                                 { holders + 1 } else { holders } };
                lock = if Local == 1 { 1 }
                       else { if lock == 0 && (Buggy == 1 || entry == 0) && marker == 0
                              { 1 } else { lock } };
            }

            action Exit when (steps <= 5 && holders > 0) {
                steps = steps + 1;
                holders = holders - 1;
                lock = if holders > 1 { 1 } else { 0 };
                entry = if holders > 1 { entry } else { 0 };
                window = if holders > 1 { window } else { 0 };
                marker = if holders > 1 { marker } else { 0 };
            }

            action PredecessorExits
                when (steps <= 5 && Handoff == 1 && holders == 1 && entry == 0 && window == 0)
            {
                steps = steps + 1;
                lock = 0;
                window = 1;
                marker = if Unmarked == 1 { 0 } else { 1 };
            }

            action SuccessorPublishes when (steps <= 5 && window == 1 && holders > 0) {
                steps = steps + 1;
                entry = 1;
                window = 0;
                marker = 0;
            }

            invariant AtMostOneHolder: holders <= 1;
        }
    }
}

/// Handoff address ownership model: a seamless-update candidate publishes carried session entries
/// only after commit, and the predecessor exempts its own registered candidate while refusing live
/// strangers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn handoff_address_owner_model() -> Model {
    aterm_spec::ty_model! {
        HandoffAddressOwner {
            const Buggy = 0;
            const Early = 0;
            const Blanket = 0;
            var cand = 0;
            var registered = 0;
            var entry = 0;
            var pred = 1;
            var asked = 0;
            var refused = 0;
            var steps = 0;

            action Launch when (pred == 1 && cand == 0) {
                cand = 1;
                registered = 1;
            }

            action SuccessorPublishes when (cand == 1 && entry == 0
                                            && (pred == 0 || Early == 1 || Buggy == 1)) {
                entry = 1;
            }

            action StrangerPublishes when (entry <= 1) {
                entry = 2;
            }

            action Request when (pred == 1 && steps <= 3) {
                steps = steps + 1;
                asked = entry + 1;
                refused = if entry == 2 {
                    if Blanket == 1 && registered == 1 { 0 } else { 1 }
                } else {
                    if entry == 1 && cand == 1 && (Buggy == 1 || registered == 0) { 1 } else { 0 }
                };
            }

            action Commit when (pred == 1 && cand == 1) {
                pred = 0;
            }

            action Reject when (pred == 1 && cand == 1) {
                cand = 2;
                registered = 0;
            }

            invariant CandidateNamedOnlyAfterCommit: pred == 0 || entry == 0 || entry == 2;

            invariant RefusesExactlyAStranger: if asked == 3 { refused == 1 } else { refused == 0 };
        }
    }
}
