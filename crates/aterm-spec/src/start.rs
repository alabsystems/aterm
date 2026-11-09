// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE SIGNAL STATE A TEST'S CHILD STARTS FROM: the one the test names, never the runner's.
//!
//! A test binary run as a `&` job of a non-interactive shell starts with SIGINT and SIGQUIT
//! ignored (POSIX gives an asynchronous list both), one under `nohup` with SIGHUP ignored, and
//! so does every child it spawns, since an exec keeps an ignored signal ignored. A child that is
//! sent one then never ends by it: a shell cannot even trap one it was started ignoring, and a
//! program that catches only what it was not started ignoring (a lab verb's stop, a release's
//! interrupt, a window's exit handlers) keeps it ignored. A bystander that must die of a stray
//! signal to show one was sent shows nothing. So a test spawns such a child from
//! [`at_defaults`]: the signals it names are at their default actions in this process while the
//! guard lives, and the child inherits them.
//!
//! One guard lives at a time in a process ([`TURN`]): two that overlapped would each put back
//! what the other set. Its drop puts back each action it found, a test that failed an assertion
//! included, except where something else set one meanwhile (a handler a test's subject installs
//! from another thread), which then stays. A child spawned beside it from another thread may
//! start with those signals at their defaults too. A stand-in process of a test, which runs
//! alone, sets its own at its start instead.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held by the one live [`AtDefaults`].
static TURN: Mutex<()> = Mutex::new(());

/// The signals that end a process by default and that a runner may have ignored: SIGHUP,
/// SIGINT, SIGQUIT and SIGTERM.
pub const TERMINATING: [libc::c_int; 4] =
    [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

/// Signals at their default actions in this process until this drops ([`at_defaults`]).
#[must_use = "the signals are put back as found when it drops: spawn the child while it lives"]
pub struct AtDefaults {
    /// Each signal set, and the whole action it had.
    found: Vec<(libc::c_int, libc::sigaction)>,
    _turn: MutexGuard<'static, ()>,
}

/// `signals` at their default actions in this process, once no other [`AtDefaults`] lives, until
/// the guard drops (module docs). SIGKILL and SIGSTOP have no disposition to set.
///
/// # Panics
/// Where `sigaction` refuses to set one.
pub fn at_defaults(signals: &[libc::c_int]) -> AtDefaults {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let default = action(libc::SIG_DFL);
    let mut found = Vec::with_capacity(signals.len());
    for &sig in signals {
        if sig == libc::SIGKILL || sig == libc::SIGSTOP {
            continue;
        }
        let mut was = action(libc::SIG_DFL);
        // SAFETY: the default action for `sig`; `was` receives the whole action it replaces.
        let set = unsafe { libc::sigaction(sig, &raw const default, &raw mut was) };
        assert_eq!(set, 0, "signal {sig} set to its default action");
        found.push((sig, was));
    }
    AtDefaults { found, _turn: turn }
}

/// An action with `handler`, an empty mask and no flags.
fn action(handler: libc::sighandler_t) -> libc::sigaction {
    // SAFETY: all-zero is a valid `sigaction` (an empty mask, no flags).
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    action
}

impl Drop for AtDefaults {
    fn drop(&mut self) {
        for (sig, was) in self.found.drain(..).rev() {
            let mut now = action(libc::SIG_DFL);
            // SAFETY: a null new action only reads `sig`'s current one into `now`.
            let read = unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut now) };
            if read == 0 && now.sa_sigaction == libc::SIG_DFL {
                // SAFETY: puts back the whole action `at_defaults` read for `sig` as it set it.
                unsafe { libc::sigaction(sig, &raw const was, std::ptr::null_mut()) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;

    use super::{TERMINATING, action, at_defaults};
    use crate::wait::{MUST_HAPPEN, until_within};

    /// Set, this process is the one [`a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores`]
    /// started to run its body alone.
    const ALONE: &str = "ATERM_SPEC_START_ALONE";

    /// A child, SIGKILLed when this drops still running (a test that failed before it ended).
    struct Killed(aterm_uds::spawnfd::Child);

    impl Drop for Killed {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// `sig`'s handler now.
    fn handler(sig: libc::c_int) -> libc::sighandler_t {
        let mut now = action(libc::SIG_DFL);
        // SAFETY: a null new action only reads the current one into `now`, which outlives it.
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut now) },
            0
        );
        now.sa_sigaction
    }

    /// A CHILD SPAWNED AT THE DEFAULTS ENDS BY A SIGNAL THIS PROCESS IGNORES, and the process
    /// ignores it again once the guard drops. It runs alone in a process of its own, which first
    /// ignores the four [`TERMINATING`] signals as a `nohup` `&` runner would, so no other test
    /// of this binary sees them move. NEGATIVE CONTROL (by hand: the sleep spawned before the
    /// guard): the sleep ignores its SIGTERM and the end's hang detector fails.
    #[test]
    fn a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores() {
        if std::env::var_os(ALONE).is_none() {
            let mut alone =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            alone
                .args([
                    "--exact",
                    "start::tests::a_child_spawned_at_the_defaults_ends_by_a_signal_this_process_ignores",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(ALONE, "1");
            // Twice the hang detector: the body's own detector says first what never came.
            let out = aterm_uds::deadline::output_within(&mut alone, MUST_HAPPEN * 2)
                .expect("hang detector: the body's own run never ended");
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && said.contains("test result: ok. 1 passed"),
                "{}\n{said}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        for sig in TERMINATING {
            // SAFETY: SIG_IGN is a disposition, no handler of ours; this process runs this test
            // alone.
            unsafe { libc::signal(sig, libc::SIG_IGN) };
        }
        let start = at_defaults(&TERMINATING);
        let mut sleep = Killed(
            aterm_uds::spawnfd::command("/bin/sleep")
                .arg("600")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("a sleep"),
        );
        drop(start);
        for sig in TERMINATING {
            assert_eq!(handler(sig), libc::SIG_IGN, "signal {sig} ignored again");
        }
        let pid = libc::pid_t::try_from(sleep.0.id()).expect("a pid");
        // SAFETY: a signal to this test's own unreaped child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let status = until_within(MUST_HAPPEN, "the sleep's end by its SIGTERM", || {
            sleep.0.try_wait().ok().flatten()
        });
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status}");
    }
}
