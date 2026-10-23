// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::catch_stops`]'s live arm: SIGINT, SIGTERM and SIGHUP noted instead of ending
//! the process, so `screen` can close its relay and lock a session the admin logged in to over it
//! before it exits (design §6).

use std::ffi::c_int;
use std::sync::atomic::{AtomicI32, Ordering};

/// The signals a person or the system stops a command with.
const STOPS: [c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The first of [`STOPS`] caught while a [`Catching`] lives; 0 for none.
static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// The handler: keep the first signal, nothing else (async-signal-safe).
extern "C" fn note(sig: c_int) {
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

/// The dispositions [`catch`] replaced, put back whole when this drops.
struct Catching([(c_int, Option<libc::sigaction>); 3]);

impl Drop for Catching {
    fn drop(&mut self) {
        for (sig, was) in &self.0 {
            if let Some(was) = was {
                // SAFETY: puts back the whole action `catch` replaced for `sig`.
                unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
            }
        }
        CAUGHT.store(0, Ordering::SeqCst);
    }
}

/// Catch [`STOPS`] until the handle drops.
pub(super) fn catch() -> Box<dyn std::any::Any> {
    CAUGHT.store(0, Ordering::SeqCst);
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask, no
    // flags).
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = note as extern "C" fn(c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    Box::new(Catching(STOPS.map(|sig| {
        // SAFETY: as above.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a valid disposition for `sig`; `was` receives the one it replaces.
        let set = unsafe { libc::sigaction(sig, &action, &mut was) } == 0;
        (sig, set.then_some(was))
    })))
}

/// Set SIGHUP and SIGPIPE to `SIG_IGN` for good ([`super::System::ignore_hangups`]).
pub(super) fn ignore_hangups() {
    for sig in [libc::SIGHUP, libc::SIGPIPE] {
        // SAFETY: as in `catch`.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_IGN;
        // SAFETY: a valid disposition for `sig`; the one it replaces is not wanted back.
        unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) };
    }
}

/// The first signal [`catch`] noted, while its handle lives.
pub(super) fn caught() -> Option<i32> {
    match CAUGHT.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}

/// `SIGINT`, `SIGTERM` or `SIGHUP` for one of [`STOPS`]; `signal <n>` otherwise.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        libc::SIGINT => "SIGINT".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGHUP => "SIGHUP".into(),
        n => format!("signal {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disposition of `sig` now.
    fn disposition(sig: c_int) -> libc::sighandler_t {
        // SAFETY: as in `catch`.
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: reads the action only.
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), &mut now) },
            0
        );
        now.sa_sigaction
    }

    /// Send this thread SIGHUP: nothing else in the test binary sends or catches it.
    fn hang_up() {
        #[expect(
            clippy::disallowed_methods,
            reason = "a test sends the hang-up `catch` notes, never a core-dump signal"
        )]
        // SAFETY: `raise` takes an integer and touches no memory of ours.
        let raised = unsafe { libc::raise(libc::SIGHUP) };
        assert_eq!(raised, 0);
    }

    #[test]
    fn a_stop_is_noted_while_caught_and_the_old_dispositions_come_back_after() {
        let before = STOPS.map(disposition);
        let held = catch();
        assert_eq!(caught(), None);
        hang_up();
        assert_eq!(caught(), Some(libc::SIGHUP));
        hang_up();
        assert_eq!(caught(), Some(libc::SIGHUP));
        drop(held);
        assert_eq!(STOPS.map(disposition), before);
        assert_eq!(caught(), None);
    }

    #[test]
    fn each_stop_has_its_name() {
        assert_eq!(name(libc::SIGINT), "SIGINT");
        assert_eq!(name(libc::SIGTERM), "SIGTERM");
        assert_eq!(name(libc::SIGHUP), "SIGHUP");
        assert_eq!(name(9), "signal 9");
    }
}
