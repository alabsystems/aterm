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
    static TAB_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one step of a multi-stop tab (CHT, CBT).
pub(crate) fn count_tab_step() {
    TAB_STEPS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the multi-stop tab step count.
#[cfg(test)]
pub(crate) fn take_tab_steps() -> usize {
    TAB_STEPS.with(|c| {
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

#[cfg(test)]
thread_local! {
    static REFERENCE_PATHS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread runs the scroll paths' reference implementations (the row-by-row forms
/// the fast paths replaced), which the differential tests hold the fast paths to.
#[cfg(test)]
pub(crate) fn reference_paths() -> bool {
    REFERENCE_PATHS.with(std::cell::Cell::get)
}

/// Run `f` on this thread's reference paths ([`reference_paths`]), restoring the fast paths after.
#[cfg(test)]
pub(crate) fn on_reference_paths<T>(f: impl FnOnce() -> T) -> T {
    REFERENCE_PATHS.with(|c| c.set(true));
    let out = f();
    REFERENCE_PATHS.with(|c| c.set(false));
    out
}

thread_local! {
    static SHORT_LINE_RUN_SHIFT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread's coalesced line feeds ([`crate::Grid::line_feeds_at_region_bottom`])
/// shift the region one line short of their count: the planted fault a line-run differential
/// test's negative control arms ([`with_short_line_run_shift`]).
pub(crate) fn short_line_run_shift() -> bool {
    SHORT_LINE_RUN_SHIFT.with(std::cell::Cell::get)
}

/// Run `f` with [`short_line_run_shift`] armed on this thread, disarmed again after.
pub fn with_short_line_run_shift<T>(f: impl FnOnce() -> T) -> T {
    SHORT_LINE_RUN_SHIFT.with(|c| c.set(true));
    let out = f();
    SHORT_LINE_RUN_SHIFT.with(|c| c.set(false));
    out
}

thread_local! {
    static COALESCED_SHIFTS: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
}

/// Count one coalesced shift ([`crate::Grid::line_feeds_at_region_bottom`]) of `feeds` line
/// feeds in a region of `height` rows.
pub(crate) fn count_coalesced_shift(feeds: usize, height: usize) {
    COALESCED_SHIFTS.with(|c| {
        let [shifts, multi, deep] = c.get();
        c.set([
            shifts + 1,
            multi + usize::from(feeds > 1),
            deep + usize::from(feeds > height),
        ]);
    });
}

/// Take (read and reset) this thread's coalesced shifts: how many, how many coalesced more
/// than one line feed, and how many more line feeds than the region has rows (so some of
/// their lines were never written).
pub fn take_coalesced_shifts() -> [usize; 3] {
    COALESCED_SHIFTS.with(|c| c.replace([0; 3]))
}

thread_local! {
    static SHORT_SCREEN_SETTLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SCREEN_SETTLES: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
}

/// Whether this thread's settled screen line feeds ([`crate::Grid::settle_screen_line_feeds`])
/// record a scroll delta one short of their count: the planted fault a screen line-run
/// differential's negative control arms ([`with_short_screen_settle`]).
pub(crate) fn short_screen_settle() -> bool {
    SHORT_SCREEN_SETTLE.with(std::cell::Cell::get)
}

/// Run `f` with [`short_screen_settle`] armed on this thread, disarmed again after.
pub fn with_short_screen_settle<T>(f: impl FnOnce() -> T) -> T {
    SHORT_SCREEN_SETTLE.with(|c| c.set(true));
    let out = f();
    SHORT_SCREEN_SETTLE.with(|c| c.set(false));
    out
}

thread_local! {
    static SCREEN_TEXT_SLOT_ONE_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread's screen text lines ([`crate::Grid::screen_text_line`]) write their text
/// into the ring slot one before the row their line feed brought in: the planted off-by-one a
/// screen text-line differential's negative control arms ([`with_screen_text_slot_one_up`]).
pub(crate) fn screen_text_slot_one_up() -> bool {
    SCREEN_TEXT_SLOT_ONE_UP.with(std::cell::Cell::get)
}

/// Run `f` with [`screen_text_slot_one_up`] armed on this thread, disarmed again after.
pub fn with_screen_text_slot_one_up<T>(f: impl FnOnce() -> T) -> T {
    SCREEN_TEXT_SLOT_ONE_UP.with(|c| c.set(true));
    let out = f();
    SCREEN_TEXT_SLOT_ONE_UP.with(|c| c.set(false));
    out
}

thread_local! {
    static SCREEN_TEXT_RUNS: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
}

/// Count one run of `lines` screen text lines ([`crate::Grid::settle_screen_text_lines`]).
pub(crate) fn count_screen_text_run(lines: usize) {
    SCREEN_TEXT_RUNS.with(|c| {
        let [runs, total] = c.get();
        c.set([runs + 1, total + lines]);
    });
}

/// Take (read and reset) this thread's screen text-line runs: how many, and how many lines they
/// took in all.
pub fn take_screen_text_runs() -> [usize; 2] {
    SCREEN_TEXT_RUNS.with(|c| c.replace([0; 2]))
}

/// Count one settle of `feeds` screen line feeds ([`crate::Grid::settle_screen_line_feeds`]).
pub(crate) fn count_screen_settle(feeds: usize) {
    SCREEN_SETTLES.with(|c| {
        let [settles, multi] = c.get();
        c.set([settles + 1, multi + usize::from(feeds > 1)]);
    });
}

/// Take (read and reset) this thread's screen line-feed settles: how many, and how many settled
/// more than one line feed.
pub fn take_screen_settles() -> [usize; 2] {
    SCREEN_SETTLES.with(|c| c.replace([0; 2]))
}

#[cfg(test)]
thread_local! {
    static SHORT_STRADDLE_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread reads a staged row whose cells straddle the arena's ring one cell short:
/// the planted fault the staged-row model's negative control arms ([`with_short_straddle_read`]).
#[cfg(test)]
pub(crate) fn short_straddle_read() -> bool {
    SHORT_STRADDLE_READ.with(std::cell::Cell::get)
}

/// Run `f` with [`short_straddle_read`] armed on this thread, disarmed again after.
#[cfg(test)]
pub(crate) fn with_short_straddle_read<T>(f: impl FnOnce() -> T) -> T {
    SHORT_STRADDLE_READ.with(|c| c.set(true));
    let out = f();
    SHORT_STRADDLE_READ.with(|c| c.set(false));
    out
}

#[cfg(test)]
thread_local! {
    static SCREEN_TEXT_FLUSHES: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
    static SCREEN_TEXT_BATCH_IGNORES_HEADROOM: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Count one flush of `feeds` deferred screen text line feeds (`Grid::flush_screen_text_lines`),
/// whose shed found `stored` lines in the store and left `kept`.
#[cfg(test)]
pub(crate) fn count_screen_text_flush(feeds: usize, stored: usize, kept: usize) {
    SCREEN_TEXT_FLUSHES.with(|c| {
        let [batched, lines, over_store, emptied] = c.get();
        if feeds > 1 {
            c.set([
                batched + 1,
                lines + feeds,
                over_store + usize::from(stored != 0),
                emptied + usize::from(stored != 0 && kept == 0),
            ]);
        }
    });
}

/// Take (read and reset) this thread's flushes of more than one deferred screen text line feed:
/// how many, how many line feeds they flushed in all, how many found lines in the store, and how
/// many of those emptied it.
#[cfg(test)]
pub(crate) fn take_screen_text_flushes() -> [usize; 4] {
    SCREEN_TEXT_FLUSHES.with(|c| c.replace([0; 4]))
}

/// Whether this thread's screen text runs defer their line feeds' shed wherever the rest of
/// what [`crate::Grid::screen_text_shed_batch`] asks holds, with no regard to the store's memory
/// headroom: the planted fault the flood differential's negative control arms
/// ([`with_screen_text_batch_ignoring_headroom`]).
#[cfg(test)]
pub(crate) fn screen_text_batch_ignores_headroom() -> bool {
    SCREEN_TEXT_BATCH_IGNORES_HEADROOM.with(std::cell::Cell::get)
}

/// Run `f` with [`screen_text_batch_ignores_headroom`] armed on this thread, disarmed again
/// after.
#[cfg(test)]
pub(crate) fn with_screen_text_batch_ignoring_headroom<T>(f: impl FnOnce() -> T) -> T {
    SCREEN_TEXT_BATCH_IGNORES_HEADROOM.with(|c| c.set(true));
    let out = f();
    SCREEN_TEXT_BATCH_IGNORES_HEADROOM.with(|c| c.set(false));
    out
}

#[cfg(test)]
thread_local! {
    static SCREEN_TEXT_CUT_REFUSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SCREEN_TEXT_BATCH_IGNORES_CUT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Count one screen text run's deferral that an open flood cut alone refused
/// ([`crate::Grid::screen_text_shed_batch`]): staging was within the store's memory headroom.
#[cfg(test)]
pub(crate) fn count_screen_text_cut_refusal() {
    SCREEN_TEXT_CUT_REFUSALS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) this thread's count of deferrals an open flood cut alone refused.
#[cfg(test)]
pub(crate) fn take_screen_text_cut_refusals() -> usize {
    SCREEN_TEXT_CUT_REFUSALS.with(|c| c.replace(0))
}

/// Whether this thread's screen text runs defer their line feeds' shed though a flood cut is
/// open, wherever the rest of what [`crate::Grid::screen_text_shed_batch`] asks holds: the
/// planted fault the wide-row flood differential's negative control arms
/// ([`with_screen_text_batch_ignoring_cut`]).
#[cfg(test)]
pub(crate) fn screen_text_batch_ignores_cut() -> bool {
    SCREEN_TEXT_BATCH_IGNORES_CUT.with(std::cell::Cell::get)
}

/// Run `f` with [`screen_text_batch_ignores_cut`] armed on this thread, disarmed again after.
#[cfg(test)]
pub(crate) fn with_screen_text_batch_ignoring_cut<T>(f: impl FnOnce() -> T) -> T {
    SCREEN_TEXT_BATCH_IGNORES_CUT.with(|c| c.set(true));
    let out = f();
    SCREEN_TEXT_BATCH_IGNORES_CUT.with(|c| c.set(false));
    out
}

thread_local! {
    static ROW_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one write into a row that settles its length after it: a glyph from the
/// per-character writers, or a run.
#[cfg(test)]
pub(crate) fn count_row_write() {
    ROW_WRITES.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the row write count.
#[cfg(test)]
pub(crate) fn take_row_writes() -> usize {
    ROW_WRITES.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}
