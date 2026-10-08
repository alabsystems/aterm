// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic operation counters for complexity assertions, in place of
//! wall-clock timing. Enabled under `cfg(test)` or the `testing` feature so
//! downstream crate tests can instrument grid operations.
//!
//! Use the full path `std::cell::Cell`; `Cell` here is the grid cell type.

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

// Scrollback lines taken by the rewrap passthrough (clone of width-invariant
// single-row logical lines) instead of decompose+rebuild. Asserted both ways:
// a mixed corpus drives it above zero, a wrap-changing corpus leaves it at 0.
thread_local! {
    static REFLOW_PASSTHROUGH_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Off-screen scrollback lines rewrapped synchronously inside a `Grid::resize`
// width change. Must stay bounded by the viewport, not session history
// (`tests/reflow/cost_bound.rs`).
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

// Entries iterated by hyperlink-limit cold-path walks. The guard in
// `CellExtras::enforce_hyperlink_limit` keeps this at zero unless the
// hyperlink population is really over budget.
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
///
/// `pub` under `testing` because aterm-core's selection tests pin that each
/// selected ring row is converted to a `Line` exactly once.
#[cfg(any(test, feature = "testing"))]
pub fn take_row_to_line_ops() -> usize {
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

// Ring rows a rows-only resize evacuates out of the grid ring
// (`tests/reflow/rows_only_cost_bound.rs`). Must stay bounded by the height
// delta, never by history.
thread_local! {
    static ROWS_ONLY_RESIZE_MIGRATED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the synchronous-scrollback-reflow counter by `n` (history lines
/// rewrapped on the caller's thread inside `Grid::resize`).
pub(crate) fn count_scrollback_reflow_sync_lines(n: usize) {
    SCROLLBACK_REFLOW_SYNC_LINES.with(|c| c.set(c.get() + n));
}

/// Increment the rows-only-resize migration counter by `n` (ring rows evacuated
/// out of the ring on the caller's thread inside a rows-only `Grid::resize`).
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

// Ring-tier history rows materialized directly from their stored `Row` +
// `ScrolledRowExtras` instead of the Row -> Line -> cells round trip. The
// parity test asserts it both ways: above zero on a realistic corpus, zero for
// reads that land in the warm/cold tiers.
thread_local! {
    static RING_FAST_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the ring fast-materialize counter.
pub(crate) fn count_ring_fast_materialize() {
    RING_FAST_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the ring fast-materialize count.
///
/// Feature-gated `pub` for aterm-core's parity test, which feeds real parsed
/// scrollback. Gated on the feature alone: without it the module is
/// `pub(crate)` and this would be dead code.
#[cfg(feature = "testing")]
pub fn take_ring_fast_materialize() -> usize {
    RING_FAST_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

// History rows materialized from the 3-tier store for a scrolled-back viewport
// read (`Grid::materialized_history_row`). Counts only real work (memo misses
// and unkeyable read-throughs), not memo hits, which in debug builds still
// re-materialize as a stale-row check.
thread_local! {
    static VIEWPORT_ROW_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the scrolled-back history-row materialize counter.
pub(crate) fn count_viewport_row_materialize() {
    VIEWPORT_ROW_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the scrolled-back history-row materialize count.
/// Feature-gated `pub` so aterm-core can count materializations per frame of
/// `Terminal::cell_frame_into` over real scrollback.
#[cfg(feature = "testing")]
pub fn take_viewport_row_materialize() -> usize {
    VIEWPORT_ROW_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

// Counter for the cells a resize's RGB harvest
// (`Grid::migrate_rgb_ring_to_extras`) examined for ring-stored truecolor. The
// early-out contract is a COUNT, not a clock: a screen that has written no
// truecolor since the last resize has no RGB ring and the harvest must visit no
// cell (`tests/reflow/rgb_across_resize.rs`).
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
