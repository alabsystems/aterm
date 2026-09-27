// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE BACK-OFF LADDER: a schedule of pauses, the `n`-th retry's pause
//! its `n`-th step, and every retry past the table its LAST step — never a
//! give-up. Every schedule the supervisor and its host wait on reads through
//! it (the elegance review of 2026-09-25: six hand-rolled copies of the same
//! `TABLE[n.min(len - 1)]` clamp): the host's supervisor restarts, the
//! relaunch's attempts, the upgrade's looks, the session survey's tries, and
//! the turn-end policy's API-error and usage-limit ladders. Its one
//! geometric form, [`doubling`], is every back-off that doubles to a cap: a
//! box's press retries, a re-asked question's pause and the short-turn
//! streak.

use std::time::Duration;

/// THE ONE DOUBLING BACK-OFF: `base` at step 0, twice the last at each next
/// step, at most `max` — for every `n`, never a give-up and never an
/// overflow.
#[must_use]
pub fn doubling(base: Duration, n: u32, max: Duration) -> Duration {
    base.checked_mul(1u32.checked_shl(n).unwrap_or(u32::MAX))
        .unwrap_or(max)
        .min(max)
}

/// A schedule of pauses ([module header](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder<'a>(pub &'a [Duration]);

impl Ladder<'_> {
    /// The pause at step `n` (0-based: the first retry's is step 0), the
    /// last step for every `n` past the table; `None` for an empty one.
    #[must_use]
    pub fn at(self, n: usize) -> Option<Duration> {
        self.0.get(n.min(self.0.len().checked_sub(1)?)).copied()
    }

    /// [`Self::at`] for a table that is never empty (a `const` schedule).
    #[must_use]
    pub fn step(self, n: usize) -> Duration {
        self.at(n).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each step in turn, then the last for ever; nothing from an empty
    /// table.
    #[test]
    fn a_ladder_clamps_to_its_last_step() {
        let s = |n| Duration::from_secs(n);
        let table = [s(1), s(5), s(15)];
        let l = Ladder(&table);
        assert_eq!(
            (0..6).map(|n| l.step(n)).collect::<Vec<_>>(),
            [s(1), s(5), s(15), s(15), s(15), s(15)]
        );
        assert_eq!(l.at(usize::MAX), Some(s(15)));
        assert_eq!(Ladder(&[]).at(0), None);
    }

    /// Doubling from the base, then the cap for ever — past a shift or a
    /// multiplication that would overflow too. NEGATIVE CONTROL: a cap under
    /// the base is the cap.
    #[test]
    fn a_doubling_back_off_doubles_to_its_cap() {
        let ms = Duration::from_millis;
        assert_eq!(
            (0..6)
                .map(|n| doubling(ms(500), n, ms(6000)))
                .collect::<Vec<_>>(),
            [ms(500), ms(1000), ms(2000), ms(4000), ms(6000), ms(6000)]
        );
        assert_eq!(doubling(ms(500), 31, ms(6000)), ms(6000));
        assert_eq!(doubling(ms(500), u32::MAX, ms(6000)), ms(6000));
        assert_eq!(doubling(Duration::MAX, 1, ms(6000)), ms(6000));
        assert_eq!(doubling(ms(500), 0, ms(100)), ms(100));
    }
}
