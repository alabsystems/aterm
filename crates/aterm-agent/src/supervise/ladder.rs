// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Step-table and doubling retry back-off schedules.

use std::time::Duration;

/// Returns `base` doubled `n` times and clamped to `max` without overflow.
#[must_use]
pub fn doubling(base: Duration, n: u32, max: Duration) -> Duration {
    base.checked_mul(1u32.checked_shl(n).unwrap_or(u32::MAX))
        .unwrap_or(max)
        .min(max)
}

/// A table-driven schedule of retry pauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder<'a>(pub &'a [Duration]);

impl Ladder<'_> {
    /// Returns the pause at 0-based step `n`, clamping to the last entry, or `None` if empty.
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
