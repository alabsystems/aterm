// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_successful_completed_runs_keep_a_learned_partition() {
    let binary = binary();
    let output = [
        r#"{"type":"suite","event":"started","test_count":2}"#,
        r#"{"type":"test","event":"started","name":"a::one"}"#,
        r#"{"type":"test","event":"ok","name":"a::one","exec_time":1.0}"#,
        r#"{"type":"test","event":"started","name":"b::one"}"#,
        r#"{"type":"test","event":"ok","name":"b::one","exec_time":1.0}"#,
        r#"{"type":"suite","event":"ok","passed":2,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":180.0}"#,
    ].join("\n");
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("case-events");
    std::fs::write(&path, format!("{output}\n")).unwrap();
    let observations = crate::exec::case_observations(&path, true);
    assert_eq!(observations.timings.len(), 2);
    for ok in [true, false] {
        let run = Run {
            ok,
            code: Some(if ok { 0 } else { 101 }),
            output: crate::libtest_json::render(&output, None),
            case_observations: Some(observations.clone()),
            ..Run::default()
        };
        let ran = Ran::of(run, Duration::ZERO, Duration::from_secs(180));
        let (_, line, sample) = binary_event(&binary, &ran, true, None, true);
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        if ok {
            assert_eq!(event["partition"]["filter"].as_str(), Some("a::"));
            assert_eq!(event["partition"]["selected_cases"].as_u64(), Some(1));
            assert_eq!(event["partition"]["remaining_cases"].as_u64(), Some(1));
            assert_eq!(event["partition"]["selected_case_s"].as_f64(), Some(1.0));
        } else {
            assert!(event["partition"].is_null());
        }
        let sample = sample.unwrap();
        assert_eq!(
            sample.partition.as_deref(),
            if ok { Some("a::") } else { None }
        );
    }
    std::fs::write(&path, &output).unwrap();
    let partial = crate::exec::case_observations(&path, true);
    let run = Run {
        ok: true,
        code: Some(0),
        output: crate::libtest_json::render(&output, None),
        case_observations: Some(partial),
        ..Run::default()
    };
    let ran = Ran::of(run, Duration::ZERO, Duration::from_secs(180));
    assert_eq!(
        binary_event(&binary, &ran, true, None, true)
            .2
            .unwrap()
            .partition,
        None
    );
}

fn binary() -> Binary {
    Binary {
        harness: false,
        header: "     Running unittests src/lib.rs (/owned/target/deps/fixture)".into(),
        spec: "-p fixture --lib".into(),
        record: Record {
            cwd: PathBuf::from("/owned/fixture"),
            argv: vec!["/owned/target/deps/fixture".into()],
            env: Vec::new(),
        },
    }
}

/// A direct libtest result: its known Binary supplies the rerun scope later.
fn failed(binary: &Binary, chatter: &str, failures: &[(&str, &str)]) -> Run {
    let mut output = format!(
        "{}\n\nrunning {} tests\n",
        binary.header,
        failures.len() + 1
    );
    output.push_str(chatter);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str("test simulated_disk_full ... ok\n");
    for (name, _) in failures {
        output.push_str(&format!("test {name} ... FAILED\n"));
    }
    output.push_str("\nfailures:\n\n");
    for (name, message) in failures {
        output.push_str(&format!(
            "---- {name} stdout ----\n\nthread '{name}' panicked at fixture.rs:1:1:\n{message}\n\n"
        ));
    }
    output.push_str("failures:\n");
    for (name, _) in failures {
        output.push_str(&format!("    {name}\n"));
    }
    output.push_str(&format!("\ntest result: FAILED. 1 passed; {} failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s\n\n", failures.len()));
    assert!(!output.contains("to rerun pass"));
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output,
        code: Some(101),
        spawn_error: None,
        usage: None,
    }
}

fn event(binary: &Binary, run: &Run) -> aterm_json::Value {
    let (event, sample) = event_and_sample(binary, run, true);
    if let Some(sample) = sample {
        assert_eq!(event["outcome"].as_str(), Some(sample.outcome.as_str()));
    }
    event
}

#[test]
fn a_json_binary_keeps_structured_failure_identity_and_never_falls_back_to_human_text() {
    let binary = binary();
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("original");
    let block = crate::libtest_json::NESTED_GUI_FAILURE;
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            aterm_json::json!({"type":"test", "event":"started", "name":"outer"}),
            aterm_json::json!({"type":"test", "event":"failed", "name":"outer", "stdout":block})
        ),
    )
    .unwrap();
    let mut run = failed(&binary, "", &[("human-decoy", "ordinary assertion")]);
    let control = event(&binary, &run);
    assert_eq!(control["failures"][0]["case"].as_str(), Some("human-decoy"));
    run.case_observations = Some(crate::exec::case_observations(&path, true));
    let structured = event(&binary, &run);
    assert_eq!(structured["failures"][0]["case"].as_str(), Some("outer"));
    assert!(
        structured["failures"][0]["output"]
            .as_str()
            .unwrap()
            .contains("seamless_fixture_generator_tests.rs:4723:5")
    );
    assert_eq!(structured["failures_complete"].as_bool(), Some(false));
    // Known JSON whose original log is unreadable must not manufacture a human case.
    run.case_observations = Some(crate::exec::case_observations(
        &dir.path().join("missing"),
        true,
    ));
    let unread = event(&binary, &run);
    assert!(unread["failures"].as_array().unwrap().is_empty());
    assert_eq!(unread["outcome"], control["outcome"]);
    assert_eq!(unread["failure_observation_end"].as_str(), Some("unread"));
    for raw in [
        "{\"type\":\"test\",\"event\":\"failed\"\n".to_owned(),
        format!(
            "{}\n{}\n",
            aterm_json::json!({"type":"test", "event":"started", "name":"passing"}),
            aterm_json::json!({"type":"test", "event":"ok", "name":"passing", "stdout":block})
        ),
    ] {
        std::fs::write(&path, raw).unwrap();
        run.case_observations = Some(crate::exec::case_observations(&path, true));
        let observed = event(&binary, &run);
        assert!(observed["failures"].as_array().unwrap().is_empty());
        assert_eq!(observed["outcome"], control["outcome"]);
    }
}

#[test]
fn ten_escaped_failure_samples_fit_the_event_reader_and_count_exact_omissions() {
    let binary = binary();
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("original");
    let block = format!(
        "{}{}{}",
        "\0\n\r\t\\\"界".repeat(3000),
        crate::libtest::COULD_NOT_RUN_SENTINEL,
        "actionable tail 界\\\"\n".repeat(100)
    );
    let mut raw = String::new();
    for at in 0..10 {
        let name = format!("case-{at}");
        raw.push_str(&format!("{}\n{}\n", aterm_json::json!({"type":"test", "event":"started", "name":name}), aterm_json::json!({"type":"test", "event":"failed", "name":name, "stdout":block, "source_path":"界\\\"".repeat(1024), "start_line":7, "start_col":9})));
    }
    std::fs::write(&path, raw).unwrap();
    let mut run = failed(&binary, "", &[("human", "ordinary failure")]);
    run.case_observations = Some(crate::exec::case_observations(&path, true));
    let observed = event(&binary, &run);
    assert!(aterm_json::to_string(&observed).unwrap().len() + "verify-event: \n".len() < 64 << 10);
    let failures = observed["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 10);
    for failure in failures {
        let output = failure["output"].as_str().unwrap();
        assert!(aterm_json::to_string(output).unwrap().len() <= FAILURE_OUTPUT_BYTES + 2);
        let (head, tail) = output.split_once("\n…\n").unwrap();
        assert!(block.starts_with(head));
        assert!(block.ends_with(tail));
        assert!(tail.contains("actionable tail"));
        assert_eq!(
            failure["omitted_bytes"].as_u64(),
            Some((block.len() - head.len() - tail.len()) as u64)
        );
        assert_eq!(
            failure["refused"].as_bool(),
            Some(true),
            "full block classifies before clipping"
        );
    }
}

/// The event of `run`, at a width of 3 charged 7 cores, with the fields every event has, and the history sample
/// it keeps, if it keeps one, with the figures the event has and the grant each thread had (two).
fn event_and_sample(
    binary: &Binary,
    run: &Run,
    started: bool,
) -> (aterm_json::Value, Option<crate::test_history::Sample>) {
    let mut ran = Ran::of(
        run.clone(),
        Duration::from_millis(1250),
        Duration::from_millis(8500),
    );
    ran.planned.width = 3;
    ran.planned.charge = 7;
    let evidence = crate::test_content::Evidence {
        exe_sha256: Some("ab".repeat(32)),
        content_key: Some("cd".repeat(32)),
        reason: None,
        parts: crate::test_content::KeyParts::new(),
    };
    let (_, line, sample) = binary_event(binary, &ran, started, Some(&evidence), false);
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["schema"].as_u64(), Some(1));
    assert_eq!(event["event"].as_str(), Some("test-binary-finished"));
    assert_eq!(event["test"].as_str(), Some(binary.spec.as_str()));
    assert_eq!(event["exit_code"].as_i64(), run.code.map(i64::from));
    assert_eq!(event["waiting_s"].as_f64(), Some(1.25));
    assert_eq!(event["execution_s"].as_f64(), Some(8.5));
    assert_eq!(event["width"].as_u64(), Some(3));
    assert_eq!(event["charge"].as_u64(), Some(7));
    assert_eq!(event["exe_sha256"].as_str(), Some("ab".repeat(32).as_str()));
    assert_eq!(
        event["content_key"].as_str(),
        Some("cd".repeat(32).as_str())
    );
    assert_eq!(event["recorded_execution_s"].as_f64(), Some(8.5));
    assert_eq!(event["carried"].as_bool(), Some(false));
    if let Some(sample) = &sample {
        assert_eq!(sample.width, 3);
        assert_eq!(sample.cores, Some(2), "seven cores over three threads");
        assert_eq!(event["tests"].as_u64(), sample.tests);
        assert_eq!(
            sample.wall_ms,
            if run.stopped_at.is_some() { 0 } else { 8500 }
        );
    }
    (event, sample)
}

/// The event and the sample of a binary `run` that measured it.
fn measured_sample(binary: &Binary, run: &Run) -> (aterm_json::Value, crate::test_history::Sample) {
    let (event, sample) = event_and_sample(binary, run, true);
    (
        event,
        sample.expect("a run that measured its binary keeps a sample"),
    )
}

/// A BINARY'S HISTORY KEEPS ONLY A RUN THAT MEASURED IT OR ITS LATEST DECISIVE OUTCOME: one the
/// run's ceiling left no time to start ([`not_started`]), one something outside it ended (an
/// interrupt that came before it started, a stuck holder: `spawn_error`) and one that ended as an
/// interrupt was ending the run keep no sample, each with its event as before. One verify stopped
/// at its first red case keeps an outcome-only FAIL with zero wall and no usage, while one the
/// ceiling ended mid-run keeps its measured lower bound with its wall and test count. NEGATIVE
/// CONTROL: the unstarted run counted as started, the stopped one with no stop, and the
/// interrupted one once the run is no longer ending each keep a measured sample.
#[test]
fn only_a_measured_run_or_a_decisive_outcome_keeps_a_sample() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let binary = binary();
    let never = not_started(&binary, Duration::from_secs(30));
    let (event, sample) = event_and_sample(&binary, &never, false);
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    assert!(sample.is_none(), "{sample:?}");
    assert!(event_and_sample(&binary, &never, true).1.is_some());

    for why in [
        "the run was interrupted before this child started",
        "this child waited for admission or a lock behind a holder that has made no progress",
    ] {
        let mut ended = failed(&binary, "", &[("broke", "assertion failed")]);
        ended.code = None;
        ended.spawn_error = Some(why.to_owned());
        let (event, sample) = event_and_sample(&binary, &ended, true);
        assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
        assert!(sample.is_none(), "{why}: {sample:?}");
    }

    let mut stopped = failed(&binary, "", &[("broke", "assertion failed")]);
    stopped.stopped_at = Some(crate::libtest_json::Stopped {
        spec: binary.spec.clone(),
        case: "broke".to_owned(),
    });
    let (event, sample) = event_and_sample(&binary, &stopped, true);
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    let sample = sample.expect("a decisive red keeps its outcome");
    assert_eq!(sample.outcome, "FAIL");
    assert_eq!(sample.wall_ms, 0);
    assert_eq!(
        (sample.cpu_ms, sample.starved_ms, sample.peak_bytes),
        (None, None, None)
    );
    assert_eq!((sample.width, sample.tests), (3, Some(2)));
    stopped.stopped_at = None;
    assert!(event_and_sample(&binary, &stopped, true).1.is_some());

    let ceiling = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!(
            "{}\n\nrunning 3 tests\ntest slow ... ok\n{} — killed at the ceiling\n",
            binary.header,
            crate::findings::CEILING_KILL
        ),
        code: None,
        spawn_error: None,
        usage: None,
    };
    let (event, sample) = measured_sample(&binary, &ceiling);
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    assert_eq!((sample.wall_ms, sample.tests), (8500, Some(3)));

    let mut passed = failed(&binary, "", &[]);
    passed.ok = true;
    passed.code = Some(0);
    let end = Arc::new(AtomicBool::new(true));
    crate::exec::ends::when(Arc::clone(&end), Arc::new(AtomicBool::new(false)));
    let (event, sample) = event_and_sample(&binary, &passed, true);
    assert_eq!(event["outcome"].as_str(), Some("ok"));
    assert!(sample.is_none(), "{sample:?}");
    end.store(false, Ordering::SeqCst);
    assert_eq!(measured_sample(&binary, &passed).1.outcome, "ok");
}

/// A finished binary's event carries what its tree used, the width it ran at and the tests
/// libtest said it runs, and its history sample the same figures: 2.5 CPU-s, 0.75 s starved and
/// 1 GiB held become 2500 ms, 750 ms and 1 GiB. With no reading of its tree (every platform but
/// macOS), each usage field is `null` and its sample keeps none, never 0; with no opening line,
/// no test count. NEGATIVE CONTROL: the read run's numbers are not the unread run's nulls.
#[test]
fn a_binarys_event_and_sample_carry_its_usage_width_and_tests() {
    let binary = binary();
    let mut run = failed(&binary, "", &[("broke", "assertion failed")]);
    let (unread, sample) = measured_sample(&binary, &run);
    for field in ["cpu_s", "starved_s", "peak_bytes"] {
        assert!(unread[field].is_null(), "{field}: {unread}");
    }
    assert_eq!(unread["tests"].as_u64(), Some(2));
    assert_eq!(
        (sample.cpu_ms, sample.starved_ms, sample.peak_bytes),
        (None, None, None)
    );
    run.usage = Some(crate::exec::TreeUsage {
        cpu: Duration::from_millis(2500),
        runnable: Duration::from_secs(3),
        starved: Duration::from_millis(750),
        waited: Duration::ZERO,
        wall: Duration::from_millis(8500),
        peak_bytes: 1 << 30,
        processes: 4,
    });
    let (read, sample) = measured_sample(&binary, &run);
    assert_eq!(read["cpu_s"].as_f64(), Some(2.5));
    assert_eq!(read["starved_s"].as_f64(), Some(0.75));
    assert_eq!(read["peak_bytes"].as_u64(), Some(1 << 30));
    assert_eq!(
        (sample.cpu_ms, sample.starved_ms, sample.peak_bytes),
        (Some(2500), Some(750), Some(1 << 30))
    );
    assert_eq!(sample.outcome, "FAIL");
    run.output = "thread 'main' panicked before its harness\n".to_owned();
    let (none, sample) = measured_sample(&binary, &run);
    assert!(none["tests"].is_null() && sample.tests.is_none());
}

fn assert_combined_parity(binary: &Binary, run: &Run, event: &aterm_json::Value) {
    let combined = combined(std::slice::from_ref(binary), std::slice::from_ref(run));
    let unavailable = combined.environment_failure();
    assert_eq!(
        event["outcome"].as_str(),
        Some(if unavailable.is_some() {
            "NOT RUN"
        } else {
            "FAIL"
        })
    );
    if let Some(why) = unavailable {
        assert_eq!(event["detail"].as_str(), Some(why.as_str()));
    }
    assert!(combined.output.contains("to rerun pass `-p fixture --lib`"));
}

#[test]
fn a_passing_enospc_fixture_cannot_hide_a_direct_libtest_assertion_failure() {
    let binary = binary();
    let chatter = format!(
        "aterm: expected fixture failure: {} (os error 28)\n",
        crate::exec::ENOSPC_SENTENCE
    );
    for with_header in [false, true] {
        let mut run = failed(
            &binary,
            &chatter,
            &[(
                "real_assertion",
                "assertion failed: current_binding_is_unchanged",
            )],
        );
        if !with_header {
            run.output = run
                .output
                .strip_prefix(&format!("{}\n", binary.header))
                .unwrap()
                .to_owned();
        }
        let original = run.output.clone();
        let event = event(&binary, &run);
        assert_eq!(event["outcome"].as_str(), Some("FAIL"));
        assert!(
            event["detail"]
                .as_str()
                .unwrap()
                .starts_with("test result: FAILED.")
        );
        assert_combined_parity(&binary, &run, &event);
        assert!(run.output.contains(crate::exec::ENOSPC_SENTENCE));
        assert!(!run.output.contains("to rerun pass"));
        assert_eq!(
            run.output, original,
            "classification must not mutate the raw output"
        );
    }
}

#[test]
fn actual_full_disk_failures_are_environment_refusals_at_both_report_boundaries() {
    let binary = binary();
    let message = format!(
        "could not write fixture data: {} (os error 28)",
        crate::exec::ENOSPC_SENTENCE
    );
    for failures in [
        vec![("first", message.as_str())],
        vec![("first", message.as_str()), ("second", message.as_str())],
    ] {
        let run = failed(&binary, "", &failures);
        let event = event(&binary, &run);
        assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
        assert!(
            event["detail"]
                .as_str()
                .unwrap()
                .contains(crate::exec::ENOSPC_SENTENCE)
        );
        assert_combined_parity(&binary, &run, &event);
    }
}

#[test]
fn one_full_disk_failure_cannot_hide_another_tests_real_failure() {
    let binary = binary();
    let message = format!("write failed: {}", crate::exec::ENOSPC_SENTENCE);
    let run = failed(
        &binary,
        "",
        &[
            ("disk", &message),
            ("assertion", "assertion failed: actual != expected"),
        ],
    );
    let event = event(&binary, &run);
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    assert_combined_parity(&binary, &run, &event);
}

#[test]
fn a_direct_capability_refusal_keeps_its_scope_and_reason() {
    let binary = binary();
    let reason = "the required display service is unavailable";
    let message = format!("{} — {reason}", crate::libtest::COULD_NOT_RUN_SENTINEL);
    let run = failed(&binary, "", &[("display_needed", &message)]);
    let event = event(&binary, &run);
    assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
    let detail = event["detail"].as_str().unwrap();
    assert!(detail.contains("display_needed") && detail.contains(reason));
    assert_combined_parity(&binary, &run, &event);
}

#[test]
fn an_enospc_abort_retains_the_actual_missing_exit_code() {
    let binary = binary();
    let run = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!(
            "{}\n\nrunning 1 test\nfatal write error: {}\n",
            binary.header,
            crate::exec::ENOSPC_SENTENCE
        ),
        code: None,
        spawn_error: None,
        usage: None,
    };
    let event = event(&binary, &run);
    assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
    assert!(event["exit_code"].is_null());
    assert_combined_parity(&binary, &run, &event);
}

#[test]
fn spawn_refusal_and_success_use_the_actual_process_result() {
    let binary = binary();
    let mut run = failed(
        &binary,
        crate::exec::ENOSPC_SENTENCE,
        &[("forged", "assertion failed")],
    );
    run.code = None;
    run.spawn_error = Some("the test executable is unavailable".into());
    let refusal = event(&binary, &run);
    assert_eq!(refusal["outcome"].as_str(), Some("NOT RUN"));
    assert_eq!(refusal["detail"].as_str(), run.spawn_error.as_deref());

    run.ok = true;
    run.code = Some(0);
    run.spawn_error = None;
    let passed = event(&binary, &run);
    assert_eq!(passed["outcome"].as_str(), Some("ok"));
    assert_eq!(passed["detail"].as_str(), Some(""));
    assert_eq!(passed["exit_code"].as_i64(), Some(0));
    assert!(run.output.contains("... FAILED"));
}

#[test]
fn a_bare_panic_keeps_its_cause_without_inventing_a_case_or_owning_success() {
    let binary = binary();
    let mut run = Run {
        ok: false,
        code: Some(101),
        output: "\nthread 'main' (42) panicked at owned.rs:87:9:\noriginal readiness failure\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n".into(),
        ..Run::default()
    };
    let failed = event(&binary, &run);
    assert_eq!(failed["outcome"].as_str(), Some("FAIL"));
    let detail = failed["detail"].as_str().unwrap();
    assert!(detail.contains("owned.rs:87:9"), "{detail}");
    assert!(detail.contains("original readiness failure"), "{detail}");
    assert!(!detail.contains("RUST_BACKTRACE"), "{detail}");
    assert!(failed["failures"].as_array().unwrap().is_empty());
    assert!(detail.chars().count() <= 200);

    run.ok = true;
    run.code = Some(0);
    let passed = event(&binary, &run);
    assert_eq!(passed["outcome"].as_str(), Some("ok"));
    assert_eq!(passed["detail"].as_str(), Some(""));
    assert!(passed["failures"].as_array().unwrap().is_empty());
    assert!(
        first_error(
            "error: original compiler failure\nthread 'main' panicked at decoy.rs:1:1:\ndecoy\n"
        )
        .contains("original compiler failure")
    );
}
