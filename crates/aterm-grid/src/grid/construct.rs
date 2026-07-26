// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid construction and initialization.
//!
//! Constructors for creating [`Grid`] instances with various scrollback
//! configurations. The Kani build uses a verification-optimized constructor
//! to avoid CBMC state explosion from symbolic loop bounds.

use aterm_scrollback::ScrollbackStorage;

use super::scroll_convert::LazyBuffer;
use super::state::{GridCursorState, GridPresentationState, GridStorage};
use super::{Cell, Grid, HorizontalMargins, PAGE_SIZE, PageStore, ScrollRegion};
use crate::CellExtras;
use crate::Damage;
use crate::GenerationTracker;
use crate::Row;
use crate::StyleTable;
use crate::{MAX_GRID_COLS, MAX_GRID_ROWS};
use std::collections::VecDeque;

impl Grid {
    /// Create a new grid with the given dimensions.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_scrollback(rows, cols, 10_000)
    }

    /// Create a new grid with custom ring buffer scrollback limit.
    ///
    /// Kani builds route through a verification-optimized constructor to avoid
    /// CBMC state explosion from symbolic loop bounds in row/cell initialization.
    #[cfg(kani)]
    #[must_use]
    pub fn with_scrollback(rows: u16, cols: u16, max_scrollback: usize) -> Self {
        Self::kani_stub_with_scrollback(rows, cols, max_scrollback)
    }

    /// Create a new grid with custom ring buffer scrollback limit.
    ///
    /// This sets the size of the in-memory ring buffer. For unlimited
    /// scrollback with tiered storage, use [`Grid::with_tiered_scrollback`].
    #[cfg(not(kani))]
    #[must_use]
    pub fn with_scrollback(rows: u16, cols: u16, max_scrollback: usize) -> Self {
        // Ingress clamp (§5.8): bound the allocation a hostile caller can request.
        let rows = rows.clamp(1, MAX_GRID_ROWS);
        let cols = cols.clamp(1, MAX_GRID_COLS);

        // Pre-heat pages based on initial grid size.
        // Each row needs cols * size_of::<Cell>() bytes.
        // PAGE_SIZE = 64KB = 65536 bytes.
        // Preheat only enough pages for the initial visible rows; extra pages
        // are allocated lazily on first real need (the page store grows on
        // demand in `alloc_slice`). The `max_scrollback` cap is unchanged — only
        // the eager pre-allocation timing differs.
        let bytes_per_row = (cols as usize) * std::mem::size_of::<Cell>();
        let initial_bytes = (rows as usize) * bytes_per_row;
        let pages_needed = (initial_bytes / PAGE_SIZE).max(1);
        let mut pages = PageStore::with_capacity(pages_needed);
        // Reserve only the VISIBLE rows. The scrollback ring grows lazily as
        // lines scroll off (see `grow_scrollback_ring`), so pre-sizing the full
        // `rows + max_scrollback` ring up front is pure wasted startup memory.
        // `row_index` uses `% rows.len()`, which always reflects the current
        // length, so correctness is independent of pre-reserved capacity.
        let mut row_storage = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            // SAFETY: `row_storage` and `pages` are moved into the same
            // `GridStorage`, which drops rows before pages.
            row_storage.push(unsafe { Row::new(cols, &mut pages) });
        }

        Self {
            storage: GridStorage {
                pages,
                rows: row_storage,
                visible_rows: rows,
                cols,
                max_scrollback,
                total_lines: rows as usize,
                display_offset: 0,
                ring_head: 0,
                scrollback: None,
                lazy_buffer: LazyBuffer::new(),
                scrollback_detached_for_reflow: false,
                compress_offload_active: false,
                flood_truncated_lines: 0,
                ring_byte_watermark: None,
                scrollback_clear_gen: 0,
                ring_extras: VecDeque::new(),
                generations: GenerationTracker::new(),
                absolute_row_counter: u64::from(rows),
                // Init NONZERO so `0` is a usable "never observed" sentinel (P1.0).
                content_gen: 1,
                any_double_width: false,
                has_horizontal_margins: false,
                #[cfg(feature = "disk-tier")]
                budget_enforcer: None,
                cursor_state: GridCursorState {
                    cursor: crate::Cursor::default(),
                    saved_cursor: crate::SavedCursor::default(),
                    scroll_region: ScrollRegion::full(rows),
                    horizontal_margins: HorizontalMargins::full(cols),
                    tab_stops: GridCursorState::default_tab_stops(cols),
                    pending_wrap: false,
                    cursor_template: Cell::EMPTY,
                    cursor_template_bg_rgb: None,
                    presentation: GridPresentationState {
                        damage: Damage::Full,
                        extras: CellExtras::new(),
                        styles: StyleTable::new(),
                        content_scroll_delta: 0,
                        pending_absolute_row_update: None,
                        pending_selection_row_update: None,
                    },
                },
            },
        }
    }

    /// Create a new grid with tiered scrollback storage.
    ///
    /// The ring buffer holds `ring_buffer_size` lines for fast access.
    /// Older lines are pushed to the tiered scrollback for memory-efficient
    /// long-term storage.
    ///
    /// # Arguments
    ///
    /// * `rows` - Number of visible rows
    /// * `cols` - Number of columns
    /// * `ring_buffer_size` - Size of the fast ring buffer (e.g., 1000)
    /// * `scrollback` - Tiered scrollback for long-term storage (memory or disk-backed)
    #[must_use]
    pub fn with_tiered_scrollback(
        rows: u16,
        cols: u16,
        ring_buffer_size: usize,
        scrollback: impl Into<ScrollbackStorage>,
    ) -> Self {
        // Ingress clamp (§5.8): bound the allocation a hostile caller can request.
        let rows = rows.clamp(1, MAX_GRID_ROWS);
        let cols = cols.clamp(1, MAX_GRID_COLS);

        // Pre-heat pages based on initial grid size. Only enough pages for the
        // visible rows are preheated; extra pages allocate lazily on first need.
        let bytes_per_row = (cols as usize) * std::mem::size_of::<Cell>();
        let initial_bytes = (rows as usize) * bytes_per_row;
        let pages_needed = (initial_bytes / PAGE_SIZE).max(1);
        let mut pages = PageStore::with_capacity(pages_needed);
        // Reserve only the VISIBLE rows; the ring buffer grows lazily as lines
        // scroll off. `ring_buffer_size` (the cap) is unchanged — only the eager
        // pre-allocation timing differs.
        let mut row_storage = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            // SAFETY: `row_storage` and `pages` are moved into the same
            // `GridStorage`, which drops rows before pages.
            row_storage.push(unsafe { Row::new(cols, &mut pages) });
        }

        Self {
            storage: GridStorage {
                pages,
                rows: row_storage,
                visible_rows: rows,
                cols,
                max_scrollback: ring_buffer_size,
                total_lines: rows as usize,
                display_offset: 0,
                ring_head: 0,
                scrollback: Some(scrollback.into()),
                lazy_buffer: LazyBuffer::new(),
                scrollback_detached_for_reflow: false,
                compress_offload_active: false,
                flood_truncated_lines: 0,
                ring_byte_watermark: None,
                scrollback_clear_gen: 0,
                ring_extras: VecDeque::new(),
                generations: GenerationTracker::new(),
                absolute_row_counter: u64::from(rows),
                // Init NONZERO so `0` is a usable "never observed" sentinel (P1.0).
                content_gen: 1,
                any_double_width: false,
                has_horizontal_margins: false,
                #[cfg(feature = "disk-tier")]
                budget_enforcer: None,
                cursor_state: GridCursorState {
                    cursor: crate::Cursor::default(),
                    saved_cursor: crate::SavedCursor::default(),
                    scroll_region: ScrollRegion::full(rows),
                    horizontal_margins: HorizontalMargins::full(cols),
                    tab_stops: GridCursorState::default_tab_stops(cols),
                    pending_wrap: false,
                    cursor_template: Cell::EMPTY,
                    cursor_template_bg_rgb: None,
                    presentation: GridPresentationState {
                        damage: Damage::Full,
                        extras: CellExtras::new(),
                        styles: StyleTable::new(),
                        content_scroll_delta: 0,
                        pending_absolute_row_update: None,
                        pending_selection_row_update: None,
                    },
                },
            },
        }
    }
}

impl Default for Grid {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cursor;

    // =========================================================================
    // Grid::new() basic dimensions
    // =========================================================================

    #[test]
    fn test_new_basic_dimensions() {
        let grid = Grid::new(24, 80);
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
    }

    #[test]
    fn test_new_cursor_at_origin() {
        let grid = Grid::new(24, 80);
        assert_eq!(grid.cursor_row(), 0);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn test_new_1x1_grid() {
        let grid = Grid::new(1, 1);
        assert_eq!(grid.rows(), 1);
        assert_eq!(grid.cols(), 1);
        assert_eq!(grid.cursor_row(), 0);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn test_new_large_grid() {
        let grid = Grid::new(500, 300);
        assert_eq!(grid.rows(), 500);
        assert_eq!(grid.cols(), 300);
    }

    #[test]
    fn test_new_clamps_zero_rows_to_one() {
        let grid = Grid::new(0, 80);
        assert_eq!(grid.rows(), 1, "zero rows should be clamped to 1");
    }

    #[test]
    fn test_new_clamps_zero_cols_to_one() {
        let grid = Grid::new(24, 0);
        assert_eq!(grid.cols(), 1, "zero cols should be clamped to 1");
    }

    #[test]
    fn test_new_clamps_both_zero_to_one() {
        let grid = Grid::new(0, 0);
        assert_eq!(grid.rows(), 1);
        assert_eq!(grid.cols(), 1);
    }

    #[test]
    fn test_new_clamps_oversize_to_max() {
        let grid = Grid::new(u16::MAX, u16::MAX);
        assert_eq!(grid.rows(), MAX_GRID_ROWS);
        assert_eq!(grid.cols(), MAX_GRID_COLS);
    }

    #[test]
    fn test_new_all_cells_empty() {
        let grid = Grid::new(5, 10);
        for row in 0..5u16 {
            for col in 0..10u16 {
                let cell = grid.cell(row, col).expect("cell should exist");
                assert_eq!(
                    cell.char(),
                    ' ',
                    "cell ({row}, {col}) should be space (empty)"
                );
            }
        }
    }

    #[test]
    fn test_new_scroll_region_is_full() {
        let grid = Grid::new(24, 80);
        let region = grid.scroll_region();
        assert!(
            region.is_full(24),
            "scroll region should be full screen on new grid"
        );
        assert_eq!(region.top, 0);
        assert_eq!(region.bottom, 23);
    }

    #[test]
    fn test_new_display_offset_zero() {
        let grid = Grid::new(24, 80);
        assert_eq!(grid.display_offset(), 0);
    }

    #[test]
    fn test_new_no_pending_wrap() {
        let grid = Grid::new(24, 80);
        assert!(!grid.pending_wrap());
    }

    #[test]
    fn test_new_total_lines_equals_rows() {
        let grid = Grid::new(24, 80);
        assert_eq!(grid.total_lines(), 24);
    }

    #[test]
    fn test_new_no_scrollback_lines() {
        let grid = Grid::new(24, 80);
        assert_eq!(grid.scrollback_lines(), 0);
    }

    // =========================================================================
    // Grid::with_scrollback()
    // =========================================================================

    #[test]
    fn test_with_scrollback_dimensions() {
        let grid = Grid::with_scrollback(10, 40, 5000);
        assert_eq!(grid.rows(), 10);
        assert_eq!(grid.cols(), 40);
    }

    #[test]
    fn test_with_scrollback_cursor_at_origin() {
        let grid = Grid::with_scrollback(10, 40, 5000);
        assert_eq!(grid.cursor_row(), 0);
        assert_eq!(grid.cursor_col(), 0);
    }

    #[test]
    fn test_with_scrollback_zero_scrollback() {
        let grid = Grid::with_scrollback(5, 10, 0);
        assert_eq!(grid.rows(), 5);
        assert_eq!(grid.cols(), 10);
        assert_eq!(grid.scrollback_lines(), 0);
    }

    #[test]
    fn test_with_scrollback_large_scrollback() {
        let grid = Grid::with_scrollback(24, 80, 100_000);
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
        assert_eq!(grid.scrollback_lines(), 0);
    }

    // =========================================================================
    // Grid::with_tiered_scrollback()
    // =========================================================================

    #[test]
    fn test_with_tiered_scrollback_dimensions() {
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        let grid = Grid::with_tiered_scrollback(24, 80, 1000, sb);
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
    }

    #[test]
    fn test_with_tiered_scrollback_has_scrollback() {
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        let grid = Grid::with_tiered_scrollback(24, 80, 1000, sb);
        assert!(
            grid.scrollback().is_some(),
            "tiered scrollback grid should have scrollback attached"
        );
    }

    #[test]
    fn test_with_tiered_scrollback_clamps_zero() {
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        let grid = Grid::with_tiered_scrollback(0, 0, 500, sb);
        assert_eq!(grid.rows(), 1);
        assert_eq!(grid.cols(), 1);
    }

    // =========================================================================
    // Default impl
    // =========================================================================

    #[test]
    fn test_default_is_24x80() {
        let grid = Grid::default();
        assert_eq!(grid.rows(), 24);
        assert_eq!(grid.cols(), 80);
    }

    #[test]
    fn test_default_cursor_at_origin() {
        let grid = Grid::default();
        assert_eq!(grid.cursor(), Cursor::default());
    }

    // =========================================================================
    // Invariant checks after construction
    // =========================================================================

    #[test]
    fn test_new_invariants_hold() {
        let grid = Grid::new(24, 80);
        grid.assert_invariants();
    }

    #[test]
    fn test_with_scrollback_invariants_hold() {
        let grid = Grid::with_scrollback(10, 40, 500);
        grid.assert_invariants();
    }

    #[test]
    fn test_with_tiered_scrollback_invariants_hold() {
        let sb = aterm_scrollback::Scrollback::new(50, 500, 500_000);
        let grid = Grid::with_tiered_scrollback(10, 40, 500, sb);
        grid.assert_invariants();
    }
}
