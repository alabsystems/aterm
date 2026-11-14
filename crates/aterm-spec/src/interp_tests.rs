// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::test_models::{config_catalog_snapshot_model, ring_model, subscribe_model};

#[test]
fn admission_rejects_ambiguous_model_declarations() {
    use crate::derive::{Invariant, InvariantKind, StateVar, bool_lit};
    use std::panic::catch_unwind;

    let model = Model {
        name: "Admission",
        consts: vec![],
        vars: vec![StateVar { name: "x", init: 0 }],
        fn_vars: vec![],
        actions: vec![],
        invariants: vec![],
    };
    assert_eq!(bmc(&model), Ok(1), "the unique finite model is valid");
    assert!(Resolved::new(&model).is_some());
    assert!(violated_invariants(&model).is_empty());

    let mut scalar_alias = model.clone();
    scalar_alias.name = "ScalarAlias";
    scalar_alias.vars.push(StateVar { name: "x", init: 1 });
    let mut constant_alias = model.clone();
    constant_alias.name = "ConstantAlias";
    constant_alias.consts = vec![("K", 1), ("K", 2)];
    let mut role_alias = model.clone();
    role_alias.name = "RoleAlias";
    role_alias.consts = vec![("x", 7)];
    let mut law_alias = model;
    law_alias.name = "LawAlias";
    let law = Invariant {
        name: "Case",
        kind: InvariantKind::Claim,
        expr: bool_lit(true),
    };
    law_alias.invariants = vec![law.clone(), law];
    for model in [scalar_alias, constant_alias, role_alias, law_alias] {
        assert!(
            catch_unwind(|| bmc(&model)).is_err(),
            "{}: BMC must refuse ambiguous declarations before checking a collapsed map or law list",
            model.name
        );
        assert!(
            catch_unwind(|| Resolved::new(&model)).is_err(),
            "{}",
            model.name
        );
        assert!(
            catch_unwind(|| Space::of(&model)).is_err(),
            "{}",
            model.name
        );
        assert!(
            catch_unwind(|| model.init_state()).is_err(),
            "{}",
            model.name
        );
        assert!(
            catch_unwind(|| violated_invariants(&model)).is_err(),
            "{}: the empty-law fast path must still refuse ambiguous declarations",
            model.name
        );
    }
}

#[test]
fn conformance_rejects_ambiguous_declarations_before_observed_steps() {
    use crate::derive::{Action, StateVar, Update, cst};
    use std::panic::catch_unwind;

    let mut model = Model {
        name: "ObservedAdmission",
        consts: vec![("K", 1)],
        vars: vec![StateVar { name: "x", init: 0 }],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Tick",
            guard: None,
            updates: vec![Update {
                var: "x",
                expr: cst("K"),
            }],
        }],
        invariants: vec![],
    };
    // Observations come from outside the interpreter; init_state is not called.
    let prev = State::from([("x", 0)]);
    let next = State::from([("x", 1)]);
    assert_eq!(admits(&model, &prev, &next), Some("Tick"));
    assert_eq!(admits(&model, &prev, &State::from([("x", 9)])), None);
    model.consts.push(("K", 2));
    assert!(catch_unwind(|| model.to_tla()).is_err());
    assert!(catch_unwind(|| model.init_state()).is_err());
    assert!(
        catch_unwind(|| admits(&model, &prev, &State::from([("x", 2)]))).is_err(),
        "conformance must not accept an observed step by collapsing duplicate constants"
    );
}

#[test]
fn repeated_updates_constrain_the_same_successor_value() {
    use crate::derive::{Action, Expr, StateVar, Update, in_range, int, var};

    let cases: Vec<(Expr, Expr, Vec<i64>)> = vec![
        (int(1), int(2), vec![]),
        (int(1), int(1), vec![1]),
        (int(1), var("x"), vec![]),
        (var("x"), int(0), vec![0]),
        (in_range(int(2), int(1)), int(1), vec![]),
        (int(1), in_range(int(2), int(1)), vec![]),
        (
            int(i64::MAX),
            in_range(int(i64::MAX), int(i64::MAX)),
            vec![i64::MAX],
        ),
        (
            in_range(int(0), int(2)),
            in_range(int(1), int(3)),
            vec![1, 2],
        ),
        (in_range(int(0), int(2)), int(1), vec![1]),
        (int(1), in_range(int(0), int(2)), vec![1]),
        (in_range(int(0), int(1)), in_range(int(2), int(3)), vec![]),
    ];
    for (first, second, expected) in cases {
        let model = Model {
            name: "RepeatedUpdates",
            consts: vec![],
            vars: vec![
                StateVar { name: "x", init: 0 },
                StateVar { name: "y", init: 7 },
            ],
            fn_vars: vec![],
            actions: vec![Action {
                name: "Tick",
                guard: None,
                updates: vec![
                    Update {
                        var: "x",
                        expr: first.clone(),
                    },
                    Update {
                        var: "x",
                        expr: second.clone(),
                    },
                ],
            }],
            invariants: vec![],
        };
        let state = model.init_state();
        let named = model.successors("Tick", &state);
        assert!(named.iter().all(|s| s["y"] == 7));
        let interpreted: Vec<_> = named.iter().map(|s| s["x"]).collect();
        let resolved = Resolved::new(&model).expect("declared scalar updates compile");
        let compact = resolved.successors(0, &[0, 7]);
        assert!(compact.iter().all(|s| s[1] == 7));
        let compiled: Vec<_> = compact.iter().map(|s| s[0]).collect();
        assert_eq!(compiled, expected, "compiled: {first:?} / {second:?}");
        assert_eq!(interpreted, expected, "by name: {first:?} / {second:?}");
    }
}

#[test]
fn repeated_constraints_filter_interleaved_cartesian_rows() {
    use crate::derive::{Action, StateVar, Update, in_range, int};

    let model = Model {
        name: "InterleavedUpdates",
        consts: vec![],
        vars: vec![
            StateVar { name: "x", init: 0 },
            StateVar { name: "y", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Tick",
            guard: None,
            updates: vec![
                Update {
                    var: "x",
                    expr: in_range(int(0), int(2)),
                },
                Update {
                    var: "y",
                    expr: in_range(int(3), int(4)),
                },
                Update {
                    var: "x",
                    expr: in_range(int(1), int(2)),
                },
                Update {
                    var: "y",
                    expr: in_range(int(4), int(5)),
                },
            ],
        }],
        invariants: vec![],
    };
    let named: Vec<_> = model
        .successors("Tick", &model.init_state())
        .iter()
        .map(|s| vec![s["x"], s["y"]])
        .collect();
    let compact = Resolved::new(&model).unwrap().successors(0, &[0, 0]);
    let expected = vec![vec![1, 4], vec![2, 4]];
    assert_eq!(named, expected);
    assert_eq!(compact, expected);
}

/// [`key_hash`]'s two shifts are the rotate by 5 of the `FxHash` mix, for keys of every
/// length to 4 over values with every high and low bit pattern a rotate carries. NEGATIVE
/// CONTROL: shifting right by 58 instead of 59 gives another hash.
#[test]
fn key_hash_is_the_rotate_mix() {
    let rotated = |key: &[i64]| {
        key.iter().fold(key.len() as u64, |h, &v| {
            (h.rotate_left(5) ^ v as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95)
        })
    };
    let values = [
        0,
        1,
        -1,
        i64::MIN,
        i64::MAX,
        0x0f0f_0f0f_0f0f_0f0f,
        -0x1234_5678_9abc,
    ];
    for len in 0..=4 {
        for start in 0..values.len() {
            let key: Vec<i64> = values
                .iter()
                .cycle()
                .skip(start)
                .take(len)
                .copied()
                .collect();
            assert_eq!(key_hash(&key), rotated(&key), "{key:?}");
        }
    }
}

fn assert_lasso(m: &Model, cycle: &NonProgressCycle, goal: impl Fn(&State) -> bool) {
    assert_eq!(
        cycle.stem[0],
        ("Init", m.init_state()),
        "the stem starts at Init"
    );
    for pair in cycle.stem.windows(2) {
        let ((_, before), (action, after)) = (&pair[0], &pair[1]);
        assert!(
            m.successors(action, before).contains(after),
            "stem step {action} is not the model's: {before:?} -> {after:?}"
        );
    }
    let mut at = cycle.entry().clone();
    assert!(!goal(&at), "the loop's entry is a goal state");
    for (action, next) in &cycle.cycle {
        assert!(
            m.successors(action, &at).contains(next),
            "loop step {action} is not the model's: {at:?} -> {next:?}"
        );
        assert!(!goal(next), "the loop passes a goal state at {next:?}");
        at = next.clone();
    }
    assert_eq!(&at, cycle.entry(), "the loop closes on its entry");
}

#[test]
fn a_retry_loop_that_never_lands_passes_every_older_check_and_is_found() {
    let m = crate::ty_model! {
        RetryLoop {
            const Buggy = 0;
            var landed = 0;
            var attempt = 0;
            action Retry when (landed == 0) {
                attempt = if attempt == 0 { 1 } else { 0 };
            }
            action Land when (Buggy == 0 && landed == 0) { landed = 1; }
            invariant AttemptIsAFlag: attempt <= 1;
        }
    };
    let landed = |s: &State| s["landed"] == 1;
    let fair = Fairness {
        weak: &["Retry", "Land"],
        strong: &[],
    };
    assert_eq!(find_nonprogress_cycle(&m, landed, &fair), None);

    let buggy = with_buggy(&m, 1);
    assert!(bmc(&buggy).is_ok(), "no invariant sees the loop");
    assert_eq!(
        find_deadlock(&buggy, landed),
        None,
        "no state is stuck, so the deadlock check sees nothing"
    );
    let cycle = find_nonprogress_cycle(&buggy, landed, &fair)
        .expect("the retry loop never lands, and fairness cannot help it");
    assert_lasso(&buggy, &cycle, landed);
    assert_eq!(cycle.moves, BTreeSet::from(["Retry"]));
    assert!(
        !cycle.cycle.is_empty() && cycle.cycle.iter().all(|(a, _)| *a == "Retry"),
        "a real loop of retries, not a stall: {cycle}"
    );
}

#[test]
fn a_loop_several_steps_from_init_is_reported_with_the_run_that_reaches_it() {
    let m = crate::ty_model! {
        Approach {
            const Buggy = 0;
            var phase = 0;
            var spin = 0;
            var done = 0;
            action Walk when (phase <= 1) { phase = phase + 1; }
            action Spin when (phase == 2 && done == 0) {
                spin = if spin == 0 { 1 } else { 0 };
            }
            action Finish when (Buggy == 0 && phase == 2 && done == 0) { done = 1; }
            invariant SpinIsAFlag: spin <= 1;
        }
    };
    let done = |s: &State| s["done"] == 1;
    let fair = Fairness {
        weak: &["Walk", "Spin", "Finish"],
        strong: &[],
    };
    assert_eq!(find_nonprogress_cycle(&m, done, &fair), None);
    let buggy = with_buggy(&m, 1);
    let cycle = find_nonprogress_cycle(&buggy, done, &fair).expect("the spin never finishes");
    assert_lasso(&buggy, &cycle, done);
    let stem: Vec<(&str, i64)> = cycle.stem.iter().map(|(a, s)| (*a, s["phase"])).collect();
    assert_eq!(stem, [("Init", 0), ("Walk", 1), ("Walk", 2)]);
}

#[test]
fn a_lane_that_always_lands_is_clean_and_its_fairness_is_what_makes_it_so() {
    let m = crate::ty_model! {
        Climb {
            const Top = 3;
            var x = 0;
            action Step when (x <= Top - 1) { x = x + 1; }
        }
    };
    let top = |s: &State| s["x"] == 3;
    let fair = Fairness {
        weak: &["Step"],
        strong: &[],
    };
    assert_eq!(find_nonprogress_cycle(&m, top, &fair), None);

    let stall = find_nonprogress_cycle(&m, top, &Fairness::default())
        .expect("with nothing fair, the run may stop anywhere short of the top");
    assert_lasso(&m, &stall, top);
    assert!(stall.cycle.is_empty(), "a stall stutters: {stall}");
    assert_eq!(stall.entry(), &m.init_state(), "the nearest stall is Init");
    assert_eq!(
        find_deadlock(&m, top),
        None,
        "and no deadlock check would ever report a stall that has a way out"
    );
}

#[test]
fn fairness_excludes_starvation_only_cycles() {
    let spin = crate::ty_model! {
        Spin {
            var tick = 0;
            var done = 0;
            action Tick when (done == 0) { tick = if tick == 0 { 1 } else { 0 }; }
            action Land when (done == 0) { done = 1; }
        }
    };
    let done = |s: &State| s["done"] == 1;
    let unfair = Fairness {
        weak: &["Tick"],
        strong: &[],
    };
    let starved = find_nonprogress_cycle(&spin, done, &unfair)
        .expect("with Land unfair, ticking forever is a behaviour");
    assert_lasso(&spin, &starved, done);
    assert_eq!(starved.moves, BTreeSet::from(["Tick"]));
    let weakly = Fairness {
        weak: &["Tick", "Land"],
        strong: &[],
    };
    assert_eq!(
        find_nonprogress_cycle(&spin, done, &weakly),
        None,
        "the loop starves a continuously enabled Land: weak fairness excludes it"
    );

    let flicker = crate::ty_model! {
        Flicker {
            var busy = 0;
            var done = 0;
            action Busy when (done == 0 && busy == 0) { busy = 1; }
            action Idle when (done == 0 && busy == 1) { busy = 0; }
            action Land when (done == 0 && busy == 0) { done = 1; }
        }
    };
    let weak_land = Fairness {
        weak: &["Idle", "Land"],
        strong: &[],
    };
    let flickers = find_nonprogress_cycle(&flicker, done, &weak_land)
        .expect("weak fairness does not force a gate the environment keeps closing");
    assert_lasso(&flicker, &flickers, done);
    assert_eq!(flickers.moves, BTreeSet::from(["Busy", "Idle"]));
    assert_eq!(flickers.cycle.len(), 2, "busy, then idle again: {flickers}");
    let strong_land = Fairness {
        weak: &["Idle"],
        strong: &["Land"],
    };
    assert_eq!(
        find_nonprogress_cycle(&flicker, done, &strong_land),
        None,
        "a gate that keeps opening is caught open: strong fairness excludes it"
    );
}

#[test]
fn strong_fairness_keeps_a_loop_that_never_passes_the_gate() {
    let ring = crate::ty_model! {
        Rooms {
            var at = 0;
            var done = 0;
            action Forward when (done == 0) { at = if at == 2 { 0 } else { at + 1 }; }
            action Backward when (done == 0) { at = if at == 0 { 2 } else { at - 1 }; }
            action Land when (done == 0 && at == 1) { done = 1; }
        }
    };
    let done = |s: &State| s["done"] == 1;
    let fair = Fairness {
        weak: &["Forward", "Backward"],
        strong: &["Land"],
    };
    let avoids = find_nonprogress_cycle(&ring, done, &fair)
        .expect("rooms 0 and 2 are adjacent: a run can circle them forever");
    assert_lasso(&ring, &avoids, done);
    assert!(
        avoids.cycle.iter().all(|(_, s)| s["at"] != 1) && avoids.entry()["at"] != 1,
        "the fair loop never enters the room the gate is in: {avoids}"
    );
    assert_eq!(avoids.component, 2);

    let pair = crate::ty_model! {
        TwoRooms {
            var at = 0;
            var done = 0;
            action Forward when (done == 0) { at = if at == 1 { 0 } else { 1 }; }
            action Land when (done == 0 && at == 1) { done = 1; }
        }
    };
    let fair = Fairness {
        weak: &["Forward"],
        strong: &["Land"],
    };
    assert_eq!(find_nonprogress_cycle(&pair, done, &fair), None);
}

#[test]
fn a_wedge_short_of_the_goal_is_reported_as_a_stall() {
    let m = crate::ty_model! {
        Wedge {
            var at = 0;
            action Stray when (at == 0) { at = 1; }
            action Finish when (at == 0) { at = 2; }
        }
    };
    let finished = |s: &State| s["at"] == 2;
    let fair = Fairness {
        weak: &["Finish"],
        strong: &[],
    };
    let wedge = find_nonprogress_cycle(&m, finished, &fair).expect("`Stray` wedges");
    assert_lasso(&m, &wedge, finished);
    assert!(wedge.cycle.is_empty() && wedge.moves.is_empty(), "{wedge}");
    assert_eq!(wedge.entry()["at"], 1);
    assert_eq!(find_deadlock(&m, finished).map(|s| s["at"]), Some(1));
}

#[test]
fn a_step_that_changes_nothing_is_a_stutter_not_progress() {
    let m = crate::ty_model! {
        Poller {
            var done = 0;
            action Poll when (done == 0) { done = 0; }
        }
    };
    let done = |s: &State| s["done"] == 1;
    let fair = Fairness {
        weak: &["Poll"],
        strong: &[],
    };
    let stall = find_nonprogress_cycle(&m, done, &fair).expect("polling is not landing");
    assert!(stall.cycle.is_empty() && stall.moves.is_empty(), "{stall}");
}

#[test]
fn components_are_exactly_the_mutual_reachability_classes() {
    let mut rng = crate::rng::XorShift::new(0x2545_F491_4F6C_DD1D);
    for _ in 0..300 {
        let n = rng.index(12) + 1;
        let steps: Vec<Vec<(usize, usize)>> = (0..n)
            .map(|_| {
                let fan = rng.index(4);
                (0..fan).map(|_| (0, rng.index(n))).collect()
            })
            .collect();
        let graph = StateGraph {
            steps,
            parent: vec![None; n],
            fired: BTreeSet::new(),
        };
        let nodes: Vec<usize> = (0..n).filter(|_| rng.index(4) != 0).collect();
        let member: BTreeSet<usize> = nodes.iter().copied().collect();
        let reaches = |from: usize| -> BTreeSet<usize> {
            let mut seen = BTreeSet::from([from]);
            let mut q = VecDeque::from([from]);
            while let Some(v) = q.pop_front() {
                for &(_, w) in &graph.steps[v] {
                    if member.contains(&w) && seen.insert(w) {
                        q.push_back(w);
                    }
                }
            }
            seen
        };
        let reach: BTreeMap<usize, BTreeSet<usize>> =
            nodes.iter().map(|&v| (v, reaches(v))).collect();
        let components = graph.components(&nodes);
        let mut covered = BTreeSet::new();
        for component in &components {
            for &v in component {
                assert!(
                    covered.insert(v),
                    "{v} is in two components: {components:?}"
                );
            }
            for &a in component {
                for &b in &nodes {
                    let mutual = reach[&a].contains(&b) && reach[&b].contains(&a);
                    assert_eq!(
                        component.contains(&b),
                        mutual,
                        "{a} and {b}: components {components:?} over {:?}",
                        graph.steps
                    );
                }
            }
        }
        assert_eq!(covered, member, "every asked-about state is in a component");
    }
}

#[test]
fn violation_steps_answer_each_action_as_its_own_walk_would() {
    let m = crate::ty_model! {
        ViolationSteps {
            const Cap = 2;
            var armed = 0;
            var fired = 0;
            var spun = 0;
            var late = 0;
            var tick = 0;
            action Arm when (armed == 0) { armed = 1; }
            action Tick when (tick == 0) { tick = 1; }
            action Spin when (spun == 0) { spun = 1; }
            action Fire when (armed == 1 && fired == 0) { fired = 1; }
            action Stuck when (spun == Cap) { spun = 0; }
            action Late when (late == 0) { late = if spun == 1 { 2 } else { 1 }; }
            invariant NeverArmed: armed == 0;
            invariant NeverHalfLate: 1 > late || late > 1;
        }
    };
    let actions = ["Arm", "Spin", "Fire", "Stuck", "Late"];
    let alone: Vec<Option<bool>> = actions
        .iter()
        .map(|a| violation_steps(&m, &[a])[0])
        .collect();
    assert_eq!(alone, [Some(true), Some(false), None, None, Some(false)]);
    assert_eq!(violation_steps(&m, &actions), alone);
    assert_eq!(
        violation_steps(&m, &["Stuck", "Fire", "Spin", "Arm", "Spin"]),
        [None, None, Some(false), Some(true), Some(false)]
    );
    assert_eq!(
        violation_steps(&m, &["Spin", "Spin"]),
        [Some(false), Some(false)]
    );
    assert_eq!(
        violation_steps(&m, &["Spin", "Late"]),
        [Some(false), Some(false)]
    );
}

/// One walk answers what both walks would, whichever is done first: every law fallen does not
/// end a walk with a watched action still open (`Far` fires from a lawful state only after
/// `Unbroken` fell), nor every watched action landed lawful one with a law still to fall
/// (`Deep`, past `Count`'s first lawful landing); and a step from a state a law already fell
/// counts for nothing (`Three`, from a state past the one where `Low` fell).
#[test]
fn a_mutant_report_is_the_laws_and_the_steps_in_one_walk() {
    let laws_first = crate::ty_model! {
        LawsFirst {
            var broke = 0;
            var n = 0;
            action Count when (n <= 1) { n = n + 1; }
            action Break when (broke == 0) { broke = 1; }
            action Far when (n == 2 && broke == 0) { n = 0; }
            invariant Unbroken: broke == 0;
        }
    };
    let steps_first = crate::ty_model! {
        StepsFirst {
            var broke = 0;
            var n = 0;
            action Count when (n <= 2) { n = n + 1; }
            action Break when (broke == 0) { broke = 1; }
            invariant Unbroken: broke == 0;
            invariant Deep: n <= 2;
        }
    };
    let past_the_fall = crate::ty_model! {
        PastTheFall {
            var n = 0;
            action One when (n == 0) { n = 1; }
            action Two when (n == 1) { n = 2; }
            action Three when (n == 2) { n = 3; }
            invariant Low: n == 0;
        }
    };
    assert_eq!(
        mutant_report(&past_the_fall, &["Three"]),
        (BTreeSet::from(["Low"]), vec![None])
    );
    assert_eq!(
        mutant_report(&laws_first, &["Far"]),
        (BTreeSet::from(["Unbroken"]), vec![Some(false)])
    );
    assert_eq!(
        mutant_report(&steps_first, &["Count"]),
        (BTreeSet::from(["Deep", "Unbroken"]), vec![Some(false)])
    );
    for m in [
        ring_model(),
        subscribe_model(),
        config_catalog_snapshot_model(),
        laws_first,
        steps_first,
        past_the_fall,
    ] {
        let actions: Vec<&str> = m.actions.iter().map(|a| a.name).collect();
        for b in m.mutant_dials().into_iter().chain([0]) {
            let mutant = with_buggy(&m, b);
            for watched in [&actions[..], &actions[..1], &[]] {
                assert_eq!(
                    mutant_report(&mutant, watched),
                    (
                        violated_invariants(&mutant),
                        violation_steps(&mutant, watched)
                    ),
                    "{} at Buggy = {b}, watching {watched:?}",
                    m.name
                );
            }
        }
    }
}

/// An unbounded walk with one early claim witness and another claim never settled by one.
fn runaway() -> Model {
    crate::ty_model! {
        Runaway {
            var n = 0;
            var broke = 0;
            action Up { n = n + 1; }
            action Break when (broke == 0) { broke = 1; }
            invariant Unbroken: broke == 0;
            invariant Forever: 0 <= n;
        }
    }
}

#[test]
fn a_mutant_report_past_the_budget_keeps_only_witnessed_claims() {
    assert_eq!(
        mutant_report(&runaway(), &["Up"]),
        (BTreeSet::from(["Unbroken"]), vec![Some(false)])
    );
}

#[test]
#[should_panic(expected = "state space unexpectedly large")]
fn a_mutant_report_past_the_budget_with_a_bound_open_cannot_answer() {
    let mut model = runaway();
    model.invariants[1].kind = crate::derive::InvariantKind::Bound;
    let _ = mutant_report(&model, &["Up"]);
}

#[test]
fn a_closed_projection_proves_a_bound_despite_unrelated_unbounded_state() {
    let model = crate::ty_model! {
        UnrelatedGrowth {
            var count = 0;
            var noise = 0;
            action Count when (3 > count) { count = count + 1; }
            action Noise { noise = noise + 1; }
            invariant Quiet: noise == 0;
            bound CountBounded: count <= 3;
        }
    };
    assert_eq!(mutant_report(&model, &[]).0, BTreeSet::from(["Quiet"]));
}

#[test]
#[should_panic(expected = "state space unexpectedly large")]
fn a_mutant_report_past_the_budget_with_a_step_open_cannot_answer() {
    let _ = mutant_report(&runaway(), &["Break"]);
}

#[test]
fn bmc_fired_is_bmc_and_fired_actions_in_one_walk() {
    for m in [
        ring_model(),
        subscribe_model(),
        config_catalog_snapshot_model(),
    ] {
        for b in m.mutant_dials().into_iter().chain([0]) {
            let m = with_buggy(&m, b);
            let (verdict, fired) = bmc_fired(&m);
            assert_eq!(verdict, bmc(&m), "{} Buggy={b}", m.name);
            if verdict.is_ok() {
                assert_eq!(fired, fired_actions(&m), "{} Buggy={b}", m.name);
            }
        }
    }
}

#[test]
fn a_space_answers_what_the_one_shot_walks_answer() {
    let m = crate::ty_model! {
        RetryLoop {
            const Buggy = 0;
            var landed = 0;
            var attempt = 0;
            action Retry when (landed == 0) {
                attempt = if attempt == 0 { 1 } else { 0 };
            }
            action Land when (Buggy == 0 && landed == 0) { landed = 1; }
            invariant AttemptIsAFlag: attempt <= 1;
        }
    };
    let landed = |s: &State| s["landed"] == 1;
    let fair = |weak: &'static [&'static str]| Fairness { weak, strong: &[] };
    for model in [with_buggy(&m, 0), with_buggy(&m, 1)] {
        let space = Space::of(&model);
        for weak in [&["Retry", "Land"][..], &["Retry"], &[]] {
            assert_eq!(
                space.find_nonprogress_cycle(landed, &fair(weak)),
                find_nonprogress_cycle(&model, landed, &fair(weak)),
                "{weak:?} at Buggy = {:?}",
                model.consts
            );
        }
        let live = Liveness {
            name: "Lands",
            goal: crate::derive::eq(crate::derive::var("landed"), crate::derive::int(1)),
            weak: vec!["Retry", "Land"],
            strong: vec![],
            mutants: vec![],
        };
        assert_eq!(
            space.nonprogress_under(&live),
            nonprogress_under(&model, &live)
        );
        let mut bare = model.clone();
        bare.invariants.clear();
        assert_eq!(Ok(space.states()), bmc(&bare));
        assert_eq!(space.fired(), &fired_actions(&model));
    }
    assert_eq!(
        Space::of(&with_buggy(&m, 0)).find_nonprogress_cycle(landed, &fair(&["Retry", "Land"])),
        None
    );
    assert!(
        Space::of(&with_buggy(&m, 1))
            .find_nonprogress_cycle(landed, &fair(&["Retry", "Land"]))
            .is_some()
    );

    let poller = crate::ty_model! {
        Poller {
            var done = 0;
            action Poll when (done == 0) { done = 0; }
        }
    };
    let space = Space::of(&poller);
    assert_eq!(space.fired(), &BTreeSet::from(["Poll"]));
    assert_eq!(space.fired(), &fired_actions(&poller));
    assert_eq!(space.states(), 1);
}

fn assert_walks_agree(m: &Model) {
    let c = Compiled::new(m).expect("a model whose updates write declared variables");
    let by = ByName(m);
    assert_eq!(
        bmc_on(m, &c, MAX_STATES),
        bmc_on(m, &by, MAX_STATES),
        "{}: bmc",
        m.name
    );
    assert_eq!(
        fired_actions_on(m, &c),
        fired_actions_on(m, &by),
        "{}: fired",
        m.name
    );
    assert_eq!(
        violated_invariants_on(m, &c),
        violated_invariants_on(m, &by),
        "{}: violated",
        m.name
    );
    let even = |s: &State| s.values().sum::<i64>() % 2 == 0;
    assert_eq!(
        find_deadlock_on(m, &c, &|_| false),
        find_deadlock_on(m, &by, &|_| false),
        "{}: deadlock",
        m.name
    );
    assert_eq!(
        find_deadlock_on(m, &c, &even),
        find_deadlock_on(m, &by, &even),
        "{}: deadlock (even states final)",
        m.name
    );
    let ((gc, walked), (gb, by_name)) = (explore_on(m, &c), explore_on(m, &by));
    let walked: Vec<&[i64]> = (0..walked.len()).map(|i| walked.key(i)).collect();
    let graph_states: Vec<State> = walked.iter().map(|s| c.state(s)).collect();
    assert_eq!(graph_states, by_name.states, "{}: graph states", m.name);
    assert_eq!(gc.steps, gb.steps, "{}: graph steps", m.name);
    assert_eq!(gc.parent, gb.parent, "{}: graph parents", m.name);
    assert_eq!(gc.fired, gb.fired, "{}: graph fired", m.name);
    let states = reachable_on(m, &c);
    assert_eq!(states, reachable_on(m, &by), "{}: reachable", m.name);
    assert_eq!(states, graph_states, "{}: reachable is the graph's", m.name);
    let r = Resolved::new(m).expect("compiles");
    for (i, name) in m.init_state().keys().enumerate() {
        assert_eq!(r.slot(name), i, "{}: slot of {name}", m.name);
    }
    let mut wrong = r.init();
    wrong.push(0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.state(&wrong))).is_err());
    if !m.actions.is_empty() {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.successors(0, &wrong)))
                .is_err()
        );
    }
    if !m.invariants.is_empty() {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.holds(0, &wrong))).is_err()
        );
    }
    for (st, &compact) in states.iter().zip(&walked) {
        assert_eq!(&c.state(compact), st);
        assert_eq!(st.values().copied().collect::<Vec<_>>(), compact);
        let env = m.eval_env(st);
        for (a, act) in m.actions.iter().enumerate() {
            let compiled: Vec<State> = c
                .successors(a, compact)
                .iter()
                .map(|s| c.state(s))
                .collect();
            assert_eq!(
                compiled,
                m.successors_in(act, &env, st),
                "{}: {} at {st:?}",
                m.name,
                act.name
            );
        }
        for (i, inv) in m.invariants.iter().enumerate() {
            assert_eq!(
                c.holds(i, compact),
                m.check_invariant_in(inv, &env),
                "{}: {} at {st:?}",
                m.name,
                inv.name
            );
        }
    }
}

#[test]
fn compiled_walks_are_the_by_name_walks_on_committed_models() {
    use crate::test_models::{
        native_update_status_reconciliation_model, pane_tree_model, tab_nav_model,
    };
    for m in [
        ring_model(),
        subscribe_model(),
        config_catalog_snapshot_model(),
        tab_nav_model(),
        pane_tree_model(),
        native_update_status_reconciliation_model(),
    ] {
        for b in [0, 1] {
            assert_walks_agree(&with_buggy(&m, b));
        }
    }
}

#[test]
fn compiled_walks_are_the_by_name_walks_on_the_resolution_corners() {
    use crate::derive::{
        Action, Expr, Invariant, StateVar, Update, add, and_, bool_lit, cst, eq, forall, gt, if_,
        iff, in_range, int, le, neq, or_, sub, var,
    };
    let up = |v: &'static str, e: Expr| Update { var: v, expr: e };
    let m = Model {
        name: "Corners",
        consts: vec![("K", 2), ("Buggy", 0)],
        vars: vec![
            StateVar { name: "y", init: 0 },
            StateVar { name: "x", init: 0 },
            StateVar { name: "i", init: 1 },
            StateVar { name: "n", init: 0 },
        ],
        fn_vars: vec![],
        actions: vec![
            Action {
                name: "Nested",
                guard: Some(and_(
                    eq(var("n"), int(0)),
                    forall(
                        "a",
                        int(0),
                        var("x"),
                        forall(
                            "b",
                            add(var("a"), int(1)),
                            add(var("a"), int(1)),
                            and_(gt(var("b"), var("a")), eq(sub(var("b"), var("a")), int(1))),
                        ),
                    ),
                )),
                updates: vec![up("n", int(1))],
            },
            Action {
                name: "Grow",
                guard: Some(and_(
                    le(var("x"), int(2)),
                    forall(
                        "i",
                        int(0),
                        var("x"),
                        forall("i", var("i"), int(2), le(var("i"), add(cst("K"), var("x")))),
                    ),
                )),
                updates: vec![up("x", in_range(var("x"), add(var("x"), int(1))))],
            },
            Action {
                name: "Twice",
                guard: Some(le(var("y"), int(1))),
                updates: vec![
                    up("y", in_range(int(0), int(2))),
                    up("y", add(var("y"), int(1))),
                ],
            },
            Action {
                name: "MaybeEmpty",
                guard: None,
                updates: vec![up("i", in_range(var("x"), int(1)))],
            },
            Action {
                name: "Ops",
                guard: Some(or_(
                    iff(gt(var("y"), int(0)), neq(var("i"), int(0))),
                    eq(cst("Buggy"), int(1)),
                )),
                updates: vec![
                    up(
                        "y",
                        if_(
                            gt(var("y"), int(2)),
                            int(0),
                            sub(var("x"), sub(var("y"), int(1))),
                        ),
                    ),
                    up("i", if_(bool_lit(true), sub(int(1), var("i")), int(9))),
                ],
            },
        ],
        invariants: vec![
            Invariant {
                name: "Bounded",
                kind: crate::derive::InvariantKind::Claim,
                expr: forall("k", int(1), var("i"), le(var("y"), add(var("x"), var("k")))),
            },
            Invariant {
                name: "SmallY",
                kind: crate::derive::InvariantKind::Claim,
                expr: le(var("y"), int(2)),
            },
        ],
    };
    for b in [0, 1] {
        assert_walks_agree(&with_buggy(&m, b));
    }
    assert_eq!(
        fired_actions(&m),
        BTreeSet::from(["Nested", "Grow", "Twice", "MaybeEmpty", "Ops"])
    );
    assert!(reachable_states(&m).len() > 8);
    assert_eq!(
        violated_invariants(&m),
        BTreeSet::from(["Bounded", "SmallY"])
    );
}

#[test]
#[should_panic(expected = "unbound identifier `nope` in model evaluation")]
fn the_compiled_form_raises_an_unbound_identifier_when_it_reads_it() {
    use crate::derive::{Invariant, StateVar, and_, bool_lit, int, le, var};
    let m = Model {
        name: "Unbound",
        consts: vec![],
        vars: vec![StateVar { name: "x", init: 0 }],
        fn_vars: vec![],
        actions: vec![],
        invariants: vec![Invariant {
            name: "Reads",
            kind: crate::derive::InvariantKind::Claim,
            expr: and_(bool_lit(true), le(var("nope"), int(1))),
        }],
    };
    let c = Compiled::new(&m).expect("compiles");
    let _ = c.holds(0, &[0]);
}

#[test]
fn the_compiled_form_reads_what_eval_reads_and_fails_where_eval_fails() {
    use crate::derive::{
        Expr, Invariant, StateVar, add, and_, bool_lit, eq, forall, gt, if_, iff, int, le, neq,
        or_, sub, var,
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let outcome = |f: &dyn Fn() -> Value| match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(p) => Err(p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|m| (*m).to_string()))
            .unwrap_or_default()),
    };
    let x0 = || le(var("x"), int(0));
    let y1 = || eq(var("y"), int(1));
    let cases: Vec<Expr> = vec![
        and_(bool_lit(true), int(1)),
        le(add(var("x"), bool_lit(true)), int(3)),
        if_(gt(var("x"), int(0)), and_(int(1), bool_lit(true)), x0()),
        if_(le(var("x"), int(0)), and_(int(1), bool_lit(true)), x0()),
        le(bool_lit(true), var("nope")),
        le(var("nope"), bool_lit(true)),
        or_(bool_lit(true), int(5)),
        or_(bool_lit(false), int(5)),
        or_(gt(var("x"), int(0)), or_(y1(), var("nope"))),
        and_(and_(x0(), y1()), gt(var("nope"), int(0))),
        and_(and_(x0(), neq(var("y"), int(1))), gt(var("nope"), int(0))),
        and_(x0(), and_(bool_lit(false), var("nope"))),
        and_(var("x"), x0()),
        iff(int(1), bool_lit(true)),
        iff(x0(), y1()),
        forall("k", bool_lit(true), int(2), x0()),
        forall("k", int(0), var("y"), and_(le(var("k"), int(1)), int(3))),
        if_(int(1), x0(), y1()),
        if_(bool_lit(true), add(var("x"), int(1)), bool_lit(false)),
        if_(gt(var("x"), int(0)), int(1), bool_lit(true)),
        le(if_(gt(var("x"), int(0)), var("nope"), int(1)), int(3)),
        le(if_(le(var("x"), int(0)), int(1), var("nope")), int(3)),
        add(var("y"), le(var("x"), int(0))),
        add(var("x"), var("y")),
        var("y"),
        ["i0", "i1", "i2", "i3", "i4", "i5", "i6", "i7", "i8"]
            .iter()
            .zip(0..9_i32)
            .rev()
            .fold(eq(sub(var("i8"), var("i0")), int(8)), |body, (&i, k)| {
                forall(i, int(k.into()), int(k.into()), body)
            }),
    ];
    let mut panics = 0;
    for (i, e) in cases.iter().enumerate() {
        let m = Model {
            name: "Reads",
            consts: vec![],
            vars: vec![
                StateVar { name: "x", init: 0 },
                StateVar { name: "y", init: 1 },
            ],
            fn_vars: vec![],
            actions: vec![],
            invariants: vec![Invariant {
                name: "Case",
                kind: crate::derive::InvariantKind::Claim,
                expr: e.clone(),
            }],
        };
        let c = Compiled::new(&m).expect("compiles");
        let env = m.eval_env(&m.init_state());
        let s = c.init();
        let written = outcome(&|| e.eval(&env));
        let compiled = outcome(&|| c.expr(&m, e).eval(&s));
        assert_eq!(compiled, written, "case {i} read whole: {e:?}");
        panics += usize::from(written.is_err());
        let written = outcome(&|| Value::Bool(m.check_invariant_in(&m.invariants[0], &env)));
        let compiled = outcome(&|| Value::Bool(c.holds(0, &s)));
        assert_eq!(compiled, written, "case {i} read as a boolean: {e:?}");
    }
    assert!(panics >= 8, "only {panics} cases panic");
}

#[test]
fn seen_numbers_keys_whose_tags_collide_apart_and_keeps_numbers_as_it_grows() {
    let (a, b) = (
        [9_031_110_522_149_646_314_i64],
        [5_280_068_066_879_230_317_i64],
    );
    assert_eq!(
        key_hash(&a) & TAG,
        key_hash(&b) & TAG,
        "the keys' tags collide"
    );
    let mut seen = Seen::default();
    assert_eq!(seen.number(&a), (0, true));
    assert_eq!(seen.number(&b), (1, true));
    for i in 2..1000 {
        assert_eq!(seen.number(&[i]), (usize::try_from(i).unwrap(), true));
    }
    assert_eq!(seen.number(&b), (1, false));
    assert_eq!(seen.number(&a), (0, false));
    for i in 2..1000 {
        assert_eq!(seen.number(&[i]), (usize::try_from(i).unwrap(), false));
        assert_eq!(seen.key(usize::try_from(i).unwrap()), [i]);
    }
    assert_eq!((seen.key(0), seen.key(1)), (&a[..], &b[..]));
}

#[test]
fn a_model_writing_an_undeclared_variable_is_walked_as_written() {
    use crate::derive::{Action, StateVar, Update, int};
    let m = Model {
        name: "Grows",
        consts: vec![],
        vars: vec![StateVar { name: "x", init: 0 }],
        fn_vars: vec![],
        actions: vec![Action {
            name: "Stray",
            guard: None,
            updates: vec![Update {
                var: "z",
                expr: int(1),
            }],
        }],
        invariants: vec![],
    };
    assert!(Compiled::new(&m).is_none());
    assert_eq!(bmc(&m), bmc_on(&m, &ByName(&m), MAX_STATES).0);
    assert_eq!(reachable_states(&m), reachable_on(&m, &ByName(&m)));
}

#[test]
fn states_grown_by_different_names_are_different_states() {
    use crate::derive::{Action, StateVar, Update, int};
    let writes = |name: &'static str, var: &'static str| Action {
        name,
        guard: None,
        updates: vec![Update { var, expr: int(1) }],
    };
    let m = Model {
        name: "Forks",
        consts: vec![],
        vars: vec![StateVar { name: "a", init: 0 }],
        fn_vars: vec![],
        actions: vec![writes("SetB", "b"), writes("SetC", "c")],
        invariants: vec![],
    };
    let state = |pairs: &[(&'static str, i64)]| pairs.iter().copied().collect::<State>();
    assert_eq!(
        reachable_states(&m),
        [
            state(&[("a", 0)]),
            state(&[("a", 0), ("b", 1)]),
            state(&[("a", 0), ("c", 1)]),
            state(&[("a", 0), ("b", 1), ("c", 1)]),
        ]
    );
    assert_eq!(bmc(&m), Ok(4));
}

#[test]
#[should_panic(expected = "which the model does not declare")]
fn fairness_naming_an_undeclared_action_panics() {
    let m = crate::ty_model! {
        Typo {
            var x = 0;
            action Step when (x == 0) { x = 1; }
        }
    };
    let fair = Fairness {
        weak: &["Stpe"],
        strong: &[],
    };
    let _ = find_nonprogress_cycle(&m, |s| s["x"] == 1, &fair);
}
