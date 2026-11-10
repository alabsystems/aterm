// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first proven red ends a `--fail-fast` run as it ends a release (design §4.3, A3): a
//! stand-in run in a process of its own (`test` and `doc`), whose test stage fails once the
//! doctest stage runs a child that would nap past the hang detector. The halt ends that child's
//! group, the run's verdict reads FAIL with doc NOT RUN, stopped at the first red, and the run
//! exits inside the hang detector rather than after the nap. An interrupt that reaches the halted
//! run is its first: it reports, then dies of it.

use std::path::PathBuf;
use std::time::Instant;

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading, admit_with};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::checks::Check;
use crate::exec::{Cmd, run};
use crate::ladder::Report;
use crate::plan::{StageId, StageSpec};
use crate::{Ctx, EnvSnapshot, Scope, Seams, Show};

/// Set, the test binary is [`standin_halted_run`], its log and its markers under this directory.
const STANDIN: &str = "ATERM_VERIFY_HALT_STANDIN";
/// Set, the test binary is [`standin_napping_child`], which writes its pid into this directory.
const CHILD: &str = "ATERM_VERIFY_HALT_CHILD";
/// Set, the stand-in run's doctest stage, once the halt ended its child, waits for an interrupt
/// to reach the run before it returns.
const SIGNALLED: &str = "ATERM_VERIFY_HALT_SIGNALLED";

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

/// The stand-in run's stages: doctests run [`standin_napping_child`] as a stage child; the test
/// fails once that child runs, noting when; any other passes at once.
fn stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let dir = standin_dir();
    let mut r = Report::new(spec.title.clone());
    match spec.id {
        StageId::Doctests => {
            let mut cmd = Cmd::new(std::env::current_exe().expect("test binary"));
            cmd.args = [
                "--exact",
                "halt_report_tests::standin_napping_child",
                "--test-threads=1",
                "--nocapture",
            ]
            .map(Into::into)
            .to_vec();
            cmd.envs = vec![(CHILD.into(), dir.into_os_string())];
            let child = run(&cmd, ctx.exec_env());
            if std::env::var_os(SIGNALLED).is_some() {
                until_within(MUST_HAPPEN, "an interrupt after the halt", || {
                    (crate::exec::group::pending() != 0).then_some(())
                });
            }
            r.decide_child(&child, "the napping child");
        }
        StageId::Test => {
            until_within(MUST_HAPPEN, "the doctest stage's child running", || {
                std::fs::metadata(dir.join("child")).ok()
            });
            std::fs::write(dir.join("red"), "").expect("said it failed");
            r.fail("the planted red");
        }
        _ => r.pass("the quick stage ran"),
    }
    r
}

/// Run as a stage child by [`stage`]: says it started, then naps for twice its grandparent's
/// hang detector, so only the halt ends it in time.
#[test]
fn standin_napping_child() {
    let Some(dir) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    std::fs::write(dir.join("child"), std::process::id().to_string()).expect("said started");
    std::thread::sleep(MUST_HAPPEN * 2);
}

/// Run by the tests below in a process of its own, since a halt ends every stage child of its
/// process: a `--fail-fast` verify run of `test` and `doc` that takes interrupts as verify does,
/// its log a file, that exits with its verdict's code, or dies of an interrupt once it reported.
#[test]
fn standin_halted_run() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    // Its report is held to the hang detector, not the shipped grace, however loaded the host.
    crate::exec::group::set_report_grace_for_test(MUST_HAPPEN);
    crate::exec::group::kill_on_interrupt();
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let ctx = Ctx::new(
        dir.clone(),
        Scope::crate_only("aterm-capacity"),
        EnvSnapshot::default(),
        scratch.clone(),
    )
    .with_checks(&[Check::Test, Check::Doc])
    .with_quiet(Some(Admit::Now))
    .with_halt(crate::halt_of(false, true));
    let mut log = std::fs::File::create(dir.join("log")).expect("the log");
    let code = crate::run_with(
        &ctx,
        &mut std::io::sink(),
        &mut log,
        &Show::default(),
        Seams {
            stage,
            prepare_cells: None,
            admit_quiet: admit_in_scratch,
            admit_memory: admit_in_scratch,
        },
    )
    .expect("the result written");
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

/// The halted stand-in run in `dir`, its tippy stage waiting for an interrupt where `signalled`.
fn start(dir: &std::path::Path, signalled: bool) -> Standin {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "halt_report_tests::standin_halted_run",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(STANDIN, dir)
    .stdin(aterm_uds::spawnfd::Stdio::null())
    .stdout(aterm_uds::spawnfd::Stdio::null())
    .stderr(aterm_uds::spawnfd::Stdio::null());
    if signalled {
        cmd.env(SIGNALLED, "1");
    }
    Standin(cmd.spawn().expect("the stand-in run"))
}

/// The pid of the stage child the stand-in run in `dir` started, once that child has gone.
fn gone_child(dir: &std::path::Path) -> i32 {
    let child: i32 = std::fs::read_to_string(dir.join("child"))
        .expect("the child's pid")
        .trim()
        .parse()
        .expect("a pid");
    until_within(MUST_HAPPEN, "the stage child gone", || {
        // SAFETY: signal 0 only asks whether the pid names a process.
        (unsafe { libc::kill(child, 0) } != 0).then_some(())
    });
    child
}

/// The halted run's verdict.
const VERDICT: &str =
    "verify: FAIL — test failed; 1 not run (stopped at the first red: doc); 0 ok in ";

/// The test's red while the doctest child naps: the run ends that child, reads FAIL with doc not run
/// (stopped at the first red, its row saying so), and exits FAILED inside the hang detector, half
/// the child's nap. Negative control: without `--fail-fast` the test red does not stop the run; it
/// run waits out the nap, and this test's hang detector on its end fails it.
#[test]
fn the_first_red_ends_a_fail_fast_run_and_its_napping_child() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-halt-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch dir");
    let mut run = start(dir.path(), false);
    until_within(MUST_HAPPEN, "the test's red", || {
        std::fs::metadata(dir.path().join("red")).ok()
    });
    let red = Instant::now();
    let status = until_within(MUST_HAPPEN, "the run's end after its red", || {
        run.0.try_wait().ok().flatten()
    });
    let took = red.elapsed();
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap_or_default();
    assert_eq!(status.code(), Some(crate::exit::FAILED), "{status}\n{log}");
    assert!(
        log.lines().any(|l| l.starts_with(VERDICT)),
        "the verdict: {log}"
    );
    assert!(
        log.contains("the napping child — stopped: the run failed at test"),
        "the child the halt ended reads NOT RUN with its words: {log}"
    );
    gone_child(dir.path());
    eprintln!("the first red ended the run {took:?} after it");
}

/// An interrupt that reaches a run its first red already ended is the run's first: the run
/// writes its verdict, then dies of the signal. Negative control: a halt that ends the groups as
/// a wait's interrupt does (`stop_run`, which marks a later interrupt as the second) dies of the
/// signal at once, before its log holds the verdict.
#[test]
fn an_interrupt_after_the_halt_reports_before_the_run_dies() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-halt-int-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch dir");
    let mut run = start(dir.path(), true);
    until_within(MUST_HAPPEN, "the test's red", || {
        std::fs::metadata(dir.path().join("red")).ok()
    });
    gone_child(dir.path());
    let pid = i32::try_from(run.0.id()).expect("a pid");
    // SAFETY: `kill` takes two integers; the pid is this test's own child.
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGINT) },
        0,
        "signalled the run"
    );
    let status = until_within(MUST_HAPPEN * 2, "the run's end", || {
        run.0.try_wait().ok().flatten()
    });
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap_or_default();
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status}\n{log}");
    assert!(
        log.lines().any(|l| l.starts_with(VERDICT)),
        "it reported before it died: {log}"
    );
}
