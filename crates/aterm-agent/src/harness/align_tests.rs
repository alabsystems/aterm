// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] — the bounded child runner, including the Tier-1
//! lifecycle bind to `harness_capture_worker_lifecycle_model`. The contract,
//! probe and verdict tests went with that code on 2026-09-23 (the module doc
//! says why).

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A unique temp directory the caller removes.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("aterm-align-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the temp directory is created");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// -- the bounded runner every subprocess in this tree borrows -------------------

#[test]
fn a_child_that_never_returns_is_killed_at_the_deadline() {
    // THE POINT of this shape: `wait_with_output` on a child that hangs never
    // returns, and a stale NFS mount does that to `df`.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("sleep 30");
    let started = std::time::Instant::now();
    let got = capture_bounded(cmd, Duration::from_millis(250), None, 1024);
    let why = got.expect_err("a hung child is an error, not a wait");
    assert!(why.contains("deadline"), "{why}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it returned in {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn a_child_that_closes_stdout_then_hangs_cannot_survive_the_deadline() {
    let tmp = TempDir::new("closed-stdout-hang");
    let marker = tmp.path().join("survived");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("exec 1>&-; sleep 1; : > \"$MARKER\"")
        .env("MARKER", &marker);
    let started = std::time::Instant::now();
    let why = capture_bounded(cmd, Duration::from_millis(200), None, 1024)
        .expect_err("closing stdout must not bypass the child deadline");
    assert!(why.contains("deadline"), "{why}");
    assert!(started.elapsed() < Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(1_100));
    assert!(!marker.exists(), "the timed-out child was left running");
}

#[cfg(unix)]
#[test]
fn a_descendant_holding_stdout_is_killed_with_its_process_group() {
    let tmp = TempDir::new("descendant-stdout-hang");
    let marker = tmp.path().join("survived");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("(sleep 1; : > \"$MARKER\") & exit 0")
        .env("MARKER", &marker);
    let started = std::time::Instant::now();
    let why = capture_bounded(cmd, Duration::from_millis(200), None, 1024)
        .expect_err("a descendant-held stdout must still time out");
    assert!(why.contains("deadline"), "{why}");
    assert!(started.elapsed() < Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(1_100));
    assert!(!marker.exists(), "the descendant was left running");
}

// The test binary is its own fixture: the first subprocess starts a second
// copy, which calls setsid() before retaining one inherited pipe. This is the
// case a process-group kill alone cannot close. The grandchild watches `done`
// so the parent test releases it promptly, and self-exits after 5 s on panic.
#[cfg(unix)]
const ESCAPED_MODE: &str = "ATERM_ALIGN_ESCAPED_PIPE_MODE";
#[cfg(unix)]
const ESCAPED_READY: &str = "ATERM_ALIGN_ESCAPED_PIPE_READY";
#[cfg(unix)]
const ESCAPED_DONE: &str = "ATERM_ALIGN_ESCAPED_PIPE_DONE";
#[cfg(unix)]
const ESCAPED_EXITED: &str = "ATERM_ALIGN_ESCAPED_PIPE_EXITED";

#[cfg(unix)]
fn escaped_pipe_command(mode: &str, ready: &Path, done: &Path, exited: &Path) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["--ignored", "escaped_pipe_holder_helper", "--nocapture"])
        .env(ESCAPED_MODE, mode)
        .env(ESCAPED_READY, ready)
        .env(ESCAPED_DONE, done)
        .env(ESCAPED_EXITED, exited);
    cmd
}

/// Release an orphaned fixture even if an assertion panics. Its own five-
/// second cap is the last resort; a passing test waits for the exit marker.
#[cfg(unix)]
struct EscapedHolderGuard {
    done: PathBuf,
    exited: PathBuf,
    released: bool,
}

#[cfg(unix)]
impl EscapedHolderGuard {
    fn new(done: PathBuf, exited: PathBuf) -> Self {
        Self {
            done,
            exited,
            released: false,
        }
    }

    fn release(&mut self) {
        let _ = std::fs::write(&self.done, b"done");
        self.released = true;
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !self.exited.exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(self.exited.exists(), "escaped holder did not exit");
    }
}

#[cfg(unix)]
impl Drop for EscapedHolderGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = std::fs::write(&self.done, b"done");
        }
    }
}

/// Snapshot BEFORE releasing the escaped descendant: otherwise a detached
/// worker from the historical bug could finish during fixture cleanup and
/// make an early-return regression look green.
#[cfg(unix)]
struct CaptureObservedState {
    child_reaped: bool,
    reader_done: bool,
    writer_done: bool,
}

#[cfg(unix)]
impl From<&CaptureLifecycleObservation> for CaptureObservedState {
    fn from(observed: &CaptureLifecycleObservation) -> Self {
        Self {
            child_reaped: observed.child_reaped.load(Ordering::Acquire),
            reader_done: observed.reader_done.load(Ordering::Acquire),
            writer_done: observed.writer_done.load(Ordering::Acquire),
        }
    }
}

/// Tier-1: project actual child/pipe-worker completion from one invocation
/// onto the derived lifecycle model. A forged early return with the same pipe
/// worker still live is refused by the normal model and admitted by Buggy=1.
#[cfg(unix)]
fn assert_capture_return_conforms(
    observed: CaptureObservedState,
    deadline: bool,
    still_live: &'static str,
) {
    let model = aterm_spec::derive::harness_capture_worker_lifecycle_model();
    let mut state = model.init_state();
    state.insert("child_reaped", i64::from(observed.child_reaped));
    state.insert("reader_done", i64::from(observed.reader_done));
    state.insert("writer_done", i64::from(observed.writer_done));
    state.insert("deadline", i64::from(deadline));
    assert_eq!(state["child_reaped"], 1, "the direct child was reaped");
    assert_eq!(state["reader_done"], 1, "the stdout worker was joined");
    assert_eq!(state["writer_done"], 1, "the stdin worker was joined");
    assert!(
        model.action_enabled("Return", &state),
        "the real capture returned at a model-forbidden state: {state:?}"
    );
    assert!(model.fire("Return", &mut state));
    assert!(model.check_invariant("NoReturnBeforeWorkersJoin", &state));

    let mut early = state;
    early.insert("returned", 0);
    early.insert(still_live, 0);
    assert!(
        !model.action_enabled("Return", &early),
        "early return with a live {still_live} must be refused"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(
        buggy.action_enabled("Return", &early),
        "the historical detached-worker path is the negative control"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture; invoked by escaped-pipe tests"]
#[allow(
    clippy::zombie_processes,
    reason = "the fixture parent exits while its escaped descendant holds the capture pipe"
)]
fn escaped_pipe_holder_helper() {
    let Ok(mode) = std::env::var(ESCAPED_MODE) else {
        return;
    };
    let ready = PathBuf::from(std::env::var(ESCAPED_READY).expect("ready path"));
    let done = PathBuf::from(std::env::var(ESCAPED_DONE).expect("done path"));
    let exited = PathBuf::from(std::env::var(ESCAPED_EXITED).expect("exited path"));
    match mode.as_str() {
        "parent-stdout" | "parent-stdin" => {
            let stdout = mode == "parent-stdout";
            let mut descendant = escaped_pipe_command(
                if stdout {
                    "child-stdout"
                } else {
                    "child-stdin"
                },
                &ready,
                &done,
                &exited,
            );
            descendant
                .stdin(if stdout {
                    Stdio::null()
                } else {
                    Stdio::inherit()
                })
                .stdout(if stdout {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .stderr(Stdio::null())
                .spawn()
                .expect("start escaped pipe holder");
            let until = std::time::Instant::now() + Duration::from_secs(2);
            while !ready.exists() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ready.exists(), "escaped holder never started");
        }
        "child-stdout" | "child-stdin" => {
            // SAFETY: this fixture is a freshly spawned process, not a
            // process-group leader. Leaving the capture child's private group
            // is exactly the pipe-retention case under test.
            assert!(unsafe { libc::setsid() } > 0, "leave process group");
            std::fs::write(&ready, std::process::id().to_string())
                .expect("announce escaped holder PID");
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while !done.exists() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            std::fs::write(&exited, b"exited").expect("confirm escaped holder exit");
            std::process::exit(0);
        }
        other => panic!("unknown escaped fixture mode: {other}"),
    }
}

#[cfg(unix)]
#[test]
fn escaped_descendant_holding_stdout_cannot_retain_a_reader_thread() {
    let tmp = TempDir::new("escaped-stdout");
    let ready = tmp.path().join("ready");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let cmd = escaped_pipe_command("parent-stdout", &ready, &done, &exited);
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let started = std::time::Instant::now();
    let result = capture_bounded_impl(
        cmd,
        Duration::from_secs(2),
        None,
        1024,
        Some(Arc::clone(&observed)),
    );
    let elapsed = started.elapsed();
    let returned = CaptureObservedState::from(observed.as_ref());
    holder.release();
    let why = result.expect_err("escaped descendant still holds stdout at deadline");
    assert!(why.contains("deadline"), "{why}");
    assert!(ready.exists(), "the escaped holder was not exercised");
    assert!(elapsed < Duration::from_secs(3));
    assert_capture_return_conforms(returned, true, "reader_done");
}

#[cfg(unix)]
#[test]
fn escaped_descendant_holding_stdin_cannot_retain_a_writer_thread() {
    let tmp = TempDir::new("escaped-stdin");
    let ready = tmp.path().join("ready");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let cmd = escaped_pipe_command("parent-stdin", &ready, &done, &exited);
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let started = std::time::Instant::now();
    let result = capture_bounded_impl(
        cmd,
        Duration::from_secs(2),
        Some(vec![b'x'; 1 << 20]),
        1024,
        Some(Arc::clone(&observed)),
    );
    let elapsed = started.elapsed();
    let returned = CaptureObservedState::from(observed.as_ref());
    holder.release();
    let got = result.expect("the direct child exited after its helper escaped");
    assert!(got.success());
    assert!(ready.exists(), "the escaped holder was not exercised");
    assert!(elapsed < Duration::from_secs(2));
    assert_capture_return_conforms(returned, false, "writer_done");
}

#[test]
fn stdin_reaches_the_child_and_stdout_is_capped() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("cat");
    let got = capture_bounded(cmd, Duration::from_secs(5), Some(b"hello".to_vec()), 1024)
        .expect("it runs");
    assert_eq!(got.stdout, b"hello");
    assert!(got.success());

    // The cap is a CUT, not an error: a child that prints more than the
    // caller asked for loses the tail rather than the whole answer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'aaaaaaaaaaaaaaaaaaaa'");
    let got = capture_bounded(cmd, Duration::from_secs(5), None, 4).expect("it runs");
    assert_eq!(got.stdout, b"aaaa");

    // A child that never reads its stdin cannot block the writer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'ignored\\n'");
    let got = capture_bounded(
        cmd,
        Duration::from_secs(5),
        Some(vec![b'x'; 256 * 1024]),
        1024,
    )
    .expect("it runs");
    assert_eq!(got.stdout, b"ignored\n");

    // The exit code is REPORTED, never swallowed: the caller decides what a
    // non-zero means.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("exit 7");
    let got = capture_bounded(cmd, Duration::from_secs(5), None, 16).expect("it runs");
    assert_eq!(got.code, Some(7));
    assert!(!got.success());
}
