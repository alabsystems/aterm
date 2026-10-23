// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid construction.

use aterm_scrollback::ScrollbackStorage;

use super::ViewportRowCache;
use super::scroll_convert::LazyBuffer;
use super::state::{FillDebt, GridCursorState, GridPresentationState, GridStorage};
use super::{Cell, Grid, HorizontalMargins, PAGE_SIZE, PageStore, ScrollRegion};
use crate::CellExtras;
use crate::Damage;
use crate::Row;
use crate::{MAX_GRID_COLS, MAX_GRID_ROWS};

impl Grid {
    /// Create a new grid with the given dimensions.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_scrollback(rows, cols, 10_000)
    }

    /// Create a grid with a custom ring scrollback limit. For tiered storage,
    /// use [`Grid::with_tiered_scrollback`].
    #[must_use]
    pub fn with_scrollback(rows: u16, cols: u16, max_scrollback: usize) -> Self {
        let rows = rows.clamp(1, MAX_GRID_ROWS);
        let cols = cols.clamp(1, MAX_GRID_COLS);

        let bytes_per_row = (cols as usize) * std::mem::size_of::<Cell>();
        let initial_bytes = (rows as usize) * bytes_per_row;
        let pages_needed = (initial_bytes / PAGE_SIZE).max(1);
        let mut pages = PageStore::with_capacity(pages_needed);
        let mut row_storage = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            // SAFETY: `row_storage` and `pages` are moved into the same
            // `GridStorage`, which drops rows before pages.
            row_storage.push(unsafe { Row::new(cols, &mut pages) });
        }

        Self {
            viewport_cache: ViewportRowCache::default(),
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
                pending_scrollback_settings: None,
                fill_debt: FillDebt::default(),
                staged_width_stale: false,
                detached_reader_aim: None,
                compress_offload_active: false,
                flood_truncated_lines: 0,
                reflow_window_cap_dropped: 0,
                scrollback_clear_gen: 0,
                history_renumber_epoch: 0,
                history_reveal_gen: 0,
                ring_extras: super::ring_extras::RingExtras::new(),
                resize_undo: None,
                handback: super::resize_handback::HandBack::default(),
                resize_law: super::resize_handback::ResizeLaw::default(),
                absolute_row_counter: u64::from(rows),
                content_gen: 1,
                reader_live_bottom_gen: 0,
                wrap_serial: 0,
                any_double_width: false,
                has_horizontal_margins: false,
                cursor_state: GridCursorState {
                    cursor: crate::Cursor::default(),
                    saved_cursor: crate::SavedCursor::default(),
                    scroll_region: ScrollRegion::full(rows),
                    horizontal_margins: HorizontalMargins::full(cols),
                    tab_stops: GridCursorState::default_tab_stops(cols),
                    tab_defaults_suppressed: false,
                    pending_wrap: false,
                    cursor_template: Cell::EMPTY,
                    cursor_template_bg_rgb: None,
                    presentation: GridPresentationState {
                        damage: Damage::Full,
                        extras: CellExtras::new(),
                        content_scroll_delta: 0,
                        selection_damage: crate::SelectionDamage::None,
                        last_output_damage_abs: None,
                        last_resize_row_shift: 0,
                        last_resize_shape: crate::ResizeShape::default(),
                        coordinates_invalidated: false,
                        row_band_moves: crate::RowBandMoves::default(),
                        anchors: crate::AnchorSet::default(),
                        pending_absolute_row_update: None,
                        pending_selection_row_update: None,
                        row_rev: Vec::new(),
                        row_rev_clock: 0,
                        row_rev_folded_seq: 0,
                    },
                },
            },
        }
    }

    /// Create a grid with tiered scrollback: the ring holds `ring_buffer_size`
    /// lines and older lines go to `scrollback`.
    #[must_use]
    pub fn with_tiered_scrollback(
        rows: u16,
        cols: u16,
        ring_buffer_size: usize,
        scrollback: impl Into<ScrollbackStorage>,
    ) -> Self {
        let rows = rows.clamp(1, MAX_GRID_ROWS);
        let cols = cols.clamp(1, MAX_GRID_COLS);
        let scrollback: ScrollbackStorage = scrollback.into();
        let carried = u64::try_from(scrollback.line_count()).unwrap_or(u64::MAX);

        let bytes_per_row = (cols as usize) * std::mem::size_of::<Cell>();
        let initial_bytes = (rows as usize) * bytes_per_row;
        let pages_needed = (initial_bytes / PAGE_SIZE).max(1);
        let mut pages = PageStore::with_capacity(pages_needed);
        let mut row_storage = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            // SAFETY: `row_storage` and `pages` are moved into the same
            // `GridStorage`, which drops rows before pages.
            row_storage.push(unsafe { Row::new(cols, &mut pages) });
        }

        Self {
            viewport_cache: ViewportRowCache::default(),
            storage: GridStorage {
                pages,
                rows: row_storage,
                visible_rows: rows,
                cols,
                max_scrollback: ring_buffer_size,
                total_lines: rows as usize,
                display_offset: 0,
                ring_head: 0,
                scrollback: Some(scrollback),
                lazy_buffer: LazyBuffer::new(),
                scrollback_detached_for_reflow: false,
                pending_scrollback_settings: None,
                fill_debt: FillDebt::default(),
                staged_width_stale: false,
                detached_reader_aim: None,
                compress_offload_active: false,
                flood_truncated_lines: 0,
                reflow_window_cap_dropped: 0,
                scrollback_clear_gen: 0,
                history_renumber_epoch: 0,
                history_reveal_gen: 0,
                ring_extras: super::ring_extras::RingExtras::new(),
                resize_undo: None,
                handback: super::resize_handback::HandBack::default(),
                resize_law: super::resize_handback::ResizeLaw::default(),
                absolute_row_counter: u64::from(rows).saturating_add(carried),
                content_gen: 1,
                reader_live_bottom_gen: 0,
                wrap_serial: 0,
                any_double_width: false,
                has_horizontal_margins: false,
                cursor_state: GridCursorState {
                    cursor: crate::Cursor::default(),
                    saved_cursor: crate::SavedCursor::default(),
                    scroll_region: ScrollRegion::full(rows),
                    horizontal_margins: HorizontalMargins::full(cols),
                    tab_stops: GridCursorState::default_tab_stops(cols),
                    tab_defaults_suppressed: false,
                    pending_wrap: false,
                    cursor_template: Cell::EMPTY,
                    cursor_template_bg_rgb: None,
                    presentation: GridPresentationState {
                        damage: Damage::Full,
                        extras: CellExtras::new(),
                        content_scroll_delta: 0,
                        selection_damage: crate::SelectionDamage::None,
                        last_output_damage_abs: None,
                        last_resize_row_shift: 0,
                        last_resize_shape: crate::ResizeShape::default(),
                        coordinates_invalidated: false,
                        row_band_moves: crate::RowBandMoves::default(),
                        anchors: crate::AnchorSet::default(),
                        pending_absolute_row_update: None,
                        pending_selection_row_update: None,
                        row_rev: Vec::new(),
                        row_rev_clock: 0,
                        row_rev_folded_seq: 0,
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

    #[test]
    fn constructors_clamp_dimensions_and_hold_invariants() {
        let cases = [
            ((0, 80), (1, 80)),
            ((24, 0), (24, 1)),
            ((0, 0), (1, 1)),
            ((u16::MAX, u16::MAX), (MAX_GRID_ROWS, MAX_GRID_COLS)),
            ((10, 40), (10, 40)),
        ];
        for ((rows, cols), want) in cases {
            let tiered = aterm_scrollback::Scrollback::new(50, 500, 500_000);
            for grid in [
                Grid::new(rows, cols),
                Grid::with_scrollback(rows, cols, 500),
                Grid::with_tiered_scrollback(rows, cols, 500, tiered),
            ] {
                assert_eq!((grid.rows(), grid.cols()), want, "{rows}x{cols}");
                grid.assert_invariants();
            }
        }
    }
}
