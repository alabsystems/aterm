// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The blocking write-all loop that drains a buffer to a descriptor.

use aterm_spec::derive::*;

/// The write-all loop over a buffer of `Size` bytes: `off` written so far, `done` the loop reported
/// completion.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn write_all_model() -> Model {
    let open = and_(
        eq(var("done"), int(0)),
        le(var("off"), sub(cst("Size"), int(1))),
    );
    Model {
        name: "WriteAll",
        consts: vec![("Size", 5), ("Buggy", 0)],
        vars: vec![
            StateVar {
                name: "off",
                init: 0,
            },
            StateVar {
                name: "done",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Progress",
                guard: Some(open.clone()),
                updates: vec![
                    Update {
                        var: "off",
                        expr: cst("Size"),
                    },
                    Update {
                        var: "done",
                        expr: int(1),
                    },
                ],
            },
            Action {
                name: "Interrupted",
                guard: Some(open),
                updates: vec![
                    Update {
                        var: "off",
                        expr: in_range(var("off"), sub(cst("Size"), int(1))),
                    },
                    Update {
                        var: "done",
                        expr: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
                    },
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "NoSilentDrop",
                kind: InvariantKind::Claim,
                expr: or_(eq(var("done"), int(0)), eq(var("off"), cst("Size"))),
            },
            Invariant {
                name: "OffBounded",
                kind: InvariantKind::Bound,
                expr: le(var("off"), cst("Size")),
            },
        ],
    }
}
