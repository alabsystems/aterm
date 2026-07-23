// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Extra coverage for the `ty_model!` light-annotation surface, beyond the
//! single-action ring in `macro_ring.rs`. Each macro-built model is checked
//! against the EXACT `derive::*`-constructor model it should desugar to (so the
//! macro is pinned as pure sugar), and the generated TLA+ / interpreter are
//! exercised on paths the ring does not reach:
//!
//!   * an action with NO `when` guard          -> the `guard: None` branch
//!   * two actions with partial updates        -> disjunctive `Next` + `UNCHANGED`
//!   * a nested `if/else` (else-arm is an `if`) -> `tr_else` over `Expr::If`
//!   * the `>` operator in a guard, and `-`/`+` arithmetic in updates.

use aterm_spec::derive::{
    Action, Invariant, Model, StateVar, Update, add, cst, gt, if_, le, sub, var,
};
use aterm_spec::ty_model;
use std::collections::BTreeMap;

// ── A two-action cursor whose `Deliver` action has NO guard ──────────────────
// The hand-built reference: `Grow` is guarded, `Deliver` is unguarded (None) and
// only touches `cursor`, so `Grow` emits `UNCHANGED << cursor >>` and `Deliver`
// emits `UNCHANGED << seq >>`, with a disjunctive `Next == Grow \/ Deliver`.

fn cursor_unguarded_hand() -> Model {
    Model {
        name: "CursorU",
        consts: vec![("MaxSeq", 4)],
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
                name: "Grow",
                guard: Some(le(var("seq"), sub(cst("MaxSeq"), int_lit(1)))),
                updates: vec![Update {
                    var: "seq",
                    expr: add(var("seq"), int_lit(1)),
                }],
            },
            Action {
                name: "Deliver",
                guard: None, // no `when` clause
                updates: vec![Update {
                    var: "cursor",
                    expr: var("seq"),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "CursorBounded",
            expr: le(var("cursor"), var("seq")),
        }],
    }
}

// `MaxSeq` is declared `const` in the macro, so in the hand-built reference it is
// a `ConstRef`. Use the crate's `cst` for that and `int` for literals.
fn int_lit(i: i64) -> aterm_spec::derive::Expr {
    aterm_spec::derive::int(i)
}

fn cursor_unguarded_macro() -> Model {
    ty_model! {
        CursorU {
            const MaxSeq = 4;
            var seq = 0;
            var cursor = 0;
            action Grow when (seq <= MaxSeq - 1) {
                seq = seq + 1;
            }
            action Deliver {
                cursor = seq;
            }
            invariant CursorBounded: cursor <= seq;
        }
    }
}

#[test]
fn macro_unguarded_action_matches_hand_built_tla() {
    // The hand-built reference declares `MaxSeq` via `cst(..)` (it is a CONSTANT),
    // exactly as the macro classifies a `const`-declared identifier. We compare the
    // generated TLA+ text — the source of truth that `ty` consumes.
    let m = cursor_unguarded_macro();
    let h = cursor_unguarded_hand();
    assert_eq!(
        m.to_tla(),
        h.to_tla(),
        "macro must desugar to the same TLA+"
    );
}

#[test]
fn macro_unguarded_action_emits_no_guard_conjunct() {
    let tla = cursor_unguarded_macro().to_tla();
    // Guarded action keeps its guard; unguarded action starts directly with its
    // update conjunct (no leading predicate) and leaves `seq` UNCHANGED.
    assert!(
        tla.contains("Grow == seq =< MaxSeq - 1 /\\ seq' = seq + 1 /\\ UNCHANGED << cursor >>"),
        "{tla}"
    );
    assert!(
        tla.contains("Deliver == cursor' = seq /\\ UNCHANGED << seq >>"),
        "unguarded Deliver must have no guard conjunct: {tla}"
    );
    assert!(tla.contains("Next == Grow \\/ Deliver"), "{tla}");
}

#[test]
fn macro_unguarded_action_is_always_enabled_in_interpreter() {
    let m = cursor_unguarded_macro();
    let mut st = m.init_state();
    // Deliver has no guard -> always enabled, even from the initial state.
    assert!(m.action_enabled("Deliver", &st));
    assert!(m.fire("Deliver", &mut st));
    assert_eq!(
        st[&"cursor"], 0,
        "Deliver catches cursor up to seq (still 0)"
    );
    // Grow advances seq; Deliver then catches cursor up to it.
    assert!(m.fire("Grow", &mut st));
    assert!(m.fire("Deliver", &mut st));
    assert_eq!(st[&"cursor"], 1);
    assert!(m.check_invariant("CursorBounded", &st));
}

// ── A model exercising a NESTED if/else and the `>` guard operator ───────────
// `step` advances `n`; `bump` sets `level` via a nested if/else:
//   level = if n > 4 { 2 } else { if n > 2 { 1 } else { 0 } }
// This drives `tr_expr`'s `Expr::If` arm AND `tr_else` taking a nested `if`
// (rather than a `{ block }`), which the ring never exercises.

fn tiered_hand() -> Model {
    Model {
        name: "Tiered",
        consts: vec![],
        vars: vec![
            StateVar { name: "n", init: 0 },
            StateVar {
                name: "level",
                init: 0,
            },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Step",
                guard: Some(le(var("n"), int_lit(5))),
                updates: vec![Update {
                    var: "n",
                    expr: add(var("n"), int_lit(1)),
                }],
            },
            Action {
                name: "Bump",
                guard: None,
                updates: vec![Update {
                    var: "level",
                    expr: if_(
                        gt(var("n"), int_lit(4)),
                        int_lit(2),
                        if_(gt(var("n"), int_lit(2)), int_lit(1), int_lit(0)),
                    ),
                }],
            },
        ],
        invariants: vec![Invariant {
            name: "LevelBounded",
            expr: le(var("level"), int_lit(2)),
        }],
    }
}

fn tiered_macro() -> Model {
    ty_model! {
        Tiered {
            var n = 0;
            var level = 0;
            action Step when (n <= 5) {
                n = n + 1;
            }
            action Bump {
                level = if n > 4 { 2 } else { if n > 2 { 1 } else { 0 } };
            }
            invariant LevelBounded: level <= 2;
        }
    }
}

#[test]
fn macro_nested_if_else_matches_hand_built() {
    assert_eq!(tiered_macro().to_tla(), tiered_hand().to_tla());
}

#[test]
fn macro_nested_if_else_renders_nested_tla() {
    let tla = tiered_macro().to_tla();
    // No CONSTANT line (no consts declared).
    assert!(
        !tla.contains("CONSTANT"),
        "no consts -> no CONSTANT line: {tla}"
    );
    // The nested IF/THEN/ELSE renders with both arms parenthesized.
    assert!(
        tla.contains(
            "Bump == level' = (IF n > 4 THEN 2 ELSE (IF n > 2 THEN 1 ELSE 0)) /\\ UNCHANGED << n >>"
        ),
        "{tla}"
    );
}

#[test]
fn macro_nested_if_else_interpreter_tiers() {
    // Drive the nested-conditional update across the boundaries 0/2/4.
    let m = tiered_macro();
    let expect_level = |n: i64| -> i64 {
        if n > 4 {
            2
        } else if n > 2 {
            1
        } else {
            0
        }
    };
    for steps in 0..=5usize {
        let mut st: BTreeMap<&'static str, i64> = m.init_state();
        for _ in 0..steps {
            assert!(m.fire("Step", &mut st));
        }
        assert!(m.fire("Bump", &mut st));
        assert_eq!(
            st[&"level"],
            expect_level(steps as i64),
            "nested if/else tier wrong at n={steps}"
        );
        assert!(m.check_invariant("LevelBounded", &st));
    }
}

// ── The `>` operator + parenthesized subexpression in a guard ────────────────
// Guard: `(n - 1) > 0` exercises `Expr::Paren` unwrapping and `Gt`.

#[test]
fn macro_paren_and_gt_in_guard() {
    let m = ty_model! {
        Gated {
            var n = 0;
            action Tick when ((n - 1) > 0) {
                n = n + 1;
            }
            invariant Trivial: n <= 100;
        }
    };
    let tla = m.to_tla();
    // Parens are unwrapped by the macro; the rendered guard is `n - 1 > 0`.
    assert!(tla.contains("Tick == n - 1 > 0 /\\ n' = n + 1"), "{tla}");
    // Interpreter: guard `(n-1) > 0` is false at n=0 and n=1, true from n=2.
    let mut st = m.init_state();
    assert!(
        !m.action_enabled("Tick", &st),
        "n=0: (n-1)>0 is -1>0 = false"
    );
    st.insert("n", 1);
    assert!(
        !m.action_enabled("Tick", &st),
        "n=1: (n-1)>0 is 0>0 = false"
    );
    st.insert("n", 2);
    assert!(m.action_enabled("Tick", &st), "n=2: (n-1)>0 is 1>0 = true");
}
