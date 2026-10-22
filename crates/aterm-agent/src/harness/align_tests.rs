// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`], the bounded child runner, including the Tier-1
//! lifecycle bind to `harness_capture_worker_lifecycle_model`.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// The deadline of a capture whose child must finish: a one-minute hang
/// detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A unique temp directory, removed on drop.
#[cfg(unix)]
fn temp_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-align-{tag}-"))
        .tempdir()
        .expect("the temp directory is created")
}

/// A child running `line` in this host's shell: `/bin/sh -c`, or on Windows
/// `cmd.exe /D /C`, which is handed the line verbatim and parses it itself.
fn shell(line: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg("-c").arg(line);
        cmd
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let mut cmd = aterm_uds::spawnfd::command("cmd.exe");
        cmd.args(["/D", "/C"]).raw_arg(line);
        cmd
    }
}

#[test]
fn a_child_that_never_returns_is_killed_at_the_deadline() {
    // The point of this shape: `wait_with_output` on a child that hangs never
    // returns, and a stale NFS mount does that to `df`. The child hangs for
    // two minutes and the capture must return inside a minute's hang detector
    // — the spawn is inside that window, so a loaded machine's slow spawn never
    // reads as a capture that waited the child out. On Windows the shell itself
    // loops (no `sleep` there), so the kill reaches the one process that hangs.
    let cmd = if cfg!(windows) {
        shell("for /L %i in (0,0,1) do @rem")
    } else {
        shell("sleep 120")
    };
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

// How long a stdout-hang fixture sleeps. The tests read survival off the
// kernel (the group must empty), not a clock.
#[cfg(unix)]
const HANG_S: u64 = 300;

/// Capture `script`, which writes `"$$ <its pgid>"` to `$PGIDF` then sleeps
/// `$HANG_S`, taking the deadline once the report is written, and check it gave
/// up on `path`. Returns the group and how long the capture took; panics when
/// the child does not lead its own process group, since `kill(-pgid, 0)`
/// answers ESRCH alike for a group that never existed and one that emptied.
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

// The escape test: a descendant that calls setsid() and keeps the inherited
// stdout is the case a process-group kill cannot reach. The capture child is
// this test binary, staged as `escaped_holder_stage`: it forks a holder that
// calls setsid(), writes the `escaped` marker and says so over a pipe, and the
// child exits once it has. The capture's `after_spawn` step waits for the
// marker, so the escape happens before the deadline under any load. The holder
// never execs, so close-on-exec closes nothing: it closes every descriptor
// above 2 itself and keeps only the capture's stdout until the test writes
// `done`, with a two-minute cap of its own. Only async-signal-safe calls run
// in it.
#[cfg(unix)]
const ESCAPED_HOLDER_CAP_TICKS: u32 = 2_400; // x 50 ms

/// Set in the environment when this test binary is the escape test's capture
/// child.
#[cfg(unix)]
const ESCAPE_STAGE: &str = "ATERM_ALIGN_ESCAPE_STAGE";

/// The escape test's capture: this test binary, staged as
/// [`escaped_holder_stage`].
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

/// Set the capture's `after_spawn` step: wait, up to [`HANG_DETECTOR`], until
/// `ready()` says the child has done what the test needs before the deadline.
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

/// The escape test's capture child when [`ESCAPE_STAGE`] is set (a no-op
/// otherwise): fork the holder, wait until it has left this process's group,
/// exit.
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
    // Read before the fork: only async-signal-safe calls run after it.
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
                // The holder: leave the group first, so the marker and the byte
                // mean the escape happened. A holder that cannot leave writes
                // neither, and the test fails on the missing marker.
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
                // Keep only the pipe under test: stdin to /dev/null, every
                // descriptor above 2 closed.
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
                // The capture child: exit 0 once the holder has escaped (a
                // byte), 1 if it died first (EOF).
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
        // Wait for the exit marker, bounded by the holder's own cap, which ends
        // it whether or not it saw `done`.
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
    // Read before the release: the capture returned at its deadline, while the
    // holder still held its stdout.
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
    // `printf`, and on Windows `set /p`'s prompt: both print with no newline.
    let print = |text: &str| {
        if cfg!(windows) {
            shell(&format!("<nul set /p ={text}& exit 0"))
        } else {
            shell(&format!("printf '{text}'"))
        }
    };
    let got = capture_bounded(print("hello"), HANG_DETECTOR, 1024).expect("it runs");
    assert_eq!(got.stdout, b"hello");
    assert!(got.success());

    // The cap truncates; it is not an error.
    let got = capture_bounded(print("aaaaaaaaaaaaaaaaaaaa"), HANG_DETECTOR, 4).expect("it runs");
    assert_eq!(got.stdout, b"aaaa");

    // The exit code is reported; the caller decides what non-zero means.
    let got = capture_bounded(shell("exit 7"), HANG_DETECTOR, 16).expect("it runs");
    assert_eq!(got.code, Some(7));
    assert!(!got.success());
}
