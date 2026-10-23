// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies across real `aterm --headless` processes that a `Desk::LabHeadless` instance records
//! and amends `prev_end=none|clean|killed` in the node log while a plain headless instance does not.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use aterm_update::recovery_ledger::{ledger_path, read_rows};

use crate::live_ctl::Ctl;
use crate::{headless_boot, launch_isolation};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

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

fn state_home(root: &Path) -> PathBuf {
    root.join("astate")
}

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

fn node_log(proc: &Proc) -> Vec<String> {
    Ctl::connect(&proc.sock).counted("log n").1
}

fn amended_start(proc: &Proc) -> String {
    aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        node_log(proc)
            .into_iter()
            .find(|l| l.contains(" amend n start "))
    })
    .unwrap_or_else(|| panic!("the census amends the start: {:?}", node_log(proc)))
}

fn quit(proc: &mut Proc) {
    let _ = Ctl::connect(&proc.sock).line("invoke Quit");
    let ended = aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        proc.child.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "the instance quits");
}

fn kill(proc: Proc) {
    drop(proc);
}

fn logs(root: &Path) -> PathBuf {
    state_home(root).join("logs")
}

fn ledger_pids(root: &Path) -> Vec<u32> {
    read_rows(&ledger_path(&logs(root)))
        .iter()
        .map(|row| row.pid)
        .collect()
}

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

#[test]
fn a_lab_desk_reads_how_its_previous_run_ended() {
    let world = World(launch_isolation::scratch_root("atpe"));
    let root = world.path();
    assert!(
        launch_isolation::sock_fits(&root.join("run/aterm/a.sock.token")),
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
    let consumed = markers_of(root, b_pid);
    assert!(
        consumed.len() == 1 && consumed[0].ends_with("-lab.log.seen"),
        "the lab desk's kill scan consumed B's marker: {consumed:?}"
    );
    await_row_of(root, c_pid);
    kill(c);

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
