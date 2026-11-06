// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A deadline that counts the time its host slept ([`WakeDeadline`]): what a sign-in that holds
//! the machine's sign-in lock (`accounts::SIGNIN_LOCK_FILE`) bounds each of its waits by, so a
//! sign-in the host slept past ends at its first look after the host wakes (KI-1105-14).

use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one turn of a wait that watches nothing but its deadline lasts at most
/// ([`WakeDeadline::tick`]): such a wait cannot see the clock that counts through sleep, so it
/// looks at both clocks again this often, and a deadline sleep passed ends it within this much of
/// the host's waking.
pub const SLEEP_CHECK: Duration = Duration::from_secs(1);

/// Where `now + budget` does not fit an [`Instant`], the clock that stops in sleep ends the
/// deadline this far from now.
const FAR: Duration = Duration::from_secs(86_400);

/// A deadline on two clocks: the one that stops while the host sleeps ([`Instant`]: macOS's
/// `mach_absolute_time`, Linux's `CLOCK_MONOTONIC`), and the one that counts through sleep
/// ([`aterm_uds::process::continuous_ns`]: `mach_continuous_time`, `CLOCK_BOOTTIME`) where the
/// host has one; elsewhere the first alone. It has passed once either clock says so. A budget the
/// first clock alone measures outlasts its wall time across sleep and dark wakes: a renewal's
/// 300 s held the sign-in lock for 6.2 h (KI-1105-14). A copy keeps the same end on both clocks;
/// it never starts a new budget.
#[derive(Clone, Copy, Debug)]
pub struct WakeDeadline {
    awake: Instant,
    through_sleep: Option<u64>,
    /// In a test, the [`SleepSeam`] it was made under, which says what its host slept besides.
    #[cfg(all(test, unix))]
    seam: Option<&'static AtomicU64>,
}

/// Two deadlines are the same when they end at the same point on both clocks (and, in a test,
/// read the same seam).
impl PartialEq for WakeDeadline {
    fn eq(&self, other: &Self) -> bool {
        let same = self.awake == other.awake && self.through_sleep == other.through_sleep;
        #[cfg(all(test, unix))]
        let same = same && self.seam.map(std::ptr::from_ref) == other.seam.map(std::ptr::from_ref);
        same
    }
}

impl Eq for WakeDeadline {}

impl WakeDeadline {
    /// `budget` from now, on both clocks.
    #[must_use]
    pub fn after(budget: Duration) -> Self {
        #[cfg(all(test, unix))]
        {
            let seam = SEAM.get();
            Self {
                seam,
                ..Self::after_from(budget, through_sleep_now(seam))
            }
        }
        #[cfg(not(all(test, unix)))]
        {
            Self::after_from(budget, through_sleep_now())
        }
    }

    /// `budget` from now, where `through_now` is the clock that counts through sleep now (`None`:
    /// the host has none). [`super::align`]'s capture passes its own reading, which its tests
    /// set.
    pub(super) fn after_from(budget: Duration, through_now: Option<u64>) -> Self {
        let now = Instant::now();
        Self {
            awake: now.checked_add(budget).unwrap_or_else(|| now + FAR),
            through_sleep: through_now.map(|now| now.saturating_add(nanos(budget))),
            #[cfg(all(test, unix))]
            seam: None,
        }
    }

    /// The clock that counts through sleep, now, as this deadline reads it.
    fn through_now(self) -> Option<u64> {
        #[cfg(all(test, unix))]
        {
            through_sleep_now(self.seam)
        }
        #[cfg(not(all(test, unix)))]
        {
            through_sleep_now()
        }
    }

    /// What is left of it now: the less of what each clock leaves.
    #[must_use]
    pub fn remaining(self) -> Duration {
        self.remaining_from(self.through_now())
    }

    /// [`WakeDeadline::remaining`], where `through_now` is the clock that counts through sleep
    /// now.
    pub(super) fn remaining_from(self, through_now: Option<u64>) -> Duration {
        let awake = self.awake.saturating_duration_since(Instant::now());
        match (self.through_sleep, through_now) {
            (Some(at), Some(now)) => awake.min(Duration::from_nanos(at.saturating_sub(now))),
            _ => awake,
        }
    }

    /// Whether it has passed: either clock says so.
    #[must_use]
    pub fn passed(self) -> bool {
        self.remaining().is_zero()
    }

    /// The deadline `ceiling` from now, or this one where it comes first: a sub-wait's bound,
    /// on both clocks. It never ends after this one.
    #[must_use]
    pub fn within(self, ceiling: Duration) -> Self {
        let bound = Self::after_from(ceiling, self.through_now());
        Self {
            awake: self.awake.min(bound.awake),
            through_sleep: match (self.through_sleep, bound.through_sleep) {
                (Some(this), Some(bound)) => Some(this.min(bound)),
                (this, bound) => this.or(bound),
            },
            #[cfg(all(test, unix))]
            seam: self.seam,
        }
    }

    /// It as an [`Instant`], for a leaf whose API takes one: now plus what is left of it
    /// ([`WakeDeadline::remaining`]), computed at this call. The leaf then measures what is left
    /// on the clock that stops in sleep alone, so it is handed only to a wait that a short cap
    /// bounds, or to one the caller cannot tick.
    #[must_use]
    pub fn instant(self) -> Instant {
        let now = Instant::now();
        now.checked_add(self.remaining())
            .unwrap_or_else(|| now + FAR)
    }

    /// Whether it counts the time the host slept: the host has a clock that counts through sleep.
    #[must_use]
    pub fn counts_sleep(self) -> bool {
        self.through_sleep.is_some()
    }

    /// How long one turn of a wait that watches nothing but this deadline may block: what is left
    /// of it, at most [`SLEEP_CHECK`] where it counts sleep, so the wait looks at both clocks
    /// again within a second of the host's waking.
    #[must_use]
    pub fn tick(self) -> Duration {
        let left = self.remaining();
        if self.counts_sleep() {
            left.min(SLEEP_CHECK)
        } else {
            left
        }
    }

    /// Waits on `rx` until a message comes (`Ok`), its senders are gone (`Disconnected`), or this
    /// deadline passes (`Timeout`), one [`WakeDeadline::tick`] at a time.
    /// # Errors
    /// `Timeout` once it passed, `Disconnected` once every sender is gone.
    pub fn recv<T>(self, rx: &mpsc::Receiver<T>) -> Result<T, mpsc::RecvTimeoutError> {
        loop {
            match rx.recv_timeout(self.tick()) {
                Err(mpsc::RecvTimeoutError::Timeout) if !self.passed() => {}
                other => return other,
            }
        }
    }
}

fn nanos(span: Duration) -> u64 {
    u64::try_from(span.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(all(test, unix))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(all(test, unix))]
thread_local! {
    /// The [`SleepSeam`] this thread holds: the deadlines it makes ([`WakeDeadline::after`])
    /// take it along.
    static SEAM: std::cell::Cell<Option<&'static AtomicU64>> = const { std::cell::Cell::new(None) };
}

/// The clock that counts through sleep, now.
#[cfg(not(all(test, unix)))]
fn through_sleep_now() -> Option<u64> {
    aterm_uds::process::continuous_ns()
}

/// The clock that counts through sleep, now, plus what `seam` says the host slept.
#[cfg(all(test, unix))]
fn through_sleep_now(seam: Option<&AtomicU64>) -> Option<u64> {
    let now = aterm_uds::process::continuous_ns()?;
    Some(now.saturating_add(seam.map_or(0, |slept| slept.load(Ordering::Acquire))))
}

/// TEST SEAM: what a test says its host slept, which no test can make the host do. While a
/// thread holds it, each deadline that thread makes ([`WakeDeadline::after`]) carries it, and so
/// does every deadline taken from one ([`WakeDeadline::within`], a copy), on whatever thread
/// reads it; a deadline made elsewhere never sees it. [`host_slept`] (this thread's seam) and
/// [`Sleeper::slept`] (from any thread) add to it. Its drop, a panic's unwind included, puts
/// back the seam this thread held before; the deadlines made under it keep it.
#[cfg(all(test, unix))]
pub(super) struct SleepSeam {
    slept: &'static AtomicU64,
    before: Option<&'static AtomicU64>,
}

#[cfg(all(test, unix))]
impl SleepSeam {
    /// Starts a seam at no sleep, held by this thread.
    #[must_use]
    pub(super) fn hold() -> Self {
        // One counter per seam, for the test's whole run: the deadlines made under it may
        // outlive the seam on other threads.
        let slept: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(0)));
        Self {
            slept,
            before: SEAM.replace(Some(slept)),
        }
    }

    /// A handle another thread says this seam's host slept with.
    #[must_use]
    pub(super) fn sleeper(&self) -> Sleeper {
        Sleeper(self.slept)
    }
}

#[cfg(all(test, unix))]
impl Drop for SleepSeam {
    fn drop(&mut self) {
        SEAM.set(self.before);
    }
}

/// TEST SEAM: a [`SleepSeam`]'s handle, which any thread may hold.
#[cfg(all(test, unix))]
#[derive(Clone, Copy)]
pub(super) struct Sleeper(&'static AtomicU64);

#[cfg(all(test, unix))]
impl Sleeper {
    /// The seam's host slept `slept` more, from now on, for every deadline made under it.
    pub(super) fn slept(self, slept: Duration) {
        self.0.fetch_add(nanos(slept), Ordering::AcqRel);
    }
}

/// TEST SEAM: the host of the [`SleepSeam`] this thread holds slept `slept` more, from now on,
/// for every deadline made under it ([`Sleeper::slept`]).
/// # Panics
/// No [`SleepSeam`] is held on this thread.
#[cfg(all(test, unix))]
pub(super) fn host_slept(slept: Duration) {
    Sleeper(SEAM.get().expect("a test holds a SleepSeam on this thread")).slept(slept);
}

#[cfg(all(test, unix))]
#[path = "wake_deadline_tests.rs"]
mod tests;
