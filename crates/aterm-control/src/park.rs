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
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    use super::*;

    thread_local! {
        /// What a test runs on this thread between a ring's last unlock and its wake.
        static BEFORE_WAKE: Cell<Option<fn()>> = const { Cell::new(None) };
    }

    /// The seam [`Changed::wake`] runs: this thread's hook, if a test armed one.
    pub(super) fn before_wake() {
        if let Some(hook) = BEFORE_WAKE.with(Cell::get) {
            hook();
        }
    }

    /// A slice of a millisecond, for a park that must be cut many times.
    fn short(left: Duration) -> Duration {
        left.min(Duration::from_millis(1))
    }

    /// A slice that ends with no wake asks after the caller, then after the
    /// flag, and parks again: the hang-up on its third ask ends the park after
    /// exactly three waits, and a flag moved at its first ask ends it after
    /// one. Both are counts, inside a window the hang detector bounds. A wake
    /// asks neither.
    #[test]
    fn a_bare_slice_asks_only_after_the_caller_and_the_flag() {
        static ASKS: AtomicUsize = AtomicUsize::new(0);
        fn third_ask() -> bool {
            ASKS.fetch_add(1, Ordering::SeqCst) + 1 >= 3
        }
        fn there() -> bool {
            false
        }
        let until = Instant::now() + MUST_HAPPEN;
        let sleep = |waits: &mut usize, bound: Duration| {
            *waits += 1;
            std::thread::sleep(bound);
            false
        };

        let (mut waits, flag_reads) = (0, AtomicUsize::new(0));
        let hang_up = Lane {
            slice: short,
            hung_up: third_ask,
        };
        let moved = || {
            flag_reads.fetch_add(1, Ordering::SeqCst);
            false
        };
        let woke = park(|b| sleep(&mut waits, b), until, hang_up, &moved);
        assert_eq!(woke, Woke::HungUp);
        assert_eq!(waits, 3, "one wait per ask");
        assert_eq!(
            flag_reads.load(Ordering::SeqCst),
            2,
            "the flag is read after each ask that found the caller there"
        );

        let mut waits = 0;
        let lane = Lane {
            slice: short,
            hung_up: there,
        };
        let woke = park(|b| sleep(&mut waits, b), until, lane, &|| true);
        assert_eq!((woke, waits), (Woke::Rang, 1), "a moved flag is a wake");

        ASKS.store(0, Ordering::SeqCst);
        let unread = || -> bool { panic!("a wake reads no flag") };
        let woke = park(|_| true, until, hang_up, &unread);
        assert_eq!((woke, ASKS.load(Ordering::SeqCst)), (Woke::Rang, 0));
    }

    /// Off a serving lane the park is one wait for the whole remainder, and
    /// ends `Due` on that wait's own timeout: nothing is asked. The fake wait
    /// answers that its whole bound passed unwoken without sleeping it, and the
    /// instant is the hang detector's, so no stall short of it comes before the
    /// one wait.
    #[test]
    fn an_unserved_park_is_one_wait() {
        let mut bounds = Vec::new();
        let lane = Lane {
            slice: |left: Duration| left,
            hung_up: || -> bool { panic!("an unserved park asks after no caller") },
        };
        let woke = park(
            |bound| {
                bounds.push(bound);
                false
            },
            Instant::now() + MUST_HAPPEN,
            lane,
            &|| -> bool { panic!("and reads no flag") },
        );
        assert_eq!((woke, bounds.len()), (Woke::Due, 1), "{bounds:?}");
    }

    /// A guarded park hands the guard back: a ring ends it with the state
    /// the ringer wrote readable under the guard. The negative control, no
    /// ring, ends at its instant with the guard back too.
    #[test]
    fn a_guarded_park_hands_its_guard_back() {
        let (state, changed) = (Mutex::new(0u32), Changed::new());
        let parked = AtomicBool::new(false);
        let lane = Lane {
            slice: short,
            hung_up: never,
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                until_within(MUST_HAPPEN, "the park to hold the condvar", || {
                    parked.load(Ordering::SeqCst).then_some(())
                });
                let mut held = state.lock().unwrap();
                *held = 7;
                changed.ring(held);
            });
            let until = Instant::now() + MUST_HAPPEN;
            let mut guard = state.lock().unwrap();
            parked.store(true, Ordering::SeqCst);
            while *guard != 7 {
                assert!(Instant::now() < until, "hang detector: the ring never came");
                guard = changed.park(guard, until, lane);
            }
        });

        let guard = state.lock().unwrap();
        let guard = changed.park(guard, Instant::now() + Duration::from_millis(5), lane);
        assert_eq!(*guard, 7, "the guard comes back at the instant");
    }

    /// A ring whose wake lands while a guarded park is between slices ends
    /// the park. The order is forced: the ringer counts its change under the
    /// lock and releases it, the park's slice times out and takes the lock
    /// back, the park asks after its caller, and only during that ask does
    /// the ringer's wake go out (the seam holds it back), finding nobody
    /// waiting. The count, read after the ask, ends the park; a park that
    /// lost the ring would wait another slice and ask again.
    #[test]
    fn a_ring_between_slices_ends_the_park() {
        /// 0 unrung; 1 counted and unlocked, its wake held back; 2 the park
        /// asks between slices; 3 the wake went out.
        static STAGE: AtomicU8 = AtomicU8::new(0);
        /// The asks after the wake went out.
        static LATE: AtomicUsize = AtomicUsize::new(0);
        fn between_slices() -> bool {
            match STAGE.load(Ordering::SeqCst) {
                0 => false,
                1 => {
                    STAGE.store(2, Ordering::SeqCst);
                    until_within(MUST_HAPPEN, "the ringer's wake", || {
                        (STAGE.load(Ordering::SeqCst) == 3).then_some(())
                    });
                    false
                }
                _ => {
                    // The ring was lost: end the park now rather than at its instant.
                    LATE.fetch_add(1, Ordering::SeqCst);
                    true
                }
            }
        }
        fn held_back() {
            until_within(MUST_HAPPEN, "the park between slices", || {
                (STAGE.load(Ordering::SeqCst) == 2).then_some(())
            });
        }
        let (state, changed) = (Mutex::new(0u32), Changed::new());
        let parked = AtomicBool::new(false);
        let lane = Lane {
            slice: short,
            hung_up: between_slices,
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                until_within(MUST_HAPPEN, "the park to hold the condvar", || {
                    parked.load(Ordering::SeqCst).then_some(())
                });
                // The lock is free only while the park waits out a slice, and the park
                // reads the stage only under it: it sees 1 once the change is counted.
                let mut held = state.lock().unwrap();
                *held = 7;
                STAGE.store(1, Ordering::SeqCst);
                BEFORE_WAKE.set(Some(held_back));
                changed.ring(held);
                BEFORE_WAKE.set(None);
                STAGE.store(3, Ordering::SeqCst);
            });
            let until = Instant::now() + MUST_HAPPEN;
            let mut guard = state.lock().unwrap();
            parked.store(true, Ordering::SeqCst);
            while *guard != 7 {
                assert!(Instant::now() < until, "hang detector: the ring never came");
                guard = changed.park(guard, until, lane);
            }
            drop(guard);
            // A spurious wake answered before the ask: let the ringer finish.
            let _ = STAGE.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
        });
        assert_eq!(
            LATE.load(Ordering::SeqCst),
            0,
            "a park that missed the ring asked again"
        );
    }

    /// A park that begins while `unlocked` runs, its waiter finding the work
    /// undone, ends once the work is done, though the ring's wake finds it
    /// between slices: `ring_after` counts again, under the lock, after
    /// `unlocked`. The order is forced: the waiter parks past the first
    /// count while `unlocked` runs, `unlocked` then does its work, and the
    /// ring's wake goes out (the seam holds it back) only while the park asks
    /// between slices, finding nobody waiting. A ring that counted only
    /// before `unlocked` leaves the park to wait another slice and ask again.
    #[test]
    fn a_park_begun_while_the_work_runs_ends_once_it_is_done() {
        /// 0 unrung; 1 `unlocked` runs; 2 the park began; 3 the work is done
        /// and its wake held back; 4 the park asks between slices; 5 the wake
        /// went out.
        static STAGE: AtomicU8 = AtomicU8::new(0);
        /// The work `unlocked` does, which the waiter reads.
        static DONE: AtomicBool = AtomicBool::new(false);
        /// The park ended before it asked with the wake held back.
        static ENDED: AtomicBool = AtomicBool::new(false);
        /// The asks after the wake went out.
        static LATE: AtomicUsize = AtomicUsize::new(0);
        fn between_slices() -> bool {
            match STAGE.load(Ordering::SeqCst) {
                3 => {
                    STAGE.store(4, Ordering::SeqCst);
                    until_within(MUST_HAPPEN, "the ringer's wake", || {
                        (STAGE.load(Ordering::SeqCst) == 5).then_some(())
                    });
                    false
                }
                5 => {
                    // The work's count was lost: end the park now rather than at its instant.
                    LATE.fetch_add(1, Ordering::SeqCst);
                    true
                }
                _ => false,
            }
        }
        fn held_back() {
            STAGE.store(3, Ordering::SeqCst);
            until_within(MUST_HAPPEN, "the park between slices", || {
                (STAGE.load(Ordering::SeqCst) == 4 || ENDED.load(Ordering::SeqCst)).then_some(())
            });
        }
        let (state, changed) = (Mutex::new(()), Changed::new());
        let lane = Lane {
            slice: short,
            hung_up: between_slices,
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                BEFORE_WAKE.set(Some(held_back));
                changed.ring_after(&state, state.lock().unwrap(), || {
                    STAGE.store(1, Ordering::SeqCst);
                    until_within(MUST_HAPPEN, "the park to begin", || {
                        (STAGE.load(Ordering::SeqCst) == 2).then_some(())
                    });
                    DONE.store(true, Ordering::SeqCst);
                });
                BEFORE_WAKE.set(None);
                STAGE.store(5, Ordering::SeqCst);
            });
            until_within(MUST_HAPPEN, "the work to begin", || {
                (STAGE.load(Ordering::SeqCst) == 1).then_some(())
            });
            let until = Instant::now() + MUST_HAPPEN;
            let mut guard = state.lock().unwrap();
            while !DONE.load(Ordering::SeqCst) {
                assert!(Instant::now() < until, "hang detector: the ring never came");
                // The work read undone, and the park's count read, under one hold of the lock.
                let _ = STAGE.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
                guard = changed.park(guard, until, lane);
            }
            drop(guard);
            // The park ended before its ask with the wake held back (on the second count, read
            // before the seam ran, or on a spurious wake): let the ringer finish.
            ENDED.store(true, Ordering::SeqCst);
        });
        assert_eq!(
            LATE.load(Ordering::SeqCst),
            0,
            "a park that began before the work was done asked again after it"
        );
    }
}
