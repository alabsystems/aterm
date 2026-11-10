// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A latched stop a background thread sleeps on between its rounds, or polls beside its
//! descriptors.
//!
//! A thread that sleeps in short steps to read a stop flag between them wakes for nothing many
//! times a second at rest (`aterm node serve`'s loops once read theirs every 25 ms, 40 wakeups a
//! second each, and the fleet listener's accept loop every 200 ms). A [`Stop`] is set once and
//! stays set; [`Stop::sleep`] blocks in one condition-variable wait until the stop is set or its
//! time is up, and [`Stop::set`] wakes every thread asleep on it at once. A thread that waits in
//! `poll(2)` instead (an accept loop, for its listener) adds the stop's [`Stop::bell`] to what it
//! polls, which [`Stop::set`] hangs up.
//!
//! [`Stop::set`] takes a mutex, so it is not async-signal-safe: a signal reaches a stop through a
//! thread that hears it (the self-pipe a signal handler writes), never from the handler itself.

#[cfg(unix)]
use std::os::fd::{AsFd as _, BorrowedFd};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest single sleep: a longer `total` (`Duration::MAX` included) sleeps this long, as
/// good as forever to a background thread, without overflowing a deadline.
const LONGEST: Duration = Duration::from_secs(86_400 * 365);

/// What the stop's lock guards.
#[derive(Debug, Default)]
struct Latch {
    set: bool,
    /// The write end of the bell's pipe while the stop is unset ([`Stop::bell`]): [`Stop::set`]
    /// closes it, which hangs the pipe up for every poll of its read end.
    #[cfg(unix)]
    ringer: Option<std::io::PipeWriter>,
}

/// A stop that, once set, stays set, and wakes every thread asleep on it ([`Stop::sleep`]) or
/// polling its bell ([`Stop::bell`]).
#[derive(Debug, Default)]
pub struct Stop {
    latch: Mutex<Latch>,
    rung: Condvar,
    /// The read end of the bell's pipe, made by the first [`Stop::bell`].
    #[cfg(unix)]
    bell: std::sync::OnceLock<std::io::PipeReader>,
    /// How many sleeps took the lock to wait, for a test to set the stop only once they wait.
    #[cfg(test)]
    entered: std::sync::atomic::AtomicUsize,
    /// How many waits returned, including notifications that leave the stop unset.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    wakes: std::sync::atomic::AtomicUsize,
    /// How many waits expired, to distinguish timer polling from predicate rechecks.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    expired: std::sync::atomic::AtomicUsize,
}

impl Stop {
    /// A stop not yet set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the stop is set.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.latch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set
    }

    /// Set the stop, wake every thread asleep on it, and ring its bell. `true` for the call that
    /// set it; setting it again changes nothing and answers `false`, so of two setters (a stop
    /// asked for and a loop that stopped by itself) exactly one learns it was first.
    pub fn set(&self) -> bool {
        let mut latch = self.latch.lock().unwrap_or_else(PoisonError::into_inner);
        let first = !latch.set;
        latch.set = true;
        #[cfg(unix)]
        drop(latch.ringer.take());
        drop(latch);
        self.rung.notify_all();
        first
    }

    /// Sleep until the stop is set or `total` has passed, whichever comes first, in one wait:
    /// `true` when the stop is set (at once, if it already was), `false` when the time ran out.
    pub fn sleep(&self, total: Duration) -> bool {
        let total = total.min(LONGEST);
        let deadline = Instant::now() + total;
        let mut latch = self.latch.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        while !latch.set {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (next, _timeout) = self
                .rung
                .wait_timeout(latch, left)
                .unwrap_or_else(PoisonError::into_inner);
            latch = next;
            #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
            {
                self.wakes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if _timeout.timed_out() {
                    self.expired
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        true
    }

    /// A descriptor that polls ready (hung up) once the stop is set and never before, for a
    /// thread that waits in `poll(2)` on its own descriptors rather than in [`Stop::sleep`]:
    /// `ready::either_until(listener, stop.bell()?, deadline)`. The first call makes a pipe (two
    /// descriptors, kept out of children); [`Stop::set`] closes its write end, before or after
    /// that first call, and the hangup stays raised for every later poll. Nothing is ever
    /// written to it: poll it, never read it.
    ///
    /// # Errors
    /// The pipe could not be made (this process is out of descriptors).
    #[cfg(unix)]
    pub fn bell(&self) -> std::io::Result<BorrowedFd<'_>> {
        if let Some(reader) = self.bell.get() {
            return Ok(reader.as_fd());
        }
        // Made before the latch is taken: on Apple `kept_out` waits for every spawn in flight,
        // and a `set` or `is_set` on another thread must not wait with it.
        let (reader, ringer) = crate::kept_out(std::io::pipe)?;
        let mut latch = self.latch.lock().unwrap_or_else(PoisonError::into_inner);
        let reader = match self.bell.get() {
            // Another call's pipe became the bell first; this one closes as it drops.
            Some(bell) => bell,
            None => {
                if !latch.set {
                    latch.ringer = Some(ringer);
                }
                // Only a call holding the latch installs the bell, so this one's pipe is the bell.
                self.bell.get_or_init(|| reader)
            }
        };
        Ok(reader.as_fd())
    }

    /// How many sleeps have taken the lock: each waits from then until [`Stop::set`] can take it.
    #[cfg(test)]
    fn entered(&self) -> usize {
        self.entered.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many waits on this stop returned ([`Stop::sleep`]).
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    fn wakes(&self) -> usize {
        self.wakes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many waits on this stop reached their timeout, rather than being notified.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    fn expired(&self) -> usize {
        self.expired.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
