// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;

/// The start rule as a machine. `young`: the stamped start (the latest within the
/// budget) may still confirm; `Settle`: its window passes unconfirmed; `lost`: a
/// rollback taken over a start that could still have confirmed.
fn start_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LinuxTrialStartSettles {
            const Buggy = 0;
            const Max = 3;
            var starts = 0;
            var young = 0;
            var healthy = 0;
            var rolled = 0;
            var lost = 0;
            action Count when (
                healthy == 0 && rolled == 0 && starts <= Max + 2
                    && (starts <= Max - 1 || young == 1)
            ) {
                starts = starts + 1;
                young = 1;
            }
            action RollBack when (
                healthy == 0 && rolled == 0 && starts > Max - 1 && (young == 0 || Buggy == 1)
            ) {
                starts = starts + 1;
                rolled = 1;
                lost = young;
            }
            action Confirm when (young == 1 && healthy == 0 && rolled == 0) {
                healthy = 1;
                young = 0;
            }
            action Settle when (young == 1) {
                young = 0;
            }
            invariant NoConfirmableStartLost: lost == 0;
        }
    }
}

fn projection(starts_before: u32, young: bool) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("starts", i64::from(starts_before)),
        ("young", i64::from(young)),
        ("healthy", 0),
        ("rolled", 0),
        ("lost", 0),
    ])
}

/// The machine proves no rollback is taken over a start that could still confirm, and
/// catches the count-only rule (`Buggy = 1`); then the shipping [`start_rolls_back`]
/// is bound to the model's guards over every count and every age that matters.
#[test]
fn a_start_rolls_back_only_once_the_start_before_it_has_settled() {
    let model = start_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let now = 1_000_000;
    let ages = [
        None,
        Some(0),
        Some(START_SETTLES_SECS - 1),
        Some(START_SETTLES_SECS),
        Some(3_600),
        Some(-5),
    ];
    let mut both = [false, false];
    for starts_before in 0..=crate::LINUX_TRIAL_LAUNCHES + 2 {
        for age in ages {
            let previous = age.map_or(0, |age| now - age);
            let young = start_is_young(previous, now);
            let shipping = start_rolls_back(starts_before + 1, previous, now);
            let state = projection(starts_before, young);
            assert_eq!(
                model.action_enabled("RollBack", &state),
                shipping,
                "RollBack: {starts_before} starts before, previous {age:?} s ago"
            );
            assert_eq!(
                model.action_enabled("Count", &state),
                !shipping,
                "Count: {starts_before} starts before, previous {age:?} s ago"
            );
            both[usize::from(shipping)] = true;
        }
    }
    assert_eq!(both, [true, true], "the grid reaches both verdicts");
    let burst = projection(crate::LINUX_TRIAL_LAUNCHES, true);
    assert!(!model.action_enabled("RollBack", &burst));
    assert!(!start_rolls_back(crate::LINUX_TRIAL_LAUNCHES + 1, now, now));
}

/// A wall-clock step between two starts of one boot moves no verdict: for every count,
/// real age and step (an hour either way, a second back, none), the shipping decision
/// on [`judged_now`] is the model's `RollBack` guard at the real age.
#[test]
fn a_wall_clock_step_between_two_starts_of_one_boot_moves_no_verdict() {
    let model = start_model();
    let steady = |clock: &str, secs: i64| {
        Some(Steady {
            clock: clock.to_string(),
            secs,
        })
    };
    let wall = 1_000_000;
    let previous = StartClock {
        unix: wall,
        steady: steady("boot-a time:[1]", 400),
    };
    let mut both = [false, false];
    for starts_before in 0..=crate::LINUX_TRIAL_LAUNCHES + 2 {
        for age in [0, START_SETTLES_SECS - 1, START_SETTLES_SECS, 3_600] {
            for step in [-3_600, -1, 0, 3_600] {
                let now = StartClock {
                    unix: wall + age + step,
                    steady: steady("boot-a time:[1]", 400 + age),
                };
                let judged = judged_now(&previous, &now);
                assert_eq!(judged, wall + age, "age {age}, step {step}");
                let shipping = start_rolls_back(starts_before + 1, previous.unix, judged);
                let state = projection(starts_before, age < START_SETTLES_SECS);
                assert_eq!(
                    model.action_enabled("RollBack", &state),
                    shipping,
                    "RollBack: {starts_before} before, {age} s on, wall stepped {step} s"
                );
                assert_eq!(
                    model.action_enabled("Count", &state),
                    !shipping,
                    "Count: {starts_before} before, {age} s on, wall stepped {step} s"
                );
                both[usize::from(shipping)] = true;
            }
        }
    }
    assert_eq!(both, [true, true], "the grid reaches both verdicts");
    let budget = crate::LINUX_TRIAL_LAUNCHES;
    assert!(!model.action_enabled("RollBack", &projection(budget, true)));
    assert!(
        start_rolls_back(budget + 1, wall, wall + 3_600),
        "the raw wall clock read the stepped burst as settled"
    );
    let later = wall + 3_600;
    for (then, now) in [
        (previous.steady.clone(), steady("boot-b time:[1]", 401)),
        (previous.steady.clone(), steady("boot-a time:[2]", 401)),
        (previous.steady.clone(), None),
        (None, steady("boot-a time:[1]", 401)),
    ] {
        let then = StartClock {
            unix: wall,
            steady: then,
        };
        let now = StartClock {
            unix: later,
            steady: now,
        };
        assert_eq!(judged_now(&then, &now), later, "{then:?} -> {now:?}");
    }
    let unstamped = StartClock {
        unix: 0,
        steady: steady("boot-a time:[1]", 400),
    };
    let now = StartClock {
        unix: later,
        steady: steady("boot-a time:[1]", 401),
    };
    assert_eq!(judged_now(&unstamped, &now), later, "no start stamped");
    assert!(!start_is_young(
        unstamped.unix,
        judged_now(&unstamped, &now)
    ));
}

/// Seconds per tick of [`deferral_model`]: its settle window `W` is two ticks.
const TICK_SECS: i64 = START_SETTLES_SECS / 2;

/// How long the verdict waits, as a machine with a clock. `age` is ticks since the
/// stamp (capped at the window `W`), `wait` ticks since the first start kept past the
/// budget (capped just past `W` + 1), and `late` records a start kept past the budget
/// more than `W` ticks after the first: the verdict deferred past its bound.
fn deferral_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LinuxTrialDeferralBounded {
            const Buggy = 0;
            const Once = 1;
            const Max = 3;
            const W = 2;
            var starts = 0;
            var age = 0;
            var over = 0;
            var wait = 0;
            var rolled = 0;
            var late = 0;
            var owned = 0;
            action Tick when (
                rolled == 0 && (age <= W - 1 || (over == 1 && wait <= W + 1))
            ) {
                age = if age <= W - 1 { age + 1 } else { age };
                wait = if (over == 1 && wait <= W + 1) { wait + 1 } else { wait };
            }
            action Count when (
                rolled == 0 && starts <= Max + 2 && (starts <= Max - 1 || age <= W - 1)
            ) {
                starts = starts + 1;
                age = if (starts <= Max - 1 || Buggy == 1) { 0 } else { age };
                over = if starts > Max - 1 { 1 } else { over };
                late = if (starts > Max - 1 && wait > W) { 1 } else { late };
                owned = if starts <= Max - 1 { 1 } else { owned };
            }
            action Defer when (
                rolled == 0 && (owned == 1 || Once == 0) && age > 0 && age <= W - 1
            ) {
                age = 0;
                owned = 0;
            }
            action RollBack when (rolled == 0 && starts > Max - 1 && age > W - 1) {
                starts = starts + 1;
                rolled = 1;
            }
            invariant DeferralBounded: late == 0;
        }
    }
}

/// Past the budget the verdict waits at most one settle window, plus less than one more
/// for a silent session, however fast the starts come.
#[test]
fn the_verdict_waits_at_most_one_window_past_the_budget() {
    let model = deferral_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let now = 2_000_000;
    let budget = crate::LINUX_TRIAL_LAUNCHES;
    let mut both = [false, false];
    for starts_before in 0..=budget + 2 {
        for age in 0..=2_i64 {
            for previous in [Some(now - age * TICK_SECS), (age == 2).then_some(0)]
                .into_iter()
                .flatten()
            {
                let state = BTreeMap::from([
                    ("starts", i64::from(starts_before)),
                    ("age", age),
                    ("over", 0),
                    ("wait", 0),
                    ("rolled", 0),
                    ("late", 0),
                    ("owned", 0),
                ]);
                let rolls = start_rolls_back(starts_before + 1, previous, now);
                assert_eq!(
                    model.action_enabled("RollBack", &state),
                    rolls,
                    "RollBack: {starts_before} before, stamp {previous}"
                );
                assert_eq!(
                    model.action_enabled("Count", &state),
                    !rolls,
                    "Count: {starts_before} before, stamp {previous}"
                );
                both[usize::from(rolls)] = true;
                if !rolls {
                    let mut next = state.clone();
                    assert!(model.fire("Count", &mut next));
                    let stamp = start_stamp(starts_before + 1, previous, now);
                    assert_eq!(
                        (now - stamp) / TICK_SECS,
                        next["age"],
                        "the stamp a kept start leaves: {starts_before} before"
                    );
                }
            }
        }
    }
    assert_eq!(both, [true, true], "the grid reaches both verdicts");
    let mut moved = [false, false];
    for age in 0..=2_i64 {
        for owned in [false, true] {
            let state = BTreeMap::from([
                ("starts", i64::from(budget)),
                ("age", age),
                ("over", 0),
                ("wait", 0),
                ("rolled", 0),
                ("late", 0),
                ("owned", i64::from(owned)),
            ]);
            let own = now - age * TICK_SECS;
            let shipping = owned && defer_moves(own, now);
            assert_eq!(
                model.action_enabled("Defer", &state),
                shipping,
                "Defer: stamp {age} tick(s) old, owned {owned}"
            );
            if shipping {
                let mut next = state.clone();
                assert!(model.fire("Defer", &mut next));
                assert_eq!(next["age"], 0, "the move stamps now");
                assert_eq!(next["owned"], 0, "once: the stamp is no longer its own");
            }
            moved[usize::from(shipping)] = true;
        }
    }
    assert_eq!(moved, [true, true], "the grid reaches both answers");
    assert!(!defer_moves(now, now), "no move to the same moment");
    assert!(
        !defer_moves(now - START_SETTLES_SECS, now),
        "nor of a settled stamp"
    );
    assert!(
        aterm_spec::interp::bmc(&aterm_spec::interp::with_consts(&model, &[("Once", 0)])).is_err(),
        "a session that moves the stamp as often as it stays silent defers for good"
    );
    assert_eq!(
        i64::try_from(crate::LINUX_SESSION_SILENT_DEFER.as_secs()).unwrap(),
        TICK_SECS,
        "the silent session moves its stamp one tick (half a window) after its start"
    );
}

/// The two lanes as a machine: `healthy` is the trial confirmed (checks and applies go
/// on), `proven` a window confirmed it, `crashed` a window start counted with no
/// window's confirmation since.
fn lane_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LinuxTrialLanes {
            const Buggy = 0;
            var healthy = 0;
            var proven = 0;
            var crashed = 0;
            action WindowStart when (proven == 0) {
                crashed = 1;
            }
            action WindowConfirm when (proven == 0) {
                healthy = 1;
                proven = 1;
                crashed = 0;
            }
            action SessionConfirm when (healthy == 0) {
                healthy = 1;
                proven = if Buggy == 1 { 1 } else { proven };
            }
            invariant WindowVerdictKept: crashed == 0 || proven == 0;
        }
    }
}

fn lanes_projection(lanes: Lanes, crashed: bool) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("healthy", i64::from(lanes.healthy)),
        ("proven", i64::from(lanes.window_proven)),
        ("crashed", i64::from(crashed)),
    ])
}

/// A session never vouches for a window: the model is proved (catching `Buggy = 1`),
/// and in all eight states [`start_counts`] and [`confirm`] match its guards and actions.
#[test]
fn a_session_never_vouches_for_a_window() {
    let model = lane_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut reached = [false; 2];
    for bits in 0..8u8 {
        let lanes = Lanes {
            healthy: bits & 1 != 0,
            window_proven: bits & 2 != 0,
        };
        let crashed = bits & 4 != 0;
        let state = lanes_projection(lanes, crashed);
        assert_eq!(
            model.action_enabled("WindowStart", &state),
            start_counts(Lane::Window, lanes),
            "WindowStart: {lanes:?}"
        );
        assert_eq!(start_counts(Lane::Session, lanes), !lanes.healthy);
        for (action, lane) in [
            ("WindowConfirm", Lane::Window),
            ("SessionConfirm", Lane::Session),
        ] {
            let enabled = model.action_enabled(action, &state);
            match confirm(lane, lanes) {
                LaneConfirm::Confirms {
                    lanes: after,
                    settles,
                } => {
                    assert!(enabled, "{action}: {lanes:?}");
                    let mut next = state.clone();
                    assert!(model.fire(action, &mut next));
                    assert_eq!(next["healthy"], i64::from(after.healthy), "{action}");
                    assert_eq!(next["proven"], i64::from(after.window_proven), "{action}");
                    assert_eq!(settles, !lanes.healthy, "{action}: {lanes:?}");
                    reached[0] = true;
                }
                LaneConfirm::NotPending => {
                    assert!(!enabled, "{action}: {lanes:?}");
                    reached[1] = true;
                }
            }
        }
    }
    assert_eq!(reached, [true; 2], "the grid reaches every answer");
}
