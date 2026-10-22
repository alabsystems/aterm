// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SIGTERM: a person's quits the mug, the system's ends it as before.
//!
//! Activity Monitor's Quit sends the mug SIGTERM, not a quit Apple event, and so does `kill`.
//! Left to its default action the signal ends the mug, and launchd's KeepAlive starts it again
//! with the mode on: the mug a person quit came back. launchd sends SIGTERM too, from pid 1: a
//! bootout (`aterm caffeine quit`, a job removed) and the end of a logout's or a shutdown's
//! session, which keep the mode for the next login in this boot. A macOS 27.0 VM showed the two
//! apart (2026-10-22): a probe's `si_pid` read Activity Monitor's pid for its Quit and 1 for a
//! bootout.
//!
//! So while the mug runs, [`watch`] takes SIGTERM with a handler that reads its sender: the
//! system's ([`sent_by_the_system`]) gets the default action at once, from the handler; any
//! other sender's pid goes through a pipe to a thread of the watch's own, which hands it to the
//! mug (Quit Caffeine's quit, on the main thread). The handler does only what a signal handler
//! may: atomic loads and stores, `write`, `signal` and `raise`.

use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::sync::atomic::{AtomicI32, Ordering};

/// The pipe's write end the handler wakes the watch's thread through, or -1 while nothing
/// watches. Never closed once made, so a handler running at any moment writes into this pipe
/// and never into a descriptor that later took its number.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// The pid of the latest person's SIGTERM, which the watch's thread hands on.
static SENDER: AtomicI32 = AtomicI32::new(0);

/// Whether a SIGTERM from `sender` is the system's: launchd's (pid 1) or the kernel's (0).
pub(super) fn sent_by_the_system(sender: libc::pid_t) -> bool {
    sender <= 1
}

/// SIGTERM is taken while this lives ([`watch`]); dropped, it has its default action again.
#[derive(Debug)]
pub(super) struct Watching {
    _private: (),
}

impl Drop for Watching {
    fn drop(&mut self) {
        set_default();
        WAKE.store(-1, Ordering::Release);
    }
}

/// Take SIGTERM until the returned [`Watching`] drops: a person's hands its sender's pid to
/// `quit` on a thread of the watch's own; the system's ends the process as before. A `quit`
/// that panics ends it as the default action would. `None`, with SIGTERM left as it was, when
/// SIGTERM does not have its default action (a parent ignored it) or the pipe or the thread
/// cannot be made.
pub(super) fn watch(quit: fn(libc::pid_t)) -> Option<Watching> {
    // SAFETY: all-zero is valid storage for the action; a null new action only reads.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: as above; `current` is a valid out-parameter owned by this frame.
    let read = unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut current) };
    if read != 0 || current.sa_sigaction != libc::SIG_DFL {
        return None;
    }
    let (mut reader, writer) = io::pipe().ok()?;
    // The handler must never block on a full pipe, which holds a wake already.
    // SAFETY: `fcntl` reads and sets the status flags of the pipe's live write end.
    let nonblocking = unsafe {
        let flags = libc::fcntl(writer.as_raw_fd(), libc::F_GETFL);
        flags >= 0 && libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    };
    if !nonblocking {
        return None;
    }
    std::thread::Builder::new()
        .name("caffeine-sigterm".to_owned())
        .spawn(move || {
            let mut wakes = [0_u8; 64];
            loop {
                match reader.read(&mut wakes) {
                    // Every write end is gone: the watch was never made.
                    Ok(0) => return,
                    Ok(_) => {
                        let sender = SENDER.load(Ordering::Acquire);
                        if std::panic::catch_unwind(|| quit(sender)).is_err() {
                            end_by(libc::SIGTERM);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        })
        .ok()?;
    WAKE.store(writer.as_raw_fd(), Ordering::Release);
    if !install() {
        WAKE.store(-1, Ordering::Release);
        // Dropping the write end ends the thread.
        return None;
    }
    // Kept open for good (`WAKE`).
    let _ = writer.into_raw_fd();
    Some(Watching { _private: () })
}

/// The SIGTERM handler [`watch`] installs.
extern "C" fn on_term(sig: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    let sender = if info.is_null() {
        0
    } else {
        // SAFETY: the kernel hands an `SA_SIGINFO` handler a valid `siginfo_t`.
        unsafe { (*info).si_pid }
    };
    let wake = WAKE.load(Ordering::Acquire);
    if wake < 0 || sent_by_the_system(sender) {
        end_by(sig);
        return;
    }
    SENDER.store(sender, Ordering::Release);
    // SAFETY: `__error` is this thread's errno, put back after the `write`, which reads one byte
    // of this frame into the pipe's write end, kept open for good. A failed write loses nothing:
    // a full pipe holds a wake already.
    unsafe {
        let errno = libc::__error();
        let saved = *errno;
        let wake_byte = 1_u8;
        let _ = libc::write(wake, (&raw const wake_byte).cast(), 1);
        *errno = saved;
    }
}

/// Route SIGTERM to [`on_term`]; whether it took. `SA_RESTART` restarts the restartable calls
/// it interrupts (`read`, `write`, `waitpid`, ...); `poll`, `select`, `kevent` and the sleeps
/// still return `EINTR`, which every wait the mug makes retries (`aterm_uds::deadline`'s
/// `poll`, std's sleep).
fn install() -> bool {
    // SAFETY: all-zero is a valid action, filled in below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_term
        as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
        as libc::sighandler_t;
    // SAFETY: `sa_mask` is a valid out-parameter owned by this frame.
    unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
    // SAFETY: installs `on_term`, which is async-signal-safe; the old action is not asked for.
    unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) == 0 }
}

/// SIGTERM's default action back.
fn set_default() {
    // SAFETY: all-zero is a valid empty mask and no flags; `SIG_DFL` is the default action.
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: `default` is fully initialized; the old action is not asked for.
    unsafe { libc::sigaction(libc::SIGTERM, &raw const default, std::ptr::null_mut()) };
}

/// End this process as SIGTERM's default action does: at once, or, from the handler, as soon
/// as it returns (the signal is blocked until then).
pub(super) fn end_by_sigterm() {
    end_by(libc::SIGTERM);
}

/// Raise `sig` with its default action.
fn end_by(sig: libc::c_int) {
    // SAFETY: `signal` and `raise` are async-signal-safe; the default action of the one signal
    // this is called with, SIGTERM, ends the process without a core.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        #[expect(
            clippy::disallowed_methods,
            reason = "re-raises SIGTERM with its default action, which dumps no core"
        )]
        libc::raise(sig);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::OnceLock;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// The variable naming the one test a child of this test binary runs.
    const CHILD: &str = "ATERM_TEST_CAFFEINE_SIGTERM";

    /// The hang detector of every wait here.
    const HANG: Duration = Duration::from_secs(60);

    /// What the child prints just before it signals itself as launchd would.
    const ENDING: &str = "ending by the system's SIGTERM";

    /// The hand-over's pids, for the test waiting on them (`quit` is a plain `fn`).
    static HEARD: OnceLock<mpsc::Sender<libc::pid_t>> = OnceLock::new();

    fn heard(sender: libc::pid_t) {
        if let Some(tx) = HEARD.get() {
            let _ = tx.send(sender);
        }
    }

    /// The disposition SIGTERM has now.
    fn disposition() -> libc::sigaction {
        // SAFETY: all-zero is valid storage; a null new action only reads.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut action) },
            0
        );
        action
    }

    fn set_disposition(handler: libc::sighandler_t) {
        // SAFETY: all-zero is a valid empty mask and no flags.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        // SAFETY: changes only this exact-test child's disposition.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) },
            0
        );
    }

    /// Signal dispositions are process-wide, so each test here runs alone in a child of this
    /// test binary (`<test> --exact`), never beside the runner's other threads: `None` in that
    /// child, which runs the body with SIGTERM at its default; in the parent, the child's
    /// output once it ended.
    fn isolated(test: &str) -> Option<std::process::Output> {
        let test = module_path!()
            .split_once("::")
            .map_or_else(|| test.to_owned(), |(_, path)| format!("{path}::{test}"));
        if std::env::var(CHILD).as_deref() == Ok(test.as_str()) {
            set_disposition(libc::SIG_DFL);
            return None;
        }
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test exe"));
        cmd.args([test.as_str(), "--exact", "--nocapture"])
            .env(CHILD, &test);
        Some(aterm_uds::deadline::output_within(&mut cmd, HANG).expect("the child ends"))
    }

    /// The child ran its one test to a pass.
    fn passed(out: &std::process::Output) {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A filter naming no test exits 0 too, having run nothing.
        assert!(
            stdout
                .lines()
                .any(|line| line.starts_with("test result: ok. 1 passed;")),
            "the child ran no test:\n{stdout}"
        );
    }

    #[test]
    fn only_launchd_and_the_kernel_are_the_system() {
        assert!(sent_by_the_system(0));
        assert!(sent_by_the_system(1));
        assert!(!sent_by_the_system(2));
        assert!(!sent_by_the_system(4242));
    }

    /// A SIGTERM from a pid other than launchd's reaches `quit` with that pid, and the process
    /// lives on; the watch dropped, SIGTERM has its default action again.
    #[test]
    fn a_persons_sigterm_is_handed_over_with_its_pid() {
        let Some(out) = isolated("a_persons_sigterm_is_handed_over_with_its_pid") else {
            let (tx, rx) = mpsc::channel();
            HEARD.set(tx).expect("one test sets it");
            let watching = watch(heard).expect("SIGTERM is watched");
            assert_ne!(disposition().sa_sigaction, libc::SIG_DFL);
            let me = libc::pid_t::try_from(std::process::id()).expect("a pid");
            // SAFETY: signals this process, whose SIGTERM the watch now takes.
            assert_eq!(unsafe { libc::kill(me, libc::SIGTERM) }, 0);
            assert_eq!(rx.recv_timeout(HANG), Ok(me));
            drop(watching);
            assert_eq!(disposition().sa_sigaction, libc::SIG_DFL);
            return;
        };
        passed(&out);
    }

    /// A SIGTERM from launchd ends the process as the default action does, and reaches no
    /// `quit`.
    #[test]
    fn the_systems_sigterm_ends_the_process_as_its_default_does() {
        let Some(out) = isolated("the_systems_sigterm_ends_the_process_as_its_default_does") else {
            let (tx, rx) = mpsc::channel();
            HEARD.set(tx).expect("one test sets it");
            let _watching = watch(heard).expect("SIGTERM is watched");
            // SAFETY: all-zero is a valid `siginfo_t`; the sender is launchd.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            info.si_signo = libc::SIGTERM;
            info.si_pid = 1;
            println!("{ENDING}");
            on_term(libc::SIGTERM, &raw mut info, std::ptr::null_mut());
            panic!("still running; quit heard {:?}", rx.recv_timeout(HANG));
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(ENDING),
            "the child never signalled:\n{stdout}"
        );
        assert_eq!(
            out.status.signal(),
            Some(libc::SIGTERM),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A SIGTERM a parent left ignored stays ignored: the mug does not take it.
    #[test]
    fn an_ignored_sigterm_is_left_alone() {
        let Some(out) = isolated("an_ignored_sigterm_is_left_alone") else {
            set_disposition(libc::SIG_IGN);
            assert!(watch(heard).is_none());
            assert_eq!(disposition().sa_sigaction, libc::SIG_IGN);
            return;
        };
        passed(&out);
    }
}
