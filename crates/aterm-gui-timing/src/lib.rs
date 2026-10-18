// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's ONE TIMING VALUE.
//!
//! Some of aterm-gui's waits are lengths a person never sees but a test or a
//! live headless instance waits out in full. Each such site keeps its
//! PRODUCTION length as a constant beside it and reads the length it waits
//! through [`Timing::of`] on the process's [`current`] value. Three families:
//!
//! * the fabric's commit-fence bound and its still-waiting notice
//!   (aterm-gui-fabric);
//! * the harness host's cadences: its named pauses, its restart back-off, the
//!   restart act's grace and its looks;
//! * the host's answers at its loop's turn ends and idle points (aterm-gui's
//!   `WorkerIdle`): the next look at a background break, and the moment a
//!   lagging or resetting account is decided again.
//!
//! The supervise loop's own turn-end and idle waits are aterm-agent's, read
//! under the same development seam. Two host timers stay at production in
//! every build: the disk watch's first look and its period (a fast one would
//! reclaim the machine's real build caches within a second of a test
//! instance's boot), and the activation wake's fallback poll (a test pulls its
//! trigger instead).
//!
//! * [`Timing::PRODUCTION`] — the constant itself. The default, and the only
//!   value a shipped build can have: production lengths never change.
//! * [`Timing::FAST`] — a thousandth of it (never under a millisecond): the
//!   test values. A debug build (or a `dev-seams` one) selects it when the
//!   development seam `ATERM_DEV_FAST_TIMERS` is set, so a live headless
//!   instance a test boots runs its timers fast; the unit tests' helpers
//!   select it for their whole process ([`select_fast`]).
//!
//! The choice is made once per process and only ever moves from production to
//! fast, so a timer read before a test helper ran and one read after agree from
//! then on. A test that must see a production length passes
//! [`Timing::PRODUCTION`] to what it drives, or runs in a process of its own
//! that never selects fast (aterm-gui-fabric's `tests/production_clock.rs`).
//!
//! It also keeps aterm-gui's one monotonic millisecond clock ([`now_ms`]),
//! which the window crate and the fabric endpoint below it both stamp on.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// The development seam that selects [`Timing::FAST`] in a debug build: set
/// (to anything) by the live headless test lanes. It is aterm-types' one name
/// for it, which aterm-agent's harness reads by the same rule, and which
/// `env_sanitize` denies to a nested aterm's children.
pub const DEV_FAST_TIMERS: &str = aterm_types::domain::ENV_DEV_FAST_TIMERS;

/// A timing value: the factor every routed wait divides its production length
/// by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    divisor: u32,
}

impl Timing {
    /// Every wait at its production length.
    pub const PRODUCTION: Self = Self { divisor: 1 };

    /// Every wait at a thousandth of its production length, and never under
    /// [`FAST_FLOOR`]: the 10 s fence bound waits 10 ms, a one-hour back-off
    /// 3.6 s.
    pub const FAST: Self = Self { divisor: 1000 };

    /// The length to wait for a wait whose production length is `production`.
    #[must_use]
    pub const fn of(self, production: Duration) -> Duration {
        if self.divisor == 1 || production.is_zero() {
            return production;
        }
        let scaled = production.checked_div(self.divisor);
        match scaled {
            Some(scaled) if scaled.as_nanos() >= FAST_FLOOR.as_nanos() => scaled,
            _ => FAST_FLOOR,
        }
    }

    /// [`Self::of`], never under `floor` when scaled: for a wait that races a
    /// real process (a signal's delivery and handling, a poll that reads the
    /// disk), whose test value must still leave the OS its time under load.
    /// The production length is `production`, whatever the floor.
    #[must_use]
    pub const fn of_at_least(self, production: Duration, floor: Duration) -> Duration {
        let scaled = self.of(production);
        if self.divisor == 1 || scaled.as_nanos() >= floor.as_nanos() {
            scaled
        } else if floor.as_nanos() < production.as_nanos() {
            floor
        } else {
            production
        }
    }

    /// Whether this is [`Timing::FAST`].
    #[must_use]
    pub const fn is_fast(self) -> bool {
        self.divisor != 1
    }
}

/// The shortest wait [`Timing::FAST`] scales to: a non-zero production wait
/// stays a wait (a zero one stays zero), so a loop that waits a cadence never
/// becomes a spin.
pub const FAST_FLOOR: Duration = Duration::from_millis(1);

const UNREAD: u8 = 0;
const PRODUCTION: u8 = 1;
const FAST: u8 = 2;

static CHOSEN: AtomicU8 = AtomicU8::new(UNREAD);

/// This process's timing value: [`Timing::FAST`] once [`select_fast`] ran or
/// when the development seam [`DEV_FAST_TIMERS`] is set in a build that reads
/// seams; [`Timing::PRODUCTION`] otherwise. The seam is read once.
#[must_use]
pub fn current() -> Timing {
    let chosen = match CHOSEN.load(Ordering::Acquire) {
        UNREAD => {
            let read = if aterm_types::dev_seam!(DEV_FAST_TIMERS).is_some() {
                FAST
            } else {
                PRODUCTION
            };
            // A `select_fast` that ran meanwhile wins: the choice only moves
            // from production to fast.
            match CHOSEN.compare_exchange(UNREAD, read, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => read,
                Err(now) => now,
            }
        }
        now => now,
    };
    if chosen == FAST {
        Timing::FAST
    } else {
        Timing::PRODUCTION
    }
}

/// Select [`Timing::FAST`] for the rest of this process: what the unit tests'
/// helpers call before they build what waits. A debug build's door only; a
/// release build cannot leave production lengths.
#[cfg(debug_assertions)]
pub fn select_fast() {
    CHOSEN.store(FAST, Ordering::Release);
}

/// Milliseconds since this process first read the clock: the one monotonic
/// millisecond clock aterm-gui stamps turns, mail rows and status ages on, so a
/// stamp one module took reads against another's `now_ms`. One epoch per
/// process, wherever it is read from.
#[must_use]
pub fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(Instant::now().saturating_duration_since(epoch).as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_is_the_constant_and_fast_a_thousandth_floored() {
        let ten = Duration::from_secs(10);
        assert_eq!(Timing::PRODUCTION.of(ten), ten);
        assert_eq!(Timing::FAST.of(ten), Duration::from_millis(10));
        assert_eq!(Timing::FAST.of(Duration::from_millis(40)), FAST_FLOOR);
        assert_eq!(Timing::FAST.of(Duration::ZERO), Duration::ZERO);
        assert_eq!(
            Timing::FAST.of(Duration::from_secs(3600)),
            Duration::from_millis(3600)
        );
        assert!(Timing::FAST.is_fast() && !Timing::PRODUCTION.is_fast());
    }

    /// A floored wait: production unchanged, the test value never under the
    /// floor, and never over the production length.
    #[test]
    fn a_floored_wait_keeps_production_and_its_floor() {
        let (five, second) = (Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(Timing::PRODUCTION.of_at_least(five, second), five);
        assert_eq!(Timing::FAST.of_at_least(five, second), second);
        assert_eq!(
            Timing::FAST.of_at_least(Duration::from_secs(3600), second),
            Duration::from_millis(3600)
        );
        assert_eq!(
            Timing::FAST.of_at_least(Duration::from_millis(400), second),
            Duration::from_millis(400)
        );
    }

    /// The choice only moves toward fast: once selected, every read agrees.
    #[test]
    fn select_fast_wins_for_the_rest_of_the_process() {
        select_fast();
        assert_eq!(current(), Timing::FAST);
        assert_eq!(current(), Timing::FAST);
    }
}
