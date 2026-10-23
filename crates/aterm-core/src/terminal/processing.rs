// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal input processing: the `process()` entry point and its batch prologue/epilogue.

use super::Terminal;

use aterm_types::duration_to_nanos;

/// One reading of the clocks the terminal pipeline depends on.
#[derive(Debug, Clone, Copy)]
pub struct ClockReading {
    /// Monotonic instant.
    pub monotonic: aterm_time::Instant,
    /// Wall-clock epoch milliseconds recorded into OSC 133/633 command and output marks.
    pub wall_ms: Option<u64>,
}

impl ClockReading {
    /// Capture the live clocks.
    #[must_use]
    #[inline]
    #[expect(
        clippy::disallowed_methods,
        reason = "the sole pipeline clock capture, injected into process_at"
    )]
    pub fn now() -> Self {
        Self {
            monotonic: aterm_time::Instant::now(),
            wall_ms: crate::terminal::shell::current_time_ms(),
        }
    }
}

/// Whether one retained splice accounts for the whole monotonic-row advance of
#[inline]
const fn splice_accounts_for_batch_row_advance(before: u64, after: u64, inserted: u64) -> bool {
    match after.checked_sub(before) {
        Some(advance) => advance == inserted,
        None => false,
    }
}

/// Outcome of [`Terminal::advance_kitty_animations`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyAnimationStep {
    /// A frame changed on screen.
    pub advanced: bool,
    /// When the next frame is due, `None` when nothing plays on screen.
    pub next_frame_at: Option<aterm_time::Instant>,
}

impl Terminal {
    /// Process input bytes through the parser, reading the live clocks for time-dependent state.
    #[inline]
    pub fn process(&mut self, input: &[u8]) {
        self.process_at(input, ClockReading::now());
    }

    /// Play the Kitty graphics animations up to `now`.
    pub fn advance_kitty_animations(&mut self, now: aterm_time::Instant) -> KittyAnimationStep {
        if self.transient.kitty_frames.is_empty() {
            return KittyAnimationStep::default();
        }
        let (_, mut handler) = self.split_for_process();
        let (advanced, next_frame_at) = handler.advance_kitty_animations(now);
        if advanced {
            // A frame step changes what the screen shows with no output fed
            self.transient.screen_mutations = self.transient.screen_mutations.wrapping_add(1);
        }
        KittyAnimationStep {
            advanced,
            next_frame_at,
        }
    }

    /// Whether a Kitty graphics animation is running, shown on screen.
    #[must_use]
    #[inline]
    pub fn kitty_animation_playing(&self) -> bool {
        self.transient
            .kitty_frames
            .values()
            .any(super::transient_state::KittyAnimation::plays)
    }

    /// Process input bytes through the parser at a caller-supplied clock reading.
    #[allow(
        clippy::too_many_lines,
        reason = "main entry point with sequential processing stages"
    )]
    pub fn process_at(&mut self, input: &[u8], clock: ClockReading) {
        #[cfg(debug_assertions)]
        self.debug_assert_mode_mirror_in_sync();
        if !input.is_empty() {
            self.transient.screen_mutations = self.transient.screen_mutations.wrapping_add(1);
            self.grid.drop_resize_undo();
            if let Some(parked) = self.alt_grid.as_mut() {
                parked.drop_resize_undo();
            }
        }
        self.transient.process_now = clock.monotonic;
        self.transient.process_wall_ms = clock.wall_ms;
        let pinned_offset = self.grid.display_offset();
        let lines_before = self.grid.absolute_row_counter();
        let was_alt = self.modes.alternate_screen;
        if pinned_offset > 0 {
            // Not `scroll_to_bottom`: the motion is identical, but this is the
            self.grid.pin_viewport_to_live_for_output_batch();
        }
        let damage_class: super::custody::OutputDamage;
        let selection_before = self.text_selection.has_selection();

        if self.transient.pipeline_timestamps.profiling_enabled {
            #[expect(
                clippy::disallowed_methods,
                reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
            )]
            let entry = aterm_time::Instant::now();
            let parse_start = entry;
            self.advance_parser_archiving(input);
            if self.transient.pending_parser_reset {
                self.finish_ris_batch();
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
            )]
            let parse_end = aterm_time::Instant::now();

            #[expect(
                clippy::disallowed_methods,
                reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
            )]
            let grid_start = aterm_time::Instant::now();
            damage_class = self.post_process(lines_before, was_alt);
            self.alt_archive_epilogue();
            self.note_batch_end();
            self.observe_at(self.transient.process_now);
            #[expect(
                clippy::disallowed_methods,
                reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
            )]
            let grid_end = aterm_time::Instant::now();

            #[expect(
                clippy::disallowed_methods,
                reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
            )]
            let total = entry.elapsed();
            self.record_pipeline_timestamps(
                parse_end - parse_start,
                grid_end - grid_start,
                total,
                input.len(),
            );
        } else {
            self.advance_parser_archiving(input);
            if self.transient.pending_parser_reset {
                self.finish_ris_batch();
            }
            damage_class = self.post_process(lines_before, was_alt);
            self.alt_archive_epilogue();
            self.note_batch_end();
            self.observe_at(self.transient.process_now);

            let ts = &mut self.transient.pipeline_timestamps;
            ts.last_process_bytes = u32::try_from(input.len()).unwrap_or(u32::MAX);
            ts.process_sequence = ts.process_sequence.wrapping_add(1);
        }

        self.reconcile_color_scheme_after_output();

        let parked_main_counter = self.transient.alt_park_main_row_counter.take();
        if pinned_offset > 0 {
            let entered_alt = !was_alt && self.modes.alternate_screen;
            if entered_alt {
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

        if let Some((restored_offset, counter_at_swap)) = self.transient.alt_restore_pin.take() {
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

        if was_alt == self.modes.alternate_screen {
            let lines_added = self
                .grid
                .absolute_row_counter()
                .saturating_sub(lines_before);
            self.note_output_custody(pinned_offset, damage_class, lines_added, selection_before);
        }

        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();

        self.refresh_mode_mirror();
    }

    /// Store per-stage pipeline durations in transient state.
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

    /// RIS side effects the handler cannot reach, run after the parser and
    #[inline]
    fn finish_ris_batch(&mut self) {
        self.secure_keyboard_entry = false;
        self.text_selection.clear();
        self.parked_text_selection.clear();
        self.transient.pending_parser_reset = false;
    }

    /// Post-processing after parser advances.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "UniformScroll",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "RegionDamageLow",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "RegionDamageHigh",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SelectionCustody",
            action = "WholesaleInvalidate",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[allow(clippy::too_many_lines)] // one ordered pass over the batch's drained grid records
    fn post_process(
        &mut self,
        absolute_rows_before: u64,
        was_alt: bool,
    ) -> super::custody::OutputDamage {
        // Top-anchored partial scrollback inserts logical rows immediately
        if let Some(update) = self.grid.take_absolute_row_update() {
            super::handler::apply_absolute_row_update(
                update,
                &mut self.shell,
                &mut self.marks_state,
                &mut self.transient,
                &mut self.absolute_row_revision,
            );
        }

        let left_alt = was_alt && !self.modes.alternate_screen;
        let exited_mid_batch = std::mem::take(&mut self.transient.alt_screen_left_in_batch);
        if left_alt {
            self.text_selection = std::mem::take(&mut self.parked_text_selection);
        } else if !was_alt && self.modes.alternate_screen {
            self.parked_text_selection = std::mem::take(&mut self.text_selection);
        } else if exited_mid_batch && self.modes.alternate_screen {
            self.text_selection.clear();
        }

        let selection_row_update = self.grid.take_selection_row_update();
        let scroll_delta = self.grid.take_content_scroll_delta();
        let bands = self.grid.take_row_band_moves();
        let coordinates_invalidated = self.grid.take_coordinates_invalidated();
        let bands_explain = bands.len > 0
            && !bands.inexact
            && scroll_delta == 0
            && was_alt == self.modes.alternate_screen;
        let invalidates_coordinates = selection_row_update.is_some()
            || coordinates_invalidated
            || scroll_delta == i32::MAX
            || scroll_delta < 0;
        if invalidates_coordinates {
            self.content_scroll_state.invalidate();
            let explained = bands_explain
                && !matches!(
                    selection_row_update,
                    Some(aterm_grid::AbsoluteRowUpdate::Invalidate)
                );
            if explained {
                self.content_scroll_state
                    .record_band_batch(bands.as_slice());
            }
        } else {
            debug_assert!(
                bands.len == 0 && !bands.inexact,
                "a band move must set the coordinate flag or a splice: every grid producer records the band beside the flag, never instead of it"
            );
            if scroll_delta > 0 {
                self.content_scroll_state
                    .record_uniform_up(u64::try_from(scroll_delta).unwrap_or(u64::MAX));
            }
        }
        let max_rows = i32::from(self.grid.rows());
        let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        match (selection_row_update, scroll_delta) {
            (None, 0) => {}
            (None, delta) => {
                self.text_selection
                    .adjust_for_scroll(delta, max_rows, floor);
            }
            (Some(aterm_grid::AbsoluteRowUpdate::Splice { at, inserted }), 0) => {
                let absolute_rows_after = self.grid.absolute_row_counter();
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
                    self.text_selection.clear();
                }
            }
            (Some(_), _) => self.text_selection.clear(),
        }

        let damage = self.grid.take_selection_damage();
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

        if left_alt {
            self.text_selection.adjust_for_scroll(0, max_rows, floor);
        }

        let live_top_abs = self
            .grid
            .absolute_row_counter()
            .saturating_sub(u64::from(self.grid.rows()));
        self.text_selection
            .truncate_to_floor(self.grid.oldest_absolute_row(), live_top_abs);

        self.sync_bidi_from_damage();

        if self.modes.synchronized_output {
            if let Some(start) = self.transient.sync_start {
                let expired = start
                    .checked_add(self.sync_timeout_duration)
                    .is_none_or(|deadline| self.transient.process_now >= deadline);
                if expired {
                    self.modes.synchronized_output = false;
                    self.transient.sync_start = None;
                    self.transient.sync_open_dirty = false;
                    self.transient.sync_end_seq += 1;
                }
            }
        }
        damage_class
    }

    /// Backdate the synchronized-output (mode 2026) start timestamp for testing timeout behavior.
    #[cfg(test)]
    pub fn backdate_sync_start(&mut self, duration: std::time::Duration) {
        if let Some(start) = self.transient.sync_start {
            self.transient.sync_start = start.checked_sub(duration).or(Some(start));
        }
    }

    /// Forward-date the synchronized-output (mode 2026) start timestamp for testing timer-jump.
    #[cfg(test)]
    pub fn forward_date_sync_start(&mut self, duration: std::time::Duration) {
        if let Some(start) = self.transient.sync_start {
            self.transient.sync_start = start.checked_add(duration).or(Some(start));
        }
    }
}

/// Exhaustive destructure of every `Terminal` field: a compile-time guard. A new field fails to
const fn _terminal_field_exhaustiveness_check(t: &mut Terminal) {
    let Terminal {
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
        notify_total: _,
        default_cursor_style: _,
        host_alternate_scroll: _,
        notifications: _,
        clipboard: _,
        iterm2: _,
        transient: _,
        current_working_directory: _,
        color: _,
        dcs: _,
        shell: _,
        marks_state: _,
        taskbar_progress: _,
        program_state: _,
        kitty_keyboard: _,
        xterm_keyboard: _,
        sixel: _,
        window_callback: _,
        bidi_state: _,
        secure_keyboard_entry: _,
        repaint_blink_epoch: _,
        evidence_asserted: _,
        absolute_row_revision: _,
        holder_fence: _,
        holder_carried_at: _,
        configured_modes: _,
        content_scroll_state: _,
        resize_journal: _,
        parser: _,
        text_selection: _,
        mouse_report_memo: _,
        parked_text_selection: _,
        resize_undo_selection: _,
        last_custody: _,
        last_custody_change: _,
        last_selection_taker: _,
        reader_gestures: _,
        sync_timeout_duration: _,
        clipboard_auth: _,
        shell_integration_auth: _,
        hyperlink_auth: _,
        policy: _,
        damage_epoch: _,
        damage_epoch_counted: _,
        extract_identity: _,
        extract_gen: _,
        search_slot: _,
        search_index_rebuilds: _,
        search_index_refreshes: _,
        budgeted_search: _,
        watchers: _,
        row_text_scratch: _,
        refill_mask_scratch: _,
        mode_mirror: _,
        frame_watchers: _,
        alt_archive: _,
    } = t;
}

#[cfg(test)]
mod tests {
    use super::{Terminal, splice_accounts_for_batch_row_advance};
    use crate::terminal::TerminalBuilder;
    use std::time::Duration;

    /// The RIS clear of the parked slot (`finish_ris_batch`), pinned one batch before any
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

        term.process(b"\x1bc\x1b[?1049h");

        assert!(
            !term.parked_text_selection.has_selection(),
            "the reset must retire the parked selection in its own batch; leaving it \
             for a later restore hands a pre-RIS highlight back over an erased grid"
        );
    }

    /// The same batch from the main screen, where the RIS clear of the live slot
    #[test]
    fn ris_then_enter_from_main_parks_nothing() {
        use crate::selection::{SelectionSide, SelectionType};

        let mut term = Terminal::new(6, 24);
        for i in 0..40 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        {
            let sel = term.text_selection_mut();
            sel.start_selection(-3, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(-3, 4, SelectionSide::Right);
            sel.complete_selection();
        }

        term.process(b"\x1bc\x1b[?1049h");

        assert!(term.is_alternate_screen());
        assert!(
            !term.parked_text_selection.has_selection(),
            "RIS erased the grid the main selection named; the re-entry in the same \
             batch must not park it"
        );
        assert!(!term.text_selection().has_selection());
    }

    /// RIS followed by bytes the PTY read cut mid-sequence: the tail survives
    fn ris_then_split_tail(profiling: bool, first: &[u8], second: &[u8]) -> Terminal {
        let mut t = Terminal::new(24, 80);
        t.transient.pipeline_timestamps.profiling_enabled = profiling;
        t.process(first);
        t.process(second);
        t
    }

    fn row_text(t: &Terminal, row: u16) -> String {
        t.grid().row(row).map(|r| r.to_string()).unwrap_or_default()
    }

    #[test]
    fn ris_does_not_drop_csi_split_across_process_calls() {
        use crate::grid::PackedColor;
        for profiling in [false, true] {
            let t = ris_then_split_tail(profiling, b"\x1bc\x1b[3", b"1mX");
            let cell = t.grid().cell(0, 0).expect("cell 0,0");
            assert_eq!(
                cell.char(),
                'X',
                "profiling={profiling}: {:?}",
                row_text(&t, 0)
            );
            assert_eq!(
                cell.fg_color(),
                Some(PackedColor::indexed(1)),
                "profiling={profiling}: the split SGR 31 must apply to the X"
            );
            assert_ne!(t.grid().cell(0, 1).expect("cell 0,1").char(), 'm');
        }
    }

    #[test]
    fn ris_does_not_drop_utf8_split_across_process_calls() {
        for profiling in [false, true] {
            let t = ris_then_split_tail(profiling, b"\x1bc\xe4\xb8", b"\xadA");
            let cell = t.grid().cell(0, 0).expect("cell 0,0");
            assert_eq!(
                cell.char(),
                '\u{4E2D}',
                "profiling={profiling}: {:?}",
                row_text(&t, 0)
            );
            assert!(cell.is_wide());
            assert_eq!(t.grid().cell(0, 2).expect("cell 0,2").char(), 'A');
        }
    }

    #[test]
    fn ris_does_not_print_a_dcs_payload_split_across_process_calls() {
        for profiling in [false, true] {
            let t = ris_then_split_tail(
                profiling,
                b"\x1bc\x1bP0;0;0q\"1;1;1;1#0;2;0;0;0",
                b"~\x1b\\",
            );
            let text = row_text(&t, 0);
            assert!(
                !text.contains('~') && !text.contains("#0"),
                "profiling={profiling}: sixel payload printed as text: {text:?}"
            );
        }
    }

    /// Negative control for piecewise selection recovery: the retained splice
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

    /// A full-screen scroll is one uniform row of motion whatever history does:
    #[test]
    fn content_scroll_state_tracks_uniform_scroll_independent_of_history() {
        for (case, ring, setup) in [
            ("zero history capacity", 0, &b""[..]),
            ("saturated history", 1, &b"\x1b[3;1H\n"[..]),
            ("alt screen", 8, &b"\x1b[?1049h"[..]),
        ] {
            let mut term = TerminalBuilder::new()
                .size(3, 12)
                .ring_buffer_size(ring)
                .build();
            term.process(setup);
            let history = term.grid().scrollback_lines();
            let before = term.content_scroll_state();

            term.process(b"\x1b[3;1H\n");

            assert_eq!(
                term.grid().scrollback_lines(),
                history,
                "{case}: the history count does not move"
            );
            assert_eq!(
                term.content_scroll_state().uniform_up_rows,
                before.uniform_up_rows + 1,
                "{case}: the content-motion signal is independent of retained history"
            );
            assert_eq!(
                term.content_scroll_state().invalidation_epoch,
                before.invalidation_epoch,
                "{case}"
            );
        }
    }

    #[test]
    fn content_scroll_state_invalidates_interior_and_top_anchored_regions() {
        let mut term = TerminalBuilder::new()
            .size(4, 12)
            .ring_buffer_size(8)
            .build();

        let before_interior = term.content_scroll_state();
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
        assert_eq!(
            after_interior.band_batches,
            before_interior.band_batches + 1
        );
        assert_eq!(after_interior.band_seq, before_interior.band_seq + 1);
        assert_eq!(
            after_interior.band(before_interior.band_seq),
            aterm_grid::RowBandMove {
                top: 1,
                bottom: 2,
                delta: -1
            }
        );

        let before_top_anchored = after_interior;
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
        assert_eq!(
            after_top_anchored.band_batches,
            before_top_anchored.band_batches + 1
        );
        assert_eq!(
            after_top_anchored.band(before_top_anchored.band_seq),
            aterm_grid::RowBandMove {
                top: 0,
                bottom: 2,
                delta: -1
            }
        );
    }

    /// The host-coordinate epoch still bumps on every alt switch spelling: the
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

    /// An erase rewrites cells in place and moves nothing, so it invalidates no host-cached grid
    #[test]
    fn an_erase_rewrites_cells_in_place_and_invalidates_no_host_coordinate() {
        for spelling in [
            &b"\x1b[J"[..],   // ED 0
            &b"\x1b[0J"[..],  // ED 0, explicit
            &b"\x1b[1J"[..],  // ED 1
            &b"\x1b[2J"[..],  // ED 2
            &b"\x1b[?J"[..],  // DECSED 0
            &b"\x1b[?1J"[..], // DECSED 1
            &b"\x1b[?2J"[..], // DECSED 2
            &b"\x1b#8"[..],   // DECALN
        ] {
            let mut term = TerminalBuilder::new()
                .size(4, 12)
                .ring_buffer_size(8)
                .build();
            term.process(b"\x1b[2;1Hthe quick");
            let before = term.content_scroll_state();
            term.process(spelling);
            let after = term.content_scroll_state();
            assert_eq!(
                after.invalidation_epoch, before.invalidation_epoch,
                "{spelling:?} rewrites cells in place: no host coordinate went stale"
            );
            assert_eq!(
                after.uniform_up_rows, before.uniform_up_rows,
                "{spelling:?} is not a translation either"
            );
        }

        for spelling in [
            &b"\x1b[K"[..],          // EL 0
            &b"\x1b[2K"[..],         // EL 2
            &b"\x1b[1;1;4;12$z"[..], // DECERA over the whole screen
        ] {
            let mut term = TerminalBuilder::new()
                .size(4, 12)
                .ring_buffer_size(8)
                .build();
            term.process(b"\x1b[2;1Hthe quick");
            let before = term.content_scroll_state();
            term.process(spelling);
            assert_eq!(
                term.content_scroll_state().invalidation_epoch,
                before.invalidation_epoch,
                "{spelling:?} was already honest and must stay so"
            );
        }

        let mut term = TerminalBuilder::new()
            .size(4, 12)
            .ring_buffer_size(8)
            .build();
        term.process(b"\x1b[2;1Hthe quick");
        let before = term.content_scroll_state();
        term.process(b"\x1b[1;1H\x1b[L"); // IL — rows below the cursor move down
        assert_eq!(
            term.content_scroll_state().invalidation_epoch,
            before.invalidation_epoch + 1,
            "an insert-line MOVES rows and must still invalidate host coordinates"
        );
    }

    /// ED 3 (the `clear` command's leading `ESC[3J`) evicts history and moves no live
    #[test]
    fn discarding_scrollback_evicts_history_and_moves_no_live_coordinate() {
        use crate::selection::{SelectionSide, SelectionType};

        for spelling in [&b"\x1b[3J"[..], &b"\x1b[3J\x1b[H\x1b[2J"[..]] {
            let mut term = TerminalBuilder::new()
                .size(4, 12)
                .ring_buffer_size(64)
                .build();
            for i in 0..20 {
                term.process(format!("line-{i}\r\n").as_bytes());
            }
            term.process(b"\x1b[2;1Hthe quick");
            assert!(
                term.grid().scrollback_lines() > 0,
                "precondition: there is history for {spelling:?} to discard"
            );

            let rows = term.grid().rows();
            let before = term.content_scroll_state();
            let live_before: Vec<(u64, Option<String>)> = (0..rows)
                .map(|r| (term.grid().visible_to_absolute(r), term.row_text(r.into())))
                .collect();

            term.process(spelling);

            let after = term.content_scroll_state();
            assert_eq!(
                after.invalidation_epoch, before.invalidation_epoch,
                "{spelling:?} discards HISTORY: no live host coordinate went stale"
            );
            assert_eq!(
                after.uniform_up_rows, before.uniform_up_rows,
                "{spelling:?} is not a translation either"
            );
            assert_eq!(
                term.grid().scrollback_lines(),
                0,
                "{spelling:?} really did discard the scrollback"
            );
            let live_after: Vec<(u64, Option<String>)> = (0..rows)
                .map(|r| (term.grid().visible_to_absolute(r), term.row_text(r.into())))
                .collect();
            let abs_before: Vec<u64> = live_before.iter().map(|(a, _)| *a).collect();
            let abs_after: Vec<u64> = live_after.iter().map(|(a, _)| *a).collect();
            assert_eq!(
                abs_before, abs_after,
                "{spelling:?}: visible row r must keep its absolute number"
            );
            if spelling == b"\x1b[3J" {
                assert_eq!(
                    live_before, live_after,
                    "ED 3 alone: visible row r, column c is the same cell before and after"
                );
            }
        }

        let mut term = TerminalBuilder::new()
            .size(4, 12)
            .ring_buffer_size(64)
            .build();
        for i in 0..20 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        assert!(
            term.text_selection().has_selection(),
            "precondition: a selection exists to destroy"
        );
        term.process(b"\x1b[3J");
        assert!(
            !term.text_selection().has_selection(),
            "ED 3 discards the coordinate space the selection named: it must still clear"
        );

        let mut term = TerminalBuilder::new()
            .size(4, 12)
            .ring_buffer_size(64)
            .build();
        term.process(b"\x1b[2;1Hthe quick");
        let before = term.content_scroll_state();
        term.process(b"\x1b[1;1H\x1b[L");
        assert_eq!(
            term.content_scroll_state().invalidation_epoch,
            before.invalidation_epoch + 1,
            "an insert-line MOVES rows and must still invalidate host coordinates"
        );
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

        term.process(b"\x1b[3;1H\n\n\n");
        let uniform = term.content_scroll_state();
        assert_eq!(uniform.uniform_up_rows, initial.uniform_up_rows + 3);
        assert_eq!(uniform.invalidation_epoch, initial.invalidation_epoch);

        term.process(b"\x1b[3;1H\n\x1b[2;3r\x1b[3;1H\n");
        let mixed = term.content_scroll_state();
        assert_eq!(mixed.uniform_up_rows, uniform.uniform_up_rows);
        assert_eq!(mixed.invalidation_epoch, uniform.invalidation_epoch + 1);
        assert_eq!(
            mixed.band_batches, uniform.band_batches,
            "a uniform scroll mixed with a band is not one transform: the epoch step stays unexplained"
        );
    }

    /// A Codex Enter (`ESC[?2026h ESC[12;1H ESC[J` then `ESC[12;57r ESC[12;1H
    #[test]
    fn a_codex_enter_batch_is_one_composed_band_batch() {
        let mut term = TerminalBuilder::new()
            .size(57, 151)
            .ring_buffer_size(1000)
            .build();
        term.process(b"\x1b[14;3Hhello");
        let before = term.content_scroll_state();
        term.process(
            b"\x1b[?2026h\x1b[12;1H\x1b[J\x1b[12;57r\x1b[12;1H\x1bM\x1bM\x1bM\x1bM\x1bM\x1b[r\x1b[1;16r\x1b[11;1H\r\n",
        );
        let after = term.content_scroll_state();
        assert_eq!(after.uniform_up_rows, before.uniform_up_rows);
        assert_eq!(after.invalidation_epoch, before.invalidation_epoch + 1);
        assert_eq!(after.band_batches, before.band_batches + 1);
        assert_eq!(
            after.band_seq,
            before.band_seq + 1,
            "five RIs compose into one move"
        );
        assert_eq!(
            after.band(before.band_seq),
            aterm_grid::RowBandMove {
                top: 11,
                bottom: 56,
                delta: 5
            }
        );
        assert_eq!(
            super::super::state::ContentScrollState::delta_since(Some(before), after),
            super::super::state::ContentScrollDelta::Bands {
                first_seq: before.band_seq,
                count: 1
            }
        );
    }

    /// A Codex streamed line with the viewport at the bottom (`ESC[1;52r ESC[52;1H
    #[test]
    fn a_codex_phase_b_line_is_an_explained_top_anchored_band() {
        let mut term = TerminalBuilder::new()
            .size(57, 151)
            .ring_buffer_size(1000)
            .build();
        term.process(b"\x1b[1;1Htranscript row zero");
        let before = term.content_scroll_state();
        let history_before = term.grid().scrollback_lines();
        term.process(b"\x1b[?2026h\x1b[1;52r\x1b[52;1H\r\nx");
        let after = term.content_scroll_state();
        assert_eq!(after.uniform_up_rows, before.uniform_up_rows);
        assert_eq!(after.invalidation_epoch, before.invalidation_epoch + 1);
        assert_eq!(after.band_batches, before.band_batches + 1);
        assert_eq!(
            after.band(before.band_seq),
            aterm_grid::RowBandMove {
                top: 0,
                bottom: 51,
                delta: -1
            }
        );
        assert_eq!(term.grid().scrollback_lines(), history_before + 1);
        term.process(b"y");
        assert_eq!(
            term.content_scroll_state(),
            after,
            "plain output after a splice cannot replay its coordinate flag"
        );
    }

    /// Every coordinate move the band record cannot describe still bumps the
    #[test]
    fn a_poisoned_or_mixed_batch_advances_the_epoch_without_a_band_batch() {
        let mut term = TerminalBuilder::new()
            .size(10, 20)
            .ring_buffer_size(8)
            .build();
        term.process(b"\x1b[3;1Hsome text here");

        let before = term.content_scroll_state();
        term.process(b"\x1b[?69h\x1b[2;6s\x1b[2;5r\x1b[3;1H\x1b[S");
        let after_margined = term.content_scroll_state();
        assert_eq!(
            after_margined.invalidation_epoch,
            before.invalidation_epoch + 1,
            "a margined SU moves rows"
        );
        assert_eq!(
            after_margined.band_batches, before.band_batches,
            "a rectangle scroll is not a row-band translate"
        );

        term.process(b"\x1bc");
        let after_ris = term.content_scroll_state();
        assert_eq!(
            after_ris.invalidation_epoch,
            after_margined.invalidation_epoch + 1
        );
        assert_eq!(after_ris.band_batches, after_margined.band_batches);

        term.process(b"\x1b[?1049h");
        let after_alt = term.content_scroll_state();
        assert_eq!(
            after_alt.invalidation_epoch,
            after_ris.invalidation_epoch + 1
        );
        assert_eq!(after_alt.band_batches, after_ris.band_batches);
        assert_eq!(
            after_alt.band_seq, before.band_seq,
            "no move was published by any of the three"
        );
    }

    /// While the user is scrolled back, live output (e.g. `tail -f`) does not
    #[test]
    fn scrolled_back_viewport_pins_during_live_output() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(200)
            .build();

        for i in 0..20 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        term.scroll_display(8); // display_offset = 8
        let offset_before = term.grid().display_offset();
        assert!(offset_before > 0, "precondition: user is scrolled back");
        let top_before = term.display_row_text(0).unwrap_or_default();
        assert!(
            top_before.contains("line-"),
            "precondition: a scrollback line is visible at the top, got {top_before:?}",
        );

        for i in 20..30 {
            term.process(format!("live-{i}\r\n").as_bytes());
        }

        let top_after = term.display_row_text(0).unwrap_or_default();
        assert_eq!(
            top_after, top_before,
            "SCR-1: the viewport must stay pinned on the same content during live output",
        );
        let offset_after = term.grid().display_offset();
        assert_eq!(
            offset_after,
            offset_before + 10,
            "display_offset must advance by the number of lines entering scrollback",
        );
        for r in 0..4 {
            let row = term.display_row_text(r).unwrap_or_default();
            assert!(
                !row.contains("live-2") && !row.contains("live-29"),
                "live output must not appear in the pinned viewport, row{r}={row:?}",
            );
        }
    }

    /// The close counter alone cannot authorize a close+immediate-reopen
    #[test]
    fn sync_open_dirty_tracks_the_current_episode_only() {
        let clock = super::ClockReading::now();
        let mut term = Terminal::new(24, 80);
        assert!(!term.sync_open_dirty());

        term.process_at(b"\x1b[?2026h", clock);
        assert!(term.modes().synchronized_output());
        assert!(
            !term.sync_open_dirty(),
            "the opening delimiter itself starts a clean episode"
        );

        term.process_at(b"old-frame", clock);
        assert!(term.sync_open_dirty(), "output dirties the open episode");
        let before_close = term.sync_end_seq();

        term.process_at(b"\x1b[?2026l\x1b[?2026h", clock);
        assert!(term.modes().synchronized_output());
        assert_eq!(term.sync_end_seq(), before_close + 1);
        assert!(
            !term.sync_open_dirty(),
            "close+reopen with no following action leaves the new episode clean"
        );

        term.process_at(b"next-frame-prefix", clock);
        assert!(
            term.sync_open_dirty(),
            "a prefix after the reopen forbids presenting the prior close from the current grid"
        );
        term.process_at(b"\x1b[?2026h", clock);
        assert!(
            term.sync_open_dirty(),
            "a redundant DECSET must not launder a dirty episode"
        );

        term.process_at(b"\x1b[?2026l", clock);
        assert!(!term.modes().synchronized_output());
        assert!(!term.sync_open_dirty(), "an ordinary close clears dirty");
    }

    /// Every parser action family that can commit presented state feeds the
    #[test]
    fn sync_open_dirty_covers_parser_action_families() {
        let clock = super::ClockReading::now();
        let mut term = Terminal::new(24, 80);
        let actions: &[(&str, &[u8])] = &[
            ("bulk print", b"text"),
            ("unicode print", "éé".as_bytes()),
            ("execute", b"\x08"),
            ("CSI", b"\x1b[2C"),
            ("CSI subparams", b"\x1b[4:3m"),
            ("ESC", b"\x1b7"),
            ("OSC", b"\x1b]0;sync-dirty\x07"),
            ("DCS commit", b"\x1bP$qm\x1b\\"),
            ("APC commit", b"\x1b_not-kitty\x1b\\"),
        ];

        for &(name, action) in actions {
            term.process_at(b"\x1b[?2026l\x1b[?2026h", clock);
            assert!(term.modes().synchronized_output(), "{name}: episode open");
            assert!(!term.sync_open_dirty(), "{name}: episode starts clean");
            term.process_at(action, clock);
            assert!(term.sync_open_dirty(), "{name}: action must mark dirty");
        }
    }

    /// The timeout force-clear ends the mode so a crashed app cannot freeze the screen, but the
    #[test]
    fn sync_frame_unfinished_is_the_programs_account_and_survives_the_timeout() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.sync_frame_unfinished());

        term.process(b"\x1b[?2026h");
        assert!(
            !term.sync_frame_unfinished(),
            "the opening delimiter alone writes nothing"
        );
        term.process(b"\x1b[?25l\x1b[3;1Hpartial");
        assert!(term.sync_frame_unfinished(), "written into: unfinished");

        term.backdate_sync_start(Duration::from_secs(120));
        term.process(b"more");
        assert!(
            !term.modes().synchronized_output() && !term.sync_open_dirty(),
            "control: the timeout force-cleared the mode-level signals"
        );
        assert!(
            term.sync_frame_unfinished(),
            "the program has not finished its frame"
        );
        term.process(b"\x1b[?2026h");
        assert!(
            term.sync_frame_unfinished(),
            "a redundant open after the timeout does not launder the frame"
        );

        term.process(b"\x1b[?25h\x1b[?2026l");
        assert!(!term.sync_frame_unfinished(), "the program's close ends it");

        term.process(b"\x1b[?2026hframe\x1b[?2026l\x1b[?2026h");
        assert!(!term.sync_frame_unfinished());
        term.process(b"x");
        assert!(term.sync_frame_unfinished());

        for (name, reset) in [("DECSTR", &b"\x1b[!p"[..]), ("RIS", b"\x1bc")] {
            term.process(b"\x1b[?2026hdirty");
            term.backdate_sync_start(Duration::from_secs(120));
            term.process(b"y");
            assert!(
                term.sync_frame_unfinished(),
                "{name}: timed out, unfinished"
            );
            term.process(reset);
            assert!(!term.sync_frame_unfinished(), "{name} ends the frame");
        }
    }

    /// A program that dies mid-frame leaves its bracket open by its own
    #[test]
    fn a_prompt_after_a_dead_programs_frame_ends_it() {
        let prompt = b"\x1b]133;A\x07$ ";
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2026h\x1b[?25l\x1b[3;1Hpartial");
        term.process(prompt);
        assert!(
            term.sync_frame_unfinished(),
            "control: a prompt inside a live bracket is the frame's own"
        );
        term.backdate_sync_start(Duration::from_secs(120));
        term.process(b"x");
        assert!(term.sync_frame_unfinished(), "timed out, still unfinished");
        term.process(prompt);
        assert!(
            !term.sync_frame_unfinished(),
            "the prompt ends the dead frame"
        );

        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?1049h\x1b[?2026h\x1b[?25l\x1b[3;1Hpartial");
        term.process(prompt);
        assert!(!term.is_alternate_screen(), "the orphaned screen is left");
        assert!(!term.sync_frame_unfinished(), "and its frame ended with it");
    }

    #[test]
    fn sync_open_dirty_clears_on_decstr_and_ris() {
        let mut term = Terminal::new(24, 80);

        term.process(b"\x1b[?2026hdirty");
        assert!(term.sync_open_dirty());
        term.process(b"\x1b[!p");
        assert!(!term.modes().synchronized_output());
        assert!(
            !term.sync_open_dirty(),
            "DECSTR closes and clears the episode"
        );

        term.process(b"\x1b[?2026hdirty");
        assert!(term.sync_open_dirty());
        term.process(b"\x1bc");
        assert!(!term.modes().synchronized_output());
        assert!(!term.sync_open_dirty(), "RIS closes and clears the episode");
    }

    /// A forward-dated start (clock skew, e.g. checkpoint restore) neither
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

        term.backdate_sync_start(Duration::from_secs(7200));
        term.process(b"y");
        assert!(
            !term.modes().synchronized_output(),
            "sync output must disable after the skewed deadline passes"
        );
    }

    /// Repaint-blink epoch, positive: the per-keystroke pattern
    #[test]
    fn repaint_blink_epoch_bumps_once_per_claude_burst() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.repaint_blink_epoch(), 0, "fresh terminal starts at 0");
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

    /// Repaint-blink epoch, negative: a DECTCEM hide outside a synchronized
    #[test]
    fn bare_hide_and_vim_style_output_never_bump_the_blink_epoch() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?25l\x1b[5;10Hx\x1b[?25h");
        assert_eq!(term.repaint_blink_epoch(), 0, "bare hide: no sync, no bump");
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
        term.process(b"\x1b[?2026htear-free write\x1b[?2026l");
        assert_eq!(term.repaint_blink_epoch(), 0, "sync alone never bumps");
        term.process(b"\x1b[?2026h\x1b[?25h\x1b[?2026l");
        assert_eq!(
            term.repaint_blink_epoch(),
            0,
            "show-inside-sync never bumps"
        );
        term.process(b"\x1b[?2026h\x1b[?25lredraw\x1b[?25h\x1b[?2026l");
        assert_eq!(term.repaint_blink_epoch(), 1);
    }

    /// `process_at` is a pure function of `(bytes, ClockReading)`: the bell
    #[test]
    fn process_at_is_deterministic_under_injected_clock() {
        use super::ClockReading;
        use aterm_time::Instant;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

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

        let base = Instant::now(); // test harness base, fed back as injected input
        let a = run(base);
        let b = run(base);
        assert_eq!(
            a, b,
            "process_at must be deterministic for a fixed (bytes, clock) schedule"
        );

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
