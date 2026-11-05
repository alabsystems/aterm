// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine-number negotiation (`docs/DESIGN-machine-names-2026-11-04.md` §6.6), as seven small
//! models rather than one: a model of every action at once over four nodes and three numbers has
//! far more reachable states than the interpreter walks (`aterm_spec::interp`'s bound), and each
//! law needs only the part of the protocol its mutants break. Each model is built as data with
//! the derive helpers, because a holder or a primary is a rank over several nodes, which a
//! closure writes once and the model instantiates per node.
//!
//! The models keep each node's record fields the way `aterm_types::machine_name` reads them: its
//! NAME, class rank (0 for `chosen` and `seeded`, 1 for `auto`), `since`, `base`, `tgen` and,
//! where a machine has two nodes, `sgen` and `act`. A `Settle` action stores what `settle`
//! answers over the records in view (`phase` 1 until it has run), a person's presses are guarded
//! by what `check` says of them, and a `Pick` stores what `choose` takes. aterm-types'
//! conformance test (`machine_name::model_tests`) walks every reachable state and runs the real
//! functions on records built from it.

use aterm_spec::derive::*;

/// `Buggy = k`: mutant `k` is on.
fn bug(k: i64) -> Expr {
    eq(cst("Buggy"), int(k))
}

/// None of the mutants `ks` is on.
fn none_of(ks: &[i64]) -> Expr {
    all(ks.iter().map(|k| neq(cst("Buggy"), int(*k))))
}

fn not(e: Expr) -> Expr {
    eq(if_(e, int(1), int(0)), int(0))
}

fn lt(a: Expr, b: Expr) -> Expr {
    gt(b, a)
}

fn all(es: impl IntoIterator<Item = Expr>) -> Expr {
    es.into_iter().reduce(and_).unwrap_or(bool_lit(true))
}

fn any(es: impl IntoIterator<Item = Expr>) -> Expr {
    es.into_iter().reduce(or_).unwrap_or(bool_lit(false))
}

fn implies(a: Expr, b: Expr) -> Expr {
    or_(not(a), b)
}

/// 1 when `e` holds, else 0.
fn bit(e: Expr) -> Expr {
    if_(e, int(1), int(0))
}

/// A flag that stays set once `now` holds.
fn latch(name: &'static str, now: Expr) -> Update {
    up(name, bit(or_(eq(var(name), int(1)), now)))
}

/// A boolean `correct`, or under mutant `k` its alternative.
fn mutated(correct: Expr, alts: Vec<(i64, Expr)>) -> Expr {
    let ks: Vec<i64> = alts.iter().map(|(k, _)| *k).collect();
    let fixed = and_(none_of(&ks), correct);
    any(alts
        .into_iter()
        .map(|(k, e)| and_(bug(k), e))
        .chain([fixed]))
}

/// An integer `correct`, or under mutant `k` its alternative.
fn mutated_int(correct: Expr, alts: Vec<(i64, Expr)>) -> Expr {
    alts.into_iter()
        .rev()
        .fold(correct, |rest, (k, e)| if_(bug(k), e, rest))
}

/// A boolean known per mutant when the model is built: `correct` with none of `alts`' mutants on.
fn literal(correct: bool, alts: &[(i64, bool)]) -> Expr {
    let ks: Vec<i64> = alts.iter().map(|(k, _)| *k).collect();
    let on = alts.iter().filter(|(_, b)| *b).map(|(k, _)| bug(*k));
    any(on.chain(correct.then(|| none_of(&ks))))
}

/// `vals[i]` where `idx` is `ids[i]`, and the last value for any other `idx`.
fn sel(idx: &Expr, ids: &[i64], vals: Vec<Expr>) -> Expr {
    let mut pairs: Vec<(i64, Expr)> = ids.iter().copied().zip(vals).collect();
    let last = pairs.pop().map_or(int(0), |(_, v)| v);
    pairs
        .into_iter()
        .rev()
        .fold(last, |rest, (id, v)| if_(eq(idx.clone(), int(id)), v, rest))
}

fn max2(a: Expr, b: Expr) -> Expr {
    if_(gt(a.clone(), b.clone()), a, b)
}

fn up(var: &'static str, expr: Expr) -> Update {
    Update { var, expr }
}

fn st(name: &'static str, init: i64) -> StateVar {
    StateVar { name, init }
}

fn law(name: &'static str, expr: Expr) -> Invariant {
    Invariant {
        name,
        expr,
        kind: InvariantKind::Claim,
    }
}

fn bound(name: &'static str, expr: Expr) -> Invariant {
    Invariant {
        name,
        expr,
        kind: InvariantKind::Bound,
    }
}

/// Whether the first key is before the second by `levels` (each a strict `<` and an `=`), and
/// `last` on a tie at every level.
fn lex(levels: Vec<(Expr, Expr)>, last: Expr) -> Expr {
    levels
        .into_iter()
        .rev()
        .fold(last, |rest, (lt, eq)| or_(lt, and_(eq, rest)))
}

/// A strict `<` and an `=` of two values, one level of [`lex`].
fn level(a: &Expr, b: &Expr) -> (Expr, Expr) {
    (lt(a.clone(), b.clone()), eq(a.clone(), b.clone()))
}

/// Each update of `fields` written where `by` holds, the variable kept elsewhere.
fn write_where(by: &Expr, fields: Vec<(&'static str, Expr)>) -> Vec<Update> {
    fields
        .into_iter()
        .map(|(name, value)| up(name, if_(by.clone(), value, var(name))))
        .collect()
}

/// One node's claim on the number a holder is asked of, as settle's step 3 ranks it (§6.1).
#[derive(Clone)]
struct Claim {
    id: i64,
    on: Expr,
    class: Expr,
    since: Expr,
    base: Expr,
    tgen: Expr,
}

impl Claim {
    /// The step-3 key, `(class, since, base)` before `other`'s, then `last` on a tie.
    fn before(&self, other: &Self, last: Expr) -> Expr {
        lex(
            vec![
                level(&self.class, &other.class),
                level(&self.since, &other.since),
                level(&self.base, &other.base),
            ],
            last,
        )
    }

    /// `other` drops this claim (§6.1 step 3): the same `base`, a higher `tgen`.
    fn dropped_by(&self, other: &Self) -> Expr {
        and_(
            eq(other.base.clone(), self.base.clone()),
            gt(other.tgen.clone(), self.tgen.clone()),
        )
    }
}

/// The holder of the number `claims` are on: 0 for none, else the winning claim's id. `drops(d,
/// c)` is whether `d` drops `c`, and `before(c, d)` whether `c` ranks first.
fn holder(
    claims: &[Claim],
    drops: &dyn Fn(&Claim, &Claim) -> Expr,
    before: &dyn Fn(&Claim, &Claim) -> Expr,
) -> Expr {
    let others = |c: &Claim| claims.iter().filter(|d| d.id != c.id).collect::<Vec<_>>();
    let stands = |c: &Claim| {
        and_(
            c.on.clone(),
            all(others(c)
                .into_iter()
                .map(|d| not(and_(d.on.clone(), drops(d, c))))),
        )
    };
    let wins = |c: &Claim| {
        and_(
            stands(c),
            all(others(c)
                .into_iter()
                .map(|d| or_(not(stands(d)), before(c, d)))),
        )
    };
    claims
        .iter()
        .rev()
        .fold(int(0), |rest, c| if_(wins(c), int(c.id), rest))
}

/// Settle's step 3 with no mutant: the same `base` and a higher `tgen` drop a claim, then the
/// lowest `(class, since, base, node)` holds.
fn settled_holder(claims: &[Claim]) -> Expr {
    holder(claims, &|d, c| c.dropped_by(d), &|c, d| {
        c.before(d, bool_lit(c.id < d.id))
    })
}

/// Step 3 over one number N (m1) and three machines of one node each (§6.1), with the presses
/// `check` decides (§5.3; §6.4 A person's rename): `Rename` onto N when it is free or held
/// `auto`, `Take` of a `chosen` holder's claim, and `TakeTwice`, the two other nodes each taking
/// it, neither hearing the other. `Claim` is an automatic claim made on a view that lacks the
/// others (a partition), and `Move` a displaced node's move off N (`n` 2: a number of its own,
/// with N lost in `was`). `who` is the node whose act comes next; nodes 2 and 3 settle as two
/// readers (`ha`, `hb`), and `tk` is the latest taker that should hold.
///
/// Mutants: 1 ties broken by "me first", 2 the step-3 key without the node ids (each reader takes
/// the first claim in its own input order), 3 both sides of a tie believe they lose, 4 a take
/// drops a claim of its chain at an equal `tgen` too (TakeTwice's two takes drop each other), 5
/// class ignored, 6 `auto` resolved latest-wins, 7 the drop keyed on the immediately taken node
/// (`k`), not `base`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_holder_model() -> Model {
    const ID: [i64; 3] = [1, 2, 3];
    const NUM: [&str; 3] = ["n1", "n2", "n3"];
    const CLASS: [&str; 3] = ["c1", "c2", "c3"];
    const SINCE: [&str; 3] = ["s1", "s2", "s3"];
    const BASE: [&str; 3] = ["b1", "b2", "b3"];
    const TGEN: [&str; 3] = ["t1", "t2", "t3"];
    const TOOK: [&str; 3] = ["k1", "k2", "k3"];
    const ME_FIRST: i64 = 1;
    const INPUT_ORDER: i64 = 2;
    const BOTH_LOSE: i64 = 3;
    const DROP_EQUAL: i64 = 4;
    const NO_CLASS: i64 = 5;
    const LATEST_AUTO: i64 = 6;
    const DROP_TAKEN: i64 = 7;
    let v = var;
    let claims: Vec<Claim> = (0..3)
        .map(|i| Claim {
            id: ID[i],
            on: eq(v(NUM[i]), int(1)),
            class: v(CLASS[i]),
            since: v(SINCE[i]),
            base: v(BASE[i]),
            tgen: v(TGEN[i]),
        })
        .collect();
    let took = |c: &Claim| {
        let at = ID.iter().position(|id| *id == c.id).unwrap_or(0);
        v(TOOK[at])
    };
    let drops = |d: &Claim, c: &Claim| {
        mutated(
            c.dropped_by(d),
            vec![
                (
                    DROP_EQUAL,
                    and_(
                        eq(d.base.clone(), c.base.clone()),
                        le(c.tgen.clone(), d.tgen.clone()),
                    ),
                ),
                (
                    DROP_TAKEN,
                    and_(eq(took(d), int(c.id)), gt(d.tgen.clone(), c.tgen.clone())),
                ),
            ],
        )
    };
    // How reader `r` (node 2 or 3) ranks `c` before `d`.
    let ranked_by = |r: i64| {
        move |c: &Claim, d: &Claim| {
            let autos = and_(eq(c.class.clone(), int(1)), eq(d.class.clone(), int(1)));
            let latest = and_(bug(LATEST_AUTO), autos);
            let since_first = or_(
                and_(latest.clone(), gt(c.since.clone(), d.since.clone())),
                and_(not(latest), lt(c.since.clone(), d.since.clone())),
            );
            let tie = literal(
                c.id < d.id,
                &[
                    (ME_FIRST, c.id == r || (d.id != r && c.id < d.id)),
                    (INPUT_ORDER, if r == 2 { c.id < d.id } else { c.id > d.id }),
                    (BOTH_LOSE, false),
                ],
            );
            lex(
                vec![
                    (
                        and_(none_of(&[NO_CLASS]), lt(c.class.clone(), d.class.clone())),
                        or_(bug(NO_CLASS), eq(c.class.clone(), d.class.clone())),
                    ),
                    (since_first, eq(c.since.clone(), d.since.clone())),
                    level(&c.base, &d.base),
                ],
                tie,
            )
        }
    };
    let settled = || eq(v("phase"), int(0));
    let fresh = |e: Expr| or_(eq(v("phase"), int(1)), e);
    let who = || v("who");
    let of = |idx: Expr, field: [&'static str; 3]| sel(&idx, &ID, field.map(v).to_vec());
    let holder_class = || of(v("ha"), CLASS);
    let holder_tgen = || of(v("ha"), TGEN);
    let room = || le(v("clock"), cst("ClockMax"));
    let bump = || up("clock", add(v("clock"), int(1)));
    let pending = || up("phase", int(1));
    // Node `i`'s whole claim: NAME, class, since, base, tgen and the node it took.
    let claim_of = |i: usize, fields: [Expr; 6]| {
        let [n, c, s, b, t, k] = fields;
        vec![
            (NUM[i], n),
            (CLASS[i], c),
            (SINCE[i], s),
            (BASE[i], b),
            (TGEN[i], t),
            (TOOK[i], k),
        ]
    };
    let own = |i: usize, n: i64, c: i64, s: Expr| {
        claim_of(i, [int(n), int(c), s, int(ID[i]), int(0), int(0)])
    };
    let take = |i: usize| {
        claim_of(
            i,
            [
                int(1),
                int(0),
                of(v("ha"), SINCE),
                of(v("ha"), BASE),
                add(holder_tgen(), int(1)),
                v("ha"),
            ],
        )
    };
    let by_who = |i: usize| eq(who(), int(ID[i]));
    let by_taker = |i: usize| neq(v("ha"), int(ID[i]));
    let each = |by: &dyn Fn(usize) -> Expr, f: &dyn Fn(usize) -> Vec<(&'static str, Expr)>| {
        (0..3)
            .flat_map(|i| write_where(&by(i), f(i)))
            .collect::<Vec<_>>()
    };
    let then = |mut updates: Vec<Update>, more: Vec<Update>| {
        updates.extend(more);
        updates
    };
    let takeable = || {
        all([
            settled(),
            neq(v("ha"), int(0)),
            eq(holder_class(), int(0)),
            le(add(holder_tgen(), int(1)), cst("TMax")),
        ])
    };
    let mut vars = Vec::new();
    for i in 0..3 {
        vars.extend([
            st(NUM[i], 0),
            st(CLASS[i], 1),
            st(SINCE[i], 0),
            st(BASE[i], ID[i]),
            st(TGEN[i], 0),
            st(TOOK[i], 0),
        ]);
    }
    vars.extend([
        st("clock", 8),
        st("who", 1),
        st("phase", 1),
        st("ha", 0),
        st("hb", 0),
        st("tk", 0),
    ]);
    Model {
        name: "MachineNumberHolder",
        consts: vec![("Buggy", 0), ("ClockMax", 11), ("TMax", 2)],
        vars,
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(eq(v("phase"), int(1))),
                updates: vec![
                    up("ha", holder(&claims, &drops, &ranked_by(2))),
                    up("hb", holder(&claims, &drops, &ranked_by(3))),
                    up("who", in_range(int(1), int(3))),
                    up("phase", int(0)),
                ],
            },
            Action {
                name: "Claim",
                guard: Some(all([settled(), eq(of(who(), NUM), int(0)), room()])),
                updates: then(
                    each(&by_who, &|i| own(i, 1, 1, v("clock"))),
                    vec![bump(), pending()],
                ),
            },
            Action {
                name: "Move",
                guard: Some(all([
                    settled(),
                    eq(of(who(), NUM), int(1)),
                    neq(v("ha"), who()),
                    neq(v("ha"), int(0)),
                ])),
                updates: then(each(&by_who, &|i| own(i, 2, 1, int(0))), vec![pending()]),
            },
            Action {
                name: "Rename",
                guard: Some(all([
                    settled(),
                    room(),
                    neq(v("ha"), who()),
                    or_(eq(v("ha"), int(0)), eq(holder_class(), int(1))),
                ])),
                updates: then(
                    each(&by_who, &|i| own(i, 1, 0, v("clock"))),
                    vec![bump(), pending()],
                ),
            },
            Action {
                name: "Take",
                guard: Some(and_(takeable(), neq(v("ha"), who()))),
                updates: then(each(&by_who, &take), vec![up("tk", who()), pending()]),
            },
            Action {
                name: "TakeTwice",
                guard: Some(takeable()),
                updates: then(
                    each(&by_taker, &take),
                    vec![
                        up("tk", if_(eq(v("ha"), int(1)), int(2), int(1))),
                        pending(),
                    ],
                ),
            },
        ],
        invariants: vec![
            law("Agreement", fresh(eq(v("ha"), v("hb")))),
            law(
                "Resolvable",
                fresh(implies(
                    any(NUM.map(|n| eq(v(n), int(1)))),
                    neq(v("ha"), int(0)),
                )),
            ),
            law(
                "PersonKept",
                fresh(implies(
                    any((0..3).map(|i| and_(eq(v(NUM[i]), int(1)), eq(v(CLASS[i]), int(0))))),
                    and_(neq(v("ha"), int(0)), eq(holder_class(), int(0))),
                )),
            ),
            law(
                "IncumbentKept",
                fresh(all((0..3).flat_map(|i| {
                    (0..3).filter(move |j| *j != i).map(move |j| {
                        not(all([
                            eq(v("ha"), int(ID[j])),
                            eq(v(NUM[i]), int(1)),
                            eq(v(CLASS[i]), int(1)),
                            eq(v(CLASS[j]), int(1)),
                            lt(v(SINCE[i]), v(SINCE[j])),
                        ]))
                    })
                }))),
            ),
            law(
                "TakeHolds",
                fresh(or_(eq(v("tk"), int(0)), eq(v("ha"), v("tk")))),
            ),
            bound(
                "HolderBox",
                all([
                    le(v("clock"), add(cst("ClockMax"), int(1))),
                    le(v("t1"), cst("TMax")),
                    le(v("t2"), cst("TMax")),
                    le(v("t3"), cst("TMax")),
                ]),
            ),
        ],
    }
}

/// Older builds' implied records (§6.1), each node's migration (§7.1) and a re-founding of the
/// default fleet (§6.4 Leaving), over one number N (m1): nodes 1 and 2 run an older build whose
/// signed `name=` is N (`o` 1: an implied record, `seeded` at since 7), node 3 a new one. Each
/// older node migrates, seeding N at its evidence rank (`Rank1`, `Rank2`) only when its implied
/// claim held N, and otherwise counting N lost and going on to its next evidence (`n` 2: a
/// number of its own). Node 3's person takes the holder's claim where `check` offers a take (a
/// migrated holder), and `Refound` is the vault's re-founded default fleet, which keeps every
/// claim's class and since. `l` marks an older node whose claim on N lost.
///
/// Mutants: 1 implied records ranked at since 0, 2 Migrate seeds at the evidence rank
/// regardless, 3 Refound claims each carried NAME as `auto` with a fresh since, 4 a take writes a
/// fresh since.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_evidence_model() -> Model {
    const ID: [i64; 3] = [1, 2, 3];
    const NUM: [&str; 3] = ["n1", "n2", "n3"];
    const CLASS: [&str; 3] = ["c1", "c2", "c3"];
    const SINCE: [&str; 3] = ["s1", "s2", "s3"];
    const BASE: [&str; 3] = ["b1", "b2", "b3"];
    const TGEN: [&str; 3] = ["t1", "t2", "t3"];
    const OLD: [&str; 2] = ["o1", "o2"];
    const LOST: [&str; 2] = ["l1", "l2"];
    const RANK: [&str; 2] = ["Rank1", "Rank2"];
    const IMPLIED_FIRST: i64 = 1;
    const MIGRATE_ANYWAY: i64 = 2;
    const REFOUND_AUTO: i64 = 3;
    const FRESH_TAKE: i64 = 4;
    let v = var;
    let old = |i: usize| {
        OLD.get(i)
            .copied()
            .map_or(bool_lit(false), |o| eq(v(o), int(1)))
    };
    let claims: Vec<Claim> = (0..3)
        .map(|i| Claim {
            id: ID[i],
            on: eq(v(NUM[i]), int(1)),
            class: v(CLASS[i]),
            since: if_(and_(old(i), bug(IMPLIED_FIRST)), int(0), v(SINCE[i])),
            base: v(BASE[i]),
            tgen: v(TGEN[i]),
        })
        .collect();
    let h = settled_holder(&claims);
    let settled = || eq(v("phase"), int(0));
    let fresh = |e: Expr| or_(eq(v("phase"), int(1)), e);
    let of = |field: [&'static str; 3]| sel(&v("h"), &ID, field.map(v).to_vec());
    let pending = || up("phase", int(1));
    let room = || le(v("clock"), cst("ClockMax"));
    let migrate = |i: usize| {
        let seeds = or_(eq(v("h"), int(ID[i])), bug(MIGRATE_ANYWAY));
        Action {
            name: ["Migrate1", "Migrate2"][i],
            guard: Some(and_(settled(), old(i))),
            updates: vec![
                up(OLD[i], int(0)),
                up(NUM[i], if_(seeds.clone(), int(1), int(2))),
                up(CLASS[i], if_(seeds.clone(), int(0), int(1))),
                up(SINCE[i], if_(seeds, cst(RANK[i]), int(0))),
                pending(),
            ],
        }
    };
    let lost = |i: usize| {
        latch(
            LOST[i],
            and_(eq(v(NUM[i]), int(1)), neq(h.clone(), int(ID[i]))),
        )
    };
    let mut refound = vec![
        up("rf", int(1)),
        up("clock", add(v("clock"), int(1))),
        pending(),
    ];
    for i in 0..3 {
        let carried = all([not(old(i)), eq(v(NUM[i]), int(1)), bug(REFOUND_AUTO)]);
        refound.extend([
            up(CLASS[i], if_(carried.clone(), int(1), v(CLASS[i]))),
            up(SINCE[i], if_(carried, v("clock"), v(SINCE[i]))),
        ]);
    }
    let mut vars = vec![st("o1", 1), st("o2", 1)];
    for i in 0..3 {
        let older = i < 2;
        vars.extend([
            st(NUM[i], i64::from(older)),
            st(CLASS[i], i64::from(!older)),
            st(SINCE[i], if older { 7 } else { 0 }),
            st(BASE[i], ID[i]),
            st(TGEN[i], 0),
        ]);
    }
    vars.extend([
        st("l1", 0),
        st("l2", 0),
        st("clock", 8),
        st("phase", 1),
        st("h", 0),
        st("tk", 0),
        st("rf", 0),
    ]);
    Model {
        name: "MachineNumberEvidence",
        consts: vec![("Buggy", 0), ("Rank1", 2), ("Rank2", 1), ("ClockMax", 10)],
        vars,
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(eq(v("phase"), int(1))),
                updates: vec![up("h", h.clone()), lost(0), lost(1), up("phase", int(0))],
            },
            migrate(0),
            migrate(1),
            Action {
                name: "Take",
                guard: Some(all([
                    settled(),
                    room(),
                    eq(v("tk"), int(0)),
                    neq(v("h"), int(0)),
                    neq(v("h"), int(3)),
                    eq(sel(&v("h"), &[1, 2], vec![v("o1"), v("o2")]), int(0)),
                    eq(of(CLASS), int(0)),
                ])),
                updates: vec![
                    up("n3", int(1)),
                    up("c3", int(0)),
                    up("s3", mutated_int(of(SINCE), vec![(FRESH_TAKE, v("clock"))])),
                    up("b3", of(BASE)),
                    up("t3", add(of(TGEN), int(1))),
                    up("clock", add(v("clock"), int(1))),
                    up("tk", int(3)),
                    pending(),
                ],
            },
            Action {
                name: "Refound",
                guard: Some(all([settled(), room(), eq(v("rf"), int(0))])),
                updates: refound,
            },
        ],
        invariants: vec![
            law(
                "EvidenceOrder",
                fresh(all((0..3).flat_map(|i| {
                    (0..2).filter(move |j| *j != i).map(move |j| {
                        not(all([
                            not(old(i)),
                            eq(v(NUM[i]), int(1)),
                            eq(v(CLASS[i]), int(0)),
                            le(v(SINCE[i]), int(6)),
                            old(j),
                            eq(v("h"), int(ID[j])),
                        ]))
                    })
                }))),
            ),
            law(
                "NoReversal",
                fresh(all((0..2).map(|i| {
                    not(and_(eq(v(LOST[i]), int(1)), eq(v("h"), int(ID[i]))))
                }))),
            ),
            law(
                "TakeHolds",
                fresh(or_(eq(v("tk"), int(0)), eq(v("h"), v("tk")))),
            ),
            bound("EvidenceBox", le(v("clock"), add(cst("ClockMax"), int(1)))),
        ],
    }
}

/// One machine of two nodes, P (1) and Q (2), sharing an anchor (§6.1 step 2; §6.4 A second
/// account): the machine's primary, its title and its one number. Both start unnumbered, P's
/// first record (`act` 5) before Q's (6). `Pick` is `choose` for the node settle leaves an
/// unnumbered primary (`u`), `Migrate` a node seeding its evidence NAME (P m5 at rank 1, Q m6 at
/// rank 1 or 2), and `Resign` a re-signing at a higher `rev` (`r`) that moves no claim field.
/// `pr` is the primary, `ti` each node's title, `gain` the node that last gained a NAME, `rs`
/// whether the last change was a re-signing. A pick acts on the view settled before it (`phase`
/// 2: picks since that settle), as each of two nodes picks on the view both read.
///
/// Mutants: 1 primaries by the latest `act`, 2 the primary by the greatest `rev`, 3 a machine with
/// no NAME has no primary, 4 anchors ignored (each node a machine of its own).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_group_model() -> Model {
    const NUM: [&str; 2] = ["n1", "n2"];
    const CLASS: [&str; 2] = ["c1", "c2"];
    const SINCE: [&str; 2] = ["s1", "s2"];
    const ACT: [&str; 2] = ["a1", "a2"];
    const REV: [&str; 2] = ["r1", "r2"];
    const MIGRATED: [&str; 2] = ["mg1", "mg2"];
    const NEEDS: [&str; 2] = ["u1", "u2"];
    const TITLE: [&str; 2] = ["ti1", "ti2"];
    const NAME: [&str; 2] = ["Name1", "Name2"];
    const LATEST_ACT: i64 = 1;
    const BY_REV: i64 = 2;
    const NO_PRIMARY: i64 = 3;
    const NO_ANCHOR: i64 = 4;
    let v = var;
    let carries = |i: usize| neq(v(NUM[i]), int(0));
    let p_first = mutated(
        lex(
            vec![
                level(&v("c1"), &v("c2")),
                level(&v("s1"), &v("s2")),
                (
                    mutated(
                        lt(v("a1"), v("a2")),
                        vec![(LATEST_ACT, gt(v("a1"), v("a2")))],
                    ),
                    eq(v("a1"), v("a2")),
                ),
            ],
            bool_lit(true),
        ),
        vec![(BY_REV, le(v("r2"), v("r1")))],
    );
    let none_carry = and_(not(carries(0)), not(carries(1)));
    let named = if_(
        and_(carries(0), carries(1)),
        if_(p_first, int(1), int(2)),
        if_(carries(0), int(1), int(2)),
    );
    let newest = if_(gt(v("a2"), v("a1")), int(2), int(1));
    let pr = if_(
        or_(bug(NO_ANCHOR), and_(bug(NO_PRIMARY), none_carry.clone())),
        int(0),
        if_(none_carry.clone(), newest, named),
    );
    let needs = |i: usize| {
        and_(
            eq(v(NUM[i]), int(0)),
            any([
                eq(pr.clone(), int(i64::from(i == 1) + 1)),
                and_(bug(NO_PRIMARY), none_carry.clone()),
                bug(NO_ANCHOR),
            ]),
        )
    };
    let title = |i: usize| {
        if_(
            bug(NO_ANCHOR),
            v(NUM[i]),
            sel(&pr, &[1, 2, 0], vec![v("n1"), v("n2"), int(0)]),
        )
    };
    let key_first = |i: usize, j: usize| {
        lex(
            vec![
                level(&v(CLASS[i]), &v(CLASS[j])),
                level(&v(SINCE[i]), &v(SINCE[j])),
            ],
            bool_lit(false),
        )
    };
    let flips = any([(0, 1), (1, 0)].map(|(i, j): (usize, usize)| {
        let (gainer, other) = (int(i64::from(i == 1) + 1), int(i64::from(j == 1) + 1));
        all([
            eq(v("gain"), gainer.clone()),
            eq(pr.clone(), gainer),
            eq(v("pr"), other),
            carries(j),
            not(key_first(i, j)),
        ])
    }));
    let settled = || eq(v("phase"), int(0));
    let room = || le(v("clock"), cst("ClockMax"));
    let picker = || if_(eq(v("u1"), int(1)), int(1), int(2));
    let top = max2(v("n1"), v("n2"));
    let mut pick = Vec::new();
    for i in 0..2 {
        let by = eq(picker(), int(i64::from(i == 1) + 1));
        pick.extend(write_where(
            &by,
            vec![
                (NUM[i], add(top.clone(), int(1))),
                (CLASS[i], int(1)),
                (SINCE[i], v("clock")),
                (ACT[i], v("clock")),
                (NEEDS[i], int(0)),
            ],
        ));
    }
    pick.extend([
        up("clock", add(v("clock"), int(1))),
        up("picks", add(v("picks"), int(1))),
        up("gain", picker()),
        up("phase", int(2)),
    ]);
    let migrate = |i: usize| Action {
        name: ["Migrate1", "Migrate2"][i],
        guard: Some(all([
            settled(),
            room(),
            eq(v(MIGRATED[i]), int(0)),
            eq(v(NUM[i]), int(0)),
        ])),
        updates: vec![
            up(NUM[i], cst(NAME[i])),
            up(CLASS[i], int(0)),
            up(
                SINCE[i],
                if i == 0 {
                    int(1)
                } else {
                    in_range(int(1), int(2))
                },
            ),
            up(ACT[i], v("clock")),
            up("clock", add(v("clock"), int(1))),
            up(MIGRATED[i], int(1)),
            up("gain", int(i64::from(i == 1) + 1)),
            up("rs", int(0)),
            up("phase", int(1)),
        ],
    };
    let resign = |i: usize| Action {
        name: ["Resign1", "Resign2"][i],
        guard: Some(and_(settled(), lt(v(REV[i]), cst("RevMax")))),
        updates: vec![
            up(REV[i], add(v(REV[i]), int(1))),
            up("gain", int(0)),
            up("rs", int(1)),
            up("phase", int(1)),
        ],
    };
    let mut vars = Vec::new();
    for (i, act) in [(0, 5), (1, 6)] {
        vars.extend([
            st(NUM[i], 0),
            st(CLASS[i], 1),
            st(SINCE[i], 0),
            st(ACT[i], act),
            st(REV[i], 0),
            st(MIGRATED[i], 0),
            st(NEEDS[i], 0),
            st(TITLE[i], 0),
        ]);
    }
    vars.extend(
        [
            ("clock", 8),
            ("phase", 1),
            ("pr", 0),
            ("picks", 0),
            ("flip", 0),
            ("unst", 0),
            ("gain", 0),
            ("rs", 0),
        ]
        .map(|(name, init)| st(name, init)),
    );
    Model {
        name: "MachineNumberGroup",
        consts: vec![
            ("Buggy", 0),
            ("ClockMax", 12),
            ("RevMax", 1),
            ("Name1", 5),
            ("Name2", 6),
        ],
        vars,
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(neq(v("phase"), int(0))),
                updates: vec![
                    up("pr", pr.clone()),
                    up("u1", bit(needs(0))),
                    up("u2", bit(needs(1))),
                    up("ti1", title(0)),
                    up("ti2", title(1)),
                    latch("unst", and_(eq(v("rs"), int(1)), neq(pr.clone(), v("pr")))),
                    latch("flip", flips),
                    up("gain", int(0)),
                    up("rs", int(0)),
                    up("phase", int(0)),
                ],
            },
            Action {
                name: "Pick",
                guard: Some(all([
                    neq(v("phase"), int(1)),
                    or_(eq(v("u1"), int(1)), eq(v("u2"), int(1))),
                    room(),
                ])),
                updates: pick,
            },
            migrate(0),
            migrate(1),
            resign(0),
            resign(1),
        ],
        invariants: vec![
            law("PrimaryStable", eq(v("unst"), int(0))),
            law("OneNumberPerMachine", le(v("picks"), int(1))),
            law("NoLateFlip", eq(v("flip"), int(0))),
            law(
                "OneTitle",
                or_(neq(v("phase"), int(0)), eq(v("ti1"), v("ti2"))),
            ),
            bound(
                "GroupBox",
                all([
                    le(v("clock"), add(cst("ClockMax"), int(1))),
                    le(v("r1"), cst("RevMax")),
                    le(v("r2"), cst("RevMax")),
                    le(v("picks"), int(2)),
                ]),
            ),
        ],
    }
}

/// Succession (§6.4 A reinstall, §6.1 step 2's `sgen` tier): P (1) and Q (2) share an anchor, and
/// X (3) is another machine holding m12 (`chosen`, since 9). P holds m8 (`chosen`, since 8);
/// `Succeed` is Q writing P's claim one take and one succession generation on (`sup`: P is
/// superseded); then `Take` takes X's m12 (a claim younger than P's), or `Move` moves Q to m20
/// as `auto`, each keeping Q's `sgen`; `StaleTake` is the superseded P, on a view that lacked
/// its successor, taking Q's claim at its own `sgen` 0 (a higher `tgen` of the same `base`, the
/// §11.69 case); `TakeBack` is P writing Q's claim back at Q's `sgen` + 1 (`supq`: now Q is
/// superseded). Settle stores the primary (`pr`) and each number's holder.
///
/// Mutants: 1 primaries by the claim alone, with no `sgen` tier; 2 `sgen` left out of the
/// successor's later records; 3 the same-base `tgen` drop run over the whole group before the
/// `sgen` tier (the order before §11.69), so the stale take drops the successor's claim.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_succession_model() -> Model {
    const NUM: [&str; 2] = ["n1", "n2"];
    const CLASS: [&str; 2] = ["c1", "c2"];
    const SINCE: [&str; 2] = ["s1", "s2"];
    const BASE: [&str; 2] = ["b1", "b2"];
    const TGEN: [&str; 2] = ["t1", "t2"];
    const SGEN: [&str; 2] = ["g1", "g2"];
    const ACT: [&str; 2] = ["a1", "a2"];
    const NO_SGEN: i64 = 1;
    const DROP_SGEN: i64 = 2;
    const DROP_FIRST: i64 = 3;
    let v = var;
    let top_sgen = max2(v("g1"), v("g2"));
    let in_tier = |i: usize| {
        mutated(
            eq(v(SGEN[i]), top_sgen.clone()),
            vec![(NO_SGEN, bool_lit(true))],
        )
    };
    let remains = |i: usize, j: usize| {
        and_(
            in_tier(i),
            not(all([
                neq(v(NUM[i]), int(0)),
                mutated(in_tier(j), vec![(DROP_FIRST, bool_lit(true))]),
                eq(v(NUM[j]), v(NUM[i])),
                eq(v(BASE[j]), v(BASE[i])),
                gt(v(TGEN[j]), v(TGEN[i])),
            ])),
        )
    };
    let carrier = |i: usize, j: usize| and_(remains(i, j), neq(v(NUM[i]), int(0)));
    let p_first = lex(
        vec![
            level(&v("c1"), &v("c2")),
            level(&v("s1"), &v("s2")),
            level(&v("a1"), &v("a2")),
        ],
        bool_lit(true),
    );
    let named = if_(
        and_(carrier(0, 1), carrier(1, 0)),
        if_(p_first, int(1), int(2)),
        if_(carrier(0, 1), int(1), int(2)),
    );
    // With no carrier, the latest `act` among the nodes that remain, and P (settle's `first`)
    // when none does.
    let newest = if_(
        and_(remains(0, 1), remains(1, 0)),
        if_(gt(v("a2"), v("a1")), int(2), int(1)),
        if_(remains(1, 0), int(2), int(1)),
    );
    let pr = if_(or_(carrier(0, 1), carrier(1, 0)), named, newest);
    let held = |num: i64| {
        let mut claims: Vec<Claim> = (0..2)
            .map(|i| Claim {
                id: i64::from(i == 1) + 1,
                on: and_(
                    eq(pr.clone(), int(i64::from(i == 1) + 1)),
                    eq(v(NUM[i]), int(num)),
                ),
                class: v(CLASS[i]),
                since: v(SINCE[i]),
                base: v(BASE[i]),
                tgen: v(TGEN[i]),
            })
            .collect();
        claims.push(Claim {
            id: 3,
            on: bool_lit(num == 12),
            class: int(0),
            since: int(9),
            base: int(3),
            tgen: int(0),
        });
        settled_holder(&claims)
    };
    let settled = || eq(v("phase"), int(0));
    let fresh = |e: Expr| or_(eq(v("phase"), int(1)), e);
    let tick = || up("clock", add(v("clock"), int(1)));
    let pending = || up("phase", int(1));
    let kept_sgen = || mutated_int(v("g2"), vec![(DROP_SGEN, int(0))]);
    let neither = |node: i64| all(["pr", "h8", "h12", "h20"].map(|h| neq(v(h), int(node))));
    let mut vars = Vec::new();
    for (i, [n, c, s, a]) in [(0, [8, 0, 8, 8]), (1, [0, 1, 0, 9])] {
        vars.extend([
            st(NUM[i], n),
            st(CLASS[i], c),
            st(SINCE[i], s),
            st(BASE[i], i64::from(i == 1) + 1),
            st(TGEN[i], 0),
            st(SGEN[i], 0),
            st(ACT[i], a),
        ]);
    }
    vars.extend(
        [
            ("clock", 10),
            ("phase", 1),
            ("pr", 0),
            ("h8", 0),
            ("h12", 0),
            ("h20", 0),
            ("sup", 0),
            ("supq", 0),
            ("took", 0),
            ("moved", 0),
            ("stale", 0),
            ("back", 0),
        ]
        .map(|(name, init)| st(name, init)),
    );
    Model {
        name: "MachineNumberSuccession",
        consts: vec![("Buggy", 0)],
        vars,
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(eq(v("phase"), int(1))),
                updates: vec![
                    up("pr", pr.clone()),
                    up("h8", held(8)),
                    up("h12", held(12)),
                    up("h20", held(20)),
                    up("phase", int(0)),
                ],
            },
            Action {
                name: "Succeed",
                guard: Some(all([
                    settled(),
                    eq(v("sup"), int(0)),
                    eq(v("supq"), int(0)),
                ])),
                updates: vec![
                    up("n2", v("n1")),
                    up("c2", v("c1")),
                    up("s2", v("s1")),
                    up("b2", v("b1")),
                    up("t2", add(v("t1"), int(1))),
                    up("g2", add(v("g1"), int(1))),
                    up("a2", v("clock")),
                    up("sup", int(1)),
                    tick(),
                    pending(),
                ],
            },
            Action {
                name: "Take",
                guard: Some(all([
                    settled(),
                    eq(v("sup"), int(1)),
                    eq(v("took"), int(0)),
                    eq(v("pr"), int(2)),
                    eq(v("h12"), int(3)),
                ])),
                updates: vec![
                    up("n2", int(12)),
                    up("c2", int(0)),
                    up("s2", int(9)),
                    up("b2", int(3)),
                    up("t2", int(1)),
                    up("g2", kept_sgen()),
                    up("a2", v("clock")),
                    up("took", int(1)),
                    tick(),
                    pending(),
                ],
            },
            Action {
                name: "Move",
                guard: Some(all([
                    settled(),
                    eq(v("sup"), int(1)),
                    eq(v("moved"), int(0)),
                    eq(v("pr"), int(2)),
                ])),
                updates: vec![
                    up("n2", int(20)),
                    up("c2", int(1)),
                    up("s2", v("clock")),
                    up("b2", int(2)),
                    up("t2", int(0)),
                    up("g2", kept_sgen()),
                    up("a2", v("clock")),
                    up("moved", int(1)),
                    tick(),
                    pending(),
                ],
            },
            Action {
                name: "StaleTake",
                guard: Some(all([
                    settled(),
                    eq(v("sup"), int(1)),
                    eq(v("stale"), int(0)),
                    eq(v("pr"), int(2)),
                    neq(v("n2"), int(0)),
                ])),
                updates: vec![
                    up("n1", v("n2")),
                    up("c1", v("c2")),
                    up("s1", v("s2")),
                    up("b1", v("b2")),
                    up("t1", add(v("t2"), int(1))),
                    up("a1", v("clock")),
                    up("stale", int(1)),
                    tick(),
                    pending(),
                ],
            },
            Action {
                name: "TakeBack",
                guard: Some(all([
                    settled(),
                    eq(v("sup"), int(1)),
                    eq(v("back"), int(0)),
                    eq(v("pr"), int(2)),
                ])),
                updates: vec![
                    up("n1", v("n2")),
                    up("c1", v("c2")),
                    up("s1", v("s2")),
                    up("b1", v("b2")),
                    up("t1", add(v("t2"), int(1))),
                    up("g1", add(v("g2"), int(1))),
                    up("a1", v("clock")),
                    up("sup", int(0)),
                    up("supq", int(1)),
                    up("back", int(1)),
                    tick(),
                    pending(),
                ],
            },
        ],
        invariants: vec![
            law(
                "SuccessionFinal",
                fresh(and_(
                    implies(eq(v("sup"), int(1)), neither(1)),
                    implies(eq(v("supq"), int(1)), neither(2)),
                )),
            ),
            bound("SuccessionBox", le(v("clock"), int(15))),
        ],
    }
}

/// A stripping relay and an older build on the reader's own Mac (§6.1 unknown claims and opaque
/// entries; §6.2's deferral), over one number N (m1) as reader J (3) sees it: S (1) and V (2)
/// claim N `auto` (V since 9; S since `ss`, 8 until `Reclaim` re-claims it at 10, after which J
/// still holds S's older record, `held`). `RelayStrips` hands J S's newer entry stripped of its
/// record (`strip` 1) or of its `name=` too (2), and `Fetch` reads it first-hand. O (4), an older
/// build on J's Mac, is live (`live`) and proven there by a probe that reached its listener;
/// `Roam` changes its address, `Down` stops its listener (`listen` 0), and it stays proven while
/// it is live, its listener stopped or not, until `Upgrade` (it upgrades or falls silent, so it is
/// no older entry in view any more). Settle stores the holder J names (`h`, 0 for an unconfirmed
/// N) and how many nodes rank before J for a pick (`d`); `Pick` is J's `choose`, `jn` the number
/// it takes and `bad` whether it picked while an entry opaque to it was live.
///
/// Mutants: 1 a stripped entry read as an implied record, 2 Currency dropped (J settles on the
/// older record it holds behind a stripped newer entry), 3 `choose` without the deferral, 4
/// opacity by equal addresses (after Roam, the older node is not proven on J's Mac), 5 opacity
/// that ends when the older node's listener stops.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_stripped_model() -> Model {
    const IMPLIED: i64 = 1;
    const NO_CURRENCY: i64 = 2;
    const NO_DEFER: i64 = 3;
    const BY_ADDRESS: i64 = 4;
    const BY_LISTENER: i64 = 5;
    let v = var;
    let record = or_(
        eq(v("strip"), int(0)),
        and_(bug(NO_CURRENCY), eq(v("held"), int(1))),
    );
    let implied = all([not(record.clone()), eq(v("strip"), int(1)), bug(IMPLIED)]);
    let unconfirmed = all([
        not(record.clone()),
        not(implied.clone()),
        eq(v("strip"), int(1)),
    ]);
    let since = if_(eq(v("strip"), int(0)), v("ss"), int(8));
    let s_holds = or_(implied.clone(), and_(record.clone(), lt(since, int(9))));
    let proven = mutated(
        bool_lit(true),
        vec![
            (BY_ADDRESS, eq(v("roam"), int(0))),
            (BY_LISTENER, eq(v("listen"), int(1))),
        ],
    );
    // What J defers on, and what is opaque to J in truth: S's stripped copy, and O while live.
    let deferred = or_(neq(v("strip"), int(0)), and_(eq(v("live"), int(1)), proven));
    let opaque = or_(neq(v("strip"), int(0)), eq(v("live"), int(1)));
    let pending = || up("phase", int(1));
    Model {
        name: "MachineNumberStripped",
        consts: vec![("Buggy", 0)],
        vars: [
            ("strip", 0),
            ("ss", 8),
            ("held", 0),
            ("roam", 0),
            ("live", 1),
            ("listen", 1),
            ("jn", 0),
            ("bad", 0),
            ("h", 0),
            ("d", 0),
            ("phase", 1),
        ]
        .map(|(name, init)| st(name, init))
        .to_vec(),
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(eq(v("phase"), int(1))),
                updates: vec![
                    up("h", if_(unconfirmed, int(0), if_(s_holds, int(1), int(2)))),
                    up("d", bit(or_(record, implied))),
                    up("phase", int(0)),
                ],
            },
            Action {
                name: "Pick",
                guard: Some(all([
                    eq(v("phase"), int(0)),
                    eq(v("jn"), int(0)),
                    or_(not(deferred), bug(NO_DEFER)),
                ])),
                updates: vec![
                    up("jn", add(int(2), v("d"))),
                    latch("bad", opaque),
                    pending(),
                ],
            },
            Action {
                name: "RelayStrips",
                guard: Some(eq(v("strip"), int(0))),
                updates: vec![up("strip", in_range(int(1), int(2))), pending()],
            },
            Action {
                name: "Fetch",
                guard: Some(neq(v("strip"), int(0))),
                updates: vec![up("strip", int(0)), pending()],
            },
            Action {
                name: "Reclaim",
                guard: Some(eq(v("ss"), int(8))),
                updates: vec![up("ss", int(10)), up("held", int(1)), pending()],
            },
            Action {
                name: "Roam",
                guard: Some(eq(v("roam"), int(0))),
                updates: vec![up("roam", int(1))],
            },
            Action {
                name: "Down",
                guard: Some(eq(v("listen"), int(1))),
                updates: vec![up("listen", int(0))],
            },
            Action {
                name: "Upgrade",
                guard: Some(eq(v("live"), int(1))),
                updates: vec![up("live", int(0))],
            },
        ],
        invariants: vec![
            law(
                "StrippedHoldsNothing",
                any([
                    eq(v("phase"), int(1)),
                    neq(v("strip"), int(1)),
                    eq(v("h"), int(0)),
                    eq(v("h"), if_(eq(v("ss"), int(8)), int(1), int(2))),
                ]),
            ),
            law("StrippedActsOnNothing", eq(v("bad"), int(0))),
        ],
    }
}

/// `choose`'s automatic pick for node A (1), which needs a number (§6.2), over what the view
/// holds when it picks. B (2) holds m1 once it joins (`joined`), and A's own record claims m1 at a
/// later since once it contests it (`disp`: displaced while B is in view, holding m1 otherwise).
/// E (3) holds m5 once it arrives (`e` 1) and leaves a tombstone once it expires (2). A planned
/// `computers` row is labelled m6 (`row`). A's history (`hist`): 1 it lost m4 in a contest, 2 it
/// kept m4 as `prev`. W (4) is a migration that waits keeping a NAME as `prev` (`wait`: 1 m7, above
/// every `top` here, which `prev` does not raise; 2 m4, A's want). D (5) names A's want m4 too
/// (`rival`: 1 as its admission label, 2 as an unknown claim's signed `name=`). `Pick` stores
/// the number (`res`) and whether it came as a want (`kind` 1) or a pick above `top` (2).
///
/// Mutants: 1 Expire leaves no tombstone, 2 step 2 numbers the numbers above `top` without
/// leaving out those evidence names or a kept number holds (no shared list), 3 a loser
/// re-claims what it lost (its `lost` entry no bar to a want), 4 a pick at `top` itself, m0 on an
/// empty view, 5 a want's free test that reads only holders and `lost` (another machine's record
/// or label no bar).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_choose_model() -> Model {
    const NO_TOMBSTONE: i64 = 1;
    const NO_SPENT: i64 = 2;
    const RECLAIM: i64 = 3;
    const AT_TOP: i64 = 4;
    const WANT_UNCHECKED: i64 = 5;
    let v = var;
    let is = |name: &'static str, value: i64| eq(v(name), int(value));
    let e5 = or_(is("e", 1), and_(is("e", 2), none_of(&[NO_TOMBSTONE])));
    let named4 = or_(le(int(1), v("rival")), is("wait", 2));
    let needs = or_(is("disp", 0), is("joined", 1));
    let want = and_(
        mutated(is("hist", 2), vec![(RECLAIM, le(int(1), v("hist")))]),
        mutated(
            and_(is("rival", 0), neq(v("wait"), int(2))),
            vec![(WANT_UNCHECKED, neq(v("rival"), int(2)))],
        ),
    );
    let top = max2(
        max2(
            bit(or_(is("joined", 1), is("disp", 1))),
            if_(e5.clone(), int(5), int(0)),
        ),
        if_(or_(is("hist", 1), le(int(1), v("rival"))), int(4), int(0)),
    );
    let excluded = |x: &Expr| {
        let at = |n: i64| eq(x.clone(), int(n));
        any([
            and_(at(1), or_(is("joined", 1), is("disp", 1))),
            and_(at(4), or_(le(int(1), v("hist")), named4.clone())),
            and_(at(5), e5.clone()),
            and_(at(6), is("row", 1)),
            and_(at(7), is("wait", 1)),
        ])
    };
    let skip = |x: Expr| {
        if_(
            and_(excluded(&x), none_of(&[NO_SPENT])),
            add(x.clone(), int(1)),
            x,
        )
    };
    let start = mutated_int(add(top.clone(), int(1)), vec![(AT_TOP, top)]);
    let pick = skip(skip(skip(start)));
    let before = || is("pk", 0);
    let picked = |e: Expr| or_(before(), e);
    let toggle = |name: &'static str, var: &'static str, from: i64, to: Expr| Action {
        name,
        guard: Some(and_(before(), is(var, from))),
        updates: vec![up(var, to)],
    };
    Model {
        name: "MachineNumberChoose",
        consts: vec![("Buggy", 0)],
        vars: [
            ("joined", 0),
            ("disp", 0),
            ("e", 0),
            ("row", 0),
            ("hist", 0),
            ("wait", 0),
            ("rival", 0),
            ("pk", 0),
            ("res", 0),
            ("kind", 0),
        ]
        .map(|(name, init)| st(name, init))
        .to_vec(),
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Pick",
                guard: Some(and_(before(), needs)),
                updates: vec![
                    up("res", if_(want.clone(), int(4), pick)),
                    up("kind", if_(want, int(1), int(2))),
                    up("pk", int(1)),
                ],
            },
            toggle("Join", "joined", 0, int(1)),
            toggle("Contest", "disp", 0, int(1)),
            toggle("Arrive", "e", 0, int(1)),
            toggle("Expire", "e", 1, int(2)),
            toggle("Row", "row", 0, int(1)),
            toggle("Recall", "hist", 0, in_range(int(1), int(2))),
            toggle("Wait", "wait", 0, in_range(int(1), int(2))),
            toggle("Rival", "rival", 0, in_range(int(1), int(2))),
        ],
        invariants: vec![
            law(
                "NoAutoReuse",
                picked(not(any([
                    and_(is("joined", 1), is("res", 1)),
                    and_(le(int(1), v("e")), is("res", 5)),
                    and_(is("row", 1), is("res", 6)),
                    and_(is("wait", 1), is("res", 7)),
                    and_(named4, is("res", 4)),
                ]))),
            ),
            law("NoMoveBack", picked(not(and_(is("hist", 1), is("res", 4))))),
            law("NeverZero", picked(neq(v("res"), int(0)))),
        ],
    }
}

/// A person's rename press against the machine's own second node (§5.3; §6.4 A second account),
/// where `check`'s simulated settle decides: P (1) and Q (2) share an anchor, and P holds m5
/// seeded at rank 1. `Rename` is the primary's press to m7, which `check` lets rename only when
/// the renamed record would still rank first in its machine; `Seed` is Q's migration seeding m6
/// at rank 2 as a second node that has not dropped it yet, `Wait` a migration that waits keeping
/// m6 as `prev` (`w`), and `Drop` Q publishing its next record with no NAME. Settle stores the
/// primary (`pr`); `pressed` marks a press since then, and `lost` a press after which the machine
/// did not hold m7.
///
/// Mutant: 1 `check` without its simulated settle (classify's verdict alone).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn machine_number_press_model() -> Model {
    const NUM: [&str; 2] = ["n1", "n2"];
    const CLASS: [&str; 2] = ["c1", "c2"];
    const SINCE: [&str; 2] = ["s1", "s2"];
    const ACT: [&str; 2] = ["a1", "a2"];
    const NO_SIM: i64 = 1;
    let v = var;
    let carries = |i: usize| {
        if i == 0 {
            neq(v("n1"), int(0))
        } else {
            or_(neq(v("n2"), int(0)), eq(v("w2"), int(1)))
        }
    };
    let p_first = lex(
        vec![
            level(&v("c1"), &v("c2")),
            level(&v("s1"), &v("s2")),
            level(&v("a1"), &v("a2")),
        ],
        bool_lit(true),
    );
    let pr = if_(
        and_(carries(0), carries(1)),
        if_(p_first, int(1), int(2)),
        if_(
            carries(0),
            int(1),
            if_(
                carries(1),
                int(2),
                if_(gt(v("a2"), v("a1")), int(2), int(1)),
            ),
        ),
    );
    let of_pr = |field: [&'static str; 2]| if_(eq(v("pr"), int(1)), v(field[0]), v(field[1]));
    // The other node of the machine carries a claim of class `chosen` or `seeded`, which ranks
    // before the press's fresh since.
    let rival = if_(
        eq(v("pr"), int(1)),
        and_(carries(1), eq(v("c2"), int(0))),
        and_(carries(0), eq(v("c1"), int(0))),
    );
    let settled = || eq(v("phase"), int(0));
    let mut press = Vec::new();
    for i in 0..2 {
        let by = eq(v("pr"), int(i64::from(i == 1) + 1));
        press.extend(write_where(
            &by,
            vec![
                (NUM[i], int(7)),
                (CLASS[i], int(0)),
                (SINCE[i], v("clock")),
                (ACT[i], v("clock")),
            ],
        ));
    }
    press.extend([
        up("w2", if_(eq(v("pr"), int(2)), int(0), v("w2"))),
        up("clock", add(v("clock"), int(1))),
        up("pressed", int(1)),
        up("phase", int(1)),
    ]);
    let second = || and_(settled(), eq(v("pr"), int(1)));
    Model {
        name: "MachineNumberPress",
        consts: vec![("Buggy", 0)],
        vars: [
            ("n1", 5),
            ("c1", 0),
            ("s1", 1),
            ("a1", 8),
            ("n2", 0),
            ("c2", 1),
            ("s2", 0),
            ("a2", 9),
            ("w2", 0),
            ("migrated", 0),
            ("clock", 10),
            ("pr", 0),
            ("pressed", 0),
            ("lost", 0),
            ("phase", 1),
        ]
        .map(|(name, init)| st(name, init))
        .to_vec(),
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Settle",
                guard: Some(eq(v("phase"), int(1))),
                updates: vec![
                    up("pr", pr.clone()),
                    latch(
                        "lost",
                        and_(
                            eq(v("pressed"), int(1)),
                            neq(if_(eq(pr, int(1)), v("n1"), v("n2")), int(7)),
                        ),
                    ),
                    up("pressed", int(0)),
                    up("phase", int(0)),
                ],
            },
            Action {
                name: "Rename",
                guard: Some(all([
                    settled(),
                    neq(of_pr(NUM), int(7)),
                    mutated(not(rival), vec![(NO_SIM, bool_lit(true))]),
                ])),
                updates: press,
            },
            Action {
                name: "Seed",
                guard: Some(all([
                    second(),
                    eq(v("migrated"), int(0)),
                    eq(v("n2"), int(0)),
                ])),
                updates: vec![
                    up("n2", int(6)),
                    up("c2", int(0)),
                    up("s2", int(2)),
                    up("a2", v("clock")),
                    up("migrated", int(1)),
                    up("clock", add(v("clock"), int(1))),
                    up("phase", int(1)),
                ],
            },
            Action {
                name: "Wait",
                guard: Some(all([
                    second(),
                    eq(v("migrated"), int(0)),
                    eq(v("n2"), int(0)),
                ])),
                updates: vec![
                    up("w2", int(1)),
                    up("c2", int(0)),
                    up("s2", int(2)),
                    up("a2", v("clock")),
                    up("migrated", int(1)),
                    up("clock", add(v("clock"), int(1))),
                    up("phase", int(1)),
                ],
            },
            Action {
                name: "Drop",
                guard: Some(and_(second(), carries(1))),
                updates: vec![
                    up("n2", int(0)),
                    up("w2", int(0)),
                    up("c2", int(1)),
                    up("s2", int(0)),
                    up("a2", v("clock")),
                    up("clock", add(v("clock"), int(1))),
                    up("phase", int(1)),
                ],
            },
        ],
        invariants: vec![
            law("PressHolds", eq(v("lost"), int(0))),
            bound("PressBox", le(v("clock"), int(15))),
        ],
    }
}
