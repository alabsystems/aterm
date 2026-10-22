// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread-local work and heap-allocation counters for `cfg(test)` regression
//! tests (`work_count_tests.rs`, `claude_card_tests.rs`).

use std::cell::Cell;

thread_local! {
    /// Layout plans built (`App::plan_tab`).
    static PLANS_BUILT: Cell<usize> = const { Cell::new(0) };
    /// Output stamps touched by the present walk.
    static LATENCY_STAMPS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    /// Sessions probed by the status sweep.
    static STATUS_PROBES: Cell<usize> = const { Cell::new(0) };
    /// Clipboard workers started (`spawn::install_clipboard`).
    static CLIPBOARD_WORKERS: Cell<usize> = const { Cell::new(0) };
    /// Tabs walked by the title-drift gate.
    static TITLE_TAB_TOUCHES: Cell<usize> = const { Cell::new(0) };
    /// The subscribe idle wake: retained ledger records (shell blocks, turn
    /// records, timeline events) an `events` digest touched.
    static RETAINED_RECORDS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    /// Ring rows (`history`, `timeline`, `exits`) formatted into a reply.
    static RING_ROWS_FORMATTED: Cell<usize> = const { Cell::new(0) };
    /// Styled frames gathered off a terminal (`gather_styled_frame`): one per
    /// frame change per session, however many readers want it.
    static STYLED_FRAMES_GATHERED: Cell<usize> = const { Cell::new(0) };
    /// Inline images base64-encoded for the wire (`image_payload_memo`): one
    /// per image while the memo keeps it, however many frames carry it.
    static IMAGES_ENCODED: Cell<usize> = const { Cell::new(0) };
    /// Engine samples an `events` read took (`subscribe::sample_engine_events`):
    /// the session's term lock, held for its blocks, title, bell and resize
    /// counters. At most one per `EVENTS_PACE` per watch under a flood, plus
    /// one per record (`session_watch::Watchers::ring_record`).
    static ENGINE_SAMPLES: Cell<usize> = const { Cell::new(0) };
    /// `events` reads a push pass made for an owed read come due, not for a
    /// ring or a record of its own (`subscribe::pass_streams`): a read that
    /// waited out the pace.
    static OWED_EVENTS_READS: Cell<usize> = const { Cell::new(0) };
    /// Subscribe watches read by a push pass (`subscribe::frames_for_watch`):
    /// the watches whose session rang, not every watch per wake.
    static WATCHES_VISITED: Cell<usize> = const { Cell::new(0) };
    /// Subscribe watches probed against the store for liveness
    /// (`subscribe::prune_closed`): none while the roster journal stands.
    static WATCHES_PROBED: Cell<usize> = const { Cell::new(0) };
    /// Windows asked whether they are on screen
    /// (`messages_host::band_on_screen`: off macOS, a round trip to the
    /// window server).
    static ON_SCREEN_ASKS: Cell<usize> = const { Cell::new(0) };
    /// Strip re-stamps a task's end asked for (`App::restamp_for_end`,
    /// design §11.2.3 item 7): one per end, one per look that quiets one.
    static END_RESTAMPS: Cell<usize> = const { Cell::new(0) };
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

pub(crate) fn ring_row_formatted() {
    bump(&RING_ROWS_FORMATTED);
}
pub(crate) fn take_ring_rows_formatted() -> usize {
    take(&RING_ROWS_FORMATTED)
}

pub(crate) fn styled_frame_gathered() {
    bump(&STYLED_FRAMES_GATHERED);
}
pub(crate) fn take_styled_frames_gathered() -> usize {
    take(&STYLED_FRAMES_GATHERED)
}

pub(crate) fn image_encoded() {
    bump(&IMAGES_ENCODED);
}
pub(crate) fn take_images_encoded() -> usize {
    take(&IMAGES_ENCODED)
}

pub(crate) fn engine_sampled() {
    bump(&ENGINE_SAMPLES);
}
pub(crate) fn take_engine_samples() -> usize {
    take(&ENGINE_SAMPLES)
}

pub(crate) fn owed_events_read() {
    bump(&OWED_EVENTS_READS);
}
pub(crate) fn take_owed_events_reads() -> usize {
    take(&OWED_EVENTS_READS)
}

pub(crate) fn watch_visited() {
    bump(&WATCHES_VISITED);
}
pub(crate) fn take_watches_visited() -> usize {
    take(&WATCHES_VISITED)
}

pub(crate) fn watch_probed() {
    bump(&WATCHES_PROBED);
}
pub(crate) fn take_watches_probed() -> usize {
    take(&WATCHES_PROBED)
}

pub(crate) fn on_screen_asked() {
    bump(&ON_SCREEN_ASKS);
}
pub(crate) fn take_on_screen_asks() -> usize {
    take(&ON_SCREEN_ASKS)
}

pub(crate) fn end_restamped() {
    bump(&END_RESTAMPS);
}
pub(crate) fn take_end_restamps() -> usize {
    take(&END_RESTAMPS)
}

pub(crate) fn timer_resolved() {
    bump(&TIMER_RESOLVES);
}
pub(crate) fn take_timer_resolves() -> usize {
    take(&TIMER_RESOLVES)
}

// ---------------------------------------------------------------------------
// The strip's hovers, as pushed
// ---------------------------------------------------------------------------

thread_local! {
    /// Each window's tab hovers as its strip was last stamped
    /// (`App::refresh_window_tabs`): what a native toolbar would show,
    /// which a headless window has none of. A hover is composed only at a
    /// push, so a test reads here whether a move stamped the strip again
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.3.3 item 3), where
    /// `App::tab_tooltips`, composed on the call, would always read fresh.
    static TOOLTIPS_PUSHED: std::cell::RefCell<Vec<(crate::WindowId, Vec<Option<String>>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// Strip stamps (`App::refresh_window_tabs`), every window's: one per
    /// push, so a test can say a move stamped a strip once, not twice.
    static TOOLTIP_PUSHES: Cell<usize> = const { Cell::new(0) };
    /// Redraws the hover's restamp asked for (`App::restamp_for_hover`):
    /// only where the stamp moved the strip's fingerprint.
    static HOVER_REDRAWS: Cell<usize> = const { Cell::new(0) };
}

/// The strip stamps since the last take.
pub(crate) fn take_tooltip_pushes() -> usize {
    take(&TOOLTIP_PUSHES)
}

/// The hover's restamp asked a window to redraw.
pub(crate) fn hover_redraw_asked() {
    bump(&HOVER_REDRAWS);
}

/// The redraws the hover's restamp asked for since the last take.
pub(crate) fn take_hover_redraws() -> usize {
    take(&HOVER_REDRAWS)
}

/// Window `window`'s strip was stamped with `tooltips`.
pub(crate) fn tooltips_pushed(window: crate::WindowId, tooltips: &[Option<String>]) {
    bump(&TOOLTIP_PUSHES);
    TOOLTIPS_PUSHED.with(|pushed| {
        let mut pushed = pushed.borrow_mut();
        match pushed.iter_mut().find(|(wid, _)| *wid == window) {
            Some((_, tips)) => tooltips.clone_into(tips),
            None => pushed.push((window, tooltips.to_vec())),
        }
    });
}

/// Window `window`'s tab hovers as its strip was last stamped, if it was.
pub(crate) fn last_tooltips_pushed(window: crate::WindowId) -> Option<Vec<Option<String>>> {
    TOOLTIPS_PUSHED.with(|pushed| {
        pushed
            .borrow()
            .iter()
            .find(|(wid, _)| *wid == window)
            .map(|(_, tips)| tips.clone())
    })
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
    /// Harness card frames the emitter DREW (`HarnessCard::draw`), as
    /// against a settled frame replayed (`HarnessCard::replay`).
    static CARD_TICKS: Cell<usize> = const { Cell::new(0) };
    /// Harness card layouts computed over the pane's cells
    /// (`claude_card::layout`).
    static CARD_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    /// Launch cards parsed off a pane's text (`claude_card::read_card`).
    static CARD_READS: Cell<usize> = const { Cell::new(0) };
    /// Update notices scanned for under a placed card
    /// (`claude_footer::PaneTextCache::update_notice`), as against a scan
    /// kept from the last rebuild of the pane's text.
    static CARD_NOTICE_READS: Cell<usize> = const { Cell::new(0) };
    /// Sessions' Autopilot reads (`App::session_autopilot`: one metadata
    /// lock each).
    static AUTOPILOT_READS: Cell<usize> = const { Cell::new(0) };
    /// Harness card lines and banners whose ink was laid
    /// (`HarnessCard::line_ink_into`, from `claude_card::ink_words`), as
    /// against a line or a banner kept from the last frame.
    static CARD_LINE_INKS: Cell<usize> = const { Cell::new(0) };
    /// Session timeline locks the footer's splice took
    /// (`App::splice_claude_footer`, the harness card's leaf beside it): the
    /// PTY reader holds that lock while output streams, so each one is a
    /// chance for the frame to wait.
    static FOOTER_TIMELINE_LOCKS: Cell<usize> = const { Cell::new(0) };
    /// Session timeline locks the dev card probe took to record a card
    /// pass's outcome (`App::compose_claude_cards` under
    /// `ATERM_DEV_CARD_PROBE=1`, card engine §8): one per pane the pass
    /// judged, none with the probe off.
    static CARD_PROBE_LOCKS: Cell<usize> = const { Cell::new(0) };
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
    /// Owned copies of a window's staged model a capture took to rasterize
    /// (`App::take_rerender_input`), as against reading the presented scratch
    /// in place or copying the presented destination.
    static CAPTURE_INPUTS_CLONED: Cell<usize> = const { Cell::new(0) };
    /// Output→present latency samples booked at presents a capture's barrier
    /// forced (`metrics::record_present`'s `capture_forced`): this thread's
    /// share of the `capture_presents` metrics key.
    static CAPTURE_FORCED_SAMPLES: Cell<usize> = const { Cell::new(0) };
    /// Output→present latency samples booked at every other present.
    static UNFORCED_SAMPLES: Cell<usize> = const { Cell::new(0) };
    /// Keystroke input→present slices a present booked
    /// (`metrics::record_present`): this thread's share of `n_input`.
    static INPUT_SLICES_BOOKED: Cell<usize> = const { Cell::new(0) };
    /// Of those, the slices a proven prediction closed, before the key's
    /// echo: this thread's share of `input_predicted`.
    static INPUT_SLICES_PREDICTED: Cell<usize> = const { Cell::new(0) };
    /// Chip blocks fitted and laid (`App::claude_lights_block`), as against
    /// a pane's block kept from a frame whose fit key was the same.
    static FOOTER_BLOCK_FITS: Cell<usize> = const { Cell::new(0) };
    /// The traffic published beside a session's facts read for its card
    /// (`App::fact_menu_rows`): at the card's open and its wakes, never by a
    /// frame or a shut card.
    static TRAFFIC_SIDE_READS: Cell<usize> = const { Cell::new(0) };
    /// Autopilot chip titles built (`claude_lights::autopilot_title`): one
    /// where a reading starts or its inputs moved, none while nobody reads.
    static AUTOPILOT_TITLES_BUILT: Cell<usize> = const { Cell::new(0) };
    /// Pane rows whose text a footer rebuild compared with the last
    /// rebuild's (`claude_footer::PaneTextCache::rows_as`), as against rows
    /// the engine's revisions vouch unchanged.
    static FOOTER_ROWS_COMPARED: Cell<usize> = const { Cell::new(0) };
    /// Composer top rules read off the frame for their plain run
    /// (`claude_footer::PaneTextCache::top_run`), as against a reading kept
    /// with the pane's rows.
    static FOOTER_TOP_READS: Cell<usize> = const { Cell::new(0) };
    /// Composer top rules composed (`claude_footer::compose_top`), as
    /// against a composition kept from a frame whose inputs were the same.
    static FOOTER_TOPS_COMPOSED: Cell<usize> = const { Cell::new(0) };
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

pub(crate) fn card_notice_read() {
    bump(&CARD_NOTICE_READS);
}
pub(crate) fn take_card_notice_reads() -> usize {
    take(&CARD_NOTICE_READS)
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

pub(crate) fn card_probe_locked() {
    bump(&CARD_PROBE_LOCKS);
}
pub(crate) fn take_card_probe_locks() -> usize {
    take(&CARD_PROBE_LOCKS)
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

pub(crate) fn footer_top_read() {
    bump(&FOOTER_TOP_READS);
}
pub(crate) fn take_footer_top_reads() -> usize {
    take(&FOOTER_TOP_READS)
}

pub(crate) fn footer_top_composed() {
    bump(&FOOTER_TOPS_COMPOSED);
}
pub(crate) fn take_footer_tops_composed() -> usize {
    take(&FOOTER_TOPS_COMPOSED)
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

pub(crate) fn capture_input_cloned() {
    bump(&CAPTURE_INPUTS_CLONED);
}
pub(crate) fn take_capture_inputs_cloned() -> usize {
    take(&CAPTURE_INPUTS_CLONED)
}

pub(crate) fn present_sample_booked(capture_forced: bool) {
    bump(if capture_forced {
        &CAPTURE_FORCED_SAMPLES
    } else {
        &UNFORCED_SAMPLES
    });
}
/// `(capture-forced, other)` latency samples booked since the last take.
pub(crate) fn take_present_samples() -> (usize, usize) {
    (take(&CAPTURE_FORCED_SAMPLES), take(&UNFORCED_SAMPLES))
}

pub(crate) fn input_slices_booked(booked: u32, predicted: u32) {
    for (counter, n) in [
        (&INPUT_SLICES_BOOKED, booked),
        (&INPUT_SLICES_PREDICTED, predicted),
    ] {
        counter.with(|c| c.set(c.get() + usize::try_from(n).unwrap_or(usize::MAX)));
    }
}
/// `(booked, of them predicted)` input slices since the last take.
pub(crate) fn take_input_slices() -> (usize, usize) {
    (take(&INPUT_SLICES_BOOKED), take(&INPUT_SLICES_PREDICTED))
}

pub(crate) fn footer_block_fitted() {
    bump(&FOOTER_BLOCK_FITS);
}

pub(crate) fn traffic_side_read() {
    bump(&TRAFFIC_SIDE_READS);
}
#[cfg(unix)]
pub(crate) fn take_traffic_side_reads() -> usize {
    take(&TRAFFIC_SIDE_READS)
}
pub(crate) fn autopilot_title_built() {
    bump(&AUTOPILOT_TITLES_BUILT);
}
pub(crate) fn take_autopilot_titles_built() -> usize {
    take(&AUTOPILOT_TITLES_BUILT)
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

thread_local! {
    /// Agents panels located and their cells read off a frame
    /// (`claude_panel::read_panel`): at most once per rebuild of a Claude
    /// Code pane's text, and only where the panel's gate holds.
    static PANEL_READS: Cell<usize> = const { Cell::new(0) };
    /// Folds and retimes of a panel's dots (`claude_panel`): a reading that
    /// moved, or a dot's next turn come due — never a frame at rest.
    static PANEL_FOLDS: Cell<usize> = const { Cell::new(0) };
    /// A panel's look made anew (its ink, halos and slabs), as against one
    /// kept from a frame of the same key and put on the glass as it is.
    static PANEL_LOOKS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn panel_read() {
    bump(&PANEL_READS);
}
pub(crate) fn take_panel_reads() -> usize {
    take(&PANEL_READS)
}

pub(crate) fn panel_folded() {
    bump(&PANEL_FOLDS);
}
pub(crate) fn take_panel_folds() -> usize {
    take(&PANEL_FOLDS)
}

pub(crate) fn panel_look_made() {
    bump(&PANEL_LOOKS);
}
pub(crate) fn take_panel_looks() -> usize {
    take(&PANEL_LOOKS)
}

thread_local! {
    /// Footer wakes armed (`claude_lights::wake_after`): one sleeping lane
    /// thread each, posting one wake.
    static FOOTER_WAKES_ARMED: Cell<usize> = const { Cell::new(0) };
    /// Claude's mode pill read for its relabel (`claude_footer::read_pane`).
    static PILL_RELABEL_READS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn pill_relabel_read() {
    bump(&PILL_RELABEL_READS);
}
#[cfg(unix)]
pub(crate) fn take_pill_relabel_reads() -> usize {
    take(&PILL_RELABEL_READS)
}

pub(crate) fn footer_wake_armed() {
    bump(&FOOTER_WAKES_ARMED);
}
#[cfg(unix)]
pub(crate) fn take_footer_wakes_armed() -> usize {
    take(&FOOTER_WAKES_ARMED)
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
