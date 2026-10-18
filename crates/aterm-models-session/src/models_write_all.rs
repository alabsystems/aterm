// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The blocking write-all loop that drains a buffer to a descriptor.
//!
//! The defect took an EINTR or a short write for completion and reported success with the
//! tail unwritten; the fix loops until every byte is written.

use aterm_spec::derive::*;

/// The write-all loop over a buffer of `Size` bytes: `off` written so far, `done` the loop
/// reported completion.
///
/// `Progress` is the write that finishes the buffer; `Interrupted` is an EINTR (no
/// advance) or a short write (an advance that leaves bytes), any `off..Size-1` (the
/// nondeterministic `in_range` that makes this model hand-built). A partial advance that
/// does not finish is the same step as a short write, so these two actions reach every
/// state the loop can.
///
/// Mutant: `Buggy = 1` reports completion on an interrupted or short write (the dropped
/// tail).
///
/// Tier-0: `aterm-models-session/tests/derived_isolation.rs`. No Tier-1 binds it: the unix
/// `aterm_pty::write_all` this models has no production caller.
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
            // The loop reports completion only when every byte was written.
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
