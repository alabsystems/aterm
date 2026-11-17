// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sliced wait loop ([`park`], [`Lane::slice`]) beside [`crate::hangup`] that checks
//! [`Lane::hung_up`] within [`crate::hangup::HANGUP_POLL`] for blocking control verbs, the
//! condvar a guarded park waits on ([`Changed`]), and the parks the hang-up bell rings
//! ([`Lane::bell`]: [`Changed::park`], [`park_on_slot`]).

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, WaitTimeoutResult};
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
    /// How a park the hang-up bell can ring ([`Changed::park`], [`park_on_slot`]) hears its
    /// caller go; `None` for a park that arms no bell. [`park`] itself, whose wait no bell can
    /// reach, never reads it.
    pub bell: Option<BellLane>,
}

/// How a park the hang-up bell can ring hears its caller go ([`Lane::bell`]): on a thread that
/// serves a connection it arms [`Self::bell`] on that connection for the park, and each wait
/// begun while that arm stands and the bell still serves it ([`aterm_uds::hangup::Armed::ringing`],
/// read before each wait) is bounded by [`Self::slice`] in place of [`Lane::slice`], the bell's
/// ring ending the wait when the caller hangs up.
#[derive(Clone, Copy)]
pub struct BellLane {
    /// The bell ([`aterm_uds::hangup::Bell::process`] as shipped, which
    /// [`crate::hangup::ServingFd::enter`] starts). A park arms it and never starts it, so the
    /// arm waits for no spawn under the park's lock: a bell not started arms nothing.
    #[cfg(unix)]
    pub bell: fn() -> &'static aterm_uds::hangup::Bell,
    /// The longest one wait runs while the arm stands and the bell serves it.
    pub slice: fn(Duration) -> Duration,
}

impl BellLane {
    /// The process's bell, for a park a condition variable's notify rings ([`Changed::park`]):
    /// a notify between the park's last ask and its wait is lost, so each wait is capped at
    /// [`crate::hangup::ARMED_POLL`] ([`crate::hangup::armed_park`]).
    pub const NOTIFIED: BellLane = BellLane {
        #[cfg(unix)]
        bell: aterm_uds::hangup::Bell::process,
        slice: crate::hangup::armed_park,
    };
}

impl Lane {
    /// The connection this thread serves, if it serves one: sliced at
    /// [`crate::hangup::HANGUP_POLL`], or, in a park the process's hang-up bell rings, at
    /// [`crate::hangup::ARMED_POLL`] while armed ([`BellLane::NOTIFIED`]).
    pub const SERVED: Lane = Lane {
        slice: crate::hangup::hangup_park,
        hung_up: crate::hangup::caller_hung_up,
        bell: Some(BellLane::NOTIFIED),
    };

    /// This lane for a park that finishes whether or not its caller is still there: it asks
    /// after no caller and arms no bell.
    #[must_use]
    #[inline]
    pub const fn ignoring_hangup(self) -> Lane {
        Lane {
            slice: self.slice,
            hung_up: never,
            bell: None,
        }
    }

    /// This lane for a park whose ring a single-slot channel keeps ([`park_on_slot`]): a ring
    /// sent before the wait begins waits in the slot, so none is lost, and while armed one wait
    /// runs for all that is left.
    #[must_use]
    #[inline]
    pub const fn slotted(self) -> Lane {
        Lane {
            bell: match self.bell {
                // Off Unix a bell lane is its slice alone, so the new one takes nothing of it.
                #[cfg_attr(
                    not(unix),
                    expect(unused_variables, reason = "off Unix the slice is all a lane holds")
                )]
                Some(bell) => Some(BellLane {
                    slice: unsliced,
                    ..bell
                }),
                None => None,
            },
            ..self
        }
    }
}

#[inline]
const fn never() -> bool {
    false
}

/// One wait for all that is left: an armed slot park's slice ([`Lane::slotted`]).
#[inline]
const fn unsliced(left: Duration) -> Duration {
    left
}

/// Run `f` with the slice a park the hang-up bell can ring bounds each wait by: with `lane.bell`
/// set, on a thread that serves a connection, `lane.bell`'s bell is armed on that connection with
/// `ring()` for the whole of `f`, and a wait begun while that arm stands and the bell serves it is
/// bounded by the bell lane's slice; otherwise (no bell lane, no connection served, the arm
/// refused because the bell was not started, could not start, has ended or holds that descriptor
/// already, or the bell ended since the arm) by `lane.slice`. Whether the bell still serves the
/// arm is read before each wait: a bell that ends rings every arm it holds, a ring a park between
/// two waits never hears where it is a condvar's notify, so from the next wait on that park is
/// sliced by `lane` again, and the park its caller makes next finds the arm refused. The arm
/// never starts the bell, so it waits for no spawn under its caller's lock.
#[cfg(unix)]
fn belled<R>(
    lane: Lane,
    ring: impl FnOnce() -> aterm_uds::hangup::Ring,
    f: impl FnOnce(&dyn Fn(Duration) -> Duration) -> R,
) -> R {
    let Some(bell) = lane.bell else {
        return f(&lane.slice);
    };
    crate::hangup::with_served(|served| {
        // Held to the end of `f`: its drop takes the arm out, after which no ring comes.
        let armed = served
            .as_ref()
            .and_then(|fd| (bell.bell)().arm(fd, ring()).ok());
        let slice = |left| {
            if armed
                .as_ref()
                .is_some_and(aterm_uds::hangup::Armed::ringing)
            {
                (bell.slice)(left)
            } else {
                (lane.slice)(left)
            }
        };
        f(&slice)
    })
}

/// [`park`] on a wait that ends at a send into `slot`, a single-slot channel whose receiver
/// `wait` waits on: with `lane.bell` set, on a thread that serves a connection, the hang-up bell
/// is armed on that connection to ring `slot` for the park, so a caller that hangs up ends the
/// wait, and each wait begun while that arm stands and the bell serves it is bounded by the bell
/// lane's slice ([`Lane::slotted`]: all that is left). Otherwise it slices by `lane` as [`park`]
/// does.
pub fn park_on_slot(
    mut wait: impl FnMut(Duration) -> bool,
    until: Instant,
    lane: Lane,
    moved: &dyn Fn() -> bool,
    slot: &SyncSender<()>,
) -> Woke {
    let parked = move |slice: &dyn Fn(Duration) -> Duration| {
        park_holding(
            (),
            move |(), bound| ((), wait(bound)),
            until,
            slice,
            lane.hung_up,
            moved,
        )
        .1
    };
    #[cfg(unix)]
    {
        belled(lane, || aterm_uds::hangup::Ring::Slot(slot.clone()), parked)
    }
    #[cfg(not(unix))]
    {
        let _ = slot;
        parked(&lane.slice)
    }
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
    park_holding(
        (),
        |(), bound| ((), wait(bound)),
        until,
        &lane.slice,
        lane.hung_up,
        moved,
    )
    .1
}

/// The condvar that signals a change to the state a `Mutex<T>` guards, and the count of those
/// changes. Its waiters wake at a ring ([`Self::ring`], [`Self::ring_after`]), which takes a guard
/// of that state and counts the change before it releases the lock, and at the hang-up bell's
/// notify while a park on it is armed ([`Self::park`]), which takes no guard and counts nothing:
/// a woken waiter reads the state, the count and its caller again. A guarded park reads the count
/// under the lock as it parks and again under the lock after every slice, so a ring whose wake
/// found nobody waiting (the parked thread was between slices: its slice had timed out while the
/// ringer held the lock, or the wake came before the next slice began) still ends the park; a
/// bell notify that found nobody waiting is made up at the end of the slice its lane bounds.
pub struct Changed<T> {
    /// Shared with the hang-up bell's arm while a park on it is armed ([`Self::park`]).
    signal: Arc<Condvar>,
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
    pub fn new() -> Self {
        Self {
            signal: Arc::new(Condvar::new()),
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
    /// began, read between slices, is a wake. With `lane.bell` set, on a thread that serves a
    /// connection, the hang-up bell is armed on that connection for the park to notify this
    /// condvar when the caller hangs up, and each wait begun while that arm stands and the bell
    /// serves it is bounded by the bell lane's slice ([`BellLane::NOTIFIED`]: the bound on a
    /// notify lost between the last ask and the wait); otherwise, a bell that ended mid-park
    /// included, it slices by `lane`. The guard comes back on every exit, and its holder reads
    /// the state again (and asks after its caller).
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
        let parked = |slice: &dyn Fn(Duration) -> Duration| {
            park_holding(guard, wait, until, slice, lane.hung_up, &moved).0
        };
        #[cfg(unix)]
        {
            belled(
                lane,
                || aterm_uds::hangup::Ring::Notify(Arc::clone(&self.signal)),
                parked,
            )
        }
        #[cfg(not(unix))]
        {
            parked(&lane.slice)
        }
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

/// The one slice loop: each wait is bounded by `slice` of what is left, and a bare slice asks
/// `hung_up`, then `moved`.
fn park_holding<S>(
    mut held: S,
    mut wait: impl FnMut(S, Duration) -> (S, bool),
    until: Instant,
    slice: &dyn Fn(Duration) -> Duration,
    hung_up: fn() -> bool,
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
        let bound = slice(left);
        let (back, woke) = wait(held, bound);
        held = back;
        if woke {
            return (held, Woke::Rang);
        }
        if bound >= left {
            return (held, Woke::Due);
        }
        if hung_up() {
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
