// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic WORK counters for the many-tab / many-session passes whose
//! wins are structural (docs/PERF-REGRESSION-DEFENCE.md, W-4) — the
//! `aterm_grid::test_counters` idiom, for this crate.
//!
//! Each counter sits at the site that does the WORK a win removed, never at
//! the site that makes the decision: a counter on the decision reads the same
//! whether or not the work behind it came back. The asserting tests live in
//! `work_count_tests.rs` (or beside the fixtures they need: the harness
//! card's in `claude_card_tests.rs`) and run in every `verify test`, which no
//! timing lane does.
//!
//! Thread-local, so parallel tests cannot bleed into each other; every counted
//! site runs on the calling (test) thread. Compiled only under `cfg(test)`.
//!
//! So is the test binary's heap counter ([`allocations_in`]): the global
//! allocator is `System`'s, counted per thread while a test asks.

use std::cell::Cell;

thread_local! {
    /// Layout plans built (`App::plan_tab`): the redraw plan-build
    /// cardinality, and MPT-2's discarded background-tab plans.
    static PLANS_BUILT: Cell<usize> = const { Cell::new(0) };
    /// MPT-3: output stamps touched (swapped or cleared) by the present walk.
    static LATENCY_STAMPS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    /// MPT-4: sessions probed by the status sweep.
    static STATUS_PROBES: Cell<usize> = const { Cell::new(0) };
    /// Clipboard workers started (`spawn::install_clipboard`): one per session
    /// that wrote the clipboard, none for one that never did.
    static CLIPBOARD_WORKERS: Cell<usize> = const { Cell::new(0) };
    /// MPT-5: tabs walked by the title-drift gate.
    static TITLE_TAB_TOUCHES: Cell<usize> = const { Cell::new(0) };
    /// The subscribe idle wake: retained ledger records (shell blocks, turn
    /// records, timeline events) an `events` digest touched.
    static RETAINED_RECORDS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    /// Windows asked whether they are on screen
    /// (`messages_host::band_on_screen`: off macOS, a round trip to the
    /// window server).
    static ON_SCREEN_ASKS: Cell<usize> = const { Cell::new(0) };
    /// Timed choices resolved to an instant (`App::resolve_timer_choice`, one
    /// wall read): one per press of the `Every session` row's Timer, however
    /// many sessions it arms, so they all land at that one instant.
    static TIMER_RESOLVES: Cell<usize> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    counter.with(|c| c.set(c.get() + 1));
}

fn take(counter: &'static std::thread::LocalKey<Cell<usize>>) -> usize {
    counter.with(|c| c.replace(0))
}

pub(crate) fn plan_built() {
    bump(&PLANS_BUILT);
}
pub(crate) fn take_plans_built() -> usize {
    take(&PLANS_BUILT)
}
pub(crate) fn plans_built() -> usize {
    PLANS_BUILT.with(Cell::get)
}

pub(crate) fn latency_stamp_touched() {
    bump(&LATENCY_STAMPS_TOUCHED);
}
pub(crate) fn take_latency_stamps_touched() -> usize {
    take(&LATENCY_STAMPS_TOUCHED)
}

pub(crate) fn clipboard_worker_started() {
    bump(&CLIPBOARD_WORKERS);
}
pub(crate) fn take_clipboard_workers_started() -> usize {
    take(&CLIPBOARD_WORKERS)
}

pub(crate) fn status_probe() {
    bump(&STATUS_PROBES);
}
pub(crate) fn take_status_probes() -> usize {
    take(&STATUS_PROBES)
}

pub(crate) fn title_tab_touched() {
    bump(&TITLE_TAB_TOUCHES);
}
pub(crate) fn take_title_tab_touches() -> usize {
    take(&TITLE_TAB_TOUCHES)
}

pub(crate) fn retained_record_touched() {
    bump(&RETAINED_RECORDS_TOUCHED);
}
pub(crate) fn take_retained_records_touched() -> usize {
    take(&RETAINED_RECORDS_TOUCHED)
}

pub(crate) fn on_screen_asked() {
    bump(&ON_SCREEN_ASKS);
}
pub(crate) fn take_on_screen_asks() -> usize {
    take(&ON_SCREEN_ASKS)
}

pub(crate) fn timer_resolved() {
    bump(&TIMER_RESOLVES);
}
pub(crate) fn take_timer_resolves() -> usize {
    take(&TIMER_RESOLVES)
}

// ---------------------------------------------------------------------------
// Heap allocations, per thread
// ---------------------------------------------------------------------------

thread_local! {
    /// Whether [`allocations_in`] is counting on this thread.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// Allocations and reallocations made on this thread while counting.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// The test binary's allocator: `System`'s, and a count of the calls a
/// thread inside [`allocations_in`] makes. Thread-local, as the counters
/// above are, so the suite's other threads never bleed into a count; a
/// const-initialised `Cell` has no destructor and never allocates, and
/// `try_with` answers nothing (no count) on a thread being torn down.
struct CountingAlloc;

impl CountingAlloc {
    fn note() {
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
        });
    }
}

// SAFETY: every method forwards to `System` with the caller's layout and
// pointer unchanged; the count beside it never allocates.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's layout.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        Self::note();
        // SAFETY: forwarded with the caller's pointer, layout and size.
        unsafe { std::alloc::System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: forwarded with the caller's pointer and layout.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Run `f` and say how many heap allocations (and reallocations) it made
/// on this thread: the per-frame paths' allocation budget, asserted.
pub(crate) fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING.with(|on| on.set(true));
    let out = f();
    COUNTING.with(|on| on.set(false));
    (out, ALLOCATIONS.with(|n| n.replace(0)))
}

// ---------------------------------------------------------------------------
// The harness card and chips (`claude_card`, `claude_lights`)
// ---------------------------------------------------------------------------

thread_local! {
    /// Clock reads the harness's per-frame and per-wake paths made
    /// (`claude_card::card_now`, the Autopilot's claim check, a chip
    /// click's read-back): none for a card at rest, a session with no
    /// lapsing claim or a window with no card.
    static HARNESS_CLOCK_READS: Cell<usize> = const { Cell::new(0) };
    /// Harness card frames the emitter DREW (`HarnessCard::tick`), as
    /// against a settled frame replayed from its copy.
    static CARD_TICKS: Cell<usize> = const { Cell::new(0) };
    /// Harness card layouts computed over the pane's cells
    /// (`claude_card::layout`).
    static CARD_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    /// Launch cards parsed off a pane's text (`claude_card::read_card`).
    static CARD_READS: Cell<usize> = const { Cell::new(0) };
    /// Sessions' Autopilot reads (`App::session_autopilot`: one metadata
    /// lock each).
    static AUTOPILOT_READS: Cell<usize> = const { Cell::new(0) };
    /// Harness card lines whose ink was laid (`HarnessCard::line_ink_into`,
    /// from `claude_card::make_line`), as against a line kept from the last
    /// frame.
    static CARD_LINE_INKS: Cell<usize> = const { Cell::new(0) };
    /// Session timeline locks the footer's splice took
    /// (`App::splice_claude_footer`, the harness card's leaf beside it): the
    /// PTY reader holds that lock while output streams, so each one is a
    /// chance for the frame to wait.
    static FOOTER_TIMELINE_LOCKS: Cell<usize> = const { Cell::new(0) };
    /// Rebuilds of a Claude Code pane's text (`claude_footer::PaneTextCache`):
    /// what a frame whose revision moved costs the footer, card or none.
    static PANE_TEXT_REBUILDS: Cell<usize> = const { Cell::new(0) };
    /// Clock reads the footer's own per-frame paths made (the lights' repaint
    /// key and block, `claude_lights::lights_now`): the floor the harness's
    /// pass is measured against.
    static FOOTER_CLOCK_READS: Cell<usize> = const { Cell::new(0) };
    /// Pane cells the harness card's layout tested, or tested again to keep
    /// a layout (`claude_card::PaneRow::blank`, `PaneCells::wide`): one a
    /// cell a test.
    static CARD_CELLS_READ: Cell<usize> = const { Cell::new(0) };
    /// Sorts of the card's recorded keys to take its light back out of a
    /// stream (`claude_card::remove_each_once`), as against a tail truncated
    /// or a stream that holds none of it.
    static CARD_STRIP_SORTS: Cell<usize> = const { Cell::new(0) };
    /// Light quads the harness card moved into the nova stream's pixels
    /// (`claude_card::nova_of`): none for a card replayed at rest, whose
    /// moved kitty and stars are kept.
    static CARD_NOVA_MOVES: Cell<usize> = const { Cell::new(0) };
    /// Frame cells the footer's write copied into the frame
    /// (`claude_footer::write_edits`): the chips' and the facts' cells and the
    /// card's line, never the rule's own glyphs between them.
    static FOOTER_CELLS_WRITTEN: Cell<usize> = const { Cell::new(0) };
    /// Claude Code panes' rows read for the footer and the lights
    /// (`claude_footer::read_pane`): the composer's frame searched for and
    /// the rule's mode row and the lights' pill read under it, as against
    /// readings kept from the last rebuild of the pane's text.
    static FOOTER_PANE_READS: Cell<usize> = const { Cell::new(0) };
    /// The composer's frame searched for again to say whether a rebuild's
    /// changed rows moved it (`claude_footer::Readings::after`): none where
    /// every changed row is above the rows the search read.
    static FOOTER_FRAME_SEARCHES: Cell<usize> = const { Cell::new(0) };
    /// Launch cards read off a pane's text for the footer's open model or
    /// effort (`claude_footer::PaneTextCache::footer_card`).
    static FOOTER_CARD_READS: Cell<usize> = const { Cell::new(0) };
    /// Footer rules fitted and composed from the facts and the chips
    /// (`claude_footer::PaneFooter::compose`), as against a composition kept
    /// from a frame whose inputs were the same.
    static FOOTER_RULES_COMPOSED: Cell<usize> = const { Cell::new(0) };
    /// Composer rules read off the frame for the cells the footer may cover
    /// (`claude_footer::PaneTextCache::rule_run`), as against a reading kept
    /// with the pane's rows.
    static FOOTER_RULE_MASKS: Cell<usize> = const { Cell::new(0) };
    /// Chrome band tones derived from a theme (`chrome_band::band_colors`),
    /// as against the last derivation's kept for the same theme.
    static BAND_DERIVES: Cell<usize> = const { Cell::new(0) };
    /// Chip blocks fitted and laid (`App::claude_lights_block`), as against
    /// a pane's block kept from a frame whose fit key was the same.
    static FOOTER_BLOCK_FITS: Cell<usize> = const { Cell::new(0) };
    /// Pane rows whose text a footer rebuild compared with the last
    /// rebuild's (`claude_footer::PaneTextCache::rows_as`), as against rows
    /// the engine's revisions vouch unchanged.
    static FOOTER_ROWS_COMPARED: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn harness_clock_read() {
    bump(&HARNESS_CLOCK_READS);
}
pub(crate) fn take_harness_clock_reads() -> usize {
    take(&HARNESS_CLOCK_READS)
}

pub(crate) fn card_ticked() {
    bump(&CARD_TICKS);
}
pub(crate) fn take_card_ticks() -> usize {
    take(&CARD_TICKS)
}

pub(crate) fn card_laid_out() {
    bump(&CARD_LAYOUTS);
}
pub(crate) fn take_card_layouts() -> usize {
    take(&CARD_LAYOUTS)
}

pub(crate) fn card_read() {
    bump(&CARD_READS);
}
pub(crate) fn take_card_reads() -> usize {
    take(&CARD_READS)
}

pub(crate) fn autopilot_read() {
    bump(&AUTOPILOT_READS);
}
pub(crate) fn take_autopilot_reads() -> usize {
    take(&AUTOPILOT_READS)
}

pub(crate) fn card_line_inked() {
    bump(&CARD_LINE_INKS);
}
pub(crate) fn take_card_line_inks() -> usize {
    take(&CARD_LINE_INKS)
}

pub(crate) fn footer_timeline_locked() {
    bump(&FOOTER_TIMELINE_LOCKS);
}
pub(crate) fn take_footer_timeline_locks() -> usize {
    take(&FOOTER_TIMELINE_LOCKS)
}

pub(crate) fn footer_clock_read() {
    bump(&FOOTER_CLOCK_READS);
}
pub(crate) fn take_footer_clock_reads() -> usize {
    take(&FOOTER_CLOCK_READS)
}

pub(crate) fn pane_text_rebuilt() {
    bump(&PANE_TEXT_REBUILDS);
}
pub(crate) fn take_pane_text_rebuilds() -> usize {
    take(&PANE_TEXT_REBUILDS)
}

pub(crate) fn card_cell_read() {
    bump(&CARD_CELLS_READ);
}
pub(crate) fn take_card_cells_read() -> usize {
    take(&CARD_CELLS_READ)
}

pub(crate) fn card_strip_sorted() {
    bump(&CARD_STRIP_SORTS);
}
pub(crate) fn take_card_strip_sorts() -> usize {
    take(&CARD_STRIP_SORTS)
}

pub(crate) fn card_nova_moved(quads: usize) {
    CARD_NOVA_MOVES.with(|c| c.set(c.get() + quads));
}
pub(crate) fn take_card_nova_moves() -> usize {
    take(&CARD_NOVA_MOVES)
}

pub(crate) fn footer_cells_written(cells: usize) {
    FOOTER_CELLS_WRITTEN.with(|c| c.set(c.get() + cells));
}
pub(crate) fn take_footer_cells_written() -> usize {
    take(&FOOTER_CELLS_WRITTEN)
}

pub(crate) fn footer_pane_read() {
    bump(&FOOTER_PANE_READS);
}
pub(crate) fn take_footer_pane_reads() -> usize {
    take(&FOOTER_PANE_READS)
}

pub(crate) fn footer_frame_searched() {
    bump(&FOOTER_FRAME_SEARCHES);
}
pub(crate) fn take_footer_frame_searches() -> usize {
    take(&FOOTER_FRAME_SEARCHES)
}

pub(crate) fn footer_card_read() {
    bump(&FOOTER_CARD_READS);
}
pub(crate) fn take_footer_card_reads() -> usize {
    take(&FOOTER_CARD_READS)
}

pub(crate) fn footer_rule_composed() {
    bump(&FOOTER_RULES_COMPOSED);
}
pub(crate) fn take_footer_rules_composed() -> usize {
    take(&FOOTER_RULES_COMPOSED)
}

pub(crate) fn band_derived() {
    bump(&BAND_DERIVES);
}
pub(crate) fn take_band_derives() -> usize {
    take(&BAND_DERIVES)
}

pub(crate) fn footer_rule_masked() {
    bump(&FOOTER_RULE_MASKS);
}
pub(crate) fn take_footer_rule_masks() -> usize {
    take(&FOOTER_RULE_MASKS)
}

pub(crate) fn footer_block_fitted() {
    bump(&FOOTER_BLOCK_FITS);
}
pub(crate) fn take_footer_block_fits() -> usize {
    take(&FOOTER_BLOCK_FITS)
}

pub(crate) fn footer_row_compared() {
    bump(&FOOTER_ROWS_COMPARED);
}
pub(crate) fn take_footer_rows_compared() -> usize {
    take(&FOOTER_ROWS_COMPARED)
}

// ---------------------------------------------------------------------------
// The renderer's (`aterm_render::test_counters`)
// ---------------------------------------------------------------------------

/// Window cell geometries the renderer derived off its primary face
/// (`aterm_render::Renderer::cell_geometry` with none kept for the px): a
/// metrics read, and a face parse for a variable primary such as SF Mono.
/// Counted where the renderer derives, on this thread. The renderer keeps no
/// counters in a build without debug assertions, so there this reads 0.
pub(crate) fn take_cell_geometry_derivations() -> usize {
    #[cfg(debug_assertions)]
    let taken = aterm_render::test_counters::take_cell_geometry_derivations();
    #[cfg(not(debug_assertions))]
    let taken = 0;
    taken
}
