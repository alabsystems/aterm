// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Region and column shifts for [`CellExtras`]: scroll-region shifts and ICH/DCH
//! column shifts. Each touches a subset of rows, so each drain-rebuilds (O(E)).

use aterm_hash::FxHashMap;

use super::extra_collection::CellExtras;
use crate::extra::{CellCoord, CellExtra};

impl CellExtras {
    /// Shift rows up within `[top, bottom]` by `n`: rows `[top, top + n)` drop,
    /// `[top + n, bottom]` shift up, rows outside stay. One drain-rebuild (O(E)
    /// for any `n`) that also compacts the row offset.
    pub(crate) fn shift_region_up_by(&mut self, top: u16, bottom: u16, n: u16) {
        // The dense rings shift the same way (in every shift below): ring-only
        // entries never reach the map, so a wholesale wipe would drop truecolor
        // and complex chars outside the region.
        self.shift_rings_region_up(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        // No pending offset and no entry inside `[top, bottom]`: the rebuild
        // would be an identity copy.
        let offset = self.take_row_offset();
        if offset == 0
            && !self
                .data
                .keys()
                .any(|coord| (top..=bottom).contains(&coord.row))
        {
            return;
        }
        let shift_start = top.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            // Internal row < offset: stale, drop.
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift rows down within `[top, bottom]` by `n`: rows `[bottom-n+1, bottom]`
    /// drop, `[top, bottom-n]` shift down. One drain-rebuild that also compacts
    /// the row offset.
    pub(crate) fn shift_region_down_by(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rings_region_down(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        if offset == 0
            && !self
                .data
                .keys()
                .any(|coord| (top..=bottom).contains(&coord.row))
        {
            return;
        }
        let drop_start = bottom.saturating_sub(n.saturating_sub(1));
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row >= top && ext_row <= bottom {
                if ext_row < drop_start {
                    // ext_row + n <= bottom, so this never saturates.
                    new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
                }
            } else {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift extras up within the rect `[top, bottom] × [left, right]` (DECLRMM
    /// scroll and DL): rows `[top, top + n)` drop, rows `[top + n, bottom]` move
    /// up by `n`, entries outside the rect stay.
    pub(crate) fn shift_rect_up_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        self.shift_rings_rect_up(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
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

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shift extras down within the rect `[top, bottom] × [left, right]`
    /// (DECLRMM scroll and IL): rows `[top, bottom - n]` move down by `n`, rows
    /// `[bottom - n + 1, bottom]` drop, entries outside the rect stay.
    pub(crate) fn shift_rect_down_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        self.shift_rings_rect_down(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
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

            if ext_row < drop_start {
                new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Extract every entry on `internal_row` in `[start_col, max_col)`, removing
    /// it from the map, and return the `(col, extra)` pairs.
    ///
    /// Probes the column range (O(width)) unless the map is smaller than the
    /// range, where a key scan is cheaper. The two branches extract in different
    /// orders, which is unobservable: the caller's reinsert is order-independent.
    fn take_row_range(
        &mut self,
        internal_row: u16,
        start_col: u16,
        max_col: u16,
    ) -> aterm_alloc::SmallVec<(u16, CellExtra), 8> {
        let mut extracted: aterm_alloc::SmallVec<(u16, CellExtra), 8> =
            aterm_alloc::SmallVec::new();
        let width = usize::from(max_col.saturating_sub(start_col));
        if width.saturating_mul(2) <= self.data.len() {
            for col in start_col..max_col {
                // `internal_row` is already offset-mapped.
                if let Some(extra) = self.data.remove(&CellCoord::new(internal_row, col)) {
                    extracted.push((col, extra));
                }
            }
        } else {
            let keys: aterm_alloc::SmallVec<CellCoord, 8> = self
                .data
                .keys()
                .filter(|c| c.row == internal_row && c.col >= start_col && c.col < max_col)
                .copied()
                .collect();
            for key in keys {
                if let Some(extra) = self.data.remove(&key) {
                    extracted.push((key.col, extra));
                }
            }
        }
        extracted
    }

    /// Shift columns right within a single row for ICH (Insert Character).
    ///
    /// Columns in `[start_col, max_col - count)` shift right by `count`;
    /// `[start_col, start_col + count)` are cleared; columns pushed past
    /// `max_col` drop. Only the target row's entries move.
    pub fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_right(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        // Extract all affected entries, then reinsert shifted, so a destination
        // cannot collide with a not-yet-processed source key. Entries at or past
        // max_col are outside the margin (DECLRMM) and stay in place.
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        for (col, extra) in extracted {
            let new_col = col.saturating_add(count);
            if new_col < max_col {
                self.data
                    .insert(CellCoord::new(internal_row, new_col), extra);
            }
        }
        self.maybe_shrink();
    }

    /// Shift columns left within a single row for DCH (Delete Character).
    ///
    /// Columns in `[start_col, start_col + count)` are deleted; columns in
    /// `[start_col + count, max_col)` shift left by `count`. Only the target
    /// row's entries move.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_left(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        let shift_start = start_col.saturating_add(count);
        // Extract then reinsert, as in shift_cols_right.
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        for (col, extra) in extracted {
            if col >= shift_start {
                self.data
                    .insert(CellCoord::new(internal_row, col - count), extra);
            }
        }
        self.maybe_shrink();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// An extra whose fg RGB makes `has_data()` true.
    fn extra_with_fg(r: u8, g: u8, b: u8) -> CellExtra {
        let mut e = CellExtra::default();
        e.set_fg_rgb(Some([r, g, b]));
        e
    }

    fn extra_with_hyperlink(url: &str) -> CellExtra {
        let mut e = CellExtra::default();
        e.set_hyperlink(Some(Arc::from(url)));
        e
    }

    fn insert(extras: &mut CellExtras, row: u16, col: u16, extra: CellExtra) {
        extras.set(CellCoord::new(row, col), extra);
    }

    fn has_fg_at(extras: &CellExtras, row: u16, col: u16) -> bool {
        extras
            .get(CellCoord::new(row, col))
            .and_then(|e| e.fg_rgb())
            .is_some()
    }

    fn fg_at(extras: &CellExtras, row: u16, col: u16) -> Option<[u8; 3]> {
        extras
            .get(CellCoord::new(row, col))
            .and_then(|e| e.fg_rgb())
    }

    // shift_region_up_by

    #[test]
    fn test_shift_region_up_full_screen() {
        let mut extras = CellExtras::new();
        for r in 0..5u16 {
            insert(&mut extras, r, 0, extra_with_fg(r as u8, 0, 0));
        }
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

        // Accumulate a row offset.
        extras.shift_rows_up_by(0, 2);
        // Then insert at external row 3 (internal 3 + offset).
        insert(&mut extras, 3, 1, extra_with_fg(30, 0, 0));

        extras.shift_region_up_by(0, 23, 1);
        assert_eq!(fg_at(&extras, 2, 1), Some([30, 0, 0]));
    }

    // shift_region_down_by

    #[test]
    fn test_shift_region_down_drops_bottom_rows() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 8, 0, extra_with_fg(80, 0, 0));
        insert(&mut extras, 9, 0, extra_with_fg(90, 0, 0));
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));

        extras.shift_region_down_by(0, 9, 2);
        assert_eq!(fg_at(&extras, 7, 0), Some([50, 0, 0]));
        assert!(!has_fg_at(&extras, 8, 0), "bottom rows should be dropped");
        assert!(!has_fg_at(&extras, 9, 0), "bottom rows should be dropped");
    }

    #[test]
    fn test_shift_region_down_single_row_shift() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 0, extra_with_fg(1, 0, 0));
        insert(&mut extras, 9, 0, extra_with_fg(9, 0, 0));

        extras.shift_region_down_by(0, 9, 1);
        assert_eq!(fg_at(&extras, 1, 0), Some([1, 0, 0]));
        assert!(!has_fg_at(&extras, 9, 0) || fg_at(&extras, 9, 0) != Some([9, 0, 0]));
    }

    // shift_rect_up_by

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
        insert(&mut extras, 4, 10, extra_with_fg(40, 10, 0));
        insert(&mut extras, 4, 2, extra_with_fg(40, 2, 0));
        insert(&mut extras, 4, 20, extra_with_fg(40, 20, 0));

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
        insert(&mut extras, 2, 10, extra_with_fg(20, 0, 0));
        insert(&mut extras, 5, 10, extra_with_fg(50, 0, 0));

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
        insert(&mut extras, 0, 10, extra_with_fg(1, 0, 0));
        insert(&mut extras, 15, 10, extra_with_fg(150, 0, 0));

        extras.shift_rect_up_by(2, 8, 5, 15, 1);
        assert_eq!(fg_at(&extras, 0, 10), Some([1, 0, 0]));
        assert_eq!(fg_at(&extras, 15, 10), Some([150, 0, 0]));
    }

    // shift_rect_down_by

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
        insert(&mut extras, 3, 10, extra_with_fg(30, 10, 0));
        insert(&mut extras, 3, 2, extra_with_fg(30, 2, 0));
        insert(&mut extras, 3, 20, extra_with_fg(30, 20, 0));

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
        insert(&mut extras, 8, 10, extra_with_fg(80, 0, 0));
        insert(&mut extras, 2, 10, extra_with_fg(20, 0, 0));

        extras.shift_rect_down_by(2, 8, 5, 15, 2);
        assert!(!has_fg_at(&extras, 8, 10), "bottom row in rect dropped");
        assert_eq!(
            fg_at(&extras, 4, 10),
            Some([20, 0, 0]),
            "shifted down within rect"
        );
    }

    // shift_cols_right (ICH)

    #[test]
    fn test_shift_cols_right_drops_past_max_col() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 78, extra_with_fg(78, 0, 0));

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
        // Col 70 is past a DECLRMM right margin at col 49.
        insert(&mut extras, 0, 70, extra_with_fg(70, 0, 0));
        insert(&mut extras, 0, 10, extra_with_fg(100, 0, 0));

        extras.shift_cols_right(0, 5, 3, 50);
        assert_eq!(
            fg_at(&extras, 0, 70),
            Some([70, 0, 0]),
            "beyond max_col preserved"
        );
        assert_eq!(fg_at(&extras, 0, 13), Some([100, 0, 0]));
    }

    // shift_cols_left (DCH)

    #[test]
    fn test_shift_cols_left_deletes_range() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 5, extra_with_fg(50, 0, 0));
        insert(&mut extras, 0, 6, extra_with_fg(60, 0, 0));
        insert(&mut extras, 0, 7, extra_with_fg(70, 0, 0));

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

        extras.shift_cols_left(0, 5, 3, 80);
        assert_eq!(
            fg_at(&extras, 0, 2),
            Some([20, 0, 0]),
            "before start preserved"
        );
        assert_eq!(fg_at(&extras, 0, 7), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_left_preserves_beyond_max_col() {
        let mut extras = CellExtras::new();
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

    // Boundary coordinates

    #[test]
    fn test_shift_region_up_boundary_at_top_equals_n() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 5, 0, extra_with_fg(50, 0, 0));
        insert(&mut extras, 6, 0, extra_with_fg(60, 0, 0));

        extras.shift_region_up_by(5, 6, 2);
        assert_eq!(
            extras.len(),
            0,
            "all entries in region deleted when n covers whole region"
        );
    }

    #[test]
    fn test_shift_region_down_n_equals_region_size() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 0, extra_with_fg(1, 0, 0));
        insert(&mut extras, 1, 0, extra_with_fg(2, 0, 0));
        insert(&mut extras, 2, 0, extra_with_fg(3, 0, 0));

        extras.shift_region_down_by(0, 2, 3);
        assert_eq!(extras.len(), 0, "all dropped when n covers whole region");
    }

    #[test]
    fn test_shift_region_up_single_entry_at_boundary() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 10, 0, extra_with_fg(100, 0, 0));

        extras.shift_region_up_by(5, 10, 3);
        assert_eq!(fg_at(&extras, 7, 0), Some([100, 0, 0]));
    }

    #[test]
    fn test_shift_cols_right_at_max_col_boundary() {
        let mut extras = CellExtras::new();
        insert(&mut extras, 0, 79, extra_with_fg(79, 0, 0));

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

        extras.shift_cols_left(0, 0, 10, 80);
        assert_eq!(extras.len(), 0);
    }

    // Data preservation

    #[test]
    fn test_shift_region_up_preserves_extra_data() {
        let mut extras = CellExtras::new();
        insert(
            &mut extras,
            5,
            3,
            extra_with_hyperlink("https://example.com"),
        );

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

    // Scroll region (DECSTBM)

    #[test]
    fn test_decstbm_scroll_up_within_margins() {
        let mut extras = CellExtras::new();
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

    // DECLRMM rectangular scroll

    #[test]
    fn test_declrmm_rect_scroll_up() {
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

    // RGB-ring shifts: `set_rgb_ring_range` truecolor lives only in the dense
    // ring; a shift moves or clears only the affected ring cells.

    /// Writes a bg RGB into the dense ring at (row, col).
    fn set_ring_bg(extras: &mut CellExtras, row: u16, col: u16, rgb: [u8; 3]) {
        extras.set_rgb_ring_range(row, col, col + 1, None, Some(rgb), 24, 80);
    }

    #[test]
    fn test_region_up_preserves_out_of_region_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 0, 0, [75, 255, 115]); // above region
        set_ring_bg(&mut extras, 14, 3, [1, 2, 3]); // inside region

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
        extras.shift_region_up_by(5, 20, 2);
        assert_eq!(extras.bg_rgb_for(5, 0), Some([10, 20, 30]), "row 7 -> 5");
        assert_eq!(extras.bg_rgb_for(7, 0), None, "old position cleared/moved");
    }

    #[test]
    fn test_region_down_shifts_and_preserves_ring_bg() {
        let mut extras = CellExtras::new();
        set_ring_bg(&mut extras, 22, 0, [9, 9, 9]); // below region (preserved)
        set_ring_bg(&mut extras, 3, 1, [4, 5, 6]); // inside region
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

        extras.shift_region_up_by(0, 23, 3);
        assert_eq!(fg_at(&extras, 2, 0), Some([50, 0, 0]));
        assert_eq!(fg_at(&extras, 2, 10), Some([50, 10, 0]));
        assert_eq!(fg_at(&extras, 2, 79), Some([50, 79, 0]));
    }

    #[test]
    fn test_shift_region_out_of_region_extras_skip_drain_rebuild() {
        let mut extras = CellExtras::new();
        insert(
            &mut extras,
            0,
            0,
            extra_with_hyperlink("https://status-top.example"),
        );
        insert(
            &mut extras,
            23,
            0,
            extra_with_hyperlink("https://status-bottom.example"),
        );

        // The fast path leaves the map untouched, so the entry keeps its address.
        let before_ptr = extras.get(CellCoord::new(0, 0)).unwrap() as *const CellExtra;
        extras.shift_region_up_by(2, 20, 3);
        let after_up_ptr = extras.get(CellCoord::new(0, 0)).unwrap() as *const CellExtra;
        assert_eq!(
            before_ptr, after_up_ptr,
            "up shift outside region must not reallocate"
        );

        extras.shift_region_down_by(2, 20, 3);
        let after_down_ptr = extras.get(CellCoord::new(0, 0)).unwrap() as *const CellExtra;
        assert_eq!(
            before_ptr, after_down_ptr,
            "down shift outside region must not reallocate"
        );
    }
}
