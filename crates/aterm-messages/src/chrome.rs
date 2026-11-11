// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-window chrome regrow floor preventing rapid shrink-then-grow grid resizes.

use crate::{Duration, Instant};

/// Minimum interval between a chrome row rise and the next unforced fall.
pub const CHROME_REGROW_SETTLE: Duration = Duration::from_millis(400);

/// Per-window timestamp of the last chrome row rise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegrowFloor {
    last_rise: Option<Instant>,
}

impl RegrowFloor {
    /// Records that the window's chrome rows rose at `now`.
    #[inline]
    pub fn rose(&mut self, now: Instant) {
        self.last_rise = Some(self.last_rise.map_or(now, |x| x.max(now)));
    }

    /// When the window's chrome rows last rose; `None` if they never have.
    #[inline]
    #[must_use]
    pub const fn last_rise(self) -> Option<Instant> {
        self.last_rise
    }

    /// Returns the earliest instant an unforced fall may re-grid the window.
    #[inline]
    #[must_use]
    pub fn floor(self) -> Option<Instant> {
        self.last_rise
            .map(|r| r.checked_add(CHROME_REGROW_SETTLE).unwrap_or(r))
    }
}

/// Returns whether an unforced fall may re-grid at `now` given `floor`.
#[inline]
#[must_use]
pub fn fall_allowed(floor: Option<Instant>, now: Instant) -> bool {
    floor.is_none_or(|f| now >= f)
}

/// Returns the later of `due` and `floor`.
#[inline]
#[must_use]
pub fn at_floor(due: Instant, floor: Option<Instant>) -> Instant {
    floor.map_or(due, |f| due.max(f))
}

/// Returns the latest floor across `floors`, or `None` if all are `None`.
#[inline]
#[must_use]
pub fn latest(floors: impl IntoIterator<Item = Option<Instant>>) -> Option<Instant> {
    floors.into_iter().flatten().max()
}

/// Returns whether a re-grid from `reserved` to `rows` increases chrome rows.
#[inline]
#[must_use]
pub const fn is_rise(reserved: u16, rows: u16) -> bool {
    rows > reserved
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
