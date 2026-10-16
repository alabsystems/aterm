// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! `ty_model!` paths the single-action ring in `macro_ring.rs` does not reach: an unguarded
//! action, partial updates, a nested if/else and a parenthesized `>` guard. Each macro-built
//! model must derive the same TLA+ as its hand-built `derive::*` counterpart.

use aterm_spec::derive::{
    Action, Invariant, Model, StateVar, Update, add, and_, cst, eq, gt, if_, le, or_, sub, var,
};
use aterm_spec::ty_model;
use std::collections::BTreeMap;

// Two actions with partial updates, one unguarded: each emits `UNCHANGED` for the other's
// variable and `Next` is their disjunction.

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
            kind: aterm_spec::derive::InvariantKind::Claim,
            expr: le(var("cursor"), var("seq")),
        }],
    }
}

// `MaxSeq` is a `const` in the macro, so the hand-built models spell it `cst(..)`.
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
    // The unguarded action starts with its update conjunct.
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
    assert!(m.action_enabled("Deliver", &st));
    assert!(m.fire("Deliver", &mut st));
    assert_eq!(
        st[&"cursor"], 0,
        "Deliver catches cursor up to seq (still 0)"
    );
    assert!(m.fire("Grow", &mut st));
    assert!(m.fire("Deliver", &mut st));
    assert_eq!(st[&"cursor"], 1);
    assert!(m.check_invariant("CursorBounded", &st));
}

// A nested if/else (the else arm is an `if`, not a block) exercises `tr_else` over `Expr::If`.

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
            kind: aterm_spec::derive::InvariantKind::Claim,
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
    assert!(
        !tla.contains("CONSTANT"),
        "no consts -> no CONSTANT line: {tla}"
    );
    // Both arms of the nested IF are parenthesized.
    assert!(
        tla.contains(
            "Bump == level' = (IF n > 4 THEN 2 ELSE (IF n > 2 THEN 1 ELSE 0)) /\\ UNCHANGED << n >>"
        ),
        "{tla}"
    );
}

#[test]
fn macro_nested_if_else_interpreter_tiers() {
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

// A parenthesized guard exercises the `Expr::Paren` arm and `>`.

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
    // The macro unwraps the parens.
    assert!(tla.contains("Tick == n - 1 > 0 /\\ n' = n + 1"), "{tla}");
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

// ── Grouping: an expression reads as Rust reads it ───────────────────────────
// `ty_model!` reads its expressions itself (aterm-macro-support, no syn; SP.8 of
// docs/AUDIT-third-party-2026-10-15.md), so how it groups one is pinned here
// against the builders written out: `+`/`-` fold left, `&&` binds tighter than
// `||`, a comparison tighter than both, parentheses regroup (a parenthesized
// comparison may be compared again), an `if` is an operand, and integers may be
// written in any base, with `_`, and negative where a constant is declared.

fn grouping_macro() -> Model {
    ty_model! {
        Grouping {
            const Max = 0x10;
            const Floor = -2;
            var a = 1_0;
            var b = 0b11;
            action Mix when (a - b + 1 <= Max && b > Floor || a == 0 && b == 0) {
                a = a - (b + 1) + if a > b { 1 } else if (a <= b) == (b > 0) { 2 } else { 3 };
                b = (a + b);
            }
            invariant Bounded: a + b - Floor <= Max + Max;
        }
    }
}

fn grouping_hand() -> Model {
    Model {
        name: "Grouping",
        consts: vec![("Max", 16), ("Floor", -2)],
        vars: vec![
            StateVar {
                name: "a",
                init: 10,
            },
            StateVar { name: "b", init: 3 },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Mix",
            guard: Some(or_(
                and_(
                    le(add(sub(var("a"), var("b")), int_lit(1)), cst("Max")),
                    gt(var("b"), cst("Floor")),
                ),
                and_(eq(var("a"), int_lit(0)), eq(var("b"), int_lit(0))),
            )),
            updates: vec![
                Update {
                    var: "a",
                    expr: add(
                        sub(var("a"), add(var("b"), int_lit(1))),
                        if_(
                            gt(var("a"), var("b")),
                            int_lit(1),
                            if_(
                                eq(le(var("a"), var("b")), gt(var("b"), int_lit(0))),
                                int_lit(2),
                                int_lit(3),
                            ),
                        ),
                    ),
                },
                Update {
                    var: "b",
                    expr: add(var("a"), var("b")),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "Bounded",
            kind: aterm_spec::derive::InvariantKind::Claim,
            expr: le(
                sub(add(var("a"), var("b")), cst("Floor")),
                add(cst("Max"), cst("Max")),
            ),
        }],
    }
}

#[test]
fn macro_groups_an_expression_as_rust_does() {
    // Debug, not TLA+: the rendering parenthesizes for TLA+'s own grouping, and
    // two trees can render alike; the trees themselves must be equal.
    assert_eq!(
        format!("{:#?}", grouping_macro()),
        format!("{:#?}", grouping_hand())
    );
}

#[test]
fn macro_reads_compiler_lexed_unicode_integer_suffixes() {
    // U+0315 is a valid identifier continuation, but not alphanumeric. These
    // are compiler-lexed literal tokens; a second ASCII-like suffix check
    // must not reject them. Ordinary suffixes and the signed boundary remain
    // controls for the numeric decoding shared by declarations/expressions.
    let actual = ty_model! {
        UnicodeIntegerSuffix {
            const Limit = 2u̕;
            const Floor = -9223372036854775808u̕;
            scenario Limit = 3u̕;
            var x = 0u8;
            action Step when (x <= 1u̕) { x = x + 1u̕; }
            bound Bounded: x <= Limit;
        }
    };
    let expected = Model {
        name: "UnicodeIntegerSuffix",
        consts: vec![("Limit", 2), ("Floor", i64::MIN)],
        vars: vec![StateVar { name: "x", init: 0 }],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Step",
            guard: Some(le(var("x"), int_lit(1))),
            updates: vec![Update {
                var: "x",
                expr: add(var("x"), int_lit(1)),
            }],
        }],
        invariants: vec![Invariant {
            name: "Bounded",
            kind: aterm_spec::derive::InvariantKind::Bound,
            expr: le(var("x"), cst("Limit")),
        }],
    };
    assert_eq!(format!("{actual:#?}"), format!("{expected:#?}"));
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configs| configs[0].name == actual.name)
        .expect("the Unicode-suffixed scenario is registered");
    assert!(
        configurations
            .iter()
            .any(|m| m.consts.contains(&("Limit", 3)))
    );
}

// ── Writing a model registers it ─────────────────────────────────────────────
// `ty_model!` submits what it writes, `#[model]` a hand-built model and
// `#[liveness(model)]` an obligation with its model, so the sweeps reach each
// one without a list to keep.

#[aterm_spec::model]
fn registered_by_attribute() -> Model {
    let mut m = cursor_unguarded_hand();
    m.name = "RegisteredByAttribute";
    m
}

#[aterm_spec::liveness(registered_by_attribute)]
fn registered_obligation() -> aterm_spec::derive::Liveness {
    aterm_spec::derive::Liveness {
        name: "RegisteredObligation",
        goal: le(var("cursor"), var("seq")),
        weak: vec![],
        strong: vec![],
        mutants: vec![],
    }
}

/// A model only an obligation names: stating the obligation registers it.
fn stated_over_only() -> Model {
    let mut m = cursor_unguarded_hand();
    m.name = "StatedOverOnly";
    m
}

#[aterm_spec::liveness(stated_over_only)]
fn stated_over_only_obligation() -> aterm_spec::derive::Liveness {
    aterm_spec::derive::Liveness {
        name: "StatedOverOnlyObligation",
        ..registered_obligation()
    }
}

#[test]
fn writing_a_model_registers_it() {
    let written = ty_model! {
        RegisteredByMacro {
            var x = 0;
            action Step when (x <= 0) { x = x + 1; }
            invariant Small: x <= 1;
        }
    };
    let registered = aterm_spec::xref::model_registry();
    for (name, tla) in [
        ("RegisteredByMacro", written.to_tla()),
        ("RegisteredByAttribute", registered_by_attribute().to_tla()),
        ("StatedOverOnly", stated_over_only().to_tla()),
    ] {
        let found: Vec<_> = registered.iter().filter(|m| m.name == name).collect();
        assert_eq!(found.len(), 1, "{name} is registered once");
        assert_eq!(found[0].to_tla(), tla, "{name} is registered as written");
    }
    // A scenario is a configuration swept beside the committed one.
    let capped = ty_model! {
        RegisteredWithScenario {
            const Buggy = 0;
            const Cap = 0;
            scenario Cap = 2;
            var x = 0;
            action Step when (x <= Cap) { x = x + 1; }
            bound XBounded: x <= 3;
        }
    };
    let configs = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configs| configs[0].name == capped.name)
        .expect("the model is registered");
    let consts: Vec<_> = configs.iter().map(|c| c.consts.clone()).collect();
    assert_eq!(
        consts,
        [
            vec![("Buggy", 0), ("Cap", 0)],
            vec![("Buggy", 0), ("Cap", 2)]
        ]
    );
    let obligations = aterm_spec::xref::liveness_registry();
    let (model, live) = obligations
        .iter()
        .find(|(_, live)| live.name == "RegisteredObligation")
        .expect("the obligation is registered");
    assert_eq!(model.name, "RegisteredByAttribute");
    assert_eq!(live.goal.to_tla(), registered_obligation().goal.to_tla());
}
