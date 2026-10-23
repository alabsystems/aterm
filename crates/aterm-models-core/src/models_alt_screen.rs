// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The DEC private mode 1049 alternate-screen round trip.

use aterm_spec::derive::*;

/// The alternate-screen round trip, over one row of `Cells` cells.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn alt_screen_model() -> Model {
    let flag = |name: &'static str| StateVar { name, init: 0 };
    Model {
        name: "AltScreen",
        consts: vec![("Cells", 3), ("Buggy", 0)],
        vars: vec![
            flag("active"),
            flag("entered"),
            flag("touched"),
            flag("cursor"),
            flag("saved"),
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "WriteMain",
                guard: Some(and_(eq(var("active"), int(0)), eq(var("entered"), int(0)))),
                updates: vec![Update {
                    var: "cursor",
                    expr: in_range(int(1), cst("Cells")),
                }],
            },
            Action {
                name: "Enter",
                guard: Some(eq(var("active"), int(0))),
                updates: vec![
                    Update {
                        var: "saved",
                        expr: var("cursor"),
                    },
                    Update {
                        var: "active",
                        expr: int(1),
                    },
                    Update {
                        var: "entered",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Scribble",
                guard: Some(eq(var("active"), int(1))),
                updates: vec![
                    Update {
                        var: "cursor",
                        expr: in_range(int(1), cst("Cells")),
                    },
                    Update {
                        var: "touched",
                        expr: if_(eq(cst("Buggy"), int(1)), int(1), var("touched")),
                    },
                ],
            },
            Action {
                name: "Leave",
                guard: Some(eq(var("active"), int(1))),
                updates: vec![
                    Update {
                        var: "active",
                        expr: int(0),
                    },
                    Update {
                        var: "cursor",
                        expr: if_(eq(cst("Buggy"), int(2)), var("cursor"), var("saved")),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "MainUntouched",
                kind: InvariantKind::Claim,
                expr: or_(
                    eq(var("active"), int(1)),
                    or_(eq(var("entered"), int(0)), eq(var("touched"), int(0))),
                ),
            },
            Invariant {
                name: "CursorRestored",
                kind: InvariantKind::Claim,
                expr: or_(
                    eq(var("active"), int(1)),
                    or_(eq(var("entered"), int(0)), eq(var("cursor"), var("saved"))),
                ),
            },
            Invariant {
                name: "CursorOnScreen",
                kind: InvariantKind::Bound,
                expr: le(var("cursor"), cst("Cells")),
            },
            Invariant {
                name: "SavedOnScreen",
                kind: InvariantKind::Bound,
                expr: le(var("saved"), cst("Cells")),
            },
        ],
    }
}
