// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISPOSITIONS A TEST STARTS FROM (a test seam, `cfg(any(test, debug_assertions))`, for this
//! crate's tests and its integration tests): a test whose subject reads a signal's disposition (a
//! forwarding or an admission's watch that keeps an ignored one, a front that passes on only what
//! it catches, a build that inherits its front's) sets the one it starts from, never the runner's.
//! A test binary run as a `&` job of a non-interactive shell starts with SIGINT and SIGQUIT
//! ignored (POSIX gives an asynchronous list both), one under `nohup` with SIGHUP ignored, and so
//! does every child it spawns, since an exec keeps an ignored signal ignored.
//!
//! [`Starting`] sets them under [`super::FORWARDING_TURN`], so no forwarding or front of this
//! binary is armed while they change, and puts back what it found when it drops, under that turn
//! again, a test that failed an assertion included. One test at a time holds one ([`STARTING`]):
//! two that overlapped would each put back what the other had set.
//!
//! An admission's interrupt watch ([`aterm_toolchain::interrupt::Watch`]) sets SIGINT, SIGTERM,
//! SIGHUP and SIGQUIT aside outside that turn, and one that began before a [`Starting`] and ended
//! inside it would leave its own handler for good: the watch finds the default the start set, not
//! its handler, so it puts nothing back, and the start then puts back the handler it found. So a
//! [`Starting`] holds a watch of its own, begun before it sets anything and ended after it puts
//! everything back, both in the turn: any other watch of this binary is then nested inside it and
//! sets nothing aside, and the outermost one's end puts back what it set aside. While it lives, an
//! interrupt the binary does not ignore and the start did not set is noted, not acted on, and sent
//! again when it ends.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held by the one live [`Starting`].
static STARTING: Mutex<()> = Mutex::new(());

/// Signals set to their default actions until this drops, then put back as they were found.
#[must_use = "the signals are put back as found when it drops"]
pub struct Starting {
    /// Each signal set, and the whole action it had.
    found: Vec<(libc::c_int, libc::sigaction)>,
    /// This start's own interrupt watch (module docs), ended in [`Drop`] once `found` is put back.
    watch: Option<aterm_toolchain::interrupt::Watch>,
    _own: MutexGuard<'static, ()>,
}

impl Starting {
    /// `signals` at their default actions, once no other test's [`Starting`] is live. SIGKILL and
    /// SIGSTOP have no disposition to set; they are at their defaults already.
    ///
    /// # Panics
    /// Where `sigaction` refuses to set one.
    pub fn at_defaults(signals: &[libc::c_int]) -> Self {
        let own = STARTING.lock().unwrap_or_else(PoisonError::into_inner);
        let _turn = super::FORWARDING_TURN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let watch = aterm_toolchain::interrupt::Watch::enter();
        // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask,
        // no flags); this one is the default action.
        let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
        default.sa_sigaction = libc::SIG_DFL;
        let mut found = Vec::with_capacity(signals.len());
        for &sig in signals {
            if sig == libc::SIGKILL || sig == libc::SIGSTOP {
                continue;
            }
            // SAFETY: as above.
            let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: the default action for `sig`; `was` receives the whole action it replaces.
            let set = unsafe { libc::sigaction(sig, &raw const default, &raw mut was) };
            assert_eq!(set, 0, "signal {sig} set to its default action");
            found.push((sig, was));
        }
        Self {
            found,
            watch: Some(watch),
            _own: own,
        }
    }

    /// `spawn` run in this process's forwarding turn ([`super::spawn_outside_a_stand_aside`]): no
    /// wait of this binary stands aside (SIGINT ignored) while a child inherits these
    /// dispositions.
    pub fn spawning<T>(&self, spawn: impl FnOnce() -> T) -> T {
        super::spawn_outside_a_stand_aside(spawn)
    }
}

impl Drop for Starting {
    fn drop(&mut self) {
        let _turn = super::FORWARDING_TURN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (sig, was) in self.found.drain(..).rev() {
            // SAFETY: puts back the whole action `at_defaults` read for `sig` as it set it.
            unsafe { libc::sigaction(sig, &raw const was, std::ptr::null_mut()) };
        }
        drop(self.watch.take());
    }
}

#[cfg(test)]
#[path = "unix_starting_tests.rs"]
mod tests;
