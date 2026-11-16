// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bell a background thread parks on until a deadline or a ring, for the waits an idle Mac
//! should end only on what they wait for (ATERM-KI-1108-2): the services watch's rest
//! (`services_watch`) and the window presence's beat (`window_presence`).
//!
//! A ring tells the bell's state `T` and wakes its waiter; a wait takes what it hears from that
//! state, a ring made before the wait included, or ends at its deadline. A condition variable,
//! not a descriptor, so nothing here can fail to be made and no waiter keeps a timed fallback.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A condition variable over the state its rings tell.
#[derive(Debug)]
pub(crate) struct ParkBell<T> {
    told: Mutex<T>,
    cv: Condvar,
    /// How many times a wait parked on the condition variable: a test rings only once its waiter
    /// is parked, so the ring can reach it only through the condition variable.
    #[cfg(test)]
    parks: std::sync::atomic::AtomicUsize,
}

impl<T> ParkBell<T> {
    /// A bell whose state is `told`.
    pub(crate) const fn new(told: T) -> Self {
        Self {
            told: Mutex::new(told),
            cv: Condvar::new(),
            #[cfg(test)]
            parks: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Tell the state (`tell`, whose answer is returned) and wake the waiter.
    pub(crate) fn ring<R>(&self, tell: impl FnOnce(&mut T) -> R) -> R {
        let said = tell(&mut self.told.lock().unwrap_or_else(PoisonError::into_inner));
        self.cv.notify_all();
        said
    }

    /// Wait at most `within` for `heard` to take something from the state, which it reads first
    /// (so a ring before the wait is heard) and again at each wake: what it took, or `None` once
    /// `within` passed with nothing.
    pub(crate) fn wait<R>(
        &self,
        within: Duration,
        mut heard: impl FnMut(&mut T) -> Option<R>,
    ) -> Option<R> {
        let deadline = Instant::now().checked_add(within);
        let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(said) = heard(&mut told) {
                return Some(said);
            }
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if left.is_some_and(|left| left.is_zero()) {
                return None;
            }
            // Counted under the lock the wait releases as it parks: a ring that follows the
            // count reaches the waiter only through the condition variable.
            #[cfg(test)]
            self.parks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            told = match left {
                Some(left) => {
                    self.cv
                        .wait_timeout(told, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.cv.wait(told).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// How many times a wait parked ([`Self::wait`]).
    #[cfg(test)]
    pub(crate) fn parks(&self) -> usize {
        self.parks.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
#[path = "park_bell_tests.rs"]
mod tests;
