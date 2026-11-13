// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Check, Outcome};

fn compiler_wire(stage: &str, artifact: bool) -> String {
    let fixture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let line = fixture.lines().nth(usize::from(artifact)).unwrap();
    crate::report::provisional_compile_event(stage, line).unwrap()
}

#[test]
fn compiler_progress_is_latest_file_order_metadata_independent_of_terminal_verdicts() {
    let dispatch = compiler_wire("test compile", false);
    let artifact = compiler_wire("test compile", true);
    let before = observe(&format!("{dispatch}\n"));
    assert_eq!(
        before.json()["compiler_progress"][0]["latest"]["message_kind"].as_str(),
        Some("dispatch")
    );
    let text = format!("{dispatch}\n{artifact}\n{artifact}\n");
    let observed = observe(&text);
    assert_eq!(observed.compiler_message_records, 3);
    assert_eq!(observed.compiler_progress[0].observed_message_records, 3);
    assert_eq!(
        observed.json()["compiler_progress"][0]["latest"]["message_kind"].as_str(),
        Some("artifact")
    );
    assert!(observed.checks.is_empty());
    assert_eq!(observed.terminal_records, 0);
    let terminal = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let completed = observe(&format!("{text}{terminal}"));
    let baseline = observe(&terminal);
    assert_eq!(completed.compiler_progress, observed.compiler_progress);
    assert_eq!(completed.checks, baseline.checks);
    assert_eq!(completed.early_cases, baseline.early_cases);
    assert_eq!(
        completed.terminal_consistency,
        baseline.terminal_consistency
    );
    assert_eq!(
        completed.json()["compiler_progress_complete"].as_bool(),
        Some(false)
    );
    assert!(
        completed
            .render()
            .contains("last supported record in supplied file order")
    );
}

#[test]
fn compiler_progress_keeps_exact_invocations_and_unattributed_records_separate() {
    let first = writer(None);
    let second = writer(None);
    let dispatch = compiler_wire("compile", false);
    let artifact = compiler_wire("compile", true);
    let observed = observe(&format!(
        "{}{}{dispatch}\n",
        first.prepare(&dispatch),
        second.prepare(&artifact)
    ));
    assert_eq!(observed.invocations.len(), 2);
    for group in &observed.invocations {
        assert_eq!(group.observed.compiler_message_records, 1);
        let expected = if Some(group.id.as_str()) == first.id() {
            "dispatch"
        } else {
            "artifact"
        };
        assert_eq!(
            group.observed.json()["compiler_progress"][0]["latest"]["message_kind"].as_str(),
            Some(expected)
        );
    }
    assert_eq!(
        observed
            .unattributed
            .as_ref()
            .unwrap()
            .compiler_message_records,
        1
    );
    let mut conflict: aterm_json::Value = aterm_json::from_str(
        second
            .prepare(&artifact)
            .trim()
            .strip_prefix("verify-event: ")
            .unwrap(),
    )
    .unwrap();
    let binding = conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap();
    binding.insert("id".into(), first.id().unwrap().into());
    binding.insert(
        "source_observation".into(),
        aterm_json::json!({
            "observed_head":null,"selection":"different","selection_truncated":false,"checks":[],
            "checks_omitted":0,"release":false,"fix":false,"no_windows":false,"compile_only":false,
        }),
    );
    let conflicting = observe(&format!(
        "{}verify-event: {conflict}\n",
        first.prepare(&dispatch)
    ));
    assert!(conflicting.invocations[0].source_conflict);
    assert!(conflicting.invocations[0].source_observation.is_none());
    assert_eq!(
        conflicting.invocations[0].observed.compiler_message_records,
        2
    );
}

#[test]
fn unsupported_compiler_metadata_is_unprojected_and_cannot_change_finished_checks() {
    let wire = compiler_wire("compile", false);
    let complete = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let baseline = observe(&complete);
    for unsupported in [
        wire.replace("\"provisional\":true", "\"provisional\":false"),
        wire.replace("\"sample_complete\":false", "\"sample_complete\":true"),
        wire.replace(
            "\"provisional\":true",
            "\"provisional\":false,\"provisional\":true",
        ),
        wire.replace(
            "\"stage\":\"compile\"",
            "\"stage\":\"other\",\"stage\":\"compile\"",
        ),
        wire.replace(
            "\"compile_mode\":\"build\"",
            "\"compile_mode\":\"arbitrary\"",
        ),
        wire.replace("\"target_name\":null", "\"target_name\":\"private\""),
        compiler_wire("compile", true).replace(
            "\"target_kinds\":[\"lib\"]",
            "\"target_kinds\":[\"unknown\"]",
        ),
        compiler_wire("compile", true).replace("dispatch_serialization_probe", "/private/target"),
        wire.replace(
            "\"stage\":\"compile\"",
            &format!(
                "\"stage\":\"{}\"",
                "x".repeat(crate::compiler_observation::STAGE_BYTES + 1)
            ),
        ),
    ] {
        assert_ne!(unsupported, wire);
        let observed = observe(&format!("{complete}{unsupported}\n"));
        assert_eq!(observed.compiler_message_records, 0, "{unsupported}");
        assert_eq!(observed.checks, baseline.checks);
        assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
        assert_eq!(
            observed.malformed_complete_records,
            baseline.malformed_complete_records
        );
        assert_eq!(
            observed.unprojected_records,
            baseline.unprojected_records + 1
        );
    }
    for missing in [
        "stage",
        "provisional",
        "sample_complete",
        "compile_mode",
        "target_name",
    ] {
        let mut value: aterm_json::Value =
            aterm_json::from_str(wire.strip_prefix("verify-event: ").unwrap()).unwrap();
        value.as_object_mut().unwrap().remove(missing);
        let observed = observe(&format!("{complete}verify-event: {value}\n"));
        assert_eq!(observed.compiler_message_records, 0);
        assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
        assert_eq!(
            observed.unprojected_records,
            baseline.unprojected_records + 1
        );
    }
    let partial = observe(&wire);
    assert!(partial.trailing_partial);
    assert_eq!(partial.compiler_message_records, 0);
    assert_eq!(observe(&format!("{wire}\r\n")).compiler_message_records, 1);
}

#[test]
fn compiler_rows_are_capped_but_retained_rows_still_update_and_full_keys_do_not_merge() {
    let mut text = String::new();
    for at in 0..COMPILER_STAGE_ROWS + 2 {
        text.push_str(&format!(
            "{}\n",
            compiler_wire(&format!("stage-{at}"), false)
        ));
    }
    text.push_str(&format!("{}\n", compiler_wire("stage-0", true)));
    let observed = observe(&text);
    assert_eq!(observed.compiler_progress.len(), COMPILER_STAGE_ROWS);
    assert_eq!(observed.compiler_message_records, COMPILER_STAGE_ROWS + 3);
    assert_eq!(observed.compiler_omitted_records, 2);
    let row = observed
        .compiler_progress
        .iter()
        .find(|row| row.stage == "stage-0")
        .unwrap();
    assert_eq!(row.observed_message_records, 2);
    let prefix = "\n\u{1b}界".repeat(150);
    let mut text = String::new();
    for suffix in ["one", "two"] {
        let wire = compiler_wire(&format!("{prefix}{suffix}"), true);
        let mut value: aterm_json::Value =
            aterm_json::from_str(wire.strip_prefix("verify-event: ").unwrap()).unwrap();
        for field in [
            "output",
            "source_path",
            "fresh",
            "rebuild_reason",
            "producer_unit_sha256",
        ] {
            value
                .as_object_mut()
                .unwrap()
                .insert(field.into(), "PRIVATE-SENTINEL".into());
        }
        text.push_str(&format!("verify-event: {value}\n"));
    }
    let observed = observe(&text);
    assert_eq!(observed.compiler_progress.len(), 2);
    let json = observed.json();
    assert_eq!(
        json["compiler_progress"][0]["stage"],
        json["compiler_progress"][1]["stage"]
    );
    assert_eq!(
        json["compiler_progress"][0]["stage_truncated"].as_bool(),
        Some(true)
    );
    assert!(!json.to_string().contains("PRIVATE-SENTINEL"));
    assert!(!observed.render().contains("PRIVATE-SENTINEL"));
    assert!(!observed.render().contains('\u{1b}'));
}

const FAILED_CASE: &str = r#"verify-event: {"schema":1,"event":"test-case-failed","outcome":"FAIL","test":"-p fixture --lib","case":"t::a","binary_completed":false,"sample_complete":false}"#;
const PHASE_END: &str = r#"verify-event: {"schema":1,"event":"test-phase-ended","check":"test","test_run":1,"phase_sequence":6,"phase":"execution-admission","scope_exit":"advanced","seconds":1.5,"provisional":true,"sample_complete":false}"#;

fn changed_phase(key: &str, value: aterm_json::Value) -> String {
    let mut wire: aterm_json::Value =
        aterm_json::from_str(PHASE_END.strip_prefix("verify-event: ").unwrap()).unwrap();
    wire.as_object_mut().unwrap().insert(key.into(), value);
    format!("verify-event: {wire}\n")
}

#[test]
fn phase_ends_reject_unsupported_payloads_without_changing_verdicts() {
    let terminal = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let baseline = observe(&terminal);
    let zero = observe(&changed_phase("seconds", aterm_json::json!(0)));
    assert_eq!(zero.phase_ends[0].end.as_ref().unwrap().seconds(), 0.0);
    for (key, value) in [
        ("event", aterm_json::json!("test-phase-started")),
        ("provisional", aterm_json::json!(false)),
        ("sample_complete", aterm_json::json!(true)),
        ("check", aterm_json::json!("doc")),
        ("check", aterm_json::json!("unknown")),
        ("test_run", aterm_json::json!(0)),
        ("test_run", aterm_json::json!(-1)),
        ("phase_sequence", aterm_json::json!(0)),
        ("phase_sequence", aterm_json::json!(4294967296u64)),
        ("phase", aterm_json::json!("unknown")),
        ("scope_exit", aterm_json::json!("completed")),
        ("seconds", aterm_json::json!(-1.0)),
        ("seconds", aterm_json::json!("1.5")),
    ] {
        let invalid = observe(&format!("{terminal}{}", changed_phase(key, value)));
        assert!(invalid.phase_ends.is_empty(), "{key}");
        assert_eq!(invalid.unprojected_records, 1);
        assert_eq!(invalid.checks, baseline.checks);
        assert_eq!(invalid.terminal_consistency, baseline.terminal_consistency);
    }
    let json = PHASE_END.strip_prefix("verify-event: ").unwrap();
    assert!(crate::test_progress::End::read(&json.replace("1.5", "1e999")).is_none());
    assert!(
        crate::test_progress::End::read(
            &json.replace("\"test_run\":1", "\"test_run\":18446744073709551616")
        )
        .is_none()
    );
    for key in [
        "schema",
        "event",
        "check",
        "test_run",
        "phase_sequence",
        "phase",
        "scope_exit",
        "seconds",
        "provisional",
        "sample_complete",
    ] {
        let value: aterm_json::Value = aterm_json::from_str(json).unwrap();
        let duplicate = format!(
            "{},\"{key}\":{}}}",
            json.strip_suffix('}').unwrap(),
            value[key]
        );
        let invalid = observe(&format!("{terminal}verify-event: {duplicate}\n"));
        assert!(invalid.phase_ends.is_empty(), "duplicate {key}");
        let header = matches!(key, "schema" | "event");
        assert_eq!(invalid.unprojected_records, if header { 0 } else { 1 });
        assert_eq!(
            invalid.malformed_complete_records,
            if header { 1 } else { 0 }
        );
        assert_eq!(invalid.checks, baseline.checks);
        assert_eq!(
            invalid.terminal_consistency,
            if header {
                "unavailable"
            } else {
                baseline.terminal_consistency
            }
        );
    }
    let partial = observe(PHASE_END);
    assert!(partial.phase_ends.is_empty());
    assert!(partial.trailing_partial);
    assert_eq!(observe(&format!("{PHASE_END}\r\n")).phase_ends.len(), 1);
}

#[test]
fn phase_ends_keep_exact_partitions_and_make_counter_conflicts_sticky_unknown() {
    let first = writer(None);
    let second = writer(Some("b".repeat(40)));
    let first_line = first.prepare(PHASE_END);
    let second_line = second.prepare(&changed_phase("seconds", aterm_json::json!(2.5)));
    let observed = observe(&format!("{first_line}{second_line}"));
    assert_eq!(observed.phase_ends.len(), 2);
    assert!(observed.phase_ends.iter().all(|row| !row.conflict));
    assert_eq!(observed.invocations.len(), 2);
    assert!(
        observed
            .invocations
            .iter()
            .all(|group| group.observed.phase_ends.len() == 1)
    );
    for (key, value) in [
        ("check", aterm_json::json!("checkers")),
        ("phase", aterm_json::json!("dispatch")),
        ("seconds", aterm_json::json!(2.5)),
        ("scope_exit", aterm_json::json!("returned")),
    ] {
        let conflicting = first.prepare(&changed_phase(key, value));
        let observed = observe(&format!(
            "{first_line}{first_line}{conflicting}{first_line}"
        ));
        assert_eq!(observed.phase_ends.len(), 1);
        assert!(observed.phase_ends[0].conflict);
        assert_eq!(observed.phase_ends[0].occurrences, 4);
        assert!(observed.phase_ends[0].end.is_none());
        let json = observed.json();
        let row = &json["phase_ends"][0];
        for field in ["check", "phase", "seconds", "scope_exit"] {
            assert!(row[field].is_null());
        }
    }
    let repeated = observe(&format!("{first_line}{first_line}"));
    assert_eq!(repeated.phase_ends[0].occurrences, 2);
    assert!(!repeated.phase_ends[0].conflict);
    let mut conflict: aterm_json::Value =
        aterm_json::from_str(second_line.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), first.id().unwrap().into());
    let observed = observe(&format!(
        "{first_line}verify-event: {conflict}\n{PHASE_END}\n"
    ));
    assert!(observed.invocations[0].source_conflict);
    assert!(observed.invocations[0].source_observation.is_none());
    assert!(observed.invocations[0].observed.phase_ends[0].conflict);
    assert_eq!(observed.unattributed.as_ref().unwrap().phase_ends.len(), 1);
    assert!(
        observed.unattributed.as_ref().unwrap().phase_ends[0]
            .invocation_id
            .is_none()
    );
    conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), "invalid".into());
    let invalid = observe(&format!("{first_line}verify-event: {conflict}\n"));
    assert!(!invalid.invocations[0].observed.phase_ends[0].conflict);
    assert!(
        invalid.unattributed.as_ref().unwrap().phase_ends[0]
            .invocation_id
            .is_none()
    );
}

#[test]
fn phase_ends_cap_keys_update_existing_and_survive_terminals_without_start_joins() {
    let mut text = String::new();
    for sequence in 1..=PHASE_END_ROWS + 2 {
        text.push_str(&changed_phase(
            "phase_sequence",
            aterm_json::json!(u64::try_from(sequence).unwrap()),
        ));
    }
    text.push_str(&changed_phase("phase_sequence", aterm_json::json!(1)));
    let observed = observe(&text);
    assert_eq!(observed.phase_ends.len(), PHASE_END_ROWS);
    assert_eq!(observed.phase_end_records, PHASE_END_ROWS + 3);
    assert_eq!(observed.phase_end_omitted_records, 2);
    assert_eq!(observed.phase_ends[0].occurrences, 2);
    let terminal = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let completed = observe(&format!("{text}{terminal}"));
    assert_eq!(completed.phase_ends, observed.phase_ends);
    assert_eq!(
        completed.terminal_consistency,
        observe(&terminal).terminal_consistency
    );
    assert_eq!(
        completed.json()["phase_end_observations_complete"].as_bool(),
        Some(false)
    );
    let start = changed_phase("event", aterm_json::json!("test-phase-started"));
    let observed = observe(&start);
    assert!(observed.phase_ends.is_empty());
    assert_eq!(observed.unprojected_records, 1);
}

fn long_case_wire(test: &str, case: &str, at: u64) -> String {
    format!(
        "verify-event: {}\n",
        aterm_json::json!({
            "schema":1,"event":"test-case-running-long","test":test,"case":case,
            "outcome":"RUNNING","binary_completed":false,"sample_complete":false,
            "halts":false,"at_unix_ms":at,
        })
    )
}

#[test]
fn long_cases_are_historical_metadata_and_never_change_terminal_consistency() {
    let terminal = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let wire = long_case_wire("-p fixture --lib", "t::a", 20);
    let observed = observe(&format!(
        "{wire}{terminal}{}",
        long_case_wire("-p fixture --lib", "t::a", 10)
    ));
    let baseline = observe(&terminal);
    assert_eq!(observed.checks, baseline.checks);
    assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
    assert!(observed.early_cases.is_empty());
    assert_eq!(observed.long_case_records, 2);
    assert_eq!(observed.long_cases[0].occurrences, 2);
    assert_eq!(observed.long_cases[0].last_reported_at_unix_ms, 10);
    assert_eq!(
        observed.json()["long_case_observations_complete"].as_bool(),
        Some(false)
    );
    assert!(observe(wire.trim_end()).long_cases.is_empty());
    let crlf = wire.replace('\n', "\r\n");
    assert_eq!(observe(&crlf).long_cases.len(), 1);
    for (key, bad) in [
        ("test", aterm_json::json!("")),
        ("case", aterm_json::Value::Null),
        ("outcome", aterm_json::json!("FAIL")),
        ("binary_completed", aterm_json::json!(true)),
        ("sample_complete", aterm_json::json!(true)),
        ("halts", aterm_json::json!(true)),
        ("at_unix_ms", aterm_json::json!(-1)),
        ("at_unix_ms", aterm_json::json!("10")),
        ("test", aterm_json::json!("x".repeat(CASE_NAME_BYTES + 1))),
        ("case", aterm_json::json!("x".repeat(CASE_NAME_BYTES + 1))),
    ] {
        let mut value: aterm_json::Value =
            aterm_json::from_str(wire.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
        value.as_object_mut().unwrap().insert(key.into(), bad);
        let invalid = observe(&format!("{terminal}verify-event: {value}\n"));
        assert!(invalid.long_cases.is_empty(), "{key}");
        assert_eq!(invalid.unprojected_records, 1);
        assert_eq!(invalid.checks, baseline.checks);
        assert_eq!(invalid.terminal_consistency, baseline.terminal_consistency);
    }
    for key in [
        "schema",
        "event",
        "test",
        "case",
        "outcome",
        "binary_completed",
        "sample_complete",
        "halts",
        "at_unix_ms",
    ] {
        let json = wire.trim().strip_prefix("verify-event: ").unwrap();
        let original: aterm_json::Value = aterm_json::from_str(json).unwrap();
        let duplicate = format!(
            "{},\"{key}\":{}}}",
            json.strip_suffix('}').unwrap(),
            original[key]
        );
        let invalid = observe(&format!("{terminal}verify-event: {duplicate}\n"));
        assert!(invalid.long_cases.is_empty(), "duplicate {key}");
        let header = matches!(key, "schema" | "event");
        assert_eq!(invalid.unprojected_records, if header { 0 } else { 1 });
        assert_eq!(
            invalid.malformed_complete_records,
            if header { 1 } else { 0 }
        );
        assert_eq!(invalid.checks, baseline.checks);
        assert_eq!(
            invalid.terminal_consistency,
            if header {
                "unavailable"
            } else {
                baseline.terminal_consistency
            }
        );
    }
}

#[test]
fn long_cases_keep_exact_invocations_source_conflicts_and_unattributed_records() {
    let first = writer(None);
    let second = writer(Some("b".repeat(40)));
    let wire = long_case_wire("spec", "case", 10);
    let first_line = first.prepare(&wire);
    let second_line = second.prepare(&wire);
    let observed = observe(&format!("{first_line}{second_line}{wire}"));
    assert_eq!(observed.invocations.len(), 2);
    assert!(
        observed
            .invocations
            .iter()
            .all(|run| run.observed.long_case_records == 1)
    );
    assert_eq!(observed.unattributed.as_ref().unwrap().long_case_records, 1);
    let mut conflict: aterm_json::Value =
        aterm_json::from_str(second_line.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), first.id().unwrap().into());
    let observed = observe(&format!("{first_line}verify-event: {conflict}\n"));
    assert_eq!(observed.invocations.len(), 1);
    assert!(observed.invocations[0].source_conflict);
    assert!(observed.invocations[0].source_observation.is_none());
    assert_eq!(
        observed.invocations[0].observed.long_cases[0].occurrences,
        2
    );
    conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), "invalid".into());
    let observed = observe(&format!("{first_line}verify-event: {conflict}\n"));
    assert_eq!(observed.invocations[0].observed.long_case_records, 1);
    assert_eq!(observed.unattributed.as_ref().unwrap().long_case_records, 1);
}

#[test]
fn long_cases_bound_rows_update_retained_keys_and_clip_only_presentation() {
    let mut text = String::new();
    for index in 0..LONG_CASE_ROWS + 2 {
        text.push_str(&long_case_wire("spec", &format!("case-{index}"), 10));
    }
    text.push_str(&long_case_wire("spec", "case-0", 5));
    let observed = observe(&text);
    assert_eq!(observed.long_cases.len(), LONG_CASE_ROWS);
    assert_eq!(observed.long_case_records, LONG_CASE_ROWS + 3);
    assert_eq!(observed.long_case_omitted_records, 2);
    let retained = observed
        .long_cases
        .iter()
        .find(|row| row.case == "case-0")
        .unwrap();
    assert_eq!(retained.occurrences, 2);
    assert_eq!(retained.last_reported_at_unix_ms, 5);
    let prefix = "\n\u{1b}界".repeat(150);
    let mut text = String::new();
    for suffix in ["one", "two"] {
        let mut value: aterm_json::Value = aterm_json::from_str(
            long_case_wire(
                &format!("{prefix}{suffix}"),
                &format!("{prefix}{suffix}"),
                10,
            )
            .trim()
            .strip_prefix("verify-event: ")
            .unwrap(),
        )
        .unwrap();
        for key in ["output", "source_path", "detail", "elapsed_s"] {
            value
                .as_object_mut()
                .unwrap()
                .insert(key.into(), "PRIVATE-SENTINEL".into());
        }
        text.push_str(&format!("verify-event: {value}\n"));
    }
    let observed = observe(&text);
    assert_eq!(observed.long_cases.len(), 2);
    let json = observed.json();
    assert_eq!(json["long_cases"][0]["case"], json["long_cases"][1]["case"]);
    assert_eq!(
        json["long_cases"][0]["case_truncated"].as_bool(),
        Some(true)
    );
    assert!(!json.to_string().contains("PRIVATE-SENTINEL"));
    assert!(!observed.render().contains("PRIVATE-SENTINEL"));
    assert!(!observed.render().contains('\u{1b}'));
}

#[test]
fn case_observations_keep_repeats_conflicts_and_null_or_empty_reported_names() {
    let mut refused: aterm_json::Value =
        aterm_json::from_str(FAILED_CASE.strip_prefix("verify-event: ").unwrap()).unwrap();
    refused.as_object_mut().unwrap().insert(
        "outcome".into(),
        aterm_json::Value::String("NOT RUN".into()),
    );
    let mut unnamed = refused.clone();
    unnamed
        .as_object_mut()
        .unwrap()
        .insert("case".into(), aterm_json::Value::Null);
    let mut empty = refused.clone();
    empty
        .as_object_mut()
        .unwrap()
        .insert("case".into(), aterm_json::Value::String(String::new()));
    let observed = observe(&format!(
        "{FAILED_CASE}\n{FAILED_CASE}\nverify-event: {refused}\nverify-event: {unnamed}\nverify-event: {empty}\n"
    ));
    assert_eq!(observed.early_cases.len(), 3);
    let named = observed
        .early_cases
        .iter()
        .find(|case| case.case.as_deref() == Some("t::a"))
        .unwrap();
    assert_eq!(named.occurrences, 3);
    assert_eq!(
        named
            .outcomes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["FAIL", "NOT RUN"]
    );
    assert!(observed.early_cases.iter().any(|case| case.case.is_none()));
    assert!(
        observed
            .early_cases
            .iter()
            .any(|case| case.case.as_deref() == Some(""))
    );
    assert!(observed.checks.is_empty());
    assert_eq!(observed.terminal_records, 0);
    assert_eq!(
        observed.json()["case_observations_complete"].as_bool(),
        Some(false)
    );
}

#[test]
fn unsupported_case_metadata_and_running_observations_leave_terminal_consistency_unchanged() {
    let complete = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS)
    );
    let baseline = observe(&complete);
    for (key, value) in [
        ("test", aterm_json::Value::Null),
        ("test", aterm_json::Value::String(String::new())),
        ("case", aterm_json::json!(3)),
        ("outcome", aterm_json::Value::String("RUNNING".into())),
        ("binary_completed", aterm_json::json!(true)),
        ("sample_complete", aterm_json::Value::Null),
        (
            "event",
            aterm_json::Value::String("test-case-running-long".into()),
        ),
    ] {
        let mut value_of_event: aterm_json::Value =
            aterm_json::from_str(FAILED_CASE.strip_prefix("verify-event: ").unwrap()).unwrap();
        value_of_event
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        let observed = observe(&format!("{complete}verify-event: {value_of_event}\n"));
        assert!(observed.early_cases.is_empty());
        assert_eq!(observed.checks, baseline.checks);
        assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
        assert_eq!(
            observed.malformed_complete_records,
            baseline.malformed_complete_records
        );
        assert_eq!(
            observed.unprojected_records,
            baseline.unprojected_records + 1
        );
    }
    for missing in ["test", "case", "binary_completed", "sample_complete"] {
        let mut value: aterm_json::Value =
            aterm_json::from_str(FAILED_CASE.strip_prefix("verify-event: ").unwrap()).unwrap();
        value.as_object_mut().unwrap().remove(missing);
        let observed = observe(&format!("{complete}verify-event: {value}\n"));
        assert!(observed.early_cases.is_empty());
        assert_eq!(observed.checks, baseline.checks);
        assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
        assert_eq!(
            observed.malformed_complete_records,
            baseline.malformed_complete_records
        );
        assert_eq!(
            observed.unprojected_records,
            baseline.unprojected_records + 1
        );
    }
}

#[test]
fn case_metadata_obeys_complete_record_bounds_and_exact_invocation_groups() {
    let first = writer(None);
    let second = writer(None);
    let bound = first.prepare(FAILED_CASE);
    let observed = observe(&format!(
        "{bound}{}{FAILED_CASE}\n",
        second.prepare(FAILED_CASE)
    ));
    assert_eq!(observed.invocations.len(), 2);
    assert!(
        observed
            .invocations
            .iter()
            .all(|group| group.observed.early_cases[0].occurrences == 1)
    );
    assert_eq!(
        observed.unattributed.as_ref().unwrap().early_cases[0].occurrences,
        1
    );
    let crlf = format!("{}\r\n", bound.trim_end_matches('\n'));
    assert_eq!(observe(&crlf).invocations[0].observed.early_cases.len(), 1);
    let partial = observe(bound.trim_end_matches('\n'));
    assert!(partial.early_cases.is_empty());
    assert!(partial.invocations.is_empty());
    assert!(partial.trailing_partial);
    let mut large: aterm_json::Value =
        aterm_json::from_str(bound.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    large.as_object_mut().unwrap().insert(
        "padding".into(),
        aterm_json::Value::String("x".repeat(MAX_RECORD_BYTES + 1)),
    );
    let oversized = observe(&format!("verify-event: {large}\n"));
    assert!(oversized.early_cases.is_empty());
    assert!(oversized.invocations.is_empty());
    assert_eq!(oversized.malformed_complete_records, 1);
    let other_source = writer(Some("b".repeat(40))).prepare(FAILED_CASE);
    let mut conflict: aterm_json::Value =
        aterm_json::from_str(other_source.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    let id = first.id().unwrap().to_owned();
    conflict
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), aterm_json::Value::String(id));
    let conflicted = observe(&format!("{bound}verify-event: {conflict}\n"));
    assert_eq!(conflicted.invocations.len(), 1);
    assert!(conflicted.invocations[0].source_conflict);
    assert!(conflicted.invocations[0].source_observation.is_none());
    assert_eq!(
        conflicted.invocations[0].observed.early_cases[0].occurrences,
        2
    );
}

#[test]
fn clipped_case_presentation_never_merges_full_keys_or_exposes_assertion_fields() {
    let prefix = "\n\u{1b}\\\"界".repeat(150);
    let mut text = String::new();
    for suffix in ["first", "second"] {
        let mut value: aterm_json::Value =
            aterm_json::from_str(FAILED_CASE.strip_prefix("verify-event: ").unwrap()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert(
            "test".into(),
            aterm_json::Value::String(format!("{prefix}{suffix}")),
        );
        object.insert(
            "case".into(),
            aterm_json::Value::String(format!("{prefix}{suffix}")),
        );
        for field in ["output", "source_path", "detail", "rerun"] {
            object.insert(
                field.into(),
                aterm_json::Value::String(format!("PRIVATE-{field}-SENTINEL")),
            );
        }
        text.push_str(&format!("verify-event: {value}\n"));
    }
    let observed = observe(&text);
    assert_eq!(observed.early_cases.len(), 2);
    assert!(
        observed
            .early_cases
            .iter()
            .all(|case| case.occurrences == 1)
    );
    let json = observed.json();
    let cases = json["early_cases"].as_array().unwrap();
    assert_eq!(cases[0]["test"], cases[1]["test"]);
    assert!(
        cases
            .iter()
            .all(|case| case["test_truncated"].as_bool() == Some(true))
    );
    assert!(
        cases
            .iter()
            .all(|case| case["case_truncated"].as_bool() == Some(true))
    );
    assert!(
        cases
            .iter()
            .all(|case| case["test"].to_string().len() <= crate::report::EVENT_ROW_BYTES + 2)
    );
    assert!(
        cases
            .iter()
            .all(|case| case["case"].to_string().len() <= crate::report::EVENT_ROW_BYTES + 2)
    );
    let rendered = observed.render();
    assert!(!rendered.contains('\u{1b}'));
    for field in ["output", "source_path", "detail", "rerun"] {
        let sentinel = format!("PRIVATE-{field}-SENTINEL");
        assert!(!json.to_string().contains(&sentinel));
        assert!(!rendered.contains(&sentinel));
    }
}

fn writer(head: Option<String>) -> crate::event_log::EventLog {
    let ctx = crate::Ctx::new(
        PathBuf::from("/fixture"),
        crate::Scope::crate_only("aterm-verify"),
        crate::EnvSnapshot::default(),
        PathBuf::from("/fixture/scratch"),
    )
    .with_checks(&[Check::Doc])
    .with_head_at_start(head);
    let mut writer = crate::event_log::EventLog::unrecorded();
    writer.bind(crate::event_log::Context::captured(&ctx));
    writer
}

#[test]
fn ambiguous_raw_bindings_stay_globally_observed_but_unattributed() {
    let writer = writer(None);
    let check = writer.prepare(&event(Check::Doc));
    let terminal = writer.prepare(&crate::report::run_event(
        &[result(Check::Doc, Outcome::Ok)],
        crate::exit::PASS,
    ));
    let value: aterm_json::Value =
        aterm_json::from_str(check.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    let binding = value["invocation"].to_string();
    let needle = format!("\"invocation\":{binding}");
    let text = format!("{check}{terminal}");
    let baseline = observe(&text);
    let other = "f".repeat(32);
    let duplicate_id = binding.replacen("\"id\":", &format!("\"id\":\"{other}\",\"id\":"), 1);
    let escaped_id = binding.replacen("\"id\":", &format!("\"\\u0069d\":\"{other}\",\"id\":"), 1);
    let source = binding.replacen(
        "\"selection\":",
        "\"selection\":\"other\",\"selection\":",
        1,
    );
    let escaped_source = binding.replacen(
        "\"observed_head\":",
        "\"\\u006fbserved_head\":null,\"observed_head\":",
        1,
    );
    let bindings = [duplicate_id, escaped_id, source, escaped_source];
    let mut replacements = bindings
        .iter()
        .map(|binding| format!("\"invocation\":{binding}"))
        .collect::<Vec<_>>();
    replacements.push(format!("\"invocation\":null,{needle}"));
    for replacement in replacements {
        assert_ne!(replacement, needle);
        let ambiguous = text.replace(&needle, &replacement);
        assert_ne!(ambiguous, text);
        let observed = observe(&ambiguous);
        assert_eq!(observed.checks, baseline.checks);
        assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
        assert_eq!(observed.malformed_complete_records, 0);
        assert!(observed.invocations.is_empty());
        let unknown = observed.unattributed.as_ref().unwrap();
        assert_eq!(unknown.checks, baseline.checks);
        assert_eq!(unknown.terminal_consistency, "matching-observed-rows");
    }
    let second = self::writer(None);
    let mixed = observe(&format!(
        "{text}{}{}\n",
        second.prepare(&event(Check::Doc)),
        event(Check::Doc)
    ));
    assert_eq!(mixed.invocations.len(), 2);
    assert_eq!(mixed.unattributed.as_ref().unwrap().checks.len(), 1);
}

#[test]
fn duplicate_raw_event_headers_cannot_supply_last_winning_checks_or_terminals() {
    let writer = writer(None);
    for raw in [
        event(Check::Doc),
        crate::report::run_event(&[result(Check::Doc, Outcome::Ok)], crate::exit::PASS),
    ] {
        let bound = writer.prepare(&raw);
        for (key, replacement) in [
            ("\"schema\":", "\"schema\":0,\"schema\":"),
            ("\"schema\":", "\"\\u0073chema\":1,\"schema\":"),
            ("\"event\":", "\"event\":\"other\",\"event\":"),
            ("\"event\":", "\"\\u0065vent\":\"other\",\"event\":"),
        ] {
            let ambiguous = bound.replacen(key, replacement, 1);
            assert_ne!(ambiguous, bound);
            let observed = observe(&ambiguous);
            assert!(observed.checks.is_empty());
            assert_eq!(observed.terminal_records, 0);
            assert_eq!(observed.malformed_complete_records, 1);
            assert!(observed.invocations.is_empty());
        }
    }
}

#[test]
fn an_oversized_rejected_record_cannot_supply_attribution_and_crlf_uses_the_same_gate() {
    let writer = writer(None);
    let bound = writer.prepare(&event(Check::Doc));
    let crlf = format!("{}\r\n", bound.trim_end_matches('\n'));
    assert_eq!(observe(&crlf).invocations.len(), 1);
    let mut value: aterm_json::Value =
        aterm_json::from_str(bound.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "pad".into(),
        aterm_json::Value::String("x".repeat(MAX_RECORD_BYTES + 1)),
    );
    let observed = observe(&format!("verify-event: {value}\n"));
    assert!(observed.invocations.is_empty());
    assert_eq!(observed.malformed_complete_records, 1);
    assert_eq!(observed.unattributed.unwrap().malformed_complete_records, 1);
    assert!(observed.checks.is_empty());
}

#[test]
fn incomplete_overlapping_invocations_partition_by_each_event_and_leave_legacy_unknown() {
    let first = writer(None);
    let second = writer(None);
    // Neither writer needs a header for its actual check record to retain attribution.
    let text = format!(
        "{}{}{}",
        first.prepare(&event(Check::Doc)),
        second.prepare(&event(Check::Doc)),
        event(Check::Fmt)
    );
    let observed = observe(&text);
    assert_eq!(observed.invocations.len(), 2);
    assert!(
        observed
            .invocations
            .iter()
            .all(|run| run.observed.checks[0].occurrences == 1)
    );
    assert_eq!(
        observed.unattributed.as_ref().unwrap().checks[0].check,
        "fmt"
    );
    assert!(observed.json()["run_identity"].is_null());
    assert!(observed.json()["source_identity"].is_null());
    assert_eq!(
        observed.json()["aggregate_scope"].as_str(),
        Some("all-records")
    );
    // A preceding bound start record cannot claim an adjacent legacy record.
    let header_only = format!(
        "{}{}",
        first.prepare("verify-event: {\"schema\":1,\"event\":\"invocation-started\"}"),
        event(Check::Doc)
    );
    let observed = observe(&header_only);
    assert!(observed.invocations[0].observed.checks.is_empty());
    assert_eq!(observed.unattributed.unwrap().checks[0].check, "doc");
}

#[test]
fn conflicting_source_metadata_and_partial_invalid_ids_never_become_checkout_proof() {
    let first = writer(Some("a".repeat(40)));
    let other = writer(Some("b".repeat(40)));
    let original = first.prepare(&event(Check::Doc));
    let mut foreign: aterm_json::Value = aterm_json::from_str(
        other
            .prepare(&event(Check::Fmt))
            .trim()
            .strip_prefix("verify-event: ")
            .unwrap(),
    )
    .unwrap();
    foreign
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "id".into(),
            aterm_json::Value::String(first.id().unwrap().to_owned()),
        );
    let mut invalid = foreign.clone();
    invalid
        .as_object_mut()
        .unwrap()
        .get_mut("invocation")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("id".into(), aterm_json::Value::String("not-an-id".into()));
    let text =
        format!("{original}verify-event: {foreign}\nverify-event: {invalid}\nverify-event: {{");
    let observed = observe(&text);
    assert_eq!(observed.invocations.len(), 1);
    assert!(observed.invocations[0].source_conflict);
    assert!(observed.invocations[0].source_observation.is_none());
    assert!(observed.unattributed.is_some());
    assert!(observed.trailing_partial);
    assert!(observed.json()["source_identity"].is_null());
    assert!(observed.json()["invocations"][0]["source_observation"].is_null());
}

fn result(check: Check, value: Outcome) -> crate::report::CheckResult {
    crate::report::CheckResult {
        check,
        outcome: value,
        seconds: 1.0,
        detail: "private failure body".into(),
        findings: Vec::new(),
        rerun: "private rerun arguments".into(),
        stopped: false,
    }
}

fn event(check: Check) -> String {
    crate::report::check_event(&result(check, Outcome::Ok), &[])
}

#[test]
fn full_durable_file_keeps_earlier_checks_despite_later_output_volume() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("verify.events");
    let log = crate::event_log::EventLog::open(&path).unwrap();
    log.write(&event(Check::Doc));
    log.write(&event(Check::Lint));
    for index in 0..1100 {
        log.write(&crate::report::stage_started(index, &"stage".repeat(1000)));
    }
    log.write(&event(Check::Test));
    let text = std::fs::read_to_string(&path).unwrap();
    let observed = observe(&text);
    assert_eq!(
        observed
            .checks
            .iter()
            .map(|row| row.check.as_str())
            .collect::<Vec<_>>(),
        ["doc", "lint", "test"]
    );
    let mut tail = text.lines().rev().take(1000).collect::<Vec<_>>();
    tail.reverse();
    let tail = observe(&(tail.join("\n") + "\n"));
    assert_eq!(
        tail.checks
            .iter()
            .map(|row| row.check.as_str())
            .collect::<Vec<_>>(),
        ["test"]
    );
    let serialized = observed.json().to_string();
    assert!(!serialized.contains("private failure") && !serialized.contains("private rerun"));
}

#[test]
fn legacy_incomplete_appended_runs_are_indistinguishable_from_one_run() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let one = dir.path().join("one.events");
    let two = dir.path().join("two.events");
    // Historical report bytes predate per-event attribution. Current EventLog writers
    // deliberately cannot reproduce this ambiguity: each now supplies its own identity.
    std::fs::write(&one, format!("{}{}", event(Check::Doc), event(Check::Lint))).unwrap();
    for row in [event(Check::Doc), event(Check::Lint)] {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&two)
            .unwrap()
            .write_all(row.as_bytes())
            .unwrap();
    }
    let one_incomplete_run = std::fs::read_to_string(one).unwrap();
    let separate_incomplete_runs = std::fs::read_to_string(two).unwrap();
    assert_eq!(one_incomplete_run, separate_incomplete_runs);
    let observed = observe(&one_incomplete_run);
    assert_eq!(observed, observe(&separate_incomplete_runs));
    assert!(observed.invocations.is_empty());
    assert!(observed.unattributed.is_some());
    assert_eq!(observed.terminal_consistency, "not-observed");
    assert_eq!(observed.json()["run_identity"], aterm_json::Value::Null);
    assert_eq!(observed.json()["source_identity"], aterm_json::Value::Null);
}

#[test]
fn duplicates_conflicts_and_terminal_lists_do_not_become_latest_run_verdicts() {
    let doc = result(Check::Doc, Outcome::Ok);
    let complete = format!(
        "{}{}",
        event(Check::Doc),
        crate::report::run_event(&[doc], 0)
    );
    assert_eq!(
        observe(&complete).terminal_consistency,
        "matching-observed-rows"
    );
    let twice = observe(&format!("{complete}{complete}"));
    assert_eq!(twice.terminal_consistency, "multiple-terminal-records");
    assert_eq!(twice.checks[0].occurrences, 2);
    let failed = crate::report::check_event(
        &result(Check::Doc, Outcome::Fail(crate::Severity::GateFailed)),
        &[],
    );
    let conflict = observe(&format!("{complete}{failed}"));
    assert_eq!(conflict.checks[0].outcomes.len(), 2);
    assert_eq!(conflict.terminal_consistency, "mismatch");
}

#[test]
fn malformed_complete_and_partial_records_are_distinct_and_never_filled_in() {
    let partial = observe(&format!("{}verify-event: {{", event(Check::Doc)));
    assert!(partial.trailing_partial);
    assert_eq!(partial.malformed_complete_records, 0);
    let invalid = observe(
        "verify-event: {\nverify-event: {\"schema\":2,\"event\":\"check-finished\"}\nverify-event: {\"schema\":1,\"event\":\"check-finished\"}\n",
    );
    assert_eq!(invalid.malformed_complete_records, 3);
    assert!(invalid.checks.is_empty());
}

#[test]
fn explicit_file_reader_needs_no_checkout_or_toolchain_and_never_runs_checks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("observations.events");
    std::fs::write(&path, event(Check::Doc)).unwrap();
    assert_eq!(
        crate::run_cli(
            &[
                "status".into(),
                "--events".into(),
                path.display().to_string(),
                "--json".into(),
            ],
            dir.path()
        ),
        crate::exit::PASS
    );
    assert_eq!(
        crate::run_cli(
            &["status".into(), "--events".into(), "absent.events".into(),],
            dir.path()
        ),
        crate::exit::COULD_NOT_RUN
    );
}
