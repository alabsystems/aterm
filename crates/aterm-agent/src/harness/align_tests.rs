// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`], the bounded child runner, including the Tier-1
//! lifecycle bind to `harness_capture_worker_lifecycle_model`.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// The deadline of a capture whose child MUST finish — it is read as a hang
/// past this (AGENTS.md: a hang detector is a minute, never a latency budget).
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A unique temp directory, removed on drop.
#[cfg(unix)]
fn temp_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-align-{tag}-"))
        .tempdir()
        .expect("the temp directory is created")
}

// -- the bounded runner every subprocess in this tree borrows -------------------

#[test]
fn a_child_that_never_returns_is_killed_at_the_deadline() {
    // The point of this shape: `wait_with_output` on a child that hangs never
    // returns, and a stale NFS mount does that to `df`. The child hangs for
    // two minutes and the capture must return inside a minute's hang detector
    // — the spawn is inside that window, so a loaded machine's slow spawn never
    // reads as a capture that waited the child out.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("sleep 120");
    let started = std::time::Instant::now();
    let got = capture_bounded(cmd, Duration::from_millis(250), 1024);
    let why = got.expect_err("a hung child is an error, not a wait");
    assert!(why.contains("deadline"), "{why}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "it returned in {:?}",
        started.elapsed()
    );
}

// The two stdout-hang tests below read survival off the kernel, not a clock.
//
// The fixture hangs for `HANG_S`, and the process that must not survive
// writes `$$` (the capture's child) and the process group the kernel says it
// is in (`ps -o pgid=`). The two must be equal before the group is read,
// since `kill(-pgid, 0)` answers ESRCH for a group that never existed as for
// one that emptied. Once the capture gives up, the group must have no member
// left, with a minute's hang detector for reaping an orphan.
//
// The capture takes its deadline only once the fixture has written its report
// (the `after_spawn` step, [`settle_until`]), so one take is the verdict
// whatever the load.
#[cfg(unix)]
const HANG_S: u64 = 300;

/// Capture `script` (which writes `"<$$> <its pgid>"` to `$PGIDF`, then hangs
/// for `$HANG_S`), its deadline taken once the report is written, and check
/// it gave up on `path`: the group, and how long the capture took. Panics
/// when the capture's child does not lead its own process group.
#[cfg(unix)]
fn capture_a_hanging_group(tag: &str, script: &str, path: &str) -> (libc::pid_t, Duration) {
    let tmp = temp_dir(tag);
    let pgid_file = tmp.path().join("pgid");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .env("PGIDF", &pgid_file)
        .env("HANG_S", HANG_S.to_string());
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let report = pgid_file.clone();
    settle_until(&observed, move || {
        std::fs::read_to_string(&report).is_ok_and(|r| r.ends_with('\n'))
    });
    let started = std::time::Instant::now();
    let why = capture_bounded_impl(cmd, Duration::from_millis(200), 1024, Some(observed))
        .expect_err("a hung child is an error, not a wait");
    let took = started.elapsed();
    let report = std::fs::read_to_string(&pgid_file).unwrap_or_default();
    let mut words = report
        .split_whitespace()
        .map(|word| word.parse::<libc::pid_t>().ok().filter(|&id| id > 1));
    let (Some(Some(pid)), Some(Some(pgid))) = (words.next(), words.next()) else {
        panic!("the fixture wrote no report ({report:?}); the capture said: {why}");
    };
    assert_eq!(
        pgid, pid,
        "the capture's child does not lead its own process group (it is in \
         {pgid}), so no group kill can reach what it leaves running"
    );
    assert!(
        why.contains(path),
        "the capture gave up on another path: {why}"
    );
    (pgid, took)
}

/// Whether process group `pgid` has emptied — every member killed and reaped —
/// within a minute. A survivor is killed (SIGKILL) before this answers `false`.
#[cfg(unix)]
fn group_emptied(pgid: libc::pid_t) -> bool {
    let started = std::time::Instant::now();
    loop {
        // SAFETY: signal 0 delivers nothing: `kill` only reports whether the
        // group still has a member this user may signal.
        if unsafe { libc::kill(-pgid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if started.elapsed() >= Duration::from_secs(60) {
            // SAFETY: as above; SIGKILL to a group this test's fixture made.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn a_child_that_closes_stdout_then_hangs_cannot_survive_the_deadline() {
    // The report is written after the close, so the post-EOF wait starts as soon
    // as the shell does.
    let (pgid, took) = capture_a_hanging_group(
        "closed-stdout-hang",
        "exec 1>&-; echo \"$$ $(ps -o pgid= -p $$)\" > \"$PGIDF\"; sleep \"$HANG_S\"",
        "closed its output",
    );
    assert!(
        took < Duration::from_secs(HANG_S / 2),
        "closing stdout bypassed the deadline: the capture took {took:?}"
    );
    assert!(group_emptied(pgid), "the timed-out child was left running");
}

#[cfg(unix)]
#[test]
fn a_descendant_holding_stdout_is_killed_with_its_process_group() {
    // The child reads its group before it forks; the descendant, which inherits
    // it, writes the report, so a report means the descendant was running.
    let (pgid, took) = capture_a_hanging_group(
        "descendant-stdout-hang",
        "g=$(ps -o pgid= -p $$); (echo \"$$ $g\" > \"$PGIDF\"; sleep \"$HANG_S\") & exit 0",
        "ran past its",
    );
    assert!(
        took < Duration::from_secs(HANG_S / 2),
        "a descendant-held stdout did not time out: the capture took {took:?}"
    );
    assert!(group_emptied(pgid), "the descendant was left running");
}

// THE ESCAPE HAPPENS BEFORE THE CAPTURE'S DEADLINE STARTS (2026-09-25). A
// descendant that calls setsid() and keeps one inherited pipe is the case a
// process-group kill alone cannot close. It was first built from the test
// binary itself — a second copy that started a THIRD, which left the group and
// wrote a `ready` file — two libtest process starts, in series, inside the
// capture's own 2 s budget, which runs from `spawn` returning. Under load the
// deadline won: its group kill took the not-yet-escaped holder with it (the
// test then failed "escaped holder did not exit"), or the child's own 2 s
// wait for `ready` gave up and it exited non-zero. Then the holder was forked
// in the capture child's pre-exec hook, so the escape happened inside `spawn`.
//
// Now (2026-10-01) the capture's `Command` runs no hook: on Darwin it is one
// `posix_spawn` (`aterm_uds::spawnfd`). The capture child is ONE copy of this
// test binary, staged as [`escaped_holder_stage`]: it forks the holder, which
// calls setsid(), writes the `escaped` marker and says so over a pipe; the
// child exits once it has. What keeps the escape ahead of the deadline is the
// capture's test-only `after_spawn` step ([`settle_until`]), which waits for
// the marker before the capture takes its deadline — so the escape
// happens-before the deadline on every run, whatever the load. The holder
// never execs, so it closes every descriptor above 2 and keeps only the
// capture's stdout, until the test writes `done` (its own two-minute cap is
// the last resort). Only async-signal-safe calls run in the forked holder.
#[cfg(unix)]
const ESCAPED_HOLDER_CAP_TICKS: u32 = 2_400; // x 50 ms

/// Present in the environment: this test binary IS the escape test's capture
/// child.
#[cfg(unix)]
const ESCAPE_STAGE: &str = "ATERM_ALIGN_ESCAPE_STAGE";

/// A capture child — this test binary, staged as [`escaped_holder_stage`] —
/// that forks a holder of the capture's stdout, which LEAVES the child's
/// process group and writes `escaped`; the child then exits 0.
#[cfg(unix)]
fn escaped_pipe_command(done: &Path, exited: &Path, escaped: &Path) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "--exact",
        "harness::align::tests::escaped_holder_stage",
        "--nocapture",
        "--test-threads=1",
        "--quiet",
    ])
    .env(ESCAPE_STAGE, "1")
    .env("ATERM_ALIGN_ESCAPE_DONE", done)
    .env("ATERM_ALIGN_ESCAPE_EXITED", exited)
    .env("ATERM_ALIGN_ESCAPE_ESCAPED", escaped);
    cmd
}

/// The capture's `after_spawn` step: wait until the child has done what a
/// test needs done before the capture takes its deadline (`ready`: the
/// escaped holder's marker, a fixture's report) — a hang detector of a
/// minute, not a budget.
#[cfg(unix)]
fn settle_until(observed: &CaptureLifecycleObservation, ready: impl Fn() -> bool + Send + 'static) {
    *observed
        .after_spawn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
        let until = std::time::Instant::now() + HANG_DETECTOR;
        while !ready() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }));
}

/// THE CAPTURE CHILD of the escape test, when [`ESCAPE_STAGE`] is set (and
/// nothing at all otherwise): fork the holder, wait until it has left this
/// process's group, exit.
#[cfg(unix)]
#[test]
fn escaped_holder_stage() {
    use std::os::unix::ffi::OsStrExt as _;
    if std::env::var_os(ESCAPE_STAGE).is_none() {
        return;
    }
    let path = |var: &str| {
        let path = std::env::var_os(var).expect("the staged paths");
        std::ffi::CString::new(path.as_bytes()).expect("a path with no NUL")
    };
    let (done, exited, escaped) = (
        path("ATERM_ALIGN_ESCAPE_DONE"),
        path("ATERM_ALIGN_ESCAPE_EXITED"),
        path("ATERM_ALIGN_ESCAPE_ESCAPED"),
    );
    // Read here, not in the forked holder: only async-signal-safe calls run
    // after the fork. The kernel's per-process descriptor maximum, which no
    // thread can raise before the fork.
    let top = aterm_uds::spawnfd::descriptor_bound();
    let interrupted = || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
    let mut sync: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `pipe` fills the two-element array; `fork` takes nothing. The
    // holder branch makes only async-signal-safe calls (setsid, open, close,
    // write, dup2, access, poll, _exit) on memory prepared before the fork, and
    // never returns; this branch reads one byte and leaves through `_exit`.
    unsafe {
        assert_eq!(libc::pipe(sync.as_mut_ptr()), 0, "pipe");
        match libc::fork() {
            -1 => panic!("fork: {}", std::io::Error::last_os_error()),
            0 => {
                // THE HOLDER. Leave the capture child's group first; that is
                // the escape under test, so the marker and the byte are
                // written only once it has happened. A holder that could not
                // leave writes neither, and the test fails on the missing
                // marker instead of testing a holder the group kill can reach.
                if libc::setsid() < 0 {
                    libc::_exit(1);
                }
                let marker = libc::open(
                    escaped.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o600,
                );
                if marker >= 0 {
                    libc::close(marker);
                }
                while libc::write(sync[1], b"e".as_ptr().cast(), 1) < 0 && interrupted() {}
                // Keep ONLY the pipe under test: stdin goes to /dev/null, and
                // every descriptor above 2 is closed.
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null >= 0 {
                    libc::dup2(null, 0);
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
                // THE CAPTURE CHILD: exit once the holder has escaped (a byte),
                // or fail if it died first (EOF).
                libc::close(sync[1]);
                let mut byte = 0u8;
                let got = loop {
                    let n = libc::read(sync[0], (&raw mut byte).cast(), 1);
                    if n >= 0 || !interrupted() {
                        break n;
                    }
                };
                libc::_exit(if got == 1 { 0 } else { 1 })
            }
        }
    }
}

/// Release an orphaned fixture even if an assertion panics. Its own two-minute
/// cap is the last resort; a passing test waits for the exit marker.
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
        let cap = Duration::from_millis(50 * u64::from(ESCAPED_HOLDER_CAP_TICKS));
        let until = std::time::Instant::now() + cap;
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

/// Snapshot before releasing the escaped descendant: otherwise a detached
/// worker could finish during fixture cleanup and make an early return look
/// green.
#[cfg(unix)]
struct CaptureObservedState {
    child_reaped: bool,
    reader_done: bool,
}

#[cfg(unix)]
impl From<&CaptureLifecycleObservation> for CaptureObservedState {
    fn from(observed: &CaptureLifecycleObservation) -> Self {
        Self {
            child_reaped: observed.child_reaped.load(Ordering::Acquire),
            reader_done: observed.reader_done.load(Ordering::Acquire),
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
    let model = aterm_models_harness::harness_capture_worker_lifecycle_model();
    let mut state = model.init_state();
    state.insert("child_reaped", i64::from(observed.child_reaped));
    state.insert("reader_done", i64::from(observed.reader_done));
    state.insert("deadline", i64::from(deadline));
    assert_eq!(state["child_reaped"], 1, "the direct child was reaped");
    assert_eq!(state["reader_done"], 1, "the stdout worker was joined");
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
    let tmp = temp_dir("escaped-stdout");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let escaped = tmp.path().join("escaped");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let cmd = escaped_pipe_command(&done, &exited, &escaped);
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let marker = escaped.clone();
    settle_until(&observed, move || marker.exists());
    let result = capture_bounded_impl(
        cmd,
        Duration::from_secs(2),
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

#[test]
fn stdout_is_capped_and_the_exit_code_reported() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'hello'");
    let got = capture_bounded(cmd, HANG_DETECTOR, 1024).expect("it runs");
    assert_eq!(got.stdout, b"hello");
    assert!(got.success());

    // The cap is a CUT, not an error: a child that prints more than the
    // caller asked for loses the tail rather than the whole answer.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'aaaaaaaaaaaaaaaaaaaa'");
    let got = capture_bounded(cmd, HANG_DETECTOR, 4).expect("it runs");
    assert_eq!(got.stdout, b"aaaa");

    // The exit code is REPORTED, never swallowed: the caller decides what a
    // non-zero means.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("exit 7");
    let got = capture_bounded(cmd, HANG_DETECTOR, 16).expect("it runs");
    assert_eq!(got.code, Some(7));
    assert!(!got.success());
}
