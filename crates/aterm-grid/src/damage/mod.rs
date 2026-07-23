// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Damage tracking for efficient rendering.
//!
//! Only re-render cells that have changed since the last frame.
//!
//! ## Design
//!
//! - Track damaged row ranges using bitsets for O(1) marking and efficient iteration
//! - Support full damage (resize, scroll, clear screen)
//! - Support partial damage (individual cell changes)
//! - O(1) damage queries
//! - Fast iteration using `trailing_zeros()` to skip undamaged regions
//! - Column-level damage bounds for fine-grained GPU rendering
//! - Rectangle merging to reduce draw calls
//!
//! ## Usage
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

#[cfg(kani)]
pub(crate) use display_offset::DisplayOffsetDamage;
pub(crate) use display_offset::compute_display_offset_damage;
pub use iter::{BitsetRowIterator, DamageBoundsIterator, DamagedRowIterator};
pub use rect::LineDamageBounds;
pub use tracker::{DamageTracker, RowDamageBounds};

#[cfg(any(test, kani, feature = "testing"))]
pub use iter::MergedDamageIterator;
#[cfg(any(test, kani, feature = "testing"))]
pub use rect::DamageRect;

#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_display_offset;

#[cfg(kani)]
mod proofs;

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
    #[cfg(any(test, kani, feature = "testing"))]
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

    /// Mark a wide (double-width) cell pair as damaged in a single call.
    ///
    /// Equivalent to `mark_cell(row, col); mark_cell(row, col + 1)` but
    /// avoids the redundant bounds check, bit OR, and min/max.
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

    /// Reset damage tracking (call after render).
    ///
    /// Reuses existing allocations when the row count hasn't changed (the common
    /// frame-to-frame case), avoiding a heap alloc+free pair per frame. Falls
    /// back to fresh allocation only when transitioning from `Full` state.
    pub(crate) fn reset(&mut self, rows: u16) {
        match self {
            Damage::Partial(tracker) => tracker.clear(rows),
            Damage::Full => *self = Damage::Partial(DamageTracker::new(rows)),
        }
    }

    /// Iterate over damaged rows.
    ///
    /// For `Full` damage, yields all rows 0..rows.
    /// For `Partial` damage, uses bitset operations to efficiently skip undamaged rows.
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

    /// Iterate over damaged rows with their column bounds.
    ///
    /// This is the primary API for renderers. Each yielded `LineDamageBounds`
    /// contains the row index and the column range [left, right) that needs
    /// to be redrawn.
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

    /// Iterate over merged damage rectangles.
    ///
    /// This is useful for GPU rendering where batching adjacent rows
    /// into rectangles reduces draw calls.
    #[cfg(any(test, kani, feature = "testing"))]
    pub fn iter_merged(&self, rows: u16, cols: u16) -> MergedDamageIterator<'_> {
        MergedDamageIterator::new(self, rows, cols)
    }
}
