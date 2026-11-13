// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn read_timeline(logs: &[(&str, &str)]) -> Timeline {
    let dir = aterm_tempfile::tempdir().unwrap();
    for (name, text) in logs {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    Timeline::of_logs(dir.path()).unwrap()
}

fn timeline_of(logs: &[(&str, &str)]) -> aterm_json::Value {
    read_timeline(logs).value()
}

#[test]
fn cut_gate_costs_survive_the_log_without_inflating_step_totals() {
    let gate = aterm_json::json!({
        "schema": 1, "event": "cut-gate-finished", "gate": "digest",
        "execution_s": 3.0, "self_cpu_s": 2.0,
        "child_cpu": {"observed_s": 1.0}, "inclusive": true
    });
    let log = format!(
        "{}{}{}release-event: {gate}\n",
        stage("publish", "started", 1000, 0, 0),
        stage("publish", "ok", 1003, 0, 3),
        child("publish", 9.0, 1024),
    );
    let value = timeline_of(&[("08-publish.log", &log)]);
    assert_eq!(value["cut_gates"][0], gate);
    assert_eq!(value["cut_gates"].as_array().unwrap().len(), 1);
    // A nested cost is diagnostic detail, not another child to charge.
    assert_eq!(value["steps"][0]["cpu_s"].as_f64(), Some(9.0));
    assert_eq!(value["steps"][0]["children"].as_u64(), Some(1));
}

fn stage(step: &str, outcome: &str, at: u64, waiting: u64, execution: u64) -> String {
    format!(
        "release-event: {}\n",
        aterm_json::json!({
            "schema": 1, "event": "release-stage", "stage": step, "outcome": outcome,
            "at_unix_s": at, "waiting_s": waiting, "execution_s": execution,
        })
    )
}

fn child(step: &str, cpu: f64, peak: u64) -> String {
    format!(
        "release-event: {}\n",
        aterm_json::json!({
            "schema": 1, "event": "step-child-finished", "step": step, "exit_code": 0,
            "started_at_unix_ms": 1, "ended_at_unix_ms": 2,
            "child_cpu": {"scope": "reaped-child-and-waited-descendants", "observed_s": cpu},
            "peak_bytes": peak,
        })
    )
}

fn attributed(event: aterm_json::Value, id: &str) -> aterm_json::Value {
    let mut event = event;
    event.as_object_mut().unwrap().insert(
        "invocation".into(),
        aterm_json::json!({
            "id":id, "identity_error":null, "source_observation":null,
        }),
    );
    event
}

fn attributed_line(line: &str, id: &str) -> String {
    let event = aterm_json::from_str(line.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    format!("verify-event: {}\n", attributed(event, id))
}

fn verify_start(id: &str, at: u64) -> aterm_json::Value {
    attributed(
        aterm_json::json!({"schema":1,"event":"stage-started",
        "stage_index":3,"stage":"test","started_at_unix_ms":at}),
        id,
    )
}

fn verify_finish(id: &str, at: u64) -> aterm_json::Value {
    attributed(
        aterm_json::json!({"schema":1,"event":"stage-finished",
        "stage_index":3,"stage":"test","ended_at_unix_ms":at,"execution_s":2.0,
        "child_cpu":{"observed_s":7.0},"decisions":[{"outcome":"FAIL","detail":"kept"}]}),
        id,
    )
}

#[test]
fn raw_ambiguous_attribution_cannot_supply_or_consume_a_stage_start() {
    for padding in [0, 9000] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let writer = aterm_verify::event_log::EventLog::open(&path).unwrap();
        let mut start = verify_start(&"a".repeat(32), 100);
        let mut finish = verify_finish(&"a".repeat(32), 400);
        finish.as_object_mut().unwrap().remove("ended_at_unix_ms");
        for value in [&mut start, &mut finish] {
            value
                .as_object_mut()
                .unwrap()
                .insert("pad".into(), "x".repeat(padding).into());
        }
        writer.write(&format!("verify-event: {start}"));
        writer.write(&format!("verify-event: {finish}"));
        let recorded = std::fs::read_to_string(&path).unwrap();
        let mut lines = recorded.lines();
        let start = format!("{}\n", lines.next().unwrap());
        let finish = format!("{}\n", lines.next().unwrap());
        assert!(lines.next().is_none());
        let (_, id) = aterm_verify::event_log::diagnostic_event(
            start.trim().strip_prefix("verify-event: ").unwrap(),
        )
        .unwrap();
        assert!(id.is_some());
        for (key, replacement) in [
            ("\"id\":", "\"id\":\"unknown\",\"id\":"),
            ("\"id\":", "\"\\u0069d\":\"unknown\",\"id\":"),
            ("\"invocation\":", "\"invocation\":null,\"invocation\":"),
            (
                "\"identity_error\":",
                "\"identity_error\":\"unavailable\",\"identity_error\":",
            ),
        ] {
            let bad_start = start.replacen(key, replacement, 1);
            let bad_finish = finish.replacen(key, replacement, 1);
            assert_ne!(bad_start, start);
            let text = format!("{start}{bad_start}{bad_finish}{finish}");
            let mut timeline = Timeline::default();
            timeline.read("06-verify.log", text.as_bytes()).unwrap();
            let value = timeline.value();
            let rows = value["branches"].as_array().unwrap();
            assert_eq!(rows.len(), 2);
            assert!(rows[0]["started_at_unix_ms"].is_null());
            assert!(rows[0]["ended_at_unix_ms"].is_null());
            assert_eq!(rows[1]["started_at_unix_ms"].as_u64(), Some(100));
            assert_eq!(rows[1]["ended_at_unix_ms"].as_u64(), Some(2100));
        }
        for (key, replacement) in [
            ("\"schema\":", "\"schema\":0,\"schema\":"),
            ("\"event\":", "\"\\u0065vent\":\"other\",\"event\":"),
        ] {
            let malformed = finish.replacen(key, replacement, 1);
            assert_ne!(malformed, finish);
            let text = format!("{start}{malformed}{finish}");
            let mut timeline = Timeline::default();
            timeline.read("06-verify.log", text.as_bytes()).unwrap();
            let value = timeline.value();
            let rows = value["branches"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["started_at_unix_ms"].as_u64(), Some(100));
        }
    }
}

#[test]
fn a_later_never_started_invocation_cannot_borrow_an_abandoned_start() {
    let a = "a".repeat(32);
    let b = "b".repeat(32);
    let text = [
        verify_start(&a, 100),
        verify_finish(&b, 200),
        verify_finish(&a, 300),
    ]
    .iter()
    .map(|event| format!("verify-event: {event}\n"))
    .collect::<String>();
    let mut timeline = Timeline::default();
    timeline.read("06-verify.log", text.as_bytes()).unwrap();
    let value = timeline.value();
    let rows = value["branches"].as_array().unwrap();
    assert!(rows[0]["started_at_unix_ms"].is_null());
    assert_eq!(rows[0]["ended_at_unix_ms"].as_u64(), Some(200));
    assert_eq!(rows[0]["cpu_s"].as_f64(), Some(7.0));
    assert_eq!(rows[0]["execution_s"].as_f64(), Some(2.0));
    assert_eq!(rows[1]["started_at_unix_ms"].as_u64(), Some(100));
    assert_eq!(value["first_red"]["ended_at_unix_ms"].as_u64(), Some(200));
    assert_eq!(
        value["first_red"]["decisions"][0]["detail"].as_str(),
        Some("kept")
    );
}

#[test]
fn stage_pairing_keeps_invocations_and_logs_independent() {
    let a = "a".repeat(32);
    let b = "b".repeat(32);
    let mut timeline = Timeline::default();
    timeline.observe("06-verify.log", &verify_start(&a, 100));
    timeline.observe("06-verify.log", &verify_start(&b, 200));
    timeline.observe("04-qualify-tree.log", &verify_start(&a, 300));
    timeline.observe("06-verify.log", &verify_finish(&a, 400));
    timeline.observe("04-qualify-tree.log", &verify_finish(&a, 500));
    timeline.observe("06-verify.log", &verify_finish(&b, 600));
    let value = timeline.value();
    let starts: Vec<_> = value["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["started_at_unix_ms"].as_u64())
        .collect();
    assert_eq!(starts, [Some(100), Some(300), Some(200)]);
}

#[test]
fn unsupported_attribution_cannot_supply_or_consume_a_stage_start() {
    let a = "a".repeat(32);
    for invalid in [
        aterm_json::Value::Null,
        aterm_json::json!({"id":"invalid","identity_error":null,"source_observation":null}),
        aterm_json::json!({"id":a,"identity_error":"unavailable","source_observation":null}),
        aterm_json::json!({"id":a,"identity_error":null,"source_observation":{"unknown":"context"}}),
    ] {
        let mut timeline = Timeline::default();
        timeline.observe("log", &verify_start(&a, 100));
        for mut event in [verify_start(&a, 200), verify_finish(&a, 300)] {
            if invalid.is_null() {
                event.as_object_mut().unwrap().remove("invocation");
            } else {
                event
                    .as_object_mut()
                    .unwrap()
                    .insert("invocation".into(), invalid.clone());
            }
            timeline.observe("log", &event);
        }
        timeline.observe("log", &verify_finish(&a, 400));
        let value = timeline.value();
        assert!(value["branches"][0]["started_at_unix_ms"].is_null());
        assert_eq!(
            value["branches"][1]["started_at_unix_ms"].as_u64(),
            Some(100)
        );
    }
}

#[test]
fn repeated_starts_and_consumed_stage_keys_never_choose_a_timestamp() {
    let a = "a".repeat(32);
    let mut timeline = Timeline::default();
    timeline.observe("log", &verify_start(&a, 100));
    timeline.observe("log", &verify_start(&a, 200));
    timeline.observe("log", &verify_finish(&a, 300));
    timeline.observe("log", &verify_start(&a, 400));
    timeline.observe("log", &verify_finish(&a, 500));
    let value = timeline.value();
    assert!(
        value["branches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["started_at_unix_ms"].is_null())
    );
    let mut out_of_order = Timeline::default();
    out_of_order.observe("log", &verify_finish(&a, 100));
    out_of_order.observe("log", &verify_start(&a, 200));
    out_of_order.observe("log", &verify_finish(&a, 300));
    assert!(
        out_of_order.value()["branches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["started_at_unix_ms"].is_null())
    );
}

#[test]
fn the_stage_key_bound_keeps_overflow_and_repeated_starts_unknown() {
    let a = "a".repeat(32);
    let mut timeline = Timeline::default();
    for index in 0..MAX_ROWS {
        let mut event = verify_start(&a, 100);
        event
            .as_object_mut()
            .unwrap()
            .insert("stage_index".into(), u64::try_from(index).unwrap().into());
        timeline.observe("log", &event);
    }
    // A repeated key at capacity must still lose its timestamp; overflow supplies none.
    timeline.observe("log", &verify_start(&a, 200));
    timeline.observe("log", &verify_finish(&a, 300));
    let mut overflow = verify_start(&a, 400);
    overflow.as_object_mut().unwrap().insert(
        "stage_index".into(),
        u64::try_from(MAX_ROWS).unwrap().into(),
    );
    timeline.observe("log", &overflow);
    let mut finished = verify_finish(&a, 500);
    finished.as_object_mut().unwrap().insert(
        "stage_index".into(),
        u64::try_from(MAX_ROWS).unwrap().into(),
    );
    timeline.observe("log", &finished);
    assert_eq!(timeline.stage_starts.len(), MAX_ROWS);
    assert!(
        timeline.value()["branches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["started_at_unix_ms"].is_null())
    );
}

/// Steps and their children's CPU add up, a verify stage's red dates the first red from the
/// run's start, and each record kind is counted where it was written.
#[test]
fn the_timeline_sums_steps_and_branches_and_dates_the_first_red() {
    let transcript = [
        stage("checks", "started", 1000, 0, 0),
        stage("checks", "ok", 1010, 0, 10),
        stage("build", "started", 1010, 0, 0),
        stage("build", "ok", 1110, 0, 100),
        stage("verify", "started", 1110, 0, 0),
        stage("verify", "FAIL", 1300, 20, 170),
    ]
    .concat();
    let build = [
        child("build", 30.0, 5 << 30),
        child("build", 10.5, 7 << 30),
        format!(
            "release-event: {}\n",
            aterm_json::json!({
                "schema": 1, "event": "build-child-finished", "package": "aterm",
                "target": null, "verb": "build", "outcome": "ok", "execution_s": 90.0,
                "child_cpu": {"observed_s": 400.0}, "peak_bytes": 3,
                "units": {"fresh": 10, "compiled": 5}, "output_bytes": 1000,
            })
        ),
        "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"hashed\",\"entries\":9,\"secs\":1.0}\n".to_owned(),
        "release-event: {\"schema\":1,\"event\":\"inputs\",\"mode\":\"rechecked\",\"entries\":9,\"secs\":0.1}\n".to_owned(),
    ]
    .concat();
    let verify = [
        "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":0,\"stage\":\"lint\",\"started_at_unix_ms\":1120000}\n",
        "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":0,\"stage\":\"lint\",\"waiting_s\":1.0,\"execution_s\":30.0,\"child_cpu\":{\"observed_s\":200.0},\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":\"x\"}]}\n",
        "verify-event: {\"schema\":1,\"event\":\"compile-finished\",\"units\":{\"fresh\":600,\"compiled\":2,\"unknown\":0},\"output_bytes\":24}\n",
        "verify-event: {\"schema\":1,\"event\":\"disk\",\"mode\":\"claimed\",\"walks\":3}\n",
    ]
    .into_iter().enumerate().map(|(at, line)| if at < 2 {
        attributed_line(line, &"a".repeat(32))
    } else { line.to_owned() }).collect::<String>();
    let value = timeline_of(&[
        ("transcript.log", &transcript),
        ("05-build.log", &build),
        ("06-verify.log", &verify),
    ]);
    assert_eq!(value["run_started_at_unix_s"].as_u64(), Some(1000));
    assert_eq!(value["critical_path_s"].as_f64(), Some(280.0), "{value}");
    let steps = value["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{value}");
    assert_eq!(steps[1]["step"].as_str(), Some("build"));
    assert_eq!(steps[1]["cpu_s"].as_f64(), Some(40.5));
    assert_eq!(steps[1]["peak_bytes"].as_u64(), Some(7 << 30));
    assert_eq!(steps[1]["children"].as_u64(), Some(2));
    assert!(
        steps[0]["cpu_s"].is_null(),
        "no record is null, never 0: {value}"
    );
    // lint started at 1120.000 s and ran 30 s: red at 1150 s, 150 s after checks began,
    // earlier than the verify step's own FAIL at 1300 s.
    assert_eq!(
        value["time_to_first_red_s"].as_f64(),
        Some(150.0),
        "{value}"
    );
    assert_eq!(value["first_red"]["stage"].as_str(), Some("lint"));
    let branches = value["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2, "{value}");
    assert_eq!(branches[0]["kind"].as_str(), Some("slice"));
    assert_eq!(branches[1]["ended_at_unix_ms"].as_u64(), Some(1_150_000));
    assert_eq!(value["full_hashes"].as_u64(), Some(1));
    assert_eq!(value["seal_rechecks"].as_u64(), Some(1));
    assert_eq!(value["disk_walks"].as_u64(), Some(3));
    assert_eq!(value["units"]["fresh"].as_u64(), Some(610));
    assert_eq!(value["units"]["compiled"].as_u64(), Some(7));
    assert_eq!(value["output_bytes"].as_u64(), Some(1024));
}

/// EACH QUALIFY BRANCH IS ITS OWN ROW, BOOKED ONLY ITS OWN CHILDREN: a branch's start, staging
/// push and end come from its `qualify-branch` events; it is booked the children that ended
/// in its log or a side log of it (the build's fixtures) while it ran, so a second round's
/// build in the same log is booked only its own; a branch's red dates the first red; and a
/// branch never started is a row with no start. NEGATIVE CONTROLS: booking a log's children
/// without the branch's window gives the second build the first one's CPU too, and booking
/// the branch's own log alone leaves the fixtures' CPU out of the first build's.
#[test]
fn each_qualify_branch_is_its_own_row_booked_only_its_own_children() {
    let branch = |b: &str, phase: &str, at: u64, secs: u64| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema": 1, "event": "qualify-branch", "branch": b,
                "log": format!("04-qualify-{b}.log"), "phase": phase,
                "at_unix_s": at, "seconds": secs})
        )
    };
    let child_at = |cpu: f64, ended_ms: u64| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema": 1, "event": "step-child-finished",
                "step": "qualify", "exit_code": 0, "ended_at_unix_ms": ended_ms,
                "child_cpu": {"observed_s": cpu}, "starved_s": cpu / 10.0,
                "peak_bytes": (cpu as u64) << 20})
        )
    };
    let transcript = [
        stage("checks", "started", 990, 0, 0),
        stage("checks", "ok", 1000, 0, 10),
        stage("qualify", "started", 1000, 0, 0),
        branch("tree", "started", 1000, 0),
        branch("build", "started", 1010, 0),
        branch("export", "started", 1011, 0),
        branch("export", "push-began", 1300, 0),
        branch("tree", "red", 1600, 600),
        branch("build", "stopped", 1601, 591),
        branch("export", "passed", 1700, 689),
        branch("build", "started", 2000, 0),
        branch("build", "passed", 2500, 500),
        stage("qualify", "FAIL", 2500, 0, 1500),
    ]
    .concat();
    let value = timeline_of(&[
        ("transcript.log", &transcript),
        ("04-qualify-tree.log", &child_at(100.0, 1_599_000)),
        (
            "04-qualify-build.log",
            &[child_at(300.0, 1_590_000), child_at(50.0, 2_400_000)].concat(),
        ),
        ("04-qualify-build-fixtures.log", &child_at(20.0, 1_595_000)),
        ("04-qualify-export.log", &child_at(5.0, 1_699_500)),
    ]);
    let rows = value["qualify"].as_array().expect("qualify rows");
    let row = |b: &str, n: usize| {
        rows.iter()
            .filter(|r| r["branch"].as_str() == Some(b))
            .nth(n)
            .unwrap_or_else(|| panic!("no row {n} of {b}: {value}"))
    };
    assert_eq!(rows.len(), 4, "{value}");
    assert_eq!(row("tree", 0)["outcome"].as_str(), Some("red"));
    assert_eq!(row("tree", 0)["cpu_s"].as_f64(), Some(100.0), "{value}");
    assert_eq!(row("build", 0)["outcome"].as_str(), Some("stopped"));
    assert_eq!(row("build", 0)["execution_s"].as_u64(), Some(591));
    assert_eq!(row("build", 0)["cpu_s"].as_f64(), Some(320.0), "{value}");
    assert_eq!(row("build", 0)["starved_s"].as_f64(), Some(32.0), "{value}");
    assert_eq!(row("build", 0)["peak_bytes"].as_u64(), Some(300 << 20));
    assert_eq!(row("build", 1)["cpu_s"].as_f64(), Some(50.0), "{value}");
    assert_eq!(row("build", 1)["starved_s"].as_f64(), Some(5.0), "{value}");
    assert_eq!(
        row("export", 0)["push_began_at_unix_s"].as_u64(),
        Some(1300)
    );
    assert_eq!(row("export", 0)["cpu_s"].as_f64(), Some(5.0));
    // The tree's red at 1600 s, 610 s after the run began.
    assert_eq!(
        value["time_to_first_red_s"].as_f64(),
        Some(610.0),
        "{value}"
    );
    assert_eq!(value["first_red"]["branch"].as_str(), Some("tree"));
    // Every qualify child is the step's: its sum is the graph's CPU.
    assert_eq!(value["steps"][1]["cpu_s"].as_f64(), Some(475.0), "{value}");

    let never = timeline_of(&[("transcript.log", &branch("export", "stopped", 1601, 0))]);
    let row = &never["qualify"][0];
    assert!(
        row["started_at_unix_s"].is_null() && row["execution_s"].is_null(),
        "{never}"
    );
    assert_eq!(row["outcome"].as_str(), Some("stopped"));
    assert!(
        row["cpu_s"].is_null(),
        "no record is null, never 0: {never}"
    );
}

fn qualify_event(branch: &str, phase: &str, log: Option<&str>, at: Option<u64>) -> String {
    format!(
        "release-event: {}\n",
        aterm_json::json!({
            "schema":1, "event":"qualify-branch", "branch":branch,
            "phase":phase, "log":log, "at_unix_s":at,
        })
    )
}

fn qualify_child(cpu: f64, at: Option<u64>) -> String {
    format!(
        "release-event: {}\n",
        aterm_json::json!({
            "schema":1, "event":"step-child-finished", "step":"qualify",
            "ended_at_unix_ms":at, "child_cpu":{"observed_s":cpu}, "peak_bytes":1234,
        })
    )
}

#[test]
fn same_second_rounds_leave_shared_children_unassigned_and_keep_unique_contributions() {
    let transcript = [
        qualify_event("build", "started", Some("04-build.log"), Some(1000)),
        qualify_event("build", "passed", Some("04-build.log"), Some(1010)),
        qualify_event("build", "started", Some("04-build.log"), Some(1010)),
        qualify_event("build", "passed", Some("04-build.log"), Some(1020)),
    ]
    .concat();
    let shared = qualify_child(17.0, Some(1_010_500));
    let ambiguous = read_timeline(&[("transcript.log", &transcript), ("04-build.log", &shared)]);
    assert_eq!(ambiguous.children.len(), 1);
    let ambiguous = ambiguous.value();
    for row in ambiguous["qualify"].as_array().unwrap() {
        assert!(row["cpu_s"].is_null());
        assert!(row["peak_bytes"].is_null());
        assert_eq!(row["children"].as_u64(), Some(0));
        assert_eq!(row["outcome"].as_str(), Some("passed"));
    }
    let children = [
        qualify_child(2.0, Some(1_009_500)),
        shared,
        qualify_child(3.0, Some(1_011_500)),
    ]
    .concat();
    let timeline = read_timeline(&[("transcript.log", &transcript), ("04-build.log", &children)]);
    assert_eq!(timeline.children.len(), 3);
    let value = timeline.value();
    let rows = value["qualify"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["cpu_s"].as_f64(), Some(2.0));
    assert_eq!(rows[1]["cpu_s"].as_f64(), Some(3.0));
    assert!(rows.iter().all(|row| row["children"].as_u64() == Some(1)));
}

#[test]
fn nested_log_stems_do_not_duplicate_children_and_prefix_neighbors_are_not_linked() {
    let transcript = [
        qualify_event("outer", "started", Some("04-build.log"), Some(1000)),
        qualify_event(
            "inner",
            "started",
            Some("04-build-fixtures.log"),
            Some(1000),
        ),
        qualify_event("outer", "passed", Some("04-build.log"), Some(1010)),
        qualify_event("inner", "passed", Some("04-build-fixtures.log"), Some(1010)),
    ]
    .concat();
    let own = qualify_child(2.0, Some(1_005_000));
    let shared = qualify_child(17.0, Some(1_005_000));
    let unrelated = qualify_child(99.0, Some(1_005_000));
    let timeline = read_timeline(&[
        ("transcript.log", &transcript),
        ("04-build.log", &own),
        ("04-build-fixtures.log", &shared),
        ("04-buildish.log", &unrelated),
    ]);
    assert_eq!(timeline.children.len(), 3);
    let value = timeline.value();
    let rows = value["qualify"].as_array().unwrap();
    assert_eq!(rows[0]["cpu_s"].as_f64(), Some(2.0));
    assert_eq!(rows[0]["children"].as_u64(), Some(1));
    assert!(rows[1]["cpu_s"].is_null());
    assert_eq!(rows[1]["children"].as_u64(), Some(0));
}

#[test]
fn uncertain_linked_windows_and_child_times_cannot_establish_unique_ownership() {
    for (start, end) in [
        (None, Some(1010)),
        (Some(1000), None),
        (Some(1012), Some(1000)),
        (Some(u64::MAX), Some(u64::MAX)),
        (Some(1000), Some(u64::MAX)),
    ] {
        let transcript = [
            qualify_event("known", "started", Some("04-build.log"), Some(1000)),
            qualify_event("unknown", "started", Some("04-build.log"), start),
            qualify_event("known", "passed", Some("04-build.log"), Some(1010)),
            qualify_event("unknown", "passed", Some("04-build.log"), end),
        ]
        .concat();
        let child = qualify_child(17.0, Some(1_005_000));
        let timeline = read_timeline(&[("transcript.log", &transcript), ("04-build.log", &child)]);
        assert_eq!(timeline.children.len(), 1);
        let value = timeline.value();
        assert!(
            value["qualify"].as_array().unwrap().iter().all(|row| {
                row["cpu_s"].is_null()
                    && row["peak_bytes"].is_null()
                    && row["children"].as_u64() == Some(0)
            }),
            "{value}"
        );
    }
    let transcript = [
        qualify_event("known", "started", Some("04-build.log"), Some(1000)),
        qualify_event("known", "passed", Some("04-build.log"), Some(1010)),
    ]
    .concat();
    let child = qualify_child(17.0, None);
    let timeline = read_timeline(&[("transcript.log", &transcript), ("04-build.log", &child)]);
    assert_eq!(timeline.children.len(), 1);
    let value = timeline.value();
    assert!(value["qualify"][0]["cpu_s"].is_null());
    assert_eq!(value["qualify"][0]["children"].as_u64(), Some(0));
}

/// A RELEASE COUNTS NO FULL HASH ONLY WHERE EVERY CUTTER OF IT LOGGED ITS OWN SEAL CALL: the
/// cutter's call in its transcript makes a mode no record names known, 0 (A2's "0 on a warm
/// machine"); a run's children's records alone (an older run, whose cutter's calls reached
/// its terminal only) leave it null; a continued image counts the preceding image's seal
/// calls and walks with its own, and is null where those cannot be read or that image's own
/// call reached no log. NEGATIVE CONTROLS: making any call's record know both counts reads 0
/// for the older run, and reading the continued image's logs alone reads 0 for a release that
/// hashed once.
#[test]
fn a_release_counts_no_full_hash_only_where_every_cutter_logged_its_own_seal_call() {
    let call = |mode: &str| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema":1,"event":"inputs","mode":mode,"entries":9,"secs":0.5})
        )
    };
    let continued = |event: &str, from: &Path| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema":1,"event":event,"from":from.display().to_string()})
        )
    };
    let counts = |value: &aterm_json::Value| {
        (
            value["full_hashes"].as_u64(),
            value["seal_rechecks"].as_u64(),
            value["disk_walks"].as_u64(),
        )
    };
    let write = |logs: &[(&str, String)]| {
        let dir = aterm_tempfile::tempdir().unwrap();
        for (name, text) in logs {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    };
    let of = |dir: &Path| Timeline::of_logs(dir).unwrap().value();

    let warm = write(&[
        (TRANSCRIPT, call("adopted")),
        ("05-build.log", call("rechecked")),
    ]);
    assert_eq!(counts(&of(warm.path())), (Some(0), Some(2), None));

    let older = write(&[(
        "05-build.log",
        [call("adopted"), call("rechecked")].concat(),
    )]);
    assert_eq!(counts(&of(older.path())), (None, Some(2), None));

    // A bump release: the outer image hashed at step 1, then exec'd R's own cutter.
    let outer = write(&[
        (TRANSCRIPT, call("hashed")),
        (
            "01-checks.log",
            "verify-event: {\"schema\":1,\"event\":\"disk\",\"mode\":\"claimed\",\"walks\":1}\n"
                .to_owned(),
        ),
    ]);
    let rerun = write(&[
        (
            TRANSCRIPT,
            [continued("handed", outer.path()), call("adopted")].concat(),
        ),
        ("06-verify.log", call("rechecked")),
    ]);
    assert_eq!(counts(&of(rerun.path())), (Some(1), Some(2), Some(1)));

    let gone = outer.path().join("gone");
    let lost = write(&[(
        TRANSCRIPT,
        [continued("continued", &gone), call("adopted")].concat(),
    )]);
    assert_eq!(counts(&of(lost.path())), (None, Some(1), None));

    let unlogged = write(&[("01-checks.log", call("adopted"))]);
    let after_unlogged = write(&[(
        TRANSCRIPT,
        [continued("continued", unlogged.path()), call("adopted")].concat(),
    )]);
    assert_eq!(counts(&of(after_unlogged.path())), (None, Some(2), None));

    let none = write(&[(TRANSCRIPT, String::new())]);
    assert_eq!(counts(&of(none.path())), (None, None, None));
}

/// A failed test case dates the red when libtest reported it, before its stage ends; a stage
/// that states when it ended is dated by that. A case its block spared (NOT RUN: refused, out
/// of disk), said earlier, dates nothing.
#[test]
fn a_failed_case_dates_the_red_before_its_stage_ends() {
    let transcript = [
        stage("verify", "started", 2000, 0, 0),
        stage("verify", "FAIL", 2600, 0, 600),
    ]
    .concat();
    let verify = [
        "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":3,\"stage\":\"test\",\"started_at_unix_ms\":2001000}\n",
        "verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"NOT RUN\",\"test\":\"-p x --lib\",\"case\":\"spared\",\"at_unix_ms\":2010000}\n",
        "verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"FAIL\",\"test\":\"-p x --lib\",\"case\":\"t\",\"at_unix_ms\":2042000}\n",
        "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":3,\"stage\":\"test\",\"ended_at_unix_ms\":2500000,\"execution_s\":10.0,\"decisions\":[{\"outcome\":\"FAIL\",\"detail\":\"x\"}]}\n",
    ]
    .concat();
    let value = timeline_of(&[("transcript.log", &transcript), ("06-verify.log", &verify)]);
    assert_eq!(value["time_to_first_red_s"].as_f64(), Some(42.0), "{value}");
    assert_eq!(value["first_red"]["case"].as_str(), Some("t"));
    assert_eq!(
        value["branches"][0]["ended_at_unix_ms"].as_u64(),
        Some(2_500_000)
    );
}

/// An older run's logs, with none of the newer records, answer null for what they never
/// said rather than counting it as nothing.
#[test]
fn records_a_run_never_wrote_are_null_not_zero() {
    let value = timeline_of(&[(
        "transcript.log",
        &[
            stage("checks", "started", 5, 0, 0),
            stage("checks", "ok", 6, 0, 1),
        ]
        .concat(),
    )]);
    for field in [
        "time_to_first_red_s",
        "full_hashes",
        "disk_walks",
        "units",
        "output_bytes",
        "publish_phases",
    ] {
        assert!(value[field].is_null(), "{field}: {value}");
    }
    assert_eq!(value["critical_path_s"].as_f64(), Some(1.0));
}

/// The publish step splits at the engine's own section headers; the span before the first
/// is promote, whose sections the engine replays only after it ends.
#[test]
fn publish_phases_split_at_the_engines_section_headers() {
    let transcript = [
        stage("publish", "started", 100, 0, 0),
        stage("publish", "ok", 200, 0, 100),
    ]
    .concat();
    let line = |at: u64, text: &str| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema":1,"event":"publish-line","at_unix_ms":at,"line":text})
        )
    };
    let publish = [
        line(110_000, "  delegating to pub promote"),
        line(160_000, "== sign-release aterm v1"),
        line(170_000, "uploading x"),
        line(190_000, "== done"),
        "release-event: {\"schema\":1,\"event\":\"step-child-finished\",\"step\":\"publish\",\"exit_code\":0,\"started_at_unix_ms\":100000,\"ended_at_unix_ms\":195000}\n".to_owned(),
    ]
    .concat();
    let value = timeline_of(&[
        ("transcript.log", &transcript),
        ("08-publish-arrivals.log", &publish),
    ]);
    let phases = value["publish_phases"].as_array().unwrap();
    assert_eq!(phases.len(), 3, "{value}");
    assert_eq!(phases[0]["seconds"].as_f64(), Some(60.0));
    assert_eq!(
        phases[1]["phase"].as_str(),
        Some("== sign-release aterm v1")
    );
    assert_eq!(phases[1]["seconds"].as_f64(), Some(30.0));
    assert_eq!(phases[2]["seconds"].as_f64(), Some(5.0));
}

/// A step child's record is never a red: a retried child that exited non-zero is not the
/// run's first failure.
#[test]
fn a_step_childs_exit_is_not_a_red() {
    let value = timeline_of(&[
        (
            "transcript.log",
            &[
                stage("commit", "started", 1, 0, 0),
                stage("commit", "ok", 2, 0, 1),
            ]
            .concat(),
        ),
        (
            "03-commit.log",
            "release-event: {\"schema\":1,\"event\":\"step-child-finished\",\"step\":\"commit\",\"exit_code\":1}\n",
        ),
    ]);
    assert!(value["time_to_first_red_s"].is_null(), "{value}");
    assert_eq!(value["steps"][0]["children"].as_u64(), Some(1));
}
