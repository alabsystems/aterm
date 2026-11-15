// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::Scope;
use crate::test_history::{History, Sample};

fn sample(cpu_ms: Option<u64>, at_ms: u64) -> Sample {
    Sample {
        at_ms,
        outcome: "ok".into(),
        wall_ms: 1000,
        cpu_ms,
        starved_ms: None,
        peak_bytes: None,
        width: 1,
        cores: Some(1),
        tests: Some(1),
        partition: None,
    }
}

/// One binary's samples, oldest first: each one's CPU reading and finish time.
type Samples<'s> = &'s [(Option<u64>, u64)];

/// A history of `rows`: each a binary's re-run spec and its samples.
fn history(rows: &[(&str, Samples<'_>)]) -> History {
    let mut history = History::default();
    for (spec, samples) in rows {
        for (cpu, at) in *samples {
            history.push(spec, sample(*cpu, *at));
        }
    }
    history
}

const MIN: u64 = 60_000;

fn packages(names: &[&str]) -> Scope {
    Scope::Packages(names.iter().map(|n| (*n).to_owned()).collect())
}

/// The empty cone: a change that touched no crate.
fn no_crates() -> Scope {
    Scope::Changed(crate::scope::Changed {
        base: "main".into(),
        crates: Vec::new(),
        any_lib: false,
        seeds: Vec::new(),
        alone: Vec::new(),
    })
}

/// A run is predicted from each of its crates' binaries' latest CPU reading (a later sample that
/// read none leaves the one before standing), summed and spread over the machine's cores; the
/// whole workspace is every recorded crate. A binary the dispatch splits counts by the halves of
/// its newest split: not by its parent's row from before the split, nor by an older split's
/// halves, which the history keeps; one run whole again counts by its parent's row alone.
/// NEGATIVE CONTROL: another crate's binaries add nothing to a scope that does not hold it.
#[test]
fn a_run_is_predicted_from_its_crates_binaries_as_the_dispatch_runs_them() {
    let history = history(&[
        (
            "-p alpha --lib",
            &[(Some(90 * MIN), 0), (Some(30 * MIN), 1), (None, 2)],
        ),
        ("-p alpha --test it", &[(Some(6 * MIN), 1)]),
        ("-p beta --lib", &[(Some(18 * MIN), 1)]),
        // delta's lib was run whole, then split at `old::`, then at `new::`.
        ("-p delta --lib", &[(Some(100 * MIN), 0)]),
        ("-p delta --lib -- old::", &[(Some(40 * MIN), 1)]),
        ("-p delta --lib -- --skip old::", &[(Some(45 * MIN), 1)]),
        ("-p delta --lib -- --skip new::", &[(Some(25 * MIN), 6)]),
        ("-p delta --lib -- new::", &[(Some(20 * MIN), 5)]),
        ("-p delta --test it", &[(Some(MIN), 6)]),
        // eps's lib was split, then run whole again.
        ("-p eps --lib -- a::", &[(Some(10 * MIN), 1)]),
        ("-p eps --lib -- --skip a::", &[(Some(10 * MIN), 1)]),
        ("-p eps --lib", &[(Some(7 * MIN), 9)]),
    ]);
    assert_eq!(
        predict(&history, &packages(&["alpha"]), 18),
        Prediction {
            cpu_ms: 36 * MIN,
            machine_ms: 2 * MIN,
            unknown: 0,
        }
    );
    assert_eq!(
        predict(&history, &packages(&["beta"]), 18).cpu_ms,
        18 * MIN,
        "alpha's binaries are not beta's"
    );
    assert_eq!(
        predict(&history, &packages(&["delta"]), 18).cpu_ms,
        (20 + 25 + 1) * MIN,
        "the newest split's halves, not the parent's or the older split's rows"
    );
    assert_eq!(
        predict(&history, &packages(&["eps"]), 18).cpu_ms,
        7 * MIN,
        "run whole again, by its parent's row alone"
    );
    assert_eq!(
        predict(&history, &Scope::Workspace, 18).cpu_ms,
        (36 + 18 + 46 + 7) * MIN
    );
}

/// A crate none of whose binaries is recorded (none ran, or none read its CPU) counts as the
/// median recorded crate, so a cone of one small unrecorded crate is not heavy even where the
/// largest crate alone is; the empty cone predicts nothing and is not heavy, though the whole
/// workspace is. NEGATIVE CONTROL: the largest crate itself is heavy, and with nothing recorded
/// nothing orders a run as long.
#[test]
fn an_unrecorded_crate_counts_as_the_median_crate_and_the_empty_cone_as_nothing() {
    let history = history(&[
        ("-p huge --lib", &[(Some(60 * MIN), 1)]),
        ("-p one --lib", &[(Some(MIN), 1)]),
        ("-p two --lib", &[(Some(2 * MIN), 1)]),
        ("-p three --lib", &[(Some(3 * MIN), 1)]),
        ("-p unread --lib", &[(None, 1)]),
    ]);
    assert!(predict(&history, &packages(&["huge"]), 18).heavy());
    assert_eq!(
        predict(&history, &packages(&["new", "unread"]), 18),
        Prediction {
            cpu_ms: 2 * 3 * MIN,
            machine_ms: 20_000,
            unknown: 2,
        }
    );
    assert!(!predict(&history, &packages(&["new"]), 18).heavy());
    assert_eq!(
        predict(&history, &no_crates(), 18),
        Prediction::default(),
        "the empty cone judges no crate"
    );
    assert!(predict(&history, &Scope::Workspace, 18).heavy());
    assert_eq!(
        predict(&History::default(), &packages(&["alpha"]), 18),
        Prediction {
            cpu_ms: 0,
            machine_ms: 0,
            unknown: 1,
        },
        "with nothing recorded, nothing orders a run as long"
    );
}

/// A run is heavy from [`HEAVY`] of the machine up: three machine-minutes. NEGATIVE CONTROL: a
/// millisecond less is not.
#[test]
fn a_run_is_heavy_from_three_machine_minutes() {
    let at = |machine_ms| Prediction {
        machine_ms,
        ..Prediction::default()
    };
    assert_eq!(HEAVY, Duration::from_secs(180));
    assert!(at(3 * MIN).heavy());
    assert!(at(40 * MIN).heavy());
    assert!(!at(3 * MIN - 1).heavy());
}

/// Each turn event is a known, well-formed record of its kind that names the prediction and the
/// key: the queued one what it waits behind, the turn how long it waited, the CPU one what the
/// run used against its prediction.
#[test]
fn the_turn_events_are_known_records_with_their_figures() {
    use crate::event_log::{Observation, observation};
    let prediction = Prediction {
        cpu_ms: 90 * MIN,
        machine_ms: 5 * MIN,
        unknown: 1,
    };
    let heavy = aterm_capacity::Heavy {
        since_ms: 1_000,
        predicted_ms: 5 * MIN,
    };
    let read = |line: String, kind: EventKind| {
        let json = line.strip_prefix("verify-event: ").expect("an event line");
        match observation(json) {
            Observation::Known { kind: k, value } if k == kind => value,
            _ => panic!("not a known {kind:?}: {line}"),
        }
    };
    let queued = read(
        queued_event(prediction, heavy, "the judgment turn: pid 7 holds it"),
        EventKind::JudgmentQueued,
    );
    assert_eq!(
        queued["behind"].as_str(),
        Some("the judgment turn: pid 7 holds it")
    );
    assert_eq!(queued["key"].as_u64(), Some(1_000 + 5 * MIN));
    assert_eq!(queued["predicted_ms"].as_u64(), Some(5 * MIN));
    let turn = read(
        turn_event(prediction, heavy, Duration::from_secs(90), true),
        EventKind::JudgmentTurn,
    );
    assert_eq!(turn["waited_ms"].as_u64(), Some(90_000));
    assert_eq!(turn["since_ms"].as_u64(), Some(1_000));
    assert_eq!(turn["held"].as_bool(), Some(true));
    let without = read(
        turn_event(prediction, heavy, Duration::from_secs(9), false),
        EventKind::JudgmentTurn,
    );
    assert_eq!(without["held"].as_bool(), Some(false));
    let used = Used {
        tests_cpu_ms: 72 * MIN,
        run_cpu_ms: Some(180 * MIN),
        wall: Duration::from_secs(600),
    };
    let cpu = read(cpu_event(prediction, used, 18), EventKind::JudgmentCpu);
    assert_eq!(
        (
            cpu["tests_cpu_ms"].as_u64(),
            cpu["tests_machine_ms"].as_u64(),
            cpu["predicted_cpu_ms"].as_u64(),
            cpu["run_cpu_ms"].as_u64(),
        ),
        (
            Some(72 * MIN),
            Some(4 * MIN),
            Some(90 * MIN),
            Some(180 * MIN)
        ),
        "the tests' CPU stands beside the prediction of it, the whole tree's apart"
    );
    let unread = read(
        cpu_event(
            prediction,
            Used {
                run_cpu_ms: None,
                ..used
            },
            18,
        ),
        EventKind::JudgmentCpu,
    );
    assert!(unread["run_cpu_ms"].is_null());
}
