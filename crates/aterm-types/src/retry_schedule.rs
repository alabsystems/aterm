// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When setup tries an unfinished step again by itself. Setup never stops trying a step that
//! failed or was cut off: no try count turns into a hold that waits for a person's press, and
//! nothing about it is kept in memory alone. Each consumer persists the time of its last try and
//! how many tries in a row left the step unfinished, so a relaunch, an update or a reboot reads
//! the same due time, and names its own [`Backoff`]: how soon the first try again comes and the
//! longest pause it ever waits. A fact that changes what the try would do (a network change, a
//! vault change, a new build, a grant) is each consumer's early wake, beside this schedule.

use std::time::Duration;

/// A doubling schedule with a ceiling: no pause before the first try, `first` after it, twice the
/// last pause after each later try, never more than `longest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    first: Duration,
    longest: Duration,
}

impl Backoff {
    /// A schedule whose first pause is `first` and whose longest is `longest`; `first` past
    /// `longest` waits `longest` from the first try on.
    #[must_use]
    pub const fn new(first: Duration, longest: Duration) -> Self {
        Self { first, longest }
    }

    /// The pause after `tries` tries in a row that left the step unfinished: none before the
    /// first try, then `first`, doubled per try, never past `longest`.
    #[must_use]
    pub fn after(self, tries: u32) -> Duration {
        if tries == 0 {
            return Duration::ZERO;
        }
        let doublings = (tries - 1).min(u32::BITS - 1);
        self.first
            .checked_mul(1 << doublings)
            .unwrap_or(self.longest)
            .min(self.longest)
    }

    /// The unix second at which the next try is due, for a last try at `at` (unix seconds) that
    /// was the `tries`-th in a row to leave the step unfinished.
    #[must_use]
    pub fn due(self, at: u64, tries: u32) -> u64 {
        at.saturating_add(self.after(tries).as_secs())
    }

    /// The longest pause this schedule ever waits.
    #[must_use]
    pub const fn longest(self) -> Duration {
        self.longest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn doubles_from_the_first_pause_up_to_the_longest() {
        let schedule = Backoff::new(MINUTE, 15 * MINUTE);
        let pauses: Vec<u64> = (0..7)
            .map(|tries| schedule.after(tries).as_secs() / 60)
            .collect();
        assert_eq!(pauses, [0, 1, 2, 4, 8, 15, 15]);
    }

    #[test]
    fn never_overflows_into_a_stop() {
        let schedule = Backoff::new(MINUTE, 60 * MINUTE);
        assert_eq!(schedule.after(u32::MAX), 60 * MINUTE);
        assert_eq!(schedule.due(u64::MAX - 1, u32::MAX), u64::MAX);
    }

    #[test]
    fn a_first_pause_past_the_longest_waits_the_longest() {
        let schedule = Backoff::new(60 * MINUTE, 15 * MINUTE);
        assert_eq!(schedule.after(1), 15 * MINUTE);
        assert_eq!(schedule.due(100, 1), 100 + 15 * 60);
    }
}
