// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A latched stop a background thread sleeps on between its rounds.
//!
//! A thread that sleeps in short steps to read a stop flag between them wakes for nothing many
//! times a second at rest (`aterm node serve`'s loops once read theirs every 25 ms, 40 wakeups a
//! second each). A [`Stop`] is set once and stays
//! set; [`Stop::sleep`] blocks in one condition-variable wait until the stop is set or its time is
//! up, and [`Stop::set`] wakes every thread asleep on it at once.
//!
//! [`Stop::set`] takes a mutex, so it is not async-signal-safe: a signal reaches a stop through a
//! thread that hears it (the self-pipe a signal handler writes), never from the handler itself.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest single sleep: a longer `total` (`Duration::MAX` included) sleeps this long, as
/// good as forever to a background thread, without overflowing a deadline.
const LONGEST: Duration = Duration::from_secs(86_400 * 365);

/// A stop that, once set, stays set, and wakes every thread asleep on it ([`Stop::sleep`]).
#[derive(Debug, Default)]
pub struct Stop {
    set: Mutex<bool>,
    rung: Condvar,
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
        *self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Set the stop, and wake every thread asleep on it. Setting it again changes nothing.
    pub fn set(&self) {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.rung.notify_all();
    }

    /// Sleep until the stop is set or `total` has passed, whichever comes first, in one wait:
    /// `true` when the stop is set (at once, if it already was), `false` when the time ran out.
    pub fn sleep(&self, total: Duration) -> bool {
        let total = total.min(LONGEST);
        let deadline = Instant::now() + total;
        let mut set = self.set.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        while !*set {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (next, _timeout) = self
                .rung
                .wait_timeout(set, left)
                .unwrap_or_else(PoisonError::into_inner);
            set = next;
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
