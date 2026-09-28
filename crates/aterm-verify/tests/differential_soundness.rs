// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DIFFERENTIAL VERDICT NEVER EXCUSES A NEW RED (2026-09-27, second
//! review). An adversarial pass over the differential gate found sixteen ways
//! a branch with a failure main did not have — or a red main has had past
//! the age cap — was judged INHERITED, with the merge contract claimed and
//! exit 0. Each test here is one of them, built the way the gate builds it
//! (the test stage's combined log, a row's child output, real receipts and
//! real git), and asserts the gate now BLOCKS it. A control beside most says
//! what must still be inherited, so a fix that blocks everything cannot pass.
//!
//! A SECOND ROUND (2026-09-27, third review) found the gate never asked WHAT
//! judged main's reds — a base made by another trustc, another `ty` or other
//! compile flags was a base (B1-B3, B12) — that a branch judged by the
//! absolute rule restarted main's clocks in the receipt that becomes the next
//! base once it lands (B4), that a smoke's poll which gave up, or a client
//! that answered nothing, read as main's flake (B6), that a newline in a
//! receipt value forged a key (B8), that a test child which ran no test —
//! an ambient runner outranking the gate's — was a pass (B9), and that a slow
//! clock judging an old receipt still read ages short (B11): the `b…` cases
//! below. Three more live beside the runs they need: a compile red main has
//! hid the test run it prevented (B10, `gate_contract`'s
//! `an_excused_compile_red_never_excuses_the_tests_it_kept_from_running`), a
//! module named `…measuring` left the merge contract through libtest's
//! substring `--skip` (B13, `stages::every_skipped_module_is_declared_where_its_stage_looks`),
//! and a verdict nobody read was still filed as a receipt (B7,
//! `environment_contract`'s
//! `a_verdict_that_could_not_be_written_files_no_receipt`).
//!
//! A THIRD ROUND (2026-09-27, fourth review) found what a red kept from
//! running still excused: a whole row is the same output wherever its check
//! stopped, so only a check that ran to its end is inherited now. Its cases
//! live beside the runs they need — the drives behind a failed build (P1, P2,
//! P3) and a suite that stops at its first failure (P5) in `gate_contract`,
//! a lint red in a library (P4) and the build environment in
//! `differential`'s own tests — and two here: a checker at another store
//! build (C4, in `b2_b3_…`) and main's oldest clock across a toolchain switch
//! (C6).
//!
//! UNIX-PINNED at the target, as `environment_contract.rs` is: the git-backed
//! cases drive real repositories through a shell-less `git`, and the logs are
//! cargo's on a unix host.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_verify::differential::{self, Against, BaseReds, Disposition, NewWhy, Plan, Tools};
use aterm_verify::exec::Run;
use aterm_verify::ladder::{Finding, Report, Tally, tally};
use aterm_verify::receipt::{self, Failure, Receipt};
use aterm_verify::testrun::{Binary, Record, combined};
use aterm_verify::verdict::verdict_against;
use aterm_verify::{Mode, Scope};

const LABEL: &str = "targo test --workspace --tests";
const NOW: u64 = 1_790_000_000;

fn bin(name: &str) -> Binary {
    Binary {
        header: format!("     Running tests/{name}.rs (target/debug/deps/{name}-1)"),
        spec: format!("-p x --test {name}"),
        record: Record {
            cwd: PathBuf::from("/r/crates/x"),
            argv: vec![format!("/r/target/debug/deps/{name}-1").into()],
            env: vec![("CARGO_PKG_NAME".into(), "x".into())],
        },
        alone: None,
    }
}

/// One binary's own log: `failures` fail with their captured blocks.
fn binary_log(b: &Binary, failures: &[(&str, &str)]) -> String {
    let mut s = format!("{}\n\nrunning {} tests\n", b.header, failures.len() + 1);
    for (n, _) in failures {
        s.push_str(&format!("test {n} ... FAILED\n"));
    }
    s.push_str("test passes ... ok\n\nfailures:\n\n");
    for (n, block) in failures {
        s.push_str(&format!("---- {n} stdout ----\n{block}\n\n"));
    }
    s.push_str("failures:\n");
    for (n, _) in failures {
        s.push_str(&format!("    {n}\n"));
    }
    s.push_str(&format!(
        "\ntest result: FAILED. 1 passed; {} failed; 0 ignored; 0 measured; 0 filtered out; \
         finished in 0.42s\n\n",
        failures.len()
    ));
    s
}

/// A binary whose one test passed.
fn green_log(b: &Binary) -> String {
    format!(
        "{}\n\nrunning 1 test\ntest ok ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 \
         ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        b.header
    )
}

fn run_of(output: String, code: Option<i32>) -> Run {
    Run {
        ok: code == Some(0),
        output,
        code,
        spawn_error: None,
    }
}

/// The test stage's row for these binaries' runs, as the gate builds it.
fn test_report(bins: &[Binary], runs: &[Run]) -> Report {
    let all = combined(bins, runs);
    let mut r = Report::new("test run (--workspace)");
    r.raw(all.output.as_str());
    r.decide_test_child(&all, LABEL);
    r
}

/// MAIN's reds from main's report, as a baseline records them (red for an
/// hour), and the BRANCH's verdict judged against them.
fn judge_branch(main: &Report, branch: &Report) -> aterm_verify::Verdict {
    let t_main = tally(std::slice::from_ref(main));
    assert!(
        t_main.failed(),
        "main must be red for the case to mean anything"
    );
    let failures = differential::recorded_failures(&t_main, None, &"a".repeat(40), NOW - 3600);
    let base = BaseReds {
        commit: "a".repeat(40),
        source: "receipt aaaaaaaaa".into(),
        failures,
        now: NOW,
    };
    let t_branch = tally(std::slice::from_ref(branch));
    assert!(t_branch.failed(), "the branch is red");
    verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &t_branch,
        &Against::Base(base),
    )
}

/// The gate blocked it: no merge contract, exit 1.
#[track_caller]
fn blocks(v: &aterm_verify::Verdict) {
    assert!(
        !v.claims_merge_contract && v.exit == aterm_verify::exit::FAILED,
        "the gate excused a new red:\n{}",
        v.text
    );
    assert!(v.text.contains("1 new, 0 inherited"), "{}", v.text);
}

/// The gate inherited it: nothing new, exit 0.
#[track_caller]
fn inherits(v: &aterm_verify::Verdict) {
    assert_eq!(v.exit, aterm_verify::exit::PASS, "{}", v.text);
    assert!(v.text.contains("0 new, 1 inherited"), "{}", v.text);
}

fn one_test(block: &str) -> Report {
    let b = bin("render");
    let log = binary_log(&b, &[("grid::row_renders", block)]);
    test_report(&[b], &[run_of(log, Some(101))])
}

/// `thread 'grid::row_renders' (<tid>) panicked at …render.rs:<at>:` and
/// `msg` under it.
fn panic_at(tid: u32, at: &str, msg: &str) -> String {
    format!(
        "\nthread 'grid::row_renders' ({tid}) panicked at crates/x/tests/render.rs:{at}:\n{msg}"
    )
}

// ---------------------------------------------------------------------------
// What a failure printed is what it said
// ---------------------------------------------------------------------------

/// The control for every fingerprint case: the same failure, told
/// differently only in its run-to-run noise — a thread id, a pid, a temp dir,
/// a home — is main's.
#[test]
fn the_same_failure_in_other_noise_is_inherited() {
    let msg = |tid, pid, dir: &str| {
        panic_at(
            tid,
            "40:5",
            &format!("lock held by pid {pid} in {dir}/lock\n  left: 1\n right: 2"),
        )
    };
    inherits(&judge_branch(
        &one_test(&msg(41377, 5521, "/private/tmp/.tmpAb1")),
        &one_test(&msg(7, 88, "/var/folders/xy/zw/T/.tmpQ9")),
    ));
}

/// A1. WHITESPACE IS DATA in a terminal emulator: a rendered row with two
/// spaces and one with eight are two failures.
#[test]
fn a1_whitespace_in_an_assert_value_is_the_message() {
    let row = |v: &str| {
        panic_at(
            7,
            "40:5",
            &format!("assertion `left == right` failed\n  left: \"{v}\"\n right: \"ab c\""),
        )
    };
    blocks(&judge_branch(
        &one_test(&row("ab  c")),
        &one_test(&row("ab        c")),
    ));
}

/// A2. What the test printed BEFORE its panic is the message: each mismatch
/// a golden-frame test prints, then one fixed sentence.
#[test]
fn a2_output_printed_before_the_panic_is_the_message() {
    let panic = panic_at(7, "77:5", "cells differ from the golden frame (see above)");
    blocks(&judge_branch(
        &one_test(&format!("cell (3,4): got 'x' want 'y'{panic}")),
        &one_test(&format!(
            "cell (3,4): got 'x' want 'y'\ncell (9,1): got ' ' want 'q'\n\
             cell (0,0): got '\\u{{0}}' want 'A'{panic}"
        )),
    ));
}

/// A3. Another assertion in the same file failing with the same words.
#[test]
fn a3_another_assert_site_with_the_same_message_is_new() {
    let msg = "called `Option::unwrap()` on a `None` value";
    blocks(&judge_branch(
        &one_test(&panic_at(7, "21:30", msg)),
        &one_test(&panic_at(7, "388:14", msg)),
    ));
}

/// A4. A duration that is the value under test — a parsed timeout — and a
/// measured one that is what failed (a render budget).
#[test]
fn a4_a_duration_value_is_the_message() {
    let parsed = |v: &str| {
        panic_at(
            7,
            "9:5",
            &format!(
                "assertion `left == right` failed: idle timeout from config\n  left: {v}\n \
                 right: 3s"
            ),
        )
    };
    blocks(&judge_branch(
        &one_test(&parsed("4s")),
        &one_test(&parsed("90s")),
    ));
    let budget = |ms: &str| panic_at(7, "9:5", &format!("frame took {ms}, over the 16ms budget"));
    blocks(&judge_branch(
        &one_test(&budget("20ms")),
        &one_test(&budget("900ms")),
    ));
}

/// A5. Hex that is data — an RGBA colour printed `{:08x}`, a digest printed
/// `{:#x}`.
#[test]
fn a5_hex_data_is_the_message() {
    let colour = |v: &str| {
        panic_at(
            7,
            "9:5",
            &format!(
                "assertion `left == right` failed: cursor colour\n  left: \"{v}\"\n right: \
                 \"ff00aa80\""
            ),
        )
    };
    blocks(&judge_branch(
        &one_test(&colour("1a2b3cff")),
        &one_test(&colour("9e0d4c11")),
    ));
    let digest = |v: &str| panic_at(7, "9:5", &format!("digest mismatch: got {v}"));
    blocks(&judge_branch(
        &one_test(&digest("0x1a2b3c4d5e6f7a8b")),
        &one_test(&digest("0x9e0d4c11aa22bb33")),
    ));
}

/// A6. An absolute path that is the value under test (config resolution).
#[test]
fn a6_an_absolute_path_value_is_the_message() {
    let path = |v: &str| {
        panic_at(
            7,
            "9:5",
            &format!(
                "assertion `left == right` failed\n  left: \"{v}\"\n right: \
                 \"/Users//u/Library/aterm/aterm.toml\""
            ),
        )
    };
    blocks(&judge_branch(
        &one_test(&path("/Users//u/.config/aterm/aterm.toml")),
        &one_test(&path("/etc/aterm.toml")),
    ));
}

/// A7. A `failures:` line inside a test's own message — an oracle's list of
/// the cases that disagreed. The branch's longer list is never compared: the
/// log is unaccounted, so the row is opaque.
#[test]
fn a7_a_failures_line_in_the_message_is_never_inherited() {
    let oracle = |cases: &str| {
        panic_at(
            7,
            "9:5",
            &format!("the corpus disagreed with the oracle\nfailures:\n{cases}"),
        )
    };
    let v = judge_branch(
        &one_test(&oracle("  case_07: got 3 want 4")),
        &one_test(&oracle(
            "  case_07: got 3 want 4\n  case_08: got 0 want 9\n  case_19: PANIC in parser",
        )),
    );
    blocks(&v);
    assert!(v.text.contains("never inherited"), "{}", v.text);
}

/// A8. `RUST_BACKTRACE` set: a helper thread's panic and backtrace came
/// first, and the test's own assertion after it went unread.
#[test]
fn a8_a_backtrace_does_not_end_the_message() {
    let block = |left: &str, trace: bool| {
        let bt = |frames: &str| {
            if trace {
                format!("stack backtrace:\n{frames}")
            } else {
                "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
                    .to_string()
            }
        };
        format!(
            "\nthread '<unnamed>' (9) panicked at crates/x/src/worker.rs:50:9:\n\
             worker channel closed\n{}{}\n{}",
            bt(
                "   0: rust_begin_unwind\n             at /rustc/abc/library/std/src/panicking.rs:665:5\n   \
                1: x::worker::run\n"
            ),
            panic_at(
                7,
                "9:5",
                &format!("assertion `left == right` failed\n  left: {left}\n right: 2")
            )
            .trim_start(),
            bt("   0: x\n")
        )
    };
    blocks(&judge_branch(
        &one_test(&block("1", true)),
        &one_test(&block("9999", true)),
    ));
    // The control: the same failure with and without the backtraces the
    // machine's `RUST_BACKTRACE` adds is main's.
    inherits(&judge_branch(
        &one_test(&block("1", false)),
        &one_test(&block("1", true)),
    ));
}

/// A13. A real deadlock reads as main's timing flake: main's test times out
/// under load after 5.01s, the branch's deadlocks and the same wait gives up
/// after 5s. A timing-shaped failure is never inherited.
#[test]
fn a13_a_real_deadlock_is_not_mains_timing_flake() {
    let v = judge_branch(
        &one_test(&panic_at(
            7,
            "9:5",
            "no reply from the render worker: timed out after 5.01s",
        )),
        &one_test(&panic_at(
            7,
            "9:5",
            "no reply from the render worker: timed out after 5s",
        )),
    );
    blocks(&v);
    assert!(
        v.text.contains("reads as a clock running out"),
        "{}",
        v.text
    );
}

/// A16. Row-level findings: two formatter diffs that differ only in their
/// spacing, in another crate's `lib.rs` at another line. (The formatter's
/// row as the stage decides it since the fourth review: inheritable only
/// when the verb printed its verdict, `differential::fmt_ran_to_end`.)
#[test]
fn a16_a_rows_whitespace_and_paths_are_its_message() {
    let row = |out: &str| {
        let mut r = Report::new("formatting");
        r.fail_checker_child(
            &run_of(
                format!(
                    "{out}{} — findings in: trustfmt\n",
                    differential::LINT_VERDICT_FAILED
                ),
                Some(1),
            ),
            "gate lint --fmt-only",
            differential::fmt_ran_to_end,
        );
        r
    };
    let main = "Diff in /Users//u/snap/crates/a/src/lib.rs:12:\n fn f() {\n-\tlet x = 1;\n+    \
                let x = 1;\n }\n";
    blocks(&judge_branch(
        &row(main),
        &row(
            "Diff in /Users//u/snap/crates/b/src/lib.rs:907:\n fn f() {\n-  let x  =  1;\n+    \
             let x = 1;\n }\n",
        ),
    ));
    blocks(&judge_branch(
        &row(main),
        &row(&main.replace("-\tlet", "-\t\tlet")),
    ));
    // The control: the same diff on another machine's snapshot is main's.
    inherits(&judge_branch(
        &row(&main.replace("/Users//u/snap", "/Users//u/aterm-verify.noindex")),
        &row(&main.replace("/Users//u/snap", "/Users//v/aterm-gate-verify.noindex")),
    ));
}

// ---------------------------------------------------------------------------
// A binary's end is part of its failure
// ---------------------------------------------------------------------------

/// A9. A binary failed main's red test AND THEN died of a signal (a crash in
/// teardown, a background thread's abort after the summary): the crash was in
/// no finding. Through the gate's own test runner (`killed by a signal`) and
/// through cargo's serial log (`signal: 11, SIGSEGV`).
#[test]
fn a9_a_crash_after_the_test_result_is_never_inherited() {
    let block = panic_at(7, "9:5", "known red");
    let b = bin("render");
    let red = || binary_log(&b, &[("grid::row_renders", &block)]);
    let main = test_report(std::slice::from_ref(&b), &[run_of(red(), Some(101))]);
    let branch = test_report(std::slice::from_ref(&b), &[run_of(red(), None)]);
    assert!(branch.render().contains("killed by a signal"));
    let t = tally(std::slice::from_ref(&branch));
    assert_eq!(t.all_findings().count(), 1);
    assert!(
        t.all_findings().all(|f| f.opaque.is_some()),
        "{:?}",
        t.findings
    );
    blocks(&judge_branch(&main, &branch));

    let serial = format!(
        "{}error: test failed, to rerun pass `-p x --test render`\n\nCaused by:\n  process \
         didn't exit successfully: `/r/target/debug/deps/render-1` (signal: 11, SIGSEGV: \
         invalid memory reference)\n",
        red()
    );
    let mut r = Report::new("test run (--workspace)");
    r.decide_test_child(&run_of(serial, Some(101)), LABEL);
    blocks(&judge_branch(&main, &r));
    // The control: the same red, exiting with libtest's 101, is main's.
    inherits(&judge_branch(
        &main,
        &test_report(std::slice::from_ref(&b), &[run_of(red(), Some(101))]),
    ));
}

/// A10. A binary printed main's red and its result, then hung in teardown
/// until the ceiling killed it: the TIMEOUT note landed after the result,
/// outside every block, and the hang was in no finding.
#[test]
fn a10_a_hang_after_the_test_result_is_never_inherited() {
    let block = panic_at(7, "9:5", "known red");
    let b = bin("render");
    let red = binary_log(&b, &[("grid::row_renders", &block)]);
    let main = test_report(std::slice::from_ref(&b), &[run_of(red.clone(), Some(101))]);
    let hung = format!(
        "{red}{} — child killed after 2700.0s, over the 2700.0s wall-clock ceiling\n\
         \x20 child: /r/target/debug/deps/render-1\n\
         \x20 test binary: tests/render.rs (target/debug/deps/render-1) finished (its `test \
         result:` line printed): the wedge is in cargo between or after test binaries, not in a \
         test\n",
        differential::CEILING_KILL
    );
    let branch = test_report(std::slice::from_ref(&b), &[run_of(hung, None)]);
    let v = judge_branch(&main, &branch);
    blocks(&v);
    assert!(v.text.contains("wall-clock ceiling"), "{}", v.text);
}

// ---------------------------------------------------------------------------
// The base, and the age cap
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    git_env(dir, args, &[])
}

/// [`git`] with more of the environment set — a committer date.
fn git_env(dir: &Path, args: &[&str], env: &[(&str, String)]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    std::fs::write(dir.join(file), body).expect("write");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

fn red_x(since: &str, since_when: u64) -> Failure {
    Failure {
        id: "-p x --test render -- grid::row_renders".into(),
        hash: "0123456789abcdef".into(),
        since: since.into(),
        since_when,
    }
}

/// The compiler main's receipts below were made by.
const TOOLCHAIN: &str = "/store/trust/9192/bin trustc 43f8b339fe0c";

/// The spec checkers main's receipts below were made with.
const CHECKERS: &str =
    "ty = /store/ty/3007/bin/ty (atpkg store, ty 0.15.0); trust-ir = absent; ay = absent";

/// The tools main's receipts below were made by — a run's own, unless a case
/// says otherwise.
fn tools() -> Tools {
    Tools {
        toolchain: TOOLCHAIN.into(),
        checkers: CHECKERS.into(),
        build_env: Some("none".into()),
    }
}

/// A whole-tree baseline receipt for `head` listing `failures`.
fn baseline_receipt(root: &Path, head: &str, failures: Vec<Failure>, when: u64) -> Receipt {
    Receipt {
        head: head.to_string(),
        tree: receipt::tree_of(root, head),
        mode: "fast".into(),
        scope: "workspace".into(),
        verdict: if failures.is_empty() { "PASS" } else { "FAIL" }.into(),
        merge_contract: failures.is_empty(),
        skipped: "none".into(),
        toolchain: TOOLCHAIN.into(),
        checkers: CHECKERS.into(),
        build_env: Some("none".into()),
        failures: Some(failures),
        baseline: true,
        when,
        ..Receipt::default()
    }
}

/// A11. A STALE `origin/main`. Main moved to M2, which fixed red X, and the
/// branch merged M2 from the local `main` before this checkout fetched it:
/// the merge-base with `origin/main` was still M1, whose receipt lists X, so
/// a conflict resolution that put X back was INHERITED. A HEAD holding a
/// local-main commit `origin/main` lacks now has no base.
#[test]
fn a11_a_stale_origin_main_is_no_base() {
    let root = aterm_verify::mktemp_dir("atv-sound-stale").expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let m1 = commit(&root, "a.txt", "1");
    receipt::write(
        &root,
        &baseline_receipt(&root, &m1, vec![red_x(&m1, NOW - 3600)], NOW - 3600),
    )
    .expect("receipt");
    let m2 = commit(&root, "a.txt", "2 (fixes X)");
    receipt::write(&root, &baseline_receipt(&root, &m2, Vec::new(), NOW - 1800)).expect("receipt");
    git(&root, &["update-ref", "refs/remotes/origin/main", &m1]);
    git(&root, &["switch", "-q", "-c", "feature", &m2]);
    let head = commit(&root, "b.txt", "puts X back");

    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Absolute { why, .. } = &plan else {
        panic!("judged against a main older than the one merged: {plan:?}");
    };
    assert!(
        why.contains(&format!("holds {}", differential::short(&m2)))
            && why.contains("is stale here"),
        "{why}"
    );
    assert!(matches!(plan.against(NOW), Against::Absolute(Some(_))));

    // The control: fetched, the base is M2, and X is not on it.
    git(&root, &["update-ref", "refs/remotes/origin/main", &m2]);
    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Judge(found) = &plan else {
        panic!("a fresh origin/main is a base: {plan:?}");
    };
    assert_eq!(found.commit, m2);
    let x = Finding {
        id: red_x(&m1, 0).id,
        hash: red_x(&m1, 0).hash,
        opaque: None,
        timing: None,
    };
    assert_eq!(
        differential::judge(&x, &found.reds(NOW)),
        Disposition::New(NewWhy::NotOnMain)
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A12. A `--baseline` on a machine that had never fetched the notes read no
/// since chain, recorded every red main had as red since NOW — undoing the
/// age cap — and published that over the notes it never read. The baseline
/// now fetches the notes before it reads the chain, and is refused when it
/// cannot; a remote with no notes yet is a first baseline.
#[test]
fn a12_a_baseline_reads_mains_published_notes_first() {
    let base = aterm_verify::mktemp_dir("atv-sound-cap").expect("mktemp");
    let origin = base.join("origin.git");
    let publisher = base.join("publisher");
    let fresh = base.join("fresh");
    std::fs::create_dir_all(&publisher).expect("mkdir");
    let origin_s = origin.to_str().expect("utf-8");
    git(&base, &["init", "-q", "--bare", "-b", "main", origin_s]);
    git(&publisher, &["init", "-q", "-b", "main"]);
    let m1 = commit(&publisher, "a.txt", "1");
    // Main's published note for M1: X red since THREE DAYS ago.
    let old = NOW - 3 * 86_400;
    let note = baseline_receipt(&publisher, &m1, vec![red_x(&m1, old)], old).render();
    std::fs::write(base.join("note"), &note).expect("write");
    let note_path = base.join("note");
    git(
        &publisher,
        &[
            "notes",
            &format!("--ref={}", differential::NOTES_REF),
            "add",
            "-F",
            note_path.to_str().expect("utf-8"),
            &m1,
        ],
    );
    let m2 = commit(&publisher, "a.txt", "2");
    git(&publisher, &["remote", "add", "origin", origin_s]);
    git(
        &publisher,
        &["push", "-q", "origin", "main", differential::NOTES_REF],
    );
    // Another machine: a plain clone, which fetches no notes.
    git(
        &base,
        &["clone", "-q", origin_s, fresh.to_str().expect("utf-8")],
    );
    assert_eq!(git(&fresh, &["rev-parse", "HEAD"]), m2);
    assert_eq!(
        differential::baseline_refusal(&fresh, &m2, false),
        None,
        "the notes fetch"
    );

    let plan = differential::resolve(&fresh, &m2, true, &tools());
    let Plan::Absolute { chain, .. } = &plan else {
        panic!("a baseline is absolute: {plan:?}");
    };
    assert_eq!(
        chain.as_ref().map(|c| c.commit.as_str()),
        Some(m1.as_str()),
        "the chain is main's published note"
    );
    // The baseline finds X red again: its since is main's, three days old…
    let b = bin("render");
    let log = binary_log(
        &b,
        &[("grid::row_renders", &panic_at(7, "9:5", "known red"))],
    );
    let mut r = Report::new("t");
    r.decide_test_child(
        &combined(std::slice::from_ref(&b), &[run_of(log, Some(101))]),
        LABEL,
    );
    let t = tally(&[r]);
    let recorded = differential::recorded_failures(&t, plan.since_from(NOW).as_ref(), &m2, NOW);
    assert_eq!(recorded[0].since_when, old, "carried, not restarted");
    // …so a branch off M2 failing X the same way is past the cap.
    let reds = BaseReds {
        commit: m2.clone(),
        source: "note".into(),
        failures: recorded,
        now: NOW + 60,
    };
    let f = t.all_findings().next().expect("X");
    assert!(matches!(
        differential::judge(f, &reds),
        Disposition::Expired(_)
    ));

    // A remote that cannot be read refuses the baseline, before any stage.
    git(
        &fresh,
        &["remote", "set-url", "origin", "/nonexistent/atv-sound.git"],
    );
    let refused = differential::baseline_refusal(&fresh, &m2, false).expect("refused");
    assert!(refused.contains("could not be fetched"), "{refused}");
    // A remote with no notes yet is a first baseline, not a refusal.
    let bare = base.join("bare.git");
    let bare_s = bare.to_str().expect("utf-8");
    git(&base, &["init", "-q", "--bare", "-b", "main", bare_s]);
    git(&publisher, &["push", "-q", bare_s, "main"]);
    git(&fresh, &["remote", "set-url", "origin", bare_s]);
    git(&fresh, &["fetch", "-q", "origin"]);
    assert_eq!(differential::baseline_refusal(&fresh, &m2, false), None);
    std::fs::remove_dir_all(&base).ok();
}

/// A14. One baseline whose test log was unaccounted — any other binary on
/// main crashed — itemized none of the test stage's reds, so X's since
/// vanished and the next baseline restarted it at NOW. The receipt of such a
/// run carries X HIDDEN with its since, and the clock runs on.
#[test]
fn a14_an_unaccounted_baseline_does_not_restart_x_s_clock() {
    let block = panic_at(7, "9:5", "known red");
    let (b, c) = (bin("render"), bin("other"));
    let x_red = || run_of(binary_log(&b, &[("grid::row_renders", &block)]), Some(101));
    let found = |r: Receipt| differential::Found {
        commit: r.head.clone(),
        source: format!("note {}", differential::short(&r.head)),
        receipt: Receipt::parse(&r.render()).expect("a receipt round-trips"),
        clock_floor: None,
    };
    let receipt = |head: &str, t: &Tally, chain: Option<&BaseReds>, when: u64| Receipt {
        head: head.to_string(),
        mode: "fast".into(),
        scope: "workspace".into(),
        verdict: "FAIL".into(),
        skipped: "none".into(),
        failures: Some(differential::recorded_failures(t, chain, head, when)),
        hidden: differential::hidden_failures(t, chain),
        baseline: true,
        when,
        ..Receipt::default()
    };
    // Baseline 1 (three days ago): X itemized.
    let t1 = tally(&[test_report(
        &[b.clone(), c.clone()],
        &[x_red(), run_of(green_log(&c), Some(0))],
    )]);
    let r1 = found(receipt(&"1".repeat(40), &t1, None, NOW - 3 * 86_400));
    // Baseline 2 (two days ago): X still red, but `other` crashed.
    let t2 = tally(&[test_report(
        &[b.clone(), c.clone()],
        &[
            x_red(),
            run_of(format!("{}\n\nrunning 1 test\n", c.header), None),
        ],
    )]);
    let chain1 = r1.chain(NOW - 2 * 86_400);
    let r2 = found(receipt(
        &"2".repeat(40),
        &t2,
        Some(&chain1),
        NOW - 2 * 86_400,
    ));
    let x_id = &r1.receipt.failures.as_ref().expect("listed")[0].id;
    assert!(
        r2.receipt
            .failures
            .as_ref()
            .expect("listed")
            .iter()
            .all(|f| &f.id != x_id),
        "X is not itemized in baseline 2"
    );
    assert_eq!(r2.receipt.hidden.len(), 1, "{:?}", r2.receipt);
    // Baseline 3 (an hour ago): X itemized again, since carried.
    let t3 = tally(&[test_report(
        &[b.clone(), c.clone()],
        &[x_red(), run_of(green_log(&c), Some(0))],
    )]);
    let r3 = found(receipt(
        &"3".repeat(40),
        &t3,
        Some(&r2.chain(NOW - 3600)),
        NOW - 3600,
    ));
    let x = &r3.receipt.failures.as_ref().expect("listed")[0];
    assert_eq!(x.since_when, NOW - 3 * 86_400, "X's clock ran on");
    let f = t3.all_findings().next().expect("X");
    assert!(matches!(
        differential::judge(f, &r3.reds(NOW)),
        Disposition::Expired(_)
    ));
}

/// A15. A judging machine whose clock is behind main's receipt read every
/// red's age short: 30 h red, judged on a clock 8 h slow, was inside the cap.
#[test]
fn a15_a_clock_behind_mains_receipt_is_judged_by_the_absolute_rule() {
    let since = NOW - 30 * 3600;
    let plan = Plan::Judge(differential::Found {
        commit: "a".repeat(40),
        source: "note aaaaaaaaa".into(),
        receipt: Receipt {
            failures: Some(vec![red_x(&"b".repeat(40), since)]),
            when: NOW - 3600,
            ..Receipt::default()
        },
        clock_floor: None,
    });
    let f = Finding {
        id: red_x("", 0).id,
        hash: red_x("", 0).hash,
        opaque: None,
        timing: None,
    };
    let Against::Base(at_real) = plan.against(NOW) else {
        panic!("a clock ahead of the receipt judges against it");
    };
    assert!(matches!(
        differential::judge(&f, &at_real),
        Disposition::Expired(_)
    ));
    let slow = plan.against(NOW - 8 * 3600);
    let Against::Absolute(Some(why)) = &slow else {
        panic!("8 h slow: {slow:?}");
    };
    assert!(why.contains("before main's note aaaaaaaaa"), "{why}");
    // Inside the slack, the receipt is still read.
    assert!(matches!(
        plan.against(NOW - 3600 - differential::CLOCK_SLACK_SECS),
        Against::Base(_)
    ));
}

// ---------------------------------------------------------------------------
// Round two (2026-09-27, third review): what judged a run, and main's clocks
// ---------------------------------------------------------------------------

/// A repository with `main` at one commit whose store receipt lists X red
/// for an hour, `origin/main` there, and HEAD one commit off it on a branch:
/// `(root, main, head)`.
fn forked_off_a_red_main(tag: &str) -> (PathBuf, String, String) {
    let root = aterm_verify::mktemp_dir(tag).expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let m = commit(&root, "a.txt", "1");
    receipt::write(
        &root,
        &baseline_receipt(&root, &m, vec![red_x(&m, NOW - 3600)], NOW - 3600),
    )
    .expect("receipt");
    git(&root, &["update-ref", "refs/remotes/origin/main", &m]);
    git(&root, &["switch", "-q", "-c", "feature"]);
    let head = commit(&root, "b.txt", "a change");
    (root, m, head)
}

/// X as the branch finds it: listed on main with the same id and hash.
fn x_again() -> Finding {
    Finding {
        id: red_x("", 0).id,
        hash: red_x("", 0).hash,
        opaque: None,
        timing: None,
    }
}

/// B1. MAIN'S REDS UNDER ANOTHER COMPILER. Main's receipt for the base was
/// made by one trustc; this run's is another (a re-seal, a newer store build
/// — or the older one on the machine that published main's note). Main may be
/// green on X under this compiler, and a branch that breaks X the same way
/// read as main's red: the base was never compared with the run's tools, so X
/// was INHERITED. A base now serves only a run of the same trustc commit and
/// the same spec checkers; with none, the absolute rule, naming the
/// difference — and main's clocks are still carried from the base it
/// refused. The control: the same compiler in another directory (another
/// machine's store) is the same compiler.
#[test]
fn b1_a_base_made_by_other_tools_is_no_base() {
    let (root, m, head) = forked_off_a_red_main("atv-sound-tools");
    let resealed = Tools {
        toolchain: "/store/trust/9300/bin trustc 77aa00bb11cc".into(),
        ..tools()
    };
    let plan = differential::resolve(&root, &head, false, &resealed);
    let Plan::Absolute { why, chain } = &plan else {
        panic!("judged against reds another compiler found: {plan:?}");
    };
    assert!(
        why.contains("trustc 43f8b339fe0c") && why.contains("trustc 77aa00bb11cc"),
        "{why}"
    );
    assert_eq!(
        chain.as_ref().map(|c| c.commit.as_str()),
        Some(m.as_str()),
        "main's clocks are carried from the base it could not judge by"
    );
    assert!(matches!(plan.against(NOW), Against::Absolute(Some(_))));

    // Another `ty` — the same compiler.
    let other_ty = Tools {
        checkers: CHECKERS.replace("ty 0.15.0", "ty 0.16.0"),
        ..tools()
    };
    let plan = differential::resolve(&root, &head, false, &other_ty);
    let Plan::Absolute { why, .. } = &plan else {
        panic!("judged against reds another checker found: {plan:?}");
    };
    assert!(
        why.contains("ty 0.15.0") && why.contains("ty 0.16.0"),
        "{why}"
    );

    // The control: the same trustc commit and checkers, kept elsewhere.
    let elsewhere = Tools {
        toolchain: "/Users//someone/Library/aterm/pkg/store/trust/9192/bin trustc 43f8b339fe0c"
            .into(),
        checkers: CHECKERS.replace("/store/ty/3007", "/Users//someone/store/ty/3007"),
        ..tools()
    };
    let plan = differential::resolve(&root, &head, false, &elsewhere);
    let Plan::Judge(found) = &plan else {
        panic!("the same tools on another machine judge against main: {plan:?}");
    };
    assert_eq!(found.commit, m);
    assert!(differential::judge(&x_again(), &found.reds(NOW)).excused());

    // A receipt an older gate wrote names no compiler: it serves no run.
    let unnamed = Receipt {
        toolchain: String::new(),
        ..baseline_receipt(&root, &m, vec![red_x(&m, NOW - 60)], NOW - 60)
    };
    receipt::write(&root, &unnamed).expect("receipt");
    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Absolute { why, .. } = &plan else {
        panic!("judged against a receipt that names no compiler: {plan:?}");
    };
    assert!(why.contains("names no compiler commit"), "{why}");
    std::fs::remove_dir_all(&root).ok();
}

/// B1, the published half. A NOTE FROM ANOTHER COMPILER. Main's baseline
/// note was published from a machine whose trustc is not this run's; it is
/// refused like a store receipt — and a candidate that WAS made by this
/// run's tools is still found: here the store's receipt is another
/// compiler's and the note is this one's, so the note is the base.
#[test]
fn b1_a_note_from_another_compiler_is_passed_over_for_one_that_serves() {
    let (root, m, head) = forked_off_a_red_main("atv-sound-note");
    // The store's receipt: another compiler's.
    let stale = Receipt {
        toolchain: "/store/trust/9000/bin trustc 0ld0ld0ld0ld".into(),
        ..baseline_receipt(&root, &m, vec![red_x(&m, NOW - 60)], NOW - 60)
    };
    receipt::write(&root, &stale).expect("receipt");
    let note = baseline_receipt(&root, &m, vec![red_x(&m, NOW - 3600)], NOW - 3600).render();
    let file = root.join(".git").join("note");
    std::fs::write(&file, note).expect("write");
    git(
        &root,
        &[
            "notes",
            &format!("--ref={}", differential::NOTES_REF),
            "add",
            "-F",
            file.to_str().expect("utf-8"),
            &m,
        ],
    );
    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Judge(found) = &plan else {
        panic!("the note made by this run's compiler is a base: {plan:?}");
    };
    assert_eq!(found.source, format!("note {}", differential::short(&m)));

    // A third compiler: neither serves, and both are named.
    let third = Tools {
        toolchain: "/store/trust/9400/bin trustc 7h1rd7h1rd7h".into(),
        ..tools()
    };
    let plan = differential::resolve(&root, &head, false, &third);
    let Plan::Absolute { why, .. } = &plan else {
        panic!("judged against reds another compiler found: {plan:?}");
    };
    assert!(
        why.contains("trustc 0ld0ld0ld0ld") && why.contains("trustc 43f8b339fe0c"),
        "{why}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// B2, B3. The comparison itself: the same compiler is the same trustc
/// COMMIT (not the directory a machine keeps it in); the same checkers are
/// each absent on both sides, or found by the same tier saying the same
/// `--version` — and at the same path where either gave no version. A side
/// that cannot say which compiler or which checkers made it (no line, a
/// trustc that named no commit) is never the same as any.
#[test]
fn b2_b3_the_same_tools_are_the_same_commit_and_the_same_checkers() {
    let with = |toolchain: &str, checkers: &str| Tools {
        toolchain: toolchain.into(),
        checkers: checkers.into(),
        ..tools()
    };
    let same = |a: &Tools, b: &Tools| differential::tools_differ(a, b).is_none();
    let ours = tools();
    assert!(same(&ours, &ours));
    assert!(same(
        &with("/elsewhere/bin trustc 43f8b339fe0c", CHECKERS),
        &ours
    ));
    // Another compiler, or none named.
    for toolchain in [
        "/store/trust/9192/bin trustc 43f8b339fe0d",
        "/store/trust/9192/bin trustc unknown",
        "",
    ] {
        assert!(!same(&with(toolchain, CHECKERS), &ours), "{toolchain:?}");
        assert!(!same(&ours, &with(toolchain, CHECKERS)), "{toolchain:?}");
    }
    assert!(!same(
        &with("/store/trust/9192/bin trustc unknown", CHECKERS),
        &with("/store/trust/9192/bin trustc unknown", CHECKERS)
    ));
    // Another checker: a version, a tier, absent on one side, none named.
    for checkers in [
        CHECKERS.replace("ty 0.15.0", "ty 0.15.1"),
        CHECKERS.replace("atpkg store", "PATH"),
        "ty = absent; trust-ir = absent; ay = absent".to_string(),
        CHECKERS.replace(
            "ay = absent",
            "ay = /store/ay/12/bin/ay (atpkg store, ay 0.3.0)",
        ),
        String::new(),
        "ty".to_string(),
    ] {
        assert!(!same(&with(TOOLCHAIN, &checkers), &ours), "{checkers:?}");
    }
    // No version to compare: the same file, or not the same checker.
    let quiet = CHECKERS.replace("ty 0.15.0", "no --version answer");
    assert!(same(&with(TOOLCHAIN, &quiet), &with(TOOLCHAIN, &quiet)));
    assert!(!same(
        &with(TOOLCHAIN, &quiet),
        &with(TOOLCHAIN, &quiet.replace("3007", "3010"))
    ));
    assert!(!same(&with(TOOLCHAIN, &quiet), &ours));

    // C4 (2026-09-27, fourth review): the same version at another STORE
    // BUILD is another checker. `ty --version` prints no build id (`ty
    // 0.13.0`), so a rebuilt `ty` read as the same one; the store build is
    // the resolved path after `/store/`, the same on every machine.
    assert!(!same(
        &with(TOOLCHAIN, &CHECKERS.replace("ty/3007", "ty/3008")),
        &ours
    ));
    assert!(same(
        &with(
            TOOLCHAIN,
            &CHECKERS.replace(
                "/store/ty/3007",
                "/Users//else/Library/Application Support/aterm/pkg/store/ty/3007"
            )
        ),
        &ours
    ));
    // Found on PATH, the version is all there is to compare.
    let on_path = CHECKERS.replace("atpkg store", "PATH");
    assert!(same(
        &with(
            TOOLCHAIN,
            &on_path.replace("/store/ty/3007/bin/ty", "/opt/ty/bin/ty")
        ),
        &with(TOOLCHAIN, &on_path)
    ));
}

/// B4. A BRANCH JUDGED BY THE ABSOLUTE RULE RESTARTED MAIN'S CLOCK, AND ITS
/// RECEIPT BECAME MAIN'S. Main M0 has been red on X for three days; main
/// moved to M1, which nobody ran. A branch C off M1 has no base (no receipt
/// for M1) and is judged by the absolute rule — with NO since chain, so its
/// receipt listed X as red since C's own run. Main takes slices by
/// fast-forward: C lands, and the next branch's base IS C, whose receipt says
/// X is an hour old — INHERITED, the 24 h cap undone. Now the absolute plan
/// carries main's clocks from the newest receipt on the base's history.
#[test]
fn b4_a_branch_judged_absolute_carries_mains_clock_into_the_next_base() {
    let root = aterm_verify::mktemp_dir("atv-sound-ff").expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let m0 = commit(&root, "a.txt", "0");
    let old = NOW - 3 * 86_400;
    receipt::write(
        &root,
        &baseline_receipt(&root, &m0, vec![red_x(&m0, old)], old),
    )
    .expect("receipt");
    let m1 = commit(&root, "a.txt", "1");
    git(&root, &["update-ref", "refs/remotes/origin/main", &m1]);
    git(&root, &["switch", "-q", "-c", "slice"]);
    let c = commit(&root, "b.txt", "a slice");

    let plan = differential::resolve(&root, &c, false, &tools());
    let Plan::Absolute { why, .. } = &plan else {
        panic!("no receipt for M1: {plan:?}");
    };
    assert!(
        why.contains(&format!(
            "no usable receipt for the base {}",
            differential::short(&m1)
        )),
        "{why}"
    );
    // C's run finds X red, the way main does.
    let t = tally(&[one_test(&panic_at(7, "9:5", "known red"))]);
    let ran = NOW - 3600;
    let recorded = differential::recorded_failures(&t, plan.since_from(ran).as_ref(), &c, ran);
    assert_eq!(
        (recorded[0].since.as_str(), recorded[0].since_when),
        (m0.as_str(), old),
        "C's receipt carries when main went red on X"
    );
    // C lands by fast-forward, and a branch off it is judged against C.
    receipt::write(
        &root,
        &Receipt {
            head: c.clone(),
            tree: receipt::tree_of(&root, &c),
            mode: "fast".into(),
            scope: "workspace".into(),
            verdict: "FAIL".into(),
            skipped: "none".into(),
            toolchain: TOOLCHAIN.into(),
            checkers: CHECKERS.into(),
            build_env: Some("none".into()),
            failures: Some(recorded),
            when: ran,
            ..Receipt::default()
        },
    )
    .expect("receipt");
    git(&root, &["update-ref", "refs/remotes/origin/main", &c]);
    git(&root, &["switch", "-q", "-c", "next"]);
    let d = commit(&root, "c.txt", "the next slice");
    let plan = differential::resolve(&root, &d, false, &tools());
    let Plan::Judge(found) = &plan else {
        panic!("C's receipt is the base: {plan:?}");
    };
    assert_eq!(found.commit, c);
    let x = t.all_findings().next().expect("X");
    assert!(
        matches!(
            differential::judge(x, &found.reds(NOW)),
            Disposition::Expired(_)
        ),
        "main has been red on X for three days: {:?}",
        differential::judge(x, &found.reds(NOW))
    );
    std::fs::remove_dir_all(&root).ok();
}

/// B6. A POLL THAT GAVE UP IS A CLOCK THAT RAN OUT. Main's control-socket
/// smoke is timing-flaky: a slow start under load misses the hundred polls and
/// prints `control socket never started listening` over the GUI's log tail —
/// often empty. A branch whose startup DEADLOCKS prints the same row over the
/// same tail, and a control client that answered nothing (`<no reply>`: a
/// crash, a hang, a slow start) reads the same whatever happened: both were
/// INHERITED. The control: a reply that says what went wrong is main's.
#[test]
fn b6_a_poll_that_gave_up_is_not_mains_flake() {
    let row = |label: &str, tail: &str| {
        let mut r = Report::new("headless control-socket smoke");
        r.fail(label);
        r.raw(tail);
        r
    };
    for (label, tail) in [
        ("ats: control socket never started listening", ""),
        (
            "ats: control socket never started listening",
            "        headless smoke child log (last 80 lines):\n          aterm: starting\n",
        ),
        (
            "smoke: metrics carry no wake_heals after the typing burst -> <no reply>",
            "",
        ),
        (
            "gui smoke [effects=off]: the frontmost window never produced an initial present \
             [<no reply>]",
            "",
        ),
    ] {
        let v = judge_branch(&row(label, tail), &row(label, tail));
        blocks(&v);
        assert!(
            v.text.contains("reads as a clock running out"),
            "{label}: {}",
            v.text
        );
    }
    // The control: a reply that says what went wrong, on a row after which
    // the smoke goes on (its last check: `Report::fail_and_continue`), is
    // main's.
    let went_on = |label: &str| {
        let mut r = Report::new("headless control-socket smoke");
        r.fail_and_continue(label);
        r
    };
    inherits(&judge_branch(
        &went_on(
            "smoke: 2 lost output wake(s) healed during a plain typing burst -> OK wake_heals=2",
        ),
        &went_on(
            "smoke: 2 lost output wake(s) healed during a plain typing burst -> OK wake_heals=2",
        ),
    ));
    // …and the same reply on a row that ENDED the smoke hid every check
    // behind it (2026-09-27, fourth review): never main's.
    let ended = "gui smoke [effects=off]: metrics reset -> ERR no session named main";
    let v = judge_branch(&row(ended, ""), &row(ended, ""));
    blocks(&v);
    assert!(
        v.text.contains("nothing shows it ran to its end"),
        "{}",
        v.text
    );
}

/// B11. A SLOW CLOCK, JUDGING A RECEIPT OLDER THAN ITS ERROR. Main's receipt
/// for the base was written 30 h ago, when X was already red; this machine's
/// clock is 8 h slow, so it reads later than that receipt — the second
/// review's check passed — and X's age read 22 h: INHERITED. Main's history
/// holds a later time: its newest commit, an hour old. A clock behind it is
/// no clock either.
#[test]
fn b11_a_clock_behind_mains_newest_commit_is_no_clock() {
    let root = aterm_verify::mktemp_dir("atv-sound-slow").expect("mktemp");
    let at = |when: u64| {
        let date = format!("@{when} +0000");
        vec![
            ("GIT_COMMITTER_DATE", date.clone()),
            ("GIT_AUTHOR_DATE", date),
        ]
    };
    let commit_at = |file: &str, body: &str, when: u64| {
        std::fs::write(root.join(file), body).expect("write");
        git(&root, &["add", "-A"]);
        git_env(&root, &["commit", "-q", "-m", file], &at(when));
        git(&root, &["rev-parse", "HEAD"])
    };
    git(&root, &["init", "-q", "-b", "main"]);
    let m = commit_at("a.txt", "1", NOW - 31 * 3600);
    let red_since = NOW - 30 * 3600;
    receipt::write(
        &root,
        &baseline_receipt(&root, &m, vec![red_x(&m, red_since)], red_since),
    )
    .expect("receipt");
    let tip = commit_at("a.txt", "2", NOW - 3600);
    git(&root, &["update-ref", "refs/remotes/origin/main", &tip]);
    git(&root, &["switch", "-q", "-c", "feature", &m]);
    let head = commit_at("b.txt", "a change", NOW - 20 * 3600);

    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Judge(found) = &plan else {
        panic!("M's receipt is the base: {plan:?}");
    };
    assert_eq!(found.commit, m);
    let Against::Base(at_real) = plan.against(NOW) else {
        panic!("a right clock judges against main");
    };
    assert!(matches!(
        differential::judge(&x_again(), &at_real),
        Disposition::Expired(_)
    ));
    let slow = plan.against(NOW - 8 * 3600);
    let Against::Absolute(Some(why)) = &slow else {
        panic!("8 h slow, behind main's newest commit: {slow:?}");
    };
    assert!(why.contains("the newest commit on origin/main"), "{why}");
    std::fs::remove_dir_all(&root).ok();
}

/// B8. A NEWLINE IN THE SCOPE FORGED THE RECEIPT. A `--changed` run's scope
/// is `changed:<the --base it was given>`, written verbatim; a base with a
/// newline in it put `merge-contract yes` and `scope workspace` on lines of
/// their own. The release cutter's reader takes a key's FIRST line — a
/// narrowed, failed run counted as a pass — and the gate's own reader its
/// LAST: the narrowed receipt read as a whole-tree one, a base. Every value
/// is one line now.
#[test]
fn b8_a_newline_in_a_value_forges_no_key() {
    let forged = Receipt {
        head: "a".repeat(40),
        mode: "fast".into(),
        scope: "changed:main\nmerge-contract yes\nscope workspace".into(),
        verdict: "FAIL".into(),
        merge_contract: false,
        skipped: "none".into(),
        failures: Some(Vec::new()),
        base: Some("b\nbaseline yes".into()),
        when: NOW,
        ..Receipt::default()
    };
    let text = forged.render();
    let first = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .map(str::to_string)
    };
    assert_eq!(first("merge-contract").as_deref(), Some("no"), "{text}");
    let back = Receipt::parse(&text).expect("it parses");
    assert!(back.scope.starts_with("changed:main"), "{back:?}");
    assert!(!back.merge_contract && !back.baseline, "{back:?}");
    assert!(
        differential::usable(&back).is_err(),
        "a narrowed run is no base"
    );
}

/// B9. A RUNNER THAT RAN NO TEST. The host's runner in a cargo config or the
/// environment (`CARGO_TARGET_<TRIPLE>_RUNNER=true`) outranks the gate's
/// `cfg(all())` recording runner: the recording pass falls back to cargo's
/// serial child, which starts every binary through the same runner — and a
/// child that exits 0 was an `ok` row with not one test run. A test child
/// that announced binaries and printed no `test result:` decided nothing.
#[test]
fn b9_a_test_child_that_ran_no_test_is_no_pass() {
    let vacuous = "   Compiling x v0.1.0\n    Finished `test` profile\n     Running \
                   unittests src/lib.rs (target/debug/deps/x-1)\n     Running tests/render.rs \
                   (target/debug/deps/render-1)\n";
    let mut r = Report::new("test run (--workspace)");
    r.decide_test_child(&run_of(vacuous.into(), Some(0)), LABEL);
    let t = tally(&[r]);
    assert_eq!(t.could_not_run.len(), 1, "{t:?}");
    assert!(
        t.could_not_run[0].contains("not one printed a libtest `test result:` line"),
        "{t:?}"
    );
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &t, &Against::absolute());
    assert_eq!(v.exit, aterm_verify::exit::COULD_NOT_RUN, "{}", v.text);
    assert!(!v.claims_merge_contract);
    // The controls: a binary that ran its tests (none, filtered out, is a
    // run), and a child that announced no binary at all.
    let b = bin("render");
    let mut r = Report::new("t");
    r.decide_test_child(&run_of(green_log(&b), Some(0)), LABEL);
    let mut filtered = Report::new("t");
    filtered.decide_test_child(
        &run_of(
            format!(
                "{}\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 \
                 measured; 12 filtered out; finished in 0.00s\n",
                b.header
            ),
            Some(0),
        ),
        LABEL,
    );
    let mut empty = Report::new("t");
    empty.decide_test_child(
        &run_of("    Finished `test` profile\n".into(), Some(0)),
        LABEL,
    );
    let t = tally(&[r, filtered, empty]);
    assert!(!t.failed() && t.skips.is_empty(), "{t:?}");
}

/// B12. MAIN'S REDS UNDER OTHER COMPILE FLAGS. The same trustc under
/// `RUSTFLAGS` (the documented flag-spelling override REPLACES the config's
/// flags, `--cfg clean_islands` with them), behind `RUSTC` or a wrapper, with
/// a profile override, or starting test binaries through a target runner,
/// builds or runs other code — and a receipt did not say which it was under,
/// so main's reds found under one were a branch's base under another. The
/// receipt names its build environment now (`build-env`), and a base serves
/// only a run under the same; one that does not say serves none.
#[test]
fn b12_a_base_built_under_other_flags_is_no_base() {
    let (root, _m, head) = forked_off_a_red_main("atv-sound-flags");
    let flagged = Tools {
        build_env: Some(differential::build_env([(
            "RUSTFLAGS".into(),
            "-Ztrust-verify=off".into(),
        )])),
        ..tools()
    };
    let plan = differential::resolve(&root, &head, false, &flagged);
    let Plan::Absolute { why, .. } = &plan else {
        panic!("judged against reds built under other flags: {plan:?}");
    };
    assert!(
        why.contains("under `none`") && why.contains("RUSTFLAGS=\"-Ztrust-verify=off\""),
        "{why}"
    );
    // The control: the same environment is the same build.
    assert!(matches!(
        differential::resolve(&root, &head, false, &tools()),
        Plan::Judge(_)
    ));
    // A receipt that does not say serves no run.
    let silent = Tools {
        build_env: None,
        ..tools()
    };
    assert!(differential::tools_differ(&silent, &tools()).is_some());
    std::fs::remove_dir_all(&root).ok();

    // What is recorded: compile and test-run configuration, sorted, and
    // nothing else — never `CARGO_TARGET_DIR`, which no child sees.
    let env = |pairs: &[(&str, &str)]| {
        differential::build_env(pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())))
    };
    assert_eq!(env(&[]), "none");
    assert_eq!(
        env(&[
            ("PATH", "/bin"),
            ("CARGO_TARGET_DIR", "/t"),
            ("HOME", "/h"),
            ("CARGO_BUILD_JOBS", "4"),
        ]),
        "none"
    );
    assert_eq!(
        env(&[
            ("RUSTC_WRAPPER", "sccache"),
            ("CARGO_PROFILE_TEST_DEBUG_ASSERTIONS", "false"),
            ("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER", "true"),
        ]),
        "CARGO_PROFILE_TEST_DEBUG_ASSERTIONS=\"false\" \
         CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER=\"true\" RUSTC_WRAPPER=\"sccache\""
    );
}

// ---------------------------------------------------------------------------
// Round three (2026-09-27, fourth review): main's oldest clock
// ---------------------------------------------------------------------------

/// C6. A TOOLCHAIN SWITCH RESTARTED MAIN'S CLOCK. This machine's store
/// receipt for the base, made by the compiler it ran before a switch, has
/// recorded X red for 30 h; the published note, made by this run's compiler
/// after the switch, records X since an hour ago. The note is the base — the
/// store's receipt, another compiler's, cannot serve — and its younger clock
/// judged the cap: X was INHERITED, the 24 h cap restarted by the switch. Now
/// the oldest clock any receipt about the base records for X judges, whatever
/// tools made it: an older since only makes a red block sooner. The control:
/// with no older clock anywhere, the base's own judges.
#[test]
fn c6_a_toolchain_switch_never_restarts_mains_clock() {
    let (root, m, head) = forked_off_a_red_main("atv-sound-oldest");
    let Plan::Judge(found) = differential::resolve(&root, &head, false, &tools()) else {
        panic!("main's receipt is a base");
    };
    assert!(
        matches!(
            differential::judge(&x_again(), &found.reds(NOW)),
            Disposition::Inherited(_)
        ),
        "the control: an hour-old red is main's"
    );

    let before_the_switch = Receipt {
        toolchain: "/store/trust/9000/bin trustc 0ld0ld0ld0ld".into(),
        ..baseline_receipt(&root, &m, vec![red_x(&m, NOW - 30 * 3600)], NOW - 7200)
    };
    receipt::write(&root, &before_the_switch).expect("receipt");
    let note = baseline_receipt(&root, &m, vec![red_x(&m, NOW - 3600)], NOW - 3600).render();
    let file = root.join(".git").join("note");
    std::fs::write(&file, note).expect("write");
    git(
        &root,
        &[
            "notes",
            &format!("--ref={}", differential::NOTES_REF),
            "add",
            "-f",
            "-F",
            file.to_str().expect("utf-8"),
            &m,
        ],
    );
    let plan = differential::resolve(&root, &head, false, &tools());
    let Plan::Judge(found) = &plan else {
        panic!("the note made by this run's compiler is a base: {plan:?}");
    };
    assert_eq!(found.source, format!("note {}", differential::short(&m)));
    match differential::judge(&x_again(), &found.reds(NOW)) {
        Disposition::Expired(since) => assert_eq!(since.when, NOW - 30 * 3600),
        other => panic!("X has been red on main for 30 h, whatever read it: {other:?}"),
    }
    std::fs::remove_dir_all(&root).ok();
}
