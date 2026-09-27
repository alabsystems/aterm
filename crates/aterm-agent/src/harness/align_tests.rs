// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] — the bounded child runner, including the Tier-1
//! lifecycle bind to `harness_capture_worker_lifecycle_model`. The contract,
//! probe and verdict tests went with that code on 2026-09-23 (the module doc
//! says why).

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A unique temp directory the caller removes.
#[cfg(unix)]
struct TempDir(PathBuf);

#[cfg(unix)]
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

#[cfg(unix)]
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

// THE ESCAPE HAPPENS INSIDE THE SPAWN, BEFORE THE CAPTURE'S DEADLINE STARTS
// (2026-09-25). A descendant that calls setsid() and keeps one inherited pipe is
// the case a process-group kill alone cannot close. It used to be built from the
// test binary itself: the capture's child was a second copy of it that started
// a THIRD copy, which left the group and wrote a `ready` file — two libtest
// process starts, in series, inside the capture's own 2 s budget, which runs
// from `spawn` returning. Under load the deadline won: its group kill took the
// not-yet-escaped holder with it (the stdout test then failed "escaped holder
// did not exit"), or the child's own 2 s wait for `ready` gave up and it exited
// non-zero (the stdin test's `got.success()`).
//
// Now the holder is forked by the capture child's pre-exec hook, which runs
// after `process_group(0)` has put the child in its private group and before
// it execs `/bin/sh`. The holder calls setsid() and says so over a pipe; the
// child execs only once it has; `spawn` returns only after that exec and after
// the holder has closed its copy of the spawn's status pipe; and the capture
// takes its deadline after `spawn` returns. So the escape happens-before the
// deadline on every run, whatever the load. The holder never execs, so it
// closes every descriptor above 2 — the spawn's status pipe, and every pipe a
// parallel test holds — and keeps only the capture pipe under test, until the
// test writes `done` (its own 60 s cap is the last resort). Only
// async-signal-safe calls run in either forked process.
#[cfg(unix)]
const ESCAPED_HOLDER_CAP_TICKS: u32 = 1_200; // x 50 ms

/// A capture child whose pre-exec hook forks a holder of the capture's fd
/// `held` (0: its stdin, 1: its stdout) that has LEFT the child's process group,
/// and then execs `/bin/sh -c 'exit 0'`.
#[cfg(unix)]
fn escaped_pipe_command(held: libc::c_int, done: &Path, exited: &Path) -> Command {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::process::CommandExt as _;
    let c_path =
        |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).expect("a path with no NUL");
    let (done, exited) = (c_path(done), c_path(exited));
    // Read here, not in the forked holder: only async-signal-safe calls run
    // after the fork. The descriptor ceiling is the soft RLIMIT_NOFILE (the
    // workspace's libc carries no `_SC_OPEN_MAX`); RLIM_INFINITY and anything
    // past the cap sweep 65,536.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` only fills the `rlimit` this frame owns.
    let top = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0 {
        libc::c_int::try_from(limit.rlim_cur.min(65_536)).unwrap_or(65_536)
    } else {
        65_536
    };
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "exit 0"]);
    // SAFETY: the hook runs in the forked child before exec and calls only
    // async-signal-safe functions (pipe, fork, setsid, write, open, dup2, close,
    // access, poll, read, _exit, and errno) on memory allocated before the
    // spawn; the holder it forks never returns from the hook.
    unsafe {
        cmd.pre_exec(move || {
            let interrupted =
                || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
            let mut sync: [libc::c_int; 2] = [-1, -1];
            if libc::pipe(sync.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => {
                    // THE HOLDER. Leave the capture child's group first; that
                    // is the escape under test, so the byte that lets the
                    // child exec is written only once it has happened. A
                    // holder that could not leave writes nothing, and the
                    // spawn fails instead of testing a holder the group kill
                    // can reach.
                    if libc::setsid() < 0 {
                        libc::_exit(1);
                    }
                    while libc::write(sync[1], b"e".as_ptr().cast(), 1) < 0 && interrupted() {}
                    // Keep ONLY the pipe under test: the other stdio slot goes
                    // to /dev/null, and every descriptor above 2 is closed.
                    let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                    if null >= 0 {
                        libc::dup2(null, if held == 0 { 1 } else { 0 });
                    }
                    for fd in 3..top {
                        libc::close(fd);
                    }
                    let mut ticks = 0;
                    while libc::access(done.as_ptr(), libc::F_OK) != 0
                        && ticks < ESCAPED_HOLDER_CAP_TICKS
                    {
                        libc::poll(std::ptr::null_mut(), 0, 50);
                        ticks += 1;
                    }
                    let fd = libc::open(
                        exited.as_ptr(),
                        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                        0o600,
                    );
                    if fd >= 0 {
                        libc::close(fd);
                    }
                    libc::_exit(0)
                }
                _ => {
                    // THE CAPTURE CHILD: exec only once the holder has escaped
                    // (a byte), or fail the spawn if it died first (EOF).
                    libc::close(sync[1]);
                    let mut byte = 0u8;
                    let got = loop {
                        let n = libc::read(sync[0], (&raw mut byte).cast(), 1);
                        if n >= 0 || !interrupted() {
                            break n;
                        }
                    };
                    libc::close(sync[0]);
                    if got == 1 {
                        Ok(())
                    } else {
                        Err(std::io::Error::from_raw_os_error(libc::ECHILD))
                    }
                }
            }
        });
    }
    cmd
}

/// Release an orphaned fixture even if an assertion panics. Its own 60 s cap
/// is the last resort; a passing test waits for the exit marker.
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
        // The exit marker is the event; the bound only names a hang, and is
        // the holder's own cap, which ends it whether or not it saw `done`.
        let until = std::time::Instant::now() + Duration::from_secs(60);
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
fn escaped_descendant_holding_stdout_cannot_retain_a_reader_thread() {
    let tmp = TempDir::new("escaped-stdout");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let cmd = escaped_pipe_command(1, &done, &exited);
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let result = capture_bounded_impl(
        cmd,
        Duration::from_secs(2),
        None,
        1024,
        Some(Arc::clone(&observed)),
    );
    let returned = CaptureObservedState::from(observed.as_ref());
    // Read before the release: the capture came back while the escaped holder
    // still held its stdout — at its deadline, not when the holder let go.
    let holder_held_on = !exited.exists();
    holder.release();
    let why = result.expect_err("escaped descendant still holds stdout at deadline");
    assert!(why.contains("deadline"), "{why}");
    assert!(
        holder_held_on,
        "the capture returned only once the escaped holder had exited"
    );
    assert_capture_return_conforms(returned, true, "reader_done");
}

#[cfg(unix)]
#[test]
fn escaped_descendant_holding_stdin_cannot_retain_a_writer_thread() {
    let tmp = TempDir::new("escaped-stdin");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let cmd = escaped_pipe_command(0, &done, &exited);
    let observed = Arc::new(CaptureLifecycleObservation::default());
    // The child exits at once, so a passing capture never waits on this
    // budget; it is the line a writer left running until the deadline
    // crosses, and it sits below the holder's own 60 s cap so that a writer
    // left running until the HOLDER lets go crosses it too.
    let budget = Duration::from_secs(30);
    let started = std::time::Instant::now();
    let result = capture_bounded_impl(
        cmd,
        budget,
        Some(vec![b'x'; 1 << 20]),
        1024,
        Some(Arc::clone(&observed)),
    );
    let elapsed = started.elapsed();
    let returned = CaptureObservedState::from(observed.as_ref());
    let holder_held_on = !exited.exists();
    holder.release();
    let got = result.expect("the direct child exited after its helper escaped");
    assert!(got.success());
    assert!(
        holder_held_on,
        "the capture returned only once the escaped holder had exited"
    );
    assert!(
        elapsed < budget,
        "the stdin writer ran on to the capture's deadline: {elapsed:?}"
    );
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
