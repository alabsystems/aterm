// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::catch_stops`]'s live arm: catches SIGINT, SIGTERM, SIGHUP and SIGQUIT, and
//! leaves one already ignored (`nohup`, `trap '' HUP`) ignored.

use std::ffi::c_int;
use std::sync::atomic::{AtomicI32, Ordering};

const STOPS: [c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// The handler: keep the first signal, nothing else (async-signal-safe).
extern "C" fn note(sig: c_int) {
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

struct Catching([(c_int, Option<libc::sigaction>); 4]);

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

/// Catch [`STOPS`] until the handle drops. A signal this process ignores already stays ignored:
/// whoever started it asked it to outlive that signal (a hang-up under `nohup`), so it is no stop.
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
        // SAFETY: reads `sig`'s action only.
        if unsafe { libc::sigaction(sig, std::ptr::null(), &mut was) } != 0
            || was.sa_sigaction == libc::SIG_IGN
        {
            return (sig, None);
        }
        // SAFETY: a valid disposition for `sig`; the one it replaces is `was`, read above.
        let set = unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } == 0;
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

/// `SIGINT`, `SIGTERM`, `SIGHUP` or `SIGQUIT` for one of [`STOPS`]; `signal <n>` otherwise.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        libc::SIGINT => "SIGINT".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        n => format!("signal {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Dispositions and [`CAUGHT`] are the process's: one test at a time touches them.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// A signal's disposition set to `to` until this drops, then put back.
    struct SetTo(c_int, libc::sigaction);

    impl SetTo {
        fn new(sig: c_int, to: libc::sighandler_t) -> Self {
            // SAFETY: as in `catch`.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = to;
            // SAFETY: as in `catch`.
            let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: a valid disposition for a catchable signal; `was` receives the one it
            // replaces.
            assert_eq!(unsafe { libc::sigaction(sig, &action, &mut was) }, 0);
            Self(sig, was)
        }
    }

    impl Drop for SetTo {
        fn drop(&mut self) {
            // SAFETY: puts back the whole action `new` replaced.
            unsafe { libc::sigaction(self.0, &self.1, std::ptr::null_mut()) };
        }
    }

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
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Whatever this test binary inherited (a background job's shell ignores SIGINT and
        // SIGQUIT), every stop here is one nobody ignores.
        let _default = STOPS.map(|sig| SetTo::new(sig, libc::SIG_DFL));
        let before = STOPS.map(disposition);
        let held = catch();
        // A Ctrl-C, a Ctrl-\, a kill and a closed tab are each caught. Only the hang-up is
        // raised: SIGQUIT's default dumps core, and the catch is what this reads.
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
            let now = disposition(sig);
            assert!(
                now != libc::SIG_DFL && now != libc::SIG_IGN,
                "{} is caught",
                name(sig)
            );
        }
        assert_eq!(caught(), None);
        hang_up();
        assert_eq!(caught(), Some(libc::SIGHUP));
        hang_up();
        assert_eq!(caught(), Some(libc::SIGHUP));
        drop(held);
        assert_eq!(STOPS.map(disposition), before);
        assert_eq!(caught(), None);
    }

    /// A hang-up this process ignores (`nohup`) is no stop: it stays ignored while the stops are
    /// caught, is never noted, and is still ignored after.
    #[test]
    fn an_ignored_hang_up_stays_ignored_and_is_no_stop() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ignored = SetTo::new(libc::SIGHUP, libc::SIG_IGN);
        let held = catch();
        assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
        hang_up();
        assert_eq!(caught(), None);
        drop(held);
        assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
    }

    #[test]
    fn each_stop_has_its_name() {
        assert_eq!(name(libc::SIGINT), "SIGINT");
        assert_eq!(name(libc::SIGTERM), "SIGTERM");
        assert_eq!(name(libc::SIGHUP), "SIGHUP");
        assert_eq!(name(libc::SIGQUIT), "SIGQUIT");
        assert_eq!(name(9), "signal 9");
    }
}
