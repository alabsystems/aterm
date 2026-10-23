// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic operation counters for complexity assertions in test and debug builds.

thread_local! {
    static EXTRAS_SHIFT_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static EXTRAS_CLEAR_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static ROW_TO_LINE_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static REFLOW_ROW_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static REFLOW_CELL_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static REFLOW_PASSTHROUGH_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    static SCROLLBACK_REFLOW_SYNC_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the extras shift operation counter by `n` (entries iterated).
pub(crate) fn count_extras_shift_ops(n: usize) {
    EXTRAS_SHIFT_OPS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the extras shift operation count.
#[cfg(test)]
pub(crate) fn take_extras_shift_ops() -> usize {
    EXTRAS_SHIFT_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static HYPERLINK_WALK_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the hyperlink cold-path walk counter by `n` (entries iterated).
pub(crate) fn count_hyperlink_walk_ops(n: usize) {
    HYPERLINK_WALK_OPS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the hyperlink cold-path walk count.
#[cfg(test)]
pub(crate) fn take_hyperlink_walk_ops() -> usize {
    HYPERLINK_WALK_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Increment the extras clear operation counter by `n` (entries iterated).
pub(crate) fn count_extras_clear_ops(n: usize) {
    EXTRAS_CLEAR_OPS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the extras clear operation count.
#[cfg(test)]
pub(crate) fn take_extras_clear_ops() -> usize {
    EXTRAS_CLEAR_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Increment the row-to-line operation counter.
pub(crate) fn count_row_to_line_op() {
    ROW_TO_LINE_OPS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the row-to-line operation count.
#[cfg(any(test, debug_assertions))]
pub fn take_row_to_line_ops() -> usize {
    ROW_TO_LINE_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Increment the reflow row operation counter.
pub(crate) fn count_reflow_row_op() {
    REFLOW_ROW_OPS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the reflow row operation count.
#[cfg(test)]
pub(crate) fn take_reflow_row_ops() -> usize {
    REFLOW_ROW_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Increment the reflow cell operation counter by `n` (cells copied).
pub(crate) fn count_reflow_cell_ops(n: usize) {
    REFLOW_CELL_OPS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the reflow cell operation count.
#[cfg(test)]
pub(crate) fn take_reflow_cell_ops() -> usize {
    REFLOW_CELL_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Increment the rewrap-passthrough counter by `n`.
pub(crate) fn count_reflow_passthrough_lines(n: usize) {
    REFLOW_PASSTHROUGH_LINES.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the rewrap-passthrough line count.
#[cfg(test)]
pub(crate) fn take_reflow_passthrough_lines() -> usize {
    REFLOW_PASSTHROUGH_LINES.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static HANDBACK_HISTORY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the hand-back history-read counter by `n`.
pub(crate) fn count_handback_history_reads(n: usize) {
    HANDBACK_HISTORY_READS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the hand-back history-read count.
#[cfg(test)]
pub(crate) fn take_handback_history_reads() -> usize {
    HANDBACK_HISTORY_READS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static ROWS_ONLY_RESIZE_MIGRATED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the synchronous-scrollback-reflow counter by `n`.
pub(crate) fn count_scrollback_reflow_sync_lines(n: usize) {
    SCROLLBACK_REFLOW_SYNC_LINES.with(|c| c.set(c.get() + n));
}

/// Increment the rows-only-resize migration counter by `n`.
pub(crate) fn count_rows_only_resize_migrated_rows(n: usize) {
    ROWS_ONLY_RESIZE_MIGRATED_ROWS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the rows-only-resize migration count.
#[cfg(test)]
pub(crate) fn take_rows_only_resize_migrated_rows() -> usize {
    ROWS_ONLY_RESIZE_MIGRATED_ROWS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Take (read and reset) the synchronous-scrollback-reflow line count.
#[cfg(test)]
pub(crate) fn take_scrollback_reflow_sync_lines() -> usize {
    SCROLLBACK_REFLOW_SYNC_LINES.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static RING_FAST_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the ring fast-materialize counter.
pub(crate) fn count_ring_fast_materialize() {
    RING_FAST_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the ring fast-materialize count.
#[cfg(debug_assertions)]
pub fn take_ring_fast_materialize() -> usize {
    RING_FAST_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static VIEWPORT_ROW_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the scrolled-back history-row materialize counter.
pub(crate) fn count_viewport_row_materialize() {
    VIEWPORT_ROW_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the scrolled-back history-row materialize count.
#[cfg(debug_assertions)]
pub fn take_viewport_row_materialize() -> usize {
    VIEWPORT_ROW_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

thread_local! {
    static RGB_HARVEST_CELLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the RGB-harvest counter by `n` cells examined.
#[cfg(test)]
pub(crate) fn count_rgb_harvest_cells(n: usize) {
    RGB_HARVEST_CELLS.with(|c| c.set(c.get() + n));
}

/// Take (read and reset) the RGB-harvest cell count.
#[cfg(test)]
pub(crate) fn take_rgb_harvest_cells() -> usize {
    RGB_HARVEST_CELLS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}
