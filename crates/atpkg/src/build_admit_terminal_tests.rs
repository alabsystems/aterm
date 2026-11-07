// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated controlling terminals, never the person's terminal or process group.

use super::*;
use aterm_uds::spawnfd::{self, Stdio};
use std::io::Write as _;
use std::os::fd::{FromRawFd as _, OwnedFd};

struct OwnedChild(spawnfd::Child);
impl std::ops::Deref for OwnedChild {
    type Target = spawnfd::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Native kill is inert after reaping; a stopped child must never leave
        // fixture cleanup waiting for an exit it has not been told to make.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn diagnostics(child: &mut spawnfd::Child) -> String {
    use std::io::Read as _;
    let mut text = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut text).unwrap();
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut text).unwrap();
    }
    text
}

pub(super) const WORLD: &str = "ATPKG_NATIVE_TERMINAL_WORLD";
const MODE: &str = "ATPKG_NATIVE_TERMINAL_MODE";
const JOB_WORKER: &str = "ATPKG_NATIVE_TERMINAL_JOB_WORKER";
const QUIT_WORKER: &str = "ATPKG_NATIVE_TERMINAL_QUIT_WORKER";
pub(super) const CANCEL_WORKER: &str = "ATPKG_NATIVE_TERMINAL_CANCEL_WORKER";
pub(super) const CANCEL_EXIT_BARRIER: &str = "ATPKG_NATIVE_TERMINAL_CANCEL_EXIT_BARRIER";
#[path = "build_admit_live_tests.rs"]
mod live_tests;
static CANCEL_NOTE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn cancellation_requested(_: i32) {
    let fd = CANCEL_NOTE.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: the private worker owns a nonblocking pipe kept open through
        // this caught handler's lifetime; one byte, no allocation, preserve errno.
        unsafe {
            let errno = libc::__error();
            let saved = *errno;
            let byte = 1u8;
            libc::write(fd, (&raw const byte).cast(), 1);
            *errno = saved;
        }
    }
}

#[test]
fn native_cancel_worker_if_requested() {
    if std::env::var_os(CANCEL_WORKER).is_none() {
        return;
    }
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::ExitStatusExt as _;
    let world = PathBuf::from(std::env::var_os(WORLD).unwrap());
    let (mut read, write) = std::io::pipe().unwrap();
    // SAFETY: this worker's new pipe writer, before publishing it to its handler.
    assert_eq!(
        unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    CANCEL_NOTE.store(write.as_raw_fd(), std::sync::atomic::Ordering::SeqCst);
    // SAFETY: only this isolated worker's disposition, while its pipe stays open.
    let previous = unsafe {
        libc::signal(
            libc::SIGTERM,
            cancellation_requested as extern "C" fn(i32) as libc::sighandler_t,
        )
    };
    assert_ne!(previous, libc::SIG_ERR);
    let mut compiler = OwnedChild(
        spawnfd::command("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let input = aterm_uds::deadline::cat_echoes(&mut compiler);
    let running = std::env::var(CANCEL_WORKER).unwrap() == "running";
    if !running {
        // SAFETY: this worker owns its compiler child and is itself the private
        // admitted root. Both exact processes stop; neither is the resource owner.
        assert_eq!(
            unsafe { libc::kill(i32::try_from(compiler.id()).unwrap(), libc::SIGSTOP) },
            0
        );
        assert_eq!(
            compiler.wait_job_event().unwrap().acknowledge().unwrap(),
            Some(spawnfd::ChildEvent::Stopped(spawnfd::ChildStop::Signal(
                libc::SIGSTOP
            )))
        );
    }
    std::fs::write(world.join("root-pid"), std::process::id().to_string()).unwrap();
    std::fs::write(world.join("ready"), b"root and compiler ready").unwrap();
    // SAFETY: stop this private worker, whose controller remains available.
    if !running {
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) }, 0);
    }
    read.read_exact(&mut [0u8; 1]).unwrap();
    if std::env::var_os(CANCEL_EXIT_BARRIER).is_some() {
        // The controller acknowledges CONT before permitting this cleanup root to exit.
        // Its group therefore stays live while TERM's follow-up CONT is sent.
        std::io::stdin().read_exact(&mut [0u8; 1]).unwrap();
    }
    // Keep the writer until the actual signal status is collected, so EOF did
    // not end the stand-in. The root reaps its compiler before successful exit.
    assert_eq!(compiler.wait().unwrap().signal(), Some(libc::SIGTERM));
    drop(input);
    // SAFETY: restore before closing the pipe used by the caught handler.
    unsafe {
        libc::signal(libc::SIGTERM, previous);
    }
    CANCEL_NOTE.store(-1, std::sync::atomic::Ordering::SeqCst);
}

/// Exercise the private actor's caught handler synchronously, never default death.
fn inject_key(signal: i32) {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: query this private actor's disposition into a live output.
    assert_eq!(
        unsafe { libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful query initialized the complete action.
    let action = unsafe { action.assume_init() };
    assert_ne!(action.sa_sigaction, libc::SIG_DFL);
    assert_ne!(action.sa_sigaction, libc::SIG_IGN);
    #[expect(
        clippy::disallowed_methods,
        reason = "Runs only this isolated actor's verified caught handler on this thread, never a default core-dump death."
    )]
    // SAFETY: a caught disposition was checked; only this fixture thread is targeted.
    let result = unsafe { libc::raise(signal) };
    assert_eq!(result, 0);
}

extern "C" fn quit_received(_: i32) {
    // SAFETY: _exit is async-signal-safe; this private fixture acknowledges QUIT
    // by normal exit73, never its default core-dump death or Rust signal unwinding.
    unsafe { libc::_exit(73) }
}

#[test]
fn native_quit_worker_if_requested() {
    if std::env::var_os(QUIT_WORKER).is_none() {
        return;
    }
    let world = PathBuf::from(std::env::var_os(WORLD).unwrap());
    // SAFETY: only this isolated worker's disposition, before its readiness proof.
    assert_ne!(
        unsafe {
            libc::signal(
                libc::SIGQUIT,
                quit_received as extern "C" fn(i32) as libc::sighandler_t,
            )
        },
        libc::SIG_ERR
    );
    std::fs::write(world.join("key-ready"), b"caught QUIT installed").unwrap();
    let _ = std::io::stdin().read_line(&mut String::new()).unwrap();
}

#[test]
fn native_job_worker_if_requested() {
    if std::env::var_os(JOB_WORKER).is_none() {
        return;
    }
    let world = PathBuf::from(std::env::var_os(WORLD).unwrap());
    std::fs::write(world.join("ready"), b"worker ready").unwrap();
    let mut line = String::new();
    loop {
        match std::io::stdin().read_line(&mut line) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            result => {
                assert!(result.unwrap() > 0);
                break;
            }
        }
    }
    assert_eq!(line, "ready\n");
}

fn signal_mask() -> libc::sigset_t {
    let mut mask = std::mem::MaybeUninit::zeroed();
    // SAFETY: read this thread's mask without changing it, into a live output.
    assert_eq!(
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), mask.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful query initialized the complete mask.
    unsafe { mask.assume_init() }
}

fn foreground() -> i32 {
    // SAFETY: this helper runs only inside the isolated controlling-terminal actor.
    unsafe { libc::tcgetpgrp(0) }
}

/// Simulate a separate job-control writer inside this isolated test session.
/// Production handoff never changes a process-wide signal disposition.
fn other_foreground(group: i32) {
    // SAFETY: test-only actor and terminal; restore the exact disposition immediately.
    unsafe {
        let previous = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        let result = libc::tcsetpgrp(0, group);
        libc::signal(libc::SIGTTOU, previous);
        assert_eq!(result, 0);
    }
}

#[test]
fn native_terminal_actor_if_requested() {
    let Some(world) = std::env::var_os(WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    let trace = |step: &str| {
        std::fs::write(world.join("trace"), step).unwrap();
    };
    let mode = std::env::var(MODE).unwrap();
    // SAFETY: the parent created this new session with only an isolated PTY slave
    // on stdin. It is this actor's terminal, never the person's terminal.
    assert_eq!(unsafe { libc::ioctl(0, libc::TIOCSCTTY.into(), 0) }, 0);
    // SAFETY: process and controlling-terminal queries on this isolated actor.
    let original = unsafe { libc::getpgrp() };
    assert_eq!(foreground(), original);
    trace("controlling terminal ready");
    let original_mask = signal_mask();
    if mode.starts_with("live-") {
        live_tests::actor(&world, &mode);
        return;
    }
    if let Some(signal) = match mode.as_str() {
        "paused-int" => Some(libc::SIGINT),
        "paused-quit" => Some(libc::SIGQUIT),
        "shared-int" => Some(libc::SIGINT),
        "cohort-prelaunch-int" => Some(libc::SIGINT),
        "foreground-int" => Some(libc::SIGINT),
        _ => None,
    } {
        // SAFETY: only this isolated fixture actor's disposition is changed,
        // before arming the front. The launcher may have ignored these signals.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
        let queued = mode == "cohort-prelaunch-int";
        let worker_foreground = mode == "foreground-int";
        let front = if queued {
            front::Front::arm_cohort()
        } else {
            front::Front::arm()
        }
        .unwrap();
        if queued {
            assert!(
                front::key_passes(0),
                "unknown native birth cannot discard a key"
            );
            inject_key(signal);
        }
        let separate = mode != "shared-int";
        let mut command = if signal == libc::SIGQUIT {
            let mut command = spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "build_admit::tests::terminal::native_quit_worker_if_requested",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(QUIT_WORKER, "1");
            command
        } else {
            spawnfd::command("/bin/cat")
        };
        if separate {
            command.process_group(0);
        }
        let mut child = OwnedChild(
            command
                .start_suspended(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut input = child.stdin.take();
        acknowledge_initial_stop(&mut child).unwrap();
        front.aim(child.id(), None);
        if separate && !queued {
            child
                .take_foreground(std::fs::File::open("/dev/tty").unwrap())
                .unwrap();
        }
        if !queued {
            child.resume().unwrap();
            if signal == libc::SIGQUIT {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "caught QUIT worker ready",
                    || {
                        assert!(
                            !child.exited().unwrap(),
                            "QUIT worker ended before installing its caught handler"
                        );
                        world.join("key-ready").exists().then_some(())
                    },
                );
            }
            if !worker_foreground {
                // SAFETY: this actor owns the unreaped child; no numeric historical pid.
                assert_eq!(
                    unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGSTOP) },
                    0
                );
                assert_eq!(
                    front
                        .acknowledge_job(child.wait_job_event().unwrap())
                        .unwrap(),
                    Some(spawnfd::ChildEvent::Stopped(spawnfd::ChildStop::Signal(
                        libc::SIGSTOP
                    )))
                );
            }
        }
        assert_eq!(
            foreground(),
            if worker_foreground {
                i32::try_from(child.id()).unwrap()
            } else {
                original
            }
        );
        let passes = front::key_passes(0);
        if !queued {
            // Deliver this private controller's actual handler. The
            // foreground worker case also checks a signal sent to its background
            // controller's pid, which must still reach the actual worker.
            inject_key(signal);
        }
        // Joining the forwarder drains the synchronously enqueued key before
        // any continuation/EOF. This supplies a barrier without a timing guess.
        front.disarm();
        if !child.exited().unwrap() {
            child.resume().unwrap();
        }
        drop(input.take());
        let status = child.wait().unwrap();
        std::fs::write(world.join("ready"), b"key control ended").unwrap();
        assert_eq!(foreground(), original);
        assert_eq!(signal_mask(), original_mask);
        use std::os::unix::process::ExitStatusExt as _;
        assert!(
            passes == separate
                && if separate {
                    if signal == libc::SIGQUIT {
                        status.code() == Some(73)
                    } else {
                        status.signal() == Some(signal)
                    }
                } else {
                    status.success()
                },
            "key route {mode}: passes={passes}, status={status}, expected signal={signal}"
        );
        return;
    }
    if mode == "job-events" || mode == "job-terminate" {
        // An interactive shell starts job children with these defaults. This
        // actor owns a private session; no production signal policy is changed.
        for signal in [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU] {
            // SAFETY: isolated fixture process, before spawning its actual job.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
            }
        }
        let machine = machine();
        let selected = cohort_seat(&world, &machine);
        let ledger = selected.ledger.as_ref().unwrap();
        let Admission::Claimed(held) = admit_cohort(&test_build(), &world, &selected, &mut |_| {})
        else {
            panic!("event admission refused")
        };
        let expected = held.claim().record().need.mem;
        std::fs::write(
            world.join("fence"),
            ledger.dir().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        std::fs::write(world.join("budget"), expected.to_string()).unwrap();
        let terminate = mode == "job-terminate";
        let mut command = if terminate {
            let mut command = spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "build_admit::tests::terminal::native_cancel_worker_if_requested",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CANCEL_WORKER, "1");
            command
        } else {
            let mut command = spawnfd::command(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "build_admit::tests::terminal::native_job_worker_if_requested",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(JOB_WORKER, "1");
            command
        };
        command.stdout(Stdio::null()).stderr(Stdio::from(
            std::fs::File::create(world.join("worker-errors")).unwrap(),
        ));
        let mut stops = 0;
        let mut resumes = 0;
        let mut exited = false;
        let mut group_requests = Vec::new();
        let status = supervise_terminal_events(
            *held,
            &mut command,
            std::fs::File::open("/dev/tty").unwrap(),
            &mut |_| {},
            &mut |event| match event {
                SupervisedEvent::Stopped(signal) => {
                    stops += 1;
                    assert_eq!(
                        signal,
                        if terminate {
                            libc::SIGSTOP
                        } else if stops == 1 {
                            libc::SIGTSTP
                        } else {
                            libc::SIGTTIN
                        }
                    );
                    assert_eq!(foreground(), original, "stop yields the terminal");
                    let rows = ledger.list().unwrap();
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0].need.mem, expected);
                    assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
                    std::fs::write(world.join(format!("paused-{stops}")), b"held").unwrap();
                    aterm_spec::wait::until_within(
                        aterm_spec::wait::MUST_HAPPEN,
                        "explicit owner resume decision",
                        || world.join(format!("resume-{stops}")).exists().then_some(()),
                    );
                    Ok(if terminate {
                        JobControl::TerminateGroup
                    } else if stops == 1 {
                        JobControl::ResumeBackground
                    } else {
                        JobControl::ResumeForeground
                    })
                }
                SupervisedEvent::ResumeAccepted { foreground } => {
                    resumes += 1;
                    assert_eq!(foreground, resumes == 2);
                    std::fs::write(world.join(format!("resumed-{resumes}")), b"accepted").unwrap();
                    Ok(JobControl::Wait)
                }
                SupervisedEvent::RootExited(_usage) => {
                    exited = true;
                    Ok(JobControl::Wait)
                }
                SupervisedEvent::RootExitObserved => {
                    panic!("old event-only supervisor does not retain root exit")
                }
                SupervisedEvent::CohortDrained => Ok(JobControl::Wait),
                SupervisedEvent::CohortRetained => panic!("fixture group must drain"),
                SupervisedEvent::GroupRequestAccepted(signal) => {
                    group_requests.push(signal);
                    let rows = ledger.list().unwrap();
                    assert_eq!(rows.len(), 1, "acceptance does not release the claim");
                    assert_eq!(rows[0].need.mem, expected);
                    assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
                    Ok(JobControl::Wait)
                }
            },
        )
        .unwrap();
        assert!(
            status.success(),
            "worker {status}: {}",
            std::fs::read_to_string(world.join("worker-errors")).unwrap_or_default()
        );
        assert_eq!(stops, if terminate { 1 } else { 2 });
        assert_eq!(resumes, if terminate { 0 } else { 2 });
        assert_eq!(
            group_requests,
            if terminate {
                vec![
                    aterm_capacity::cohort::launch::CohortSignal::Terminate,
                    aterm_capacity::cohort::launch::CohortSignal::Continue,
                ]
            } else {
                Vec::new()
            }
        );
        assert!(exited);
        if terminate {
            assert!(
                !selected.own.as_ref().unwrap().exists(),
                "cancelled success cannot train a whole-build memory sample"
            );
        }
        assert_eq!(foreground(), original);
        assert_eq!(signal_mask(), original_mask);
        assert!(ledger.list().unwrap().is_empty());
        assert!(!command.starts_suspended());
        return;
    }
    let mut command = spawnfd::command(if mode == "signal" {
        "/bin/cat"
    } else {
        "/bin/sh"
    });
    if mode != "signal" {
        command.args(["-c", "read line; test \"$line\" = ready"]);
    }
    command
        .process_group(0)
        .start_suspended(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if mode == "supervise" || mode == "bad-terminal" {
        command = spawnfd::command("/bin/sh");
        command.args(["-c", "printf ran > \"$ATPKG_NATIVE_TERMINAL_WORLD/target-ran\"; printf go > \"$ATPKG_NATIVE_TERMINAL_WORLD/ready\"; read line; test \"$line\" = ready"])
            .env(WORLD, &world).stdout(Stdio::null()).stderr(Stdio::null());
        let machine = machine();
        let selected = cohort_seat(&world, &machine);
        let Admission::Claimed(held) = admit_cohort(&test_build(), &world, &selected, &mut |_| {})
        else {
            panic!("terminal cohort admission refused")
        };
        let terminal = std::fs::File::open(if mode == "bad-terminal" {
            "/dev/null"
        } else {
            "/dev/tty"
        })
        .unwrap();
        let result = supervise_terminal(*held, &mut command, terminal, &mut |_| {});
        if mode == "bad-terminal" {
            assert!(result.is_err());
            assert!(
                !world.join("target-ran").exists(),
                "bad handoff never resumes target code"
            );
            std::fs::write(world.join("ready"), b"refused").unwrap();
        } else {
            assert!(result.unwrap().success());
        }
        assert!(
            !command.starts_suspended(),
            "caller command policy restored"
        );
        assert_eq!(foreground(), original);
        assert_eq!(signal_mask(), original_mask);
        assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
        return;
    }
    trace("spawning suspended child");
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(
        !child.exited().unwrap(),
        "a start-suspended child is still live"
    );
    assert!(
        child
            .take_foreground(std::fs::File::open("/dev/null").unwrap())
            .is_err(),
        "a non-terminal grants no custody"
    );
    assert_eq!(foreground(), original);
    trace("child spawned; taking foreground");
    child
        .take_foreground(std::fs::File::open("/dev/tty").unwrap())
        .unwrap();
    trace("foreground taken; checking nonblocking wait");
    let group = i32::try_from(child.id()).unwrap();
    assert_eq!(
        signal_mask(),
        original_mask,
        "handoff restores the caller's signal mask"
    );
    assert!(
        child
            .take_foreground(std::fs::File::open("/dev/tty").unwrap())
            .is_err(),
        "a second lease cannot replace the active one"
    );
    assert_eq!(foreground(), group);
    assert!(
        child.try_wait().unwrap().is_none(),
        "a live child is not reaped"
    );
    trace("nonblocking wait done; checking resource wait");
    assert!(child.try_wait_with_rusage().unwrap().is_none());
    trace("resource wait done; resuming child");
    assert_eq!(foreground(), group, "nonblocking reads keep live custody");
    if mode == "elsewhere" {
        let mut other = OwnedChild(
            spawnfd::command("/bin/cat")
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        assert!(
            other
                .take_foreground(std::fs::File::open("/dev/tty").unwrap())
                .is_err(),
            "a background controller cannot steal foreground"
        );
        child.kill().unwrap();
        let other_group = i32::try_from(other.id()).unwrap();
        other_foreground(other_group);
        std::fs::write(world.join("ready"), b"elsewhere").unwrap();
        assert!(!child.wait().unwrap().success());
        assert_eq!(
            foreground(),
            other_group,
            "an intervening handoff is respected"
        );
        other_foreground(original);
        other.stdin.take();
        assert!(other.wait().unwrap().success());
        return;
    }
    child.resume().unwrap();
    trace("child resumed; ready for input");
    std::fs::write(world.join("ready"), b"go").unwrap();
    let status = match mode.as_str() {
        "wait" | "signal" => child.wait().unwrap(),
        "try" => aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "native child exit",
            || child.try_wait().unwrap(),
        ),
        "usage" => child.wait_with_rusage().unwrap().status,
        "try-usage" => {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "native child resource exit",
                || child.try_wait_with_rusage().unwrap(),
            )
            .status
        }
        _ => panic!("unknown terminal mode"),
    };
    if mode == "signal" {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            status.signal(),
            Some(libc::SIGINT),
            "the actual terminal delivered Ctrl-C"
        );
    } else {
        assert!(
            status.success(),
            "the child read its actual foreground terminal: {status}"
        );
    }
    assert_eq!(
        foreground(),
        original,
        "the wait restored foreground BEFORE reaping"
    );
    assert!(
        child
            .take_foreground(std::fs::File::open("/dev/tty").unwrap())
            .is_err(),
        "a reaped child cannot claim terminal custody"
    );
    assert_eq!(
        signal_mask(),
        original_mask,
        "restoration keeps the caller's signal mask"
    );
}

#[test]
fn native_owning_waits_restore_a_real_terminals_foreground_group() {
    for mode in [
        "wait",
        "try",
        "usage",
        "try-usage",
        "signal",
        "elsewhere",
        "supervise",
        "bad-terminal",
        "paused-int",
        "paused-quit",
        "shared-int",
        "cohort-prelaunch-int",
        "foreground-int",
        "job-events",
        "job-terminate",
        "live-normal",
        "live-running",
        "live-pin",
        "live-cleanup",
        "live-timer",
        "live-grace-expiry",
        "live-bad-policy",
        "live-error",
    ] {
        let world = aterm_tempfile::tempdir().unwrap();
        let (master, slave) = aterm_pty::open_pty_pair().unwrap();
        // SAFETY: each fresh PTY descriptor has exactly one owner here.
        let (mut master, slave) = unsafe {
            (
                std::fs::File::from_raw_fd(master),
                OwnedFd::from_raw_fd(slave),
            )
        };
        // The isolated terminal needs its output consumed just like a real
        // terminal emulator. Otherwise session teardown may wait on unread echo.
        let mut terminal_output = master.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let _ = std::io::copy(&mut terminal_output, &mut std::io::sink());
        });
        // The "signal" mode's actor and its cat keep the SIGINT they inherit: ^C must end them.
        let mut actor = OwnedChild(crate::platform::spawn_outside_a_stand_aside(|| {
            spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "build_admit::tests::terminal::native_terminal_actor_if_requested",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(WORLD, world.path())
                .env(MODE, mode)
                .setsid(true)
                .stdin(Stdio::from(slave))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        }));
        let mut last = String::new();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "actor terminal handed over",
            || {
                // Several modes write ready and end at once, so the exit is
                // read first: in every mode ready exists before the actor
                // ends (the actor writes it, or a worker does while the actor
                // waits on this test), so an exit seen before the look means
                // the look finds ready. Only an exit with no ready is an end
                // before the handoff.
                let exited = actor.try_wait().unwrap();
                if world.path().join("ready").exists() {
                    return Some(());
                }
                let step = std::fs::read_to_string(world.path().join("trace")).unwrap_or_default();
                if step != last {
                    eprintln!("terminal {mode}: {step}");
                    last = step;
                }
                if let Some(status) = exited {
                    panic!(
                        "terminal actor ended before handoff ({mode}): {status}: {}",
                        diagnostics(&mut actor)
                    );
                }
                None
            },
        );
        if mode.starts_with("live-") {
            if mode == "live-normal" {
                master.write_all(b"ready\n").unwrap();
            } else {
                std::fs::write(world.path().join("cancel"), b"user intent").unwrap();
            }
        } else if mode == "job-events" || mode == "job-terminate" {
            if mode == "job-events" {
                master.write_all(&[26]).unwrap();
            }
            let expected: u64 = std::fs::read_to_string(world.path().join("budget"))
                .unwrap()
                .parse()
                .unwrap();
            let ledger = Ledger::at(PathBuf::from(
                std::fs::read_to_string(world.path().join("fence")).unwrap(),
            ));
            let steps = if mode == "job-terminate" { 1 } else { 2 };
            for step in 1..=steps {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "actual job stopped with budget retained",
                    || {
                        if let Some(status) = actor.try_wait().unwrap() {
                            panic!("event actor ended: {status}: {}", diagnostics(&mut actor));
                        }
                        world
                            .path()
                            .join(format!("paused-{step}"))
                            .exists()
                            .then_some(())
                    },
                );
                let rows = ledger.list().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].need.mem, expected);
                assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
                // No SIGCONT is sent to the resource-owner actor. It continues
                // servicing explicit commands while its actual job is stopped.
                std::fs::write(
                    world.path().join(format!("resume-{step}")),
                    b"owner decision",
                )
                .unwrap();
                if mode == "job-events" {
                    aterm_spec::wait::until_within(
                        aterm_spec::wait::MUST_HAPPEN,
                        "resume accepted",
                        || {
                            world
                                .path()
                                .join(format!("resumed-{step}"))
                                .exists()
                                .then_some(())
                        },
                    );
                }
            }
            if mode == "job-events" {
                master.write_all(b"ready\n").unwrap();
            }
        } else if mode == "signal" {
            master.write_all(&[3]).unwrap();
        } else if mode != "elsewhere"
            && mode != "bad-terminal"
            && !mode.starts_with("paused-")
            && mode != "shared-int"
            && mode != "cohort-prelaunch-int"
            && mode != "foreground-int"
        {
            master.write_all(b"ready\n").unwrap();
        }
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "terminal restored and actor ended",
            || actor.try_wait().unwrap(),
        );
        let output = diagnostics(&mut actor);
        // How it ended and its last step: a signal's end loses the actor's buffered stdout.
        let step = std::fs::read_to_string(world.path().join("trace")).unwrap_or_default();
        assert!(
            status.success(),
            "{mode}: {status} after {step:?}: {output}"
        );
        drop(master);
        reader.join().unwrap();
    }
}
