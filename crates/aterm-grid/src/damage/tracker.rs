// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bitset-based damage accumulation for tracking changed rows and cells.

/// Tracks which rows (and optionally columns) are damaged.
#[derive(Debug, Clone)]
pub struct DamageTracker {
    /// Bitset for damaged rows (1 bit per row).
    pub(crate) row_bits: Vec<u64>,
    /// Per-row column damage bounds: (min_col, max_col) if damaged.
    row_bounds: Vec<RowDamageBounds>,
}

/// Column damage bounds for a single row.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowDamageBounds {
    /// Minimum damaged column (inclusive).
    pub left: u16,
    /// Maximum damaged column (exclusive).
    pub right: u16,
    /// Whether any damage exists in this row.
    pub damaged: bool,
}

impl DamageTracker {
    /// Create a new damage tracker for the given number of rows.
    #[must_use]
    pub fn new(rows: u16) -> Self {
        let num_words = (rows as usize).div_ceil(64);
        Self {
            row_bits: vec![0; num_words],
            row_bounds: vec![RowDamageBounds::default(); rows as usize],
        }
    }

    /// Mark a single row as fully damaged.
    #[inline]
    pub(crate) fn mark_row(&mut self, row: u16) {
        let row = row as usize;
        if row < self.row_bounds.len() {
            let word = row / 64;
            let bit = row % 64;
            self.row_bits[word] |= 1 << bit;
            self.row_bounds[row] = RowDamageBounds {
                left: 0,
                right: u16::MAX,
                damaged: true,
            };
        }
    }

    /// Mark a range of rows as damaged.
    #[inline]
    pub(crate) fn mark_rows(&mut self, start: u16, end: u16) {
        for row in start..end {
            self.mark_row(row);
        }
    }

    /// Mark a wide (double-width) cell pair as damaged in a single call.
    ///
    /// Equivalent to `mark_cell(row, col); mark_cell(row, col + 1)` but
    /// avoids the redundant bounds check, bit OR, and branch.
    #[inline]
    pub(crate) fn mark_wide_cell(&mut self, row: u16, col: u16) {
        let row_idx = row as usize;
        if row_idx < self.row_bounds.len() {
            let word = row_idx / 64;
            let bit = row_idx % 64;
            self.row_bits[word] |= 1 << bit;

            let bounds = &mut self.row_bounds[row_idx];
            if bounds.damaged {
                bounds.left = bounds.left.min(col);
                bounds.right = bounds.right.max(col.saturating_add(2));
            } else {
                bounds.left = col;
                bounds.right = col.saturating_add(2);
                bounds.damaged = true;
            }
        }
    }

    /// Mark a specific cell as damaged.
    #[inline]
    pub(crate) fn mark_cell(&mut self, row: u16, col: u16) {
        let row_idx = row as usize;
        if row_idx < self.row_bounds.len() {
            let word = row_idx / 64;
            let bit = row_idx % 64;
            self.row_bits[word] |= 1 << bit;

            let bounds = &mut self.row_bounds[row_idx];
            if bounds.damaged {
                bounds.left = bounds.left.min(col);
                bounds.right = bounds.right.max(col.saturating_add(1));
            } else {
                bounds.left = col;
                bounds.right = col.saturating_add(1);
                bounds.damaged = true;
            }
        }
    }

    /// Check if a row is damaged.
    #[must_use]
    #[inline]
    pub(crate) fn is_row_damaged(&self, row: u16) -> bool {
        let row = row as usize;
        if row >= self.row_bounds.len() {
            return false;
        }
        let word = row / 64;
        let bit = row % 64;
        (self.row_bits[word] & (1 << bit)) != 0
    }

    /// Get damage bounds for a row.
    #[must_use]
    #[inline]
    pub(crate) fn row_damage_bounds(&self, row: u16) -> Option<(u16, u16)> {
        let row = row as usize;
        if row < self.row_bounds.len() && self.row_bounds[row].damaged {
            Some((self.row_bounds[row].left, self.row_bounds[row].right))
        } else {
            None
        }
    }

    /// Clear all damage, reusing existing allocations.
    ///
    /// When `rows` matches the current capacity (the common frame-to-frame case),
    /// zeroes buffers in-place with zero heap allocation. When `rows` changed
    /// (terminal resize), resizes the buffers.
    pub(crate) fn clear(&mut self, rows: u16) {
        let needed_words = (rows as usize).div_ceil(64);
        let needed_bounds = rows as usize;

        if self.row_bits.len() == needed_words && self.row_bounds.len() == needed_bounds {
            // Same size: zero in-place — no heap allocation.
            self.row_bits.fill(0);
            self.row_bounds.fill(RowDamageBounds::default());
        } else {
            // Row count changed (resize): must reallocate.
            self.row_bits.clear();
            self.row_bits.resize(needed_words, 0);
            self.row_bounds.clear();
            self.row_bounds
                .resize(needed_bounds, RowDamageBounds::default());
        }
    }

    /// Count total damaged rows.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn damaged_row_count(&self) -> usize {
        self.row_bits.iter().map(|w| w.count_ones() as usize).sum()
    }
}
