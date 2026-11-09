// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `TestDispatch` (`aterm_models_capacity::test_dispatch_model`): the real
//! `aterm_verify::test_plan::Dispatcher`, on the model's four binaries, share and claim, is driven
//! through every schedule of starts, ends, live-rate reads and a halt, and each call's effect,
//! projected onto the model's variables, is replayed through the model. A start the dispatcher
//! makes is the model's `Start<i>`, an end `Finish<i>`, a live rate a read moves `LiveRises` or
//! `LiveFalls`, a halt `Halt`; a start
//! or an end it refuses is one the model has no step for. The schedules reach exactly the model's
//! states.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use aterm_models_capacity::test_dispatch_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_verify::test_plan::{Dispatcher, Phase, Plan, Planned};

type S = BTreeMap<&'static str, i64>;

/// The model's constant `name`.
fn constant(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no constant {name}"))
}

/// The model's four binaries, share and claim, as a plan and a dispatcher of it.
fn dispatcher(m: &Model) -> Dispatcher {
    let u32_of = |name: &str| u32::try_from(constant(m, name)).expect("a small constant");
    let u64_of = |name: &str| u64::try_from(constant(m, name)).expect("a small constant");
    let plan = Plan {
        share: u32_of("Share"),
        target: Duration::from_secs(1),
        order: (1..=4)
            .map(|i| Planned {
                index: i - 1,
                // A waiting binary may use every libtest thread while consuming fewer cores.
                // Charging its width instead breaks the model's reachable schedules.
                width: u32_of("Share"),
                charge: u32_of(&format!("C{i}")),
                peak: u64_of(&format!("P{i}")),
                predicted: Some(Duration::from_secs(1)),
            })
            .collect(),
    };
    Dispatcher::new(&plan, Some(u64_of("Memory")))
}

/// The real dispatcher's state on the model's variables, `live` being the last rate read.
fn project(d: &Dispatcher, live: i64) -> S {
    let phase = |i: usize| match d.phase(i) {
        Some(Phase::Queued) => 0,
        Some(Phase::Running) => 1,
        Some(Phase::Done) => 2,
        None => panic!("binary {i} is not in the plan"),
    };
    let started = (0..4).filter(|&i| phase(i) > 0).count();
    BTreeMap::from([
        ("p1", phase(0)),
        ("p2", phase(1)),
        ("p3", phase(2)),
        ("p4", phase(3)),
        ("next", i64::try_from(started).expect("four") + 1),
        ("held", i64::from(d.held())),
        ("bytes", i64::try_from(d.held_bytes()).expect("small")),
        ("live", live),
        ("halted", i64::from(d.halted())),
        ("started_halted", 0),
        ("extra", 0),
    ])
}

/// Every action named `prefix<1..=4>` has no step from `state`.
fn none_enabled(m: &Model, prefix: &str, state: &S) -> bool {
    (1..=4).all(|i| m.successors(&format!("{prefix}{i}"), state).is_empty())
}

#[test]
fn every_schedule_of_the_real_dispatcher_is_a_run_of_the_model() {
    let m = test_dispatch_model();
    let share = constant(&m, "Share");
    let start = (dispatcher(&m), 0_i64);
    assert_eq!(
        project(&start.0, 0),
        m.init_state(),
        "the initial states agree"
    );
    let mut seen: BTreeSet<S> = BTreeSet::new();
    let mut queue = VecDeque::from([start]);
    let replay = |prev: &S, next: &S, want: &str| {
        assert_eq!(
            interp::admits(&m, prev, next),
            Some(
                m.actions
                    .iter()
                    .find(|a| a.name == want)
                    .expect("an action")
                    .name
            ),
            "the dispatcher's {want} from {prev:?} to {next:?} is no step of the model"
        );
    };
    while let Some((d, live)) = queue.pop_front() {
        let prev = project(&d, live);
        if !seen.insert(prev.clone()) {
            continue;
        }
        let mut next = d.clone();
        let rate = f64::from(i32::try_from(live).expect("small"));
        match next.next(rate) {
            Some(b) => {
                replay(
                    &prev,
                    &project(&next, live),
                    &format!("Start{}", b.index + 1),
                );
                queue.push_back((next, live));
            }
            None => assert!(
                none_enabled(&m, "Start", &prev),
                "the dispatcher refused a start the model takes from {prev:?}"
            ),
        }
        for i in 0..4 {
            let mut next = d.clone();
            match next.finish(i) {
                Ok(()) => {
                    replay(&prev, &project(&next, live), &format!("Finish{}", i + 1));
                    queue.push_back((next, live));
                }
                Err(_) => assert!(
                    m.successors(&format!("Finish{}", i + 1), &prev).is_empty(),
                    "the dispatcher refused an end the model takes from {prev:?}"
                ),
            }
        }
        if d.held() > 0 {
            if live <= share {
                replay(&prev, &project(&d, live + 1), "LiveRises");
                queue.push_back((d.clone(), live + 1));
            }
            if live > 0 {
                replay(&prev, &project(&d, live - 1), "LiveFalls");
                queue.push_back((d.clone(), live - 1));
            }
        }
        if !d.halted() {
            let mut next = d.clone();
            next.halt();
            replay(&prev, &project(&next, live), "Halt");
            queue.push_back((next, live));
        }
    }
    let reachable: BTreeSet<S> = interp::reachable_states(&m).into_iter().collect();
    assert_eq!(
        seen, reachable,
        "the schedules reach exactly the model's states"
    );
}

/// NEGATIVE CONTROL: the model refuses what the real dispatcher refuses. A second release of the
/// first binary's charge, which `finish` refuses, is a step only of the mutant dial
/// (`ReleaseAgain1`), and so is a second binary started beside the first past the share
/// (`StartOverShare2`); the committed model, which the schedules replay through, admits neither,
/// and the mutant dial breaks its invariants.
#[test]
fn the_model_refuses_a_double_release_and_a_start_past_the_share() {
    let m = test_dispatch_model();
    let buggy = interp::with_buggy(&m, 1);
    let mut d = dispatcher(&m);
    assert!(d.next(0.0).is_some());
    let running = project(&d, 0);
    let mut over = running.clone();
    over.insert("p2", 1);
    over.insert("next", 3);
    over.insert("held", running["held"] + constant(&m, "C2"));
    over.insert("bytes", running["bytes"] + constant(&m, "P2"));
    assert_eq!(
        d.next(0.0),
        None,
        "the second does not fit beside the first"
    );
    assert_eq!(interp::admits(&m, &running, &over), None);
    assert_eq!(
        interp::admits(&buggy, &running, &over),
        Some("StartOverShare2")
    );
    d.finish(0).expect("it ran");
    let ended = project(&d, 0);
    assert!(d.finish(0).is_err(), "a second end is refused");
    let mut again = ended.clone();
    again.insert("held", ended["held"] - constant(&m, "C1"));
    again.insert("bytes", ended["bytes"] - constant(&m, "P1"));
    again.insert("extra", 1);
    assert_eq!(interp::admits(&m, &ended, &again), None);
    assert_eq!(
        interp::admits(&buggy, &ended, &again),
        Some("ReleaseAgain1")
    );
    assert!(interp::bmc(&m).is_ok());
    assert!(interp::bmc(&buggy).is_err());
}
