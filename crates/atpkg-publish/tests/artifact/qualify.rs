// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A product's qualification list: what `[workspace.metadata.atpkg] qualify` may say, and what
//! a step's run must show to count as a pass.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use atpkg_publish::artifact::qualify::{self, Plan, Receipt, Runner, Site, Stage, Verdict};
use atpkg_publish::artifact::tools::{Ran, Run, RunError};
use atpkg_publish::triple::AARCH64_APPLE_DARWIN as ARM;

use super::super::shared::scratch;

/// `targo metadata`'s JSON for a workspace whose recipe carries `qualify` (JSON text), and a
/// member `ay` with no recipe of its own.
fn workspace(qualify: &str) -> String {
    format!(
        r#"{{"packages":[{{"name":"ay","version":"0.27.0","manifest_path":"/w/Cargo.toml"}}],
            "metadata":{{"atpkg":{{"expose":["ay"],"build-args":["-p","ay"],"qualify":{qualify}}}}}}}"#
    )
}

const FMT: &str =
    r#"{"name":"fmt","stage":"source","run":["targo","fmt","--check"],"within-secs":120}"#;
const UNIT: &str = r#"{"name":"unit","stage":"source","run":["targo","--unverified","test","--locked"],"within-secs":1800,"min-tests":40}"#;
const BUILD: &str = r#"{"name":"build","stage":"build","within-secs":3600}"#;
const VERSION: &str =
    r#"{"name":"version","stage":"packed","run":["{bin}/ay","--version"],"within-secs":30}"#;

fn plan(steps: &[&str]) -> Plan {
    qualify::parse(
        workspace(&format!("[{}]", steps.join(","))).as_bytes(),
        "ay",
    )
    .expect("a list that parses")
    .expect("a declared list")
}

fn refusal(steps: &[&str]) -> String {
    match qualify::parse(
        workspace(&format!("[{}]", steps.join(","))).as_bytes(),
        "ay",
    ) {
        Err(e) => e.to_string(),
        Ok(plan) => panic!("{steps:?} parsed: {plan:?}"),
    }
}

#[test]
fn a_list_is_its_source_checks_one_build_and_the_packed_controls_in_order() {
    let plan = plan(&[FMT, UNIT, BUILD, VERSION]);
    let names =
        |steps: &[qualify::Step]| -> Vec<String> { steps.iter().map(|s| s.name.clone()).collect() };
    assert_eq!(names(plan.source()), ["fmt", "unit"]);
    assert_eq!(plan.build().name, "build");
    assert_eq!(plan.build().within, Duration::from_secs(3600));
    assert!(plan.build().run.is_empty());
    assert_eq!(names(plan.packed()), ["version"]);
    let unit = &plan.source()[1];
    assert_eq!(unit.stage, Stage::Source);
    assert_eq!(unit.within, Duration::from_secs(1800));
    assert_eq!(unit.min_tests, Some(40));
    assert_eq!(unit.run, ["targo", "--unverified", "test", "--locked"]);
    assert_eq!(plan.source()[0].min_tests, None);
}

#[test]
fn a_program_with_its_own_recipe_is_qualified_by_that_recipe_alone() {
    let json = format!(
        r#"{{"packages":[
              {{"name":"atpkg-keys","version":"0.0.0","manifest_path":"/w/crates/atpkg-keys/Cargo.toml",
                "metadata":{{"atpkg":{{"expose":["atpkg-keys"],"qualify":[{BUILD},{VERSION}]}}}}}},
              {{"name":"bare","version":"0.0.0","manifest_path":"/w/crates/bare/Cargo.toml",
                "metadata":{{"atpkg":{{"expose":["bare"]}}}}}}],
            "metadata":{{"atpkg":{{"expose":["aterm"],"qualify":[{FMT},{BUILD},{VERSION}]}}}}}}"#
    );
    let own = qualify::parse(json.as_bytes(), "atpkg-keys")
        .expect("parses")
        .expect("its own list");
    assert!(
        own.source().is_empty(),
        "the workspace's fmt step is not its own"
    );
    // A package recipe that declares no list is not lent the workspace's.
    assert_eq!(
        qualify::parse(json.as_bytes(), "bare").expect("parses"),
        None
    );
    // A program with no package recipe reads the workspace's.
    let app = qualify::parse(json.as_bytes(), "aterm")
        .expect("parses")
        .expect("the workspace list");
    assert_eq!(app.source().len(), 1);
}

#[test]
fn a_repository_that_declares_no_list_has_none() {
    let json =
        r#"{"packages":[{"name":"ay","version":"0.27.0"}],"metadata":{"atpkg":{"expose":["ay"]}}}"#;
    assert_eq!(qualify::parse(json.as_bytes(), "ay").expect("parses"), None);
    let bare = r#"{"packages":[{"name":"ay","version":"0.27.0"}],"metadata":null}"#;
    assert_eq!(qualify::parse(bare.as_bytes(), "ay").expect("parses"), None);
}

#[test]
fn a_malformed_list_is_refused_by_what_is_wrong_with_it() {
    let step = |body: &str| format!("{{{body}}}");
    let cases: Vec<(Vec<String>, &str)> = vec![
        (vec![], "is empty"),
        (
            vec![
                step(r#""name":"fmt","stage":"source","run":["x"],"within_secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`within_secs` is not a step key",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"source","run":["x"]"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "every step commits its time bound",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"source","run":["x"],"within-secs":0"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`within-secs` must be",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"source","run":["x"],"within-secs":86401"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`within-secs` must be",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"lint","run":["x"],"within-secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`stage` must be",
        ),
        (
            vec![
                step(r#""name":"Fmt Check","stage":"source","run":["x"],"within-secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`name` must be",
        ),
        (
            vec![
                step(r#""name":"build","stage":"build","run":["make"],"within-secs":9"#),
                VERSION.into(),
            ],
            "the build is the pack's own",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"source","within-secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`run` must name a command",
        ),
        (
            vec![
                step(r#""name":"fmt","stage":"source","run":["targo",""],"within-secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`run` must be an array of nonempty strings",
        ),
        (
            vec![
                step(r#""name":"early","stage":"source","run":["{bin}/ay"],"within-secs":9"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "does not exist before the build",
        ),
        (
            vec![
                step(r#""name":"unit","stage":"source","run":["x"],"within-secs":9,"min-tests":0"#),
                BUILD.into(),
                VERSION.into(),
            ],
            "`min-tests` must be",
        ),
        (
            vec![FMT.into(), FMT.into(), BUILD.into(), VERSION.into()],
            "names `fmt` twice",
        ),
        (
            vec![FMT.into(), VERSION.into()],
            "exactly one \"build\" step, and carries 0",
        ),
        (
            vec![
                BUILD.into(),
                step(r#""name":"again","stage":"build","within-secs":9"#),
                VERSION.into(),
            ],
            "exactly one \"build\" step, and carries 2",
        ),
        (
            vec![BUILD.into(), FMT.into(), VERSION.into()],
            "source step `fmt` after the build",
        ),
        (
            vec![VERSION.into(), BUILD.into()],
            "packed step `version` before the build",
        ),
        (vec![FMT.into(), BUILD.into()], "carries no \"packed\" step"),
        (
            // A control that names nothing of the packed tree runs none of it.
            vec![
                BUILD.into(),
                step(r#""name":"ok","stage":"packed","run":["/usr/bin/true"],"within-secs":5"#),
            ],
            "must name the packed tree ({bin} or {root})",
        ),
    ];
    for (steps, want) in cases {
        let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
        let said = refusal(&steps);
        assert!(said.contains(want), "{steps:?}: wanted `{want}` in: {said}");
        assert!(said.contains("ay's metadata.atpkg qualify"), "{said}");
    }
    let not_a_list = workspace(r#"{"name":"fmt"}"#);
    let said = qualify::parse(not_a_list.as_bytes(), "ay")
        .expect_err("a table is not a list")
        .to_string();
    assert!(said.contains("must be an array of step tables"), "{said}");
}

/// A runner that answers each command from a script and records what it was asked to run.
#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<(String, std::result::Result<Ran, RunError>)>>,
    asked: Mutex<Vec<Run>>,
}

impl Scripted {
    /// Answer the command whose argv contains `word` with `answer`.
    fn on(self, word: &str, answer: std::result::Result<Ran, RunError>) -> Self {
        self.answers
            .lock()
            .expect("answers")
            .push((word.to_string(), answer));
        self
    }

    fn asked(&self) -> Vec<Run> {
        self.asked.lock().expect("asked").clone()
    }
}

impl Runner for Scripted {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        self.asked.lock().expect("asked").push(run.clone());
        let line = format!("{} {}", run.program.display(), run.argv().join(" "));
        self.answers
            .lock()
            .expect("answers")
            .iter()
            .find(|(word, _)| line.contains(word.as_str()))
            .map_or_else(
                || panic!("the script has no answer for `{line}`"),
                |(_, answer)| answer.clone(),
            )
    }
}

fn exit(code: i32, stdout: &str, stderr: &str) -> std::result::Result<Ran, RunError> {
    Ok(Ran {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

fn summary(passed: u64, failed: u64, filtered: u64) -> String {
    format!(
        "running {} tests\n\ntest result: {}. {passed} passed; {failed} failed; 0 ignored; 0 \
         measured; {filtered} filtered out; finished in 0.01s\n",
        passed + failed,
        if failed == 0 { "ok" } else { "FAILED" }
    )
}

fn source_site<'a>(checkout: &'a Path, logs: &'a Path) -> Site<'a> {
    Site {
        cwd: checkout,
        packed: None,
        logs,
    }
}

fn why(receipt: &Receipt) -> &str {
    match &receipt.verdict {
        Verdict::Failed(why) => why,
        Verdict::Passed => panic!("{} passed", receipt.line()),
    }
}

#[test]
fn a_passing_step_leaves_its_exit_bound_and_the_digests_of_its_kept_output() {
    let tmp = scratch("qualify-pass");
    let (checkout, logs) = (tmp.path().join("src"), tmp.path().join("logs"));
    std::fs::create_dir_all(&checkout).expect("mkdir");
    let plan = plan(&[FMT, BUILD, VERSION]);
    let runner = Scripted::default().on("fmt --check", exit(0, "formatted\n", "note\n"));
    let receipt = qualify::run_step(&runner, &plan.source()[0], &source_site(&checkout, &logs))
        .expect("the step ran");
    assert_eq!(receipt.verdict, Verdict::Passed, "{}", receipt.line());
    assert_eq!((receipt.exit, receipt.within_secs), (Some(0), 120));
    assert_eq!(receipt.argv, ["targo", "fmt", "--check"]);
    assert_eq!(receipt.tests, None, "a formatter prints no libtest summary");
    // The raw output is kept, and the receipt's digests are of exactly those bytes.
    assert_eq!(
        std::fs::read(logs.join("fmt.stdout")).expect("stdout"),
        b"formatted\n"
    );
    assert_eq!(
        receipt.stdout_sha256,
        atpkg::tree::sha256_file(&logs.join("fmt.stdout")).expect("hash")
    );
    assert_eq!(
        receipt.stderr_sha256,
        atpkg::tree::sha256_file(&logs.join("fmt.stderr")).expect("hash")
    );
    // It ran in the frozen checkout, ended at its committed bound, without the caller's
    // compiler-policy overrides.
    let asked = &runner.asked()[0];
    assert_eq!(asked.cwd.as_deref(), Some(checkout.as_path()));
    assert_eq!(asked.deadline, Some(Duration::from_secs(120)));
    assert!(asked.env_remove.iter().any(|k| k == "RUSTFLAGS"));
}

#[test]
fn a_run_that_matched_zero_tests_is_a_failure_though_it_exits_0() {
    let tmp = scratch("qualify-zero");
    let plan = plan(&[
        r#"{"name":"filter","stage":"source","run":["targo","--unverified","test","no_such_test"],"within-secs":60}"#,
        BUILD,
        VERSION,
    ]);
    // Two test binaries, each of which the filter emptied.
    let stdout = format!("{}{}", summary(0, 0, 12), summary(0, 0, 3));
    let runner = Scripted::default().on("no_such_test", exit(0, &stdout, ""));
    let receipt = qualify::run_step(
        &runner,
        &plan.source()[0],
        &source_site(tmp.path(), &tmp.path().join("logs")),
    )
    .expect("the step ran");
    assert_eq!(receipt.exit, Some(0));
    assert!(
        why(&receipt).contains("matched zero tests"),
        "{}",
        receipt.line()
    );
    let tests = receipt.tests.expect("two summaries");
    assert_eq!((tests.summaries, tests.ran()), (2, 0));

    // One binary with nothing to run beside one that ran is an ordinary workspace: a pass.
    let stdout = format!("{}{}", summary(0, 0, 0), summary(7, 0, 0));
    let runner = Scripted::default().on("no_such_test", exit(0, &stdout, ""));
    let receipt = qualify::run_step(
        &runner,
        &plan.source()[0],
        &source_site(tmp.path(), &tmp.path().join("logs")),
    )
    .expect("the step ran");
    assert_eq!(receipt.verdict, Verdict::Passed, "{}", receipt.line());
    assert_eq!(receipt.tests.expect("summaries").passed, 7);
}

#[test]
fn a_declared_test_floor_counts_what_passed_across_every_summary() {
    let tmp = scratch("qualify-floor");
    let plan = plan(&[UNIT, BUILD, VERSION]);
    let logs = tmp.path().join("logs");
    let site = source_site(tmp.path(), &logs);
    let run = |stdout: &str, stderr: &str| {
        let runner = Scripted::default().on("test --locked", exit(0, stdout, stderr));
        qualify::run_step(&runner, &plan.source()[0], &site).expect("the step ran")
    };
    // libtest writes its summaries to stdout; a harness may route them through stderr.
    let across = run(&summary(25, 0, 0), &summary(15, 0, 0));
    assert_eq!(across.verdict, Verdict::Passed, "{}", across.line());
    let short = run(&format!("{}{}", summary(25, 0, 0), summary(14, 0, 0)), "");
    assert!(
        why(&short).contains("39 tests passed, fewer than its floor of 40"),
        "{}",
        short.line()
    );
    let silent = run("compiled, ran nothing\n", "");
    assert!(
        why(&silent).contains("printed no test result"),
        "{}",
        silent.line()
    );
}

#[test]
fn a_step_that_fails_or_outlives_its_bound_is_not_a_pass() {
    let tmp = scratch("qualify-red");
    let plan = plan(&[FMT, UNIT, BUILD, VERSION]);
    let logs = tmp.path().join("logs");
    let site = source_site(tmp.path(), &logs);
    let red = Scripted::default().on("fmt --check", exit(1, "", "Diff in src/lib.rs\n"));
    let receipt = qualify::run_step(&red, &plan.source()[0], &site).expect("the step ran");
    assert_eq!(receipt.exit, Some(1));
    assert!(why(&receipt).contains("exited 1"), "{}", receipt.line());
    assert_eq!(
        std::fs::read(tmp.path().join("logs/fmt.stderr")).expect("the raw failure is kept"),
        b"Diff in src/lib.rs\n"
    );

    // A failing test run that still printed a passing count is a failure by its exit.
    let failing = Scripted::default().on("test --locked", exit(101, &summary(60, 1, 0), ""));
    let receipt = qualify::run_step(&failing, &plan.source()[1], &site).expect("the step ran");
    assert!(why(&receipt).contains("exited 101"), "{}", receipt.line());

    let hung = Scripted::default().on("test --locked", Err(RunError::TimedOut));
    let receipt = qualify::run_step(&hung, &plan.source()[1], &site).expect("the step ran");
    assert_eq!(receipt.exit, None);
    assert!(
        why(&receipt).contains("did not finish within its committed bound of 1800 s"),
        "{}",
        receipt.line()
    );

    let absent = Scripted::default().on("fmt --check", Err(RunError::Spawn("no targo".into())));
    let receipt = qualify::run_step(&absent, &plan.source()[0], &site).expect("the step ran");
    assert!(
        why(&receipt).contains("could not start: no targo"),
        "{}",
        receipt.line()
    );
}

#[test]
fn the_host_runner_keeps_a_commands_output_and_ends_one_at_its_bound() {
    use atpkg_publish::artifact::tools::HostTools;
    let tmp = scratch("qualify-host");
    let logs = tmp.path().join("logs");
    let site = source_site(tmp.path(), &logs);
    let step = |name: &str, run: &[&str], within: u64| qualify::Step {
        name: name.into(),
        stage: Stage::Source,
        run: run.iter().map(|w| (*w).to_string()).collect(),
        within: Duration::from_secs(within),
        min_tests: None,
    };
    // A real child: its exit is the receipt's, its streams are the logs, and it ran where the
    // site says.
    let said = step(
        "said",
        &["/bin/sh", "-c", "pwd; echo to-stderr >&2; exit 3"],
        60,
    );
    let receipt = qualify::run_step(&HostTools, &said, &site).expect("the step ran");
    assert_eq!(receipt.exit, Some(3), "{}", receipt.line());
    assert!(why(&receipt).contains("exited 3"), "{}", receipt.line());
    let stdout = std::fs::read_to_string(logs.join("said.stdout")).expect("kept");
    assert_eq!(
        std::fs::canonicalize(stdout.trim()).expect("the working directory"),
        std::fs::canonicalize(tmp.path()).expect("the site")
    );
    assert_eq!(
        std::fs::read(logs.join("said.stderr")).expect("kept"),
        b"to-stderr\n"
    );
    // A child that outlives its bound is ended at it, with what it started: the step returns
    // long before the child would have, and is a failure naming the bound.
    let hung = step("hung", &["/bin/sh", "-c", "sleep 600 & wait"], 1);
    let started = std::time::Instant::now();
    let receipt = qualify::run_step(&HostTools, &hung, &site).expect("the step ran");
    assert!(
        started.elapsed() < Duration::from_secs(300),
        "the bound did not end it"
    );
    assert_eq!(receipt.exit, None);
    assert!(
        why(&receipt).contains("did not finish within its committed bound of 1 s"),
        "{}",
        receipt.line()
    );
}

#[test]
fn a_packed_control_runs_the_unpacked_tree_from_outside_the_checkout() {
    let tmp = scratch("qualify-packed");
    let (root, cwd, logs) = (
        tmp.path().join("unpacked"),
        tmp.path().join("cwd"),
        tmp.path().join("logs"),
    );
    let plan = plan(&[
        BUILD,
        r#"{"name":"tree","stage":"packed","run":["{bin}/ay","--check","{root}/share/cases"],"within-secs":30}"#,
    ]);
    let runner = Scripted::default().on("--check", exit(0, "ok\n", ""));
    let site = Site {
        cwd: &cwd,
        packed: Some((&root, ARM)),
        logs: &logs,
    };
    let receipt = qualify::run_step(&runner, &plan.packed()[0], &site).expect("the step ran");
    assert_eq!(receipt.verdict, Verdict::Passed, "{}", receipt.line());
    assert_eq!(receipt.target, Some(ARM));
    let bin = root.join("bin/ay").display().to_string();
    let cases = root.join("share/cases").display().to_string();
    assert_eq!(receipt.argv, [bin.as_str(), "--check", cases.as_str()]);
    let asked = &runner.asked()[0];
    assert_eq!(asked.program, root.join("bin/ay"));
    assert_eq!(asked.cwd.as_deref(), Some(cwd.as_path()));
    assert!(logs.join(format!("tree.{ARM}.stdout")).is_file());

    // With no tree to run, a packed step is a usage error, never a pass over nothing.
    let nowhere = Site {
        cwd: &cwd,
        packed: None,
        logs: &logs,
    };
    assert!(qualify::run_step(&runner, &plan.packed()[0], &nowhere).is_err());
    assert!(qualify::run_step(&runner, plan.build(), &site).is_err());
}

#[test]
fn the_steps_stop_at_the_first_that_does_not_pass() {
    let tmp = scratch("qualify-stop");
    let plan = plan(&[FMT, UNIT, BUILD, VERSION]);
    let runner = Scripted::default()
        .on("fmt --check", exit(1, "", "unformatted\n"))
        .on("test --locked", exit(0, &summary(50, 0, 0), ""));
    let receipts = qualify::run_steps(
        &runner,
        plan.source(),
        &source_site(tmp.path(), &tmp.path().join("logs")),
    )
    .expect("the steps ran");
    assert_eq!(
        receipts.len(),
        1,
        "the test suite never ran behind a red formatter"
    );
    assert!(!receipts[0].passed());
    assert_eq!(runner.asked().len(), 1);

    let green = Scripted::default()
        .on("fmt --check", exit(0, "", ""))
        .on("test --locked", exit(0, &summary(50, 0, 0), ""));
    let receipts = qualify::run_steps(
        &green,
        plan.source(),
        &source_site(tmp.path(), &tmp.path().join("logs")),
    )
    .expect("the steps ran");
    assert!(receipts.iter().all(Receipt::passed));
    assert_eq!(receipts.len(), 2);
}

#[test]
fn the_builds_bound_is_judged_when_the_pack_returns() {
    let plan = plan(&[BUILD, VERSION]);
    let built = Receipt::build(plan.build(), Duration::from_secs(900), Ok(()));
    assert_eq!(
        (built.exit, built.verdict.clone()),
        (Some(0), Verdict::Passed)
    );
    let slow = Receipt::build(plan.build(), Duration::from_secs(3601), Ok(()));
    assert!(
        why(&slow).contains("past its committed bound of 3600 s"),
        "{}",
        slow.line()
    );
    let refused = Receipt::build(
        plan.build(),
        Duration::from_secs(5),
        Err("the pack failed for aarch64-apple-darwin: no bin".into()),
    );
    assert_eq!(refused.exit, Some(1));
    assert!(why(&refused).contains("no bin"));
}
