// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The atpkg index channel's writer/reader contract: `tools/atpkg-index.sh` publishes
//! `atpkg-index-N`, and a client finds the newest by walking tags up from its verified floor.

use super::*;

/// Tag state: absent. Tags 2..=4 each hold one of these codes; the channel's first index,
/// `atpkg-index-1`, is published before the model starts.
const ABSENT: i64 = 0;

/// One publisher's names and tag codes. A draft is what a `gh release create` killed
/// between its create and its publish leaves behind (gh 2.96.0): visible to a publisher
/// holding write access, answered 404 by the download host every client reads.
struct Publisher {
    /// Phase: 0 idle, 1 holds a baseline, 2 created a draft.
    phase: &'static str,
    /// The baseline's index number.
    base: &'static str,
    /// The number this publisher publishes.
    next: &'static str,
    /// Its actions: read, create, finish, die, converge, refuse.
    actions: [&'static str; 6],
    /// Its tag codes.
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

/// `atpkg-index-k` answers on the download host: tag 1 always, a draft never.
fn published(k: i64) -> Expr {
    if k == 1 {
        return bool_lit(true);
    }
    or_(eq(tag(k), int(2)), eq(tag(k), int(4)))
}

/// A release exists under the tag, draft or published — what `gh` shows a publisher with
/// write access.
fn present(k: i64) -> Expr {
    if k == 1 {
        return bool_lit(true);
    }
    neq(tag(k), int(ABSENT))
}

/// The highest tag `holds` is true of (1 when none of 2..=4 is).
fn highest(holds: fn(i64) -> Expr) -> Expr {
    if_(
        holds(4),
        int(4),
        if_(holds(3), int(3), if_(holds(2), int(2), int(1))),
    )
}

/// Whether the number `n` (an expression over 1..=4) is a published tag.
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

/// The code under the number `n` (an expression over 2..=4).
fn code_at(n: &Expr) -> Expr {
    if_(
        eq(n.clone(), int(2)),
        tag(2),
        if_(eq(n.clone(), int(3)), tag(3), tag(4)),
    )
}

/// The three tag updates that write `code` under the number `n`, leaving the others.
fn write_at(n: &Expr, code: &Expr) -> Vec<Update> {
    [2, 3, 4]
        .into_iter()
        .map(|k| Update {
            var: ["", "", "t2", "t3", "t4"][k as usize],
            expr: if_(eq(n.clone(), int(k)), code.clone(), tag(k)),
        })
        .collect()
}

/// THE READER: a strict walk from the verified floor `f` — HEAD `f+1`, `f+2`, … on the
/// download host and stop at the first number that does not answer. The client's own walk
/// may do more (look past one missing number), never less, so a channel this walk crosses
/// is one every client crosses.
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

/// Publishing index N is admitted: the number is the baseline's successor and the baseline
/// is PUBLISHED (the indexer's two guards; `Buggy = 1` drops both).
fn number_admitted(p: &Publisher) -> Expr {
    or_(
        eq(cst("Buggy"), int(1)),
        and_(
            eq(var(p.next), add(var(p.base), int(1))),
            number_published(&var(p.base)),
        ),
    )
}

/// `tools/atpkg-index.sh` (the WRITER) and the client's tag walk (the READER) as ONE
/// contract. Two publishers — the ALab lane, the Linux lane, the rustc-group lane and a
/// hand publish all run the same indexer — each READ a baseline (the channel's newest
/// published index), then publish `atpkg-index-<baseline + 1>`:
///
/// * `Create` — the tag is absent: `gh release create` makes a draft, uploads, and
///   publishes it (`Finish`); a run killed in between (`Die`) leaves the draft.
/// * `Converge` — the tag holds THIS publisher's identical bytes (its own killed run): the
///   missing assets are uploaded and a draft is published.
/// * `Refuse` — anything else: another publisher's bytes under the tag (the upload is a
///   compare-and-swap, never a clobber), a baseline that is not published, or a number that
///   is not the baseline's successor. Nothing is written; the next run re-reads.
///
/// A client (`Walk`) moves its verified floor to where the strict walk lands; `Seed<k>` puts
/// a fresh store's floor on any published index, so the walk is judged from every floor.
///
/// Invariants: the published tags are ONE RUN from the first (`OneRun`), so the walk from
/// every verified floor lands the newest published index (`WalkLandsNewest`), and no
/// publisher ever replaces a tag another one wrote (`NoOverwrite`).
///
/// `Buggy = 1` is the publish path before 2026-09-23: a baseline read that counts DRAFTS (a
/// `gh release list` without `--exclude-drafts`, under a token that sees them), a number
/// past the baseline's successor (the indexer only warned on one), no published-baseline
/// check, and an upload that clobbers instead of comparing bytes. Each invariant has its
/// own counterexample: A publishing 3 over an absent 2, or B publishing 4 on A's orphaned
/// draft at 3, is a hole the walk from 1 cannot cross; B converging onto A's tag overwrites
/// it.
///
/// Tier-1 (`crates/atpkg/src/net/index_publish_conformance.rs`) drives the real indexer's
/// number and compare-and-swap decisions from every modelled publisher state against a
/// fixture channel, and the real tag walk over every reachable channel; its negative
/// control is a `Buggy = 1` hole that the real walk does not cross either.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
        // The baseline READ: the newest published tag (Buggy: the newest release of any
        // kind). The number is its successor (Buggy: the one after, where there is one —
        // an INDEX_BUILD the old indexer only warned about).
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
                expr: and_(
                    or_(not(published(3)), published(2)),
                    or_(not(published(4)), published(3)),
                ),
            },
            Invariant {
                name: "WalkLandsNewest",
                expr: eq(walk_from(var("floor")), highest(published)),
            },
            Invariant {
                name: "NoOverwrite",
                expr: eq(var("overwrote"), int(0)),
            },
        ],
    }
}
