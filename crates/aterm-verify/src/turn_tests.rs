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

/// An actual source plan drives the ordinary scheduler and reporting path. Each callback writes
/// a create-new marker, so a repeated tree row fails rather than hiding behind a set/count.
#[cfg(unix)]
fn prequeue_stage(ctx: &crate::Ctx, spec: &crate::plan::StageSpec) -> crate::Report {
    use crate::plan::StageId;
    use std::io::Write as _;
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(ctx.root.join(format!("ran-{:?}", spec.id)))
        .unwrap();
    marker.write_all(b"completed").unwrap();
    let mut report = crate::Report::new(&spec.title);
    if spec.id == StageId::Census && ctx.root.join("red-census").exists() {
        report.fail("planted source census failure");
    } else if spec.id == StageId::ExportScan && ctx.root.join("unknown-export").exists() {
        report.cannot_run("source export unavailable");
    } else {
        if !matches!(
            spec.id,
            StageId::Census | StageId::ReleasedShim | StageId::Formatting | StageId::ExportScan
        ) {
            assert!(
                ctx.root.join("admitted").exists(),
                "compiler work preceded run admission"
            );
        }
        report.pass("the selected source row ran once");
    }
    report
}

#[cfg(unix)]
fn prequeue_context(root: &std::path::Path) -> crate::Ctx {
    std::fs::create_dir_all(root).unwrap();
    let history_path = root.join("history.json");
    let mut history = History::default();
    history.push(
        "-p fixture --lib",
        sample(Some(10 * 180_000 * u64::from(aterm_capacity::cores())), 1),
    );
    std::fs::write(&history_path, history.encode()).unwrap();
    crate::Ctx::new(
        root.to_owned(),
        Scope::crate_only("fixture"),
        crate::EnvSnapshot::default(),
        root.join("scratch"),
    )
    .with_checks(&crate::checks::DEV)
    .with_test_history(Some(history_path))
    .with_halt(crate::sched::Halt::after_preflight(|_| {}))
}

#[cfg(unix)]
fn prequeue_run(ctx: &crate::Ctx, admission: &crate::prequeue::Admission<'_>) -> (i32, String) {
    let seams = crate::Seams {
        stage: prequeue_stage,
        prepare_build_dir: |_| Ok(()),
        prepare_cells: None,
        admit_quiet: |_, _| panic!("no quiet claim in this source fixture"),
        admit_memory: |_, _| panic!("no stage memory claim in this source fixture"),
    };
    let plan = crate::RunPlan::capture(ctx, seams).unwrap();
    assert!(
        Prepared::capture(&plan, Some(1)).is_some(),
        "the actual captured history makes this run heavy"
    );
    let mut output = Vec::new();
    let code = crate::run_with_plan(
        ctx,
        &mut output,
        &mut std::io::sink(),
        &crate::Show::default(),
        seams,
        Some(plan),
        Some(admission),
    )
    .unwrap();
    (code, String::from_utf8(output).unwrap())
}

/// Census is genuinely red while another native process holds the stand-in ledger's turn. The
/// ordinary scheduler ends at that red; it never invokes turn/disk or a compiler callback.
#[cfg(unix)]
#[test]
fn a_red_tree_row_ends_a_heavy_run_before_the_judgment_turn() {
    use aterm_capacity::{Fixed, Ledger, Need, Polled, Reading, Waiter};
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let ctx = prequeue_context(root);
    std::fs::write(root.join("red-census"), b"red").unwrap();
    let ledger = Ledger::at(root.join("ledger"));
    let machine = Fixed::new(Reading::of_gib(48, 40, 18));
    let (input, keep_alive) = aterm_uds::pipe().unwrap();
    let mut child = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(input)
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let holder = Waiter::enter_stand_in(
        &ledger,
        &machine,
        Need::new("another verifier").heavy(1, 180_000),
        Some(child.id()),
        1,
    )
    .unwrap();
    let Polled::Admitted(held) = holder.poll(&machine) else {
        panic!("the private turn is available");
    };
    assert!(
        aterm_capacity::admit::try_admit_with(
            &ledger,
            &machine,
            Need::new("competitor").heavy(2, 180_000)
        )
        .unwrap()
        .is_none(),
        "the native other process actually holds this turn"
    );
    let acquire = |_: &crate::RunPlan<'_>, _: Duration| -> (Duration, Result<(), String>) {
        panic!("a red source row asked for turn/disk");
    };
    let admission = crate::prequeue::Admission::new(&acquire);
    let (code, output) = prequeue_run(&ctx, &admission);
    assert_eq!(code, crate::exit::FAILED, "{output}");
    assert!(output.contains("census"), "{output}");
    assert!(!root.join("admitted").exists());
    assert!(
        !root.join("ran-ReleasedShim").exists(),
        "census stops bootstrap too"
    );
    assert_eq!(
        ledger
            .list()
            .unwrap()
            .iter()
            .filter(|row| row.held.is_some())
            .count(),
        1
    );
    drop(held);
    drop(keep_alive);
    assert!(child.wait().unwrap().success());
}

/// All four real scheduler callbacks complete before a single actual private-ledger turn is
/// acquired; every compiler callback then observes that admission. No tree row is repeated.
#[cfg(unix)]
#[test]
fn a_heavy_run_takes_the_turn_only_after_its_tree_rows_pass() {
    use aterm_capacity::{Fixed, Ledger, Need, Reading};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let ctx = prequeue_context(root);
    let ledger = Ledger::at(root.join("ledger"));
    let machine = Fixed::new(Reading::of_gib(48, 40, 18));
    let calls = AtomicUsize::new(0);
    let held = Mutex::new(None);
    let acquire = |plan: &crate::RunPlan<'_>, _: Duration| {
        assert!(std::ptr::eq(plan.context(), &ctx));
        for id in [
            crate::plan::StageId::Census,
            crate::plan::StageId::ReleasedShim,
            crate::plan::StageId::Formatting,
            crate::plan::StageId::ExportScan,
        ] {
            assert!(
                root.join(format!("ran-{id:?}")).exists(),
                "{id:?} did not complete"
            );
        }
        assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
        *held.lock().unwrap() = Some(
            aterm_capacity::admit::try_admit_with(
                &ledger,
                &machine,
                Need::new("heavy source fixture").heavy(1, 180_000),
            )
            .unwrap()
            .expect("one actual turn"),
        );
        std::fs::write(root.join("admitted"), b"turn and run disk boundary").unwrap();
        (Duration::ZERO, Ok(()))
    };
    let admission = crate::prequeue::Admission::new(&acquire);
    let (code, output) = prequeue_run(&ctx, &admission);
    assert_eq!(code, crate::exit::PASS, "{output}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(root.join("ran-Tippy").exists());
    assert!(root.join("ran-TestCompile").exists());
    assert!(root.join("ran-Test").exists());
    assert_eq!(
        ledger
            .list()
            .unwrap()
            .iter()
            .filter(|row| row.held.is_some())
            .count(),
        1
    );
    drop(held.lock().unwrap().take());
    assert!(ledger.list().unwrap().is_empty());
}

/// Unanswered tree input is not a passing preflight. A resource refusal is shared once by
/// concurrent later stages, with no stage callback or admission retry.
#[cfg(unix)]
#[test]
fn unanswered_tree_and_refused_run_resources_start_no_compiler() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for unknown in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let ctx = prequeue_context(root);
        if unknown {
            std::fs::write(root.join("unknown-export"), b"unknown").unwrap();
        }
        let calls = AtomicUsize::new(0);
        let acquire = |_: &crate::RunPlan<'_>, _: Duration| {
            calls.fetch_add(1, Ordering::SeqCst);
            (Duration::ZERO, Err("planted disk admission refusal".into()))
        };
        let admission = crate::prequeue::Admission::new(&acquire);
        let (code, output) = prequeue_run(&ctx, &admission);
        assert_eq!(code, crate::exit::COULD_NOT_RUN, "{unknown}: {output}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(!unknown),
            "{output}"
        );
        assert!(!root.join("ran-Tippy").exists());
        assert!(!root.join("ran-TestCompile").exists());
        assert!(!root.join("ran-Test").exists());
    }
}

/// The history and queue rank are captured once, while the context remains borrowed. Neither a
/// later history write nor another invocation can substitute a new source for that preparation.
#[cfg(unix)]
#[test]
fn a_prepared_turn_keeps_its_prediction_and_refuses_another_context() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ctx = prequeue_context(dir.path());
    let plan = crate::RunPlan::capture(
        &ctx,
        crate::Seams {
            prepare_cells: None,
            ..crate::LIVE
        },
    )
    .unwrap();
    let prepared = Prepared::capture(&plan, Some(17)).unwrap();
    let prediction = prepared.prediction;
    std::fs::write(
        ctx.test_history.as_ref().unwrap(),
        History::default().encode(),
    )
    .unwrap();
    assert!(
        Prepared::capture(&plan, Some(18)).is_none(),
        "a fresh invocation sees the new light history"
    );
    assert_eq!(prepared.prediction, prediction);
    assert_eq!(prepared.heavy.since_ms, 17);
    let other = prequeue_context(&dir.path().join("other"));
    let other_plan = crate::RunPlan::capture(
        &other,
        crate::Seams {
            prepare_cells: None,
            ..crate::LIVE
        },
    )
    .unwrap();
    let (waited, refused) = prepared.take(&other_plan, aterm_capacity::Admit::Wait, Duration::ZERO);
    assert_eq!(waited, Duration::ZERO);
    assert!(
        refused.is_err(),
        "another context cannot ask using this prediction/source"
    );
}

/// A callback unwind leaves no returned admission outcome. Another worker must refuse rather
/// than reacquire resources whose original owner may already have changed before the panic.
#[cfg(unix)]
#[test]
fn a_panicked_run_admission_is_not_retried_by_another_worker() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = aterm_tempfile::tempdir().unwrap();
    let ctx = prequeue_context(dir.path());
    let plan = crate::RunPlan::capture(
        &ctx,
        crate::Seams {
            prepare_cells: None,
            ..crate::LIVE
        },
    )
    .unwrap();
    let calls = AtomicUsize::new(0);
    let acquire = |_: &crate::RunPlan<'_>, _: Duration| -> (Duration, Result<(), String>) {
        calls.fetch_add(1, Ordering::SeqCst);
        panic!("resource callback stopped before returning an outcome");
    };
    let admission = crate::prequeue::Admission::new(&acquire);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || admission.acquire(&plan, Duration::ZERO)
        ))
        .is_err()
    );
    let next = std::thread::scope(|scope| {
        scope
            .spawn(|| admission.acquire(&plan, Duration::ZERO))
            .join()
    });
    assert!(
        next.is_ok(),
        "another worker must not execute the panicking callback again"
    );
    assert!(next.unwrap().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
