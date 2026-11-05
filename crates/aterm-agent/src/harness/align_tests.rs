// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] and its conformance bind to `harness_capture_worker_lifecycle_model`.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// The deadline of a capture whose child must finish.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A unique temp directory, removed on drop.
#[cfg(unix)]
fn temp_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-align-{tag}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("the temp directory is created")
}

/// A child running `line` in this host's shell.
fn shell(line: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = aterm_uds::spawnfd::command(crate::test_program::kept("/bin/sh"));
        cmd.arg("-c")
            .arg(line)
            .env("ATERM_TEST_SLEEP", crate::test_program::kept("/bin/sleep"));
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
    let cmd = if cfg!(windows) {
        shell("for /L %i in (0,0,1) do @rem")
    } else {
        shell("\"$ATERM_TEST_SLEEP\" 120")
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

#[cfg(unix)]
#[test]
fn a_slow_spawn_spends_the_budget() {
    let slow_spawn = |held: Duration| {
        let observed = Arc::new(CaptureLifecycleObservation::default());
        *observed
            .before_spawn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Box::new(move || std::thread::sleep(held)));
        Some(observed)
    };
    let budget = Duration::from_millis(50);
    capture_bounded_impl(
        shell("printf hello"),
        budget,
        1024,
        None,
        slow_spawn(budget * 2),
    )
    .expect_err("a spawn held past the budget leaves the child no time");

    let got = capture_bounded_impl(
        shell("printf hello"),
        HANG_DETECTOR,
        1024,
        None,
        slow_spawn(budget * 2),
    )
    .expect("the child runs within a budget that covers its spawn");
    let Ended::Exited(got) = got else {
        panic!("nothing was watched for, so nothing stops it: {got:?}");
    };
    assert_eq!(got.stdout, b"hello");
}

#[cfg(unix)]
const HANG_S: u64 = 300;

/// Capture `script`.
#[cfg(unix)]
fn capture_a_hanging_group(tag: &str, script: &str, path: &str) -> (libc::pid_t, Duration) {
    let tmp = temp_dir(tag);
    let pgid_file = tmp.path().join("pgid");
    let mut cmd = Command::new(crate::test_program::kept("/bin/sh"));
    cmd.arg("-c")
        .arg(script)
        .env("PGIDF", &pgid_file)
        .env(
            "ATERM_TEST_PS",
            crate::test_program::kept(aterm_uds::os_path!("ps")),
        )
        .env("ATERM_TEST_SLEEP", crate::test_program::kept("/bin/sleep"))
        .env("HANG_S", HANG_S.to_string());
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let report = pgid_file.clone();
    settle_until(&observed, move || {
        std::fs::read_to_string(&report).is_ok_and(|r| r.ends_with('\n'))
    });
    let started = std::time::Instant::now();
    let why = capture_bounded_impl(cmd, Duration::from_millis(200), 1024, None, Some(observed))
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
/// within a minute.
#[cfg(unix)]
fn group_emptied(pgid: libc::pid_t) -> bool {
    let started = std::time::Instant::now();
    loop {
        // SAFETY: signal 0 only checks whether the process group exists.
        if unsafe { libc::kill(-pgid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if started.elapsed() >= Duration::from_secs(60) {
            // Again until none is left: a member's fork can dodge one signal (XNU).
            let until = std::time::Instant::now() + Duration::from_secs(60);
            // SAFETY: as above; SIGKILL to a group this test's fixture made.
            while unsafe { libc::kill(-pgid, libc::SIGKILL) } == 0
                && std::time::Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn a_child_that_closes_stdout_then_hangs_cannot_survive_the_deadline() {
    let (pgid, took) = capture_a_hanging_group(
        "closed-stdout-hang",
        "exec 1>&-; echo \"$$ $(\"$ATERM_TEST_PS\" -o pgid= -p $$)\" > \"$PGIDF\"; \"$ATERM_TEST_SLEEP\" \"$HANG_S\"",
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
    let (pgid, took) = capture_a_hanging_group(
        "descendant-stdout-hang",
        "g=$(\"$ATERM_TEST_PS\" -o pgid= -p $$); (echo \"$$ $g\" > \"$PGIDF\"; \"$ATERM_TEST_SLEEP\" \"$HANG_S\") & exit 0",
        "ran past its",
    );
    assert!(
        took < Duration::from_secs(HANG_S / 2),
        "a descendant-held stdout did not time out: the capture took {took:?}"
    );
    assert!(group_emptied(pgid), "the descendant was left running");
}

/// The child forks a `sleep` and kills it, over and over, so the deadline's
/// group SIGKILL lands while it is forking: XNU leaves the child of that fork
/// unsignalled, a `sleep` nothing kills ([`aterm_uds::process::kill_group_until_empty`]).
#[cfg(unix)]
#[test]
fn a_group_still_forking_at_the_deadline_is_killed_whole() {
    let (pgid, _) = capture_a_hanging_group(
        "forking-group",
        "echo \"$$ $(\"$ATERM_TEST_PS\" -o pgid= -p $$)\" > \"$PGIDF\"; while :; do \"$ATERM_TEST_SLEEP\" \"$HANG_S\" & kill -KILL $!; done",
        "ran past its",
    );
    assert!(
        group_emptied(pgid),
        "a child forked as the group was killed ran on"
    );
}

#[cfg(unix)]
const ESCAPED_HOLDER_CAP_TICKS: u32 = 2_400; // x 50 ms

/// Set in the environment when this test binary is the escape test's capture child.
#[cfg(unix)]
const ESCAPE_STAGE: &str = "ATERM_ALIGN_ESCAPE_STAGE";

/// Set as well when the stage's capture child prints [`STOP`] on stderr once its holder escaped.
#[cfg(unix)]
const ESCAPE_STOP: &str = "ATERM_ALIGN_ESCAPE_STOP";

/// The escape test's capture ([`escaped_holder_stage`]).
#[cfg(unix)]
fn escaped_pipe_command(done: &Path, exited: &Path, escaped: &Path) -> Command {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "--exact",
        "harness::align::tests::escaped_descendant_holding_stdout_cannot_retain_a_reader_thread",
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

/// Set the capture's `after_spawn` step ([`HANG_DETECTOR`]).
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

#[cfg(unix)]
fn escaped_holder_stage() -> ! {
    use std::os::unix::ffi::OsStrExt as _;
    let path = |var: &str| {
        let path = std::env::var_os(var).expect("the staged paths");
        std::ffi::CString::new(path.as_bytes()).expect("a path with no NUL")
    };
    let (done, exited, escaped) = (
        path("ATERM_ALIGN_ESCAPE_DONE"),
        path("ATERM_ALIGN_ESCAPE_EXITED"),
        path("ATERM_ALIGN_ESCAPE_ESCAPED"),
    );
    let top = aterm_uds::spawnfd::descriptor_bound();
    let print_stop = std::env::var_os(ESCAPE_STOP).is_some();
    let interrupted = || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
    let mut sync: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `pipe` fills the two-element array; after `fork` the holder makes only
    // async-signal-safe calls on memory prepared before it and never returns, and this branch
    // leaves through `_exit`.
    unsafe {
        assert_eq!(libc::pipe(sync.as_mut_ptr()), 0, "pipe");
        match libc::fork() {
            -1 => panic!("fork: {}", std::io::Error::last_os_error()),
            0 => {
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
                libc::close(sync[1]);
                let mut byte = 0u8;
                let got = loop {
                    let n = libc::read(sync[0], (&raw mut byte).cast(), 1);
                    if n >= 0 || !interrupted() {
                        break n;
                    }
                };
                if print_stop {
                    libc::write(2, STOP.as_ptr().cast(), STOP.len());
                }
                libc::_exit(if got == 1 { 0 } else { 1 })
            }
        }
    }
}

/// Release an orphaned fixture even if an assertion panics.
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

/// Snapshot before releasing the escaped descendant.
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

/// Tier-1: project actual child/pipe-worker completion from one invocation onto the derived
/// lifecycle model.
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
    if std::env::var_os(ESCAPE_STAGE).is_some() {
        escaped_holder_stage();
    }
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
        None,
        Some(Arc::clone(&observed)),
    );
    let returned = CaptureObservedState::from(observed.as_ref());
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

    let got = capture_bounded(print("aaaaaaaaaaaaaaaaaaaa"), HANG_DETECTOR, 4).expect("it runs");
    assert_eq!(got.stdout, b"aaaa");

    let got = capture_bounded(shell("exit 7"), HANG_DETECTOR, 16).expect("it runs");
    assert_eq!(got.code, Some(7));
    assert!(!got.success());
}

/// The stop text the watched-capture tests print.
const STOP: &[u8] = b"Authentication required";

#[cfg(unix)]
#[test]
fn a_stop_text_on_stdout_ends_the_capture_before_its_deadline() {
    let started = std::time::Instant::now();
    let got = capture_bounded_until(
        shell("printf 'Authentication required. Please visit'; \"$ATERM_TEST_SLEEP\" 120"),
        HANG_DETECTOR,
        1024,
        STOP,
    );
    assert_eq!(got, Ok(Ended::Stopped));
    assert!(
        started.elapsed() < HANG_DETECTOR,
        "it waited out its deadline: {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn a_stop_text_on_stderr_ends_the_capture_and_reaps_the_child() {
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let got = capture_bounded_impl(
        shell("printf 'Authentication ' >&2; printf 'required' >&2; \"$ATERM_TEST_SLEEP\" 120"),
        HANG_DETECTOR,
        1024,
        Some(STOP),
        Some(Arc::clone(&observed)),
    );
    assert_eq!(
        got,
        Ok(Ended::Stopped),
        "a match split across writes still counts"
    );
    assert_capture_return_conforms(
        CaptureObservedState::from(observed.as_ref()),
        false,
        "reader_done",
    );
}

#[cfg(unix)]
#[test]
fn a_stop_text_written_just_before_exit_is_still_seen() {
    let got = capture_bounded_until(
        shell("printf '{}'; printf 'Authentication required' >&2; exit 1"),
        HANG_DETECTOR,
        1024,
        STOP,
    );
    assert_eq!(got, Ok(Ended::Stopped));
}

#[test]
fn a_watched_capture_without_the_stop_text_reports_as_before() {
    let line = if cfg!(windows) {
        "<nul set /p =hello& exit 0"
    } else {
        "printf hello; printf 'some log' >&2"
    };
    let got = capture_bounded_until(shell(line), HANG_DETECTOR, 1024, STOP).expect("it runs");
    let Ended::Exited(got) = got else {
        panic!("no stop text was printed: {got:?}");
    };
    assert_eq!(got.stdout, b"hello");
    assert!(got.success());
}

#[test]
fn a_stop_text_split_across_chunks_is_found() {
    let mut seen = Vec::new();
    assert!(!append_and_find(&mut seen, b"xx Authentication ", STOP));
    assert!(append_and_find(&mut seen, b"required", STOP));
    let mut seen = Vec::new();
    let found = STOP
        .chunks(1)
        .map(|byte| append_and_find(&mut seen, byte, STOP))
        .collect::<Vec<_>>();
    assert_eq!(found.iter().filter(|f| **f).count(), 1, "{found:?}");
    assert_eq!(found.last(), Some(&true), "found on its last byte");
    assert!(!append_and_find(
        &mut Vec::new(),
        b"Authentication requir",
        STOP
    ));
}

/// A capture stopped on stderr while a holder outside the killed group keeps stdout open joins
/// its reader at once (the cancel pipe), not when the deadline would have freed it.
#[cfg(unix)]
#[test]
fn a_stop_on_stderr_returns_while_an_escaped_holder_keeps_stdout() {
    let tmp = temp_dir("escaped-stop");
    let done = tmp.path().join("done");
    let exited = tmp.path().join("exited");
    let escaped = tmp.path().join("escaped");
    let mut holder = EscapedHolderGuard::new(done.clone(), exited.clone());
    let mut cmd = escaped_pipe_command(&done, &exited, &escaped);
    cmd.env(ESCAPE_STOP, "1");
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let marker = escaped.clone();
    settle_until(&observed, move || marker.exists());
    let started = std::time::Instant::now();
    let result = capture_bounded_impl(
        cmd,
        HANG_DETECTOR,
        1024,
        Some(STOP),
        Some(Arc::clone(&observed)),
    );
    let took = started.elapsed();
    let returned = CaptureObservedState::from(observed.as_ref());
    let holder_held_on = !exited.exists();
    holder.release();
    assert_eq!(result, Ok(Ended::Stopped));
    assert!(
        holder_held_on,
        "the holder still held stdout when the capture returned"
    );
    assert!(
        took < HANG_DETECTOR,
        "the reader was freed only by the deadline: {took:?}"
    );
    assert_capture_return_conforms(returned, false, "reader_done");
}
