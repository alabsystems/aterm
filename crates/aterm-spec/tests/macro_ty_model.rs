// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Desugaring equivalence tests for the `ty_model!` macro.

use aterm_spec::derive::{
    Action, Invariant, Model, StateVar, Update, add, and_, cst, eq, gt, if_, le, or_, sub, var,
};
use aterm_spec::ty_model;

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

fn ring_hand() -> Model {
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
            guard: Some(le(var("seq"), sub(cst("MaxSeq"), int_lit(1)))),
            updates: vec![
                Update {
                    var: "seq",
                    expr: add(var("seq"), int_lit(1)),
                },
                Update {
                    var: "lo",
                    expr: if_(
                        gt(
                            add(sub(add(var("seq"), int_lit(1)), var("lo")), int_lit(1)),
                            add(cst("Cap"), cst("Buggy")),
                        ),
                        add(var("lo"), int_lit(1)),
                        var("lo"),
                    ),
                },
            ],
        }],
        invariants: vec![Invariant {
            name: "LenBounded",
            kind: aterm_spec::derive::InvariantKind::Claim,
            expr: le(add(sub(var("seq"), var("lo")), int_lit(1)), cst("Cap")),
        }],
    }
}

fn ring_macro() -> Model {
    ty_model! {
        Ring {
            const MaxSeq = 6;
            const Cap = 3;
            const Buggy = 0;
            var seq = 0;
            var lo = 1;
            action Push when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                lo = if (seq + 1) - lo + 1 > Cap + Buggy { lo + 1 } else { lo };
            }
            invariant LenBounded: seq - lo + 1 <= Cap;
        }
    }
}

#[test]
fn ty_model_desugars_to_the_hand_built_model() {
    let cases: [(&str, Model, Model); 3] = [
        ("ring", ring_macro(), ring_hand()),
        (
            "unguarded",
            cursor_unguarded_macro(),
            cursor_unguarded_hand(),
        ),
        ("nested if/else", tiered_macro(), tiered_hand()),
    ];
    for (case, macro_built, hand_built) in cases {
        assert_eq!(macro_built.to_tla(), hand_built.to_tla(), "{case}: TLA+");
        assert_eq!(macro_built.to_cfg(), hand_built.to_cfg(), "{case}: cfg");
        assert_eq!(
            macro_built.transition_spec(),
            hand_built.transition_spec(),
            "{case}: open-Init form"
        );
    }
}

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
    assert_eq!(
        format!("{:#?}", grouping_macro()),
        format!("{:#?}", grouping_hand())
    );
}

#[test]
fn macro_reads_compiler_lexed_unicode_integer_suffixes() {
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
