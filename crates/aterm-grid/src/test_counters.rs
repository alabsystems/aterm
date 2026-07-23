// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic operation counters for complexity assertions.
//!
//! These counters replace wall-clock timing for complexity assertions in tests.
//! Behind `#[cfg(any(test, feature = "testing"))]` so downstream crate tests
//! (aterm-core) can instrument grid operations via the `testing` feature.
//!
//! NOTE: Use full path `std::cell::Cell` to avoid conflict with grid `Cell` type.

// Counter for CellExtras shift operations (entries iterated per shift call).
thread_local! {
    static EXTRAS_SHIFT_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for CellExtras clear/retain operations (entries iterated per clear call).
thread_local! {
    static EXTRAS_CLEAR_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for row-to-line conversion operations (scroll_up hot path).
thread_local! {
    static ROW_TO_LINE_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for cells processed during row_to_line (O(cols) verification).
thread_local! {
    static ROW_TO_LINE_CELLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for reflow row processing operations.
thread_local! {
    static REFLOW_ROW_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for cells processed during reflow (O(cols) verification).
thread_local! {
    static REFLOW_CELL_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counter for off-screen scrollback lines rewrapped SYNCHRONOUSLY inside a
// `Grid::resize` width change (the L0-hang budget: this must stay bounded by the
// viewport, NOT by session history — see `tests/reflow/cost_bound.rs` and the
// bounded-cost obligation. A non-zero count on a deep-history resize is the
// signature of the whole-Mac freeze: the reflow ran on the caller's thread, under
// its lock, in O(history)).
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

/// Increment the cell counter (for O(cols) verification).
pub(crate) fn count_row_to_line_cell() {
    ROW_TO_LINE_CELLS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the row-to-line operation count.
#[cfg(test)]
pub(crate) fn take_row_to_line_ops() -> usize {
    ROW_TO_LINE_OPS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Take (read and reset) the cell count.
#[cfg(test)]
pub(crate) fn take_row_to_line_cells() -> usize {
    ROW_TO_LINE_CELLS.with(|c| {
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

/// Increment the synchronous-scrollback-reflow counter by `n` (history lines
/// rewrapped on the caller's thread inside `Grid::resize`).
pub(crate) fn count_scrollback_reflow_sync_lines(n: usize) {
    SCROLLBACK_REFLOW_SYNC_LINES.with(|c| c.set(c.get() + n));
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
