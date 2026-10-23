// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Models (`tab_nav_model`, `pane_tree_model`) shared with `aterm-spec`'s engine unit tests.

use aterm_spec::derive::*;

/// Per-window tab-strip index machine (`{ active, count }`).
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn tab_nav_model() -> Model {
    Model {
        name: "TabNav",
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "count",
                init: 1,
            },
            StateVar {
                name: "active",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "NewTab",
                guard: Some(le(var("count"), sub(cst("Cap"), int(1)))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: var("count"),
                    },
                    Update {
                        var: "count",
                        expr: add(var("count"), int(1)),
                    },
                ],
            },
            Action {
                name: "SelectTab",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![Update {
                    var: "active",
                    expr: in_range(int(0), sub(var("count"), int(1))),
                }],
            },
            Action {
                name: "Cycle",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![Update {
                    var: "active",
                    expr: if_(
                        gt(add(var("active"), int(1)), sub(var("count"), int(1))),
                        int(0),
                        add(var("active"), int(1)),
                    ),
                }],
            },
            Action {
                name: "Close",
                guard: Some(or_(
                    gt(var("count"), int(1)),
                    and_(eq(cst("Buggy"), int(2)), gt(var("count"), int(0))),
                )),
                updates: vec![
                    Update {
                        var: "active",
                        expr: if_(
                            eq(cst("Buggy"), int(1)),
                            var("active"),
                            if_(
                                gt(var("active"), sub(var("count"), int(2))),
                                sub(var("count"), int(2)),
                                var("active"),
                            ),
                        ),
                    },
                    Update {
                        var: "count",
                        expr: sub(var("count"), int(1)),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "CountPositive",
                kind: InvariantKind::Claim,
                expr: gt(var("count"), int(0)),
            },
            Invariant {
                name: "ActiveInRange",
                kind: InvariantKind::Claim,
                expr: le(var("active"), sub(var("count"), int(1))),
            },
        ],
    }
}

/// Focused leaf index machine of a tab's `PaneTree`.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn pane_tree_model() -> Model {
    Model {
        name: "PaneTree",
        consts: vec![("Cap", 4), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "leaf_count",
                init: 1,
            },
            StateVar {
                name: "focused",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Split",
                guard: Some(le(var("leaf_count"), sub(cst("Cap"), int(1)))),
                updates: vec![
                    Update {
                        var: "focused",
                        expr: var("leaf_count"),
                    },
                    Update {
                        var: "leaf_count",
                        expr: add(var("leaf_count"), int(1)),
                    },
                ],
            },
            Action {
                name: "Close",
                guard: Some(gt(var("leaf_count"), int(1))),
                updates: vec![
                    Update {
                        var: "focused",
                        expr: in_range(
                            int(0),
                            if_(
                                eq(cst("Buggy"), int(1)),
                                sub(var("leaf_count"), int(1)),
                                sub(var("leaf_count"), int(2)),
                            ),
                        ),
                    },
                    Update {
                        var: "leaf_count",
                        expr: sub(var("leaf_count"), int(1)),
                    },
                ],
            },
        ],
        invariants: vec![Invariant {
            name: "FocusInRange",
            kind: InvariantKind::Claim,
            expr: le(var("focused"), sub(var("leaf_count"), int(1))),
        }],
    }
}
