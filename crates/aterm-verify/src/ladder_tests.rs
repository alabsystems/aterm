// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_skip_is_counted_and_named() {
    let mut a = Report::new("a");
    a.skip("targo test (no targo)");
    a.pass("something real");
    let mut b = Report::new("b");
    b.skip("gui smoke (macOS only)");
    b.fail("a finding");
    b.cannot_run("no driver");
    b.raw("  NOTICE: raw text is never a decision");

    let t = tally(&[a, b]);
    assert_eq!(t.skipped(), 2);
    assert_eq!(t.skips, ["targo test (no targo)", "gui smoke (macOS only)"]);
    assert_eq!(t.gate_failures, ["a finding"]);
    assert_eq!(t.could_not_run, ["no driver"]);
    assert!(t.failed());
}

/// An oracle that decided nothing and printed no reason still names one on its row: how it
/// ended (LR-6). CONTROL: [`Report::record_child`], which the oracles took before, leaves the
/// bare label; a named cause and a pass keep theirs.
#[test]
fn an_oracle_that_decided_nothing_names_how_it_ended_on_its_row() {
    let run = |code: Option<i32>| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: code == Some(0),
        output: "== x86_64-pc-windows-msvc\n".into(),
        code,
        spawn_error: None,
        usage: None,
    };
    let row = |run: &Run, outcome: Outcome, named: Option<&str>| {
        let mut r = Report::new("cells");
        r.record_oracle(run, outcome, "win32 oracle", named.map(str::to_string));
        r.outcomes()
            .map(|(o, l)| (o, l.to_string()))
            .collect::<Vec<_>>()
    };
    let nr = Outcome::Fail(Severity::CouldNotRun);
    for code in [None, Some(3)] {
        assert_eq!(
            row(&run(code), nr, None),
            [(nr, format!("win32 oracle — {}", run(code).unnamed_cause()))]
        );
    }
    assert!(run(Some(3)).unnamed_cause().contains(" 3 "));
    assert_ne!(run(None).unnamed_cause(), run(Some(3)).unnamed_cause());
    assert_eq!(
        row(&run(Some(3)), nr, Some("no cached windows-sys0.61.2")),
        [(nr, "win32 oracle — no cached windows-sys0.61.2".to_string())]
    );
    assert_eq!(
        row(&run(Some(0)), Outcome::Ok, None),
        [(Outcome::Ok, "win32 oracle".to_string())]
    );
    let mut old = Report::new("cells");
    old.record_child(&run(None), nr, "win32 oracle");
    assert_eq!(
        old.outcomes().collect::<Vec<_>>(),
        [(nr, "win32 oracle")],
        "negative control: the bare row this replaces"
    );
}

#[test]
fn a_child_that_died_of_the_machine_is_could_not_run_and_one_that_failed_is_a_finding() {
    let run = |ok: bool, output: &str, spawn: Option<&str>| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok,
        output: output.into(),
        code: None,
        spawn_error: spawn.map(str::to_string),
        usage: None,
    };
    let noise = "ERROR logger: auxiliary checksum unavailable\n".repeat(32);
    let mut r = Report::new("test (--workspace)");
    r.decide_child(&run(true, "", None), "passed");
    r.decide_child(
        &run(
            false,
            &format!("{noise}error[E0308]: mismatched types"),
            None,
        ),
        "a finding",
    );
    r.decide_child(
        &run(
            false,
            "aterm-verify: cannot run /s2/targo: No space left on device (os error 28)",
            Some("No space left on device (os error 28)"),
        ),
        "never spawned",
    );
    r.fail_child(
        &run(
            false,
            &format!(
                "{noise}error: failed to write lib.rmeta: No space left on device (os error 28)"
            ),
            None,
        ),
        "starved",
    );
    let t = tally(std::slice::from_ref(&r));
    assert_eq!(t.gate_failures, ["a finding"]);
    assert_eq!(t.could_not_run.len(), 2, "never spawned, starved: {t:?}");
    let found = r.findings();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "a finding");
    assert_eq!(found[0].message, ["error[E0308]: mismatched types"]);
}

/// One plain failure beside the refusals makes the test child a finding:
/// that failure alone, and the refusals beside it a COULD NOT RUN row.
#[test]
fn a_test_child_whose_every_failure_is_a_refusal_is_could_not_run() {
    let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
    let log = |second: &str| {
        format!(
            "     Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-1)\n\n\
             running 2 tests\ntest a ... FAILED\ntest b ... FAILED\n\nfailures:\n\n\
             ---- a stdout ----\n{sentinel} — strays alive\n\n\
             ---- b stdout ----\n{second}\n\nfailures:\n    a\n    b\n\n\
             test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered \
             out; finished in 1.00s\n\nerror: test failed, to rerun pass `-p x --test \
             bridge_e2e`\n"
        )
    };
    let run = |output: String| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output,
        code: Some(101),
        spawn_error: None,
        usage: None,
    };
    let mut r = Report::new("test (--workspace)");
    r.decide_child(&run(log(&format!("{sentinel} — strays alive"))), "refused");
    r.decide_test_child(
        &run(log("assertion failed: a finding")),
        "found",
        &findings::Rerun::default(),
    );
    let t = tally(std::slice::from_ref(&r));
    assert_eq!(t.gate_failures, ["found"]);
    assert_eq!(t.could_not_run.len(), 2, "{t:?}");
    assert_eq!(
        t.could_not_run[1],
        "found — 1 of its failing tests refused the machine, not the tree (a) — strays alive",
        "the refusal named with what it said: {t:?}"
    );
    let ids: Vec<String> = r.findings().into_iter().map(|f| f.id).collect();
    assert_eq!(
        ids,
        ["-p x --test bridge_e2e -- b"],
        "the refusal is no finding"
    );
}
