// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sliced wait loop ([`park`], [`Lane::slice`]) beside [`crate::hangup`] that checks
//! [`Lane::hung_up`] within [`crate::hangup::HANGUP_POLL`] for blocking control verbs.

use std::sync::{Condvar, MutexGuard, PoisonError};
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

/// Park on `wait` until it wakes or `until` comes ([`Lane::hung_up`]).
pub fn park(
    mut wait: impl FnMut(Duration) -> bool,
    until: Instant,
    lane: Lane,
    moved: &dyn Fn() -> bool,
) -> Woke {
    park_holding((), |(), bound| ((), wait(bound)), until, lane, moved).1
}

/// [`park`] on `changed`, the condvar that signals the state `guard` guards.
pub fn park_guarded<'a, T>(
    changed: &Condvar,
    guard: MutexGuard<'a, T>,
    until: Instant,
    lane: Lane,
) -> MutexGuard<'a, T> {
    let wait = |guard, bound| {
        let (guard, waited) = changed
            .wait_timeout(guard, bound)
            .unwrap_or_else(PoisonError::into_inner);
        (guard, !waited.timed_out())
    };
    park_holding(guard, wait, until, lane, &|| false).0
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use aterm_spec::wait::MUST_HAPPEN;

    use super::*;

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
    /// ends `Due`: nothing is asked.
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
                std::thread::sleep(bound);
                false
            },
            Instant::now() + Duration::from_millis(20),
            lane,
            &|| -> bool { panic!("and reads no flag") },
        );
        assert_eq!((woke, bounds.len()), (Woke::Due, 1), "{bounds:?}");
    }

    /// A guarded park hands the guard back: a notify ends it with the state
    /// the notifier wrote readable under the guard. The negative control, no
    /// notify, ends at its instant with the guard back too.
    #[test]
    fn a_guarded_park_hands_its_guard_back() {
        let shared = Arc::new((Mutex::new(0u32), Condvar::new()));
        let parked = Arc::new(AtomicBool::new(false));
        let notifier = {
            let (shared, parked) = (shared.clone(), parked.clone());
            std::thread::spawn(move || {
                aterm_spec::wait::until_within(MUST_HAPPEN, "the park to hold the condvar", || {
                    parked.load(Ordering::SeqCst).then_some(())
                });
                *shared.0.lock().unwrap() = 7;
                shared.1.notify_all();
            })
        };
        let lane = Lane {
            slice: short,
            hung_up: never,
        };
        let until = Instant::now() + MUST_HAPPEN;
        let mut guard = shared.0.lock().unwrap();
        parked.store(true, Ordering::SeqCst);
        while *guard != 7 {
            assert!(
                Instant::now() < until,
                "hang detector: the notify never came"
            );
            guard = park_guarded(&shared.1, guard, until, lane);
        }
        drop(guard);
        notifier.join().unwrap();

        let guard = shared.0.lock().unwrap();
        let guard = park_guarded(
            &shared.1,
            guard,
            Instant::now() + Duration::from_millis(5),
            lane,
        );
        assert_eq!(*guard, 7, "the guard comes back at the instant");
    }
}
