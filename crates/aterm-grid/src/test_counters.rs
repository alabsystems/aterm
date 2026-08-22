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

// Counter for scrollback lines that took the RFL-4a rewrap PASSTHROUGH
// (clone-through of width-invariant single-row logical lines) instead of the
// full decompose+rebuild path. Reach is asserted in BOTH directions: a mixed
// corpus must drive it above zero (the fast path really fires) and a
// wrap-changing corpus must leave it at zero (the gate never over-fires).
thread_local! {
    static REFLOW_PASSTHROUGH_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

// Counter for hyperlink-limit COLD-PATH walks (entries iterated per walk).
//
// The guard in `CellExtras::enforce_hyperlink_limit` is supposed to keep this
// at zero unless the hyperlink population is genuinely over budget; a
// non-zero-per-call count is the signature of the guard testing the wrong
// quantity (total extras entries instead of hyperlink-bearing ones).
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

/// Increment the rewrap-passthrough counter by `n` (lines clone-through-ed by
/// the RFL-4a wrap-invariance fast path in `reflow_scrollback_lines`).
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
