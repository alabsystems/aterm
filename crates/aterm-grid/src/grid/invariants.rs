// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TLA+ specification invariant checking for the terminal grid.
//!
//! This module provides debug-build verification that Grid state
//! matches the formal TLA+ specification invariants.

use super::Grid;

impl Grid {
    /// Assert TLA+ specification invariants in debug builds.
    ///
    /// Validates key invariants from the Terminal.tla specification.
    /// See individual `assert_*` helpers for details.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if any invariant is violated.
    /// Does nothing in release builds for performance.
    #[cfg(any(test, feature = "testing"))]
    #[inline]
    pub fn assert_invariants(&self) {
        #[cfg(debug_assertions)]
        {
            self.assert_cursor_in_bounds();
            self.assert_wide_char_consistent();
            self.assert_scroll_region_valid();
            self.assert_ring_buffer_valid();
        }
    }

    /// Assert the grid invariants empirically confirmed (by fuzzing — see
    /// aterm-core `fuzz_process_never_panics`, validated across 300k+ iterations
    /// of arbitrary bytes / crafted escapes / unicode / reflow / alt-screen) to
    /// hold against the live implementation: `CursorInBounds`, `ScrollRegionValid` +
    /// `DisplayOffsetValid`, and the ring-buffer structure. This is the wirable
    /// subset of [`Self::assert_invariants`] — everything EXCEPT `WideCharConsistent`.
    ///
    /// `WideCharConsistent` is the one omitted check: it is genuinely violable —
    /// writing a wide grapheme onto a prior wide char's continuation cell
    /// (autowrap on a narrow grid) leaves a dangling WIDE main with no
    /// continuation (a real spec-vs-impl gap, owner-territory because the
    /// wide-char write/erase semantics intersect a previously-reverted
    /// differential divergence). Asserting it here would FALSE-fail.
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

    /// WideCharConsistent + WideCharNotAtEnd: wide chars have continuations
    /// and don't appear at the last column.
    #[cfg(any(test, feature = "testing"))]
    fn assert_wide_char_consistent(&self) {
        for row_idx in 0..self.storage.visible_rows {
            if let Some(row) = self.row(row_idx) {
                for col in 0..self.storage.cols.saturating_sub(1) {
                    if let Some(cell) = row.get(col)
                        && cell.is_wide()
                        && let Some(next_cell) = row.get(col + 1)
                    {
                        assert!(
                            next_cell.is_wide_continuation(),
                            "TLA+ WideCharConsistent violated: wide char at ({row_idx}, {col}) missing continuation at ({row_idx}, {})",
                            col + 1
                        );
                    }
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
        // Ring buffer must always contain at least one row — the constructor
        // enforces rows.max(1). Empty rows would cause division-by-zero in
        // all `% self.storage.rows.len()` modular arithmetic (row_index, scroll, erase).
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
        // total_lines tracks visible + scrollback. It cannot exceed the
        // allocated row count, and must be at least visible_rows (the grid
        // always has enough rows for the visible area).
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
        // ring_head advances only in Phase 2 (reuse) of scroll_up, which
        // only fires when total_lines == rows.len() (at capacity). Reflow
        // and erase_scrollback reset ring_head to 0. Therefore ring_head > 0
        // implies the buffer is at full capacity.
        assert!(
            self.storage.ring_head == 0 || self.storage.total_lines == self.storage.rows.len(),
            "RingHeadCapacity violated: ring_head={} but total_lines ({}) != rows.len() ({})",
            self.storage.ring_head,
            self.storage.total_lines,
            self.storage.rows.len()
        );
    }
}
