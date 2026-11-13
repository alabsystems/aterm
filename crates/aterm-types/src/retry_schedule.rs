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

    /// [`Self::due`] as read at `now` (unix seconds): a last try recorded after `now` means the
    /// clock was set back since, which leaves a pause that cannot be measured, so the next try is
    /// due at once rather than at a time the clock may take far longer than [`Self::longest`] to
    /// reach. The try then made records the corrected time, and the schedule goes on from it.
    #[must_use]
    pub fn due_by(self, at: u64, tries: u32, now: u64) -> u64 {
        if at > now { now } else { self.due(at, tries) }
    }

    /// The longest pause this schedule ever waits.
    #[must_use]
    pub const fn longest(self) -> Duration {
        self.longest
    }
}

#[cfg(test)]
#[path = "retry_schedule_tests.rs"]
mod tests;
