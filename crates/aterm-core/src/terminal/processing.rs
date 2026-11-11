// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal input processing: the `process()` entry point and its batch prologue/epilogue.

use super::Terminal;

use aterm_types::BatchStages;

/// One reading of the clocks the terminal pipeline depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// a [`Terminal::process_at`] batch.
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

/// The profiling diagnostic's clock: real latency, which never feeds grid state.
#[expect(
    clippy::disallowed_methods,
    reason = "profiling diagnostic, gated: measures real latency and never feeds grid state"
)]
fn profiling_now() -> aterm_time::Instant {
    aterm_time::Instant::now()
}

impl Terminal {
    /// Process input bytes through the parser, reading the live clocks for time-dependent state.
    #[inline]
    pub fn process(&mut self, input: &[u8]) {
        self.process_at(input, ClockReading::now());
    }

    /// Test seam: with `on`, print text the reference way, as before multi-byte runs joined the
    /// ASCII between their characters and narrow glyphs reached the grid as a run: the parser
    /// ends each run at its first ASCII byte (`Parser::set_reference_runs`) and each narrow
    /// glyph of a run is its own grid write. The differential test
    /// (`tests/unicode_run_differential.rs`) holds the fast path to it.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn set_reference_print_path(&mut self, on: bool) {
        self.transient.reference_print_path = on;
        self.parser.set_reference_runs(on);
    }

    /// Play the Kitty graphics animations up to `now`.
    pub fn advance_kitty_animations(&mut self, now: aterm_time::Instant) -> KittyAnimationStep {
        if self.transient.kitty_frames.is_empty() {
            return KittyAnimationStep::default();
        }
        let (_, mut handler) = self.split_for_process();
        let (advanced, mutated, next_frame_at) = handler.advance_kitty_animations(now);
        if advanced {
            self.transient.screen_mutations = self.transient.screen_mutations.wrapping_add(1);
        }
        if mutated {
            self.host_journal.push_at(
                super::HostOp::AdvanceKittyAnimations,
                ClockReading {
                    monotonic: now,
                    wall_ms: None,
                },
            );
        }
        KittyAnimationStep {
            advanced,
            next_frame_at,
        }
    }

    /// Whether a Kitty graphics animation is running, shown on screen or not;
    /// an animation in an unshown session waits until it is drawn again.
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
        // The history the pin reads, which an erase of it in this batch voids. Every reset erases
        // the main grid's history, the one grid a pin can sit on (the alternate screen keeps none).
        let pinned_history = self.grid.scrollback_clear_gen();
        let lines_before = self.grid.absolute_row_counter();
        let was_alt = self.modes.alternate_screen;
        self.shell.probe_transitioned = false;
        if pinned_offset > 0 {
            self.grid.pin_viewport_to_live_for_output_batch();
        }
        let selection_before = self.text_selection.has_selection();

        // One body for every batch; the clock is read only where profiling is on as it begins.
        let entry = self
            .transient
            .pipeline_timestamps
            .profiling_enabled
            .then(profiling_now);
        self.advance_parser_archiving(input);
        if self.transient.pending_parser_reset {
            self.finish_ris_batch();
        }
        let parse_end = entry.map(|_| profiling_now());
        let damage_class = self.post_process(lines_before, was_alt);
        self.alt_archive_epilogue();
        self.note_batch_end();
        self.observe_at(self.transient.process_now);
        let stages = entry.zip(parse_end).map(|(entry, parse_end)| {
            let grid_end = profiling_now();
            BatchStages {
                parse: parse_end - entry,
                grid: grid_end - parse_end,
                total: grid_end - entry,
            }
        });
        self.transient
            .pipeline_timestamps
            .note_batch(input.len(), stages);

        if std::mem::take(&mut self.shell.probe_transitioned) {
            self.host_journal.push(super::HostOp::ForegroundJob {
                active: self.shell.foreground_job,
                probed: self.shell.probe_job_active,
                restore_cwd: false,
            });
        }

        self.reconcile_color_scheme_after_output();

        let parked_main_counter = self.transient.alt_park_main_row_counter.take();
        let unerased = |grid: &crate::grid::Grid| grid.scrollback_clear_gen() == pinned_history;
        if pinned_offset > 0 {
            let entered_alt = !was_alt && self.modes.alternate_screen;
            if entered_alt {
                let lines_added = parked_main_counter
                    .unwrap_or(lines_before)
                    .saturating_sub(lines_before);
                if let Some(main_grid) = self.alt_grid.as_mut().filter(|grid| unerased(grid)) {
                    main_grid.repin_display_offset(pinned_offset, lines_added);
                }
            } else if unerased(&self.grid) {
                let lines_added = self
                    .grid
                    .absolute_row_counter()
                    .saturating_sub(lines_before);
                self.grid.repin_display_offset(pinned_offset, lines_added);
            }
        }

        if let Some((restored_offset, counter_at_swap, gen_at_swap)) =
            self.transient.alt_restore_pin.take()
        {
            let restored_grid = if self.modes.alternate_screen {
                self.alt_grid.as_mut()
            } else {
                Some(&mut self.grid)
            };
            if let Some(grid) =
                restored_grid.filter(|grid| grid.scrollback_clear_gen() == gen_at_swap)
            {
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

    /// RIS side effects the handler cannot reach, run after the parser and
    /// before `post_process`.
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
        // before a protected footer. Keep every durable OSC/shell anchor paired
        // with the footer content that stayed fixed on screen.
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
                    self.end_synchronized_output();
                }
            }
        }
        damage_class
    }

    /// Test seam: with `on`, the engine takes every run of text lines one action at a time, as
    /// before line feeds at the bottom of a history-free scroll region were coalesced, so the
    /// differential tests can hold the coalesced path to it.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn set_reference_line_runs(&mut self, on: bool) {
        self.transient.reference_line_runs = on;
    }

    /// End an open synchronized-output (mode 2026) frame as its timeout does, without the parser, so a partly received character stays pending: whether one was open.
    pub fn end_synchronized_output(&mut self) -> bool {
        if !self.modes.synchronized_output {
            return false;
        }
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_open_dirty = false;
        self.transient.sync_end_seq += 1;
        true
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
        host_journal: _,
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
#[path = "processing_tests.rs"]
mod tests;
