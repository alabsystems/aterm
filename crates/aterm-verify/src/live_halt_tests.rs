// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A halted run ends a test binary at its first decisive red case (ATERM-KI-1105-18): a stand-in
//! run, in a process of its own since a halt ends every stage child of its process, drives
//! [`crate::testrun::run_planned`] over two binaries one at a time. Binary A is this test binary
//! filtered to [`crate::libtest_fixture`]'s cases for the arm, ending in the napper, which waits
//! for a release file only the test writes; binary B writes a marker. Each arm reads the stand-in's
//! rows, its rendered log and its events.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::libtest_fixture::{FIXTURE, NONCE};
use crate::testrun::{Binary, Record, RunPlan};
use crate::{Ctx, EnvSnapshot, Scope};

/// Set (to a directory), the test binary is [`standin_live_run`], its rows, log and events there.
const STANDIN: &str = "ATERM_VERIFY_LIVE_HALT_STANDIN";
/// The stand-in's arm: the fixture cases binary A runs, and whether its halt is armed.
const ARM: &str = "ATERM_VERIFY_LIVE_HALT_ARM";
/// Set (to a directory), the test binary is [`standin_marker`], which writes `b` there.
const MARKER: &str = "ATERM_VERIFY_LIVE_HALT_MARKER";

/// The fixture cases of an arm, before the napper, and whether its halt is armed.
fn arm(name: &str) -> (&'static [&'static str], bool) {
    match name {
        "red" => (&["c_plain"], true),
        "refusal" => (&["c_refused"], true),
        "disk" => (&["d_enospc"], true),
        "mixed" => (&["c_refused", "d_enospc"], true),
        "bare" => (&["c_plain"], false),
        "long" => (&[], true),
        other => panic!("no arm {other}"),
    }
}

fn binary(spec: &str, argv: Vec<OsString>, extra: (&str, &Path)) -> Binary {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| k != STANDIN && k != ARM && k != "RUST_TEST_NOCAPTURE")
        .collect();
    env.push((extra.0.into(), extra.1.as_os_str().to_owned()));
    env.push(("CARGO_PKG_NAME".into(), "aterm-verify".into()));
    Binary {
        header: format!("     Running {spec} ({})", argv[0].to_string_lossy()),
        spec: spec.into(),
        record: Record {
            cwd: std::env::current_dir().expect("a working directory"),
            argv,
            env,
        },
        harness: true,
    }
}

/// Run as binary B: says it ran.
#[test]
fn standin_marker() {
    if let Some(dir) = std::env::var_os(MARKER).map(PathBuf::from) {
        std::fs::write(dir.join("b"), "").expect("the marker");
    }
}

/// The stand-in run, run by the tests below in a process of its own: `run_planned` over A and B,
/// one at a time, its rows (`<word>\t<label>`) in `rows`, its block in `block`, its findings in
/// `findings`; its events go to its stderr, which the test keeps.
#[test]
fn standin_live_run() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let (cases, armed) = arm(&std::env::var(ARM).expect("an arm"));
    let exe: OsString = std::env::current_exe().expect("this binary").into();
    let mut argv_a = vec![exe.clone()];
    argv_a.extend(
        cases
            .iter()
            .map(|c| OsString::from(format!("libtest_fixture::{c}"))),
    );
    argv_a.extend(["libtest_fixture::z_napper".into(), "--exact".into()]);
    let mut a = binary("-p aterm-verify --lib a", argv_a, (FIXTURE, &dir));
    if std::env::var(ARM).as_deref() == Ok("long") {
        a.record
            .env
            .push((crate::libtest_fixture::LONG_RUNNING.into(), "1".into()));
    }
    let argv_b = vec![
        exe,
        "live_halt_tests::standin_marker".into(),
        "--exact".into(),
    ];
    let b = binary("-p aterm-verify --lib b", argv_b, (MARKER, &dir));
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let ctx = Ctx::new(
        dir.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        scratch,
    )
    .with_halt(crate::halt_of(false, armed));
    let mut r = crate::ladder::Report::new("test");
    let planned = RunPlan::fixture(vec![a, b], 1);
    crate::testrun::run_planned(&ctx, &mut r, "test", planned, crate::checks::Check::Test);
    let rows: String = r
        .outcomes()
        .map(|(o, label)| format!("{}\t{label}\n", crate::report::word(o)))
        .collect();
    let findings: String = r
        .findings()
        .iter()
        .map(|f| format!("{}\n{}\n", f.id, f.message.join("\n")))
        .collect();
    std::fs::write(dir.join("rows"), rows).expect("the rows");
    std::fs::write(dir.join("findings"), findings).expect("the findings");
    std::fs::write(dir.join("block"), r.render()).expect("the block");
    std::fs::write(dir.join("done"), "").expect("done");
}

/// What one arm's stand-in run left.
struct Ran {
    rows: String,
    findings: String,
    block: String,
    events: String,
    b_ran: bool,
}

/// A stand-in, killed when it is dropped still running (a test that failed before it ended).
struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Run the stand-in for `arm` in `dir`; `during` runs while it runs, given the stand-in's
/// events so far.
fn run_arm(dir: &Path, arm: &str, during: impl FnOnce(&dyn Fn() -> String)) -> Ran {
    let events = dir.join("events");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "live_halt_tests::standin_live_run",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(STANDIN, dir)
    .env(ARM, arm)
    .env_remove(FIXTURE)
    .stdin(aterm_uds::spawnfd::Stdio::null())
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .stderr(std::fs::File::create(&events).expect("the events file"));
    let mut child = Standin(cmd.spawn().expect("the stand-in"));
    let read = || std::fs::read_to_string(&events).unwrap_or_default();
    during(&read);
    let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
        child.0.try_wait().ok().flatten()
    });
    let text = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    assert!(
        status.success() && dir.join("done").exists(),
        "{status}\n{}",
        read()
    );
    Ran {
        rows: text("rows"),
        findings: text("findings"),
        block: text("block"),
        events: read(),
        b_ran: dir.join("b").exists(),
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atv-live-")
        .tempdir()
        .expect("a scratch dir")
}

/// The words of each row, in order.
fn words(rows: &str) -> Vec<&str> {
    rows.lines().filter_map(|l| l.split('\t').next()).collect()
}

/// THE FIRST DECISIVE RED CASE ENDS ITS BINARY AND THE RUN: A's plain red ends A while its napper
/// still waits for a release nobody writes, and B never starts. The row is FAIL with the red's
/// finding and its nonce, the log says verify stopped A and holds no napper result, and the NOT
/// RUN row names one of two binaries. NEGATIVE CONTROL: with no live halt the napper would wait
/// out the hang detector and fail the stand-in's end.
#[test]
fn a_decisive_red_case_ends_its_binary_and_the_run() {
    let dir = scratch();
    let ran = run_arm(dir.path(), "red", |_| {});
    let all = format!("{}\n{}\n{}", ran.rows, ran.block, ran.events);
    assert_eq!(words(&ran.rows), ["FAIL", "NOT RUN"], "{all}");
    assert!(
        ran.findings.contains("libtest_fixture::c_plain") && ran.findings.contains(NONCE),
        "{all}"
    );
    assert!(ran.block.contains("aterm-verify: stopped -p aterm-verify --lib a at its first red case `libtest_fixture::c_plain`"), "{all}");
    assert!(!ran.block.contains("z_napper ... ok"), "{all}");
    assert!(!ran.b_ran, "{all}");
    assert!(ran.rows.contains("1 of 2 test binaries not run"), "{all}");
    assert!(ran.events.contains("\"halts\":true"), "{all}");
    assert!(
        ran.events
            .contains("\"stopped_at\":\"libtest_fixture::c_plain\""),
        "{all}"
    );
}

/// Wait for a spared case's `test-case-failed`, see A still running, release its napper.
fn release_after_spared(dir: &Path, read: &dyn Fn() -> String, cases: usize) {
    until_within(MUST_HAPPEN, "the spared cases said", || {
        (read().matches("\"halts\":false").count() >= cases).then_some(())
    });
    let started = until_within(MUST_HAPPEN, "the napper started", || {
        std::fs::read_to_string(dir.join("napper.started")).ok()
    });
    let pid: i32 = started.trim().parse().expect("a pid");
    // SAFETY: signal 0 only asks whether the pid names a process.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "A runs on past a spared case"
    );
    std::fs::write(dir.join("napper.release"), "").expect("the release");
}

/// A REFUSAL AND A FULL DISK HALT NOTHING: A's spared case is said at once, A runs on until its
/// napper is released, A reads NOT RUN for its machine, its case's event reads NOT RUN, B runs,
/// and nothing halts. NEGATIVE CONTROL: the red arm above halts at its case.
#[test]
fn a_refused_or_out_of_disk_case_ends_nothing() {
    for (arm, says) in [
        ("refusal", "refused the machine"),
        ("disk", "ran out of disk"),
    ] {
        let dir = scratch();
        let ran = run_arm(dir.path(), arm, |read| {
            release_after_spared(dir.path(), read, 1)
        });
        let all = format!("{}\n{}\n{}", ran.rows, ran.block, ran.events);
        assert!(ran.b_ran, "{arm}: {all}");
        assert_eq!(words(&ran.rows), ["NOT RUN"], "{arm}: {all}");
        assert!(ran.rows.contains(says), "{arm}: {all}");
        assert!(!ran.rows.contains("stopped:"), "{arm}: {all}");
        assert!(!ran.events.contains("\"halts\":true"), "{arm}: {all}");
        let cases: Vec<&str> = ran
            .events
            .lines()
            .filter(|l| l.contains("\"test-case-failed\""))
            .collect();
        assert!(!cases.is_empty(), "{arm}: {all}");
        assert!(
            cases.iter().all(|l| l.contains("\"outcome\":\"NOT RUN\"")),
            "{arm}: a spared case reads NOT RUN: {all}"
        );
    }
}

/// A REFUSAL THEN A FULL DISK IS RED AT THE SECOND: neither every-refused nor every-out-of-disk
/// can hold once both are seen. The refused case keeps its own NOT RUN row beside the full disk's
/// FAIL, as a mixed binary's report always reads (`findings::split_test_findings`).
#[test]
fn a_refusal_then_a_full_disk_halts_at_the_second() {
    let dir = scratch();
    let ran = run_arm(dir.path(), "mixed", |_| {});
    let all = format!("{}\n{}\n{}", ran.rows, ran.block, ran.events);
    assert_eq!(words(&ran.rows), ["NOT RUN", "FAIL", "NOT RUN"], "{all}");
    assert!(ran.rows.contains("1 of 2 test binaries not run"), "{all}");
    assert!(
        ran.events
            .contains("\"stopped_at\":\"libtest_fixture::d_enospc\""),
        "{all}"
    );
    assert!(!ran.b_ran, "{all}");
}

/// A BARE RUN KEEPS ITS SWEEP: its red is said at once, ends nothing, and the napper and B run.
#[test]
fn a_bare_run_says_its_red_and_ends_nothing() {
    let dir = scratch();
    let ran = run_arm(dir.path(), "bare", |read| {
        release_after_spared(dir.path(), read, 1)
    });
    let all = format!("{}\n{}\n{}", ran.rows, ran.block, ran.events);
    assert_eq!(words(&ran.rows), ["FAIL"], "{all}");
    assert!(ran.block.contains("z_napper ... ok"), "{all}");
    assert!(ran.b_ran, "{all}");
}

#[test]
fn a_long_running_case_is_journaled_before_completion_and_halts_nothing() {
    let dir = scratch();
    let mut held = None;
    let ran = run_arm(dir.path(), "long", |read| {
        let event = until_within(MUST_HAPPEN, "the live long-running case event", || {
            read().lines().find_map(|line| {
                let event: aterm_json::Value =
                    aterm_json::from_str(line.strip_prefix("verify-event: ")?).ok()?;
                (event["event"].as_str() == Some("test-case-running-long")).then_some(event)
            })
        });
        let phase = read()
            .lines()
            .filter_map(|line| {
                aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ")?).ok()
            })
            .rfind(|event| {
                matches!(
                    event["event"].as_str(),
                    Some("test-phase-started" | "test-phase-ended")
                )
            });
        held = Some((
            event,
            phase,
            crate::status::observe(&read()),
            dir.path().join("napper.started").is_file(),
            dir.path().join("b").exists(),
        ));
        std::fs::write(dir.path().join("napper.release"), "").expect("release the case");
    });
    let (event, phase, observed, started, b_started) = held.expect("the held snapshot");
    assert_eq!(event["case"].as_str(), Some("libtest_fixture::z_napper"));
    assert_eq!(event["test"].as_str(), Some("-p aterm-verify --lib a"));
    assert_eq!(event["outcome"].as_str(), Some("RUNNING"));
    assert_eq!(event["halts"].as_bool(), Some(false));
    assert_eq!(event["binary_completed"].as_bool(), Some(false));
    assert!(started);
    assert!(!b_started, "the first binary still owned its turn");
    let phase = phase.expect("the live dispatch phase");
    assert_eq!(phase["event"].as_str(), Some("test-phase-started"));
    assert_eq!(phase["phase"].as_str(), Some("dispatch"));
    assert_eq!(observed.long_cases.len(), 1);
    assert_eq!(observed.long_cases[0].test, "-p aterm-verify --lib a");
    assert_eq!(observed.long_cases[0].case, "libtest_fixture::z_napper");
    assert_eq!(observed.long_cases[0].occurrences, 1);
    assert_eq!(
        crate::status::observe(&ran.events).long_cases,
        observed.long_cases
    );
    let all = format!("{}\n{}\n{}", ran.rows, ran.block, ran.events);
    assert!(ran.b_ran, "{all}");
    assert_eq!(words(&ran.rows), ["ok"], "{all}");
    assert!(ran.findings.is_empty(), "{all}");
    assert!(ran.block.contains("z_napper ... ok"), "{all}");
    assert!(!ran.events.contains("\"test-case-failed\""), "{all}");
}
