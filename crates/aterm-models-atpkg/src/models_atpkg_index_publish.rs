// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg index channel's writer/reader contract: `atpkg-publish index` publishes
//! `atpkg-index-N`, and a client finds the newest by walking tags up from its verified floor.

use aterm_spec::derive::*;

const ABSENT: i64 = 0;

struct Publisher {
    phase: &'static str,
    base: &'static str,
    next: &'static str,
    actions: [&'static str; 6],
    draft: i64,
    done: i64,
}

const PUBLISHERS: [Publisher; 2] = [
    Publisher {
        phase: "pa",
        base: "ba",
        next: "na",
        actions: [
            "ReadA",
            "CreateA",
            "FinishA",
            "DieA",
            "ConvergeA",
            "RefuseA",
        ],
        draft: 1,
        done: 2,
    },
    Publisher {
        phase: "pb",
        base: "bb",
        next: "nb",
        actions: [
            "ReadB",
            "CreateB",
            "FinishB",
            "DieB",
            "ConvergeB",
            "RefuseB",
        ],
        draft: 3,
        done: 4,
    },
];

fn tag(k: i64) -> Expr {
    match k {
        2 => var("t2"),
        3 => var("t3"),
        4 => var("t4"),
        _ => unreachable!("the model's tags are 2..=4"),
    }
}

fn not(e: Expr) -> Expr {
    iff(e, bool_lit(false))
}

fn published(k: i64) -> Expr {
    if k == 1 {
        return bool_lit(true);
    }
    or_(eq(tag(k), int(2)), eq(tag(k), int(4)))
}

fn present(k: i64) -> Expr {
    if k == 1 {
        return bool_lit(true);
    }
    neq(tag(k), int(ABSENT))
}

fn highest(holds: fn(i64) -> Expr) -> Expr {
    if_(
        holds(4),
        int(4),
        if_(holds(3), int(3), if_(holds(2), int(2), int(1))),
    )
}

fn number_published(n: &Expr) -> Expr {
    or_(
        eq(n.clone(), int(1)),
        or_(
            and_(eq(n.clone(), int(2)), published(2)),
            or_(
                and_(eq(n.clone(), int(3)), published(3)),
                and_(eq(n.clone(), int(4)), published(4)),
            ),
        ),
    )
}

fn code_at(n: &Expr) -> Expr {
    if_(
        eq(n.clone(), int(2)),
        tag(2),
        if_(eq(n.clone(), int(3)), tag(3), tag(4)),
    )
}

fn write_at(n: &Expr, code: &Expr) -> Vec<Update> {
    [2, 3, 4]
        .into_iter()
        .map(|k| Update {
            var: ["", "", "t2", "t3", "t4"][k as usize],
            expr: if_(eq(n.clone(), int(k)), code.clone(), tag(k)),
        })
        .collect()
}

/// Strict client walk from verified floor `f`, stopping at the first missing tag.
fn walk_from(f: Expr) -> Expr {
    let from3 = if_(published(4), int(4), int(3));
    let from2 = if_(published(3), from3.clone(), int(2));
    let from1 = if_(published(2), from2.clone(), int(1));
    if_(
        eq(f.clone(), int(1)),
        from1,
        if_(
            eq(f.clone(), int(2)),
            from2,
            if_(eq(f, int(3)), from3, int(4)),
        ),
    )
}

fn number_admitted(p: &Publisher) -> Expr {
    or_(
        eq(cst("Buggy"), int(1)),
        and_(
            eq(var(p.next), add(var(p.base), int(1))),
            number_published(&var(p.base)),
        ),
    )
}

/// Formal model of `atpkg-publish index` writers (`Read`, `Create`, `Finish`, `Die`,
/// `Converge`, `Refuse`) and the client tag walk (`Walk`, `Seed2..=4`).
/// Verifies that published tags form a contiguous prefix (`OneRun`), the strict walk
/// from any floor reaches the newest index (`WalkLandsNewest`), and publishers never
/// clobber another publisher's tag (`NoOverwrite`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn atpkg_index_publish_walk_model() -> Model {
    let buggy = || eq(cst("Buggy"), int(1));
    let mut actions = Vec::new();
    for p in &PUBLISHERS {
        let [read, create, finish, die, converge, refuse] = p.actions;
        let (draft, done) = (p.draft, p.done);
        let own = |code: Expr| or_(eq(code.clone(), int(draft)), eq(code, int(done)));
        let at_next = code_at(&var(p.next));
        let admitted = number_admitted(p);
        let create_ok = and_(eq(at_next.clone(), int(ABSENT)), admitted.clone());
        let converge_ok = and_(
            or_(
                own(at_next.clone()),
                and_(buggy(), neq(at_next.clone(), int(ABSENT))),
            ),
            admitted,
        );
        let newest = if_(buggy(), highest(present), highest(published));
        let idle = |updates: &mut Vec<Update>| {
            for (var_name, value) in [(p.phase, 0), (p.base, 1), (p.next, 2)] {
                updates.push(Update {
                    var: var_name,
                    expr: int(value),
                });
            }
        };
        actions.push(Action {
            name: read,
            guard: Some(and_(
                eq(var(p.phase), int(0)),
                le(add(newest.clone(), int(1)), int(4)),
            )),
            updates: vec![
                Update {
                    var: p.phase,
                    expr: int(1),
                },
                Update {
                    var: p.base,
                    expr: newest.clone(),
                },
                Update {
                    var: p.next,
                    expr: if_(
                        and_(buggy(), le(add(newest.clone(), int(2)), int(4))),
                        add(newest.clone(), int(2)),
                        add(newest, int(1)),
                    ),
                },
            ],
        });
        let mut created = write_at(&var(p.next), &int(draft));
        created.push(Update {
            var: p.phase,
            expr: int(2),
        });
        actions.push(Action {
            name: create,
            guard: Some(and_(eq(var(p.phase), int(1)), create_ok.clone())),
            updates: created,
        });
        let mut finished = write_at(&var(p.next), &int(done));
        idle(&mut finished);
        actions.push(Action {
            name: finish,
            guard: Some(eq(var(p.phase), int(2))),
            updates: finished,
        });
        let mut died = Vec::new();
        idle(&mut died);
        actions.push(Action {
            name: die,
            guard: Some(eq(var(p.phase), int(2))),
            updates: died,
        });
        let mut converged = write_at(&var(p.next), &int(done));
        converged.push(Update {
            var: "overwrote",
            expr: if_(own(at_next.clone()), var("overwrote"), int(1)),
        });
        idle(&mut converged);
        actions.push(Action {
            name: converge,
            guard: Some(and_(eq(var(p.phase), int(1)), converge_ok.clone())),
            updates: converged,
        });
        let mut refused = Vec::new();
        idle(&mut refused);
        actions.push(Action {
            name: refuse,
            guard: Some(and_(
                eq(var(p.phase), int(1)),
                and_(not(create_ok), not(converge_ok)),
            )),
            updates: refused,
        });
    }
    actions.push(Action {
        name: "Walk",
        guard: None,
        updates: vec![Update {
            var: "floor",
            expr: walk_from(var("floor")),
        }],
    });
    for k in 2..=4 {
        actions.push(Action {
            name: ["", "", "Seed2", "Seed3", "Seed4"][k as usize],
            guard: Some(published(k)),
            updates: vec![Update {
                var: "floor",
                expr: int(k),
            }],
        });
    }
    let state = |name: &'static str, init: i64| StateVar { name, init };
    Model {
        name: "AtpkgIndexPublishWalk",
        consts: vec![("Buggy", 0)],
        vars: vec![
            state("t2", ABSENT),
            state("t3", ABSENT),
            state("t4", ABSENT),
            // Publisher phases: 0 idle, 1 holds a baseline, 2 created a draft.
            state("pa", 0),
            state("ba", 1),
            state("na", 2),
            state("pb", 0),
            state("bb", 1),
            state("nb", 2),
            state("floor", 1),
            state("overwrote", 0),
        ],
        fn_vars: vec![],
        actions,
        invariants: vec![
            Invariant {
                name: "OneRun",
                kind: InvariantKind::Claim,
                expr: and_(
                    or_(not(published(3)), published(2)),
                    or_(not(published(4)), published(3)),
                ),
            },
            Invariant {
                name: "WalkLandsNewest",
                kind: InvariantKind::Claim,
                expr: eq(walk_from(var("floor")), highest(published)),
            },
            Invariant {
                name: "NoOverwrite",
                kind: InvariantKind::Claim,
                expr: eq(var("overwrote"), int(0)),
            },
        ],
    }
}
