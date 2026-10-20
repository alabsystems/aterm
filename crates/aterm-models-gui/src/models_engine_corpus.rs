// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The aterm-models-gui models aterm-spec's own unit tests run the engine on (`tab_nav_model`, `pane_tree_model`),
//! re-exported from this crate's root like every other. aterm-spec `#[path]`-mounts this file
//! (`src/test_models.rs`), so it holds only these and names only `aterm_spec::…` paths: an edit
//! to the rest of this crate's models then rebuilds no aterm-spec test and does not seed
//! aterm-spec, and with it every crate that depends on aterm-spec, into `verify`'s change cone.

use aterm_spec::derive::*;

/// Tab navigation: the per-window tab-strip index machine (`TabIndex` in `aterm-gui`,
/// `{ active, count }`). A window holds at least one tab and the renderer indexes the live tab
/// list with `active`, so the contract is `count >= 1` and `active <= count - 1`. The actions are
/// the four mutators `add` (`NewTab`), `switch_to` (`SelectTab`), `cycle(true)` (`Cycle`) and a
/// non-exit `close` (`Close`), bounded by `Cap` tabs.
///
/// `Buggy = 1` makes `Close` forget the re-clamp, so closing the last (active) tab leaves the
/// renderer indexing a tab that no longer exists; `ty` must refute `ActiveInRange`.
///
/// Hand-built rather than `ty_model!` because `SelectTab` needs the nondeterministic `in_range`
/// update, as `window_routing_model` does.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn tab_nav_model() -> Model {
    Model {
        name: "TabNav",
        // `Cap` bounds the tab count; `Buggy` turns the Close re-clamp off.
        consts: vec![("Cap", 4), ("Buggy", 0)],
        // A fresh window: one active tab.
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
            // add(): append a tab and switch to it; RHS read the pre-state, so `active' = count`.
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
            // switch_to(i) (Cmd-1..9 / switch_tab_in): `i` is user input, not a function of the
            // projection, so the update is nondeterministic over every valid index.
            Action {
                name: "SelectTab",
                guard: Some(gt(var("count"), int(1))),
                updates: vec![Update {
                    var: "active",
                    expr: in_range(int(0), sub(var("count"), int(1))),
                }],
            },
            // cycle(true) (Cmd-Shift-]): `(active + 1) % count` without `%`, exact under
            // `ActiveInRange`.
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
            // close(i) with count > 1: `count' = count - 1`, then re-clamp. Closing the last
            // (active) tab is the worst case: active drops to `count - 2`, matching `close`'s
            // `else if active >= count { active = count - 1 }` arm. `Buggy` forgets the clamp.
            // `Buggy = 2` closes the last tab too.
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
            // A window always has at least one tab.
            Invariant {
                name: "CountPositive",
                kind: InvariantKind::Claim,
                expr: gt(var("count"), int(0)),
            },
            // The renderer indexes the live tab list with `active`.
            Invariant {
                name: "ActiveInRange",
                kind: InvariantKind::Claim,
                expr: le(var("active"), sub(var("count"), int(1))),
            },
        ],
    }
}

/// Split-pane tree integrity: the focused leaf index of a tab's `PaneTree` (aterm-gui
/// `pane.rs`) never leaves `0..leaf_count-1`, so input and the cursor never route to a pane
/// that no longer exists. `Buggy = 1` forgets the Close re-point, leaving `focused` one past
/// the shrunk end. Only the `CloseOutcome::Collapsed` arm is modelled; a `LastPane` close is a
/// tab-machine transition (`tab_nav` / `window_routing`).
///
/// No invariant says the tree is non-empty: `SplitTree` has no empty value (a sole leaf's
/// `remove_leaf` reports `OnlyLeaf`), so it would only restate Close's guard. A removal elided
/// from release builds (inside a `debug_assert!`) keeps a valid state and breaks the transition,
/// which no invariant here sees; that fix carries its own both-profile regressions.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn pane_tree_model() -> Model {
    Model {
        name: "PaneTree",
        // `Cap` bounds the leaf count; `Buggy` turns the Close re-point off.
        consts: vec![("Cap", 4), ("Buggy", 0)],
        // A fresh tab: one focused leaf.
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
            // split_focused: the new pane is the second child, so the new last leaf in tree
            // order, and focus moves to it.
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
            // close_pane on a split tab (`CloseOutcome::Collapsed`): the surviving leaf focus
            // moves to is not a function of the projection, so focused' is nondeterministic.
            // `in_range` must be the top-level RHS (nested in an IF it renders a set as an `=`
            // RHS, a type error), so `Buggy` stretches the upper bound one past the new end.
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
        invariants: vec![
            // The focused leaf is in range.
            Invariant {
                name: "FocusInRange",
                kind: InvariantKind::Claim,
                expr: le(var("focused"), sub(var("leaf_count"), int(1))),
            },
        ],
    }
}
