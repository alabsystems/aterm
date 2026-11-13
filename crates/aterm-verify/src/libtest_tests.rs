// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEADER: &str = "     Running unittests src/lib.rs (target/debug/deps/x-abc)\n";

/// A binary's test count is its first opening line's, `running 1 test` or `running 133
/// tests`, coloured or not: a nested run a test prints later is not the binary's, nor is a
/// line that only begins or ends the same way. A log with no opening line (the binary died
/// before its harness) has none. NEGATIVE CONTROL: read from its last opening line, the
/// nested run's 1 answers for the binary's 133.
#[test]
fn a_binarys_test_count_is_its_first_opening_line() {
    let log = format!(
        "{HEADER}\nrunning 133 tests\ntest a ... ok\n  running 7 tests\nrunning 1 test\n\
         test result: ok. 133 passed\n"
    );
    assert_eq!(tests_run(&log), Some(133));
    assert_eq!(
        tests_run("\u{1b}[0mrunning 1 test\u{1b}[0m\r\ntest a ... ok\n"),
        Some(1)
    );
    for none in [
        "",
        "running tests\n",
        "running 4 testers\n",
        "warming 4 tests\n",
        "running -1 tests\n",
        "running 99999999999999999999999 tests\n",
        "thread 'main' panicked\n",
    ] {
        assert_eq!(tests_run(none), None, "{none:?}");
    }
}

#[test]
fn completed_counts_distinguish_passed_ignored_and_measured_cases() {
    for (verdict, passed, failed, ignored, measured) in [
        ("ok", 2, 0, 1, 0),
        ("FAILED", 1, 1, 1, 0),
        ("ok", 1, 0, 1, 1),
        ("ok", 0, 0, 0, 0),
    ] {
        let announced = passed + failed + ignored + measured;
        let log = format!(
            "running {announced} tests\r\n\x1b[32mtest result: {verdict}. {passed} passed; {failed} failed; {ignored} ignored; {measured} measured; 7 filtered out; finished in 0.01s\x1b[0m\r\n"
        );
        assert_eq!(
            test_counts(&log),
            Some(TestCounts {
                passed,
                failed,
                ignored,
                measured,
                filtered_out: 7,
            })
        );
    }
}

#[test]
fn incomplete_or_inconsistent_counts_stay_unknown() {
    let valid = "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    assert_eq!(
        test_counts(valid),
        None,
        "a summary without a harness banner"
    );
    for summary in [
        "",
        "test result: ok. 3 passed\n",
        "test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "test result: ok. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "test result: FAILED. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "test result: ok. -1 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "test result: ok. 18446744073709551615 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n",
    ] {
        assert_eq!(
            test_counts(&format!("running 3 tests\n{summary}")),
            None,
            "{summary}"
        );
    }
    assert_eq!(
        test_counts(&format!("running 3 tests\n{valid}test result: malformed\n")),
        None,
        "an earlier summary cannot fill in a malformed terminal summary"
    );
}

#[test]
fn completed_counts_use_the_terminal_summary_not_an_earlier_nested_one() {
    let log = "running 3 tests\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.02s\n";
    let counts = test_counts(log).unwrap();
    assert_eq!(
        (tests_run(log), counts.passed, counts.ignored),
        (Some(3), 2, 1)
    );
}

/// Each binary is read on its own, coloured or not: a target whose rows were
/// all ignored or filtered out passed none even beside one that passed, a
/// re-exec child's result is not its parent's, and a binary with no result
/// line is `None`.
#[test]
fn passed_is_read_per_target_never_summed() {
    let line = |verdict: &str, passed: usize, ignored: usize| {
        format!(
            "test result: {verdict}. {passed} passed; 0 failed; {ignored} ignored; 0 measured; \
             0 filtered out; finished in 0.01s\n"
        )
    };
    let paint = "     Running tests/paint.rs (target/debug/deps/paint-1a)\n";
    let spin = "     Running tests/spin.rs (target/debug/deps/spin-2b)\n";
    let t = |name: &str, n: Option<usize>| (name.to_string(), n);
    let one_empty = format!("{paint}{}{spin}{}", line("ok", 0, 23), line("ok", 2, 0));
    assert_eq!(
        passed_by_target(&one_empty),
        [t("tests/paint.rs", Some(0)), t("tests/spin.rs", Some(2))]
    );
    let child = format!(
        "{paint}running 1 test\n{}{}",
        line("ok", 1, 0),
        line("FAILED", 0, 0)
    );
    assert_eq!(passed_by_target(&child), [t("tests/paint.rs", Some(0))]);
    assert_eq!(
        passed_by_target(&format!("{HEADER}running 2 tests\n")),
        [t("unittests src/lib.rs", None)]
    );
    assert_eq!(
        passed_by_target(&format!(
            "{spin}test result: \x1b[32mok\x1b[0m. 3 passed; 0 failed; 0 ignored\n"
        )),
        [t("tests/spin.rs", Some(3))]
    );
    assert!(passed_by_target("").is_empty());
}

/// One failing test binary as cargo and libtest print it under
/// `--no-fail-fast`: each `(name, output)` fails with that captured output.
fn failed_binary(target: &str, failures: &[(&str, &str)]) -> String {
    let mut s =
        format!("     Running {target} (target/debug/deps/{target}-abc)\n\nrunning 3 tests\n");
    for (name, _) in failures {
        s.push_str(&format!("test {name} ... FAILED\n"));
    }
    s.push_str("test passes ... ok\n\nfailures:\n\n");
    for (name, out) in failures {
        s.push_str(&format!("---- {name} stdout ----\n{out}\n\n"));
    }
    s.push_str("failures:\n");
    for (name, _) in failures {
        s.push_str(&format!("    {name}\n"));
    }
    s.push_str(&format!(
        "\ntest result: FAILED. 1 passed; {} failed; 0 ignored; 0 measured; 0 filtered out; \
         finished in 3.83s\n\nerror: test failed, to rerun pass `-p x --test {target}`\n",
        failures.len()
    ));
    s
}

#[test]
fn a_full_disk_a_passing_test_simulated_does_not_hide_a_failed_test() {
    let run = |output: String| crate::exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output,
        code: Some(101),
        spawn_error: None,
        usage: None,
    };
    let simulated = format!(
        "     Running link (target/debug/deps/link-abc)\n\nrunning 1 test\n\
         aterm-link: post 1 reservation refused, left queued: {} (os error 28)\n\
         test sim::full ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; \
         0 filtered out; finished in 0.10s\n\n",
        crate::exec::ENOSPC_SENTENCE
    );
    let red = failed_binary(
        "db",
        &[(
            "round",
            "thread 'round' panicked at src/a.rs:1:1:\nassertion failed",
        )],
    );
    assert_eq!(
        run(format!("{simulated}{red}")).environment_failure(),
        None,
        "a finding"
    );
    let full = failed_binary(
        "db",
        &[(
            "round",
            &format!(
                "thread 'round' panicked at src/a.rs:1:1:\n{}",
                crate::exec::ENOSPC_SENTENCE
            ),
        )],
    );
    assert!(
        run(full)
            .environment_failure()
            .is_some_and(|why| why.contains("ran out of disk")),
        "each failed test's own block names it"
    );
    assert!(
        run(format!(
            "error: failed to write target/x: {}",
            crate::exec::ENOSPC_SENTENCE
        ))
        .environment_failure()
        .is_some(),
        "a build that died of it"
    );
    let killed = "     Running unittests src/lib.rs (target/debug/deps/shell-abc)\n\n\
                  running 133 tests\ntest tests::one ... ok\n\
                  error: test failed, to rerun pass `-p aterm-shell-integration --lib`\n\n\
                  Caused by:\n  process didn't exit successfully: `shell-abc` (killed by a signal)\n";
    assert_eq!(
        run(format!("{simulated}{killed}")).environment_failure(),
        None,
        "a passing binary's simulated full disk never hides a later signal death"
    );
    let timed_out = format!(
        "{simulated}     Running unittests src/lib.rs (target/debug/deps/shell-abc)\n\n\
         running 133 tests\n{} — child killed after 10800.0s of work, over the 10800.0s \
         ceiling (--timeout <seconds|off>)\n  child: shell-abc\n",
        crate::findings::CEILING_KILL
    );
    assert_eq!(
        run(timed_out).environment_failure(),
        None,
        "a ceiling kill is always a finding, never out-of-disk"
    );
    let crashed_enospc = format!(
        "     Running tests/io.rs (target/debug/deps/io-abc)\n\nrunning 1 test\n\
         fatal write error: {}\nerror: test failed, to rerun pass `-p x --test io`\n\n\
         Caused by:\n  process didn't exit successfully: `io-abc` (signal: 6, SIGABRT)\n",
        crate::exec::ENOSPC_SENTENCE
    );
    assert!(
        run(format!("{simulated}{crashed_enospc}"))
            .environment_failure()
            .is_some_and(|why| why.contains("ran out of disk")),
        "a binary that genuinely aborted on ENOSPC still reports out-of-disk"
    );
    assert_eq!(
        run(format!("{simulated}{red}{crashed_enospc}")).environment_failure(),
        None,
        "a plain test failure beside an ENOSPC crash stays a finding"
    );
    assert_eq!(
        run(format!(
            "{simulated}   Compiling y v0.1.0\nerror[E0308]: mismatched types\n"
        ))
        .environment_failure(),
        None,
        "a later compile error after a passing simulated-ENOSPC binary stays a finding"
    );
    assert!(
        run(format!(
            "{simulated}   Compiling y v0.1.0\nerror: failed to write target/y: {}\n",
            crate::exec::ENOSPC_SENTENCE
        ))
        .environment_failure()
        .is_some_and(|why| why.contains("ran out of disk")),
        "a later compile error that itself hit ENOSPC reports out-of-disk"
    );
    let custom_ok = "     Running tests/custom.rs (target/debug/deps/custom-abc)\n";
    assert!(
        run(format!("{custom_ok}{crashed_enospc}"))
            .environment_failure()
            .is_some_and(|why| why.contains("ran out of disk")),
        "a passing harness=false binary before an ENOSPC crash still reports out-of-disk"
    );
    assert_eq!(
        run(format!("{simulated}{custom_ok}")).environment_failure(),
        None,
        "an unfinished harness=false binary after a passing simulated-ENOSPC binary stays a finding"
    );
}

fn refused(why: &str) -> String {
    format!(
        "\nthread 't' panicked at crates/x/tests/harness/mod.rs:177:5:\n\
         {COULD_NOT_RUN_SENTINEL} — {why}\nnote: run with `RUST_BACKTRACE=1`"
    )
}

/// Every failure, across two binaries, carries the sentinel: the child is an
/// environment refusal, named by the tests that refused.
#[test]
fn a_log_whose_every_failure_carries_the_sentinel_is_the_machines() {
    let log = format!(
        "   Compiling x v0.1.0\n{}{}     Running tests/ok.rs (target/debug/deps/ok-1)\n\n\
         running 1 test\ntest fine ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; \
         0 measured; 0 filtered out; finished in 0.01s\n\nerror: 2 targets failed:\n    \
         `-p x --test service_e2e`\n    `-p x --test paint`\n",
        failed_binary(
            "service_e2e",
            &[("starts_the_service", &refused("a stale peer is alive"))]
        ),
        failed_binary(
            "paint",
            &[
                ("measuring::row_one", &refused("take unproved")),
                ("measuring::row_two", &refused("take unproved")),
            ]
        ),
    );
    assert_eq!(
        environment_refusals(&log),
        Some(vec![
            "measuring::row_one".to_string(),
            "measuring::row_two".to_string(),
            "starts_the_service".to_string(),
        ])
    );
    assert_eq!(
        refusal_reasons(&log),
        ["a stale peer is alive", "take unproved"],
        "each distinct why once, in log order"
    );
    let colon = format!("{COULD_NOT_RUN_SENTINEL}: node could not start; install node\n");
    assert_eq!(
        refusal_reasons(&format!("\x1b[31m{colon}\x1b[0m{colon}")),
        ["node could not start; install node"]
    );
}

/// A test that re-runs itself alone in a child (aterm-render's
/// `alone::in_own_process`) re-raises the child's refusal with the child's
/// why on its own sentinel line and the child's stderr below it: one
/// refusal, so the row gives that why once, in full, and no count.
#[test]
fn a_reraised_refusal_is_one_reason_the_childs() {
    let name = "seal_maps_discovered_faces::the_seal_maps_every_discovered_face";
    let why = "this host has none of the chain/symbol/emoji candidate files";
    let child = format!(
        "\nthread '{name}' (48286027) panicked at tests/seal_maps_discovered_faces.rs:93:9:\n\
         {COULD_NOT_RUN_SENTINEL} — {why}\nnote: run with `RUST_BACKTRACE=1`\n"
    );
    let row = |first_line: &str| {
        let block = format!(
            "\nthread '{name}' (48286024) panicked at tests/common/alone.rs:63:9:\n\
             {COULD_NOT_RUN_SENTINEL} — {first_line}\n{child}"
        );
        let log = failed_binary("render_suite", &[(name, &block)]);
        let run = crate::exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: false,
            output: log.clone(),
            code: Some(101),
            spawn_error: None,
            usage: None,
        };
        (refusal_reasons(&log), run.environment_failure())
    };

    let (reasons, failure) = row(&format!("{why}\n({name}, alone in its own process)"));
    assert_eq!(reasons, [why], "the child's why, once");
    let failure = failure.expect("the machine's, not a finding");
    assert!(
        failure.ends_with(&format!("({name}) — {why}")),
        "the row names the child's why in full, with no count: {failure}"
    );

    let (reasons, failure) = row(&format!("{name}, alone in its own process:"));
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(
        failure
            .expect("still the machine's")
            .contains("alone in its own process: (the first of 2 reasons"),
        "a wrapper's own line first hides the why"
    );
}

/// A refusal never launders a finding: whatever the sentinel cannot vouch
/// for keeps the whole child a finding.
#[test]
fn a_refusal_never_launders_a_finding() {
    let finding = "thread 't' panicked at x.rs:1:1:\nassertion failed: service.ready()";
    let refusal = failed_binary(
        "service_e2e",
        &[("starts_the_service", &refused("a stale peer is alive"))],
    );
    let crash = "     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 test\n\
                 error: test failed, to rerun pass `-p x --test crash`\n\nCaused by:\n  \
                 process didn't exit successfully: `crash-1` (signal: 11, SIGSEGV: invalid \
                 memory reference)\n";
    let cases = [
        (
            "one plain failure beside a refusal",
            failed_binary(
                "paint",
                &[
                    ("measuring::row_one", &refused("take unproved")),
                    (
                        "measuring::row_two",
                        "thread 't' panicked at x.rs:1:1:\nPAINT CONFORMANCE FAILED [prompt]",
                    ),
                ],
            ),
        ),
        (
            "a plain failure alone",
            failed_binary("x", &[("t", "thread 't' panicked at x.rs:1:1:\nassert")]),
        ),
        (
            "a finding sharing a refusal's name in a later binary",
            format!(
                "{refusal}{}",
                failed_binary("service_on", &[("starts_the_service", finding)])
            ),
        ),
        (
            "a finding sharing a refusal's name in an earlier binary",
            format!(
                "{}{refusal}",
                failed_binary("service_on", &[("starts_the_service", finding)])
            ),
        ),
        (
            "a binary that died without a result",
            format!("{refusal}{crash}"),
        ),
        (
            "a binary cut off mid-run with no cargo trailer",
            format!(
                "{refusal}     Running unittests src/lib.rs (target/debug/deps/shell-1)\n\n\
                 running 133 tests\ntest tests::one ... ok\n"
            ),
        ),
        (
            "a ceiling kill after a refusal",
            format!(
                "{refusal}{} — child killed after 10800.0s of work\n",
                crate::findings::CEILING_KILL
            ),
        ),
        (
            "a listed failure with no block",
            refusal.replace(
                "failures:\n    starts_the_service\n",
                "failures:\n    starts_the_service\n    silent_one\n",
            ),
        ),
        ("no failure at all", "running 0 tests\n".to_string()),
        ("an empty log", String::new()),
    ];
    for (case, log) in &cases {
        assert_eq!(environment_refusals(log), None, "{case}:\n{log}");
    }
    let both = format!(
        "{refusal}{}",
        failed_binary(
            "service_on",
            &[("starts_the_service", &refused("a stale peer is alive"))]
        )
    );
    assert_eq!(
        environment_refusals(&both),
        Some(vec!["starts_the_service".to_string()])
    );
}

/// Each failure is named with its binary's re-run spec and its own block, the
/// refusal is marked, and two binaries failing a test of the same bare name
/// stay two failures with two specs.
#[test]
fn a_failed_test_log_names_each_failure_with_its_binarys_rerun_spec() {
    let log = format!(
        "   Compiling x v0.1.0\n{}{}\x1b[1merror\x1b[0m: 2 targets failed:\n    \
         `-p x --test probe`\n    `-p x --test world`\n",
        failed_binary(
            "probe",
            &[
                (
                    "index_probe_two",
                    "\nthread 'index_probe_two' panicked at crates/x/tests/probe.rs:9:5:\nlock held",
                ),
                ("shared_name", "\nplain failure"),
            ]
        ),
        failed_binary(
            "service",
            &[("shared_name", &refused("a stale peer is alive"))]
        ),
    );
    let got = failed_tests(&log).expect("every failure is accounted for");
    let seen: Vec<(&str, &str, bool)> = got
        .iter()
        .map(|t| (t.spec.as_str(), t.name.as_str(), t.refused))
        .collect();
    assert_eq!(
        seen,
        [
            ("-p x --test probe", "index_probe_two", false),
            ("-p x --test probe", "shared_name", false),
            ("-p x --test service", "shared_name", true),
        ]
    );
    assert!(got[0].block.contains("lock held"), "{:?}", got[0]);
    assert!(!got[0].block.contains("plain failure"), "{:?}", got[0]);
    assert!(got[1].block.contains("plain failure"), "{:?}", got[1]);
}

/// A log that cannot account for every failure itemizes nothing, so a
/// failure the list would miss is never dropped from a comparison.
#[test]
fn a_log_that_cannot_account_for_every_failure_itemizes_nothing() {
    let one = failed_binary("probe", &[("a", "\nboom")]);
    assert!(failed_tests(&one).is_some(), "the control: {one}");
    let nested = failed_binary("inner", &[("x", "\ninner boom")]);
    let quotes = |msg: &str| failed_binary("outer", &[("quotes", msg)]);
    let mut cases: Vec<(&str, String)> = vec![
        (
            "a binary that died of a signal",
            format!(
                "{one}     Running tests/crash.rs (target/debug/deps/crash-1)\n\nrunning 1 \
                 test\nerror: test failed, to rerun pass `-p x --test crash`\n\nCaused \
                 by:\n  process didn't exit successfully: `crash-1` (signal: 11, SIGSEGV)\n"
            ),
        ),
        (
            "a listed failure with no block",
            one.replace("failures:\n    a\n", "failures:\n    a\n    b\n"),
        ),
        (
            "a block no binary claims",
            one.replace(
                "error: test failed, to rerun pass `-p x --test probe`\n",
                "",
            ),
        ),
        ("no failure", "running 0 tests\n".to_string()),
        ("an empty log", String::new()),
        (
            "a quoted nested binary",
            quotes(&format!(
                "\nthread 'quotes' panicked at src/a.rs:1:1:\nladder:\n{nested}tail of the message"
            )),
        ),
        (
            "a quoted nested tail",
            quotes(
                "\nthread 'quotes' panicked at src/a.rs:1:1:\ntest result: FAILED. 0 passed; \
                 1 failed\nerror: test failed, to rerun pass `-p inner --lib`\nrest",
            ),
        ),
    ];
    for list in ["  case_07: got 3 want 4", "    case_07"] {
        let msg = format!(
            "\nthread 'corpus' panicked at src/a.rs:1:1:\nthe corpus disagreed\nfailures:\n{list}"
        );
        cases.push((
            "a failures: line in the last block",
            failed_binary("oracle", &[("corpus", &msg)]),
        ));
        cases.push((
            "a failures: line in the first block",
            failed_binary("oracle", &[("corpus", &msg), ("other", "\nboom")]),
        ));
    }
    let refusing = failed_binary("render", &[("t", &refused("a stale peer is alive"))]);
    assert!(environment_refusals(&refusing).is_some(), "{refusing}");
    for why in [
        "(signal: 11, SIGSEGV: invalid memory reference)",
        "(killed by a signal)",
        "(exit status: 134)",
    ] {
        let tail = format!(
            "\nCaused by:\n  process didn't exit successfully: `target/debug/deps/render-abc` \
             {why}\n"
        );
        assert_eq!(
            environment_refusals(&format!("{refusing}{tail}")),
            None,
            "{why}"
        );
        cases.push((
            "a binary that died after its result",
            format!("{one}{tail}"),
        ));
    }
    for (case, log) in &cases {
        assert_eq!(failed_tests(log), None, "{case}:\n{log}");
    }
    let plain = quotes("\nthread 'quotes' panicked\nrest");
    assert_eq!(failed_tests(&plain).map(|t| t.len()), Some(1));
    let oracle = failed_binary(
        "oracle",
        &[("corpus", "\nthe corpus disagreed\n  case_07: got 3 want 4")],
    );
    let got = failed_tests(&oracle).expect("accounted for");
    assert!(got[0].block.contains("case_07"), "{:?}", got[0].block);
}

#[test]
fn out_of_disk_never_masks_trailing_compile_errors_after_custom_or_enospc_binaries() {
    let custom_then_compile = "\
 Running tests/custom_harness.rs (target/debug/deps/custom-1)\n\
simulated fault injection: No space left on device (os error 28)\n\
Compiling broken v0.1.0 (/Users//example/aterm/crates/broken)\n\
error[E0425]: cannot find value `missing` in this scope\n\
error: could not compile `broken` (test \"broken\") due to 1 previous error\n";
    assert!(
        !out_of_disk(custom_then_compile),
        "passing custom-harness simulated ENOSPC must not mask a later compile error"
    );

    let enospc_then_pass_then_compile = format!(
        "{}     Running unittests src/lib.rs (target/debug/deps/link-1)\n\n\
         running 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 failed; \
         0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n\
            Documenting broken v0.1.0 (/Users//example/aterm/crates/broken)\n\
         error[E0425]: cannot find value `missing` in this scope\n\
         error: could not compile `broken` (doc) due to 1 previous error\n",
        failed_binary(
            "writer",
            &[(
                "disk_full",
                "thread 'disk_full' panicked:\nNo space left on device"
            )],
        ),
    );
    assert!(
        !out_of_disk(&enospc_then_pass_then_compile),
        "trailing non-ENOSPC compile failure must not be hidden behind an earlier ENOSPC binary"
    );

    let custom_enospc_fail = "\
 Running tests/custom_harness.rs (target/debug/deps/custom-1)\n\
fatal: No space left on device (os error 28)\n\
error: test failed, to rerun pass `-p x --test custom_harness`\n\n\
Caused by:\n\
process didn't exit successfully: `custom-1` (exit status: 1)\n";
    assert!(
        out_of_disk(custom_enospc_fail),
        "genuinely failing custom-harness binary reporting ENOSPC still classifies as out_of_disk"
    );
}

/// A CRASH AFTER A PLAIN FAILURE IS THAT FAILURE: a binary whose case failed with a block that
/// shows no full disk, and which then died of one, reads FAIL, as the same binary does when it
/// ends with libtest's report ([`out_of_disk`]'s abnormal-exit fallback reads each block with
/// [`marks`]). NEGATIVE CONTROL: with the plain block's panic itself out of disk, the crash's
/// full disk is the binary's, and it reads NOT RUN.
#[test]
fn a_plain_failure_followed_by_an_enospc_crash_reads_fail() {
    let log = |block: &str| {
        format!(
            "{HEADER}\nrunning 2 tests\ntest a::plain ... FAILED\n\nfailures:\n\n\
             ---- a::plain stdout ----\n{block}\n\nfailures:\n    a::plain\n\
             fatal write error: {} (os error 28)\n\
             error: test failed, to rerun pass `-p x --lib`\n\nCaused by:\n  process \
             didn't exit successfully: `x-abc` (signal: 6, SIGABRT)\n",
            crate::exec::ENOSPC_SENTENCE
        )
    };
    let plain = log("\nthread 'a::plain' panicked at src/a.rs:1:1:\nassertion failed");
    assert!(!out_of_disk(&plain), "{plain}");
    let full = log(&format!(
        "\nthread 'a::plain' panicked at src/a.rs:1:1:\n{} (os error 28)",
        crate::exec::ENOSPC_SENTENCE
    ));
    assert!(out_of_disk(&full), "{full}");
}

/// ONE RULE FOR A BLOCK: [`marks`] reads a sentinel and a full disk alike through ANSI, and
/// [`decisive`] holds once neither every-refused nor every-out-of-disk can hold. NEGATIVE
/// CONTROL: no case seen decides nothing.
#[test]
fn a_block_has_one_reading_and_decisiveness_follows_it() {
    let refused = marks(&format!(
        "\x1b[31m{COULD_NOT_RUN_SENTINEL}\x1b[0m — no fish"
    ));
    assert_eq!(
        refused,
        Marks {
            refused: true,
            disk: false
        }
    );
    let disk = marks(&format!(
        "panicked: {} (os error 28)",
        crate::exec::ENOSPC_SENTENCE
    ));
    assert_eq!(
        disk,
        Marks {
            refused: false,
            disk: true
        }
    );
    let plain = marks("assertion failed");
    assert!(!decisive(&[]));
    assert!(decisive(&[plain]));
    assert!(!decisive(&[refused]));
    assert!(!decisive(&[disk, disk]));
    assert!(decisive(&[refused, disk]));
    let both = Marks {
        refused: true,
        disk: true,
    };
    assert!(!decisive(&[both, refused]));
    assert!(!decisive(&[both, disk]));
}
