// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DEC private mode handler.

use crate::grid::{CellFlags, Grid};

use super::handler::TerminalHandler;
use super::stack_response::StackResponse;
use super::types::CurrentStyle;
use super::{MouseEncoding, MouseMode, SavedCursorState};
use aterm_types::CharacterSetState;
use core::fmt::Write as _;

impl TerminalHandler<'_> {
    /// DECSET/DECRST (`CSI ? Ps h/l`).
    #[allow(
        clippy::too_many_lines,
        reason = "match on ~30 DEC private mode numbers per spec"
    )]
    pub(super) fn handle_dec_mode(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
        set: bool,
    ) {
        for &param in params {
            match param {
                1 => {
                    if set {
                        self.enable_application_cursor_keys();
                    } else {
                        self.disable_application_cursor_keys();
                    }
                }
                2 => {
                    self.set_vt52_mode(!set);
                }
                3 =>
                {
                    #[allow(clippy::collapsible_match)]
                    if self.modes.deccolm_enable {
                        self.set_column_mode_132(set);
                    }
                }
                5 => {
                    self.set_reverse_video(set);
                }
                6 => {
                    if set {
                        self.enable_origin_mode();
                    } else {
                        self.disable_origin_mode();
                    }
                }
                7 => {
                    if set {
                        self.enable_auto_wrap();
                    } else {
                        self.disable_auto_wrap();
                    }
                }
                12 =>
                {
                    #[allow(
                        clippy::collapsible_match,
                        reason = "an active console host still consumes recognized mode 12"
                    )]
                    if !self.modes.win32_input_active() {
                        self.set_cursor_blink(set);
                    }
                }
                25 => {
                    if set {
                        self.show_cursor();
                    } else {
                        // Repaint-blink epoch: a DECTCEM hide inside a DEC 2026 update is
                        // full-redraw choreography (`?2026h ?25l … ?25h ?2026l`); a hide
                        // without sync is not. Counted on every hide, so re-hides register.
                        if self.modes.synchronized_output {
                            *self.repaint_blink_epoch += 1;
                        }
                        self.hide_cursor();
                    }
                }
                45 => {
                    self.set_reverse_wraparound(set);
                }
                66 => {
                    self.set_application_keypad(set);
                }
                69 => {
                    self.set_left_right_margin_mode(set);
                }
                40 => {
                    self.modes.deccolm_enable = set;
                }
                80 => {
                    self.set_sixel_display_mode(set);
                }
                95 => {
                    self.modes.decncsm = set;
                }
                47 => {
                    if set {
                        self.enter_alternate_screen_raw(cap);
                    } else {
                        self.exit_alternate_screen_raw();
                    }
                }
                1047 => {
                    if set {
                        self.enter_alternate_screen_raw(cap);
                    } else {
                        self.exit_alternate_screen_1047();
                    }
                }
                1049 => {
                    if set {
                        self.enter_alternate_screen(cap);
                    } else {
                        let dead = self.take_dead_alt_screen();
                        self.exit_alternate_screen();
                        if dead {
                            self.release_dead_app_modes();
                        }
                    }
                }
                2004 => {
                    if set {
                        self.enable_bracketed_paste();
                    } else {
                        self.disable_bracketed_paste();
                    }
                    self.transient.bracketed_paste_from_prompt = self.shell.phase
                        == super::grouped_state::ShellIntegrationPhase::CommandFinished;
                }
                9 => {
                    if set {
                        self.enable_mouse_x10_tracking();
                    } else {
                        self.disable_mouse_tracking();
                    }
                }
                1000 => {
                    if set {
                        self.enable_mouse_normal_tracking();
                    } else {
                        self.disable_mouse_tracking();
                    }
                }
                1002 => {
                    if set {
                        self.enable_mouse_button_event_tracking();
                    } else {
                        self.disable_mouse_tracking();
                    }
                }
                1003 => {
                    if set {
                        self.enable_mouse_any_event_tracking();
                    } else {
                        self.disable_mouse_tracking();
                    }
                }
                1004 => {
                    if set {
                        // Counted on the off->on edge only, as DEC 2048's report is: a
                        // program re-sending `?1004h` with every frame arms nothing new.
                        if !self.modes.focus_reporting {
                            self.transient.focus_arms = self.transient.focus_arms.wrapping_add(1);
                        }
                        self.enable_focus_reporting();
                    } else {
                        self.disable_focus_reporting();
                    }
                }
                1007 => {
                    self.set_alternate_scroll(set);
                }
                1005 => {
                    if set {
                        self.enable_utf8_mouse_encoding();
                    } else {
                        self.disable_utf8_mouse_encoding();
                    }
                }
                1006 => {
                    if set {
                        self.enable_sgr_mouse_encoding();
                    } else {
                        self.disable_sgr_mouse_encoding();
                    }
                }
                1015 => {
                    if set {
                        self.enable_urxvt_mouse_encoding();
                    } else {
                        self.disable_urxvt_mouse_encoding();
                    }
                }
                1016 => {
                    if set {
                        self.enable_sgr_pixel_mouse_encoding();
                    } else {
                        self.disable_sgr_pixel_mouse_encoding();
                    }
                }
                2026 => {
                    if set {
                        self.enable_synchronized_output();
                    } else {
                        self.disable_synchronized_output();
                    }
                }
                2031 => {
                    // What this read's output did to the scheme before the mode moves is settled
                    // under the mode it was done under, as a read that ended here would settle it.
                    super::state_accessors::settle_color_scheme_after_output(
                        self.modes,
                        self.color,
                        self.policy,
                        self.transient,
                    );
                    self.modes.report_color_scheme = set;
                }
                2048 => {
                    let was = self.modes.in_band_size_reports;
                    self.modes.in_band_size_reports = set;
                    if !set {
                        self.transient.size_report_held = false;
                    }
                    if set && !was && self.hold_size_report() {
                        self.transient.size_report_held = true;
                    } else if set && !was {
                        let report = super::state_accessors::in_band_size_report(
                            self.grid.rows(),
                            self.grid.cols(),
                            self.iterm2.host_cell_px(),
                        );
                        self.send_report(cap, report.as_bytes());
                    }
                }
                1243 => {
                    self.set_bidi_arrow_swap(set);
                }
                2500 => {
                    self.set_bidi_box_mirroring(set);
                }
                2501 => {
                    self.set_bidi_autodetection(set);
                }
                67 => {
                    self.modes.backarrow_sends_bs = set;
                }
                1035 => {
                    self.modes.special_modifiers = set;
                }
                1036 => {
                    self.modes.meta_send_escape = set;
                }
                1039 => {
                    self.modes.alt_send_escape = set;
                }
                1045 => {
                    self.modes.mode_1045 = set;
                }
                1048 => {
                    if set {
                        self.cursor_state().save_cursor_state();
                    } else {
                        self.cursor_state().restore_cursor_state();
                    }
                }
                9001 if self.modes.win32_input_enabled => {
                    self.modes.win32_input_mode = set;
                }
                _ => {}
            }
            *self.evidence_asserted |= super::program_evidence::asserted_by_dec_mode(param, set);
        }
    }

    /// Record the main grid's absolute row counter as this batch parks it (smcup), before the
    /// swap, for the epilogue's re-pin: output earlier in the batch already advanced it, and
    /// the re-pin must include that advance.
    #[inline]
    const fn park_main_row_counter(&mut self) {
        self.transient.alt_park_main_row_counter = Some(self.grid.absolute_row_counter());
    }

    fn flatten_restored_display_offset(&mut self) {
        self.transient.alt_screen_left_in_batch = true;
        let restored = self.grid.display_offset();
        if restored > 0 {
            self.grid.pin_viewport_to_live_for_output_batch();
            self.transient.alt_restore_pin = Some((
                restored,
                self.grid.absolute_row_counter(),
                self.grid.scrollback_clear_gen(),
            ));
        }
        debug_assert_eq!(
            self.grid.display_offset(),
            0,
            "a grid swapped in mid-batch must process at offset 0"
        );
    }

    /// Enter the alternate screen for mode 47/1047: buffer swap only, no cursor save, no clear.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn enter_alternate_screen_raw(&mut self, cap: &super::response_capability::ResponseCapability) {
        if self.modes.alternate_screen {
            return;
        }

        self.transient.alt_entered_without_cursor_save = true;
        self.transient.conhost_alt_screen_left_up = false;
        self.kitty_keyboard.switch_screen(true);
        self.transient.current_hyperlink = None;
        self.transient.update_has_transient_extras();
        self.sixel.pending_image = None;
        let mut new_grid = self
            .alt_grid
            .take()
            .unwrap_or_else(|| Grid::with_scrollback(self.grid.rows(), self.grid.cols(), 0));
        new_grid.drop_resize_undo();
        new_grid.set_resize_law(self.grid.resize_law());
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        new_grid.set_pending_wrap(self.grid.pending_wrap());
        Self::copy_margins(self.grid, &mut new_grid);
        Self::sync_bce_template(self.style, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        self.note_screen_replaced();
        self.grid.invalidate_host_coordinates_for_swap();
        self.send_held_size_report(cap);
    }

    /// Point `grid`'s BCE template at the current pen's background. A buffer
    /// switch that keeps the pen (`?47`/`?1047`, either way) must do this for
    /// the grid it swaps in: xterm fills every blank from the one current
    /// colour pair (screen.c `ClearCells`, `ScrnClearLines`), whichever buffer
    /// is up.
    #[inline]
    const fn sync_bce_template(style: &mut CurrentStyle, grid: &mut Grid) {
        style.update_cached_colors();
        grid.set_cursor_template(
            crate::grid::Cell::bce_blank(style.cached_colors()),
            style.bce_bg_rgb(),
        );
    }

    /// Count a whole-screen replacement for the resize journal's readers
    /// (`Terminal::screen_replaced_count`): the screen the app painted before it
    /// is gone, so a displacement a resize left on it is gone too.
    #[inline]
    fn note_screen_replaced(&mut self) {
        self.transient.screen_replaced = self.transient.screen_replaced.wrapping_add(1);
        self.transient.render_debt = crate::terminal::RenderDebt::default();
    }

    /// Copy the scroll region and horizontal margins from one grid to another. In xterm they
    /// are fields of the shared `TScreen`, so switching buffers (47/1047/1049) keeps them.
    #[inline]
    const fn copy_margins(from: &Grid, to: &mut Grid) {
        let region = from.scroll_region();
        to.set_scroll_region(region.top, region.bottom);
        let margins = from.horizontal_margins();
        to.set_horizontal_margins(margins.left, margins.right);
    }

    /// Exit the alternate screen for mode 47: buffer swap only, no cursor restore, no clear.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn exit_alternate_screen_raw(&mut self) {
        if !self.modes.alternate_screen {
            self.keep_screen_from_conhost_repaint();
            return;
        }

        self.kitty_keyboard.switch_screen(false);
        self.transient.current_hyperlink = None;
        self.transient.update_has_transient_extras();
        self.sixel.pending_image = None;
        let tab_stops = self.grid.tab_stops().to_vec();
        let tab_suppressed = self.grid.tab_defaults_suppressed();
        if let Some(mut main_grid) = self.alt_grid.take() {
            let cursor = self.grid.cursor();
            main_grid.set_cursor(cursor.row, cursor.col);
            main_grid.set_pending_wrap(self.grid.pending_wrap());
            Self::copy_margins(self.grid, &mut main_grid);
            Self::sync_bce_template(self.style, &mut main_grid);
            let mut alt = std::mem::replace(self.grid, main_grid);
            alt.drop_resize_undo();
            *self.alt_grid = Some(alt);
            self.grid.damage_mut().mark_full();
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops, tab_suppressed);
        self.modes.alternate_screen = false;
        self.transient.alt_screen_resync_screen = None;
        self.note_screen_replaced();
        self.grid.invalidate_host_coordinates_for_swap();
        self.holder_fence.note_alt_exit();
    }

    /// Exit the alternate screen for mode 1047: clear it, then swap back. No cursor restore.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn exit_alternate_screen_1047(&mut self) {
        if !self.modes.alternate_screen {
            self.keep_screen_from_conhost_repaint();
            return;
        }

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.erase_screen();
        self.exit_alternate_screen_raw();
    }

    /// Enter the alternate screen for mode 1049: save the cursor, switch, clear the alt screen.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "alt_screen",
            action = "Enter",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    fn enter_alternate_screen(&mut self, cap: &super::response_capability::ResponseCapability) {
        if self.modes.alternate_screen {
            self.cursor_save.alt = Some(self.snapshot_cursor_state());
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
            self.grid.erase_screen();
            self.note_screen_replaced();
            self.holder_fence.note_alt_cleared();
            return;
        }

        self.cursor_save.main = Some(self.snapshot_cursor_state());
        self.transient.alt_entered_without_cursor_save = false;
        self.transient.conhost_alt_screen_left_up = false;
        self.kitty_keyboard.switch_screen(true);
        self.sixel.pending_image = None;
        self.transient.current_hyperlink = None;
        self.transient.update_has_transient_extras();
        let mut new_grid = Grid::with_scrollback(self.grid.rows(), self.grid.cols(), 0);
        new_grid.set_resize_law(self.grid.resize_law());
        new_grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        new_grid.erase_screen();
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        debug_assert!(
            !new_grid.pending_wrap(),
            "1049 SET ends in ClearScreen, which resets the deferred wrap"
        );
        Self::copy_margins(self.grid, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        self.note_screen_replaced();
        self.grid.invalidate_host_coordinates_for_swap();
        self.holder_fence.note_alt_cleared();
        self.send_held_size_report(cap);
    }

    /// Exit the alternate screen for mode 1049, restoring the saved cursor.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "alt_screen",
            action = "Leave",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    fn exit_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            self.keep_screen_from_conhost_repaint();
            return;
        }

        self.kitty_keyboard.switch_screen(false);
        self.sixel.pending_image = None;
        self.transient.current_hyperlink = None;
        self.transient.update_has_transient_extras();
        let tab_stops = self.grid.tab_stops().to_vec();
        let tab_suppressed = self.grid.tab_defaults_suppressed();
        if let Some(mut main_grid) = self.alt_grid.take() {
            Self::copy_margins(self.grid, &mut main_grid);
            *self.grid = main_grid;
            self.grid.damage_mut().mark_full();
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops, tab_suppressed);
        let saved = self.cursor_save.main;
        self.restore_cursor_snapshot(saved);
        self.modes.alternate_screen = false;
        self.transient.alt_screen_resync_screen = None;
        self.note_screen_replaced();
        self.grid.invalidate_host_coordinates_for_swap();
        self.holder_fence.note_alt_exit();
    }

    /// Leave an alternate screen no app is behind any more (the app died before `?1049l`).
    pub(super) fn leave_orphaned_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }
        self.alt_archive
            .leave_orphaned(self.grid, self.transient.sync_end_seq);
        let conhost = self.modes.win32_input_active();
        let conhost_cursor = (self.grid.cursor(), self.grid.pending_wrap());
        if self.transient.alt_entered_without_cursor_save || self.cursor_save.main.is_none() {
            self.exit_alternate_screen_1047();
        } else {
            self.exit_alternate_screen();
        }
        self.release_dead_app_modes();
        if conhost {
            self.scroll_screen_into_scrollback();
            let (cursor, pending_wrap) = conhost_cursor;
            self.grid.set_cursor(cursor.row, cursor.col);
            self.grid.set_pending_wrap(pending_wrap);
        }
        self.transient.conhost_alt_screen_left_up = conhost;
    }

    /// Whether the `?1049l` about to run leaves the alternate screen of an app that died on it
    /// while its console host was asked to leave it (`TransientState::dead_alt_screen`, a
    /// prompt under the host policy or `Terminal::expect_host_alt_screen_leave`): that is the
    /// host's forwarded switch, and it takes the mark.
    #[inline]
    fn take_dead_alt_screen(&mut self) -> bool {
        let dead = self.modes.alternate_screen
            && self.transient.dead_alt_screen == Some(self.transient.screen_replaced);
        if dead {
            self.transient.dead_alt_screen = None;
        }
        dead
    }

    fn release_dead_app_modes(&mut self) {
        self.grid.reset_scroll_region();
        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();
        self.release_dead_app_input();
        if self.modes.synchronized_output || self.transient.app_sync_open {
            self.disable_synchronized_output();
        }
    }

    /// Turn off what a dead app left on that decides what the person's input becomes.
    #[inline]
    pub(super) fn release_dead_app_input(&mut self) {
        self.modes.mouse_mode = MouseMode::None;
        self.modes.mouse_encoding = MouseEncoding::X10;
        self.modes.application_cursor_keys = false;
        self.modes.application_keypad = false;
        self.release_dead_app_reports();
    }

    /// Turn off the reports a dead app asked for that no shell does.
    #[inline]
    pub(super) fn release_dead_app_reports(&mut self) {
        self.modes.in_band_size_reports = false;
        self.transient.size_report_held = false;
        self.modes.report_color_scheme = false;
        self.transient.report_buffer.clear();
    }

    /// Whether DEC 2048's enable-time size report waits instead of going out at once:
    /// behind a Windows console host (the `ResizePolicy::ConPty` posture) with the
    /// primary screen up.
    #[inline]
    const fn hold_size_report(&self) -> bool {
        self.modes.win32_input_enabled && !self.modes.alternate_screen
    }

    /// The alternate screen came up: a size report held at the enable
    /// ([`Self::hold_size_report`]) goes out now, at the size the screen has, while
    /// the mode is still on.
    #[inline]
    fn send_held_size_report(&mut self, cap: &super::response_capability::ResponseCapability) {
        if std::mem::take(&mut self.transient.size_report_held) && self.modes.in_band_size_reports {
            let report = super::state_accessors::in_band_size_report(
                self.grid.rows(),
                self.grid.cols(),
                self.iterm2.host_cell_px(),
            );
            self.send_report(cap, report.as_bytes());
        }
    }

    /// A shell prompt (`133;A`).
    #[inline]
    pub(super) const fn drop_held_size_report(&mut self) {
        self.transient.size_report_held = false;
    }

    /// A shell prompt on a dead app's screen that the console host is asked to leave (the host policy, `prompt_leaves_orphaned_alt_screen`).
    pub(super) fn take_main_pen_for_dead_screen_prompt(&mut self) {
        let saved = self.cursor_save.main;
        *self.style = saved.map_or_else(CurrentStyle::default, |s| s.style);
        *self.charset = saved.map_or_else(CharacterSetState::default, |s| s.charset);
        self.transient.current_underline_color = saved.and_then(|s| s.underline_color);
        self.transient.update_has_transient_extras();
        self.style.update_cached_colors();
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
    }

    /// At a `133;A` prompt on the alternate screen, clear bracketed paste (2004) unless the shell switched paste after `133;D`.
    #[inline]
    pub(super) fn release_dead_app_paste(&mut self) {
        if !self.transient.bracketed_paste_from_prompt {
            self.disable_bracketed_paste();
        }
    }

    /// conhost is about to repaint over our primary screen (switching the alternate buffer it was left on, `TransientState::conhost_alt_screen_left_up`, which this consumes).
    #[inline]
    pub(super) fn keep_screen_from_conhost_repaint(&mut self) {
        if std::mem::take(&mut self.transient.conhost_alt_screen_left_up) {
            self.scroll_screen_into_scrollback();
        }
    }

    fn scroll_screen_into_scrollback(&mut self) {
        let region = self.grid.scroll_region();
        if region.top != 0 || region.bottom + 1 != self.grid.rows() {
            return;
        }
        let used = (0..self.grid.rows())
            .rev()
            .find(|&r| self.grid.row(r).is_some_and(|row| !row.is_empty()))
            .map_or(0, |r| usize::from(r) + 1);
        self.grid.scroll_region_up(used);
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetBracketedPaste",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_bracketed_paste(&mut self) {
        self.modes.bracketed_paste = true;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetBracketedPaste",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_bracketed_paste(&mut self) {
        self.modes.bracketed_paste = false;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_mouse_x10_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::X10;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_mouse_normal_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::Normal;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_mouse_button_event_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::ButtonEvent;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_mouse_any_event_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::AnyEvent;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_mouse_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::None;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetSgrMouseEncoding",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_sgr_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Sgr;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetSgrMouseEncoding",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_sgr_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Sgr {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    /// Capture cursor position, style, charset and mode flags.
    #[inline]
    const fn snapshot_cursor_state(&self) -> SavedCursorState {
        SavedCursorState {
            cursor: self.grid.cursor(),
            style: *self.style,
            origin_mode: self.modes.origin_mode,
            auto_wrap: self.modes.auto_wrap,
            charset: *self.charset,
            pending_wrap: self.grid.pending_wrap(),
            underline_color: self.transient.current_underline_color,
            resized: false,
        }
    }

    fn restore_cursor_snapshot(&mut self, state: Option<SavedCursorState>) {
        let Some(state) = state else {
            self.modes.origin_mode = false;
            *self.style = CurrentStyle::default();
            *self.charset = CharacterSetState::default();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid.set_cursor(0, 0);
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        };
        {
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            self.transient.current_underline_color = state.underline_color;
            self.transient.update_has_transient_extras();
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );

            let (row, col) = if state.origin_mode {
                let region = self.grid.scroll_region();
                let clamped_row = state.cursor.row.clamp(region.top, region.bottom);
                let clamped_col = if self.modes.left_right_margin_mode {
                    let margins = self.grid.horizontal_margins();
                    state.cursor.col.clamp(margins.left, margins.right)
                } else {
                    state.cursor.col
                };
                (clamped_row, clamped_col)
            } else {
                (state.cursor.row, state.cursor.col)
            };
            super::handler_state::place_restored_cursor(self.grid, &state, row, col);
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
    }

    /// DECRQM (`CSI ? Ps $ p`): answers `CSI ? Ps ; Pm $ y`, Pm 0 not recognized, 1 set, 2 reset, 3 permanently set, 4 permanently reset.
    pub(super) fn handle_decrqm(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let mode = params.first().copied().unwrap_or(0);

        #[inline]
        const fn state(active: bool) -> u8 {
            if active { 1 } else { 2 }
        }

        let mode_state: u8 = if let Some(active) = self.query_dec_mode(mode) {
            state(active)
        } else {
            match mode {
                1048 => {
                    let saved = if self.modes.alternate_screen {
                        self.cursor_save.alt.is_some()
                    } else {
                        self.cursor_save.main.is_some()
                    };
                    state(saved)
                }
                4 | 8 | 18 | 19 | 2027 => 4,
                _ => 0,
            }
        };

        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// ANSI DECRQM (`CSI Ps $ p`).
    pub(super) fn handle_ansi_decrqm(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let mode = params.first().copied().unwrap_or(0);

        #[inline]
        const fn state(active: bool) -> u8 {
            if active { 1 } else { 2 }
        }

        let mode_state: u8 = match mode {
            4 => state(self.modes.insert_mode),
            8 => {
                // BDSM — set means implicit BiDi (the default).
                state(self.modes.bidi_mode == crate::config::BiDiMode::Implicit)
            }
            20 => state(self.modes.new_line_mode),
            12 => 4,
            _ => 0,
        };

        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// SM/RM (`CSI Ps h/l`).
    pub(super) fn handle_ansi_mode(&mut self, params: &[u16], set: bool) {
        for &param in params {
            match param {
                4 => {
                    if set {
                        self.enable_insert_mode();
                    } else {
                        self.disable_insert_mode();
                    }
                }
                8 => {
                    self.set_bidi_support_mode(set);
                }
                20 => {
                    if set {
                        self.enable_new_line_mode();
                    } else {
                        self.disable_new_line_mode();
                    }
                }
                _ => {}
            }
        }
    }

    fn parse_rect_coords(&self, params: &[u16]) -> Option<(u16, u16, u16, u16)> {
        let rows = self.grid.rows();
        let cols = self.grid.cols();

        let (row_offset, row_limit, col_offset, col_limit) = if self.modes.origin_mode {
            let region = self.grid.scroll_region();
            let margins = self.grid.horizontal_margins();
            (
                region.top,
                region.bottom + 1,
                margins.left,
                margins.right + 1,
            )
        } else {
            (0, rows, 0, cols)
        };

        let row_extent = row_limit - row_offset;
        let col_extent = col_limit - col_offset;

        let top = params
            .first()
            .copied()
            .unwrap_or(1)
            .max(1)
            .saturating_sub(1)
            .min(row_extent.saturating_sub(1))
            + row_offset;
        let left = params
            .get(1)
            .copied()
            .unwrap_or(1)
            .max(1)
            .saturating_sub(1)
            .min(col_extent.saturating_sub(1))
            + col_offset;
        let bottom = params
            .get(2)
            .copied()
            .map(|p| if p == 0 { row_extent } else { p })
            .unwrap_or(row_extent)
            .saturating_sub(1)
            .min(row_extent.saturating_sub(1))
            + row_offset;
        let right = params
            .get(3)
            .copied()
            .map(|p| if p == 0 { col_extent } else { p })
            .unwrap_or(col_extent)
            .saturating_sub(1)
            .min(col_extent.saturating_sub(1))
            + col_offset;

        if top > bottom || left > right {
            None
        } else {
            Some((top, left, bottom, right))
        }
    }

    /// DECERA, Erase Rectangular Area (VT420+).
    pub(super) fn handle_decera(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        self.grid.erase_rect(top, left, bottom, right);
    }

    /// DECCARA, Change Attributes in Rectangular Area (VT420+).
    pub(super) fn handle_deccara(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        let sgr_params = if params.len() > 4 {
            &params[4..]
        } else {
            &[0u16][..]
        };
        let (flags_to_set, flags_to_clear) = Self::sgr_params_to_flags(sgr_params);

        if self.modes.stream_attribute_extent {
            self.grid
                .change_attrs_stream(top, left, bottom, right, flags_to_set, flags_to_clear);
        } else {
            self.grid
                .change_attrs_rect(top, left, bottom, right, flags_to_set, flags_to_clear);
        }
    }

    /// DECRARA, Reverse Attributes in Rectangular Area (VT400+).
    pub(super) fn handle_decrara(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        let sgr_params = if params.len() > 4 {
            &params[4..]
        } else {
            &[0u16][..]
        };
        let (toggle, toggle_underline) = Self::sgr_params_to_reverse_flags(sgr_params);
        if toggle.is_empty() && !toggle_underline {
            return;
        }

        if self.modes.stream_attribute_extent {
            self.grid
                .reverse_attrs_stream(top, left, bottom, right, toggle, toggle_underline);
        } else {
            self.grid
                .reverse_attrs_rect(top, left, bottom, right, toggle, toggle_underline);
        }
    }

    fn sgr_params_to_reverse_flags(sgr_params: &[u16]) -> (CellFlags, bool) {
        let mut toggle = CellFlags::empty();
        let mut toggle_underline = false;

        for &param in sgr_params {
            match param {
                0 => {
                    toggle = toggle
                        .union(CellFlags::BOLD)
                        .union(CellFlags::BLINK)
                        .union(CellFlags::INVERSE);
                    toggle_underline = true;
                }
                1 => toggle = toggle.union(CellFlags::BOLD),
                4 => toggle_underline = true,
                5 => toggle = toggle.union(CellFlags::BLINK),
                7 => toggle = toggle.union(CellFlags::INVERSE),
                8 => toggle = toggle.union(CellFlags::HIDDEN),
                _ => {}
            }
        }

        (toggle, toggle_underline)
    }

    /// DECCRA, Copy Rectangular Area (VT420+).
    pub(super) fn handle_deccra(&mut self, params: &[u16]) {
        let Some((src_top, src_left, src_bottom, src_right)) = self.parse_rect_coords(params)
        else {
            return;
        };

        let dst_params = if params.len() > 5 {
            &params[5..]
        } else {
            &[1u16][..]
        };
        let (row_offset, col_offset) = if self.modes.origin_mode {
            let region = self.grid.scroll_region();
            let margins = self.grid.horizontal_margins();
            (region.top, margins.left)
        } else {
            (0, 0)
        };
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let dst_top = dst_params
            .first()
            .copied()
            .unwrap_or(1)
            .max(1)
            .saturating_sub(1)
            .min(rows.saturating_sub(1).saturating_sub(row_offset))
            + row_offset;
        let dst_left = dst_params
            .get(1)
            .copied()
            .unwrap_or(1)
            .max(1)
            .saturating_sub(1)
            .min(cols.saturating_sub(1).saturating_sub(col_offset))
            + col_offset;

        self.grid
            .copy_rect(src_top, src_left, src_bottom, src_right, dst_top, dst_left);
    }

    fn sgr_params_to_flags(sgr_params: &[u16]) -> (CellFlags, CellFlags) {
        let mut flags_to_set = CellFlags::empty();
        let mut flags_to_clear = CellFlags::empty();

        for &param in sgr_params {
            match param {
                0 => {
                    flags_to_clear = CellFlags::from_bits(CellFlags::VISUAL_FLAGS_MASK & !0x0600);
                    flags_to_set = CellFlags::empty();
                }
                1 => flags_to_set = flags_to_set.union(CellFlags::BOLD),
                2 => flags_to_set = flags_to_set.union(CellFlags::DIM),
                3 => flags_to_set = flags_to_set.union(CellFlags::ITALIC),
                4 => {
                    flags_to_clear = flags_to_clear.union(CellFlags::ALL_UNDERLINES);
                    flags_to_set = flags_to_set.union(CellFlags::UNDERLINE);
                }
                5 | 6 => flags_to_set = flags_to_set.union(CellFlags::BLINK),
                7 => flags_to_set = flags_to_set.union(CellFlags::INVERSE),
                8 => flags_to_set = flags_to_set.union(CellFlags::HIDDEN),
                9 => flags_to_set = flags_to_set.union(CellFlags::STRIKETHROUGH),
                22 => {
                    flags_to_clear = flags_to_clear.union(CellFlags::BOLD);
                    flags_to_clear = flags_to_clear.union(CellFlags::DIM);
                }
                23 => flags_to_clear = flags_to_clear.union(CellFlags::ITALIC),
                24 => {
                    flags_to_clear = flags_to_clear.union(CellFlags::ALL_UNDERLINES);
                }
                25 => flags_to_clear = flags_to_clear.union(CellFlags::BLINK),
                27 => flags_to_clear = flags_to_clear.union(CellFlags::INVERSE),
                28 => flags_to_clear = flags_to_clear.union(CellFlags::HIDDEN),
                29 => flags_to_clear = flags_to_clear.union(CellFlags::STRIKETHROUGH),
                _ => {}
            }
        }

        (flags_to_set, flags_to_clear)
    }

    /// DECFRA, Fill Rectangular Area (VT420+).
    pub(super) fn handle_decfra(&mut self, params: &[u16]) {
        let ch_code = params.first().copied().unwrap_or(0);

        let printable = matches!(ch_code, 0x20..=0x7E | 0xA0..=0xFF);
        if !printable {
            return;
        }

        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let fill = crate::grid::Cell::from_raw_parts(ch_code, colors, flags);

        let fg_rgb = self.style.fg.is_rgb().then(|| {
            let (r, g, b) = self.style.fg.rgb_components();
            [r, g, b]
        });
        let bg_rgb = self.style.bg.is_rgb().then(|| {
            let (r, g, b) = self.style.bg.rgb_components();
            [r, g, b]
        });

        let rect_params = if params.len() > 1 { &params[1..] } else { &[] };
        let Some((top, left, bottom, right)) = self.parse_rect_coords(rect_params) else {
            return;
        };

        self.grid
            .fill_rect(fill, top, left, bottom, right, fg_rgb, bg_rgb);
    }

    /// DECSERA, Selective Erase Rectangular Area (VT420+).
    pub(super) fn handle_decsera(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        self.grid.selective_erase_rect(top, left, bottom, right);
    }
}

#[cfg(test)]
#[path = "handler_dec_cursor_damage_scope_tests.rs"]
mod cursor_damage_scope_tests;

#[cfg(test)]
#[path = "handler_dec_decbkm_tests.rs"]
mod decbkm_tests;

/// Mode 2027 is permanently reset, since the print path does not keep each UAX #29 cluster
/// in one cell; Neovim 0.11+ sets it only on a reply of 2.
#[cfg(test)]
#[path = "handler_dec_grapheme_cluster_mode_tests.rs"]
mod grapheme_cluster_mode_tests;

#[cfg(test)]
#[path = "handler_dec_xterm_keyboard_mode_tests.rs"]
mod xterm_keyboard_mode_tests;

#[cfg(test)]
#[path = "handler_dec_win32_input_mode_tests.rs"]
mod win32_input_mode_tests;

impl super::Terminal {
    /// Force the primary screen: the host's recovery for an alternate screen a killed app left up.
    #[inline]
    pub fn leave_alternate_screen(&mut self) -> bool {
        self.leave_alternate_screen_at(super::ClockReading::now())
    }

    /// Force the primary screen at a caller-supplied clock reading ([`Self::leave_alternate_screen`]).
    pub fn leave_alternate_screen_at(&mut self, clock: super::ClockReading) -> bool {
        if !self.modes.alternate_screen {
            return false;
        }
        self.transient.host_leave_alternate_screen = true;
        self.process_at(b"", clock);
        debug_assert!(
            !self.modes.alternate_screen,
            "the batch takes the request first thing, and the handler refuses only a \
             screen that is not alternate"
        );
        self.host_journal
            .push_at(super::HostOp::LeaveAlternateScreen, clock);
        true
    }
}

#[cfg(test)]
#[path = "handler_dec_orphaned_alt_screen_tests.rs"]
mod orphaned_alt_screen_tests;

#[cfg(test)]
#[path = "handler_dec_conhost_left_alt_screen_tests.rs"]
mod conhost_left_alt_screen_tests;

/// `?1049l` on the primary screen moves nothing (see `exit_alternate_screen`:
/// xterm restores the DECSC slot there, Alacritty and WezTerm ignore it, and
/// aterm's own orphaned-screen recovery makes the restore dangerous).
#[cfg(test)]
#[path = "handler_dec_stray_1049_reset_tests.rs"]
mod stray_1049_reset_tests;

/// The BCE template across `?47`/`?1047` switches: the pen stays, so the
/// buffer swapped in must erase and scroll with the current background
/// (xterm's `ClearCells`/`ScrnClearLines` read the one colour pair).
#[cfg(test)]
#[path = "handler_dec_alt_switch_bce_tests.rs"]
mod alt_switch_bce_tests;

#[cfg(test)]
#[path = "conpty_dead_alt_screen_replay_tests.rs"]
mod conpty_dead_alt_screen_replay_tests;

#[cfg(test)]
#[path = "handler_dec_in_band_size_tests.rs"]
mod in_band_size_tests;

#[cfg(test)]
#[path = "handler_dec_mode_and_query_agreement_tests.rs"]
mod mode_and_query_agreement_tests;

include!("handler_dec_refinement.rs");
