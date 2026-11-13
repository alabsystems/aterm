// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn known_unprojected_metadata_does_not_claim_a_failure_or_lost_observations() {
    let records = [
        aterm_json::json!({"schema":1,"event":"invocation-started"}),
        aterm_json::json!({"schema":1,"event":"row-finished","stage":"test","row":"sample",
            "at_unix_ms":123,"seconds":1.0,"cpu_s":null,"cpu_scope":"sampled-child-process-groups",
            "cpu_coverage":"lower-bound","outcome":"FAIL"}),
        aterm_json::json!({"schema":1,"event":"test-cpu-accounted","binary_reads":1,"binary_unread":0,
            "observed_binary_cpu_s":1.0,"observed_stage_cpu_s":2.0,"other_observed_child_cpu_s":1.0,
            "reconciled":true,"scope":"sampled lower bounds"}),
    ];
    let text = records
        .iter()
        .map(|record| format!("verify-event: {record}\n"))
        .collect::<String>();
    let read = read_events(text.as_bytes()).unwrap();
    assert!(read.complete());
    assert_eq!(
        (read.count, read.unprojected_lines, read.rejected_lines),
        (3, 3, 0)
    );
    assert!(read.first_failure.is_none());

    for (record, malformed, unsupported, unavailable) in [
        ("{\"schema\":1,\"event\":\"future-kind\"}", 0, 1, 0),
        (
            "{\"schema\":1,\"event\":\"row-started\",\"event\":\"row-started\"}",
            1,
            0,
            0,
        ),
        ("{\"schema\":2,\"event\":\"row-started\"}", 1, 0, 0),
        ("{broken}", 1, 0, 0),
        (
            "{\"schema\":1,\"event\":\"observation-unavailable\",\"producer_event\":\"row-started\",\"reason\":\"record exceeds encoded line bound\"}",
            0,
            0,
            1,
        ),
    ] {
        let read = read_events(format!("{text}verify-event: {record}\n").as_bytes()).unwrap();
        assert!(!read.complete());
        assert_eq!(
            (
                read.malformed_lines,
                read.unsupported_lines,
                read.unavailable_observations
            ),
            (malformed, unsupported, unavailable)
        );
        assert!(read.first_failure.is_none());
    }
}

/// Caller-selected completed log DATA; no execution, source or qualification authority.
#[test]
#[ignore = "requires an explicit retained verifier log in ATERM_RELEASE_STATUS_LOG"]
fn retained_verifier_log_has_no_unrecognized_or_lost_records() {
    let path = std::env::var_os("ATERM_RELEASE_STATUS_LOG").expect("explicit retained log fixture");
    let file = crate::durable::open_regular_file(Path::new(&path)).unwrap();
    let read = read_events(file).unwrap();
    eprintln!(
        "retained-log parsing: events={} unprojected={} storage_markers={}",
        read.count, read.unprojected_lines, read.storage_markers
    );
    assert!(
        read.count != 0,
        "fixture contained no complete event observations"
    );
    assert!(
        read.complete(),
        "malformed={} unsupported={} unavailable={} partial={}",
        read.malformed_lines,
        read.unsupported_lines,
        read.unavailable_observations,
        read.incomplete_tail
    );
}

#[test]
fn supported_producer_progress_is_not_an_unreadable_log_or_a_failure() {
    let records = [
        aterm_json::json!({"schema":1,"event":"test-phase-started","check":"test",
            "test_run":1,"phase_sequence":1,"phase":"preparation","scope_exit":null,
            "seconds":null,"provisional":true,"sample_complete":false}),
        aterm_json::json!({"schema":1,"event":"test-phase-ended","check":"test",
            "test_run":1,"phase_sequence":1,"phase":"preparation","scope_exit":"advanced",
            "seconds":1.0,"provisional":true,"sample_complete":false}),
        aterm_json::json!({"schema":1,"event":"test-case-running-long","test":"gui",
            "case":"busy","outcome":"RUNNING","binary_completed":false,
            "sample_complete":false,"halts":false,"at_unix_ms":20000}),
        aterm_json::json!({"schema":1,"event":"compile-message-observed","stage":"test compile",
            "provisional":true,"sample_complete":false,"message_kind":"artifact",
            "compile_mode":null,"compile_kind":null,"target_name":"aterm_release",
            "target_kinds":["test"]}),
    ];
    let text = records
        .iter()
        .map(|record| format!("verify-event: {record}\n"))
        .collect::<String>();
    let observed = read_events(text.as_bytes()).unwrap();
    assert_eq!((observed.count, observed.rejected_lines), (4, 0));
    assert!(!observed.incomplete_tail);
    assert!(observed.first_failure.is_none());
    assert_eq!(observed.latest.as_ref(), records.last());

    // Each real producer kind must still use its own strict field decoder.
    for (index, from, to) in [
        (0, "\"phase_sequence\":1", "\"phase_sequence\":0"),
        (1, "\"scope_exit\":\"advanced\"", "\"scope_exit\":null"),
        (2, "\"halts\":false", "\"halts\":true"),
        (
            3,
            "\"target_kinds\":[\"test\"]",
            "\"target_kinds\":[\"invented\"]",
        ),
        (0, "\"schema\":1", "\"schema\":2"),
        (0, "\"schema\":1", "\"schema\":1,\"schema\":1"),
        (
            0,
            "\"phase_sequence\":1",
            "\"phase_sequence\":1,\"phase_sequence\":1",
        ),
        (0, "test-phase-started", "unrecognized-progress"),
    ] {
        let original = records[index].to_string();
        let invalid = original.replace(from, to);
        assert_ne!(invalid, original);
        let events = read_events(format!("verify-event: {invalid}\n").as_bytes()).unwrap();
        assert_eq!((events.count, events.rejected_lines), (0, 1), "{invalid}");
        assert!(events.latest.is_none());
        assert!(events.first_failure.is_none());
    }
    let partial = read_events(text.trim_end_matches('\n').as_bytes()).unwrap();
    assert_eq!((partial.count, partial.rejected_lines), (3, 0));
    assert!(partial.incomplete_tail);
}

#[test]
fn the_first_case_failure_gains_only_its_own_binary_completion() {
    let first = aterm_json::json!({"schema":1,"event":"test-case-failed",
        "outcome":"FAIL","test":"gui --lib","case":"broken",
        "elapsed_s":72,"binary_completed":false,"detail":"result pending"});
    let unrelated = aterm_json::json!({"schema":1,"event":"test-binary-finished",
        "test":"other --lib","outcome":"ok","exit_code":0});
    let complete = aterm_json::json!({"schema":1,"event":"test-binary-finished",
        "test":"gui --lib","outcome":"FAIL","exit_code":101,
        "execution_s":800,"failures":["broken"],"detail":"assertion failed"});
    let prefix = format!("verify-event: {first}\nverify-event: {unrelated}\n");
    let pending = read_events(prefix.as_bytes()).unwrap();
    assert_eq!(pending.first_failure, Some(first.clone()));
    let partial = read_events(format!("{prefix}verify-event: {complete}").as_bytes()).unwrap();
    assert_eq!(partial.first_failure, Some(first.clone()));
    assert!(partial.incomplete_tail);

    let later = aterm_json::json!({"schema":1,"event":"test-binary-finished",
        "test":"gui --lib","outcome":"ok","exit_code":0});
    let log = format!("{prefix}verify-event: {complete}\nverify-event: {later}\n");
    let events = read_events(log.as_bytes()).unwrap();
    let observed = events.first_failure.unwrap();
    assert_eq!(observed["case"], first["case"]);
    assert_eq!(observed["elapsed_s"], first["elapsed_s"]);
    assert_eq!(observed["outcome"], first["outcome"]);
    assert_eq!(observed["binary_completed"].as_bool(), Some(true));
    assert_eq!(observed["initial_detail"], first["detail"]);
    assert_eq!(observed["binary_result"], complete);
    assert!(!observed["detail"].as_str().unwrap().contains("pending"));
}

#[test]
fn cross_stream_failure_order_uses_observed_time_and_preserves_enrichment() {
    let gate = aterm_json::json!({"schema":1,"event":"commit-gate-failed",
        "outcome":"FAIL","gate":"notes","at_unix_s":10});
    let case = aterm_json::json!({"schema":1,"event":"test-case-failed",
        "outcome":"FAIL","test":"gui","case":"broken","at_unix_ms":11000,"binary_completed":false});
    let completion = aterm_json::json!({"schema":1,"event":"test-binary-finished",
        "outcome":"FAIL","test":"gui","exit_code":101,"detail":"assertion failed"});
    let release = read_events(format!("release-event: {gate}\n").as_bytes()).unwrap();
    let verify =
        read_events(format!("verify-event: {case}\nverify-event: {completion}\n").as_bytes())
            .unwrap();
    assert_eq!(release.first_failure.as_ref(), Some(&gate));
    let report = reported_failure(
        verify.first_failure.as_ref(),
        release.first_failure.as_ref(),
    );
    assert_eq!(report.first, gate);
    assert_eq!(report.ordering, "timestamp");
    assert!(report.candidates.is_empty());
    let later_gate =
        aterm_json::json!({"event":"commit-gate-failed","outcome":"FAIL","at_unix_s":12});
    let report = reported_failure(verify.first_failure.as_ref(), Some(&later_gate));
    assert_eq!(report.first["case"], case["case"]);
    assert_eq!(report.first["at_unix_ms"], case["at_unix_ms"]);
    assert_eq!(report.first["binary_result"], completion);
}

#[test]
fn cross_stream_unknown_times_retain_both_findings_without_a_first_claim() {
    let gate = aterm_json::json!({"event":"commit-gate-failed","outcome":"FAIL","at_unix_s":10});
    for time in [
        aterm_json::json!({"at_unix_ms":10500}),
        aterm_json::json!({}),
        aterm_json::json!({"at_unix_ms":"bad","at_unix_s":12}),
        aterm_json::json!({"at_unix_ms":12000,"at_unix_s":13}),
    ] {
        let mut case = time;
        let fields = case.as_object_mut().unwrap();
        fields.insert("event".into(), "test-case-failed".into());
        fields.insert("outcome".into(), "FAIL".into());
        fields.insert("detail".into(), "bad\u{7f}\u{9b}title".into());
        let report = reported_failure(Some(&case), Some(&gate));
        assert!(report.first.is_null());
        assert_eq!(report.candidates, vec![case, gate.clone()]);
        assert_eq!(report.ordering, "unknown");
        let text = text_status(&aterm_json::json!({
            "first_reported_failure": report.first,
            "first_reported_failure_ordering": report.ordering,
            "reported_failure_candidates": report.candidates,
        }));
        assert!(text.contains("order unknown"));
        assert!(!text.chars().any(|ch| ch == '\u{7f}' || ch == '\u{9b}'));
        assert!(!text.contains("first reported failure:"));
    }
    assert!(reported_failure(None, None).first.is_null());
    assert_eq!(reported_failure(None, Some(&gate)).first, gate);
}

#[test]
fn unfinished_stage_age_preserves_the_first_start_and_clock_uncertainty() {
    let mut events = Events::default();
    let start = |stamp| {
        aterm_json::json!({"event":"stage-started",
        "stage_index":3,"stage":"lint","started_at_unix_ms":stamp})
    };
    assert!(events.observe_stage(&start(1_000)));
    assert!(events.observe_stage(&start(2_000)));
    let stages = events.unfinished_stages_at(Some(4_500));
    assert_eq!(stages[0]["started_at_unix_ms"].as_u64(), Some(1_000));
    assert_eq!(stages[0]["elapsed_wall_ms"].as_u64(), Some(3_500));
    let status = aterm_json::json!({"unfinished_verification_stages":stages});
    assert!(text_status(&status).contains("lint (3 s since start, wall clock)"));
    assert!(events.unfinished_stages_at(Some(900))[0]["elapsed_wall_ms"].is_null());
    assert!(events.unfinished_stages_at(None)[0]["elapsed_wall_ms"].is_null());
    let malformed = aterm_json::json!({"event":"stage-started",
        "stage_index":4,"stage":"test","started_at_unix_ms":"unknown"});
    assert!(!events.observe_stage(&malformed));
    assert_eq!(events.open_stages.len(), 1);
}

#[test]
fn preflight_progress_and_failures_are_read_from_the_enclosing_step() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("05-bundle.log"), b"").unwrap();
    let start = aterm_json::json!({"schema":1,"event":"stage-started",
        "stage_index":1,"stage":"checker compile"});
    let failure = aterm_json::json!({"schema":1,"event":"test-case-failed",
        "outcome":"FAIL","test":"source closure"});
    for (phase, log) in [
        (crate::release::Step::Checks, "01-checks.log"),
        (crate::release::Step::Qualify, "04-qualify-tree.log"),
    ] {
        std::fs::write(
            dir.path().join(log),
            format!("verify-event: {start}\nverify-event: {failure}\n"),
        )
        .unwrap();
        let selected = check_log(dir.path(), Some(phase.name()));
        assert_eq!(selected, dir.path().join(log));
        let events = read_events(std::fs::File::open(selected).unwrap()).unwrap();
        assert_eq!(events.first_failure, Some(failure.clone()));
        assert_eq!(
            events.unfinished_stages(),
            aterm_json::json!([
                {"stage_index":1,"stage":"checker compile"}
            ])
        );
    }
    for phase in [
        None,
        Some("bundle"),
        Some("publish"),
        Some("check"),
        Some("../outside"),
    ] {
        assert_eq!(
            check_log(dir.path(), phase),
            dir.path().join("05-bundle.log")
        );
    }
}

#[test]
fn compiler_telemetry_keeps_the_current_release_stage_and_journal_binding() {
    let stage = aterm_json::json!({"schema":1,"event":"release-stage",
        "source":"current-source","version":"0.150.0","stage":"qualify","outcome":"started"});
    let child = aterm_json::json!({"schema":1,"event":"build-child-finished",
        "package":"aterm","outcome":"ok","execution_s":3});
    let log = format!("release-event: {stage}\nrelease-event: {child}\n");
    let events = read_events(log.as_bytes()).unwrap();
    assert_eq!(events.latest, Some(child));
    assert_eq!(events.latest_release_stage, Some(stage));
    let journal = crate::release::Journal {
        commit: "current-source".into(),
        version: "0.150.0".into(),
        ..Default::default()
    };
    let bound = assign_journal(Some(journal), events.latest_release_stage.as_ref(), true);
    assert_eq!(bound.binding, "current");
    assert!(bound.current.is_some());
    assert!(
        check_log(
            Path::new("logs"),
            events
                .latest_release_stage
                .as_ref()
                .and_then(|event| event["stage"].as_str())
        )
        .ends_with("04-qualify-tree.log")
    );
}

#[test]
fn concurrent_stages_are_paired_by_index_and_failure_does_not_hide_open_work() {
    let rows = [
        aterm_json::json!({"schema":1,"event":"stage-started","stage_index":1,"stage":"compile"}),
        aterm_json::json!({"schema":1,"event":"stage-started","stage_index":2,"stage":"compile"}),
        aterm_json::json!({"schema":1,"event":"stage-finished","stage_index":1,"stage":"compile",
            "decisions":[{"outcome":"FAIL"}]}),
    ];
    let text = rows
        .iter()
        .map(|row| format!("verify-event: {row}\n"))
        .collect::<String>();
    let events = read_events(text.as_bytes()).unwrap();
    assert_eq!(events.count, 3);
    assert!(events.first_failure.is_some());
    assert_eq!(
        events.unfinished_stages(),
        aterm_json::json!([
            {"stage_index":2,"stage":"compile"}
        ])
    );
    let status = aterm_json::json!({"state":"running","step":"verify",
        "unfinished_verification_stages":events.unfinished_stages()});
    assert!(text_status(&status).contains("unfinished verification stage: compile"));
    let done = format!(
        "{text}verify-event: {}\n",
        aterm_json::json!({
        "schema":1,"event":"stage-finished","stage_index":2,"stage":"compile"})
    );
    assert_eq!(
        read_events(done.as_bytes()).unwrap().unfinished_stages(),
        aterm_json::json!([])
    );
}

#[test]
fn stage_activity_is_unknown_for_old_logs_and_partial_finishes_leave_work_open() {
    let old = b"verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage\":\"old\"}\n";
    assert!(
        read_events(old.as_slice())
            .unwrap()
            .unfinished_stages()
            .is_null()
    );
    let partial = concat!(
        "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":3,\"stage\":\"lint\"}\n",
        "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":3"
    );
    let events = read_events(partial.as_bytes()).unwrap();
    assert!(events.incomplete_tail);
    assert_eq!(
        events.open_stages.get(&3).map(|stage| stage.title.as_str()),
        Some("lint")
    );
}

#[test]
fn stage_activity_rejects_rebinding_and_bounds_retained_work() {
    let mut events = Events::default();
    for index in 0..MAX_OPEN_STAGES {
        assert!(events.observe_stage(&aterm_json::json!({
            "event":"stage-started","stage_index":index,"stage":"build"})));
    }
    assert!(!events.observe_stage(&aterm_json::json!({
        "event":"stage-started","stage_index":MAX_OPEN_STAGES,"stage":"extra"})));
    assert!(!events.observe_stage(&aterm_json::json!({
        "event":"stage-finished","stage_index":0,"stage":"different"})));
    assert_eq!(events.open_stages.len(), MAX_OPEN_STAGES);
    assert!(events.observe_stage(&aterm_json::json!({
        "event":"stage-finished","stage_index":0,"stage":"build"})));
    assert!(events.observe_stage(&aterm_json::json!({
        "event":"stage-started","stage_index":MAX_OPEN_STAGES,"stage":"extra"})));
}

/// The build-inputs seal's counting lines (`build_inputs::current`, one per call, in every
/// mode it prints) are events of the run, never rejected lines, so a log that holds them
/// still reads as complete. CONTROL: an event of a name nothing prints is rejected.
/// NEGATIVE CONTROL: without `inputs` among the known events each is a rejected line.
#[test]
fn the_inputs_seal_lines_are_events_of_the_run() {
    let rows = ["hashed", "rechecked", "adopted"].map(
        |mode| aterm_json::json!({"schema":1,"event":"inputs","mode":mode,"entries":10,"secs":0.5}),
    );
    let lines = rows
        .iter()
        .map(|row| format!("release-event: {row}\n"))
        .collect::<String>();
    let events = read_events(lines.as_bytes()).unwrap();
    assert_eq!((events.rejected_lines, events.count), (0, 3));
    let unknown = "release-event: {\"schema\":1,\"event\":\"inputs-guessed\"}\n";
    assert_eq!(read_events(unknown.as_bytes()).unwrap().rejected_lines, 1);
}

/// The record an exec'd image writes, naming the preceding image's logs (`continued`, which
/// [`crate::release_timeline::Timeline`] follows), and the `handed` spelling a cutter already
/// running at the B1 boundary wrote are events of the run and no failure. NEGATIVE CONTROL:
/// without either among the known events that cut's observations become incomplete.
#[test]
fn a_continued_images_record_is_an_event_of_the_run() {
    for event in ["handed", "continued"] {
        let text = format!(
            "release-event: {}\n",
            aterm_json::json!({"schema": 1, "event": event, "from": "/state/logs/outer"})
        );
        let events = read_events(text.as_bytes()).unwrap();
        assert_eq!((events.rejected_lines, events.count), (0, 1), "{text}");
        assert!(events.first_failure.is_none());
    }
}

/// A frozen cutter's retirement measurement is a first-class release event, so the next
/// cutter can read the exact zero from its transcript. Changing its name makes the negative
/// control's otherwise identical line rejected.
#[test]
fn target_retirement_records_its_remaining_deps_count() {
    let line = |event| {
        format!(
            "release-event: {}\n",
            aterm_json::json!({"schema": 1, "event": event, "deps_entries": 0})
        )
    };
    let accepted = read_events(line("target-retired").as_bytes()).unwrap();
    assert_eq!((accepted.rejected_lines, accepted.count), (0, 1));
    let unknown = read_events(line("target-retire-mutant").as_bytes()).unwrap();
    assert_eq!((unknown.rejected_lines, unknown.count), (1, 0));
}

/// aterm-verify's per-unit compile evidence, a line for each unit of each compile
/// ([`aterm_verify::report::compile_unit_events`]: dispatched, finished, or a message whose
/// unit cannot be told), is an event of the run and no failure, so a verify that compiled
/// still reads as complete. NEGATIVE CONTROL: without the three among the known events each
/// is a rejected line and the run's observations are incomplete.
#[test]
fn a_compiles_unit_lines_are_events_of_the_run() {
    let unit = |reason: &str, identity: &str| {
        aterm_json::json!({
            "reason": reason, "schema": "targo.compilation-unit-dispatched.v1",
            "trust_unit_identity_sha256": identity, "trust_compile_mode": "test",
            "trust_compile_kind": "host", "fresh": false, "dirty_reason": "changed-file",
        })
        .to_string()
    };
    let (a, b) = ("a".repeat(64), "b".repeat(64));
    let two_units = unit("compilation-unit-dispatched", &a).replacen(
        &format!("\"trust_unit_identity_sha256\":\"{a}\""),
        &format!("\"trust_unit_identity_sha256\":\"{a}\",\"trust_unit_identity_sha256\":\"{b}\""),
        1,
    );
    let output = [
        unit("compilation-unit-dispatched", &a),
        unit("compiler-artifact", &a),
        two_units,
    ]
    .join("\n");
    let lines = aterm_verify::report::compile_unit_events("test", &output);
    let names: std::collections::BTreeSet<String> = lines
        .iter()
        .map(|line| {
            let json = line.strip_prefix("verify-event: ").unwrap();
            let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
            event["event"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        names,
        [
            "compile-unit-dispatched",
            "compile-unit-finished",
            "compile-unit-message-unavailable"
        ]
        .map(str::to_owned)
        .into(),
        "{lines:#?}"
    );
    let text = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let events = read_events(text.as_bytes()).unwrap();
    assert_eq!((events.rejected_lines, events.count), (0, 3), "{lines:#?}");
    assert!(events.first_failure.is_none());
}

/// A compile's timed units (`compile-unit-timed`, one per compiler its stage's reads dated)
/// are events of the run. NEGATIVE CONTROL: without them among the known events each is a
/// rejected line and the run's observations are incomplete.
#[test]
fn a_compiles_timed_units_are_events_of_the_run() {
    let span = aterm_verify::memory::UnitSpan {
        unit: aterm_verify::unitgraph::UnitName {
            package: "aterm-gui".to_owned(),
            kind: "aterm_gui test".to_owned(),
        },
        queued_at_unix_ms: Some(1),
        codegen_requested: Some(true),
        started_at_unix_ms: 2,
        ended_at_unix_ms: 3,
        cpu_s: 1.0,
        peak_bytes: 4,
    };
    let text: String = aterm_verify::compile_account::span_events("test", &[span.clone(), span])
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    let events = read_events(text.as_bytes()).unwrap();
    assert_eq!((events.rejected_lines, events.count), (0, 2), "{text}");
}

/// A check's `check-finished` line (A19's record source) is an event of the run, and a
/// failed one is a reported failure. NEGATIVE CONTROL: without `check-finished` among the
/// known events it is a rejected line and the run's observations are incomplete.
#[test]
fn a_check_finished_line_is_an_event_of_the_run() {
    let lines = "verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"fmt\",\"outcome\":\"ok\",\"seconds\":2.5}\n\
         verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"FAIL\",\"seconds\":9.0}\n";
    let events = read_events(lines.as_bytes()).unwrap();
    assert_eq!((events.rejected_lines, events.count), (0, 2));
    assert_eq!(
        events
            .first_failure
            .as_ref()
            .and_then(|e| e["check"].as_str()),
        Some("lint")
    );
}

#[test]
fn malformed_activity_never_hides_a_reported_failure_or_prints_control_titles() {
    let rows = [
        aterm_json::json!({"schema":1,"event":"stage-started","stage_index":4,"stage":"lint"}),
        aterm_json::json!({"schema":1,"event":"stage-finished","stage_index":4,
            "stage":"different","decisions":[{"outcome":"FAIL"}]}),
        aterm_json::json!({"schema":1,"event":"stage-started","stage_index":5,"stage":"bad\nname"}),
    ];
    let text = rows
        .iter()
        .map(|row| format!("verify-event: {row}\n"))
        .collect::<String>();
    let events = read_events(text.as_bytes()).unwrap();
    assert_eq!(events.count, 3);
    assert_eq!(events.rejected_lines, 2);
    assert!(events.first_failure.is_some());
    assert_eq!(
        events.open_stages.get(&4).map(|stage| stage.title.as_str()),
        Some("lint")
    );
    assert!(!events.open_stages.contains_key(&5));
}

#[test]
fn text_status_exposes_build_progress_before_verification_starts() {
    let event = aterm_json::json!({
        "event": "build-child-finished", "execution_s": 12,
        "child_cpu": {"observed_s": 9}, "outcome": "PASS",
    });
    let status = aterm_json::json!({
        "state": "running", "step": "build", "release_log": "/cut/transcript.log",
        "latest_release_event": event, "observations_complete": true,
    });
    let text = text_status(&status);
    assert!(text.contains("stage: build"));
    assert!(text.contains("release log: /cut/transcript.log"));
    assert!(text.contains(&event.to_string()));
    assert!(!text.contains("verification log:"));
    assert!(!text.contains("incomplete"));
}

#[test]
fn text_status_keeps_failure_visible_alongside_later_progress_and_partial_logs() {
    let failure = aterm_json::json!({"outcome": "FAIL", "case": "first"});
    let latest = aterm_json::json!({"outcome": "PASS", "case": "later"});
    let status = aterm_json::json!({
        "state": "running", "step": "verify",
        "first_reported_failure": failure, "latest_verification_event": latest,
        "observations_complete": false,
    });
    let text = text_status(&status);
    assert!(text.contains(&format!("first reported failure: {failure}")));
    assert!(text.contains(&format!("latest verification event: {latest}")));
    assert!(text.contains("observations: incomplete"));
}

#[test]
fn operational_record_is_bound_to_the_selected_attempt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("operational-status.json");
    let record = aterm_json::json!({"schema":1,"attempt":dir.path(),
        "observation":{"capability":{"state":"unknown","reason":"peer unavailable"}}});
    std::fs::write(&path, record.to_string()).unwrap();
    let read = operational_status(Some(dir.path()));
    assert_eq!(read, record);
    assert!(read["observation"]["capability"].get("value").is_none());
    let wrong = aterm_json::json!({"schema":1,"attempt":"another attempt","observation":{}});
    std::fs::write(&path, wrong.to_string()).unwrap();
    assert_eq!(
        operational_status(Some(dir.path()))["state"].as_str(),
        Some("unknown")
    );
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        operational_status(Some(dir.path()))["state"].as_str(),
        Some("unknown")
    );
}

#[test]
fn a_new_preflight_cannot_inherit_a_finished_releases_journal() {
    let old = crate::release::Journal {
        commit: "old-source".into(),
        version: "0.148.0".into(),
        done: vec!["check".into(), "tap-aligned".into()],
        ..Default::default()
    };
    let event = aterm_json::json!({"source":"?", "version":"?"});
    let journals = assign_journal(Some(old.clone()), Some(&event), true);
    assert_eq!(journals.binding, "unresolved");
    assert!(
        journals.current.is_none(),
        "no current completion or exception can be derived"
    );
    assert!(journals.previous.unwrap().finished());
    let event = aterm_json::json!({"source":"new-source", "version":"0.149.0"});
    let journals = assign_journal(Some(old.clone()), Some(&event), true);
    assert_eq!(journals.binding, "previous");
    assert!(journals.current.is_none());
    let event = aterm_json::json!({"source":"new-source", "version":"0.148.0"});
    assert!(
        assign_journal(Some(old.clone()), Some(&event), true)
            .current
            .is_none(),
        "matching the version alone does not bind an artifact"
    );
    let event = aterm_json::json!({"source":"old-source", "version":"0.148.0"});
    let journals = assign_journal(Some(old), Some(&event), true);
    assert_eq!(journals.binding, "current");
    assert!(journals.current.unwrap().finished());
    assert!(journals.previous.is_none());
}

#[test]
fn a_running_attempt_without_events_does_not_claim_an_old_journal() {
    let old = crate::release::Journal::default();
    assert!(
        assign_journal(Some(old.clone()), None, true)
            .current
            .is_none()
    );
    assert!(assign_journal(Some(old), None, false).current.is_some());
}

#[test]
fn status_is_a_read_only_verb_with_explicit_json_output() {
    let args = |items: &[&str]| {
        items
            .iter()
            .map(|item| (*item).to_string())
            .collect::<Vec<_>>()
    };
    assert!(matches!(
        crate::cli::parse(&args(&["release", "status", "--json"])),
        Ok(crate::cli::Cmd::ReleaseStatus {
            json: true,
            fleet: false
        })
    ));
    assert!(matches!(
        crate::cli::parse(&args(&["release", "status"])),
        Ok(crate::cli::Cmd::ReleaseStatus {
            json: false,
            fleet: false
        })
    ));
    assert!(matches!(
        crate::cli::parse(&args(&["release", "status", "--fleet"])),
        Ok(crate::cli::Cmd::ReleaseStatus {
            json: false,
            fleet: true
        })
    ));
    assert!(crate::cli::parse(&args(&["release", "status", "--fleet", "--bump"])).is_err());
    for extra in ["--bump", "--forget", "--prepare"] {
        assert!(crate::cli::parse(&args(&["release", "status", extra])).is_err());
    }
}

#[test]
fn failure_remains_visible_after_later_success_and_an_incomplete_write() {
    let bytes = concat!(
        "ordinary test output\n",
        "verify-event: {\"schema\":1,\"event\":\"test-binary-finished\",\"outcome\":\"FAIL\",\"test\":\"first\",\"waiting_s\":3,\"execution_s\":7}\n",
        "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"ok\"}]}\n",
        "verify-event: {\"schema\":1"
    );
    let events = read_events(bytes.as_bytes()).unwrap();
    assert_eq!(events.count, 2);
    assert!(events.incomplete_tail);
    let first = events.first_failure.unwrap();
    assert_eq!(first["test"].as_str(), Some("first"));
    assert_eq!(first["waiting_s"].as_u64(), Some(3));
    assert_eq!(first["execution_s"].as_u64(), Some(7));
    assert_eq!(
        events.latest.unwrap()["event"].as_str(),
        Some("stage-finished")
    );
}

#[test]
fn build_cpu_and_failure_survive_status_readback() {
    let events = read_events(b"release-event: {\"schema\":1,\"event\":\"build-child-finished\",\"outcome\":\"FAIL\",\"child_cpu\":{\"observed_s\":8},\"execution_s\":3}\n".as_slice()).unwrap();
    assert_eq!(events.count, 1);
    let failure = events.first_failure.unwrap();
    assert_eq!(failure["child_cpu"]["observed_s"].as_u64(), Some(8));
    assert_eq!(failure["execution_s"].as_u64(), Some(3));
}

#[test]
fn oversized_and_invalid_lines_do_not_hide_a_later_unavailable_stage() {
    let mut bytes = vec![b'x'; 128 * 1024];
    bytes.extend_from_slice(b"\nverify-event: not json\nverify-event: {\"schema\":1,\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"NOT RUN\"}]}\n");
    let events = read_events(bytes.as_slice()).unwrap();
    assert_eq!(events.rejected_lines, 2);
    assert_eq!(events.count, 1);
    assert!(events.first_failure.is_some());
}

#[test]
fn latest_run_pointer_is_bounded_and_trims_trailing_newline() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pointer = dir.path().join("latest-run");
    assert_eq!(read_latest_run(&pointer).unwrap(), None);

    let expected = dir.path().join("release-logs");
    std::fs::write(&pointer, format!("{}\n", expected.display())).unwrap();
    assert_eq!(read_latest_run(&pointer).unwrap(), Some(expected));

    std::fs::write(&pointer, "\n").unwrap();
    assert!(read_latest_run(&pointer).is_err());

    std::fs::write(&pointer, "relative/release-logs\n").unwrap();
    assert!(read_latest_run(&pointer).is_err());

    std::fs::write(&pointer, vec![b'a'; 8193]).unwrap();
    assert!(read_latest_run(&pointer).is_err());
}

/// ORPHANED CHILD HOLDER FALLS BACK TO `latest-run` INSTEAD OF CWD: a `running.child.<pid>`
/// note carries `logs: ""` and `step: Some(...)`, so `resolve_logs` must read `latest-run`
/// rather than `PathBuf::from("")`, while rejecting relative holder paths and symlinks.
/// NEGATIVE CONTROL: a parent holder with an absolute `logs` path resolves directly to it.
#[test]
fn orphaned_child_holder_falls_back_to_latest_run_and_refuses_relative_or_symlink_logs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pointer = dir.path().join("latest-run");
    let attempt = dir.path().join("attempt-1");
    let direct = dir.path().join("attempt-2");
    std::fs::create_dir_all(&attempt).unwrap();
    std::fs::write(&pointer, format!("{}\n", attempt.display())).unwrap();

    let child = crate::release::Holder {
        pid: 4242,
        started: "now".into(),
        logs: String::new(),
        step: Some("verify".into()),
        legacy_handed: false,
    };
    assert_eq!(
        resolve_logs(Some(&child), &pointer).unwrap(),
        Some(attempt.clone())
    );

    let parent = crate::release::Holder {
        logs: direct.display().to_string(),
        ..child.clone()
    };
    assert_eq!(resolve_logs(Some(&parent), &pointer).unwrap(), Some(direct));

    let relative = crate::release::Holder {
        logs: "relative/logs".into(),
        ..child
    };
    assert!(resolve_logs(Some(&relative), &pointer).is_err());

    #[cfg(unix)]
    {
        let target = dir.path().join("real-pointer");
        std::fs::write(&target, format!("{}\n", attempt.display())).unwrap();
        let link = dir.path().join("linked-pointer");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            read_latest_run(&link).is_err(),
            "symlinked latest-run must be refused"
        );

        let real_op = dir.path().join("real-operational.json");
        let record = aterm_json::json!({"schema":1,"attempt":attempt,"observation":{}});
        std::fs::write(&real_op, record.to_string()).unwrap();
        std::os::unix::fs::symlink(&real_op, attempt.join("operational-status.json")).unwrap();
        assert_eq!(
            operational_status(Some(&attempt))["state"].as_str(),
            Some("unknown"),
            "symlinked operational-status.json must not be trusted"
        );
    }
}

/// MALFORMED `release-stage` AND CONTROL CHARACTERS ARE REJECTED OR SCRUBBED: a `release-stage`
/// record with an empty or control-character `stage` increments `rejected_lines` without
/// overwriting `latest_release_stage`, while `text_status` scrubs `\x7f` and Unicode C1
/// controls (`\u{9b}`). NEGATIVE CONTROL: a clean `release-stage` updates `latest_release_stage`
/// with zero rejected lines.
#[test]
fn release_stage_validates_stage_name_and_text_status_scrubs_c1_controls() {
    let good = aterm_json::json!({
        "schema": 1,
        "event": "release-stage",
        "source": "abc",
        "version": "0.150.0",
        "stage": "verify",
    });
    let bad_ctrl = aterm_json::json!({
        "schema": 1,
        "event": "release-stage",
        "source": "abc",
        "version": "0.150.0",
        "stage": "build\u{9b}31m",
        "outcome": "FAIL",
    });
    let bad_empty = aterm_json::json!({
        "schema": 1,
        "event": "release-stage",
        "source": "abc",
        "version": "0.150.0",
        "stage": "",
    });
    let log =
        format!("release-event: {good}\nrelease-event: {bad_ctrl}\nrelease-event: {bad_empty}\n");
    let events = read_events(log.as_bytes()).unwrap();
    assert_eq!(events.count, 3);
    assert_eq!(events.rejected_lines, 2);
    assert_eq!(events.latest_release_stage, Some(good));
    assert!(
        events.first_failure.is_some(),
        "failure outcome on a rejected stage line remains visible"
    );

    let status = aterm_json::json!({
        "state": "running",
        "step": "verify",
        "first_reported_failure": {"case": "bad\x7f\u{9b}2Jtitle", "outcome": "FAIL"},
        "observations_complete": true,
    });
    let rendered = text_status(&status);
    assert!(
        !rendered.chars().any(|ch| ch != '\n' && ch.is_control()),
        "rendered text_status must contain no non-newline control chars: {rendered:?}"
    );
}
