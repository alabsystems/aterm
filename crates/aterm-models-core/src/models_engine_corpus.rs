// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-core models aterm-spec's own unit tests run the engine on (`ring_model`.

use aterm_spec::derive::*;

/// The bounded event-log ring as a derived model.
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
            expr: le(add(sub(var("seq"), var("lo")), int(1)), cst("Cap")),
        }],
    }
}

/// A second derived model.
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

/// A third derived model.
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
                guard: Some(or_(
                    eq(cst("Buggy"), int(1)),
                    le(var("lo"), add(var("cursor"), int(1))),
                )),
                updates: vec![
                    Update {
                        var: "cursor",
                        expr: var("seq"),
                    },
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
