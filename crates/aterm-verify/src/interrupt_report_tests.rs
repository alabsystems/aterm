// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An interrupt the run's own handler takes ends every stage group, then lets the run report
//! before it dies of the signal (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT, its
//! open interrupted-run log): a stand-in run in a process of its own (`fmt` and `lint`: a stage
//! that passes at once beside a tippy stage whose child naps), its log a file and its disk
//! claimed in a scratch ledger, is sent one SIGINT while the tippy stage's child runs. Its log
//! then holds both stages' reports, the verdict and its disk claim's end, which keeps no
//! footprint, its scratch is gone, and it still dies of SIGINT. One that never gets to its end
//! is held to the watchdog's grace.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading, admit_with};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::checks::Check;
use crate::exec::{Cmd, run};
use crate::ladder::Report;
use crate::plan::{StageId, StageSpec};
use crate::{Ctx, EnvSnapshot, Scope, Seams, Show};

/// Set, the test binary is [`standin_run_reports_once_interrupted`], its log and its markers
/// under this directory; its value's last component says whether the run then hangs.
const STANDIN: &str = "ATERM_VERIFY_INTERRUPT_REPORT_STANDIN";
/// Set, the test binary is [`standin_napping_child`], which writes its pid into this directory.
const CHILD: &str = "ATERM_VERIFY_INTERRUPT_REPORT_CHILD";
/// Set, the stand-in run never reaches its end once it has reported: it naps where it would
/// die of the signal.
const HANGS: &str = "ATERM_VERIFY_INTERRUPT_REPORT_HANGS";

/// The watchdog's grace in the stand-in run: the hang detector for one that reports, since its
/// report must happen however loaded the host, and [`crate::exec::group::REPORT_GRACE`] for one
/// that [`HANGS`], whose grace must fire.
fn report_grace(hangs: bool) -> Duration {
    if hangs {
        crate::exec::group::REPORT_GRACE
    } else {
        MUST_HAPPEN
    }
}

/// The stand-in's directory.
fn standin_dir() -> PathBuf {
    PathBuf::from(std::env::var_os(STANDIN).expect("the stand-in's directory"))
}

/// The stand-in run's admissions: at once, in a scratch ledger on an idle machine.
fn admit_in_scratch(need: Need, _: Admit) -> Result<Claim, Error> {
    admit_with(
        &Ledger::at(standin_dir().join("ledger")),
        &Fixed::new(Reading::of_gib(48, 40, 14)),
        need,
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
}

/// The stand-in run's stages: tippy runs [`standin_napping_child`] as a stage child, which naps
/// until the run's end ends it; any other passes at once, noting that it ran.
fn stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let dir = standin_dir();
    let mut r = Report::new(spec.title.clone());
    if spec.id == StageId::Tippy {
        let mut cmd = Cmd::new(std::env::current_exe().expect("test binary"));
        cmd.args = [
            "--exact",
            "interrupt_report_tests::standin_napping_child",
            "--test-threads=1",
            "--nocapture",
        ]
        .map(Into::into)
        .to_vec();
        cmd.envs = vec![(CHILD.into(), dir.into_os_string())];
        let child = run(&cmd, ctx.exec_env());
        r.decide_child(&child, "the napping child");
    } else {
        std::fs::write(dir.join("ran"), &spec.title).expect("said it ran");
        r.pass("the quick stage ran");
    }
    r
}

/// Run as a stage child by [`stage`]: says it started, then naps for twice its grandparent's
/// hang detector, so only the run's end ends it in time.
#[test]
fn standin_napping_child() {
    let Some(dir) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    std::fs::write(dir.join("child"), std::process::id().to_string()).expect("said started");
    std::thread::sleep(MUST_HAPPEN * 2);
}

/// The stand-in run's disk: its record of what runs of its scope wrote, which an interrupted
/// run leaves as it was, and the directory it writes, under `dir`.
fn standin_disk(dir: &Path) -> atpkg::admit_disk::Run {
    atpkg::admit_disk::Run {
        what: "verify fmt lint -p aterm-capacity".to_owned(),
        scope: "lint -p aterm-capacity".to_owned(),
        dirs: vec![dir.join("build")],
        record: dir.join("footprint.json"),
        cold: 1 << 30,
        on: dir.to_path_buf(),
        stuck_ends: None,
        machine: None,
    }
}

/// Run by the tests below in a process of its own: a verify run of `fmt` and `lint`, its
/// interrupt handling verify's own, held to [`report_grace`], its log a file and its disk claimed
/// ([`standin_disk`]), that ends as verify's does ([`crate::let_disk_go`], [`crate::end_run`])
/// unless [`HANGS`] says it never gets there.
#[test]
fn standin_run_reports_once_interrupted() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    crate::exec::group::set_report_grace_for_test(report_grace(std::env::var_os(HANGS).is_some()));
    crate::exec::group::kill_on_interrupt();
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let ctx = Ctx::new(
        dir.clone(),
        Scope::crate_only("aterm-capacity"),
        EnvSnapshot::default(),
        scratch,
    )
    .with_checks(&[Check::Fmt, Check::Lint])
    .with_quiet(Some(Admit::Now));
    let mut log = std::fs::File::create(dir.join("log")).expect("the log");
    std::fs::create_dir_all(dir.join("build")).expect("the run's build directory");
    let disk = atpkg::admit_disk::admit_on(
        standin_disk(&dir),
        &Ledger::at(dir.join("ledger")),
        &Fixed::new(Reading::of_gib(48, 40, 14)),
        Admit::Now,
        Pace::default(),
        &mut atpkg::admit_disk::Reclaim::of(
            Box::new(|_, _| (0, Vec::new(), Vec::new())),
            Box::new(|_, _, _| None),
            Box::new(|_, _| Vec::new()),
        ),
        &mut |_| {},
    )
    .expect("the run's disk claim");
    std::fs::write(dir.join("build").join("written"), [0u8; 1 << 16]).expect("the run's writes");
    let code = crate::run_with(
        &ctx,
        &mut std::io::stdout(),
        &mut log,
        &Show::default(),
        Seams {
            stage,
            admit_quiet: admit_in_scratch,
            admit_memory: admit_in_scratch,
        },
    )
    .expect("the result written");
    if std::env::var_os(HANGS).is_some() {
        std::thread::sleep(MUST_HAPPEN * 2);
    }
    let scratch = ctx.scratch.clone();
    crate::let_disk_go(Some(disk), code == crate::exit::PASS, Some(&mut log));
    crate::end_run(&ctx, &scratch);
    std::process::exit(code);
}

/// A stand-in, killed when it is dropped still running (a test that failed before it ended),
/// so none outlives its test.
struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Start the stand-in run in `dir` (hanging at its end where `hangs`), send it one SIGINT once
/// its tippy stage's child runs and its quick stage has run, and return how it ended, how long
/// after the signal, and the stage child's pid.
fn interrupted(dir: &Path, hangs: bool) -> (std::process::ExitStatus, std::time::Duration, i32) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "interrupt_report_tests::standin_run_reports_once_interrupted",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(STANDIN, dir)
    .stdin(aterm_uds::spawnfd::Stdio::null())
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .stderr(aterm_uds::spawnfd::Stdio::null());
    if hangs {
        cmd.env(HANGS, "1");
    }
    let mut child = Standin(cmd.spawn().expect("the stand-in run"));
    let stage_child: i32 = until_within(
        MUST_HAPPEN,
        "the quick stage run and the tippy stage's child running",
        || {
            std::fs::metadata(dir.join("ran")).ok()?;
            std::fs::read_to_string(dir.join("child"))
                .ok()?
                .trim()
                .parse()
                .ok()
        },
    );
    let pid = i32::try_from(child.0.id()).expect("a pid");
    let sent = Instant::now();
    // SAFETY: `kill` takes two integers; the pid is this test's own child.
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGINT) },
        0,
        "signalled the run"
    );
    let status = until_within(report_grace(hangs) + MUST_HAPPEN, "the run's end", || {
        child.0.try_wait().ok().flatten()
    });
    (status, sent.elapsed(), stage_child)
}

/// One SIGINT while the tippy stage's child runs: the run still dies of SIGINT once it has
/// reported, its report held to the hang detector rather than the watchdog's shipped grace (the
/// time is reported), and its log holds the quick stage's report,
/// the tippy stage's (the child ended by the run's end, which decided nothing), the verdict,
/// COULD NOT RUN, and its disk claim's end, which keeps nothing: the record of what runs of its
/// scope wrote is as it was, so the next run claims no less for what this one never wrote. Its
/// scratch is gone, and so is the stage child. Negative controls: a handler that re-raises the
/// signal as soon as the groups are ended (before this step) kills the run before its log holds
/// the verdict; a run that keeps its disk claim's footprint whatever ended it adds the
/// interrupted run to the record.
#[test]
fn an_interrupted_run_writes_its_log_before_it_dies() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-int-report-")
        .tempdir()
        .expect("a scratch dir");
    let record = standin_disk(dir.path()).record;
    let mut kept = aterm_capacity::Footprints::default();
    kept.push(aterm_capacity::Footprint {
        peak: 3 << 30,
        grew: 2 << 30,
        at_ms: 1,
        dirs: Some(atpkg::admit_disk::dirs_tag(&standin_disk(dir.path()).dirs)),
        scope: atpkg::admit_disk::scope_tag(&standin_disk(dir.path()).scope),
    });
    kept.save(&record).expect("the record of the scope's runs");
    let before = std::fs::read(&record).expect("the record");
    let (status, took, stage_child) = interrupted(dir.path(), false);
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap_or_default();
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status}\n{log}");
    assert!(log.contains("the quick stage ran"), "{log}");
    assert!(
        log.contains("the napping child — the run was interrupted while this child ran"),
        "{log}"
    );
    assert!(
        log.lines()
            .any(|l| l.starts_with("verify: COULD NOT RUN — lint could not run")),
        "the verdict: {log}"
    );
    assert_eq!(
        std::fs::read(&record).ok(),
        Some(before),
        "the interrupted run was kept as what its scope writes"
    );
    assert!(
        log.lines().any(|l| l.starts_with("disk: claimed")
            && l.ends_with("; the run was interrupted, so that is not kept for its scope")),
        "the disk claim's end: {log}"
    );
    assert!(!dir.path().join("scratch").exists(), "the run's scratch");
    until_within(MUST_HAPPEN, "the stage child gone", || {
        // SAFETY: signal 0 only asks whether the pid names a process.
        (unsafe { libc::kill(stage_child, 0) } != 0).then_some(())
    });
    eprintln!("SIGINT ended the run, reported, {took:?} after it was sent");
}

/// A run that reports but never reaches its end still dies of the SIGINT, by its watchdog,
/// [`crate::exec::group::REPORT_GRACE`] after the groups were ended and inside the hang detector
/// past that grace. Negative control: with no watchdog the run naps on past the detector.
#[test]
fn an_interrupted_run_that_never_ends_dies_at_the_watchdogs_grace() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-int-watchdog-")
        .tempdir()
        .expect("a scratch dir");
    let (status, took, _) = interrupted(dir.path(), true);
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status}");
    assert!(
        took >= crate::exec::group::REPORT_GRACE,
        "it died before its grace: {took:?}"
    );
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap_or_default();
    assert!(
        log.lines().any(|l| l.starts_with("verify: COULD NOT RUN")),
        "it reported before it hung: {log}"
    );
    eprintln!("the watchdog ended the run {took:?} after the SIGINT");
}
