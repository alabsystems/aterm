// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] and its conformance bind to `harness_capture_worker_lifecycle_model`.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// The deadline of a capture whose child must finish.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[cfg(unix)]
#[test]
fn an_unsignalable_group_is_resolved_by_the_owning_wait() {
    for error in [libc::ESRCH, libc::EPERM, libc::EIO] {
        let observed = Arc::new(CaptureLifecycleObservation::default());
        *observed.group_signal.lock().unwrap() = Some(Box::new(move |_| {
            Err(std::io::Error::from_raw_os_error(error))
        }));
        let mut cmd = shell("exec \"$ATERM_TEST_SLEEP\" 120");
        cmd.process_group(0);
        let child = cmd.spawn().unwrap();
        let mut owner = CaptureOwner::new(child, Some(observed.clone()));
        assert!(
            !owner.child.exited().unwrap(),
            "the exit is not observable yet"
        );
        let result = owner.stop(true);
        assert!(observed.child_reaped.load(Ordering::Acquire));
        assert_eq!(
            result.is_ok(),
            error != libc::EIO,
            "errno {error}: {result:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn lost_capture_custody_never_accepts_a_later_status_or_numeric_operation() {
    use std::os::unix::process::ExitStatusExt as _;
    let positive = std::process::ExitStatus::from_raw(0);
    let mut custody = CaptureCustody::Owned;
    custody.observe(&Err(std::io::ErrorKind::WouldBlock.into()));
    assert!(custody.owns_unreaped());
    custody.observe(&Err(std::io::Error::from_raw_os_error(libc::ECHILD)));
    assert_eq!(custody, CaptureCustody::Lost);
    custody.observe(&Ok(Some(positive)));
    assert_eq!(custody, CaptureCustody::Lost);
    assert!(
        !custody.owns_unreaped(),
        "neither wait nor signal may use the old numeric identity"
    );
    assert_eq!(custody.reaped(), None);
    let mut retained = CaptureCustody::Owned;
    retained.observe(&Err(std::io::ErrorKind::Interrupted.into()));
    retained.observe(&Ok(Some(positive)));
    assert_eq!(retained.reaped(), Some(positive));
    assert!(!retained.owns_unreaped());
    let mut signal_check = CaptureCustody::Owned;
    signal_check.observe_error(&std::io::Error::from_raw_os_error(libc::ECHILD));
    signal_check.observe(&Ok(Some(positive)));
    assert_eq!(signal_check, CaptureCustody::Lost);
    assert!(!signal_check.owns_unreaped());
}

#[cfg(unix)]
#[test]
fn capture_unwind_settles_the_actual_child_and_any_started_reader() {
    for after_reader in [false, true] {
        let observed = Arc::new(CaptureLifecycleObservation::default());
        let hook: Box<dyn FnOnce() + Send> = Box::new(|| panic!("synthetic caller unwind"));
        if after_reader {
            *observed.after_reader.lock().unwrap() = Some(hook);
        } else {
            *observed.after_spawn.lock().unwrap() = Some(hook);
        }
        let mut first = true;
        *observed.owning_wait.lock().unwrap() = Some(Box::new(move |child| {
            if first {
                first = false;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            child.wait()
        }));
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capture_bounded_impl(
                shell("exec \"$ATERM_TEST_SLEEP\" 120"),
                HANG_DETECTOR,
                1024,
                None,
                Some(observed.clone()),
            )
        }));
        assert!(got.is_err());
        assert!(observed.child_reaped.load(Ordering::Acquire));
        assert_eq!(observed.reader_joined.load(Ordering::Acquire), after_reader);
    }
}

#[cfg(unix)]
#[test]
fn a_reader_panic_and_its_payload_destructor_cannot_unwind_capture_custody() {
    struct Payload;
    impl Drop for Payload {
        fn drop(&mut self) {
            panic!("synthetic reader payload destructor");
        }
    }
    let observed = Arc::new(CaptureLifecycleObservation::default());
    *observed.reader_step.lock().unwrap() = Some(Box::new(|| std::panic::panic_any(Payload)));
    let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        capture_bounded_impl(
            shell("exec \"$ATERM_TEST_SLEEP\" 120"),
            HANG_DETECTOR,
            1024,
            None,
            Some(observed.clone()),
        )
    }));
    assert!(got.unwrap().is_err());
    assert!(observed.child_reaped.load(Ordering::Acquire));
    assert!(observed.reader_joined.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn an_external_capture_watch_reaps_and_joins_with_open_or_closed_stdout() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("synthetic payload destructor");
        }
    }
    for owned in [false, true] {
        for closed in [false, true] {
            for panics in [false, true] {
                let observed = Arc::new(CaptureLifecycleObservation::default());
                let mut watch = |_: &mut CaptureInput<'_>| {
                    if panics {
                        std::panic::panic_any(PanicOnDrop);
                    }
                    true
                };
                let line = if closed {
                    "exec 1>&-; exec \"$ATERM_TEST_SLEEP\" 120"
                } else {
                    "exec \"$ATERM_TEST_SLEEP\" 120"
                };
                let got = capture_with(
                    shell(line),
                    HANG_DETECTOR,
                    1024,
                    None,
                    Some(observed.clone()),
                    owned,
                    Some(&mut watch),
                );
                let case = format!("owned {owned}, stdout closed {closed}, watch panics {panics}");
                if panics {
                    assert_eq!(got.unwrap_err(), "its capture watch panicked", "{case}");
                } else {
                    assert_eq!(got.unwrap(), Ended::Stopped, "{case}");
                }
                assert!(observed.child_reaped.load(Ordering::Acquire), "{case}");
                assert!(observed.reader_done.load(Ordering::Acquire), "{case}");
            }
        }
    }
    let ran =
        capture_bounded_owned_watching(shell("printf kept; exit 7"), HANG_DETECTOR, 1024, |_| {
            false
        })
        .unwrap();
    assert_eq!(
        ran,
        Ended::Exited(Captured {
            stdout: b"kept".to_vec(),
            code: Some(7)
        })
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn completed_output_and_observed_exit_win_over_a_terminal_capture_watch() {
    for owned in [false, true] {
        for code in [0, 1, 130] {
            let observed = Arc::new(CaptureLifecycleObservation::default());
            *observed.child_ready.lock().unwrap() = Some(Box::new(|child| {
                let deadline = Instant::now() + HANG_DETECTOR;
                let mut exit = aterm_uds::deadline::ExitWait::new(child);
                while !child.exited().unwrap() {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(!remaining.is_zero(), "the owned child never exited");
                    exit.wait(remaining);
                }
            }));
            let reader = observed.clone();
            *observed.after_reader.lock().unwrap() = Some(Box::new(move || {
                // This case requires both completed output and terminal exit. Exit alone
                // does not establish that the newly spawned reader has queued its output.
                aterm_spec::wait::until_within(HANG_DETECTOR, "completed capture output", || {
                    reader.reader_done.load(Ordering::Acquire).then_some(())
                });
            }));
            let mut failed = |_: &mut CaptureInput<'_>| true;
            let ran = capture_with(
                shell(&format!("printf kept; exit {code}")),
                HANG_DETECTOR,
                1024,
                None,
                Some(observed.clone()),
                owned,
                Some(&mut failed),
            )
            .unwrap();
            assert_eq!(
                ran,
                Ended::Exited(Captured {
                    stdout: b"kept".to_vec(),
                    code: Some(code)
                }),
                "owned {owned}"
            );
            assert!(
                observed.child_reaped.load(Ordering::Acquire),
                "owned {owned}"
            );
            assert!(
                observed.reader_done.load(Ordering::Acquire),
                "owned {owned}"
            );
        }
    }
}

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
fn capture_a_hanging_group(tag: &str, script: &str) -> (libc::pid_t, Duration) {
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
    use std::os::fd::IntoRawFd as _;
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
    let (sync_r, sync_w) = aterm_uds::pipe().expect("pipe");
    let sync: [libc::c_int; 2] = [sync_r.into_raw_fd(), sync_w.into_raw_fd()];
    // The sync pipe's write end is the holder's: unmarked for this fork alone.
    let handing = aterm_uds::spawn_lock::handing(&[sync[1]]);
    // SAFETY: after `fork` the holder makes only async-signal-safe calls on memory prepared
    // before it and never returns, and this branch leaves through `_exit`.
    unsafe {
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
                // The holder's copy of the hold is never released: it `_exit`s.
                drop(handing);
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
    reader_joined: bool,
}

#[cfg(unix)]
impl From<&CaptureLifecycleObservation> for CaptureObservedState {
    fn from(observed: &CaptureLifecycleObservation) -> Self {
        Self {
            child_reaped: observed.child_reaped.load(Ordering::Acquire),
            reader_joined: observed.reader_joined.load(Ordering::Acquire),
        }
    }
}

/// Tier-1: project actual owning reap and reader join from one invocation onto the derived
/// lifecycle model. Its historical `reader_done` variable means the join receipt here.
#[cfg(unix)]
fn assert_capture_return_conforms(
    observed: CaptureObservedState,
    deadline: bool,
    still_live: &'static str,
) {
    let model = aterm_models_harness::harness_capture_worker_lifecycle_model();
    let mut state = model.init_state();
    state.insert("child_reaped", i64::from(observed.child_reaped));
    state.insert("reader_done", i64::from(observed.reader_joined));
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

/// An owned capture that runs out ends its provider by proof first: the provider says it got
/// `SIGTERM`, and it and what it left in its group are gone, each by its pid and birth.
#[cfg(unix)]
#[test]
fn an_owned_capture_past_its_deadline_ends_its_child_by_proof_then_its_group() {
    let provider = owned_fixture::Provider::new("deadline");
    let observed = Arc::new(CaptureLifecycleObservation::default());
    settle_until(&observed, provider.ready());
    let why = capture_with(
        Command::new(provider.program()),
        Duration::from_millis(200),
        1024,
        None,
        Some(Arc::clone(&observed)),
        true,
        None,
    )
    .expect_err("a provider that waits for ever is an error, not a wait");
    assert!(why.contains("deadline"), "{why}");
    let [leader, member] = provider.said();
    assert!(
        provider.termed(),
        "the provider (pid {}) got no SIGTERM before its group's kill",
        leader.pid
    );
    assert!(
        leader.gone_within_a_minute(),
        "the provider (pid {}) ran on past its capture's deadline",
        leader.pid
    );
    assert!(
        member.gone_within_a_minute(),
        "what the provider left in its group (pid {}) ran on past its capture's deadline",
        member.pid
    );
    assert!(
        observed.child_reaped.load(Ordering::Acquire),
        "the provider was reaped"
    );
}

/// A CAPTURE'S BUDGET COUNTS THE TIME ITS HOST SLEPT (KI-1105-14): where the host slept past
/// the budget while the capture waited on its child, the capture ends the child and what it left
/// in its group at its next look, owned (a renewal's) or not, though the clock that stops in
/// sleep leaves an hour of the budget. NEGATIVE CONTROLS: the rule that measured the budget on
/// that clock alone waited the hour out, and so did an unowned capture's single wait, which
/// cannot see the other clock; the one-minute hang detector reports either.
#[cfg(unix)]
#[test]
fn a_capture_budget_counts_the_time_its_host_slept() {
    const HOUR: Duration = Duration::from_secs(3600);
    assert!(
        aterm_uds::process::continuous_ns().is_some(),
        "this host has a clock that counts through sleep"
    );
    for (owned, watching) in [(false, false), (true, false), (true, true)] {
        let provider = owned_fixture::Provider::new(if owned { "slept-owned" } else { "slept" });
        let observed = Arc::new(CaptureLifecycleObservation::default());
        settle_until(&observed, provider.ready());
        // The host sleeps two hours once the capture waits on its child.
        let sleeps = Arc::clone(&observed);
        *observed
            .in_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
            let two_hours = u64::try_from((2 * HOUR).as_nanos()).expect("in range");
            sleeps.slept_ns.store(two_hours, Ordering::Release);
        }));
        let (tx, rx) = mpsc::channel();
        let program = provider.program();
        let run = Arc::clone(&observed);
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let effect = effects.clone();
        std::thread::spawn(move || {
            let mut watch = |_: &mut CaptureInput<'_>| {
                effect.fetch_add(1, Ordering::AcqRel);
                false
            };
            let _ = tx.send(capture_with(
                Command::new(program),
                HOUR,
                1024,
                None,
                Some(run),
                owned,
                if watching { Some(&mut watch) } else { None },
            ));
        });
        let why = rx
            .recv_timeout(HANG_DETECTOR)
            .unwrap_or_else(|_| {
                panic!(
                    "owned={owned}: the capture waited on the clock that stops in sleep, past the \
                     budget its host slept through"
                )
            })
            .expect_err("a provider that waits for ever is an error, not a wait");
        assert!(
            why.contains(&format!("ran past its {} ms deadline", HOUR.as_millis())),
            "owned={owned}: {why}"
        );
        assert_eq!(
            effects.load(Ordering::Acquire),
            0,
            "a slept deadline cannot admit a fresh watch effect"
        );
        let [leader, member] = provider.said();
        assert!(
            leader.gone_within_a_minute(),
            "owned={owned}: the provider (pid {}) ran on past its slept deadline",
            leader.pid
        );
        assert!(
            member.gone_within_a_minute(),
            "owned={owned}: what the provider left in its group (pid {}) ran on",
            member.pid
        );
        assert!(
            observed.child_reaped.load(Ordering::Acquire),
            "owned={owned}: the provider was reaped"
        );
    }
}

/// THE WAIT FOR A CHILD THAT CLOSED ITS OUTPUT COUNTS THE TIME ITS HOST SLEPT TOO: a child whose
/// group closed its stdout and runs on, the host sleeping past the budget once that end is read,
/// is ended and reaped at the next look, owned or not, with the words for that end. NEGATIVE
/// CONTROL: that wait measured on the clock that stops in sleep alone waited the hour out, which
/// the one-minute hang detector reports.
#[cfg(unix)]
#[test]
fn the_wait_for_a_child_that_closed_its_output_counts_the_time_its_host_slept() {
    const HOUR: Duration = Duration::from_secs(3600);
    for (owned, watching) in [(false, false), (true, false), (true, true)] {
        let tmp = temp_dir(if owned {
            "slept-closed-owned"
        } else {
            "slept-closed"
        });
        let pid_file = tmp.path().join("pid");
        let mut cmd = shell("exec 1>&-; echo $$ > \"$PIDF\"; exec \"$ATERM_TEST_SLEEP\" 3600");
        cmd.env("PIDF", &pid_file);
        let observed = Arc::new(CaptureLifecycleObservation::default());
        let report = pid_file.clone();
        settle_until(&observed, move || {
            std::fs::read_to_string(&report).is_ok_and(|r| r.ends_with('\n'))
        });
        // The end of its output is queued for the wait before the wait looks.
        let read = Arc::clone(&observed);
        *observed
            .after_reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
            aterm_spec::wait::until_within(HANG_DETECTOR, "the reader read the end", || {
                read.reader_done.load(Ordering::Acquire).then_some(())
            });
        }));
        // The host sleeps two hours once the wait has taken what is left of the budget.
        let sleeps = Arc::clone(&observed);
        *observed
            .in_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
            let two_hours = u64::try_from((2 * HOUR).as_nanos()).expect("in range");
            sleeps.slept_ns.store(two_hours, Ordering::Release);
        }));
        let (tx, rx) = mpsc::channel();
        let run = Arc::clone(&observed);
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let effect = effects.clone();
        std::thread::spawn(move || {
            let mut watch = |_: &mut CaptureInput<'_>| {
                effect.fetch_add(1, Ordering::AcqRel);
                false
            };
            let _ = tx.send(capture_with(
                cmd,
                HOUR,
                1024,
                None,
                Some(run),
                owned,
                if watching { Some(&mut watch) } else { None },
            ));
        });
        let why = rx
            .recv_timeout(HANG_DETECTOR)
            .unwrap_or_else(|_| {
                panic!(
                    "owned={owned}: the wait for a child that closed its output counted only the \
                     time its host was awake"
                )
            })
            .expect_err("a child that closed its output and runs on is an error, not a wait");
        assert!(
            why.contains("it closed its output and did not exit before its deadline"),
            "owned={owned}: {why}"
        );
        assert_eq!(
            effects.load(Ordering::Acquire),
            0,
            "closed output does not admit a fresh watch effect after sleep"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|r| r.trim().parse::<libc::pid_t>().ok())
            .expect("the child's pid");
        assert!(
            observed.child_reaped.load(Ordering::Acquire),
            "owned={owned}: the child was reaped"
        );
        assert!(
            group_emptied(pid),
            "owned={owned}: the child's group was left running"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_full_owned_input_pipe_cannot_outlive_sleep_or_scope_cancellation() {
    use crate::harness::account_work::Lifetime;
    use std::io::Write as _;
    for cancel in [false, true] {
        let provider = owned_fixture::Provider::unread_input("full-input");
        let observed = Arc::new(CaptureLifecycleObservation::default());
        settle_until(&observed, provider.ready());
        let lifetime = Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let paused = Arc::new(std::sync::Mutex::new(None));
        let paused_at_wait = paused.clone();
        let close = lifetime.clone();
        let slept = observed.clone();
        let saw_full = Arc::new(AtomicBool::new(false));
        let full = saw_full.clone();
        *observed.in_input_wait.lock().unwrap() = Some(Box::new(move || {
            full.store(true, Ordering::Release);
            if cancel {
                *paused_at_wait.lock().unwrap() = close.pause();
            } else {
                slept
                    .slept_ns
                    .store(2 * 3600 * 1_000_000_000, Ordering::Release);
            }
        }));
        let program = provider.program();
        let run = observed.clone();
        let watch_scope = scope.clone();
        let (answer, result) = mpsc::channel();
        let work = scope.wrap(move || {
            let mut watch = |input: &mut CaptureInput<'_>| {
                let pipe = input.stdin.as_mut().expect("actual owned input pipe");
                // SAFETY: this capture exclusively borrows its actual stdin pipe. Keep the
                // original flags so the writer itself must establish nonblocking operation.
                let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
                assert!(flags >= 0);
                capture_pipe_nonblocking(pipe.as_raw_fd()).unwrap();
                let mut filled = 0;
                loop {
                    match pipe.write(&[b'x'; 4096]) {
                        Ok(0) => panic!("a live owned pipe made no progress"),
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => panic!("fill actual owned stdin: {e}"),
                    }
                }
                // PIPE_BUF-sized writes can refuse while a smaller residual remains.
                // Exhaust that residual too, so the actual code write must wait.
                loop {
                    match pipe.write(b"x") {
                        Ok(0) => panic!("a live owned pipe made no progress"),
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) => panic!("fill actual owned stdin residual: {e}"),
                    }
                }
                assert!(filled > 0);
                // SAFETY: the same uniquely borrowed pipe remains open; restoring its
                // original flags changes no descriptor or peer. A blocking writer regression
                // can only recover once this test releases its owned child's input gate.
                assert_eq!(
                    unsafe {
                        libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK)
                    },
                    0
                );
                let written = input.write_all(b"oauth-code\n", || watch_scope.cancelled());
                assert!(
                    written.is_err(),
                    "full stdin must not report successful delivery after work ends"
                );
                true
            };
            let ended = capture_with(
                Command::new(program),
                Duration::from_secs(3600),
                1024,
                None,
                Some(run),
                true,
                Some(&mut watch),
            );
            let _ = answer.send(ended);
        });
        let (done, returned) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            work();
            let _ = done.send(());
        });
        let finished = returned.recv_timeout(HANG_DETECTOR);
        // Always let our actual owned child read its stdin before joining, even if a
        // broken implementation failed to recheck its budget/cancellation under backpressure.
        provider.release_input();
        if finished.is_err() {
            returned
                .recv_timeout(HANG_DETECTOR)
                .expect("cooperative input recovery returned");
        }
        worker.join().unwrap();
        let ended = result.recv_timeout(HANG_DETECTOR).unwrap();
        assert!(
            finished.is_ok(),
            "backpressure retained the actual capture beyond its work stop"
        );
        assert!(matches!(ended, Ok(Ended::Stopped)));
        assert!(saw_full.load(Ordering::Acquire));
        assert!(observed.child_reaped.load(Ordering::Acquire));
        assert!(observed.reader_joined.load(Ordering::Acquire));
        for born in provider.said() {
            assert!(born.gone_within_a_minute());
        }
        let pause = paused
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| lifetime.pause().unwrap());
        assert!(
            pause.try_wait().is_ok(),
            "capture resources must retire before the original Scope drains"
        );
    }
}

/// An unwind out of an owned capture's wait (here its settle panics) ends the provider and its
/// group on the way out, by pid and birth.
#[cfg(unix)]
#[test]
fn an_unwind_out_of_an_owned_capture_ends_its_child() {
    let provider = owned_fixture::Provider::new("unwind");
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let ready = provider.ready();
    *observed
        .after_spawn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
        let until = std::time::Instant::now() + HANG_DETECTOR;
        while !ready() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the wait for the provider unwinds");
    }));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        capture_with(
            Command::new(provider.program()),
            HANG_DETECTOR,
            1024,
            None,
            Some(Arc::clone(&observed)),
            true,
            None,
        )
    }));
    assert!(unwound.is_err(), "the capture unwound");
    let [leader, member] = provider.said();
    assert!(
        provider.termed(),
        "an unwind out of the capture gave its provider (pid {}) no SIGTERM before its group's \
         kill",
        leader.pid
    );
    assert!(
        leader.gone_within_a_minute(),
        "an unwind out of the capture left its provider (pid {}) running",
        leader.pid
    );
    assert!(
        member.gone_within_a_minute(),
        "an unwind out of the capture left the provider's group (pid {}) running",
        member.pid
    );
}

/// An owned capture whose provider signs in and exits by itself answers that exit at once, with
/// what it printed, while what it left in its group still holds its stdout, and signals nothing
/// it left there: the deadline is the hang detector, so a capture that waited for the pipe's end
/// would run out first.
#[cfg(unix)]
#[test]
fn an_owned_capture_answers_its_childs_own_exit_and_leaves_its_group_be() {
    let provider = owned_fixture::Provider::signing_in("own-exit");
    let observed = Arc::new(CaptureLifecycleObservation::default());
    let got = capture_with(
        Command::new(provider.program()),
        HANG_DETECTOR,
        1024,
        None,
        Some(Arc::clone(&observed)),
        true,
        None,
    )
    .expect("a provider that signs in and exits by itself is answered at its exit");
    let Ended::Exited(got) = got else {
        panic!("nothing was watched for, so nothing stops it: {got:?}");
    };
    assert_eq!(
        (got.code, got.stdout.as_slice()),
        (Some(0), owned_fixture::SIGNED_IN),
        "its exit and what it printed"
    );
    assert!(
        observed.child_reaped.load(Ordering::Acquire),
        "the provider was reaped"
    );
    let member = provider
        .member()
        .expect("what the provider left in its group was signalled at its own exit");
    assert!(
        observed.group_kills.load(Ordering::Acquire) == 0 && member.runs(),
        "what the provider left in its group (pid {}) was signalled at its own exit",
        member.pid
    );
}

/// An owned capture's end by proof reads the child's own unique id: a record whose unique id
/// moved gets no signal, and the child's own record gets its `SIGTERM`.
#[cfg(unix)]
#[test]
fn an_owned_end_signals_only_the_child_it_read() {
    let mut child = shell("exec \"$ATERM_TEST_SLEEP\" 600")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("start a sleep");
    let pid = i32::try_from(child.id()).expect("a pid");
    let lineage =
        aterm_spec::wait::until_within(HANG_DETECTOR, "the sleep runs, its thread run", || {
            (aterm_uds::process::thread_ran(pid) == Some(true))
                .then(|| aterm_uds::process::read_lineage(pid))
                .flatten()
        });
    let watched = aterm_uds::tether::Watched::of(&lineage);
    let moved = aterm_uds::tether::Watched {
        uniqueid: watched.uniqueid.wrapping_add(1),
        ..watched
    };
    assert_eq!(
        end_owned(moved),
        Err(aterm_uds::process::EndRefusal::NotProven),
        "a record whose unique id moved"
    );
    let ended = end_owned(watched);
    let _ = child.kill();
    let status = child.wait().expect("reap the sleep");
    assert!(
        ended.is_ok_and(|ended| ended.termed),
        "the child's own record: {ended:?}, {status:?}"
    );
}
