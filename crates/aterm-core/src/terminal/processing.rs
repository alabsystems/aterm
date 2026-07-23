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
        if pinned_offset > 0 {
            self.grid.scroll_to_bottom();
        }

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
                self.transient.pending_parser_reset = false;
            }
            let parse_end = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), not grid state (web_time = std on native, JS clock on wasm)

            let grid_start = web_time::Instant::now(); // CLOCK-EXEMPT: profiling diagnostic (gated), not grid state (web_time = std on native, JS clock on wasm)
            self.post_process(lines_before);
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
                self.transient.pending_parser_reset = false;
            }
            self.post_process(lines_before);
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
        // restored unchanged. The alt screen has no scrollback, so on alt the
        // counter does not rise and the offset stays 0 — correct.
        if pinned_offset > 0 {
            let lines_added = self
                .grid
                .absolute_row_counter()
                .saturating_sub(lines_before);
            self.grid.repin_display_offset(pinned_offset, lines_added);
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
    fn post_process(&mut self, absolute_rows_before: u64) {
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

        // Adjust selection coordinates after content scroll (#4056). A
        // top-anchored archival region is the one non-uniform case we can map
        // exactly: rows before the protected footer move toward history while
        // footer rows stay fixed on screen. The grid retains this update
        // independently because parser-order metadata handling may already
        // have drained `take_absolute_row_update()` above.
        let selection_row_update = self.grid.take_selection_row_update();
        let scroll_delta = self.grid.take_content_scroll_delta();
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
                debug_assert!(
                    splice_accounts_for_batch_row_advance(
                        absolute_rows_before,
                        absolute_rows_after,
                        inserted,
                    ),
                    "zero-delta selection splice must account for the batch's entire absolute-row advance: before={absolute_rows_before}, after={absolute_rows_after}, inserted={inserted}; every other absolute_row_counter bump must set content_scroll_delta",
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
/// - `text_selection` — UI-layer state adjusted post-process
/// - `vi` — vi-mode navigation (not VT protocol)
fn _terminal_field_exhaustiveness_check(t: &mut Terminal) {
    let Terminal {
        // --- Forwarded via define_terminal_handler! (handler.rs) ---
        grid: _,
        modes: _,
        style: _,
        current_style_id: _,
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
        parser: _,
        font: _,
        text_selection: _,
        vi: _,
        sync_timeout_duration: _,
        clipboard_auth: _,
        shell_integration_auth: _,
        hyperlink_auth: _,
        dcs_auth: _,
        policy_engine: _,
        damage_epoch: _,
        damage_epoch_counted: _,
        // Cached search index + rebuild counter: session-only, not VT state.
        search_index: _,
        search_index_rebuilds: _,
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
