// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal input processing.
//!
//! Handles the main `process()` entry point.

use super::Terminal;

use aterm_types::duration_to_nanos;

/// A single reading of the clocks the terminal pipeline depends on, captured
/// once per [`Terminal::process_at`] batch.
///
/// Folding both clocks into one snapshot makes the time the pipeline observes
/// an explicit input: [`Terminal::process`] captures it live, while replay
/// feeds back the recorded reading so the same `(bytes, ClockReading)` schedule
/// reproduces identical grid/cursor/mode state and shell-integration mark
/// timestamps — the determinism the hydratable temporal buffer relies on.
#[derive(Debug, Clone, Copy)]
pub struct ClockReading {
    /// Monotonic instant — drives the bell rate-limit and mode-2026 timeout
    /// (compared only as deltas, never serialized). `web_time::Instant` is
    /// std::time on native (byte-identical) and the JS clock on wasm32.
    pub monotonic: web_time::Instant,
    /// Wall-clock epoch milliseconds recorded into OSC 133/633 command and
    /// output marks. `None` when the platform clock is unavailable.
    pub wall_ms: Option<u64>,
}

impl ClockReading {
    /// Capture the live clocks. This is the sole wall-clock read in the
    /// production processing pipeline; everything downstream observes the
    /// captured values via `transient`.
    #[must_use]
    pub fn now() -> Self {
        Self {
            monotonic: web_time::Instant::now(), // CLOCK-EXEMPT: sole pipeline clock capture (web_time = std on native, JS clock on wasm)
            wall_ms: crate::terminal::shell::current_time_ms(), // CLOCK-EXEMPT: sole pipeline clock capture
        }
    }
}

/// Whether one retained splice accounts for the complete monotonic-row advance
/// observed during a [`Terminal::process_at`] batch.
///
/// Selection boundary recovery is sound only under this coupling invariant:
/// every mutation that advances `absolute_row_counter` without recording that
/// advance in `record_absolute_row_splice` must also make
/// `content_scroll_delta` nonzero. The latter routes a mixed batch to the
/// fail-closed selection-clear arm instead of the piecewise splice projection.
#[inline]
fn splice_accounts_for_batch_row_advance(before: u64, after: u64, inserted: u64) -> bool {
    after
        .checked_sub(before)
        .is_some_and(|advance| advance == inserted)
}

impl Terminal {
    /// Process input bytes through the parser, reading the live clocks for any
    /// time-dependent state (bell rate-limit, mode-2026 sync timeout, shell
    /// command-mark timestamps).
    ///
    /// This is the production entry point. It is exactly
    /// [`process_at`](Self::process_at) with [`ClockReading::now()`], so its
    /// observable output is unchanged from before the clock seam existed.
    #[inline]
    pub fn process(&mut self, input: &[u8]) {
        self.process_at(input, ClockReading::now());
    }

    /// Process input bytes through the parser at a caller-supplied clock reading.
    ///
    /// Every state-affecting time read in the pipeline — the bell rate-limiter,
    /// `sync_start` arming, the mode-2026 timeout, and the OSC 133/633 command
    /// and output marks — observes `clock` (stashed in `transient.process_now`
    /// / `transient.process_wall_ms`) instead of reading the host clock
    /// independently. Feeding a fixed `(bytes, ClockReading)` schedule therefore
    /// reproduces identical grid/cursor/mode state and mark timestamps
    /// regardless of real wall-clock pacing — the determinism the hydratable
    /// temporal buffer relies on for faithful replay. [`process`](Self::process)
    /// is the live wrapper that passes [`ClockReading::now()`].
    ///
    /// Diagnostic-only wall-clock reads (the gated pipeline profiling
    /// timestamps) and peer-facing rate limiting (response-buffer token
    /// bucket) intentionally still read real time: neither feeds hydrated
    /// terminal state.
    #[allow(
        clippy::too_many_lines,
        reason = "main entry point with sequential processing stages"
    )]
    pub fn process_at(&mut self, input: &[u8], clock: ClockReading) {
        // Logical clocks for this batch: the single readings every downstream
        // time-dependent read observes. Set before the parser/post_process run.
        self.transient.process_now = clock.monotonic;
        self.transient.process_wall_ms = clock.wall_ms;
        // SCR-1: if the user has scrolled back into history (display_offset > 0),
        // PIN the viewport across this batch of output instead of yanking it to
        // the live bottom (the old unconditional scroll_to_bottom). VT row
        // arithmetic and write targeting require display_offset == 0 during
        // processing (row_index maps visible rows through display_offset), so we
        // reset to 0 for the duration, then RE-PIN afterward by the number of
        // lines that entered scrollback while processing. This keeps `tail -f`
        // and similar live output from disrupting a user reading scrollback.
        //
        // The "start typing snaps to bottom" behavior is unaffected: that path
        // calls Terminal::scroll_to_bottom() explicitly on keyboard input, not
        // through process().
        let pinned_offset = self.grid.display_offset();
        let lines_before = self.grid.absolute_row_counter();
        // SELECTION CUSTODY Phase 3: remember which screen this batch STARTED on.
        // If it ends on the other one, `self.grid` at the epilogue is no longer the
        // grid whose offset was pinned here — see the re-pin below.
        let was_alt = self.modes.alternate_screen;
        if pinned_offset > 0 {
            self.grid.scroll_to_bottom();
        }
        // PRESS CUSTODY, site 1 of 3 is `pinned_offset` above — the pre-batch reading
        // position, read one statement before the line that forces it to 0. It is the
        // ONLY discriminator between output that arrived at LIVE and output that
        // arrived while the user was reading history, and nothing downstream can
        // reconstruct it: by `post_process` the offset is 0 whichever it was. Site 2
        // fills this in (the damage classification); site 3, after the re-pin below,
        // emits. Deferred rather than `= None`-initialised so the compiler, not a
        // convention, holds both `post_process` arms to assigning it.
        let damage_class: super::custody::OutputDamage;
        // …and whether a selection was alive when the batch began. `post_process` has
        // FIVE ways to destroy one that are not damage overlap (a malformed splice
        // projection, a splice mixed with another scroll, an alt exit mid-batch, the
        // `left_alt` upper-bound re-check, and a whole-interval eviction at the floor),
        // and a record that could not see them would report an ordinary output step
        // for a batch that took the user's highlight. One enum-discriminant read.
        let selection_before = self.text_selection.has_selection();

        // split_for_process() is generated by define_terminal_handler! in
        // handler.rs — struct definition and construction stay in sync (#3560).
        //
        // Pipeline timestamps are gated behind a runtime check to eliminate
        // 6 × Instant::now() calls (~150ns) from the hot path. The profiling
        // data is useful for development but not worth the overhead in
        // production throughput. Part of throughput optimization Wave 1.
        if self.transient.pipeline_timestamps.profiling_enabled {
            let entry = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), measures real latency, not grid state (web_time = std on native, JS clock on wasm)
            let parse_start = entry;
            {
                let (parser, mut handler) = self.split_for_process();
                parser.advance_fast(input, &mut handler);
            }
            // RIS sets pending_parser_reset because the parser can't be reset
            // from inside its own dispatch loop (#7153).
            if self.transient.pending_parser_reset {
                self.parser.reset();
                // Clear session-only state not accessible from the handler (#7336).
                self.secure_keyboard_entry = false;
                // Kill the parked selection HERE, not by leaning on the park/restore
                // below and not on the `All` that the reset's `erase_scrollback`
                // happens to record on the way past.
                //
                // RIS does swap the main grid back (`reset_common_fields`), but that
                // does NOT guarantee the restore fires: for `\x1bc\x1b[?1049h` — RIS
                // then re-enter alt in ONE batch — `was_alt` and
                // `modes.alternate_screen` are both true at `post_process`, so
                // neither the park nor the restore runs. That is exactly the case
                // this line is load-bearing for: without it the stale pre-RIS main
                // selection sits in the slot and is restored on the NEXT `?1049l`,
                // over a grid the reset already erased. The reason it must go is the
                // reset itself.
                self.parked_text_selection.clear();
                self.transient.pending_parser_reset = false;
            }
            let parse_end = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), not grid state (web_time = std on native, JS clock on wasm)

            let grid_start = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), not grid state (web_time = std on native, JS clock on wasm)
            damage_class = self.post_process(lines_before, was_alt);
            // Observation Kernel (L0): evaluate + latch armed watchers at the one
            // seam where this batch's mutation has landed. `process_now` is the
            // injected clock (never read here), so this is replay-deterministic.
            self.observe_at(self.transient.process_now);
            let grid_end = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), not grid state (web_time = std on native, JS clock on wasm)

            self.record_pipeline_timestamps(
                parse_end - parse_start,
                grid_end - grid_start,
                entry.elapsed(),
                input.len(),
            );
        } else {
            {
                let (parser, mut handler) = self.split_for_process();
                parser.advance_fast(input, &mut handler);
            }
            // RIS sets pending_parser_reset because the parser can't be reset
            // from inside its own dispatch loop (#7153).
            if self.transient.pending_parser_reset {
                self.parser.reset();
                // Clear session-only state not accessible from the handler (#7336).
                self.secure_keyboard_entry = false;
                // Kill the parked selection HERE, not by leaning on the park/restore
                // below and not on the `All` that the reset's `erase_scrollback`
                // happens to record on the way past.
                //
                // RIS does swap the main grid back (`reset_common_fields`), but that
                // does NOT guarantee the restore fires: for `\x1bc\x1b[?1049h` — RIS
                // then re-enter alt in ONE batch — `was_alt` and
                // `modes.alternate_screen` are both true at `post_process`, so
                // neither the park nor the restore runs. That is exactly the case
                // this line is load-bearing for: without it the stale pre-RIS main
                // selection sits in the slot and is restored on the NEXT `?1049l`,
                // over a grid the reset already erased. The reason it must go is the
                // reset itself.
                self.parked_text_selection.clear();
                self.transient.pending_parser_reset = false;
            }
            damage_class = self.post_process(lines_before, was_alt);
            // Observation Kernel (L0): see the gated branch above — same seam,
            // same injected clock, replay-deterministic.
            self.observe_at(self.transient.process_now);

            // Lightweight: just bump sequence counter and record byte count.
            let ts = &mut self.transient.pipeline_timestamps;
            ts.last_process_bytes = u32::try_from(input.len()).unwrap_or(u32::MAX);
            ts.process_sequence = ts.process_sequence.wrapping_add(1);
        }

        // SCR-1 re-pin: if the user was scrolled back, restore the viewport to
        // the same content by advancing display_offset by the number of lines
        // that entered scrollback during processing (the rise in the monotonic
        // absolute row counter). Clamped to scrollback_lines() so the invariant
        // display_offset <= scrollback_lines() holds even if eviction discarded
        // some of those lines. If no new lines scrolled in, the offset is simply
        // restored unchanged.
        //
        // SELECTION CUSTODY Phase 3 — THE RE-PIN MUST TARGET THE GRID IT PINNED.
        //
        // The old code always repinned `self.grid`, with the note "the alt screen
        // has no scrollback, so on alt the counter does not rise and the offset
        // stays 0 — correct." That reasoning holds for a batch that was ALREADY on
        // alt. It is wrong for the batch that ENTERS alt, and that batch is the
        // whole bug: `enter_alternate_screen_raw` does
        // `mem::replace(self.grid, new_grid)` (`handler_dec.rs`), so by the time we
        // get here `self.grid` is the fresh ALT grid — whose `scrollback_lines()` is
        // 0, so the repin clamps to 0 and does nothing — while the user's MAIN grid
        // is sitting in `alt_grid` carrying the zero the prologue forced on it.
        //
        // Net effect before this fix: running `less`, `man`, `vim`, `fzf` or
        // `git log` while scrolled back into history destroyed the reading position
        // PERMANENTLY. Exit restored the main grid wholesale — including its
        // display_offset of 0.
        //
        // So repin the grid that was actually pinned. `repin_display_offset` clamps
        // to that grid's own `scrollback_lines()`, so `DisplayOffsetValid` still
        // holds for whichever grid we touch.
        //
        // Its `lines_added` is the advance of the SAME grid's counter, measured to
        // the moment it was parked — NOT 0. The comment that used to justify 0 said
        // "a grid that has been swapped out stopped receiving output"; it stopped
        // receiving it only AFTER the swap, and output before the smcup in the same
        // read is routine (`"a\r\nb\r\nc\r\n\x1b[?1049h"`). See
        // `park_main_row_counter`.
        let parked_main_counter = self.transient.alt_park_main_row_counter.take();
        if pinned_offset > 0 {
            let entered_alt = !was_alt && self.modes.alternate_screen;
            if entered_alt {
                // `lines_before` and the parked counter are BOTH the main grid's, so
                // their difference is a statement about one coordinate space. The
                // fallback keeps the old behaviour if no park was recorded (it always
                // is: every enter path records one before its swap).
                let lines_added = parked_main_counter
                    .unwrap_or(lines_before)
                    .saturating_sub(lines_before);
                if let Some(main_grid) = self.alt_grid.as_mut() {
                    main_grid.repin_display_offset(pinned_offset, lines_added);
                }
            } else {
                let lines_added = self
                    .grid
                    .absolute_row_counter()
                    .saturating_sub(lines_before);
                self.grid.repin_display_offset(pinned_offset, lines_added);
            }
        }

        // The OTHER pin this batch can hold: one an rmcup took off a grid it swapped
        // back in mid-batch (`flatten_restored_display_offset`). That grid processed
        // the rest of the batch at offset 0, as everything downstream of `row_index`
        // requires, so the reading position is restored HERE — advanced by whatever
        // entered its scrollback after the swap, exactly like the prologue's pin.
        //
        // The two are mutually exclusive in practice (a batch that starts scrolled
        // back on main parks that grid at the forced 0, so its rmcup restores 0 and
        // records nothing), but if both ever fired this one is the later, more
        // specific reading and must win.
        if let Some((restored_offset, counter_at_swap)) = self.transient.alt_restore_pin.take() {
            // The restored grid is the one the batch ENDED on, unless the batch went
            // on to re-enter alt — then it is parked again and lives in `alt_grid`.
            let restored_grid = if self.modes.alternate_screen {
                self.alt_grid.as_mut()
            } else {
                Some(&mut self.grid)
            };
            if let Some(grid) = restored_grid {
                let lines_added = grid.absolute_row_counter().saturating_sub(counter_at_swap);
                grid.repin_display_offset(restored_offset, lines_added);
            }
        }

        // PRESS CUSTODY, site 3 of 3: EMIT. The first moment at which the pre-batch
        // reading position (`pinned_offset`, latched before the prologue forced it to
        // 0), the damage verdict (`damage_class`, classified inside `post_process`)
        // and the FINAL post-re-pin offset are all simultaneously true. An emit one
        // block earlier would report offset 0 for every batch, producing a trace that
        // is uniformly consistent and worthless.
        //
        // SUPPRESSED on a screen-SWITCHING batch. When the batch ends on the other
        // screen, `pinned_offset` belongs to the outgoing grid and `self.grid`'s
        // offset belongs to the incoming one, so a before/after pair would relate two
        // unrelated coordinate spaces. Recording nothing is honest; recording a step
        // across the swap would not be.
        if was_alt == self.modes.alternate_screen {
            let lines_added = self
                .grid
                .absolute_row_counter()
                .saturating_sub(lines_before);
            self.note_output_custody(pinned_offset, damage_class, lines_added, selection_before);
        }

        // Note: tmux DCS passthrough (`ESC P tmux; ... ST`) works via natural
        // parser breakout -- the parser's "anywhere" ESC transition breaks DCS
        // before escaped content accumulates, so inner sequences parse normally
        // without re-injection. The former pending_passthrough drain loop was
        // dead code and was removed in #7776.

        // INTEGRITY-SELFCHECK (M7): in debug builds, validate the active grid's
        // structural invariants (CursorInBounds / ScrollRegionValid /
        // DisplayOffsetValid / ring-buffer) at this public processing boundary —
        // the exact, fuzz-validated checks `fuzz_process_never_panics` asserts,
        // now continuously on every `process` batch in development. Free in release
        // (compiled out). The whole test suite exercises it, so any change that
        // corrupts the grid trips here immediately.
        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();
    }

    /// Store per-stage pipeline durations in transient state (#5560).
    #[inline]
    fn record_pipeline_timestamps(
        &mut self,
        parse: std::time::Duration,
        grid: std::time::Duration,
        total: std::time::Duration,
        bytes: usize,
    ) {
        let ts = &mut self.transient.pipeline_timestamps;
        ts.parse_duration_ns = duration_to_nanos(parse);
        ts.grid_duration_ns = duration_to_nanos(grid);
        ts.process_total_ns = duration_to_nanos(total);
        ts.last_process_bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
        ts.process_sequence = ts.process_sequence.wrapping_add(1);
    }

    /// Post-processing after parser advances: selection adjustment and BiDi sync.
    ///
    /// `was_alt` is which screen the batch STARTED on, captured by the caller
    /// before the parser ran — the batch may have switched screens since, and this
    /// is the only place that can tell.
    ///
    /// SELECTION CUSTODY — this is the `SelectionCustody` machine's ENGINE half, and
    /// it carries FOUR spec actions because it asks four different questions of the
    /// one selection in one pass (the macro's `write_all` precedent: one method may
    /// legitimately implement more than one spec action):
    ///
    /// * `UniformScroll` — the `(None, delta)` arm's `adjust_for_scroll`. The anchors
    ///   ride the content, so in the ABSOLUTE space the interval does not move.
    /// * `RegionDamageLow` / `RegionDamageHigh` — the damage test
    ///   (`SelectionDamage::clears_selection` over `intersects_absolute_band`). The two
    ///   model arms are the two halves of the lattice — damage that MISSED must spare
    ///   the selection, damage that HIT must clear it — and one seam decides both.
    /// * `WholesaleInvalidate` — the `SelectionDamage::All` arm of the SAME damage
    ///   test. ED 3, RIS and a Kitty unscroll destroy the coordinate space the anchors
    ///   are stated in, and this is where that reaches the selection.
    ///
    /// `Evict` is deliberately NOT anchored here. The tail's `truncate_to_floor` is an
    /// unconditional RE-clamp against a floor something else raised, so it runs on every
    /// batch and discriminates nothing; an anchor on it would name code no test could
    /// falsify. `Evict`'s one anchor is [`Terminal::set_scrollback_line_limit`], the
    /// entry point that actually raises the floor.
    ///
    /// This is the CONSUMER, deliberately: `Grid` records bands and deltas but owns no
    /// `text_selection`, so a grid-layer anchor could not witness `alive` at all. Tier-1
    /// drives these through real `Terminal::process` batches in
    /// `aterm_gui::selection_custody_conformance`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "UniformScroll",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "RegionDamageLow",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "RegionDamageHigh",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "WholesaleInvalidate",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[allow(clippy::too_many_lines)] // the crate's per-function convention (lib.rs)
    fn post_process(
        &mut self,
        absolute_rows_before: u64,
        was_alt: bool,
    ) -> super::custody::OutputDamage {
        // Top-anchored partial scrollback inserts logical rows immediately
        // before a protected footer. Keep every durable OSC/shell anchor paired
        // with the footer content that stayed fixed on screen.
        if let Some(update) = self.grid.take_absolute_row_update() {
            super::handler::apply_absolute_row_update(
                update,
                &mut self.shell,
                &mut self.marks_state,
                &mut self.semantic,
                &mut self.transient,
                &mut self.absolute_row_revision,
            );
        }

        // SELECTION CUSTODY, screen-scoped selection (the Phase-3 remainder).
        // A selection belongs to the SCREEN it was made on. Park the outgoing
        // screen's on the batch that leaves it; restore the incoming screen's on
        // the batch that comes back.
        //
        // This is the top of `post_process` on purpose, and neither of the two more
        // obvious homes works:
        //
        // - The VT handler runs MID-batch. Parking there would hand the rest of the
        //   batch's row-splice / scroll-delta / damage drain below to the wrong
        //   selection, and those signals are drained from `self.grid`, which by then
        //   is already the incoming grid.
        // - The SCR-1 epilogue runs AFTER that drain. By then a 1049 enter has
        //   already compared the band `enter_alternate_screen`'s `new_grid
        //   .erase_screen()` recorded — computed from the FRESH ALT grid's
        //   `absolute_row_counter`, so `[0, rows-1]` — against MAIN-screen anchors.
        //   `SelectionDamage::Band` carries no grid identity, so that comparison
        //   type-checks, runs, and silently relates two unrelated coordinate spaces:
        //   a main LIVE selection is destroyed and a main SCROLLBACK selection (whose
        //   absolutes are negative) survives, for no reason either one could name.
        //
        // Placed here, `self.grid` and `self.text_selection` are the same screen's
        // for the whole rest of the function.
        //
        // `mem::take` in both directions, deliberately NOT a swap: the alt screen's
        // own selection dies at exit rather than becoming a second durable selection
        // with its own lifetime. See the field doc for why that asymmetry is what
        // keeps the clear-site list finite.
        //
        // Both arms compare the batch's START screen with its END screen, so a batch
        // that EXITS and RE-ENTERS (`\x1b[?1049l\x1b[?47h` — one destroys the alt
        // buffer, the other allocates a fresh blank one) runs NEITHER, and the dead
        // alt screen's anchors would stay live over a buffer that never held the
        // selected text. `alt_screen_left_in_batch` is the exit paths' report that
        // this happened; the 47-family re-entry records no damage of its own, so
        // nothing else would clear it.
        let left_alt = was_alt && !self.modes.alternate_screen;
        let exited_mid_batch = std::mem::take(&mut self.transient.alt_screen_left_in_batch);
        if left_alt {
            self.text_selection = std::mem::take(&mut self.parked_text_selection);
        } else if !was_alt && self.modes.alternate_screen {
            self.parked_text_selection = std::mem::take(&mut self.text_selection);
        } else if exited_mid_batch && self.modes.alternate_screen {
            self.text_selection.clear();
        }

        // Adjust selection coordinates after content scroll (#4056). A
        // top-anchored archival region is the one non-uniform case we can map
        // exactly: rows before the protected footer move toward history while
        // footer rows stay fixed on screen. The grid retains this update
        // independently because parser-order metadata handling may already
        // have drained `take_absolute_row_update()` above.
        let selection_row_update = self.grid.take_selection_row_update();
        let scroll_delta = self.grid.take_content_scroll_delta();
        // Preserve the grid's precise, per-batch coordinate-motion verdict in
        // a cumulative read-only projection before selection handling consumes
        // it. A positive finite delta is composable only on the PRIMARY screen:
        // alt-screen applications own/repaint their buffer and must invalidate
        // cached host coordinates. Splices, region/down/erase sentinels, and
        // any future negative delta are likewise non-uniform or ambiguous.
        // SELECTION CUSTODY Phase 4: `scroll_delta == i32::MAX` used to carry this.
        // That sentinel had TWO jobs — invalidate host coordinate caches, and clear
        // the selection — and Phase 4 split them, because they are different
        // questions (see `coordinates_invalidated` in the grid state). The selection
        // half moved to the damage lattice; this half is now an explicit flag the
        // row-MOVING ops set. Without it, removing the sentinel would have silently
        // broken the documented `ContentScrollState::invalidation_epoch` contract for
        // every host caching grid coordinates.
        let invalidates_coordinates = selection_row_update.is_some()
            || self.grid.take_coordinates_invalidated()
            || scroll_delta == i32::MAX
            || scroll_delta < 0
            || (self.modes.alternate_screen && scroll_delta > 0);
        if invalidates_coordinates {
            self.content_scroll_state.invalidate();
        } else if scroll_delta > 0 {
            self.content_scroll_state
                .record_uniform_up(u64::try_from(scroll_delta).unwrap_or(u64::MAX));
        }
        let max_rows = i32::from(self.grid.rows());
        // Lower clear bound is the retained-history floor, not the visible
        // height: a scrollback selection has rows down to -scrollback_lines
        // and stays on screen across output (SCR-1 view pinning), so it must
        // survive ordinary content scroll and only clear once an endpoint is
        // truly evicted below the floor (or pushed above the live bottom).
        let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        match (selection_row_update, scroll_delta) {
            (None, 0) => {}
            (None, delta) => {
                self.text_selection
                    .adjust_for_scroll(delta, max_rows, floor);
            }
            (Some(aterm_grid::AbsoluteRowUpdate::Splice { at, inserted }), 0) => {
                // With no other selection-affecting operation in this batch,
                // the live top advanced exactly by `inserted`. Recover the
                // pre-splice footer boundary in terminal-relative coordinates.
                // This branch relies on every OTHER absolute-row advance also
                // setting `content_scroll_delta`, which would select the mixed-
                // operation clear arm below. Enforce that cross-field contract
                // at the process boundary in every debug/test build.
                let absolute_rows_after = self.grid.absolute_row_counter();
                // EXEMPT a batch that switched screens. `absolute_rows_before` was read
                // from the OUTGOING grid and `absolute_rows_after` from the INCOMING
                // one; the two counters are unrelated (a fresh alt grid restarts at
                // `rows`), so the accounting identity is not merely violated, it is not
                // even a statement about one coordinate space. The projection below is
                // unaffected: `at`, `inserted` and `absolute_rows_after` all come from
                // the incoming grid, so it stays self-consistent.
                //
                // The EXIT direction is live today: a top-anchored DECSTBM archival
                // scroll on main strands a splice on the main grid — nothing drains a
                // parked grid — and the batch that swaps it back drains it. This arm was
                // unreachable on a switching batch until now only because
                // `force_selection_invalidation` forced `scroll_delta == i32::MAX` into
                // the fail-closed `(Some(_), _)` arm.
                //
                // The guard is symmetric anyway. The ENTER direction cannot reach it as
                // the code stands — `enter_alternate_screen_raw` reuses the persistent
                // alt buffer, but that buffer can never RECORD a splice: the archival
                // top-anchored path needs `max_scrollback > 0 || scrollback.is_some() ||
                // scrollback_detached_for_reflow` and the alt grid is always
                // `Grid::with_scrollback(rows, cols, 0)` with no tiered store. That is a
                // property of the alt grid's construction, not of this arm, so do not
                // narrow the guard to match it.
                debug_assert!(
                    was_alt != self.modes.alternate_screen
                        || splice_accounts_for_batch_row_advance(
                            absolute_rows_before,
                            absolute_rows_after,
                            inserted,
                        ),
                    "zero-delta selection splice must account for the batch's entire absolute-row advance: before={absolute_rows_before}, after={absolute_rows_after}, inserted={inserted}; every other absolute_row_counter bump must set content_scroll_delta (a batch that also switched screens is exempt: the two counters belong to different grids)",
                );
                let new_live_top = absolute_rows_after.saturating_sub(u64::from(self.grid.rows()));
                let projection = new_live_top
                    .checked_sub(inserted)
                    .and_then(|old_live_top| at.checked_sub(old_live_top))
                    .and_then(|boundary| i32::try_from(boundary).ok())
                    .zip(i32::try_from(inserted).ok())
                    .filter(|(boundary, inserted)| {
                        *boundary >= 0 && *boundary < max_rows && *inserted > 0
                    });
                if let Some((boundary, inserted)) = projection {
                    self.text_selection
                        .adjust_for_row_splice(boundary, inserted, max_rows, floor);
                } else {
                    // A malformed/saturated grid update must never leave stale
                    // selection coordinates attached to unrelated content.
                    self.text_selection.clear();
                }
            }
            // Multiple non-composable splices, or a splice mixed with another
            // scroll/edit in one parser batch, cannot be represented by one
            // piecewise boundary. Preserve the historical fail-closed behavior.
            (Some(_), _) => self.text_selection.clear(),
        }

        // SELECTION CUSTODY Phase 4 — the DAMAGE TEST, after the geometric
        // transform above and before eviction below.
        //
        // The transform moves the selection with the content. This asks the separate
        // question the old `i32::MAX` sentinel could not: did anything this batch
        // actually REPLACE the rows the selection is sitting on? Only then is the
        // highlight meaningless and only then must it go.
        //
        // Tested in ABSOLUTE rows because that is the one space both sides agree on
        // regardless of where the viewport is. `live_top_abs` is
        // `absolute_row_counter - rows`, the same base `Grid::visible_to_absolute`
        // uses when recording, so a band recorded mid-batch and an anchor tested
        // post-transform are directly comparable.
        //
        // Asked through `clears_selection` rather than by matching the variants:
        // damage can name SEVERAL disjoint bands (a TUI repainting its title row and
        // its composer box in one batch), and a caller that matched `Band` by hand
        // could only ever see their hull — clearing a selection in the gap that
        // nothing rewrote.
        let damage = self.grid.take_selection_damage();
        // PRESS CUSTODY, site 2 of 3: the damage CLASSIFICATION this batch will be
        // recorded under. This is the only place in the process that holds both the
        // drained variant and the overlap verdict, and it holds neither for longer
        // than this block. Classifying by the VARIANT first is load-bearing:
        // `clears_selection` short-circuits `All` to `true` without consulting the
        // predicate, so the bool alone cannot tell "output replaced the selected
        // rows" from "the coordinate space is gone" — see `OutputDamage`.
        let mut damage_class = super::custody::OutputDamage::None;
        if damage != aterm_grid::SelectionDamage::None {
            let live_top_abs = self
                .grid
                .absolute_row_counter()
                .saturating_sub(u64::from(self.grid.rows()));
            let selection = &self.text_selection;
            let hit = damage.clears_selection(|lo_abs, hi_abs| {
                selection.intersects_absolute_band(live_top_abs, lo_abs, hi_abs)
            });
            damage_class = if damage == aterm_grid::SelectionDamage::All {
                super::custody::OutputDamage::All
            } else if hit {
                super::custody::OutputDamage::Hit
            } else {
                super::custody::OutputDamage::Missed
            };
            if hit {
                self.text_selection.clear();
            }
        }

        // A grid can lose scrollback WHILE PARKED — `drain_lazy_bounded` and
        // retention eviction both run against it through `Terminal`'s alt-aware
        // accessors — and it does so with no `content_scroll_delta` and no damage,
        // so the `(None, 0)` arm above performed no range check at all. Re-floor the
        // restored selection against the grid it has just come back to, or an anchor
        // now below that grid's floor survives pointing at evicted rows.
        //
        // Kept ALONGSIDE the unconditional eviction re-floor below, not replaced by
        // it: `adjust_for_scroll` also range-checks the UPPER bound and clears an
        // anchor above the live bottom, which `truncate_to_floor` deliberately does
        // not — it only knows about the floor. A selection parked while alt was up
        // and restored to a grid whose row count shrank needs exactly that upper
        // check.
        if left_alt {
            self.text_selection.adjust_for_scroll(0, max_rows, floor);
        }

        // SELECTION CUSTODY Phase 4 — EVICTION, the third and last question, after
        // motion and damage.
        //
        // Retention pressure can drop the oldest history in a batch that scrolled by
        // ZERO — a memory-budget trim, or a splice that archived rows past the ring
        // cap — and neither the transform above (which only runs on a delta or a
        // splice) nor the damage test (which is about REPLACED rows, not vanished
        // ones) asks about it. Re-flooring unconditionally here makes "no anchor sits
        // below `oldest_absolute_row()`" true on exit from every batch, and it is a
        // no-op on the ordinary path because `adjust_for_scroll` was already handed
        // the same floor.
        //
        // An evicted endpoint CLAMPS rather than clearing: the copy walk has read
        // `adj_start_row.max(-history)` for years, so this only makes the anchor
        // agree with the text a copy already returns.
        let live_top_abs = self
            .grid
            .absolute_row_counter()
            .saturating_sub(u64::from(self.grid.rows()));
        self.text_selection
            .truncate_to_floor(self.grid.oldest_absolute_row(), live_top_abs);

        // Sync BiDi cache invalidation from grid damage
        // This ensures BiDi resolutions are re-computed for modified rows
        self.sync_bidi_from_damage();

        // Enforce synchronized output timeout (mode 2026).
        // Without this, consumers must independently reimplement timeout logic.
        if self.modes.synchronized_output {
            if let Some(start) = self.transient.sync_start {
                // checked_add: bare `Instant + Duration` panics on overflow.
                // An unrepresentable deadline can only mean "never expires",
                // which is exactly the frozen-screen hang this timeout exists
                // to prevent — so treat overflow as already expired (fail
                // open: disable sync) rather than panic or hang.
                let expired = start
                    .checked_add(self.sync_timeout_duration)
                    .is_none_or(|deadline| self.transient.process_now >= deadline);
                if expired {
                    self.modes.synchronized_output = false;
                    self.transient.sync_start = None;
                    // Timeout force-clear closes the window like an ESU would.
                    self.transient.sync_end_seq += 1;
                }
            }
        }
        damage_class
    }

    /// Check if tmux control mode is active (always `false`: the tmux control
    /// mode integration is permanently compiled out).
    #[must_use]
    pub fn is_tmux_mode_active(&self) -> bool {
        false
    }

    /// Check if SSH conductor mode is active (always `false`: the SSH
    /// conductor integration is permanently compiled out).
    #[must_use]
    pub fn is_ssh_conductor_mode_active(&self) -> bool {
        false
    }

    /// Check and enforce modal protocol timeouts without processing data.
    ///
    /// Returns `true` if a modal mode was force-deactivated — always `false`:
    /// the modal protocol integrations (tmux -CC, SSH conductor) are
    /// permanently compiled out.
    pub fn check_modal_timeouts(&mut self) -> bool {
        false
    }

    /// Backdate the synchronized-output (mode 2026) start timestamp for
    /// testing timeout behavior.
    ///
    /// Saturates at the `Instant` floor: if the platform clock cannot be
    /// backdated that far, the timestamp is left unchanged.
    #[cfg(test)]
    pub fn backdate_sync_start(&mut self, duration: std::time::Duration) {
        if let Some(start) = self.transient.sync_start {
            self.transient.sync_start = start.checked_sub(duration).or(Some(start));
        }
    }

    /// Forward-date the synchronized-output (mode 2026) start timestamp for
    /// testing timer-jump behavior (simulates suspend/resume or
    /// checkpoint-restore skew where `sync_start` lands ahead of
    /// `Instant::now()`).
    #[cfg(test)]
    pub fn forward_date_sync_start(&mut self, duration: std::time::Duration) {
        if let Some(start) = self.transient.sync_start {
            self.transient.sync_start = start.checked_add(duration).or(Some(start));
        }
    }
}

/// Exhaustive destructure of all `Terminal` fields — compile-time guard (#3560).
///
/// If a new field is added to `Terminal`, this function will fail to compile,
/// forcing the developer to decide whether it should be forwarded to
/// `TerminalHandler` (add to `define_terminal_handler!` in handler.rs) or is
/// session-only (add here with `_` binding and a comment).
///
/// Session-only fields (not forwarded to handler during VT processing):
/// - `parser` — drives the handler, not passed into it
/// - `font` — rendering config, not VT protocol state
/// - `text_selection` / `parked_text_selection` — UI-layer state adjusted
///   post-process
/// - `vi` — vi-mode navigation (not VT protocol)
fn _terminal_field_exhaustiveness_check(t: &mut Terminal) {
    let Terminal {
        // --- Forwarded via define_terminal_handler! (handler.rs) ---
        grid: _,
        modes: _,
        style: _,
        charset: _,
        alt_grid: _,
        cursor_save: _,
        title: _,
        bell_callback: _,
        kitty_file_resolver: _,
        last_bell_time: _,
        bell_total: _,
        cursor_style_callback: _,
        default_cursor_style: _,
        buffer_activation_callback: _,
        notifications: _,
        clipboard: _,
        iterm2: _,
        transient: _,
        current_working_directory: _,
        color: _,
        dcs: _,
        shell: _,
        marks_state: _,
        semantic: _,
        taskbar_progress: _,
        kitty_keyboard: _,
        xterm_keyboard: _,
        #[cfg(feature = "sixel")]
            sixel: _,
        window_callback: _,
        text_sizing_callback: _,
        bidi_state: _,
        secure_keyboard_entry: _,
        // Repaint-blink epoch: bumped by the DEC dispatcher on a DECTCEM hide
        // processed inside DEC-2026 sync (forwarded so the handler can bump).
        repaint_blink_epoch: _,
        absolute_row_revision: _,
        // --- Session-only (not forwarded to handler) ---
        content_scroll_state: _,
        parser: _,
        font: _,
        text_selection: _,
        // The OTHER screen's selection, parked across an alt switch. Session-only
        // for the same reason `text_selection` is, and additionally kept out of the
        // handler by design: the park/restore is a post_process decision (see
        // there), never a VT-dispatch one.
        parked_text_selection: _,
        // PRESS CUSTODY: the last recorded custody transition. Session-only and
        // kept out of the handler on purpose — the transition is decided by the
        // press/mouse seams and by `process_at`'s three-site output protocol, never
        // by a VT dispatch, so a handler that could write it could forge one.
        last_custody: _,
        last_custody_change: _,
        last_selection_taker: _,
        vi: _,
        sync_timeout_duration: _,
        clipboard_auth: _,
        shell_integration_auth: _,
        hyperlink_auth: _,
        dcs_auth: _,
        policy: _,
        damage_epoch: _,
        damage_epoch_counted: _,
        // DMG-1 damage carrier: extraction-continuity tokens (engine identity
        // nonce + take-generation). Session-only — they describe who last
        // extracted a render snapshot and whether the damage session has been
        // consumed since, never any VT protocol state, so the handler must not
        // see them.
        extract_identity: _,
        extract_gen: _,
        // Cached search index + rebuild counter: session-only, not VT state.
        search_index: _,
        search_index_rebuilds: _,
        search_index_refreshes: _,
        // In-flight budgeted search: session-only, not VT state.
        budgeted_search: _,
        // Observation Kernel: ephemeral observation-only state, not VT state and
        // not forwarded to the handler (it is read after post_process).
        watchers: _,
        // Reused row-text scratch for the kernel's row scan — ephemeral,
        // observation-only, never VT state.
        row_text_scratch: _,
    } = t;
}

#[cfg(test)]
mod tests {
    use super::{Terminal, splice_accounts_for_batch_row_advance};
    use crate::terminal::TerminalBuilder;
    use std::time::Duration;

    /// The RIS clear in the `pending_parser_reset` block, ISOLATED.
    ///
    /// `scroll_pin_across_alt_screen::ris_then_reenter_alt_in_one_batch_leaves_no_surviving_highlight`
    /// pins the user-visible property and cannot isolate this line, and no black-box
    /// test can: RIS's own `erase_scrollback` records `SelectionDamage::All` on the
    /// main grid, that band is not drained while the alt grid is active, and the exit
    /// batch drains it in `post_process` immediately AFTER the restore — so a parked
    /// selection that survived the reset is destroyed on arrival anyway. Deleting
    /// this clear leaves the property test green for that reason alone.
    ///
    /// What the line is actually for is the FIELD invariant documented on
    /// `parked_text_selection` — "empty whenever `alternate_screen` is false", and
    /// more sharply, never outliving the reset that erased the grid it names. That is
    /// observable from inside the crate, one batch earlier than any restore, and it
    /// is where this belongs: a future grid whose restore path does not happen to
    /// carry an `All` moves the whole load onto this line.
    #[test]
    fn ris_empties_the_parked_selection_slot_in_the_batch_that_resets() {
        use crate::selection::{SelectionSide, SelectionType};

        let mut term = Terminal::new(6, 24);
        for i in 0..40 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        term.process(b"\x1b[?1049h");
        assert!(
            term.parked_text_selection.has_selection(),
            "precondition: entering alt parked the main selection in the slot"
        );

        // RIS and re-enter alt in ONE batch. `was_alt` and `modes.alternate_screen`
        // are both true at `post_process`, so neither the park nor the restore runs:
        // nothing but the reset itself can retire the slot.
        term.process(b"\x1bc\x1b[?1049h");

        assert!(
            !term.parked_text_selection.has_selection(),
            "the reset must retire the parked selection in its own batch; leaving it \
             for a later restore hands a pre-RIS highlight back over an erased grid"
        );
    }

    /// Negative control for the cross-field contract behind piecewise selection
    /// recovery: the retained splice is sufficient only when it explains the
    /// ENTIRE counter advance. One unreported ordinary scroll must reject the
    /// projection (and therefore trip the production debug assertion).
    #[test]
    fn selection_splice_must_account_for_the_entire_batch_row_advance() {
        let before = 40;
        let inserted = 3;
        assert!(splice_accounts_for_batch_row_advance(
            before,
            before + inserted,
            inserted,
        ));

        let unreported_other_scroll = 1;
        assert!(
            !splice_accounts_for_batch_row_advance(
                before,
                before + inserted + unreported_other_scroll,
                inserted,
            ),
            "negative control: an absolute-row bump without content_scroll_delta must not look like an isolated splice",
        );
        assert!(
            !splice_accounts_for_batch_row_advance(before, before - 1, inserted),
            "the monotonic counter moving backward must also reject recovery",
        );
    }

    #[test]
    fn content_scroll_state_tracks_uniform_scroll_with_zero_history_capacity() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(0)
            .build();
        let before = term.content_scroll_state();

        term.process(b"\x1b[3;1H\n");

        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "zero-capacity history cannot expose a count delta"
        );
        assert_eq!(
            term.content_scroll_state().uniform_up_rows,
            before.uniform_up_rows + 1,
            "the content-motion signal is independent of retained history"
        );
        assert_eq!(
            term.content_scroll_state().invalidation_epoch,
            before.invalidation_epoch
        );
    }

    #[test]
    fn content_scroll_state_tracks_uniform_scroll_at_saturated_history_capacity() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(1)
            .build();
        term.process(b"\x1b[3;1H\n");
        assert_eq!(term.grid().scrollback_lines(), 1, "history is at its cap");
        let before = term.content_scroll_state();

        term.process(b"\x1b[3;1H\n");

        assert_eq!(
            term.grid().scrollback_lines(),
            1,
            "eviction keeps the observable history count saturated"
        );
        assert_eq!(
            term.content_scroll_state().uniform_up_rows,
            before.uniform_up_rows + 1
        );
        assert_eq!(
            term.content_scroll_state().invalidation_epoch,
            before.invalidation_epoch
        );
    }

    #[test]
    fn content_scroll_state_invalidates_interior_and_top_anchored_regions() {
        let mut term = TerminalBuilder::new()
            .size(4, 12)
            .ring_buffer_size(8)
            .build();

        let before_interior = term.content_scroll_state();
        // Interior DECSTBM region: rows 2..=3 move, rows 1 and 4 stay fixed.
        term.process(b"\x1b[2;3r\x1b[3;1H\n");
        let after_interior = term.content_scroll_state();
        assert_eq!(
            after_interior.uniform_up_rows, before_interior.uniform_up_rows,
            "a partial-region shift is not a whole-screen translation"
        );
        assert_eq!(
            after_interior.invalidation_epoch,
            before_interior.invalidation_epoch + 1
        );

        let before_top_anchored = after_interior;
        // Top-anchored archival region: the upper band enters history while
        // the bottom row remains a protected footer. This is a row splice, not
        // a uniform viewport translation even though scrollback grows.
        term.process(b"\x1b[r\x1b[1;1HX\x1b[1;3r\x1b[3;1H\n");
        let after_top_anchored = term.content_scroll_state();
        assert_eq!(
            after_top_anchored.uniform_up_rows,
            before_top_anchored.uniform_up_rows
        );
        assert_eq!(
            after_top_anchored.invalidation_epoch,
            before_top_anchored.invalidation_epoch + 1
        );
        assert!(
            term.grid().scrollback_lines() > 0,
            "the top-anchored case closes the scrollback-growth false positive"
        );
    }

    #[test]
    fn content_scroll_state_invalidates_alt_screen_scrolls() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(8)
            .build();
        term.process(b"\x1b[?1049h");
        assert!(term.is_alternate_screen());
        let before = term.content_scroll_state();

        term.process(b"\x1b[3;1H\n");

        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "alt screen has no history"
        );
        assert_eq!(
            term.content_scroll_state().uniform_up_rows,
            before.uniform_up_rows,
            "an app-owned alt buffer is never translated by the host"
        );
        assert_eq!(
            term.content_scroll_state().invalidation_epoch,
            before.invalidation_epoch + 1
        );
    }

    /// SELECTION CUSTODY — the epoch survives the split of
    /// `force_selection_invalidation`.
    ///
    /// The four alt-switch call sites now use `invalidate_host_coordinates`, which
    /// drops the `SelectionDamage::All` half (the selection is parked, not
    /// destroyed) and KEEPS the `coordinates_invalidated` half. That half is the
    /// public `ContentScrollState::invalidation_epoch` contract: consumers with
    /// cached grid coordinates TRANSLATE rather than rebuild while the epoch is
    /// unchanged, so losing the bump would leave cursor-effect state attached to
    /// cells from the other buffer. One batch per spelling, so each bump is
    /// attributable.
    ///
    /// Delete the four calls instead of replacing them and five of these six fail.
    /// Only 1049-ENTER survives — its `new_grid.erase_screen()` bumps the epoch by
    /// itself — which makes it this test's built-in vacuity control.
    #[test]
    fn every_alt_screen_switch_advances_the_host_coordinate_epoch_exactly_once() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(8)
            .build();
        for spelling in [
            &b"\x1b[?1049h"[..],
            &b"\x1b[?1049l"[..],
            &b"\x1b[?47h"[..],
            &b"\x1b[?47l"[..],
            &b"\x1b[?1047h"[..],
            &b"\x1b[?1047l"[..],
        ] {
            let before = term.content_scroll_state();
            term.process(spelling);
            assert_eq!(
                term.content_scroll_state().invalidation_epoch,
                before.invalidation_epoch + 1,
                "{spelling:?} replaces every visible coordinate exactly once"
            );
            assert_eq!(
                term.content_scroll_state().uniform_up_rows,
                before.uniform_up_rows,
                "{spelling:?}: a buffer swap is not a translation the host can apply"
            );
        }
    }

    #[test]
    fn content_scroll_state_survives_direct_and_ris_reset_without_double_counting() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(0)
            .build();
        term.process(b"\x1b[3;1H\n");
        let before_direct = term.content_scroll_state();

        term.reset();

        let after_direct = term.content_scroll_state();
        assert_eq!(after_direct.uniform_up_rows, before_direct.uniform_up_rows);
        assert_eq!(
            after_direct.invalidation_epoch,
            before_direct.invalidation_epoch + 1
        );
        term.process(b"");
        assert_eq!(
            term.content_scroll_state(),
            after_direct,
            "direct reset drains its grid sentinel instead of reporting twice"
        );

        term.process(b"\x1bc");
        let after_ris = term.content_scroll_state();
        assert_eq!(after_ris.uniform_up_rows, after_direct.uniform_up_rows);
        assert_eq!(
            after_ris.invalidation_epoch,
            after_direct.invalidation_epoch + 1,
            "byte-stream RIS reports one coordinate invalidation"
        );
    }

    #[test]
    fn content_scroll_state_coalesces_uniform_rows_and_invalidates_mixed_batches() {
        let mut term = TerminalBuilder::new()
            .size(3, 12)
            .ring_buffer_size(0)
            .build();
        let initial = term.content_scroll_state();

        // Once parked on the bottom row, every LF is another full-screen scroll;
        // the grid coalesces their finite deltas within this parser batch.
        term.process(b"\x1b[3;1H\n\n\n");
        let uniform = term.content_scroll_state();
        assert_eq!(uniform.uniform_up_rows, initial.uniform_up_rows + 3);
        assert_eq!(uniform.invalidation_epoch, initial.invalidation_epoch);

        // A later partial-region shift in the SAME batch turns the grid verdict
        // into the fail-closed sentinel. Do not publish the earlier scroll as a
        // composable translation: one invalidation covers the whole batch.
        term.process(b"\x1b[3;1H\n\x1b[2;3r\x1b[3;1H\n");
        let mixed = term.content_scroll_state();
        assert_eq!(mixed.uniform_up_rows, uniform.uniform_up_rows);
        assert_eq!(mixed.invalidation_epoch, uniform.invalidation_epoch + 1);
    }

    /// SCR-1: while the user is scrolled back into history, live output (e.g.
    /// `tail -f`) must NOT yank the viewport to the bottom. The display_offset is
    /// re-pinned by the number of lines entering scrollback so the SAME content
    /// stays in view.
    #[test]
    fn scrolled_back_viewport_pins_during_live_output() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(200)
            .build();

        // Produce 20 numbered lines so there is plenty of scrollback.
        for i in 0..20 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        // Scroll back so a known line is at the top of the viewport.
        term.scroll_display(8); // display_offset = 8
        let offset_before = term.grid().display_offset();
        assert!(offset_before > 0, "precondition: user is scrolled back");
        let top_before = term.display_row_text(0).unwrap_or_default();
        assert!(
            top_before.contains("line-"),
            "precondition: a scrollback line is visible at the top, got {top_before:?}",
        );

        // Feed MORE live output (simulating tail -f). The viewport must stay put.
        for i in 20..30 {
            term.process(format!("live-{i}\r\n").as_bytes());
        }

        // The same content remains at the top of the viewport...
        let top_after = term.display_row_text(0).unwrap_or_default();
        assert_eq!(
            top_after, top_before,
            "SCR-1: the viewport must stay pinned on the same content during live output",
        );
        // ...and display_offset advanced by the 10 lines that entered scrollback.
        let offset_after = term.grid().display_offset();
        assert_eq!(
            offset_after,
            offset_before + 10,
            "display_offset must advance by the number of lines entering scrollback",
        );
        // None of the freshly-printed live lines are visible (still scrolled back).
        for r in 0..4 {
            let row = term.display_row_text(r).unwrap_or_default();
            assert!(
                !row.contains("live-2") && !row.contains("live-29"),
                "live output must not appear in the pinned viewport, row{r}={row:?}",
            );
        }
    }

    /// SCR-1: when the user is at the live bottom (display_offset == 0), output
    /// still flows normally and the viewport tracks the bottom (no pin).
    #[test]
    fn live_viewport_tracks_bottom_when_not_scrolled_back() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(200)
            .build();
        for i in 0..10 {
            term.process(format!("row-{i}\r\n").as_bytes());
        }
        assert_eq!(term.grid().display_offset(), 0, "starts at live bottom");
        term.process(b"newest\r\n");
        assert_eq!(
            term.grid().display_offset(),
            0,
            "viewport stays at the live bottom when not scrolled back",
        );
        // The newest line is on screen.
        let visible = term.visible_content();
        assert!(
            visible.contains("newest"),
            "live output is visible: {visible:?}"
        );
    }

    /// Mode 2026 must time out (fail open) when the timer jumps past the
    /// deadline — the backdated start simulates a forward clock jump after
    /// `CSI ?2026h` (e.g. suspend/resume), the exact scenario the timeout
    /// exists for: an application enables sync and never disables it.
    #[test]
    fn sync_output_disables_after_backdated_start() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2026h");
        assert!(
            term.modes().synchronized_output(),
            "DECSET 2026 must enable"
        );
        assert!(term.transient.sync_start.is_some());

        // Jump the timer well past the default 1000ms timeout.
        term.backdate_sync_start(Duration::from_secs(120));
        term.process(b"x"); // any processed byte runs post_process()

        assert!(
            !term.modes().synchronized_output(),
            "sync output must disable once the deadline passed (no hang)"
        );
        assert!(
            term.transient.sync_start.is_none(),
            "sync_start must be cleared with the mode"
        );
    }

    /// A forward-dated start (timer skew in the other direction, e.g.
    /// checkpoint restore) must neither panic in the deadline math nor
    /// wedge the terminal permanently: processing continues, and once the
    /// skewed deadline is behind `now`, sync output still disables.
    #[test]
    fn sync_output_survives_forward_dated_start_without_panic() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2026h");
        assert!(term.modes().synchronized_output());

        term.forward_date_sync_start(Duration::from_secs(3600));
        term.process(b"x"); // deadline far in the future — must not panic
        assert!(
            term.modes().synchronized_output(),
            "future deadline keeps sync enabled (not spuriously expired)"
        );

        // Now move the skewed start past the deadline: must still recover.
        term.backdate_sync_start(Duration::from_secs(7200));
        term.process(b"y");
        assert!(
            !term.modes().synchronized_output(),
            "sync output must disable after the skewed deadline passes"
        );
    }

    /// REPAINT-BLINK epoch, the positive law: the EXACT per-keystroke byte
    /// pattern Claude Code emits (ground truth captured live over the control
    /// socket) — `?2026h · ?25l · redraw · ?25h · ?2026l` — bumps the epoch
    /// exactly ONCE per burst, monotonically.
    #[test]
    fn repaint_blink_epoch_bumps_once_per_claude_burst() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.repaint_blink_epoch(), 0, "fresh terminal starts at 0");
        // Verbatim burst shape from the live capture (one keystroke's repaint).
        for (i, ch) in ["a", "b", "c"].iter().enumerate() {
            let burst = format!(
                "\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[{}C\x1b[21B{ch}\x1b[24;1H\x1b[22;38H\x1b[?25h\x1b[?2026l",
                36 + i
            );
            term.process(burst.as_bytes());
            assert_eq!(
                term.repaint_blink_epoch(),
                (i + 1) as u64,
                "one hide-inside-sync per burst = one bump"
            );
        }
    }

    /// REPAINT-BLINK epoch, the negative laws: a bare DECTCEM hide with NO
    /// synchronized update active (ConPTY's per-echo hide, and any plain
    /// hide/show pair) never bumps — and neither does vim-style output, which
    /// parks the cursor hidden while it draws WITHOUT ever entering sync.
    #[test]
    fn bare_hide_and_vim_style_output_never_bump_the_blink_epoch() {
        let mut term = Terminal::new(24, 80);
        // ConPTY-style per-echo choreography: hide → move/write → show, no sync.
        term.process(b"\x1b[?25l\x1b[5;10Hx\x1b[?25h");
        assert_eq!(term.repaint_blink_epoch(), 0, "bare hide: no sync, no bump");
        // vim-style: enter the alt screen, hide, draw a whole screen, show.
        term.process(b"\x1b[?1049h\x1b[?25l");
        for r in 1..=24 {
            term.process(format!("\x1b[{r};1H\x1b[2K~ vim line").as_bytes());
        }
        term.process(b"\x1b[10;3H\x1b[?25h\x1b[?1049l");
        assert_eq!(
            term.repaint_blink_epoch(),
            0,
            "vim-style hide-while-drawing (no DEC-2026) never bumps"
        );
        // Sync WITHOUT a hide inside it doesn't bump either (the pairing is
        // the discriminator, not either half alone).
        term.process(b"\x1b[?2026htear-free write\x1b[?2026l");
        assert_eq!(term.repaint_blink_epoch(), 0, "sync alone never bumps");
        // And a show inside sync is not a hide.
        term.process(b"\x1b[?2026h\x1b[?25h\x1b[?2026l");
        assert_eq!(
            term.repaint_blink_epoch(),
            0,
            "show-inside-sync never bumps"
        );
        // Positive control on the same terminal: the pairing DOES bump.
        term.process(b"\x1b[?2026h\x1b[?25lredraw\x1b[?25h\x1b[?2026l");
        assert_eq!(term.repaint_blink_epoch(), 1);
    }

    /// Clock-seam determinism (docs/design/HIERARCHICAL_SESSIONS.md Addendum B,
    /// GREEN-ORDER step 2): `process_at` must be a pure function of
    /// `(bytes, ClockReading)`. Every state-affecting time read — the bell
    /// throttle, the mode-2026 timeout, and the OSC 133 command-mark
    /// timestamps — must observe the injected reading, never the host clock.
    ///
    /// We replay one fixed schedule on two fresh terminals and assert the
    /// observable state is bit-identical, AND that it equals the values the
    /// *injected* clock dictates rather than wall-clock time. Each pinned value
    /// is exactly what would change if its site regressed to `Instant::now()`
    /// or `current_time_ms()`, so this is the regression tripwire for the seam.
    #[test]
    fn process_at_is_deterministic_under_injected_clock() {
        use super::ClockReading;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Must match ClockReading.monotonic's type (web_time::Instant = std on native).
        use web_time::Instant;

        // (bytes, monotonic offset ms from base, wall-clock epoch ms).
        // Exercises every clock-dependent site:
        //  - two bells 40ms apart (2nd throttled), then one 230ms after #1 (fires);
        //  - DECSET 2026 then a byte ~2s later (past the 1s timeout -> disables);
        //  - a full OSC 133 A/B/C/D command, timestamped from the injected wall ms.
        const SCHEDULE: &[(&[u8], u64, u64)] = &[
            (b"hello", 0, 1_000),
            (b"\x07", 10, 1_010),         // bell #1 -> fires
            (b"\x07", 50, 1_050),         // +40ms -> throttled (<100ms)
            (b"\x07", 240, 1_240),        // +230ms after #1 -> fires
            (b"\x1b[?2026h", 250, 1_250), // enable synchronized output
            (b"\x1b]133;A\x07", 260, 1_260),
            (b"\x1b]133;B\x07", 260, 1_260),
            (b"\x1b]133;C\x07", 260, 1_260),
            (b"\x1b]133;D;0\x07", 260, 1_260), // finished -> mark recorded
            (b"z", 2_300, 3_300),              // +2050ms after enable -> sync times out
        ];

        type Marks = Vec<(Option<u64>, Option<u64>, Option<u64>)>;
        // Replay the schedule on a fresh terminal and return the
        // base-independent observable snapshot.
        let run = |base: Instant| -> (String, bool, usize, Marks) {
            let mut term = Terminal::new(8, 40);
            let bells = Arc::new(AtomicUsize::new(0));
            let bells_cb = Arc::clone(&bells);
            term.bell_callback = Some(Box::new(move || {
                bells_cb.fetch_add(1, Ordering::Relaxed);
            }));
            for (bytes, off_ms, wall_ms) in SCHEDULE {
                term.process_at(
                    bytes,
                    ClockReading {
                        monotonic: base + Duration::from_millis(*off_ms),
                        wall_ms: Some(*wall_ms),
                    },
                );
            }
            let marks = term
                .command_marks()
                .iter()
                .map(|m| {
                    (
                        m.command_input_start_time_ms,
                        m.command_exec_start_time_ms,
                        m.command_end_time_ms,
                    )
                })
                .collect();
            (
                term.visible_content(),
                term.modes().synchronized_output(),
                bells.load(Ordering::Relaxed),
                marks,
            )
        };

        // Same base for both runs: the asserted observables are base-independent,
        // but sharing it makes the equality total.
        let base = Instant::now(); // CLOCK-EXEMPT: test harness base, fed back as injected input
        let a = run(base);
        let b = run(base);
        assert_eq!(
            a, b,
            "process_at must be deterministic for a fixed (bytes, clock) schedule"
        );

        // Pin each observable to what the INJECTED clock dictates.
        let (content, sync_on, bell_count, marks) = a;
        assert!(content.contains("helloz"), "grid content: {content:?}");
        assert_eq!(
            bell_count, 2,
            "bell throttle must use injected instants (2 fire, 1 throttled)"
        );
        assert!(
            !sync_on,
            "mode-2026 must time out against the injected instant (2.05s > 1s)"
        );
        assert_eq!(marks.len(), 1, "one completed OSC 133 command mark");
        assert_eq!(
            marks[0],
            (Some(1_260), Some(1_260), Some(1_260)),
            "command-mark timestamps must equal the injected wall_ms, not host time"
        );
    }
}
