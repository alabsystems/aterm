// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB DESK'S `prev_end=` (session log Phase 6, step 6C-4; ruling U9),
//! across real aterm processes sharing one scratch world. A lab desk is a
//! headless instance under the lab's desk seam (`ATERM_DEBUG_HEADLESS_RESTORE`
//! under a private `ATERM_STATE_HOME`: `Desk::LabHeadless`): it arms its crash
//! marker as `lab`, keeps a dead `lab` marker as news for its own census, and
//! runs the recovery census on its private root's log directory, which amends
//! its node log's start with how the run before it ended.
//!
//! 1. A, a lab desk on an empty ledger: `log n` shows `amend n start
//!    prev_end=none` (the positive control: the census ran). A quits as a
//!    person does (`invoke Quit`).
//! 2. B, a lab desk on the same roots: `amend n start prev_end=clean`. B is
//!    then SIGKILLed by its pid: no exit path runs, and its empty `lab` marker
//!    stays behind with its lock free.
//! 3. C, a lab desk on the same roots: `amend n start prev_end=killed`, and
//!    its own kill scan consumed B's marker (renamed `.seen`), so no lab
//!    sweep after it reads B's death as news again.
//!
//! A run is quit or killed only once its own row is in the recovery ledger
//! (`recovery-ledger.log` under the private root's log directory): the
//! census amends the start before it appends the row, on a background
//! thread, so the amend alone does not say the next run can read this one.
//!
//! NEGATIVE CONTROL: after C is SIGKILLed in turn, D starts on the same roots
//! WITHOUT the seam. A plain headless instance runs no census (its log
//! directory may be the daily driver's): its `log n` holds its start alone,
//! `prev_end=-`, and once it has quit, the ledger holds no row of D's pid,
//! while A's, B's and C's are there.
//!
//! Isolation is `support/launch_isolation.rs`'s (scratch HOME and XDG roots, a
//! private control socket, every automatic lane off, `--no-reroute`), with
//! `aterm.log` written at `info` as a lab instance writes it. Every
//! wait for something that must happen is a one-minute hang detector; each
//! instance is killed by its own pid.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use aterm_update::recovery_ledger::{ledger_path, read_rows};

use crate::live_ctl::Ctl;
use crate::{headless_boot, launch_isolation};

/// A wait for something that must happen is a hang detector (AGENTS.md).
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The development seam that gives a headless instance a window's desk
/// (`aterm-gui`'s `desk::HEADLESS_RESTORE_SEAM`).
const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// One scratch world the instances share; removed on drop, after them.
struct World(aterm_tempfile::TempDir);

impl World {
    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(self.path());
    }
}

/// One headless instance, SIGKILLed by its pid and reaped on drop.
struct Proc {
    child: Child,
    _lifeline: aterm_uds::lifeline::Lifeline,
    sock: String,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The state root the seam's private-root check reads (`ATERM_STATE_HOME`).
fn state_home(root: &Path) -> PathBuf {
    root.join("astate")
}

/// Boot one headless instance named `tag` in `root`, under the lab's desk
/// seam when `lab`. A log or spawn refusal is COULD NOT RUN; an instance that
/// exits or never listens panics (`headless_boot::await_ready`).
fn boot(root: &Path, tag: &str, lab: bool) -> Proc {
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot open the instance log ({e})")),
    };
    #[expect(
        clippy::disallowed_types,
        reason = "the launch helpers every headless live test shares (aterm_uds::lifeline, the ready FIFO) arm a std Command, whose child inherits their descriptors"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .env("ATERM_STATE_HOME", state_home(root))
        // The census reads each run's own `aterm.log` lines (a clean quit's
        // final-exit line; a run with no line of its own ended `unknown`), as
        // the lab's instances write them: the isolation's `off` would hide them.
        .env("ATERM_LOG", "info")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if lab {
        cmd.env(DESK_SEAM, "1");
    }
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = headless_boot::arm_ready(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut proc = Proc {
        child,
        _lifeline: lifeline,
        sock: sock.to_string_lossy().into_owned(),
    };
    headless_boot::await_ready(&mut proc, |p| &mut p.child, &mut ready, &log_path, |_| true);
    proc
}

/// The node log's lines (`log n`).
fn node_log(proc: &Proc) -> Vec<String> {
    Ctl::connect(&proc.sock).counted("log n").1
}

/// The census's amend of `proc`'s node-log start, under the hang detector.
fn amended_start(proc: &Proc) -> String {
    aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        node_log(proc)
            .into_iter()
            .find(|l| l.contains(" amend n start "))
    })
    .unwrap_or_else(|| panic!("the census amends the start: {:?}", node_log(proc)))
}

/// Quit `proc` as a person does (`invoke Quit`, which a headless instance
/// takes without a confirm) and wait for it to end. The reply is not read:
/// the instance may close the connection as it quits.
fn quit(proc: &mut Proc) {
    let _ = Ctl::connect(&proc.sock).line("invoke Quit");
    let ended = aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        proc.child.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "the instance quits");
}

/// SIGKILL `proc` by its pid and reap it: no exit path runs.
fn kill(proc: Proc) {
    drop(proc);
}

/// The private root's log directory: the lab desk's census, ledger and
/// crash markers.
fn logs(root: &Path) -> PathBuf {
    state_home(root).join("logs")
}

/// The pids the recovery ledger holds a row of, oldest first.
fn ledger_pids(root: &Path) -> Vec<u32> {
    read_rows(&ledger_path(&logs(root)))
        .iter()
        .map(|row| row.pid)
        .collect()
}

/// Wait, under the hang detector, for `pid`'s own row in the ledger: its
/// census has appended, so the next run reads how this one ends.
fn await_row_of(root: &Path, pid: u32) {
    let row = aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        ledger_pids(root).contains(&pid).then_some(())
    });
    assert!(
        row.is_some(),
        "the census appends pid {pid}'s row: {:?}",
        ledger_pids(root)
    );
}

/// The names of `pid`'s crash markers in the log directory.
fn markers_of(root: &Path, pid: u32) -> Vec<String> {
    let prefix = format!("crash-marker-{pid}-");
    std::fs::read_dir(logs(root))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| name.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default()
}

/// The module doc, end to end.
#[test]
fn a_lab_desk_reads_how_its_previous_run_ended() {
    let world = World(launch_isolation::scratch_root("atpe"));
    let root = world.path();
    assert!(
        root.join("run/aterm/a.sock.token").as_os_str().len() < headless_boot::MAX_SOCK_PATH,
        "the private root's socket fits sun_path"
    );

    let mut a = boot(root, "a", true);
    let a_pid = a.child.id();
    let first = amended_start(&a);
    assert!(
        first.contains(" amend n start prev_end=none "),
        "an empty ledger names no run before: {first}"
    );
    await_row_of(root, a_pid);
    quit(&mut a);
    drop(a);

    let b = boot(root, "b", true);
    let b_pid = b.child.id();
    let after_quit = amended_start(&b);
    assert!(
        after_quit.contains(" amend n start prev_end=clean "),
        "the run before quit: {after_quit}"
    );
    await_row_of(root, b_pid);
    kill(b);
    let left = markers_of(root, b_pid);
    assert!(
        left.len() == 1 && left[0].ends_with("-lab.log"),
        "the SIGKILLed lab desk left its own marker: {left:?}"
    );

    let c = boot(root, "c", true);
    let c_pid = c.child.id();
    let after_kill = amended_start(&c);
    assert!(
        after_kill.contains(" amend n start prev_end=killed "),
        "the run before was SIGKILLed: {after_kill}"
    );
    // C's kill scan ran before its census thread was spawned: B's marker is
    // consumed, not left as news for every lab sweep after it.
    let consumed = markers_of(root, b_pid);
    assert!(
        consumed.len() == 1 && consumed[0].ends_with("-lab.log.seen"),
        "the lab desk's kill scan consumed B's marker: {consumed:?}"
    );
    await_row_of(root, c_pid);
    kill(c);

    // NEGATIVE CONTROL: without the seam, no census.
    let mut d = boot(root, "d", false);
    let d_pid = d.child.id();
    let lines = node_log(&d);
    assert!(
        lines
            .first()
            .is_some_and(|l| l.starts_with("1 open n start prev_end=- ")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains(" amend n start ")),
        "a plain headless instance amends nothing: {lines:?}"
    );
    quit(&mut d);
    drop(d);
    assert_eq!(
        ledger_pids(root),
        [a_pid, b_pid, c_pid],
        "the lab desks' rows, and none of the plain headless instance's (pid {d_pid})"
    );
}
