// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine-wide full-pass rule: atpkg's `update` pass as the WRITER of `status.toml`'s
//! stamps, the window's gate, the session lane and a queued pass's own stand-down as its
//! READERS, and the store lock between a lane's decision and its pass.

use super::*;

/// One tick is the spacing (five minutes), `Interval` the six-hour walk, `MaxAge` "longer
/// ago than that"; the record's ages saturate there. A pass takes no ticks.
///
/// THE RECORD is what the readers see: `rec` (`last_pass`: 0 none, 1 ok, 2 failed or
/// offline), `pass_age` (`last_pass_at`), `ever_ok`/`ok_age` (`last_success_at`),
/// `write_age` (`updated_at`, every writer's), `hold` (`metered_hold_until`), `running` (a
/// pass's progress file). THE TRUTH is what happened, in ghosts the readers never see:
/// `last` (how the last full pass really ended), `since_end` (idle ticks since it ended)
/// and `limit` (ticks GitHub still refuses this machine's listing). The writers: a pass ends
/// — succeeded, failed (offline too), rate-limited, or rate-limited with the cache standing
/// in (a success with a hold) — and `OtherWrite`, every other writer (a vendor head-watch
/// pass, a typed verb), which moves `updated_at` alone.
///
/// THE LOCK: a lane that decides spawns a child (`queued`), which runs only once it holds
/// the store lock; lanes cannot see each other's children, so several decide in the gap. A
/// child that found the lock held and saw a full pass end since (`behind`) stands down
/// behind it, whatever it ended; one that took the lock at once runs. The lock keeps one
/// pass in flight, so that is no invariant here. Below the model's grain: a pass that ended
/// in the same second a child first found the lock held does not stand it down (the stamps
/// cannot tell it from one that ended before the wait).
///
/// The readers: `SessionLook` (a tab opens: `pkg_check::full_pass_owed` over
/// `atpkg::status::pass_stamps`), `WindowWalk` (the gate before the six-hour walk) and
/// `WindowLaunch` (a window opens: its launch rule, then the gate). `verdict` is what the
/// last look read of the record: did the last full pass fail.
///
/// `Buggy=1` is the code this replaced (ac5b4c144): the writer records no pass end and no
/// hold; the readers take a failure from the order of `updated_at` and the success and
/// space from `updated_at`; no lane honours a hold (the gate still spaces and sees a pass
/// in flight); a queued child stands down only behind a success. Each invariant has its own
/// counterexample. The progress obligation (`NoOwedPassHeldBack`) is stated on the truth,
/// never the record, so a lane that parks an owed pass — a failed one too — is caught at
/// `Buggy=0`: with `Interval` past `MaxAge` the record can never say "an interval old".
/// Tier-1: atpkg's `status` conformance (the writers, the session lane, and the historical
/// reader against `Buggy=1`), its `cli` one (the queued child's stand-down), aterm-gui's
/// (the gate and the launch rule).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
    let down = |var: &'static str| Update {
        var,
        expr: if_(le(n(1), v(var)), sub(v(var), n(1)), v(var)),
    };
    let fixed_writes = |var: &'static str, value: Expr| Update {
        var,
        expr: if_(fixed(), value, v(var)),
    };
    let idle = || eq(v("running"), n(0));
    let due = || or_(eq(v("ever_ok"), n(0)), le(cst("Interval"), v("ok_age")));
    // Did the last full pass fail, read off the record: its own word (`Stamps::last_failed`),
    // or — the old reader — a write after the last success, or any write and none.
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
    let unheld = || eq(v("hold"), n(0));
    // The three readers' decisions. The session lane reads no hold: every hold is written
    // with a pass end, which it spaces by an interval (failed) or counts its walk from (ok).
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
            and_(and_(and_(idle(), spaced_now()), unheld()), due()),
            and_(and_(idle(), spaced_then()), due()),
        )
    };
    let launch = || {
        either(
            and_(
                and_(and_(idle(), spaced_now()), unheld()),
                or_(failed_now(), due()),
            ),
            and_(and_(idle(), spaced_then()), or_(failed_then(), due())),
        )
    };
    // THE OBLIGATION, on the truth: nothing running, and never a full pass, or the last
    // one (ok or failed) an interval and the longest hold ago.
    let owed = || {
        and_(
            idle(),
            or_(
                eq(v("last"), n(0)),
                and_(
                    le(cst("Interval"), v("since_end")),
                    le(cst("HoldMax"), v("since_end")),
                ),
            ),
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
    // A child that holds the lock and runs: a pass within the spacing of another's end, or
    // inside GitHub's refusal, is what the invariants forbid.
    let starts = |runs: Expr| {
        vec![
            up("running", if_(runs.clone(), n(1), n(0))),
            flag(
                "b2b",
                and_(
                    runs.clone(),
                    and_(
                        le(n(1), v("last")),
                        le(v("since_end"), sub(cst("Spacing"), n(1))),
                    ),
                ),
            ),
            flag("early", and_(runs, le(n(1), v("limit")))),
        ]
    };
    // A pass ends: the truth, `updated_at`, the children queued behind it — and, fixed, its
    // recorded end.
    let ends = |name: &'static str, ok: bool, limited: bool| {
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
        if limited {
            updates.push(up("limit", cst("HoldMax")));
            updates.push(fixed_writes("hold", cst("HoldMax")));
        } else if ok {
            // Its listing answered: GitHub refuses nothing now, and the hold is lifted.
            updates.push(fixed_writes("hold", n(0)));
        }
        let guard = if ok && !limited {
            and_(eq(v("running"), n(1)), eq(v("limit"), n(0)))
        } else {
            eq(v("running"), n(1))
        };
        Action {
            name,
            guard: Some(guard),
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
            ("HoldMax", 2),
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
            state("hold", 0),
            state("last", 0),
            state("since_end", 0),
            state("limit", 0),
            state("verdict", 0),
            state("b2b", 0),
            state("mistook", 0),
            state("starved", 0),
            state("early", 0),
        ],
        fn_vars: vec![],
        actions: vec![
            ends("PassSucceeds", true, false),
            ends("PassRateLimitedOnCache", true, true),
            ends("PassFails", false, false),
            ends("PassRateLimited", false, true),
            Action {
                name: "OtherWrite",
                guard: Some(idle()),
                updates: vec![up("write_age", n(0))],
            },
            // Time passes only with no child at a free lock: one takes it within the tick.
            Action {
                name: "Tick",
                guard: Some(and_(idle(), eq(v("queued"), n(0)))),
                updates: vec![
                    age("pass_age"),
                    age("ok_age"),
                    age("write_age"),
                    down("hold"),
                    down("limit"),
                    up(
                        "since_end",
                        if_(
                            le(
                                v("since_end"),
                                sub(add(cst("Interval"), cst("HoldMax")), n(1)),
                            ),
                            add(v("since_end"), n(1)),
                            v("since_end"),
                        ),
                    ),
                ],
            },
            look("SessionLook", session(), true),
            look("WindowWalk", walk(), false),
            look("WindowLaunch", launch(), true),
            // A child that waited and saw a full pass end since takes the lock: it stands
            // down (Buggy: only behind a success, and ran behind a failure).
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
            // A child that no pass ended in front of takes the lock and runs.
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
                expr: eq(v("b2b"), n(0)),
            },
            Invariant {
                name: "NoMeteredPassInsideHold",
                expr: eq(v("early"), n(0)),
            },
            Invariant {
                name: "NoSuccessReadAsFailure",
                expr: eq(v("mistook"), n(0)),
            },
            Invariant {
                name: "NoOwedPassHeldBack",
                expr: eq(v("starved"), n(0)),
            },
        ],
    }
}
