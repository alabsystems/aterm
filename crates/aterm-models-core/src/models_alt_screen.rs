// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The DEC private mode 1049 alternate-screen round trip.
//!
//! `CSI ?1049 h` saves the cursor, switches the active buffer to the alternate screen and
//! clears it; the program then scribbles on the alternate screen alone; `CSI ?1049 l`
//! switches back and restores the cursor. An alternate screen that aliased the main one,
//! or a leave that forgot the restore, silently scribbles on the user's real screen.

use aterm_spec::derive::*;

/// The alternate-screen round trip, over one row of `Cells` cells.
///
/// State:
/// * `active` — 1 while the alternate screen is displayed.
/// * `entered` — the screen has been through `Enter` at least once.
/// * `touched` — the main cells differ from what they held at the first `Enter`: one
///   bit, since the round-trip law asks whether main changed, never which cell.
/// * `cursor`, `saved` — the cursor (0 home, else the 1-based cell last written) and the
///   cursor `Enter` saved.
///
/// `WriteMain` writes the main screen before the first entry (the "before" picture);
/// `Scribble` writes whatever cell the program addresses, so both choose the cursor
/// nondeterministically (`in_range`, which is why this model is hand-built).
///
/// Mutants: `Buggy = 1` aliases the alternate screen onto the main one (a scribble
/// touches main), `Buggy = 2` drops the cursor restore on `Leave`.
///
/// Tier-0: `aterm-models-core/tests/derived_isolation.rs`. Tier-1:
/// `aterm-core/tests/conformance_altscreen.rs` drives the real `Terminal` through
/// `WriteMain → Enter → Scribble → Leave` and checks each projected step on this model,
/// with the dropped restore and the aliased scribble as its negative controls.
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
            // Displaying main after a round trip, every main cell is what it was before the
            // first entry: alternate-screen writes never alias main.
            Invariant {
                name: "MainUntouched",
                kind: InvariantKind::Claim,
                expr: or_(
                    eq(var("active"), int(1)),
                    or_(eq(var("entered"), int(0)), eq(var("touched"), int(0))),
                ),
            },
            // Displaying main after a round trip, the cursor is the one `Enter` saved.
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
