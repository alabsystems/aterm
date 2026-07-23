// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Region and column shift operations for [`CellExtras`].
//!
//! Extracted from `extra_collection.rs` for file size compliance.
//! These methods handle scroll-region shifts and insert/delete character
//! column shifts — all O(E) drain-rebuild since they affect subsets of rows.

use aterm_hash::FxHashMap;

use super::extra_collection::CellExtras;
use crate::extra::{CellCoord, CellExtra};

impl CellExtras {
    /// Batch shift rows up within a bounded region by `n`.
    ///
    /// Rows in `[top, top + n)` are deleted. Rows in `[top + n, bottom]` are
    /// shifted up by `n`. Rows outside `[top, bottom]` are preserved.
    /// Single HashMap drain-rebuild: O(E) regardless of `n`.
    ///
    /// Combines offset compaction and region shift into one pass to avoid
    /// the double drain-rebuild that `compact()` + shift would require.
    pub(crate) fn shift_region_up_by(&mut self, top: u16, bottom: u16, n: u16) {
        // Shift the dense rings (RGB truecolor + complex chars) the SAME way as
        // the HashMap below, instead of wiping them wholesale. The old wipe
        // dropped truecolor for EVERY cell — including rows outside the scrolled
        // region — because ring-only entries are never spilled to the HashMap
        // (#7458 follow-up: preserve unaffected cells' colors).
        self.shift_rings_region_up(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let shift_start = top.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            // Apply accumulated row offset: convert internal -> external row.
            // Entries with internal row < offset are stale -- drop them.
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom {
                // Outside region: preserve with external coords
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                // Within region and below deleted rows: shift up by n
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
            // Rows in [top, top + n) are deleted, including the top row.
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift rows down within `[top, bottom]` by `n`. Rows in `[bottom-n+1, bottom]`
    /// dropped; `[top, bottom-n]` shifted. O(E) drain-rebuild.
    ///
    /// Combines offset compaction and region shift into one pass to avoid
    /// the double drain-rebuild that `compact()` + shift would require.
    pub(crate) fn shift_region_down_by(&mut self, top: u16, bottom: u16, n: u16) {
        // Shift the dense rings the SAME way as the HashMap — see
        // shift_region_up_by (#7458 follow-up).
        self.shift_rings_region_down(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let drop_start = bottom.saturating_sub(n.saturating_sub(1));
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            // Apply accumulated row offset: convert internal -> external row.
            // Entries with internal row < offset are stale -- drop them.
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row >= top && ext_row <= bottom {
                if ext_row < drop_start {
                    // saturating_add: prevent u16 overflow (result fits per caller invariant)
                    new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
                }
            } else {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift extras up within a rectangular region `[top, bottom] × [left, right]`.
    ///
    /// Rows `[top, top + n)` within the column range are deleted.
    /// Rows `[top + n, bottom]` within the column range shift up by `n`.
    /// Entries outside the column range `[left, right]` are preserved unchanged.
    /// Entries outside the row range `[top, bottom]` are preserved unchanged.
    ///
    /// Used by `scroll_region_up_margined` (DECLRMM) (#7415).
    pub(crate) fn shift_rect_up_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        // Shift the dense rings within the rect the SAME way as the HashMap —
        // see shift_region_up_by (#7458 follow-up).
        self.shift_rings_rect_up(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let shift_start = top.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            // Outside row range or outside column range: preserve unchanged.
            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            // Inside the rect: shift up or drop.
            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
            // Rows in [top, top + n) within the column range are deleted.
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift extras down within a rectangular region `[top, bottom] × [left, right]`.
    ///
    /// Rows `[bottom - n + 1, bottom]` within the column range are dropped.
    /// Rows `[top, bottom - n]` within the column range shift down by `n`.
    /// Entries outside the column range or row range are preserved unchanged.
    ///
    /// Used by `scroll_region_down_margined` (DECLRMM) (#7415).
    pub(crate) fn shift_rect_down_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        // Shift the dense rings within the rect the SAME way as the HashMap —
        // see shift_region_up_by (#7458 follow-up).
        self.shift_rings_rect_down(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let drop_start = bottom.saturating_sub(n.saturating_sub(1));
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            // Outside row range or outside column range: preserve unchanged.
            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            // Inside the rect: shift down or drop.
            if ext_row < drop_start {
                new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
            }
            // Rows in [drop_start, bottom] within the column range are dropped.
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift columns right within a single row for ICH (Insert Character).
    ///
    /// Columns in `[start_col, max_col - count)` shift right by `count`.
    /// Columns in `[start_col, start_col + count)` are cleared (new blanks).
    /// Columns that shift past `max_col` are dropped.
    /// Only extracts and reinserts entries on the target row — non-target rows
    /// are never moved and no new HashMap is allocated.
    pub fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        // Shift the dense rings on this row the SAME way as the HashMap, instead
        // of wiping them — preserves truecolor/complex chars of the unaffected
        // cells (and other rows) on ICH (#7458 follow-up).
        self.shift_rings_cols_right(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        // Two-phase extract-then-reinsert: collect all affected entries first,
        // then reinsert shifted versions. This avoids ordering issues where a
        // shifted destination overlaps with a not-yet-processed source key.
        // Only extract columns within [start_col, max_col); columns at or beyond
        // max_col are outside the margin boundary (DECLRMM) and must be preserved
        // in place, mirroring shift_cols_left. Without the upper bound, an entry
        // already past max_col would be extracted and then dropped by the reinsert
        // guard below, silently losing its extras (#7458 follow-up).
        let keys: aterm_alloc::SmallVec<CellCoord, 8> = self
            .data
            .keys()
            .filter(|c| c.row == internal_row && c.col >= start_col && c.col < max_col)
            .copied()
            .collect();
        let mut extracted: aterm_alloc::SmallVec<(u16, CellExtra), 8> =
            aterm_alloc::SmallVec::new();
        for key in keys {
            if let Some(extra) = self.data.remove(&key) {
                extracted.push((key.col, extra));
            }
        }
        for (col, extra) in extracted {
            let new_col = col.saturating_add(count);
            if new_col < max_col {
                self.data
                    .insert(CellCoord::new(internal_row, new_col), extra);
            }
            // Extras that shift past max_col are dropped
        }
        self.maybe_shrink();
    }

    /// Shift columns left within a single row for DCH (Delete Character).
    ///
    /// Columns in `[start_col, start_col + count)` are deleted.
    /// Columns in `[start_col + count, max_col)` shift left by `count`.
    /// Only extracts and reinserts entries on the target row — non-target rows
    /// are never moved and no new HashMap is allocated.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        // Shift the dense rings on this row the SAME way as the HashMap — see
        // shift_cols_right (#7458 follow-up).
        self.shift_rings_cols_left(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        let shift_start = start_col.saturating_add(count);
        // Two-phase extract-then-reinsert (same rationale as shift_cols_right).
        // Only extract columns within [start_col, max_col); columns at or beyond
        // max_col are outside the margin boundary and must be preserved in place.
        let keys: aterm_alloc::SmallVec<CellCoord, 8> = self
            .data
            .keys()
            .filter(|c| c.row == internal_row && c.col >= start_col && c.col < max_col)
            .copied()
            .collect();
        let mut extracted: aterm_alloc::SmallVec<(u16, CellExtra), 8> =
            aterm_alloc::SmallVec::new();
        for key in keys {
            if let Some(extra) = self.data.remove(&key) {
                extracted.push((key.col, extra));
            }
        }
        for (col, extra) in extracted {
            if col >= shift_start {
                // After deletion range: shift left by count
                self.data
                    .insert(CellCoord::new(internal_row, col - count), extra);
            }
            // Columns in [start_col, start_col + count) are deleted
        }
        self.maybe_shrink();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Helper: create a CellExtra with an fg RGB color to make `has_data()` true.
    fn extra_with_fg(r: u8, g: u8, b: u8) -> CellExtra {
        let mut e = CellExtra::default();
        e.set_fg_rgb(Some([r, g, b]));
        e
    }

    /// Helper: create a CellExtra with a hyperlink to test hyperlink-bearing entries.
    fn extra_with_hyperlink(url: &str) -> CellExtra {
        let mut e = CellExtra::default();
        e.set_hyperlink(Some(Arc::from(url)));
        e
    }

    /// Helper: insert an extra at external (row, col) into the collection.
    fn insert(extras: &mut CellExtras, row: u16, col: u16, extra: CellExtra) {
        extras.set(CellCoord::new(row, col), extra);
    }

    /// Helper: check that an extra exists at external (row, col) and has fg RGB.
    fn has_fg_at(extras: &CellExtras, row: u16, col: u16) -> bool {
        extras
            .get(CellCoord::new(row, col))
            .and_then(|e| e.fg_rgb())
            .is_some()
    }

    /// Helper: get the fg RGB value at (row, col).
    fn fg_at(extras: &CellExtras, row: u16, col: u16) -> Option<[u8; 3]> {
        extras
            .get(CellCoord::new(row, col))
            .and_then(|e| e.fg_rgb())
    }

    // =========================================================================
    // shift_region_up_by
    // =========================================================================

    #[test]
    fn test_shift_region_up_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_region_up_by(0, 23, 1);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_region_up_zero_n_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(255, 0, 0));
        extras.shift_region_up_by(0, 23, 0);
        assert!(has_fg_at(&extras, 5, 0), "n=0 should not move entries");
    }

    #[test]
    fn test_shift_region_up_deletes_top_rows() {
        let mut extras = CellExtras::new();
        // Put extras at rows 0, 1, 2 in region [0, 9]
        insert(&mut extras, 0, 0, extra_with_fg(10, 0, 0));
        insert(&mut extras, 1, 0, extra_with_fg(20, 0, 0));
        insert(&mut extras, 2, 0, extra_with_fg(30, 0, 0));

        // Shift up by 2: rows [0, 1] deleted, row 2 moves to row 0
        extras.shift_region_up_by(0, 9, 2);
        assert_eq!(extras.len(), 1);
        assert_eq!(fg_at(&extras, 0, 0), Some([30, 0, 0]));
    }

    #[test]
    fn test_shift_region_up_preserves_outside_region() {
        let mut extras = CellExtras::new();
        // Entry below region
        insert(&mut extras, 20, 5, extra_with_fg(99, 0, 0));
        // Entry inside region
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));

        // Region [2, 10], shift up by 2: row 5 -> row 3, row 20 untouched
        extras.shift_region_up_by(2, 10, 2);
        assert_eq!(fg_at(&extras, 3, 0), Some([50, 0, 0]));
        assert_eq!(fg_at(&extras, 20, 5), Some([99, 0, 0]));
    }

    #[test]
    fn test_shift_region_up_preserves_above_region() {
        let mut extras = CellExtras::new();
        // Entry above region
        insert(&mut extras, 0, 0, extra_with_fg(1, 2, 3));
        // Entry at top of region (will be deleted)
        insert(&mut extras, 5, 0, extra_with_fg(4, 5, 6));
        // Entry shifted within region
        insert(&mut extras, 7, 0, extra_with_fg(7, 8, 9));

        // Region [5, 20], shift up by 2: row 5 deleted, row 7 -> 5
        extras.shift_region_up_by(5, 20, 2);
        assert_eq!(
            fg_at(&extras, 0, 0),
            Some([1, 2, 3]),
            "above region preserved"
        );
        assert!(!has_fg_at(&extras, 5, 0) || fg_at(&extras, 5, 0) == Some([7, 8, 9]));
        assert_eq!(
            fg_at(&extras, 5, 0),
            Some([7, 8, 9]),
            "row 7 shifted to row 5"
        );
    }

    #[test]
    fn test_shift_region_up_full_screen() {
        let mut extras = CellExtras::new();
        for r in 0..5u16 {
            insert(&mut extras, r, 0, extra_with_fg(r as u8, 0, 0));
        }
        // Full-screen region [0, 23], shift up by 3: rows 0-2 deleted, rows 3-4 -> 0-1
        extras.shift_region_up_by(0, 23, 3);
        assert_eq!(extras.len(), 2);
        assert_eq!(fg_at(&extras, 0, 0), Some([3, 0, 0]));
        assert_eq!(fg_at(&extras, 1, 0), Some([4, 0, 0]));
    }

    #[test]
    fn test_shift_region_up_with_accumulated_offset() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));
        insert(&mut extras, 10, 0, extra_with_fg(100, 0, 0));

        // Simulate prior full-screen scrolls to accumulate offset
        extras.shift_rows_up_by(0, 2);
        // Now external row 5 means what was row 7, etc.
        // Insert at new external row 3 (internal = 3 + offset)
        insert(&mut extras, 3, 1, extra_with_fg(30, 0, 0));

        // Region shift should handle the offset correctly
        extras.shift_region_up_by(0, 23, 1);
        // Entry at external row 3 should move to 2
        assert_eq!(fg_at(&extras, 2, 1), Some([30, 0, 0]));
    }

    // =========================================================================
    // shift_region_down_by
    // =========================================================================

    #[test]
    fn test_shift_region_down_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_region_down_by(0, 23, 1);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_region_down_zero_n_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(255, 0, 0));
        extras.shift_region_down_by(0, 23, 0);
        assert!(has_fg_at(&extras, 5, 0), "n=0 should not move entries");
    }

    #[test]
    fn test_shift_region_down_drops_bottom_rows() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 8, 0, extra_with_fg(80, 0, 0));
        insert(&mut extras, 9, 0, extra_with_fg(90, 0, 0));
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));

        // Region [0, 9], shift down by 2: rows [8, 9] dropped, row 5 -> 7
        extras.shift_region_down_by(0, 9, 2);
        assert_eq!(fg_at(&extras, 7, 0), Some([50, 0, 0]));
        assert!(!has_fg_at(&extras, 8, 0), "bottom rows should be dropped");
        assert!(!has_fg_at(&extras, 9, 0), "bottom rows should be dropped");
    }

    #[test]
    fn test_shift_region_down_preserves_outside_region() {
        let mut extras = CellExtras::new();
        // Entry below region
        insert(&mut extras, 20, 0, extra_with_fg(200, 0, 0));
        // Entry inside region
        insert(&mut extras, 3, 0, extra_with_fg(30, 0, 0));

        // Region [0, 10], shift down by 2
        extras.shift_region_down_by(0, 10, 2);
        assert_eq!(
            fg_at(&extras, 20, 0),
            Some([200, 0, 0]),
            "outside preserved"
        );
        assert_eq!(fg_at(&extras, 5, 0), Some([30, 0, 0]), "row 3 shifted to 5");
    }

    #[test]
    fn test_shift_region_down_single_row_shift() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 0, extra_with_fg(1, 0, 0));
        insert(&mut extras, 9, 0, extra_with_fg(9, 0, 0));

        // Region [0, 9], shift down by 1: row 9 dropped, row 0 -> 1
        extras.shift_region_down_by(0, 9, 1);
        assert_eq!(fg_at(&extras, 1, 0), Some([1, 0, 0]));
        assert!(!has_fg_at(&extras, 9, 0) || fg_at(&extras, 9, 0) != Some([9, 0, 0]));
    }

    // =========================================================================
    // shift_rect_up_by
    // =========================================================================

    #[test]
    fn test_shift_rect_up_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_rect_up_by(0, 9, 5, 15, 2);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_rect_up_zero_n_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 3, 10, extra_with_fg(30, 0, 0));
        extras.shift_rect_up_by(0, 9, 5, 15, 0);
        assert!(has_fg_at(&extras, 3, 10));
    }

    #[test]
    fn test_shift_rect_up_only_affects_column_range() {
        let mut extras = CellExtras::new();
        // Inside rect column range
        insert(&mut extras, 4, 10, extra_with_fg(40, 10, 0));
        // Outside rect column range (same row, col < left)
        insert(&mut extras, 4, 2, extra_with_fg(40, 2, 0));
        // Outside rect column range (same row, col > right)
        insert(&mut extras, 4, 20, extra_with_fg(40, 20, 0));

        // Rect [2, 8] x [5, 15], shift up by 2: row 4, col 10 is inside
        extras.shift_rect_up_by(2, 8, 5, 15, 2);
        assert_eq!(
            fg_at(&extras, 2, 10),
            Some([40, 10, 0]),
            "inside rect shifted up"
        );
        assert_eq!(
            fg_at(&extras, 4, 2),
            Some([40, 2, 0]),
            "outside col range preserved"
        );
        assert_eq!(
            fg_at(&extras, 4, 20),
            Some([40, 20, 0]),
            "outside col range preserved"
        );
    }

    #[test]
    fn test_shift_rect_up_deletes_top_rows_in_rect() {
        let mut extras = CellExtras::new();
        // Row at top of rect (will be deleted)
        insert(&mut extras, 2, 10, extra_with_fg(20, 0, 0));
        // Row below deleted range
        insert(&mut extras, 5, 10, extra_with_fg(50, 0, 0));

        // Rect [2, 8] x [5, 15], shift up by 2: row 2 in [2, 3] deleted, row 5 -> 3
        extras.shift_rect_up_by(2, 8, 5, 15, 2);
        assert!(
            !has_fg_at(&extras, 2, 10),
            "top row in rect should be deleted"
        );
        assert_eq!(
            fg_at(&extras, 3, 10),
            Some([50, 0, 0]),
            "shifted up within rect"
        );
    }

    #[test]
    fn test_shift_rect_up_preserves_outside_row_range() {
        let mut extras = CellExtras::new();
        // Entry above rect row range
        insert(&mut extras, 0, 10, extra_with_fg(1, 0, 0));
        // Entry below rect row range
        insert(&mut extras, 15, 10, extra_with_fg(150, 0, 0));

        extras.shift_rect_up_by(2, 8, 5, 15, 1);
        assert_eq!(fg_at(&extras, 0, 10), Some([1, 0, 0]));
        assert_eq!(fg_at(&extras, 15, 10), Some([150, 0, 0]));
    }

    // =========================================================================
    // shift_rect_down_by
    // =========================================================================

    #[test]
    fn test_shift_rect_down_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_rect_down_by(0, 9, 5, 15, 2);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_rect_down_zero_n_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 3, 10, extra_with_fg(30, 0, 0));
        extras.shift_rect_down_by(0, 9, 5, 15, 0);
        assert!(has_fg_at(&extras, 3, 10));
    }

    #[test]
    fn test_shift_rect_down_only_affects_column_range() {
        let mut extras = CellExtras::new();
        // Inside rect column range
        insert(&mut extras, 3, 10, extra_with_fg(30, 10, 0));
        // Outside rect column range (col < left)
        insert(&mut extras, 3, 2, extra_with_fg(30, 2, 0));
        // Outside rect column range (col > right)
        insert(&mut extras, 3, 20, extra_with_fg(30, 20, 0));

        // Rect [2, 8] x [5, 15], shift down by 2
        extras.shift_rect_down_by(2, 8, 5, 15, 2);
        assert_eq!(
            fg_at(&extras, 5, 10),
            Some([30, 10, 0]),
            "inside rect shifted down"
        );
        assert_eq!(
            fg_at(&extras, 3, 2),
            Some([30, 2, 0]),
            "outside col range preserved"
        );
        assert_eq!(
            fg_at(&extras, 3, 20),
            Some([30, 20, 0]),
            "outside col range preserved"
        );
    }

    #[test]
    fn test_shift_rect_down_drops_bottom_rows_in_rect() {
        let mut extras = CellExtras::new();
        // Row near bottom (will be dropped when shifted into drop zone)
        insert(&mut extras, 8, 10, extra_with_fg(80, 0, 0));
        // Row at top of rect (will shift down)
        insert(&mut extras, 2, 10, extra_with_fg(20, 0, 0));

        // Rect [2, 8] x [5, 15], shift down by 2: drop_start = 8 - (2-1) = 7
        // Row 8 >= drop_start(7), so dropped. Row 2 < 7, so shifted to 4.
        extras.shift_rect_down_by(2, 8, 5, 15, 2);
        assert!(!has_fg_at(&extras, 8, 10), "bottom row in rect dropped");
        assert_eq!(
            fg_at(&extras, 4, 10),
            Some([20, 0, 0]),
            "shifted down within rect"
        );
    }

    // =========================================================================
    // shift_cols_right (ICH)
    // =========================================================================

    #[test]
    fn test_shift_cols_right_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_cols_right(0, 5, 3, 80);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_cols_right_zero_count_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        extras.shift_cols_right(0, 5, 0, 80);
        assert!(has_fg_at(&extras, 0, 5));
    }

    #[test]
    fn test_shift_cols_right_basic() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        // Insert 3 chars at col 5: col 5 -> 8, col 10 -> 13
        extras.shift_cols_right(0, 5, 3, 80);
        assert_eq!(fg_at(&extras, 0, 8), Some([50, 0, 0]));
        assert_eq!(fg_at(&extras, 0, 13), Some([100, 0, 0]));
        assert!(!has_fg_at(&extras, 0, 5), "old position should be empty");
    }

    #[test]
    fn test_shift_cols_right_drops_past_max_col() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 78, extra_with_fg(78, 0, 0));

        // Shift right by 5, max_col=80: col 78 + 5 = 83 >= 80, dropped
        extras.shift_cols_right(0, 0, 5, 80);
        assert!(
            !has_fg_at(&extras, 0, 78),
            "shifted past max_col should be dropped"
        );
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_cols_right_preserves_cols_before_start() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 2, extra_with_fg(20, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        // Shift right from col 5: col 2 untouched, col 10 -> 13
        extras.shift_cols_right(0, 5, 3, 80);
        assert_eq!(
            fg_at(&extras, 0, 2),
            Some([20, 0, 0]),
            "below start preserved"
        );
        assert_eq!(
            fg_at(&extras, 0, 13),
            Some([100, 0, 0]),
            "at/above start shifted"
        );
    }

    #[test]
    fn test_shift_cols_right_preserves_beyond_max_col() {
        let mut extras = CellExtras::new();
        // Entry at col 70 which is >= max_col (50) should be preserved in place.
        // Repro for DECLRMM: right margin at col 49 (max_col=50), an extra at
        // col 70 must NOT be extracted-then-dropped by the shift.
        insert(&mut extras, 0, 70, extra_with_fg(70, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        // ICH 3 at col 5, max_col=50: col 10 -> 13, col 70 untouched.
        extras.shift_cols_right(0, 5, 3, 50);
        assert_eq!(
            fg_at(&extras, 0, 70),
            Some([70, 0, 0]),
            "beyond max_col preserved"
        );
        assert_eq!(fg_at(&extras, 0, 13), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_right_other_rows_untouched() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        insert(&mut extras, 1, 5, extra_with_fg(15, 0, 0));

        // Shift right on row 0 only
        extras.shift_cols_right(0, 5, 3, 80);
        assert_eq!(fg_at(&extras, 0, 8), Some([50, 0, 0]), "target row shifted");
        assert_eq!(
            fg_at(&extras, 1, 5),
            Some([15, 0, 0]),
            "other row untouched"
        );
    }

    // =========================================================================
    // shift_cols_left (DCH)
    // =========================================================================

    #[test]
    fn test_shift_cols_left_empty_collection() {
        let mut extras = CellExtras::new();
        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_cols_left_zero_count_is_noop() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        extras.shift_cols_left(0, 5, 0, 80);
        assert!(has_fg_at(&extras, 0, 5));
    }

    #[test]
    fn test_shift_cols_left_basic() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        // Delete 3 chars at col 5: col 5 in [5, 8) deleted. col 10 >= 8, shift left by 3 -> 7
        extras.shift_cols_left(0, 5, 3, 80);
        assert!(!has_fg_at(&extras, 0, 5), "deleted column should be gone");
        assert_eq!(fg_at(&extras, 0, 7), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_left_deletes_range() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        insert(&mut extras, 0, 6, extra_with_fg(60, 0, 0));
        insert(&mut extras, 0, 7, extra_with_fg(70, 0, 0));

        // Delete 3 at col 5: cols [5, 6, 7] all deleted
        extras.shift_cols_left(0, 5, 3, 80);
        assert!(!has_fg_at(&extras, 0, 5));
        assert!(!has_fg_at(&extras, 0, 6));
        assert!(!has_fg_at(&extras, 0, 7));
        assert_eq!(extras.len(), 0);
    }

    #[test]
    fn test_shift_cols_left_preserves_cols_before_start() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 2, extra_with_fg(20, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        // Delete at col 5, count 3: col 2 untouched, col 10 -> 7
        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(
            fg_at(&extras, 0, 2),
            Some([20, 0, 0]),
            "before start preserved"
        );
        assert_eq!(fg_at(&extras, 0, 7), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_left_other_rows_untouched() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 10, extra_with_fg(10, 0, 0));
        insert(&mut extras, 2, 10, extra_with_fg(210, 0, 0));

        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(fg_at(&extras, 0, 7), Some([10, 0, 0]), "target row shifted");
        assert_eq!(
            fg_at(&extras, 2, 10),
            Some([210, 0, 0]),
            "other row untouched"
        );
    }

    #[test]
    fn test_shift_cols_left_preserves_beyond_max_col() {
        let mut extras = CellExtras::new();
        // Entry at col 85 which is >= max_col (80) should be preserved in place
        insert(&mut extras, 0, 85, extra_with_fg(85, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(
            fg_at(&extras, 0, 85),
            Some([85, 0, 0]),
            "beyond max_col preserved"
        );
        assert_eq!(fg_at(&extras, 0, 7), Some([100, 0, 0]));
    }

    // =========================================================================
    // Edge cases: boundary coordinates
    // =========================================================================

    #[test]
    fn test_shift_region_up_boundary_at_top_equals_n() {
        // When top + n > bottom, all entries in region should be deleted
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));
        insert(&mut extras, 6, 0, extra_with_fg(60, 0, 0));

        // Region [5, 6], shift up by 2: both rows are in [top, top + n) so deleted
        extras.shift_region_up_by(5, 6, 2);
        assert_eq!(
            extras.len(),
            0,
            "all entries in region deleted when n covers whole region"
        );
    }

    #[test]
    fn test_shift_region_down_n_equals_region_size() {
        // When n equals (bottom - top + 1), all entries should be dropped
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 0, extra_with_fg(1, 0, 0));
        insert(&mut extras, 1, 0, extra_with_fg(2, 0, 0));
        insert(&mut extras, 2, 0, extra_with_fg(3, 0, 0));

        // Region [0, 2], shift down by 3: drop_start = 2 - (3-1) = 0
        // All rows >= 0 within region are in drop zone
        extras.shift_region_down_by(0, 2, 3);
        assert_eq!(extras.len(), 0, "all dropped when n covers whole region");
    }

    #[test]
    fn test_shift_region_up_single_entry_at_boundary() {
        let mut extras = CellExtras::new();
        // Entry exactly at bottom of region
        insert(&mut extras, 10, 0, extra_with_fg(100, 0, 0));

        // Region [5, 10], shift up by 3: row 10 >= shift_start(8), so shifted to 7
        extras.shift_region_up_by(5, 10, 3);
        assert_eq!(fg_at(&extras, 7, 0), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_right_at_max_col_boundary() {
        let mut extras = CellExtras::new();
        // Entry at max_col - 1
        insert(&mut extras, 0, 79, extra_with_fg(79, 0, 0));

        // Shift right by 1 from col 79: 79 + 1 = 80, but max_col is 80,
        // so 80 < 80 is false => dropped
        extras.shift_cols_right(0, 79, 1, 80);
        assert_eq!(
            extras.len(),
            0,
            "entry at max_col-1 dropped when shifted by 1"
        );
    }

    #[test]
    fn test_shift_cols_left_delete_everything_on_row() {
        let mut extras = CellExtras::new();
        for col in 0..10u16 {
            insert(&mut extras, 0, col, extra_with_fg(col as u8, 0, 0));
        }

        // Delete 10 at col 0: everything in [0, 10) deleted
        extras.shift_cols_left(0, 0, 10, 80);
        // Only row 0 entries existed, all in delete range
        assert_eq!(extras.len(), 0);
    }

    // =========================================================================
    // Multi-column entries and data preservation
    // =========================================================================

    #[test]
    fn test_shift_region_up_preserves_extra_data() {
        let mut extras = CellExtras::new();
        insert(
            &mut extras,
            5,
            3,
            extra_with_hyperlink("https://example.com"),
        );

        // Region [0, 23], shift up by 3
        extras.shift_region_up_by(0, 23, 3);
        let entry = extras.get(CellCoord::new(2, 3));
        assert!(entry.is_some(), "shifted entry should exist");
        let url = entry.unwrap().hyperlink().unwrap();
        assert_eq!(&**url, "https://example.com", "hyperlink data preserved");
    }

    #[test]
    fn test_shift_cols_right_preserves_extra_data() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_hyperlink("https://test.dev"));

        extras.shift_cols_right(0, 5, 2, 80);
        let entry = extras.get(CellCoord::new(0, 7));
        assert!(entry.is_some());
        let url = entry.unwrap().hyperlink().unwrap();
        assert_eq!(&**url, "https://test.dev");
    }

    // =========================================================================
    // Scroll region (DECSTBM) simulation
    // =========================================================================

    #[test]
    fn test_decstbm_scroll_up_within_margins() {
        // Simulate DECSTBM scroll region [5, 15] scrolling up by 1
        let mut extras = CellExtras::new();
        // Entries at various positions
        insert(&mut extras, 3, 0, extra_with_fg(30, 0, 0)); // above region
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0)); // at top (will be deleted)
        insert(&mut extras, 10, 0, extra_with_fg(100, 0, 0)); // middle
        insert(&mut extras, 15, 0, extra_with_fg(150, 0, 0)); // at bottom
        insert(&mut extras, 20, 0, extra_with_fg(200, 0, 0)); // below region

        extras.shift_region_up_by(5, 15, 1);

        assert_eq!(
            fg_at(&extras, 3, 0),
            Some([30, 0, 0]),
            "above region untouched"
        );
        assert!(
            !has_fg_at(&extras, 5, 0) || fg_at(&extras, 5, 0) != Some([50, 0, 0]),
            "top row should be gone or overwritten"
        );
        assert_eq!(fg_at(&extras, 9, 0), Some([100, 0, 0]), "row 10 -> 9");
        assert_eq!(fg_at(&extras, 14, 0), Some([150, 0, 0]), "row 15 -> 14");
        assert_eq!(
            fg_at(&extras, 20, 0),
            Some([200, 0, 0]),
            "below region untouched"
        );
    }

    #[test]
    fn test_decstbm_scroll_down_within_margins() {
        // Simulate DECSTBM scroll region [5, 15] scrolling down by 1
        let mut extras = CellExtras::new();
        insert(&mut extras, 3, 0, extra_with_fg(30, 0, 0)); // above region
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0)); // at top
        insert(&mut extras, 10, 0, extra_with_fg(100, 0, 0)); // middle
        insert(&mut extras, 15, 0, extra_with_fg(150, 0, 0)); // at bottom (will be dropped)
        insert(&mut extras, 20, 0, extra_with_fg(200, 0, 0)); // below region

        extras.shift_region_down_by(5, 15, 1);

        assert_eq!(
            fg_at(&extras, 3, 0),
            Some([30, 0, 0]),
            "above region untouched"
        );
        assert_eq!(fg_at(&extras, 6, 0), Some([50, 0, 0]), "row 5 -> 6");
        assert_eq!(fg_at(&extras, 11, 0), Some([100, 0, 0]), "row 10 -> 11");
        assert!(
            !has_fg_at(&extras, 15, 0) || fg_at(&extras, 15, 0) != Some([150, 0, 0]),
            "bottom row should be gone or overwritten"
        );
        assert_eq!(
            fg_at(&extras, 20, 0),
            Some([200, 0, 0]),
            "below region untouched"
        );
    }

    // =========================================================================
    // DECLRMM rectangular scroll simulation
    // =========================================================================

    #[test]
    fn test_declrmm_rect_scroll_up() {
        // Simulate left-right margin scroll: rect [2, 8] x [10, 30]
        let mut extras = CellExtras::new();
        insert(&mut extras, 2, 15, extra_with_fg(21, 5, 0)); // top row, in col range (deleted)
        insert(&mut extras, 5, 15, extra_with_fg(51, 5, 0)); // mid, in col range (shifted)
        insert(&mut extras, 5, 5, extra_with_fg(55, 0, 0)); // mid, outside col range (preserved)

        extras.shift_rect_up_by(2, 8, 10, 30, 1);
        assert!(!has_fg_at(&extras, 2, 15), "top row in rect deleted");
        assert_eq!(
            fg_at(&extras, 4, 15),
            Some([51, 5, 0]),
            "row 5 -> 4 in rect"
        );
        assert_eq!(
            fg_at(&extras, 5, 5),
            Some([55, 0, 0]),
            "outside col range preserved"
        );
    }

    #[test]
    fn test_declrmm_rect_scroll_down() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 2, 15, extra_with_fg(21, 50, 0)); // top, in rect (shifted)
        insert(&mut extras, 8, 15, extra_with_fg(81, 50, 0)); // bottom, in rect (dropped)
        insert(&mut extras, 5, 5, extra_with_fg(55, 0, 0)); // outside col range (preserved)

        extras.shift_rect_down_by(2, 8, 10, 30, 1);
        assert_eq!(
            fg_at(&extras, 3, 15),
            Some([21, 50, 0]),
            "row 2 -> 3 in rect"
        );
        assert!(
            !has_fg_at(&extras, 8, 15) || fg_at(&extras, 8, 15) != Some([81, 50, 0]),
            "bottom row in rect dropped"
        );
        assert_eq!(
            fg_at(&extras, 5, 5),
            Some([55, 0, 0]),
            "outside col range preserved"
        );
    }

    // =========================================================================
    // Multiple entries, same row different cols
    // =========================================================================

    // =========================================================================
    // RGB-RING shifts (#7458 follow-up): truecolor lives ONLY in the dense ring
    // (set via set_rgb_ring_range), never spilled to the HashMap. The shift ops
    // must move/clear only the affected ring cells and PRESERVE everyone else.
    // =========================================================================

    /// Helper: write a bg RGB into the dense ring at (row, col).
    fn set_ring_bg(extras: &mut CellExtras, row: u16, col: u16, rgb: [u8; 3]) {
        extras.set_rgb_ring_range(row, col, col + 1, None, Some(rgb), 24, 80);
    }

    #[test]
    fn test_region_up_preserves_out_of_region_ring_bg() {
        // The exact repro class for ORACLE BUG A: a truecolor cell ABOVE the
        // scroll region must keep its bg after an in-region SU.
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 0, 0, [75, 255, 115]); // above region
        set_ring_bg(&mut extras, 14, 3, [1, 2, 3]); // inside region

        // Region rows [12, 17], scroll up by 4.
        extras.shift_region_up_by(12, 17, 4);

        assert_eq!(
            extras.bg_rgb_for(0, 0),
            Some([75, 255, 115]),
            "out-of-region truecolor bg must survive the in-region scroll",
        );
    }

    #[test]
    fn test_region_up_shifts_in_region_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 7, 0, [10, 20, 30]); // inside, below deleted rows
        // Region [5, 20], up by 2: row 7 -> row 5.
        extras.shift_region_up_by(5, 20, 2);
        assert_eq!(extras.bg_rgb_for(5, 0), Some([10, 20, 30]), "row 7 -> 5");
        assert_eq!(extras.bg_rgb_for(7, 0), None, "old position cleared/moved");
    }

    #[test]
    fn test_region_down_shifts_and_preserves_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 22, 0, [9, 9, 9]); // below region (preserved)
        set_ring_bg(&mut extras, 3, 1, [4, 5, 6]); // inside region
        // Region [0, 10], down by 2: row 3 -> 5.
        extras.shift_region_down_by(0, 10, 2);
        assert_eq!(extras.bg_rgb_for(5, 1), Some([4, 5, 6]), "row 3 -> 5");
        assert_eq!(extras.bg_rgb_for(22, 0), Some([9, 9, 9]), "below preserved");
    }

    #[test]
    fn test_cols_right_shifts_and_preserves_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 0, 5, [50, 0, 0]);
        set_ring_bg(&mut extras, 0, 10, [100, 0, 0]);
        set_ring_bg(&mut extras, 1, 5, [15, 0, 0]); // other row untouched
        // ICH 3 at col 5: col 5 -> 8, col 10 -> 13.
        extras.shift_cols_right(0, 5, 3, 80);
        assert_eq!(extras.bg_rgb_for(0, 8), Some([50, 0, 0]));
        assert_eq!(extras.bg_rgb_for(0, 13), Some([100, 0, 0]));
        assert_eq!(extras.bg_rgb_for(0, 5), None, "inserted blank");
        assert_eq!(extras.bg_rgb_for(1, 5), Some([15, 0, 0]), "other row kept");
    }

    #[test]
    fn test_cols_left_shifts_and_preserves_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 0, 5, [50, 0, 0]); // deleted
        set_ring_bg(&mut extras, 0, 10, [100, 0, 0]); // -> 7
        set_ring_bg(&mut extras, 2, 10, [210, 0, 0]); // other row untouched
        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(extras.bg_rgb_for(0, 5), None, "deleted col blank");
        assert_eq!(extras.bg_rgb_for(0, 7), Some([100, 0, 0]), "col 10 -> 7");
        assert_eq!(
            extras.bg_rgb_for(2, 10),
            Some([210, 0, 0]),
            "other row kept"
        );
    }

    #[test]
    fn test_rect_up_only_affects_rect_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 5, 12, [40, 10, 0]); // inside rect
        set_ring_bg(&mut extras, 5, 2, [40, 2, 0]); // outside col range
        // Rect [2,8] x [10,30], up by 2: (5,12) -> (3,12); (5,2) untouched.
        extras.shift_rect_up_by(2, 8, 10, 30, 2);
        assert_eq!(
            extras.bg_rgb_for(3, 12),
            Some([40, 10, 0]),
            "in-rect shifted"
        );
        assert_eq!(
            extras.bg_rgb_for(5, 2),
            Some([40, 2, 0]),
            "outside col kept"
        );
    }

    #[test]
    fn test_shift_region_up_multiple_cols_same_row() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));
        insert(&mut extras, 5, 10, extra_with_fg(50, 10, 0));
        insert(&mut extras, 5, 79, extra_with_fg(50, 79, 0));

        // Region [0, 23], shift up by 3: row 5 -> 2, all cols preserved
        extras.shift_region_up_by(0, 23, 3);
        assert_eq!(fg_at(&extras, 2, 0), Some([50, 0, 0]));
        assert_eq!(fg_at(&extras, 2, 10), Some([50, 10, 0]));
        assert_eq!(fg_at(&extras, 2, 79), Some([50, 79, 0]));
    }
}
