// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How many durable appends this process's table stores have made, and the bell a wait for them
//! sleeps on ([`Growth`]): the Commons owner's shared-files thread waits on it between passes
//! (`crate::alab::upkeep`).

use std::{
    sync::{
        Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

/// This process's growth: every table store's durable append counts here (`crate::table::store`).
pub(crate) static GROWTH: Growth = Growth::new();

/// A growth count and the bell its waits sleep on ([`Self::wait`]). An append ([`Self::grew`])
/// counts with no lock and rings only a wait that asked for growth, so appends no wait wants (a
/// burst inside the materializer's settle, any while it rests) take no lock and wake nothing, and
/// one made under a table store's locks makes no descriptor.
pub(crate) struct Growth {
    count: AtomicU64,
    /// A wait asked to be rung on the next growth: set by the wait before it reads the count,
    /// taken by the [`Self::grew`] that rings it.
    armed: AtomicBool,
    /// The leaf lock a wait reads what it waits for under and a ring takes to notify: held only
    /// to read or ring, never across another lock or a wait of anyone's but [`Self::rung`]'s.
    lock: Mutex<()>,
    rung: Condvar,
}

impl Growth {
    pub(crate) const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            lock: Mutex::new(()),
            rung: Condvar::new(),
        }
    }

    /// Count one append. It rings only where a wait asked for growth: the count is added before
    /// the ask is taken, and a wait asks before it reads the count (all four sequentially
    /// consistent), so either the wait reads this growth or this call takes its ask and rings,
    /// through the lock the wait holds until it sleeps.
    pub(crate) fn grew(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
        if self.armed.swap(false, Ordering::SeqCst) {
            self.ring();
        }
    }

    /// How many appends were counted.
    pub(crate) fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    /// Wake every wait though no store grew, so each reads again what else it waits for (its
    /// stop, the owner's `elsewhere`). What it reads must change before this call, which takes the
    /// lock the waiter reads under.
    pub(crate) fn ring(&self) {
        let _held = self.held();
        self.rung.notify_all();
    }

    /// Wait until the count leaves `seen` (`None`: no growth wakes this wait), `woke` holds, or
    /// `until` comes (`None`: no deadline), and return the count read last. Both are read with the
    /// lock held, so a growth or a ring after the read wakes this wait; `woke` takes no lock.
    pub(crate) fn wait(
        &self,
        until: Option<Instant>,
        seen: Option<u64>,
        woke: &dyn Fn() -> bool,
    ) -> u64 {
        let mut held = self.held();
        loop {
            if seen.is_some() {
                self.armed.store(true, Ordering::SeqCst);
            }
            let count = self.count();
            if seen.is_some_and(|seen| count != seen) || woke() {
                return count;
            }
            held = match until {
                None => self.rung.wait(held).unwrap_or_else(PoisonError::into_inner),
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return count;
                    }
                    self.rung
                        .wait_timeout(held, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    fn held(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "growth_tests.rs"]
mod tests;
