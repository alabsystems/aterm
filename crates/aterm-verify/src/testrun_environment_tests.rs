// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn binary() -> Binary {
    Binary {
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
        ok: false,
        output,
        code: Some(101),
        spawn_error: None,
    }
}

fn event(binary: &Binary, run: &Run) -> aterm_json::Value {
    let line = binary_event(
        binary,
        run,
        Duration::from_millis(1250),
        Duration::from_millis(8500),
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["schema"].as_u64(), Some(1));
    assert_eq!(event["event"].as_str(), Some("test-binary-finished"));
    assert_eq!(event["test"].as_str(), Some(binary.spec.as_str()));
    assert_eq!(event["exit_code"].as_i64(), run.code.map(i64::from));
    assert_eq!(event["waiting_s"].as_f64(), Some(1.25));
    assert_eq!(event["execution_s"].as_f64(), Some(8.5));
    event
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
        ok: false,
        output: format!(
            "{}\n\nrunning 1 test\nfatal write error: {}\n",
            binary.header,
            crate::exec::ENOSPC_SENTENCE
        ),
        code: None,
        spawn_error: None,
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
