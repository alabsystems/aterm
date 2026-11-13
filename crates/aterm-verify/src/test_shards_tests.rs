// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::test_history::{History, Sample};
use crate::testrun::{Record, Watch, binary_cmd};

fn sample(wall_ms: u64, prefix: Option<&str>) -> Sample {
    Sample {
        at_ms: 1,
        outcome: "ok".into(),
        wall_ms,
        cpu_ms: Some(wall_ms),
        starved_ms: Some(0),
        peak_bytes: Some(1),
        width: 4,
        cores: None,
        tests: Some(1000),
        partition: prefix.map(str::to_owned),
        exe_sha256: None,
    }
}

fn binary() -> Binary {
    Binary {
        header: String::new(),
        spec: "-p aterm-verify --lib".into(),
        harness: true,
        record: Record {
            argv: vec![std::env::current_exe().unwrap().into_os_string()],
            cwd: std::env::current_dir().unwrap(),
            env: Vec::new(),
        },
    }
}

#[test]
fn measured_case_time_selects_the_boundary_instead_of_case_population() {
    let names = [
        "a::one",
        "a::two",
        "a::three",
        "a::four",
        "a::five",
        "a::six",
        "b::slow::one",
        "b::two",
        "b::three",
        "b::four",
    ];
    let timed = |slow| {
        names
            .iter()
            .map(|name| crate::exec::CaseTiming {
                case: (*name).into(),
                execution: if *name == "b::slow::one" {
                    slow
                } else {
                    Duration::from_millis(1)
                },
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        learn(&timed(Duration::from_secs(1)), LONG).as_deref(),
        Some("b::slow::")
    );
    // Negative control: without the planted expensive case the population wins.
    assert_eq!(
        learn(&timed(Duration::from_millis(1)), LONG).as_deref(),
        Some("a::")
    );
    assert_eq!(
        learn(&timed(Duration::from_secs(1)), Duration::from_secs(119)),
        None
    );
    assert_eq!(learn(&[], LONG), None);
    let flat = [
        ("same::one", 0.001),
        ("same::two", 0.001),
        ("same::three", 1.0),
    ]
    .into_iter()
    .map(|(name, seconds)| crate::exec::CaseTiming {
        case: name.into(),
        execution: Duration::from_secs_f64(seconds),
    })
    .collect::<Vec<_>>();
    assert_eq!(learn(&flat, LONG).as_deref(), Some("same::three"));
}

#[test]
fn long_observed_partitions_expand_without_starting_an_executable() {
    let mut binary = binary();
    binary.record.argv[0] = "this-executable-must-never-start".into();
    let history = |sample| {
        let mut history = History::default();
        history.push(&binary.spec, sample);
        History::decode(&history.encode())
    };
    let long = history(sample(180_000, Some("libtest_fixture::")));
    let halves = expand(vec![binary.clone()], &long, None);
    assert_eq!(halves.len(), 2);
    assert_eq!(
        seen(&long, &halves[0].spec).unwrap().wall,
        Duration::from_secs(90)
    );
    let evidence = crate::test_content::Evidence {
        exe_sha256: Some("ab".repeat(32)),
        content_key: Some("cd".repeat(32)),
        reason: None,
    };
    let prior_spec = format!("{} -- --skip prior::", binary.spec);
    let mut passed = Passed::new();
    passed.insert(
        prior_spec.clone(),
        crate::test_content::CarryProof::passed(&prior_spec, &evidence, 1.0),
    );
    assert!(
        expand(vec![binary.clone()], &long, Some(&passed))[0]
            .spec
            .ends_with(" -- prior::")
    );
    assert_eq!(
        expand(
            vec![binary.clone()],
            &history(sample(119_999, Some("libtest_fixture::"))),
            None
        ),
        [binary.clone()]
    );
    assert_eq!(
        expand(vec![binary.clone()], &history(sample(180_000, None)), None),
        [binary.clone()]
    );
    for invalid in ["--not-a-filter", "bad filter"] {
        assert_eq!(
            expand(
                vec![binary.clone()],
                &history(sample(180_000, Some(invalid))),
                None
            ),
            [binary.clone()]
        );
    }
    let oversized = "x".repeat(crate::report::EVENT_ROW_BYTES + 1);
    assert_eq!(
        expand(
            vec![binary.clone()],
            &history(sample(180_000, Some(&oversized))),
            None
        ),
        [binary.clone()]
    );
    let full_filter = format!("{}::", "x".repeat(crate::report::EVENT_ROW_BYTES - 2));
    assert_eq!(
        expand(
            vec![binary.clone()],
            &history(sample(180_000, Some(&full_filter))),
            None
        ),
        [binary.clone()]
    );
    let mut short = long.clone();
    for half in &halves {
        short.push(&half.spec, sample(40_000, None));
    }
    assert_eq!(expand(vec![binary.clone()], &short, None), [binary.clone()]);
    let mut filtered = binary.clone();
    filtered.record.argv.push("libtest_fixture::".into());
    assert_eq!(expand(vec![filtered.clone()], &long, None), [filtered]);
    let mut custom = binary;
    custom.harness = false;
    assert_eq!(expand(vec![custom.clone()], &long, None), [custom]);
}

/// The real run entry must dispatch both planned invocations. Removing its expansion hook
/// produces one finished event instead of two, while the unobserved control stays one.
#[cfg(unix)]
#[test]
fn the_run_entry_dispatches_the_recorded_partition() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let echo = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/echo"));
    let run = |partition: Option<&str>| {
        let mut binary = binary();
        binary.record.argv[0] = echo.clone().into_os_string();
        let events = tmp.path().join(if partition.is_some() {
            "split.events"
        } else {
            "whole.events"
        });
        let ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            tmp.path().to_path_buf(),
        )
        .with_child_ceiling(Some(Duration::from_secs(60)))
        .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
        let mut planned = crate::testrun::RunPlan::fixture(vec![binary.clone()], 8);
        planned
            .history
            .push(&binary.spec, sample(180_000, partition));
        let mut report = crate::ladder::Report::new("partition fixture");
        crate::testrun::run_planned(
            &ctx,
            &mut report,
            "test",
            planned,
            crate::checks::Check::Test,
        );
        assert!(
            report
                .outcomes()
                .all(|(outcome, _)| outcome == crate::ladder::Outcome::Ok),
            "{}",
            report.render()
        );
        std::fs::read_to_string(events)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
            .filter(|event| event["event"].as_str() == Some("test-binary-finished"))
            .count()
    };
    assert_eq!(run(Some("fixture::")), 2);
    assert_eq!(run(None), 1);
}

/// Exercise generated argv against libtest. Mutating --skip into the positive filter makes
/// the halves overlap and omits the other modules, failing both assertions below.
#[test]
fn the_real_libtest_partition_covers_each_case_once() {
    let binary = binary();
    let list = |binary: &Binary| -> BTreeSet<String> {
        let mut command = aterm_uds::spawnfd::command(binary.record.exe());
        command
            .args(binary.record.argv.iter().skip(1))
            .args(["--list", "--format=terse"]);
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
            .collect()
    };
    let all = list(&binary);
    let [selected, rest] = split(&binary, "libtest_fixture::");
    let selected = list(&selected);
    let rest = list(&rest);
    assert!(!selected.is_empty() && !rest.is_empty());
    assert!(selected.is_disjoint(&rest));
    assert_eq!(selected.union(&rest).cloned().collect::<BTreeSet<_>>(), all);
}

#[test]
fn the_real_json_command_reports_completed_case_times() {
    let mut binary = binary();
    binary.record.argv.push("libtest_fixture::b_pass".into());
    let tmp = aterm_tempfile::tempdir().unwrap();
    let run = |json| {
        let recorded = binary_cmd(
            &binary,
            1,
            1,
            false,
            Watch {
                json,
                ..Default::default()
            },
        );
        let mut command = aterm_uds::spawnfd::command(&recorded.program);
        command.args(&recorded.args);
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(output.status.success());
        let path = tmp
            .path()
            .join(if json { "json.events" } else { "human.events" });
        std::fs::write(&path, &output.stdout).unwrap();
        crate::libtest_json::slow_cases(&crate::exec::case_observations(&path, json).timings)
    };
    assert_eq!(
        run(true)[0]["case"].as_str(),
        Some("libtest_fixture::b_pass")
    );
    assert!(run(false).is_empty());
}

/// The production path renders JSON before reporting it. Timings must survive that
/// conversion; reading them from the rendered text instead makes this control fail.
#[test]
fn the_run_entry_reports_case_times_after_human_rendering() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let events = tmp.path().join("timed.events");
    let ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().to_path_buf(),
    )
    .with_child_ceiling(Some(Duration::from_secs(60)))
    .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
    let mut binary = binary();
    binary.record.argv.push("libtest_fixture::b_pass".into());
    let planned = crate::testrun::RunPlan::fixture(vec![binary], 8);
    let mut report = crate::ladder::Report::new("timing fixture");
    crate::testrun::run_planned(
        &ctx,
        &mut report,
        "test",
        planned,
        crate::checks::Check::Test,
    );
    assert!(
        report
            .outcomes()
            .all(|(outcome, _)| outcome == crate::ladder::Outcome::Ok),
        "{}",
        report.render()
    );
    let completed = std::fs::read_to_string(events)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .find(|event| event["event"].as_str() == Some("test-binary-finished"))
        .unwrap();
    assert_eq!(
        completed["slow_cases"][0]["case"].as_str(),
        Some("libtest_fixture::b_pass")
    );
    assert_eq!(completed["case_timings_observed"].as_u64(), Some(1));
    assert_eq!(completed["case_timings_complete"].as_bool(), Some(true));
}
