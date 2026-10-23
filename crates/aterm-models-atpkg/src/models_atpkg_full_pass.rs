// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine-wide atpkg full-pass scheduling and store-lock model.

use aterm_spec::derive::*;

/// Machine-wide full-pass scheduling model across `status.toml` writers, readers, and queued store-lock waiters.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn atpkg_full_pass_rule_model() -> Model {
    let v = var;
    let n = int;
    let not = |e: Expr| eq(if_(e, n(1), n(0)), n(0));
    let fixed = || eq(cst("Buggy"), n(0));
    let old = || eq(cst("Buggy"), n(1));
    let either = |now: Expr, then: Expr| or_(and_(fixed(), now), and_(old(), then));
    let up = |var: &'static str, expr: Expr| Update { var, expr };
    let flag = |var: &'static str, when: Expr| Update {
        var,
        expr: if_(when, n(1), v(var)),
    };
    let age = |var: &'static str| Update {
        var,
        expr: if_(
            le(v(var), sub(cst("MaxAge"), n(1))),
            add(v(var), n(1)),
            v(var),
        ),
    };
    let fixed_writes = |var: &'static str, value: Expr| Update {
        var,
        expr: if_(fixed(), value, v(var)),
    };
    let idle = || eq(v("running"), n(0));
    let due = || or_(eq(v("ever_ok"), n(0)), le(cst("Interval"), v("ok_age")));
    let failed_now = || {
        and_(
            eq(v("rec"), n(2)),
            or_(eq(v("ever_ok"), n(0)), le(v("pass_age"), v("ok_age"))),
        )
    };
    let failed_then = || {
        or_(
            and_(
                eq(v("ever_ok"), n(1)),
                le(v("write_age"), sub(v("ok_age"), n(1))),
            ),
            and_(
                eq(v("ever_ok"), n(0)),
                le(v("write_age"), sub(cst("MaxAge"), n(1))),
            ),
        )
    };
    let read_failed = || either(failed_now(), failed_then());
    let spaced_now = || le(cst("Spacing"), v("pass_age"));
    let spaced_then = || le(cst("Spacing"), v("write_age"));
    let session = || {
        either(
            and_(
                and_(and_(idle(), spaced_now()), due()),
                or_(not(failed_now()), le(cst("Interval"), v("pass_age"))),
            ),
            and_(
                and_(and_(idle(), spaced_then()), due()),
                or_(not(failed_then()), le(cst("Interval"), v("write_age"))),
            ),
        )
    };
    let walk = || {
        either(
            and_(and_(idle(), spaced_now()), due()),
            and_(and_(idle(), spaced_then()), due()),
        )
    };
    let launch = || {
        either(
            and_(and_(idle(), spaced_now()), or_(failed_now(), due())),
            and_(and_(idle(), spaced_then()), or_(failed_then(), due())),
        )
    };
    let owed = || {
        and_(
            idle(),
            or_(eq(v("last"), n(0)), le(cst("Interval"), v("since_end"))),
        )
    };
    let look = |name: &'static str, decides: Expr, reads: bool| {
        let mut updates = vec![
            up("queued", add(v("queued"), if_(decides.clone(), n(1), n(0)))),
            flag("starved", and_(owed(), not(decides))),
        ];
        if reads {
            updates.push(up("verdict", if_(read_failed(), n(1), n(0))));
            updates.push(flag("mistook", and_(read_failed(), neq(v("last"), n(2)))));
        }
        Action {
            name,
            guard: Some(le(v("queued"), sub(cst("QueueMax"), n(1)))),
            updates,
        }
    };
    let starts = |runs: Expr| {
        vec![
            up("running", if_(runs.clone(), n(1), n(0))),
            flag(
                "b2b",
                and_(
                    runs,
                    and_(
                        le(n(1), v("last")),
                        le(v("since_end"), sub(cst("Spacing"), n(1))),
                    ),
                ),
            ),
        ]
    };
    let ends = |name: &'static str, ok: bool| {
        let mut updates = vec![
            up("running", n(0)),
            up("last", n(if ok { 1 } else { 2 })),
            up("since_end", n(0)),
            up("write_age", n(0)),
            up("behind", v("queued")),
            fixed_writes("rec", n(if ok { 1 } else { 2 })),
            fixed_writes("pass_age", n(0)),
        ];
        if ok {
            updates.push(up("ever_ok", n(1)));
            updates.push(up("ok_age", n(0)));
        }
        Action {
            name,
            guard: Some(eq(v("running"), n(1))),
            updates,
        }
    };
    let state = |name: &'static str, init: i64| StateVar { name, init };
    Model {
        name: "AtpkgFullPassRule",
        consts: vec![
            ("Buggy", 0),
            ("Spacing", 1),
            ("Interval", 3),
            ("MaxAge", 4),
            ("QueueMax", 2),
        ],
        vars: vec![
            state("running", 0),
            state("queued", 0),
            state("behind", 0),
            state("rec", 0),
            state("pass_age", 4),
            state("ever_ok", 0),
            state("ok_age", 4),
            state("write_age", 4),
            state("last", 0),
            state("since_end", 0),
            state("verdict", 0),
            state("b2b", 0),
            state("mistook", 0),
            state("starved", 0),
        ],
        fn_vars: vec![],
        actions: vec![
            ends("PassSucceeds", true),
            ends("PassFails", false),
            Action {
                name: "OtherWrite",
                guard: Some(idle()),
                updates: vec![up("write_age", n(0))],
            },
            Action {
                name: "Tick",
                guard: Some(and_(idle(), eq(v("queued"), n(0)))),
                updates: vec![
                    age("pass_age"),
                    age("ok_age"),
                    age("write_age"),
                    up(
                        "since_end",
                        if_(
                            le(v("since_end"), sub(cst("Interval"), n(1))),
                            add(v("since_end"), n(1)),
                            v("since_end"),
                        ),
                    ),
                ],
            },
            look("SessionLook", session(), true),
            look("WindowWalk", walk(), false),
            look("WindowLaunch", launch(), true),
            Action {
                name: "TakeLockBehind",
                guard: Some(and_(idle(), le(n(1), v("behind")))),
                updates: {
                    let mut updates = vec![
                        up("queued", sub(v("queued"), n(1))),
                        up("behind", sub(v("behind"), n(1))),
                    ];
                    updates.extend(starts(and_(
                        old(),
                        not(and_(eq(v("ever_ok"), n(1)), le(v("ok_age"), n(0)))),
                    )));
                    updates
                },
            },
            Action {
                name: "TakeLockFresh",
                guard: Some(and_(idle(), le(v("behind"), sub(v("queued"), n(1))))),
                updates: {
                    let mut updates = vec![up("queued", sub(v("queued"), n(1)))];
                    updates.extend(starts(bool_lit(true)));
                    updates
                },
            },
        ],
        invariants: vec![
            Invariant {
                name: "NoPassBackToBack",
                kind: InvariantKind::Claim,
                expr: eq(v("b2b"), n(0)),
            },
            Invariant {
                name: "NoSuccessReadAsFailure",
                kind: InvariantKind::Claim,
                expr: eq(v("mistook"), n(0)),
            },
            Invariant {
                name: "NoOwedPassHeldBack",
                kind: InvariantKind::Claim,
                expr: eq(v("starved"), n(0)),
            },
        ],
    }
}
