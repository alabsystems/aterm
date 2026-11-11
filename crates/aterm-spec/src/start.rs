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
#[path = "start_tests.rs"]
mod tests;
