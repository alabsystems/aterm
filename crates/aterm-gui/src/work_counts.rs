// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread-local work and heap-allocation counters for `cfg(test)` regression tests.

use std::cell::Cell;

thread_local! {
    static PLANS_BUILT: Cell<usize> = const { Cell::new(0) };
    static LATENCY_STAMPS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    static STATUS_PROBES: Cell<usize> = const { Cell::new(0) };
    static CLIPBOARD_WORKERS: Cell<usize> = const { Cell::new(0) };
    static TITLE_TAB_TOUCHES: Cell<usize> = const { Cell::new(0) };
    static RETAINED_RECORDS_TOUCHED: Cell<usize> = const { Cell::new(0) };
    static RING_ROWS_FORMATTED: Cell<usize> = const { Cell::new(0) };
    static STYLED_FRAMES_GATHERED: Cell<usize> = const { Cell::new(0) };
    static IMAGES_ENCODED: Cell<usize> = const { Cell::new(0) };
    static ENGINE_SAMPLES: Cell<usize> = const { Cell::new(0) };
    static OWED_EVENTS_READS: Cell<usize> = const { Cell::new(0) };
    static WATCHES_VISITED: Cell<usize> = const { Cell::new(0) };
    static WATCHES_PROBED: Cell<usize> = const { Cell::new(0) };
    static ON_SCREEN_ASKS: Cell<usize> = const { Cell::new(0) };
    static END_RESTAMPS: Cell<usize> = const { Cell::new(0) };
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

thread_local! {
    static TOOLTIPS_PUSHED: std::cell::RefCell<Vec<(crate::WindowId, Vec<Option<String>>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    static TOOLTIP_PUSHES: Cell<usize> = const { Cell::new(0) };
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

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Threads inside [`allocations_in`] now: while none is, an allocation
/// skips the thread-local read (unoptimised test builds pay more for that
/// read than for the allocation itself).
static COUNTING_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct CountingAlloc;

impl CountingAlloc {
    fn note() {
        if COUNTING_THREADS.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return;
        }
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
    struct Counting;
    impl Drop for Counting {
        fn drop(&mut self) {
            COUNTING.with(|on| on.set(false));
            COUNTING_THREADS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    ALLOCATIONS.with(|n| n.set(0));
    COUNTING_THREADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let counting = Counting;
    COUNTING.with(|on| on.set(true));
    let out = f();
    drop(counting);
    (out, ALLOCATIONS.with(|n| n.replace(0)))
}

thread_local! {
    static HARNESS_CLOCK_READS: Cell<usize> = const { Cell::new(0) };
    static CARD_TICKS: Cell<usize> = const { Cell::new(0) };
    static CARD_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    static CARD_READS: Cell<usize> = const { Cell::new(0) };
    static CARD_NOTICE_READS: Cell<usize> = const { Cell::new(0) };
    static AUTOPILOT_READS: Cell<usize> = const { Cell::new(0) };
    static CARD_LINE_INKS: Cell<usize> = const { Cell::new(0) };
    static FOOTER_TIMELINE_LOCKS: Cell<usize> = const { Cell::new(0) };
    static CARD_PROBE_LOCKS: Cell<usize> = const { Cell::new(0) };
    static PANE_TEXT_REBUILDS: Cell<usize> = const { Cell::new(0) };
    static FOOTER_CLOCK_READS: Cell<usize> = const { Cell::new(0) };
    static CARD_CELLS_READ: Cell<usize> = const { Cell::new(0) };
    static CARD_STRIP_SORTS: Cell<usize> = const { Cell::new(0) };
    static FOOTER_CELLS_WRITTEN: Cell<usize> = const { Cell::new(0) };
    static FOOTER_PANE_READS: Cell<usize> = const { Cell::new(0) };
    static FOOTER_FRAME_SEARCHES: Cell<usize> = const { Cell::new(0) };
    static FOOTER_CARD_READS: Cell<usize> = const { Cell::new(0) };
    static FOOTER_RULES_COMPOSED: Cell<usize> = const { Cell::new(0) };
    static FOOTER_RULE_MASKS: Cell<usize> = const { Cell::new(0) };
    static BAND_DERIVES: Cell<usize> = const { Cell::new(0) };
    static CAPTURE_INPUTS_CLONED: Cell<usize> = const { Cell::new(0) };
    static CAPTURE_FORCED_SAMPLES: Cell<usize> = const { Cell::new(0) };
    static UNFORCED_SAMPLES: Cell<usize> = const { Cell::new(0) };
    static INPUT_SLICES_BOOKED: Cell<usize> = const { Cell::new(0) };
    static INPUT_SLICES_PREDICTED: Cell<usize> = const { Cell::new(0) };
    static FOOTER_BLOCK_FITS: Cell<usize> = const { Cell::new(0) };
    static TRAFFIC_SIDE_READS: Cell<usize> = const { Cell::new(0) };
    static AUTOPILOT_TITLES_BUILT: Cell<usize> = const { Cell::new(0) };
    static FOOTER_ROWS_COMPARED: Cell<usize> = const { Cell::new(0) };
    static FOOTER_TOP_READS: Cell<usize> = const { Cell::new(0) };
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
    static PANEL_READS: Cell<usize> = const { Cell::new(0) };
    static PANEL_FOLDS: Cell<usize> = const { Cell::new(0) };
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
    static FOOTER_WAKES_ARMED: Cell<usize> = const { Cell::new(0) };
    static MODE_ROW_READS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn mode_row_read() {
    bump(&MODE_ROW_READS);
}
#[cfg(unix)]
pub(crate) fn take_mode_row_reads() -> usize {
    take(&MODE_ROW_READS)
}

pub(crate) fn footer_wake_armed() {
    bump(&FOOTER_WAKES_ARMED);
}
#[cfg(unix)]
pub(crate) fn take_footer_wakes_armed() -> usize {
    take(&FOOTER_WAKES_ARMED)
}

/// Window cell geometries the renderer derived off its primary face
/// (`aterm_render::Renderer::cell_geometry` with none kept for the px): a
/// metrics read, and a face parse for a variable primary such as SF Mono.
pub(crate) fn take_cell_geometry_derivations() -> usize {
    #[cfg(debug_assertions)]
    let taken = aterm_render::test_counters::take_cell_geometry_derivations();
    #[cfg(not(debug_assertions))]
    let taken = 0;
    taken
}

thread_local! {
    static SETTINGS_PAGES_BUILT: Cell<usize> = const { Cell::new(0) };
    static SETTINGS_LISTS_MEASURED: Cell<usize> = const { Cell::new(0) };
    static DOCK_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    static BAR_LAYOUTS: Cell<usize> = const { Cell::new(0) };
    static PAGE_WORK_UNCOUNTED: Cell<bool> = const { Cell::new(false) };
}

/// The settings page's work since the last [`take_settings_page_work`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SettingsPageWork {
    /// Pages built.
    pub(crate) pages: usize,
    /// Lists measured with no page built.
    pub(crate) measures: usize,
    /// A compact window's status docks laid out.
    pub(crate) docks: usize,
    /// Status bars laid out.
    pub(crate) bars: usize,
}

fn bump_page_work(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    if !PAGE_WORK_UNCOUNTED.with(Cell::get) {
        bump(counter);
    }
}

/// Run `f`, a check only test builds make, with the settings page's work in it uncounted.
pub(crate) fn page_work_uncounted<R>(f: impl FnOnce() -> R) -> R {
    let was = PAGE_WORK_UNCOUNTED.with(|off| off.replace(true));
    let out = f();
    PAGE_WORK_UNCOUNTED.with(|off| off.set(was));
    out
}

/// Whether the settings page's work is counted now, outside [`page_work_uncounted`].
pub(crate) fn page_work_counted() -> bool {
    !PAGE_WORK_UNCOUNTED.with(Cell::get)
}

pub(crate) fn settings_page_built() {
    bump_page_work(&SETTINGS_PAGES_BUILT);
}
pub(crate) fn settings_list_measured() {
    bump_page_work(&SETTINGS_LISTS_MEASURED);
}
pub(crate) fn dock_laid_out() {
    bump_page_work(&DOCK_LAYOUTS);
}
pub(crate) fn bar_laid_out() {
    bump_page_work(&BAR_LAYOUTS);
}
pub(crate) fn take_settings_page_work() -> SettingsPageWork {
    SettingsPageWork {
        pages: take(&SETTINGS_PAGES_BUILT),
        measures: take(&SETTINGS_LISTS_MEASURED),
        docks: take(&DOCK_LAYOUTS),
        bars: take(&BAR_LAYOUTS),
    }
}
