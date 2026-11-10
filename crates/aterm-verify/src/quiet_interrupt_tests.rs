// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One interrupt in a timed stage's wait for the quiet machine ends the whole run as COULD NOT
//! RUN (`docs/DESIGN-adaptive-build-2026-10-24.md`, WAIT-INTERRUPT): a stand-in run in a process
//! of its own (`libc` and `appkit`: a compiler stage beside two timed stages in one lane), its
//! stages stand-ins and its quiet claim in a scratch ledger behind this test's, is signalled
//! once while its first timed stage waits and its compiler stage's child runs.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading, admit_with};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::checks::Check;
use crate::exec::{Cmd, run};
use crate::ladder::Report;
use crate::plan::{StageId, StageSpec};
use crate::{Ctx, EnvSnapshot, Scope, Seams, Show};

/// Set, the test binary is [`standin_run_waits_for_the_quiet_machine`], its ledger and its
/// markers under this directory.
const STANDIN: &str = "ATERM_VERIFY_QUIET_INTERRUPT_STANDIN";
/// Set, the test binary is [`standin_stage_child`], which writes its pid into this directory.
const CHILD: &str = "ATERM_VERIFY_QUIET_INTERRUPT_CHILD";

#[test]
fn unavailable_now_and_ledger_failures_never_invoke_the_timed_callback() {
    fn unavailable(_: Need, _: Admit) -> Result<Claim, Error> {
        Err(Error::Timing(aterm_capacity::TimingIssue::Unavailable))
    }
    fn now(_: Need, _: Admit) -> Result<Claim, Error> {
        Err(Error::Timing(aterm_capacity::TimingIssue::Now))
    }
    fn ledger(_: Need, _: Admit) -> Result<Claim, Error> {
        Err(Error::Ledger(std::io::Error::other(
            "injected unavailable ledger",
        )))
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let cases: [(Admit, crate::Admitter); 3] = [
        (Admit::Wait, unavailable),
        (Admit::Now, now),
        (Admit::Wait, ledger),
    ];
    for (mode, admit) in cases {
        let ctx = Ctx::new(
            scratch.path().to_owned(),
            Scope::crate_only("aterm-capacity"),
            EnvSnapshot::default(),
            scratch.path().join("scratch"),
        )
        .with_quiet(Some(mode));
        let quiet = std::sync::Mutex::new(crate::Quiet {
            claim: None,
            left: 1,
            admit,
        });
        let calls = std::cell::Cell::new(0);
        let result = crate::quietly(&ctx, &quiet, "injected timing refusal", || {
            calls.set(calls.get() + 1)
        });
        assert!(result.is_err());
        assert_eq!(
            calls.get(),
            0,
            "production quietly must not invoke an acceptance callback after failed admission"
        );
    }
}

/// The machine both sides read: idle, so only the quiet claim this test holds keeps the
/// stand-in waiting.
fn machine() -> Fixed {
    Fixed::new(Reading::of_gib(48, 40, 14))
}

/// Add `line` to the file at `path`.
fn note(path: &Path, line: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("a marker file");
    writeln!(f, "{line}").expect("noted");
}

/// The stand-in run's quiet admission: in the scratch ledger, looking every two minutes (twice
/// the parent's hang detector, so only an interrupt ends the wait in time), noting each
/// admission asked for in `admissions` and its first wait line in `waiting`.
fn admit_in_scratch(need: Need, mode: Admit) -> Result<Claim, Error> {
    let dir = PathBuf::from(std::env::var_os(STANDIN).expect("the stand-in's directory"));
    note(&dir.join("admissions"), &need.what);
    let waiting = dir.join("waiting");
    admit_with(
        &Ledger::at(dir.join("ledger")),
        &crate::timing_test_machine::Measured::new(&machine()),
        need,
        mode,
        Pace {
            poll: MUST_HAPPEN * 2,
            ..Pace::default()
        },
        &mut |line| {
            if !waiting.exists() {
                std::fs::write(&waiting, line).expect("said waiting");
            }
        },
    )
}

/// The stand-in run's stages: libc runs [`standin_stage_child`] as a stage child, which naps
/// until the run's end ends it; a timed stage notes in `ran` that it ran.
fn stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let dir = PathBuf::from(std::env::var_os(STANDIN).expect("the stand-in's directory"));
    let mut r = Report::new(spec.title.clone());
    if spec.id == StageId::LibcOracle {
        let mut cmd = Cmd::new(std::env::current_exe().expect("test binary"));
        cmd.args = [
            "--exact",
            "quiet_interrupt_tests::standin_stage_child",
            "--test-threads=1",
            "--nocapture",
        ]
        .map(Into::into)
        .to_vec();
        cmd.envs = vec![(CHILD.into(), dir.into_os_string())];
        let child = run(&cmd, ctx.exec_env());
        r.decide_child(&child, "the stage child");
    } else {
        note(&dir.join("ran"), &spec.title);
        r.pass("ran");
    }
    r
}

/// Run as a stage child by [`stage`]: says it started, then naps for twice its grandparent's
/// hang detector, so only the run's end ends it in time.
#[test]
fn standin_stage_child() {
    let Some(dir) = std::env::var_os(CHILD).map(PathBuf::from) else {
        return;
    };
    std::fs::write(dir.join("child"), std::process::id().to_string()).expect("said started");
    std::thread::sleep(MUST_HAPPEN * 2);
}

/// Run by [`one_interrupt_in_a_timed_stages_wait_ends_the_run`] in a process of its own: a
/// verify run of `libc` and `appkit` under `--admit wait`, its interrupt handling verify's own,
/// that exits as verify does.
#[test]
fn standin_run_waits_for_the_quiet_machine() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    crate::exec::group::kill_on_interrupt();
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let ctx = Ctx::new(
        dir.clone(),
        Scope::crate_only("aterm-capacity"),
        EnvSnapshot::default(),
        scratch,
    )
    .with_checks(&[Check::Libc, Check::Appkit])
    .with_quiet(Some(Admit::Wait));
    let mut out = std::io::stdout();
    let code = crate::run_with(
        &ctx,
        &mut out,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage,
            prepare_cells: None,
            admit_quiet: admit_in_scratch,
            admit_memory: admit_in_scratch,
        },
    )
    .expect("the result written");
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

/// One SIGTERM, or one SIGINT, sent while the run's first timed stage waits for the quiet
/// machine and its compiler stage's child runs, ends the run well inside the one-minute hang
/// detector (the time is reported): exit 3, COULD NOT RUN, and no FAIL: the stage child the
/// run's end killed decided nothing, and the second timed stage neither ran nor began a wait of
/// its own (one admission was asked for). Only this test's claim is left in the ledger, and the
/// stage child is gone. Negative control: with no look at the run's end before a timed stage
/// is admitted, the second timed stage waits anew and the run runs to the detector.
#[test]
fn one_interrupt_in_a_timed_stages_wait_ends_the_run() {
    for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-quiet-int-")
            .suffix(".noindex")
            .tempdir()
            .expect("a scratch dir");
        let ledger = Ledger::at(dir.path().join("ledger"));
        let held = admit_with(
            &ledger,
            &machine(),
            Need::new("holder").quiet(28.0),
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .expect("the holder's claim");
        let mut child = Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "quiet_interrupt_tests::standin_run_waits_for_the_quiet_machine",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(STANDIN, dir.path())
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the stand-in run"),
        );
        let stage_child: i32 = until_within(
            MUST_HAPPEN,
            "the run's quiet wait, its compiler stage's child running",
            || {
                std::fs::metadata(dir.path().join("waiting")).ok()?;
                std::fs::read_to_string(dir.path().join("child"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            },
        );
        let pid = i32::try_from(child.0.id()).expect("a pid");
        let sent = Instant::now();
        // SAFETY: `kill` takes two integers; the pid is this test's own child.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "signalled the run");
        let status = until_within(MUST_HAPPEN, "the run's end", || {
            child.0.try_wait().ok().flatten()
        });
        let took = sent.elapsed();
        let mut out = String::new();
        if let Some(mut o) = child.0.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut out);
        }
        assert_eq!(status.code(), Some(3), "{word}: {status}\n{out}");
        assert!(out.contains("verify: COULD NOT RUN"), "{word}:\n{out}");
        assert!(!out.contains("FAIL"), "{word}: a finding:\n{out}");
        assert!(
            out.contains(&format!("interrupted ({word}) while it waited")),
            "{word}: names the wait:\n{out}"
        );
        let asked = std::fs::read_to_string(dir.path().join("admissions")).unwrap_or_default();
        assert_eq!(asked.lines().count(), 1, "{word}: one wait: {asked}");
        assert!(
            !dir.path().join("ran").exists(),
            "{word}: a timed stage ran after the interrupt"
        );
        let left: Vec<String> = ledger
            .dir()
            .read_dir()
            .expect("the ledger")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
            .collect();
        assert_eq!(left.len(), 1, "{word}: only the holder's claim: {left:?}");
        assert!(left[0].ends_with(".claim"), "{word}: {left:?}");
        until_within(MUST_HAPPEN, "the stage child gone", || {
            // SAFETY: signal 0 only asks whether the pid names a process.
            (unsafe { libc::kill(stage_child, 0) } != 0).then_some(())
        });
        eprintln!("{word} ended the run {took:?} after it was sent");
        drop(held);
    }
}

/// How many quiet claims [`given_up`] was asked for.
static GIVEN_UP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A quiet claim's wait given up behind pid 4242, which has made no progress for 16 minutes
/// ([`aterm_capacity::Error::Stuck`]), for a need that asked to give up so.
fn given_up(need: Need, _mode: Admit) -> Result<Claim, Error> {
    assert!(need.quiet.is_some(), "only the quiet claim is asked for");
    assert!(
        need.stuck_ends.is_some(),
        "the quiet claim asks to give up behind a stuck holder"
    );
    GIVEN_UP.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let span = std::time::Duration::from_secs(16 * 60);
    let what = "verify perf".to_owned();
    Err(Error::Stuck {
        stuck: aterm_toolchain::waits::Stuck {
            holders: vec![4242],
            span,
        },
        short: aterm_capacity::Shortfall {
            shorts: vec![aterm_capacity::Short::Quiet {
                pid: 4242,
                what: what.clone(),
                held: true,
            }],
            holders: vec![aterm_capacity::Holder {
                pid: 4242,
                what,
                checkout: String::new(),
                held: true,
                age: span,
                mem: 0,
                claimed: 0,
                alone: false,
                disk: 0,
                quiet: true,
                order: (0, 4242, 0),
                stalled: Some(span),
                credit: Vec::new(),
                overrun: None,
            }],
        },
    })
}

/// A stage that passes.
fn passes(_: &Ctx, spec: &StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    r.pass("ran");
    r
}

/// A TIMED STAGE WHOSE QUIET CLAIM GAVE UP BEHIND A HOLDER THAT MAKES NO PROGRESS IS NOT RUN,
/// AND THE RUN GOES ON (CW-WAIT-UNBOUNDED): each of appkit's timed stages asks for the quiet
/// machine in turn, and each wait given up is that stage's NOT RUN row, naming the holder and
/// how long it made no progress; the run is not ended as an interrupt ends it, so the next timed
/// stage asks again, and the run is COULD NOT RUN with no finding. NEGATIVE CONTROL: a stuck end
/// taken as an interrupt's ends the run at the first, and the second stage asks nothing.
#[test]
fn a_timed_stage_given_up_behind_a_stuck_holder_is_not_run_and_the_run_goes_on() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-quiet-stuck-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch dir");
    let ctx = Ctx::new(
        dir.path().to_path_buf(),
        Scope::crate_only("aterm-capacity"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_checks(&[Check::Appkit])
    .with_quiet(Some(Admit::Wait));
    let mut log = Vec::new();
    let code = crate::run_with(
        &ctx,
        &mut std::io::sink(),
        &mut log,
        &Show::default(),
        Seams {
            stage: passes,
            prepare_cells: None,
            admit_quiet: given_up,
            admit_memory: given_up,
        },
    )
    .expect("the result written");
    let log = String::from_utf8_lossy(&log);
    assert_eq!(code, crate::exit::COULD_NOT_RUN, "{log}");
    assert!(!log.contains("FAIL"), "a finding:\n{log}");
    let rows: Vec<&str> = log
        .lines()
        .filter(|l| l.trim_start().starts_with("NOT RUN"))
        .collect();
    let asked = GIVEN_UP.load(std::sync::atomic::Ordering::SeqCst);
    assert!(asked >= 2, "each timed stage asked: {asked}\n{log}");
    assert_eq!(rows.len(), asked, "{log}");
    for row in rows {
        for said in [
            "its quiet claim gave up its wait",
            "made no progress for 16m00s (none was signalled)",
            "pid 4242 holds the quiet machine",
            "it started nothing",
        ] {
            assert!(row.contains(said), "{said:?} in {row}");
        }
    }
}
