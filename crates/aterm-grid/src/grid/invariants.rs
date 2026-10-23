// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Debug-build checks that Grid state satisfies the formal spec's invariants.

use super::Grid;

impl Grid {
    /// Assert the grid's spec invariants (see the `assert_*` helpers).
    ///
    /// # Panics
    ///
    /// Panics if any invariant is violated.
    #[cfg(test)]
    #[inline]
    pub fn assert_invariants(&self) {
        #[cfg(debug_assertions)]
        {
            self.assert_cursor_in_bounds();
            self.assert_wide_char_consistent();
            self.assert_extras_claimed();
            self.assert_scroll_region_valid();
            self.assert_ring_buffer_valid();
            self.assert_keys_cover_retained_rows();
            self.storage.ring_extras.assert_running_sums();
            self.storage.lazy_buffer.assert_running_sums();
        }
    }

    /// Assert the O(1) structural grid invariants (`CursorInBounds`, `ScrollRegionValid`,
    /// `DisplayOffsetValid`, ring buffer, and `KeysCoverRetained`).
    #[cfg(any(debug_assertions, test))]
    pub fn assert_structural_invariants(&self) {
        self.assert_cursor_in_bounds();
        self.assert_scroll_region_valid();
        self.assert_ring_buffer_valid();
        self.assert_keys_cover_retained_rows();
    }

    /// KeysCoverRetained: `absolute_row_counter >= visible_rows +
    /// scrollback_lines()`, so the write and read maps of an absolute row name
    /// one row (see `Grid::cover_retained_rows`).
    #[cfg(any(debug_assertions, test))]
    fn assert_keys_cover_retained_rows(&self) {
        let retained =
            u64::from(self.storage.visible_rows).saturating_add(self.scrollback_lines() as u64);
        assert!(
            self.storage.absolute_row_counter >= retained,
            "KeysCoverRetained violated: absolute_row_counter {} < {retained} retained rows",
            self.storage.absolute_row_counter
        );
    }

    /// CursorInBounds: cursor.row < visible_rows && cursor.col < cols
    #[cfg(any(debug_assertions, test))]
    fn assert_cursor_in_bounds(&self) {
        assert!(
            self.storage.cursor.row < self.storage.visible_rows,
            "TLA+ CursorInBounds violated: cursor row {} >= visible_rows {}",
            self.storage.cursor.row,
            self.storage.visible_rows
        );
        assert!(
            self.storage.cursor.col < self.storage.cols,
            "TLA+ CursorInBounds violated: cursor col {} >= cols {}",
            self.storage.cursor.col,
            self.storage.cols
        );
    }

    /// Assert `WideCharConsistent` and `WideCharNotAtEnd` across all visible cells.
    ///
    /// # Panics
    ///
    /// Panics if a visible wide cell lacks its continuation or sits in the
    /// last column.
    #[cfg(any(debug_assertions, test))]
    pub fn assert_wide_char_consistent(&self) {
        let cols = usize::from(self.storage.cols);
        for row_idx in 0..self.storage.visible_rows {
            if let Some(row) = self.row(row_idx) {
                let cells = row.as_slice();
                let walked = &cells[..cells.len().min(cols)];
                let mut col = 0;
                while col + 1 < walked.len() {
                    if walked[col].is_wide() {
                        assert!(
                            walked[col + 1].is_wide_continuation(),
                            "TLA+ WideCharConsistent violated: wide char at ({row_idx}, {col}) missing continuation at ({row_idx}, {})",
                            col + 1
                        );
                    }
                    col += 1;
                }
                let last_col = self.storage.cols.saturating_sub(1);
                if let Some(cell) = row.get(last_col) {
                    assert!(
                        !cell.is_wide(),
                        "TLA+ WideCharNotAtEnd violated: wide char at ({row_idx}, {last_col}) which is last column"
                    );
                }
            }
        }
    }

    /// Assert `ExtrasClaimed`: every visible extras-map entry is claimed by its cell's `HAS_EXTRAS`
    /// bit or holds only truecolor fg/bg bytes.
    ///
    /// # Panics
    ///
    /// Panics on a visible entry whose cell lacks HAS_EXTRAS.
    #[cfg(any(debug_assertions, test))]
    pub fn assert_extras_claimed(&self) {
        let (rows, cols) = (self.storage.visible_rows, self.storage.cols);
        for (coord, extra) in self.storage.extras.iter() {
            if coord.row >= rows || coord.col >= cols {
                continue;
            }
            assert!(
                !extra.has_claimable_data()
                    || self
                        .cell(coord.row, coord.col)
                        .is_some_and(crate::Cell::has_extras),
                "ExtrasClaimed violated: an extras entry at ({}, {}) whose cell does not \
                 claim it: {extra:?}",
                coord.row,
                coord.col
            );
        }
    }

    /// ScrollRegionValid + DisplayOffsetValid.
    #[cfg(any(debug_assertions, test))]
    fn assert_scroll_region_valid(&self) {
        assert!(
            self.storage.scroll_region.top <= self.storage.scroll_region.bottom,
            "TLA+ ScrollRegionValid violated: top {} > bottom {}",
            self.storage.scroll_region.top,
            self.storage.scroll_region.bottom
        );
        assert!(
            self.storage.scroll_region.bottom < self.storage.visible_rows,
            "TLA+ ScrollRegionValid violated: bottom {} >= visible_rows {} (0-indexed)",
            self.storage.scroll_region.bottom,
            self.storage.visible_rows
        );
        let max_offset = self.storage.scrollback_lines();
        assert!(
            self.storage.display_offset <= max_offset,
            "TLA+ DisplayOffsetValid violated: display_offset {} > scrollback_lines {max_offset}",
            self.storage.display_offset
        );
    }

    /// RowsNonEmpty + RingHeadValid + TotalLinesValid + TotalLinesMinimum +
    /// RingExtrasAligned: ring buffer structural invariants.
    #[cfg(any(debug_assertions, test))]
    fn assert_ring_buffer_valid(&self) {
        assert!(
            !self.storage.rows.is_empty(),
            "RowsNonEmpty violated: ring buffer has zero rows"
        );
        assert!(
            self.storage.ring_head < self.storage.rows.len(),
            "RingHeadValid violated: ring_head {} >= rows.len() {}",
            self.storage.ring_head,
            self.storage.rows.len()
        );
        assert!(
            self.storage.total_lines <= self.storage.rows.len(),
            "TotalLinesValid violated: total_lines ({}) > rows.len() ({})",
            self.storage.total_lines,
            self.storage.rows.len()
        );
        assert!(
            self.storage.total_lines >= self.storage.visible_rows as usize,
            "TotalLinesMinimum violated: total_lines ({}) < visible_rows ({})",
            self.storage.total_lines,
            self.storage.visible_rows
        );
        assert!(
            self.storage.ring_head == 0 || self.storage.total_lines == self.storage.rows.len(),
            "RingHeadCapacity violated: ring_head={} but total_lines ({}) != rows.len() ({})",
            self.storage.ring_head,
            self.storage.total_lines,
            self.storage.rows.len()
        );
        assert_eq!(
            self.storage.ring_extras.len(),
            self.storage.ring_buffer_scrollback(),
            "RingExtrasAligned violated: ring_extras.len() != ring_buffer_scrollback()"
        );
    }
}
