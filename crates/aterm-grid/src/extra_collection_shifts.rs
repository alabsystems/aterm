// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Region and column shifts for [`CellExtras`]: scroll-region shifts and ICH/DCH
//! column shifts. Each touches a subset of rows, so each drain-rebuilds (O(E)).

use crate::extra_map::ExtraMap;

use super::extra_collection::CellExtras;
use crate::extra::{CellCoord, CellExtra};

impl CellExtras {
    /// Shifts rows up within `[top, bottom]` by `n`, dropping `[top, top + n)` and compacting the row offset.
    #[inline]
    pub(crate) fn shift_region_up_by(&mut self, top: u16, bottom: u16, n: u16) {
        // A scroll region shifts every line feed; with no map entry and no ring there is nothing
        // to move, and the call stays inline.
        if self.data.is_empty() && !self.has_rings() {
            return;
        }
        self.shift_region_up_by_held(top, bottom, n);
    }

    fn shift_region_up_by_held(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rings_region_up(top, bottom, n);
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
        let shift_start = top.checked_add(n);
        let mut new_data = ExtraMap::with_capacity(self.data.len());
        let mut max_row: Option<u16> = None;
        for (coord, extra) in self.data.drain() {
            // Internal row < offset: stale, drop.
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom {
                max_row = Some(max_row.map_or(ext_row, |m| m.max(ext_row)));
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                let dst_row = ext_row - n;
                max_row = Some(max_row.map_or(dst_row, |m| m.max(dst_row)));
                new_data.insert(CellCoord::new(dst_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.max_row_cache.set_exact_opt(max_row);
        self.enforce_hyperlink_limit();
    }

    /// Shifts rows down within `[top, bottom]` by `n`, dropping `[bottom - n + 1, bottom]` and compacting the row offset.
    #[inline]
    pub(crate) fn shift_region_down_by(&mut self, top: u16, bottom: u16, n: u16) {
        if self.data.is_empty() && !self.has_rings() {
            return;
        }
        self.shift_region_down_by_held(top, bottom, n);
    }

    fn shift_region_down_by_held(&mut self, top: u16, bottom: u16, n: u16) {
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
        let mut new_data = ExtraMap::with_capacity(self.data.len());
        let mut max_row: Option<u16> = None;
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row >= top && ext_row <= bottom {
                if ext_row < drop_start {
                    let dst_row = ext_row.saturating_add(n);
                    max_row = Some(max_row.map_or(dst_row, |m| m.max(dst_row)));
                    new_data.insert(CellCoord::new(dst_row, coord.col), extra);
                }
            } else {
                max_row = Some(max_row.map_or(ext_row, |m| m.max(ext_row)));
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.max_row_cache.set_exact_opt(max_row);
        self.enforce_hyperlink_limit();
    }

    /// Shifts extras up by `n` within `[top, bottom] × [left, right]` (DECLRMM scroll and DL).
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
        let mut new_data = ExtraMap::with_capacity(self.data.len());
        let mut max_row: Option<u16> = None;
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                max_row = Some(max_row.map_or(ext_row, |m| m.max(ext_row)));
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                let dst_row = ext_row - n;
                max_row = Some(max_row.map_or(dst_row, |m| m.max(dst_row)));
                new_data.insert(CellCoord::new(dst_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.max_row_cache.set_exact_opt(max_row);
        self.enforce_hyperlink_limit();
    }

    /// Shifts extras down by `n` within `[top, bottom] × [left, right]` (DECLRMM scroll and IL).
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
        let mut new_data = ExtraMap::with_capacity(self.data.len());
        let mut max_row: Option<u16> = None;
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                max_row = Some(max_row.map_or(ext_row, |m| m.max(ext_row)));
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if ext_row < drop_start {
                let dst_row = ext_row.saturating_add(n);
                max_row = Some(max_row.map_or(dst_row, |m| m.max(dst_row)));
                new_data.insert(CellCoord::new(dst_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.max_row_cache.set_exact_opt(max_row);
        self.enforce_hyperlink_limit();
    }

    /// Removes and returns all `(col, extra)` entries on `internal_row` in `[start_col, max_col)`.
    fn take_row_range(
        &mut self,
        internal_row: u16,
        start_col: u16,
        max_col: u16,
    ) -> aterm_alloc::SmallVec<(u16, CellExtra), 8> {
        self.settle_image_candidate();
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

    /// Shifts columns in `[start_col, max_col)` right by `count` on `row` for ICH, dropping entries pushed past `max_col`.
    pub fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_right(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        let Some(internal_row) = self.internal_row_checked(row) else {
            return;
        };
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        let had_any = !extracted.is_empty();
        let mut reinserted = false;
        for (col, extra) in extracted {
            let new_col = col.saturating_add(count);
            if new_col < max_col {
                self.data
                    .insert(CellCoord::new(internal_row, new_col), extra);
                reinserted = true;
            }
        }
        if had_any && !reinserted {
            self.max_row_cache
                .on_remove(internal_row, self.data.is_empty());
        }
        self.maybe_shrink();
    }

    /// Shifts columns in `[start_col + count, max_col)` left by `count` on `row` for DCH, deleting `[start_col, start_col + count)`.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_left(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        let Some(internal_row) = self.internal_row_checked(row) else {
            return;
        };
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let shift_start = start_col.saturating_add(count);
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        let had_any = !extracted.is_empty();
        let mut reinserted = false;
        for (col, extra) in extracted {
            if col >= shift_start {
                self.data
                    .insert(CellCoord::new(internal_row, col - count), extra);
                reinserted = true;
            }
        }
        if had_any && !reinserted {
            self.max_row_cache
                .on_remove(internal_row, self.data.is_empty());
        }
        self.maybe_shrink();
    }
}

#[cfg(test)]
#[path = "extra_collection_shifts_tests.rs"]
mod tests;
