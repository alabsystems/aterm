// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sliced wait loop ([`park`], [`Lane::slice`]) beside [`crate::hangup`] that checks
//! [`Lane::hung_up`] within [`crate::hangup::HANGUP_POLL`] for blocking control verbs, and the
//! condvar a guarded park waits on ([`Changed`]).

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, WaitTimeoutResult};
use std::time::{Duration, Instant};

/// The shortest park.
const MIN_PARK: Duration = Duration::from_millis(1);

/// How a park asks after the caller of the connection its thread serves.
#[derive(Clone, Copy)]
pub struct Lane {
    /// The longest one wait runs before the caller is asked ([`crate::hangup::hangup_park`]).
    pub slice: fn(Duration) -> Duration,
    /// Whether the caller hung up ([`crate::hangup::caller_hung_up`]).
    pub hung_up: fn() -> bool,
}

impl Lane {
    /// The connection this thread serves, if it serves one.
    pub const SERVED: Lane = Lane {
        slice: crate::hangup::hangup_park,
        hung_up: crate::hangup::caller_hung_up,
    };

    /// This lane for a park that finishes whether or not its caller is still there.
    #[must_use]
    #[inline]
    pub const fn ignoring_hangup(self) -> Lane {
        Lane {
            slice: self.slice,
            hung_up: never,
        }
    }
}

#[inline]
const fn never() -> bool {
    false
}

/// How a [`park`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Woke {
    /// The wait was woken (a spurious wake counts), or the watched flag moved between slices.
    Rang,
    /// The instant the verb named came.
    Due,
    /// The caller hung up, asked between slices.
    HungUp,
}

/// Park on `wait` until it wakes or `until` comes, or, asked between slices, [`Lane::hung_up`]
/// or `moved` answers `true`.
pub fn park(
    mut wait: impl FnMut(Duration) -> bool,
    until: Instant,
    lane: Lane,
    moved: &dyn Fn() -> bool,
) -> Woke {
    park_holding((), |(), bound| ((), wait(bound)), until, lane, moved).1
}

/// The condvar that signals a change to the state a `Mutex<T>` guards, and the count of those
/// changes. Nothing wakes its waiters but a ring ([`Self::ring`], [`Self::ring_after`]), which
/// takes a guard of that state and counts the change before it releases the lock. A guarded park
/// ([`Self::park`]) reads the count under the lock as it parks and again under the lock after
/// every slice, so a ring whose wake found nobody waiting (the parked thread was between slices:
/// its slice had timed out while the ringer held the lock, or the wake came before the next
/// slice began) still ends the park.
pub struct Changed<T> {
    signal: Condvar,
    /// The changes the rings counted (a [`Self::ring_after`] counts two): moved and read only
    /// under the lock of the state, which orders every read after the moves before it.
    rung: AtomicU64,
    state: PhantomData<fn() -> T>,
}

impl<T> Default for Changed<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Changed<T> {
    /// A condvar no ring has moved.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            signal: Condvar::new(),
            rung: AtomicU64::new(0),
            state: PhantomData,
        }
    }

    /// Count a change to the state `held` guards, then release the lock and wake every waiter.
    pub fn ring(&self, held: MutexGuard<'_, T>) {
        self.rung.fetch_add(1, Ordering::Relaxed);
        drop(held);
        self.wake();
    }

    /// [`Self::ring`] around `unlocked`, work that cannot run under the lock: count the change
    /// `held` guards and release it, run `unlocked`, then take `lock`, the mutex `held` guarded,
    /// again and count once more before the wake. A park the first count ends (at its next slice
    /// boundary) finds the change made under the lock, but perhaps not the work. The second
    /// count, made once the work is done, ends every park that began before it, so a waiter that
    /// read the work undone and parked again is not left parked after it, even if the wake finds
    /// it between slices. `lock` is taken again under every lock the caller still holds, so the
    /// caller must hold none it took after `lock`.
    pub fn ring_after<'a>(
        &self,
        lock: &'a Mutex<T>,
        held: MutexGuard<'a, T>,
        unlocked: impl FnOnce(),
    ) {
        self.rung.fetch_add(1, Ordering::Relaxed);
        drop(held);
        unlocked();
        self.ring(lock.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Wake every waiter, once the last count is made and its lock released.
    fn wake(&self) {
        #[cfg(test)]
        tests::before_wake();
        self.signal.notify_all();
    }

    /// [`park`] on this condvar with `guard`, the guard of the state it signals, until a ring
    /// or `until`, or, asked between slices, [`Lane::hung_up`]: a ring rung since the park
    /// began, read between slices, is a wake. The guard comes back on every exit, and its holder
    /// reads the state again.
    pub fn park<'a>(
        &self,
        guard: MutexGuard<'a, T>,
        until: Instant,
        lane: Lane,
    ) -> MutexGuard<'a, T> {
        let seen = self.rung.load(Ordering::Relaxed);
        let wait = |guard, bound| {
            let (guard, waited) = self.wait_timeout(guard, bound);
            (guard, !waited.timed_out())
        };
        let moved = || self.rung.load(Ordering::Relaxed) != seen;
        park_holding(guard, wait, until, lane, &moved).0
    }

    /// One wait for a ring (a spurious wake counts), recovering poison: its caller reads the
    /// state again after it.
    pub fn wait<'a>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.signal
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// [`Self::wait`] for at most `bound`, and whether it timed out.
    pub fn wait_timeout<'a>(
        &self,
        guard: MutexGuard<'a, T>,
        bound: Duration,
    ) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
        self.signal
            .wait_timeout(guard, bound)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The one slice loop.
fn park_holding<S>(
    mut held: S,
    mut wait: impl FnMut(S, Duration) -> (S, bool),
    until: Instant,
    lane: Lane,
    moved: &dyn Fn() -> bool,
) -> (S, Woke) {
    let now = Instant::now();
    let until = now
        .checked_add(MIN_PARK)
        .map_or(until, |floor| until.max(floor));
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return (held, Woke::Due);
        }
        let slice = (lane.slice)(left);
        let (back, woke) = wait(held, slice);
        held = back;
        if woke {
            return (held, Woke::Rang);
        }
        if slice >= left {
            return (held, Woke::Due);
        }
        if (lane.hung_up)() {
            return (held, Woke::HungUp);
        }
        if moved() {
            return (held, Woke::Rang);
        }
    }
}

#[cfg(test)]
#[path = "park_tests.rs"]
mod tests;
