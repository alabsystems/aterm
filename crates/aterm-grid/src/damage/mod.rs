// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Damage tracking: only cells changed since the last frame are re-rendered.
//!
//! Damaged rows are bitsets (O(1) marking, iteration skipping clean rows via
//! `trailing_zeros()`) with per-row column bounds. Resize uses full damage; a
//! scroll smaller than the screen marks only the rows it exposes.
//!
//! ```rust,ignore
//! use aterm_grid::Damage;
//!
//! let mut damage = Damage::new(24);
//!
//! if damage.is_full() {
//!     // Redraw everything
//! }
//!
//! if damage.has_damage() {
//!     for bounds in damage.iter_bounds(24, 80) {
//!         // bounds.line, bounds.left, bounds.right
//!     }
//! }
//! ```

mod display_offset;
mod iter;
mod rect;
mod tracker;

pub(crate) use display_offset::compute_display_offset_damage;
pub use iter::{BitsetRowIterator, DamageBoundsIterator, DamagedRowIterator};
pub use rect::LineDamageBounds;
pub use tracker::{DamageTracker, RowDamageBounds};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_display_offset;

/// Damage state for the terminal grid.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum Damage {
    /// Full damage - entire screen needs redraw.
    #[default]
    Full,
    /// Partial damage - only specific rows need redraw.
    Partial(DamageTracker),
}

impl Damage {
    /// Create a new damage tracker with partial tracking.
    #[cfg(test)]
    #[must_use]
    pub fn new(rows: u16) -> Self {
        Damage::Partial(DamageTracker::new(rows))
    }

    /// Mark full damage (entire screen needs redraw).
    #[inline]
    pub fn mark_full(&mut self) {
        *self = Damage::Full;
    }

    /// Mark a single row as damaged.
    #[inline]
    pub(crate) fn mark_row(&mut self, row: u16) {
        match self {
            Damage::Full => {}
            Damage::Partial(tracker) => tracker.mark_row(row),
        }
    }

    /// Mark a range of rows as damaged.
    #[inline]
    pub fn mark_rows(&mut self, start: u16, end: u16) {
        match self {
            Damage::Full => {}
            Damage::Partial(tracker) => tracker.mark_rows(start, end),
        }
    }

    /// Mark a cell as damaged.
    #[inline]
    pub fn mark_cell(&mut self, row: u16, col: u16) {
        match self {
            Damage::Full => {}
            Damage::Partial(tracker) => tracker.mark_cell(row, col),
        }
    }

    /// Mark a wide cell pair as damaged in one call (one bounds check).
    #[inline]
    pub(crate) fn mark_wide_cell(&mut self, row: u16, col: u16) {
        match self {
            Damage::Full => {}
            Damage::Partial(tracker) => tracker.mark_wide_cell(row, col),
        }
    }

    /// Check if the entire screen is damaged.
    #[must_use]
    #[inline]
    pub fn is_full(&self) -> bool {
        matches!(self, Damage::Full)
    }

    /// The mark-call clock, or `None` under [`Damage::Full`] (no tracker), where
    /// callers re-stamp unconditionally. See [`DamageTracker::mark_seq`].
    #[must_use]
    #[inline]
    pub(crate) fn mark_seq(&self) -> Option<u64> {
        match self {
            Damage::Full => None,
            Damage::Partial(tracker) => Some(tracker.mark_seq),
        }
    }

    /// Check if a row is damaged.
    #[must_use]
    #[inline]
    pub fn is_row_damaged(&self, row: u16) -> bool {
        match self {
            Damage::Full => true,
            Damage::Partial(tracker) => tracker.is_row_damaged(row),
        }
    }

    /// Get damaged row bounds for a row (returns column range if damaged).
    #[must_use]
    pub fn row_damage_bounds(&self, row: u16, cols: u16) -> Option<(u16, u16)> {
        match self {
            Damage::Full => Some((0, cols)),
            Damage::Partial(tracker) => tracker.row_damage_bounds(row).and_then(|(left, right)| {
                let left = left.min(cols);
                let right = right.min(cols);
                if left < right {
                    Some((left, right))
                } else {
                    None
                }
            }),
        }
    }

    /// Reset damage tracking (call after render), reusing allocations when the
    /// row count is unchanged.
    pub(crate) fn reset(&mut self, rows: u16) {
        match self {
            Damage::Partial(tracker) => tracker.clear(rows),
            Damage::Full => *self = Damage::Partial(DamageTracker::new(rows)),
        }
    }

    /// Iterate over damaged rows: all rows for `Full`, bitset scan for
    /// `Partial`.
    pub fn damaged_rows(&self, rows: u16) -> DamagedRowIterator<'_> {
        match self {
            Damage::Full => DamagedRowIterator::Full {
                current: 0,
                max: rows,
            },
            Damage::Partial(tracker) => {
                DamagedRowIterator::Partial(BitsetRowIterator::new(&tracker.row_bits, rows))
            }
        }
    }

    /// Iterate over damaged rows with their column range [left, right) (the
    /// primary renderer API).
    pub fn iter_bounds(&self, rows: u16, cols: u16) -> DamageBoundsIterator<'_> {
        DamageBoundsIterator::new(self, self.damaged_rows(rows), cols)
    }

    /// Check if any damage exists.
    #[must_use]
    #[inline]
    pub fn has_damage(&self) -> bool {
        match self {
            Damage::Full => true,
            Damage::Partial(tracker) => tracker.row_bits.iter().any(|&w| w != 0),
        }
    }
}
