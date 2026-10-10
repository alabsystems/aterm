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
            self.assert_scroll_region_valid();
            self.assert_ring_buffer_valid();
            self.assert_keys_cover_retained_rows();
            // The history byte sums the link budget and `memory_used` read
            // are running sums: each equals a fresh walk of its rows.
            self.storage.ring_extras.assert_running_sums();
            self.storage.lazy_buffer.assert_running_sums();
        }
    }

    /// Assert the O(1) grid invariants — `CursorInBounds`, `ScrollRegionValid` +
    /// `DisplayOffsetValid`, the ring-buffer structure and `KeysCoverRetained`
    /// — the subset of
    /// [`Self::assert_invariants`] cheap enough to run on every `process` batch.
    /// The O(cells) `WideCharConsistent` walk is
    /// [`Self::assert_wide_char_consistent`], which the no-panic fuzzers
    /// (aterm-core `fuzz_process_never_panics`) run after every step.
    ///
    /// Available in debug builds, tests, and the `testing` feature (the last so a
    /// release-mode fuzz harness can still call it). INTEGRITY-SELFCHECK (M7) wires
    /// it into `Terminal::process` under `#[cfg(debug_assertions)]`, so every debug
    /// build continuously validates the grid at the public processing boundary — the
    /// same checks the no-panic fuzzer asserts, now always-on in development. Zero
    /// cost in release.
    #[cfg(any(debug_assertions, test, feature = "testing"))]
    pub fn assert_structural_invariants(&self) {
        self.assert_cursor_in_bounds();
        self.assert_scroll_region_valid();
        self.assert_ring_buffer_valid();
        self.assert_keys_cover_retained_rows();
    }

    /// KeysCoverRetained: `absolute_row_counter >= visible_rows +
    /// scrollback_lines()` — every retained row has an absolute key at or
    /// above 0. While it holds, the two maps of an absolute row name one row:
    /// a shell's OSC 133 mark is RECORDED through `visible_to_absolute`
    /// (`counter − visible + r`) and READ BACK (`line`, `blocks`, `blocktext`)
    /// through `oldest_absolute_row()` (`counter − visible − scrollback`,
    /// saturating at 0) plus the history count. Below it the read map
    /// saturates and the write map does not, and every later mark reads back
    /// as a row above its prompt (tab #1, 2026-10-01: a rows-only grow of a
    /// grid with nothing evicted appended rows under a fixed counter).
    #[cfg(any(debug_assertions, test, feature = "testing"))]
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
    #[cfg(any(debug_assertions, test, feature = "testing"))]
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

    /// WideCharConsistent + WideCharNotAtEnd: wide chars have continuations and
    /// never sit in the last column. The writers clear orphaned halves at both
    /// edges of what they overwrite (`fixup_wide_char_write`,
    /// `fixup_wide_chars_in_range`). O(cells), so not part of
    /// [`Self::assert_structural_invariants`].
    ///
    /// # Panics
    ///
    /// Panics if a visible wide cell lacks its continuation or sits in the
    /// last column.
    #[cfg(any(debug_assertions, test, feature = "testing"))]
    pub fn assert_wide_char_consistent(&self) {
        // Walks each row's cell slice once (`Row::get` per cell re-derives the
        // slice from its page every call, which dominated debug fuzz runs).
        // Bounded by the grid's column count exactly as the per-cell `get`
        // walk was: pairs (col, col + 1) with col + 1 < cols, both in the row.
        let cols = usize::from(self.storage.cols);
        for row_idx in 0..self.storage.visible_rows {
            if let Some(row) = self.row(row_idx) {
                let cells = row.as_slice();
                let walked = &cells[..cells.len().min(cols)];
                // A plain index loop: slice indexing is a MIR primitive, where
                // a range or slice iterator costs calls per cell in a debug
                // build (this runs after every fuzz step).
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

    /// ScrollRegionValid + DisplayOffsetValid.
    #[cfg(any(debug_assertions, test, feature = "testing"))]
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

    /// RowsNonEmpty + RingHeadValid + TotalLinesValid + TotalLinesMinimum:
    /// ring buffer structural invariants.
    #[cfg(any(debug_assertions, test, feature = "testing"))]
    fn assert_ring_buffer_valid(&self) {
        // The ring always has a row (the constructor enforces rows.max(1));
        // every `% rows.len()` depends on it.
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
        // visible_rows <= total_lines <= allocated rows.
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
        // ring_head advances only when scroll_up reuses rows at capacity;
        // reflow and erase_scrollback reset it. So ring_head > 0 implies full.
        assert!(
            self.storage.ring_head == 0 || self.storage.total_lines == self.storage.rows.len(),
            "RingHeadCapacity violated: ring_head={} but total_lines ({}) != rows.len() ({})",
            self.storage.ring_head,
            self.storage.total_lines,
            self.storage.rows.len()
        );
    }
}
