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
///
/// `pub` under the `testing` feature (unlike its siblings, which are
/// `cfg(test)`-only) because a DOWNSTREAM crate's tests need it: `Line`
/// construction from a ring row is what a scrollback COPY pays per selected
/// line, and the pin that each selected row is resolved exactly ONCE
/// (aterm-core `selection.rs`, SCR-4) can only be written where
/// `selection_to_string` lives. Exactly the use the module header describes.
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

// Counter for ring rows a ROWS-ONLY resize evacuates out of the grid ring (the
// second half of the resize bounded-cost obligation — see
// `tests/reflow/rows_only_cost_bound.rs`).
//
// `SCROLLBACK_REFLOW_SYNC_LINES` above instruments the WIDTH reflow only, so a
// rows-only resize incremented nothing and the gate reported 0 — which reads as
// proof of O(viewport) when it actually means "not instrumented". It was not:
// `adjust_row_count` compared the FULL ring length against the new VISIBLE row
// target, so every rows-only resize (window-height drag, pane split/close,
// divider drag, find-bar toggle) migrated the entire ring — ~9,999 rows at the
// GUI's 10,000-line ring — into the tiered store synchronously under the
// caller's lock. This counter closes that hole: it must stay bounded by the
// HEIGHT DELTA, never by history.
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

// Counter for RING-tier history rows materialized DIRECTLY from their stored
// `Row` + `ScrolledRowExtras` (SCR-2's fast path) instead of through the
// Row -> Line -> cells round trip. Reach is asserted in BOTH directions by the
// parity test: a realistic corpus must drive it above zero (the fast path
// really fires, so the parity assertions are not vacuous), and a read that
// lands in the warm/cold tiers must leave it at zero (it never over-fires onto
// a tier whose bytes it cannot see).
thread_local! {
    static RING_FAST_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the ring fast-materialize counter.
pub(crate) fn count_ring_fast_materialize() {
    RING_FAST_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the ring fast-materialize count.
///
/// `pub` under the `testing` feature (unlike its `cfg(test)`-only siblings)
/// because the parity test that needs it lives DOWNSTREAM, in aterm-core: only
/// there can a corpus of OSC 8 / SGR 58 / emoji / CJK be fed through the real
/// parser into real scrollback, which is the only way to prove the fast path
/// agrees with the round trip on the data that actually reaches it.
///
/// Gated on the FEATURE alone, not `any(test, feature)`: without the feature
/// this module is `pub(crate)`, so a `pub fn` no aterm-grid test calls would
/// read as dead code.
#[cfg(feature = "testing")]
pub fn take_ring_fast_materialize() -> usize {
    RING_FAST_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

// Counter for HISTORY ROWS MATERIALIZED from the 3-tier store to answer a
// scrolled-back viewport read (`Grid::materialized_history_row`) — the SCR-1
// number, and the one this campaign moved from 24 per repaint to 0.
//
// WHAT IS COUNTED, PRECISELY, AND WHY THE DISTINCTION IS THE WHOLE POINT: the
// MISSES. A memo HIT does not increment, and in debug builds a hit still runs a
// full re-materialize (the stale-row net in `viewport_row_cache`'s module docs)
// — so a counter placed one level down, inside
// `materialize_scrollback_row_full`, would count that net and report 24 in
// exactly the build a test runs in. It would look like the fix had never
// landed. This counter sits at the two sites that do REAL work for the frame:
// the memo miss, and the unkeyable read-through.
thread_local! {
    static VIEWPORT_ROW_MATERIALIZE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the scrolled-back history-row materialize counter.
pub(crate) fn count_viewport_row_materialize() {
    VIEWPORT_ROW_MATERIALIZE.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the scrolled-back history-row materialize count.
///
/// `pub` under the `testing` feature (like its `take_ring_fast_materialize`
/// sibling, and for the same reason): the claim is about a FRAME, and only a
/// downstream aterm-core test can drive `Terminal::cell_frame_into` over real
/// parsed scrollback — which is the only thing that makes "materializations per
/// frame" a measurement of the product rather than of a fixture.
#[cfg(feature = "testing")]
pub fn take_viewport_row_materialize() -> usize {
    VIEWPORT_ROW_MATERIALIZE.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}
