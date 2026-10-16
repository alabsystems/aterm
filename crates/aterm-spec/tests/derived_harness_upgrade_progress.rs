// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::{
    Expr, Model, Update, Value, and_, eq, harness_upgrade_progress_liveness,
    harness_upgrade_progress_model, harness_upgrade_progress_truth, int, or_, var,
};
use aterm_spec::{interp, verify};

/// MAIN TODAY: the committed machine at `Buggy = 0`, its gap latch clear —
/// what aterm-agent's Tier-1 binds drive main's code against. With `keep`
/// set (`RecordedWordStands`) the watch recorded only a read that waits; now
/// its read says what holds the point of an act (`upgrade::held_point`, else
/// `upgrade::NEXT_POINT`) and the watch records it.
fn main_today(model: &Model) -> Model {
    interp::with_buggy(model, 0)
}

/// Main as it was before the gap closed: the committed machine with that
/// gap's latch (`keep`) set from the start.
fn latched(model: &Model, name: &str) -> Model {
    let mut m = interp::with_buggy(model, 0);
    for v in &mut m.vars {
        if v.name == name {
            v.init = 1;
        }
    }
    m
}

/// The law's truth `T` (the class of what holds the move now) at `s`.
fn truth(m: &Model, s: &interp::State) -> i64 {
    match harness_upgrade_progress_truth().eval(&m.eval_env(s)) {
        Value::Int(t) => t,
        Value::Bool(b) => panic!("the truth is a class, not {b}"),
    }
}

/// Every state `m` reaches.
fn reach(m: &Model) -> Vec<interp::State> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            queue.extend(m.successors(a.name, &s));
        }
        out.push(s);
    }
    out
}

/// The committed machine (`Buggy = 0`) with one HEALTHY action edited — a
/// regression of the lane's own step, look or clock, never a `Buggy` member:
/// its guard replaced where `guard` is `Some`, and each update in `updates`
/// replaced (or added). What the negative controls below break.
fn edited(action: &str, guard: Option<Expr>, updates: Vec<(&'static str, Expr)>) -> Model {
    let mut m = interp::with_buggy(&harness_upgrade_progress_model(), 0);
    let a = m
        .actions
        .iter_mut()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("{action} is an action"));
    if let Some(g) = guard {
        a.guard = Some(g);
    }
    for (v, expr) in updates {
        a.updates.retain(|u| u.var != v);
        a.updates.push(Update { var: v, expr });
    }
    m
}

fn is(v: &'static str, n: i64) -> Expr {
    eq(var(v), int(n))
}

/// NO UPGRADE STUCK FOREVER, ITS SAFETY (design record 2026-09-28, §4.3,
/// adapted to main and to the owner's alert rules): the watch has no hands
/// (`NoHandsOffAPoint`), every look leaves a cause that holds the move now
/// (`NoStaleLook` — the look the session needs no point for among them: a
/// wake that reads nothing leaves a stale one) and the restart never ends
/// running work (`NeverEndsRunningWork`). Each invariant is
/// caught by its own `Buggy` member, and each dead action ALONE by an invariant
/// (`verify::audit_dead_negative_controls`, the audit aterm-gui's
/// `spec_xref_closure` runs). Each ghost is written by the HEALTHY action
/// from what it did, so each invariant bounds the lane's own steps too:
/// NEGATIVE CONTROLS below edit a healthy step (written only by the mutants,
/// the ghosts would limit nothing the lane did).
/// The law's yardstick (`T`, `harness_upgrade_progress_truth`) is the healthy
/// look's own record, over every state. Tier-1 against the real code is in
/// aterm-agent's `harness::upgrade_drive` progress and watch tests.
#[test]
fn the_upgrade_progress_keeps_every_safety_law() {
    let model = harness_upgrade_progress_model();
    verify::prove_and_catch_scalar(&model, "harness upgrade progress");
    // The committed space's fired set is walked once, not once per action.
    let fired = interp::fired_actions(&interp::with_buggy(&model, 0));
    let dead: Vec<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(&model)),
        Ok(4),
        "{dead:?}"
    );

    // THE YARDSTICK IS THE LOOK: at every state the committed machine
    // reaches, the healthy `Watch` records exactly the law's `T`.
    let states = reach(&interp::with_buggy(&model, 0));
    assert!(
        states.len() > 500,
        "the machine has a space: {}",
        states.len()
    );
    let mut looked = 0;
    for s in &states {
        let want = truth(&model, s);
        let mut after = s.clone();
        if model.fire("Watch", &mut after) {
            looked += 1;
            assert_eq!((after["cause"], after["seen"]), (want, want), "{s:?}");
        }
    }
    assert!(looked > 0);

    // The watch has no hands: off a point it never moves the phase.
    let mut s = model.init_state();
    assert!(model.fire("PersonHolds", &mut s));
    assert!(model.fire("Watch", &mut s));
    assert_eq!((s["phase"], s["cause"]), (0, 1), "a person, recorded");

    // NEGATIVE CONTROLS — a healthy step regressed, each caught by the
    // invariant its ghost feeds.
    // (1) The restart over processes under the agent: `work == 0` gone from
    // the idle step's restart.
    let restart_over_work = edited(
        "RestartAtIdle",
        Some(and_(and_(is("posture", 2), is("phase", 1)), is("ready", 1))),
        vec![],
    );
    let (_, broken) = interp::bmc(&restart_over_work).expect_err("the restart over work");
    assert_eq!(broken, "NeverEndsRunningWork");
    // (2) The idle step with no point to wait for: its posture guard gone.
    let off_a_point = edited(
        "StepAtIdle",
        Some(or_(
            or_(
                is("phase", 0),
                and_(
                    and_(is("phase", 1), is("clock", 1)),
                    or_(is("ready", 0), is("work", 1)),
                ),
            ),
            and_(is("phase", 2), is("clock", 1)),
        )),
        vec![],
    );
    let (_, broken) = interp::bmc(&off_a_point).expect_err("a step off a point");
    assert_eq!(broken, "NoHandsOffAPoint");
    // (3) The design's look that records nothing (its cause left as it was).
    let unrecorded = edited("Watch", None, vec![("cause", var("cause"))]);
    let (_, broken) = interp::bmc(&unrecorded).expect_err("a look that records nothing");
    assert_eq!(broken, "NoStaleLook");
}

/// Each livelock the test above catches is the cycle its mutant names — the
/// interpreter's own cycles, read apart from the tiered proof so they are
/// found while its `ty` runs rather than after them (they were the last of
/// that test's wall, and so the binary's).
#[test]
fn each_livelock_is_the_cycle_its_mutant_names() {
    let model = harness_upgrade_progress_model();
    let live = harness_upgrade_progress_liveness();

    // No look without a point: the record keeps a cause that no longer holds
    // the move, at a posture no point is offered at.
    let blind = verify::buggy_baseline_with(&model, "NoLookWithoutAPoint");
    let cycle = interp::nonprogress_under(&blind, &live).expect("unlooked for ever");
    assert_eq!(cycle.entry()["blind"], 1, "{cycle}");
    // The watch wakes and reads nothing there: the record stays stale.
    assert_ne!(
        cycle.entry()["cause"],
        truth(&blind, cycle.entry()),
        "{cycle}"
    );
}

/// THE GAP IS CLOSED ON MAIN (an assessment found two, each a genuine path on
/// which the record or the owner's tell failed): main's machine
/// ([`main_today`], the latch clear) keeps the law,
/// and the gap replayed alone ([`latched`]) still breaks it exactly where it
/// did — the NEGATIVE CONTROL. (The other, a limit a record for ever, is the
/// classifier's: closed by `St::limit_until` and `Row::limit_passed`, pinned
/// in aterm-agent.)
///
/// * THE WATCH LEFT A STALE WORD (`RecordedWordStands`): at an idle point
///   whose step would act and does not come, the dry read said `would-…`,
///   which recorded nothing, so the record kept the word of a look long past.
///   The read says what holds the point of a give-up or a re-arm now
///   (`upgrade::held_point`), and the watch records it, else
///   `upgrade::NEXT_POINT` (`upgrade_watch::look_word`): at every state main's
///   machine reaches, a look leaves the record naming what holds the move
///   NOW — `T` — the idle points whose step would act (aterm's own class, the
///   neutral word) among them. For a Claude Code record (the Codex lane is
///   not in the model, and its act reads record nothing still); and `T` at a
///   settled break was re-decided that day, not closed by code
///   (`derived_harness_upgrade_progress_break_is_a_redecision`).
///
/// A gap found reachable again is a regression: its half of this test fails
/// on main's machine, not only under its latch.
#[test]
fn derived_harness_upgrade_progress_gaps_are_closed_on_main() {
    let model = harness_upgrade_progress_model();
    let live = harness_upgrade_progress_liveness();
    let today = main_today(&model);
    assert!(
        interp::nonprogress_under(&today, &live).is_none(),
        "main today (the latch clear) keeps the law"
    );
    let states = reach(&today);

    // THE CLOSED GAP: every look main's machine takes leaves the record
    // naming what holds the move now — and where the step at an idle point
    // would act and has not, that is aterm's own class (the neutral word),
    // whatever the record said before.
    let mut acted_at_idle = 0;
    for s in &states {
        let mut looked = s.clone();
        if !today.fire("Watch", &mut looked) {
            continue;
        }
        assert_eq!(looked["cause"], truth(&today, s), "{s:?}");
        let acts_here = s["posture"] == 2
            && (s["phase"] == 0
                || s["clock"] == 1
                || (s["phase"] == 1 && s["ready"] == 1 && s["work"] == 0));
        if acts_here && s["cause"] != 3 {
            assert_eq!(looked["cause"], 3, "a stale word replaced: {s:?}");
            acted_at_idle += 1;
        }
    }
    assert!(acted_at_idle > 0, "a stale word at an idle point, reached");
    // NEGATIVE CONTROL: main before the fix — the `keep` latch alone —
    // breaks the law by a record that keeps a stale cause the look leaves.
    let keep = latched(&model, "keep");
    let cycle = interp::nonprogress_under(&keep, &live).expect("the gap alone, before");
    eprintln!("the closed gap, as main was:\n{cycle}");
    let e = cycle.entry();
    assert_ne!(
        e["cause"],
        truth(&keep, e),
        "the gap: the record keeps a stale cause: {cycle}"
    );
    // Inside the loop, a look leaves a word that no longer holds the move:
    // its read would act. Main today's look at that very state records it.
    let mut in_loop = vec![e.clone()];
    in_loop.extend(cycle.cycle.iter().map(|(_, s)| s.clone()));
    let left = in_loop
        .iter()
        .find(|s| {
            let mut looked = (*s).clone();
            keep.fire("Watch", &mut looked)
                && looked["cause"] == s["cause"]
                && s["cause"] != truth(&keep, s)
        })
        .unwrap_or_else(|| panic!("the gap: a look leaves a stale word in the loop: {cycle}"));
    let mut now = left.clone();
    now.insert("keep", 0);
    assert!(today.fire("Watch", &mut now), "{cycle}");
    assert_eq!(now["cause"], truth(&today, left), "main today records it");
    assert_ne!(now["cause"], left["cause"], "{cycle}");
    assert!(reach(&keep).iter().any(|s| {
        s["posture"] == 2
            && s["phase"] == 0
            && s["cause"] != truth(&keep, s)
            && s["seen"] != s["cause"]
    }));
}

/// THE SETTLED BREAK IS A RE-DECISION OF THE YARDSTICK, NOT A CHANGE OF CODE
/// (a review of `RecordedWordStands`' closing). Before it the law's truth `T`
/// at a settled break of the agent's own background work was aterm's own
/// class wherever the ask flow's step there would act (`StepAtBreak`: its
/// notice, give-up or re-arm, withheld by the loop's guard), and the watch's
/// dry read — which knows no break — recorded the agent's own work there
/// (`not-idle:busy`): half the gap. The code did not
/// change for that half; `T` did (`posture <= 1` is the agent's own work).
/// This pins what that re-decision is, over every state main's machine
/// reaches where the watch looks: the two yardsticks differ at a settled
/// break whose step would act, and NOWHERE ELSE; and main's look there
/// records the agent's own work, which the old yardstick called stale. What
/// the owner is told at that look, whichever class the record names, is the
/// classifier's (aterm-agent). And it is reached: a pin over no state would
/// prove nothing.
#[test]
fn derived_harness_upgrade_progress_break_is_a_redecision() {
    let model = harness_upgrade_progress_model();
    let today = main_today(&model);
    // The old yardstick: a break's step that would act was
    // aterm's own class, as at an idle point.
    let before = |s: &interp::State| {
        if s["posture"] == 1 {
            if s["phase"] == 0 || s["clock"] == 1 {
                3
            } else {
                0
            }
        } else {
            truth(&today, s)
        }
    };
    let mut differ = 0;
    for s in reach(&today) {
        let mut looked = s.clone();
        if !today.fire("Watch", &mut looked) || before(&s) == truth(&today, &s) {
            continue;
        }
        assert_eq!(s["posture"], 1, "only at a settled break: {s:?}");
        assert!(
            today.action_enabled("StepAtBreak", &s),
            "only where the break's step would act: {s:?}"
        );
        assert_eq!(
            (looked["cause"], before(&s)),
            (0, 3),
            "main's look records its own work, the old yardstick aterm's own: {s:?}"
        );
        differ += 1;
    }
    assert!(differ > 0, "the re-decision is reached");
}
