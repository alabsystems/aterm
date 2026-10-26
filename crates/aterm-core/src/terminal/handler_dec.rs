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
    pub(super) fn handle_dec_mode(&mut self, params: &[u16], set: bool) {
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
                        self.enter_alternate_screen_raw();
                    } else {
                        self.exit_alternate_screen_raw();
                    }
                }
                1047 => {
                    if set {
                        self.enter_alternate_screen_raw();
                    } else {
                        self.exit_alternate_screen_1047();
                    }
                }
                1049 => {
                    if set {
                        self.enter_alternate_screen();
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
                        super::state_accessors::push_in_band_size_report(
                            self.transient,
                            self.grid.rows(),
                            self.grid.cols(),
                            self.iterm2.host_cell_px(),
                        );
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
            self.transient.alt_restore_pin = Some((restored, self.grid.absolute_row_counter()));
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
    fn enter_alternate_screen_raw(&mut self) {
        if self.modes.alternate_screen {
            return;
        }

        self.transient.alt_entered_without_cursor_save = true;
        self.transient.conhost_alt_screen_left_up = false;
        self.kitty_keyboard.switch_screen(true);
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
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
        self.send_held_size_report();
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
        self.transient.current_hyperlink_id = None;
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
    fn enter_alternate_screen(&mut self) {
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
        self.transient.current_hyperlink_id = None;
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
        self.send_held_size_report();
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
        self.transient.current_hyperlink_id = None;
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
    fn send_held_size_report(&mut self) {
        if std::mem::take(&mut self.transient.size_report_held) && self.modes.in_band_size_reports {
            super::state_accessors::push_in_band_size_report(
                self.transient,
                self.grid.rows(),
                self.grid.cols(),
                self.iterm2.host_cell_px(),
            );
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
            self.grid.set_cursor_following_content(row, col);
            if state.pending_wrap {
                self.grid
                    .set_pending_wrap(super::handler_state::at_a_line_end(self.grid, row, col));
            }
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

        let mode_state: u8 = match mode {
            1 => state(self.modes.application_cursor_keys),
            2 => state(!self.modes.vt52_mode),
            3 => state(self.modes.column_mode_132),
            5 => state(self.modes.reverse_video),
            6 => state(self.modes.origin_mode),
            7 => state(self.modes.auto_wrap),
            12 => state(self.modes.cursor_style.blinks()),
            25 => state(self.modes.cursor_visible),
            40 => state(self.modes.deccolm_enable),
            45 => state(self.modes.reverse_wraparound),
            66 => state(self.modes.application_keypad),
            67 => state(self.modes.backarrow_sends_bs),
            69 => state(self.modes.left_right_margin_mode),
            1035 => state(self.modes.special_modifiers),
            1036 => state(self.modes.meta_send_escape),
            1039 => state(self.modes.alt_send_escape),
            1045 => state(self.modes.mode_1045),
            80 => state(self.modes.sixel_display_mode),
            95 => state(self.modes.decncsm),
            9 => state(self.modes.mouse_mode == MouseMode::X10),
            1000 => state(self.modes.mouse_mode == MouseMode::Normal),
            1002 => state(self.modes.mouse_mode == MouseMode::ButtonEvent),
            1003 => state(self.modes.mouse_mode == MouseMode::AnyEvent),
            1004 => state(self.modes.focus_reporting),
            1007 => state(self.modes.alternate_scroll),
            1005 => state(self.modes.mouse_encoding == MouseEncoding::Utf8),
            1006 => state(self.modes.mouse_encoding == MouseEncoding::Sgr),
            1015 => state(self.modes.mouse_encoding == MouseEncoding::Urxvt),
            1016 => state(self.modes.mouse_encoding == MouseEncoding::SgrPixel),
            47 | 1047 | 1049 => state(self.modes.alternate_screen),
            1048 => {
                let saved = if self.modes.alternate_screen {
                    self.cursor_save.alt.is_some()
                } else {
                    self.cursor_save.main.is_some()
                };
                state(saved)
            }
            1243 => state(self.modes.bidi_arrow_swap),
            2004 => state(self.modes.bracketed_paste),
            2026 => state(self.modes.synchronized_output),
            2031 => state(self.modes.report_color_scheme),
            2048 => state(self.modes.in_band_size_reports),
            2500 => state(self.modes.bidi_box_mirroring),
            2501 => state(self.modes.bidi_autodetection),
            9001 if self.modes.win32_input_enabled => state(self.modes.win32_input_mode),
            4 | 8 => 4,
            18 | 19 => 4,
            2027 => 4,
            _ => 0,
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
mod cursor_damage_scope_tests {
    use crate::terminal::Terminal;

    #[test]
    fn a_cursor_only_change_damages_the_cursor_cell_not_the_whole_grid() {
        for (name, setup, seq) in [
            ("DECTCEM hide", &b""[..], &b"\x1b[?25l"[..]),
            ("DECTCEM show", &b"\x1b[?25l"[..], &b"\x1b[?25h"[..]),
            ("DECSCUSR shape", &b"\x1b[2 q"[..], &b"\x1b[6 q"[..]),
            ("mode 12 blink", &b"\x1b[?12l"[..], &b"\x1b[?12h"[..]),
        ] {
            let mut term = Terminal::new(24, 80);
            term.process(b"\x1b[9;20H");
            term.process(setup);
            term.take_damage();
            assert!(!term.has_damage(), "{name}: the fixture starts clean");

            term.process(seq);

            assert!(
                term.has_damage(),
                "{name}: a cursor-only change must still damage, or the \
                 frontend's redraw early-out swallows it"
            );
            assert!(
                !term.grid().damage().is_full(),
                "{name}: a cursor-only change must NOT damage the whole grid"
            );
            assert!(
                term.grid().damage().is_row_damaged(8),
                "{name}: the cursor's own row (0-based 8) carries the damage"
            );
            assert!(
                !term.grid().damage().is_row_damaged(0),
                "{name}: an unrelated row is untouched"
            );
        }
    }

    #[test]
    fn leaving_the_alternate_screen_marks_the_restored_grid_fully_damaged() {
        for (enter, exit) in [
            (&b"\x1b[?1049h"[..], &b"\x1b[?1049l"[..]),
            (b"\x1b[?1047h", b"\x1b[?1047l"),
            (b"\x1b[?47h", b"\x1b[?47l"),
        ] {
            let name = String::from_utf8_lossy(exit);
            let mut term = Terminal::new(8, 40);
            term.process(b"main text");
            term.take_damage();
            term.process(enter);
            term.process(b"alt text");
            let _ = term.damage_epoch();
            term.take_damage();
            let before = term.damage_epoch();
            term.process(exit);
            assert!(
                term.grid().damage().is_full(),
                "{name}: the restored main grid must be fully damaged"
            );
            assert!(
                term.damage_epoch() > before,
                "{name}: the swap back must advance the damage epoch"
            );
        }
    }

    #[test]
    fn a_whole_screen_change_still_marks_the_whole_grid() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[9;20H");
        term.take_damage();
        term.process(b"\x1b[?5h");
        assert!(
            term.grid().damage().is_full(),
            "reverse video repaints every cell and must mark the grid full"
        );
    }
}

#[cfg(test)]
mod decbkm_tests {
    use crate::terminal::Terminal;
    use aterm_types::keyboard::KeyboardMode;

    #[test]
    fn mode_67_toggles_backarrow_and_keyboard_mode() {
        let mut term = Terminal::new(24, 80);
        assert!(
            !term
                .keyboard_mode()
                .contains(KeyboardMode::BACKARROW_SENDS_BS)
        );
        term.process(b"\x1b[?67h");
        assert!(
            term.keyboard_mode()
                .contains(KeyboardMode::BACKARROW_SENDS_BS)
        );
        term.process(b"\x1b[?67l");
        assert!(
            !term
                .keyboard_mode()
                .contains(KeyboardMode::BACKARROW_SENDS_BS)
        );
    }
}

/// Mode 2027 is permanently reset, since the print path does not keep each UAX #29 cluster
/// in one cell; Neovim 0.11+ sets it only on a reply of 2.
#[cfg(test)]
mod grapheme_cluster_mode_tests {
    use crate::terminal::Terminal;

    #[test]
    fn mode_2027_is_permanently_reset() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2027$p");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?2027;4$y",
            "power-on"
        );
        term.process(b"\x1b[?2027h\x1b[?2027$p");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?2027;4$y",
            "after CSI ? 2027 h"
        );
        assert!(!term.modes().grapheme_cluster_mode);
        term.process(b"\x1bP$q?2027\x1b\\");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1bP1$r?2027l\x1b\\"
        );
    }
}

#[cfg(test)]
mod xterm_keyboard_mode_tests {
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, encode_key};

    #[test]
    fn mode_1035_special_modifiers_toggles_flag() {
        let mut term = Terminal::new(24, 80);
        assert!(
            !term
                .keyboard_mode()
                .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
        );
        term.process(b"\x1b[?1035l");
        assert!(
            term.keyboard_mode()
                .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
        );
        term.process(b"\x1b[?1035h");
        assert!(
            !term
                .keyboard_mode()
                .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
        );
    }

    #[test]
    fn mode_1039_reset_drops_alt_escape_prefix_in_encoding() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
            vec![0x1b, b'a']
        );
        term.process(b"\x1b[?1039l");
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
            vec![b'a']
        );
    }

    #[test]
    fn mode_1036_set_adds_meta_escape_prefix_in_encoding() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
            vec![b'a']
        );
        term.process(b"\x1b[?1036h");
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
            vec![0x1b, b'a']
        );
    }

    #[test]
    fn decrqm_reports_keyboard_mode_states() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?1035$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1035;1$y");
        term.process(b"\x1b[?1036$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1036;2$y");
        term.process(b"\x1b[?1039$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1039;1$y");
        term.process(b"\x1b[?1045$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1045;2$y");

        term.process(b"\x1b[?1035l");
        term.process(b"\x1b[?1036h");
        term.process(b"\x1b[?1039l");
        term.process(b"\x1b[?1045h");
        term.process(b"\x1b[?1035$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1035;2$y");
        term.process(b"\x1b[?1036$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1036;1$y");
        term.process(b"\x1b[?1039$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1039;2$y");
        term.process(b"\x1b[?1045$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1045;1$y");
    }
}

#[cfg(test)]
mod win32_input_mode_tests {
    use crate::terminal::Terminal;
    use aterm_types::keyboard::KeyboardMode;

    fn conpty_term() -> Terminal {
        let mut term = Terminal::new(24, 80);
        term.set_win32_input_enabled(true);
        term
    }

    #[test]
    fn ris_clears_mode_9001() {
        let mut term = conpty_term();
        term.process(b"\x1b[?9001h");
        term.process(b"\x1bc");
        assert!(!term.modes().win32_input_mode);
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    }

    #[test]
    fn mode_9001_survives_decstr() {
        let mut term = conpty_term();
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[!p");
        assert!(term.modes().win32_input_mode);
    }
}

impl super::Terminal {
    /// Force the primary screen: the host's recovery for an alternate screen a killed app left up.
    pub fn leave_alternate_screen(&mut self) -> bool {
        if !self.modes.alternate_screen {
            return false;
        }
        self.transient.host_leave_alternate_screen = true;
        self.process(b"");
        debug_assert!(
            !self.modes.alternate_screen,
            "the batch takes the request first thing, and the handler refuses only a \
             screen that is not alternate"
        );
        true
    }
}

#[cfg(test)]
mod orphaned_alt_screen_tests {
    use crate::terminal::Terminal;

    fn on_alt_with_history() -> Terminal {
        let mut term = Terminal::new(5, 40);
        for i in 0..8 {
            term.process(format!("main line {i}\r\n").as_bytes());
        }
        term.process(b"$ less file\r\n\x1b[?1049h\x1b[H\x1b[2Jpager row 1\r\npager row 2");
        assert!(term.modes().alternate_screen);
        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "the alt grid has no scrollback"
        );
        term
    }

    #[test]
    fn host_leave_restores_the_main_grid_and_its_scrollback() {
        let mut term = on_alt_with_history();
        assert!(term.leave_alternate_screen());
        assert!(!term.modes().alternate_screen);
        assert_eq!(term.grid().scrollback_lines(), 5);
        let screen: Vec<String> = (0..5).map(|r| term.row_text(r).unwrap()).collect();
        assert_eq!(screen[3].trim_end(), "$ less file");
        assert!(
            screen.iter().all(|r| !r.contains("pager")),
            "the pager rows stay on the alt grid, not the restored screen: {screen:?}"
        );
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (4, 0));
    }

    #[test]
    fn host_leave_on_the_main_screen_is_a_refused_no_op() {
        let mut term = Terminal::new(5, 40);
        term.process(b"hello\r\n");
        let before = term.row_text(0).unwrap();
        assert!(!term.leave_alternate_screen());
        assert!(!term.modes().alternate_screen);
        assert_eq!(term.row_text(0).unwrap(), before);
        assert_eq!(term.grid().scrollback_lines(), 0);
    }

    #[test]
    fn host_leave_is_one_shot_and_a_later_app_can_enter_again() {
        let mut term = on_alt_with_history();
        assert!(term.leave_alternate_screen());
        term.process(b"\x1b[?1049h\x1b[Hsecond app");
        assert!(term.modes().alternate_screen);
        assert_eq!(term.row_text(0).unwrap().trim_end(), "second app");
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
    }

    #[test]
    fn host_leave_of_a_mode_47_screen_keeps_the_cursor_where_the_app_had_it() {
        let mut term = Terminal::new(5, 40);
        term.process(b"one\r\n\x1b[?1049h\x1b[5;9Hpager\x1b[?1049l");
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            (1, 0),
            "the clean 1049 exit"
        );
        term.process(b"two\r\n\x1b[?47h\x1b[3;5Hdead app");
        assert!(term.modes().alternate_screen);
        assert!(term.leave_alternate_screen());
        assert!(!term.modes().alternate_screen);
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (2, 12));
        assert_eq!(term.row_text(0).unwrap().trim_end(), "one");
        assert_eq!(term.row_text(1).unwrap().trim_end(), "two");
        let archived = term.alt_archive().texts();
        assert_eq!(archived.last().map(String::as_str), Some("    dead app"));
        term.process(b"\x1b[?47h");
        assert!(
            (0..5).all(|r| !term.row_text(r).unwrap().contains("dead app")),
            "a later ?47h re-showed the dead screen"
        );
    }

    #[test]
    fn host_leave_of_a_1049_screen_entered_after_a_47_one_restores_its_save() {
        let mut term = Terminal::new(5, 40);
        term.process(b"one\r\n\x1b[?47hx\x1b[?47l\r\ntwo\r\n");
        term.process(b"\x1b[?1049h\x1b[4;7Hpager");
        assert!(term.leave_alternate_screen());
        let cursor = term.cursor();
        assert_eq!(
            (cursor.row, cursor.col),
            (3, 0),
            "the cursor `?1049h` saved"
        );
    }

    #[test]
    fn a_dead_apps_scroll_region_and_input_modes_do_not_outlive_it() {
        use crate::terminal::{MouseEncoding, MouseMode};
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?1004hhistory\r\n$ vim file\r\n");
        term.process(b"\x1b[?1049h\x1b[1;5r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=\x1b[?2026h");
        term.process(b"\x1b[Hbuffer row\r\n");
        term.process(b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(!term.modes().alternate_screen);
        let modes = term.modes();
        assert_eq!(modes.mouse_mode, MouseMode::None);
        assert_eq!(modes.mouse_encoding, MouseEncoding::X10);
        assert!(!modes.application_cursor_keys);
        assert!(!modes.application_keypad);
        assert!(!modes.synchronized_output);
        assert!(modes.focus_reporting);
        let region = term.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (0, 9));
        term.process(b"seq 20\r\n");
        for i in 0..20 {
            term.process(format!("out {i}\r\n").as_bytes());
        }
        let first = term.grid().oldest_absolute_row();
        let n = term.grid().scrollback_lines() + 10;
        let rows: Vec<String> = (first..)
            .take(n)
            .map(|abs| {
                term.abs_row_text(abs)
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect();
        for want in ["history", "$ vim file", "$ seq 20"]
            .map(str::to_string)
            .into_iter()
            .chain((0..20).map(|i| format!("out {i}")))
        {
            assert_eq!(
                rows.iter().filter(|r| **r == want).count(),
                1,
                "{want:?}: {rows:?}"
            );
        }
    }

    #[test]
    fn the_host_leave_releases_the_dead_apps_modes_too() {
        let mut term = on_alt_with_history();
        term.process(b"\x1b[2;4r\x1b[?1000h\x1b[?1h\x1b[?2048h\x1b[?2031h\x1b[?2004h");
        assert!(term.leave_alternate_screen());
        let region = term.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (0, 4));
        assert_eq!(term.modes().mouse_mode, crate::terminal::MouseMode::None);
        assert!(!term.modes().application_cursor_keys);
        assert!(!term.modes().in_band_size_reports);
        assert!(!term.modes().report_color_scheme);
        assert!(term.modes().bracketed_paste);
    }
}

#[cfg(test)]
mod conhost_left_alt_screen_tests {
    use crate::terminal::Terminal;

    const ROWS: u16 = 10;
    const PROMPT: &[u8] = b"\x1b]133;A\x07PS> \x1b]133;B\x07";

    fn all_rows(term: &Terminal) -> Vec<String> {
        let first = term.grid().oldest_absolute_row();
        let n = term.grid().scrollback_lines() + usize::from(ROWS);
        (first..)
            .take(n)
            .map(|abs| {
                term.abs_row_text(abs)
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn count(rows: &[String], want: &str) -> usize {
        rows.iter().filter(|r| *r == want).count()
    }

    fn blank_repaint_then_enter() -> Vec<u8> {
        let mut read = b"\x1b[?25l\x1b[H".to_vec();
        for _ in 1..ROWS {
            read.extend_from_slice(b"\x1b[K\r\n");
        }
        read.extend_from_slice(b"\x1b[K\x1b[?25h\x1b[?1049h");
        read
    }

    fn stale_primary_repaint() -> Vec<u8> {
        let mut read = b"\x1b[?25l\x1b[H".to_vec();
        for row in ["history 1", "history 2", "PS> less file"] {
            read.extend_from_slice(row.as_bytes());
            read.extend_from_slice(b"\x1b[K\r\n");
        }
        read.extend_from_slice(b"\x1b[K\x1b[4;1H\x1b[?25h");
        read
    }

    fn recovered_with_two_commands(conhost: bool) -> Terminal {
        let mut term = Terminal::new(ROWS, 40);
        if conhost {
            term.set_win32_input_enabled(true);
            term.process(b"\x1b[?9001h\x1b[?1004h");
        }
        term.process(b"history 1\r\nhistory 2\r\n");
        term.process(PROMPT);
        term.process(b"less file\r\n\x1b]133;C\x07");
        term.process(b"\x1b[?1049h");
        term.process(b"\x1b[H\x1b[2Jpager 1\r\npager 2\r\n:");
        term.process(b"\r");
        term.process(b"\n\x1b]133;D;-1\x07");
        term.process(PROMPT);
        assert!(
            !term.modes().alternate_screen,
            "the prompt recovered the tab"
        );
        for cmd in ["one", "two"] {
            term.process(
                format!("echo {cmd}\r\n\x1b]133;C\x07{cmd}\r\n\x1b]133;D;0\x07").as_bytes(),
            );
            term.process(PROMPT);
        }
        term.process(b"less file\r\n\x1b]133;C\x07");
        term
    }

    const TYPED_AFTER: [&str; 4] = ["PS> echo one", "one", "PS> echo two", "two"];

    #[test]
    fn a_second_pager_under_conhost_keeps_what_was_typed_after_the_recovery() {
        let mut term = recovered_with_two_commands(true);
        assert_eq!(
            term.grid().scrollback_lines(),
            3,
            "the recovery moved the history rows out of conhost's reach, nothing since"
        );
        term.process(&blank_repaint_then_enter());
        assert!(term.modes().alternate_screen);
        term.process(b"\x1b[Hsecond pager");
        term.process(b"\x1b[10;1H\x1b[?1049l");
        term.process(&stale_primary_repaint());
        term.process(b"\x1b]133;D;0\x07");
        term.process(PROMPT);
        let rows = all_rows(&term);
        for want in TYPED_AFTER {
            assert_eq!(
                count(&rows, want),
                1,
                "{want:?} must survive once: {rows:?}"
            );
        }
        assert_eq!(term.row_text(2).unwrap().trim_end(), "PS> less file");
        assert_eq!(term.row_text(3).unwrap().trim_end(), "PS>");
    }

    #[test]
    fn without_conhost_the_same_bytes_move_nothing() {
        let mut term = recovered_with_two_commands(false);
        term.process(&blank_repaint_then_enter());
        assert!(term.modes().alternate_screen);
        term.process(b"\x1b[?1049l");
        term.process(&stale_primary_repaint());
        assert_eq!(term.grid().scrollback_lines(), 0);
        let rows = all_rows(&term);
        for want in TYPED_AFTER {
            assert_eq!(count(&rows, want), 0, "{rows:?}");
        }
    }

    #[test]
    fn a_stray_1049l_under_conhost_keeps_the_rows_from_the_primary_repaint() {
        let mut term = recovered_with_two_commands(true);
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        term.process(&stale_primary_repaint());
        let rows = all_rows(&term);
        for want in TYPED_AFTER {
            assert_eq!(count(&rows, want), 1, "{rows:?}");
        }
        let kept = term.grid().scrollback_lines();
        term.process(b"\x1b[?1049l");
        assert_eq!(term.grid().scrollback_lines(), kept);
    }

    #[test]
    fn the_host_forced_leave_under_conhost_keeps_the_rows_too() {
        let mut term = Terminal::new(ROWS, 40);
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h");
        term.process(b"C:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J");
        for i in 1..ROWS {
            term.process(format!("pager {i}\r\n").as_bytes());
        }
        term.process(b":\r\nC:\\>echo stuck\r\nstuck\r\n\r\nC:\\>");
        assert!(term.leave_alternate_screen());
        assert_eq!((term.cursor().row, term.cursor().col), (ROWS - 1, 4));
        assert_eq!(all_rows(&term)[0], "C:\\>less file");
        assert_eq!(term.grid().scrollback_lines(), 1);
        let archived = term.alt_archive().texts();
        for want in ["C:\\>echo stuck", "stuck", "C:\\>"] {
            assert!(archived.iter().any(|r| r == want), "{want:?}: {archived:?}");
        }
        term.process(b"echo after\r\nafter\r\n\r\nC:\\>");
        term.process(&blank_repaint_then_enter());
        term.process(b"\x1b[?1049l");
        let rows = all_rows(&term);
        assert_eq!(count(&rows, "C:\\>less file"), 1, "{rows:?}");
        assert_eq!(count(&rows, "after"), 1, "{rows:?}");
        assert_eq!(count(&rows, "    echo after"), 1, "{rows:?}");
    }

    #[test]
    fn a_programs_9001_does_not_fake_a_console_host_for_the_recovery() {
        let mut term = Terminal::new(ROWS, 40);
        term.process(b"\x1b[?9001h");
        term.process(b"$ less file\r\n\x1b[?1049h\x1b[H\x1b[2J");
        for i in 1..ROWS {
            term.process(format!("pager {i}\r\n").as_bytes());
        }
        assert!(term.leave_alternate_screen());
        assert_eq!((term.cursor().row, term.cursor().col), (1, 0));
        assert_eq!(term.grid().scrollback_lines(), 0);
        assert_eq!(term.row_text(0).unwrap().trim_end(), "$ less file");
    }

    fn recovered_on_the_bottom_row(conhost: bool) -> Terminal {
        let mut term = Terminal::new(ROWS, 40);
        if conhost {
            term.set_win32_input_enabled(true);
            term.process(b"\x1b[?9001h\x1b[?1004h");
        }
        term.process(b"history 1\r\nhistory 2\r\n");
        term.process(PROMPT);
        term.process(b"less file\r\n\x1b]133;C\x07\x1b[?1049h\x1b[H\x1b[2J");
        for i in 1..ROWS {
            term.process(format!("pager {i}\r\n").as_bytes());
        }
        term.process(b":");
        term.process(b"\r\n\x1b]133;D;-1\x07");
        term.process(PROMPT);
        assert!(!term.modes().alternate_screen);
        term
    }

    const HISTORY: [&str; 3] = ["history 1", "history 2", "PS> less file"];

    #[test]
    fn under_conhost_the_prompt_lands_on_conhosts_row_and_absolute_moves_agree() {
        let mut term = recovered_on_the_bottom_row(true);
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (ROWS - 1, 4), "after `PS> `");
        assert_eq!(term.grid().scrollback_lines(), 3);
        assert_eq!(all_rows(&term)[..3], HISTORY);
        let head = format!("echo {}", "x".repeat(31));
        term.process(head.as_bytes());
        term.process(format!("\r\n\x1b[{};40Hx", ROWS - 1).as_bytes());
        term.process(b"TAIL");
        assert_eq!(
            term.row_text(usize::from(ROWS - 2)).unwrap().trim_end(),
            format!("PS> {head}")
        );
        assert_eq!(
            term.row_text(usize::from(ROWS - 1)).unwrap().trim_end(),
            "TAIL",
            "the tail is on the row right after its head"
        );
    }

    #[test]
    fn a_conhost_repaint_after_the_recovery_cannot_reach_the_history() {
        let mut term = recovered_on_the_bottom_row(true);
        term.process(b"echo one\r\n\x1b]133;C\x07one\r\n\x1b]133;D;0\x07");
        term.process(PROMPT);
        let mut repaint = b"\x1b[?25l\x1b[H".to_vec();
        let conhost_buffer = (4..ROWS)
            .map(|i| format!("pager {i}"))
            .chain([":", "PS> echo one", "one", "PS> "].map(str::to_string));
        for (i, row) in conhost_buffer.enumerate() {
            if i > 0 {
                repaint.extend_from_slice(b"\r\n");
            }
            repaint.extend_from_slice(row.as_bytes());
            repaint.extend_from_slice(b"\x1b[K");
        }
        repaint.extend_from_slice(format!("\x1b[{ROWS};5H\x1b[?25h").as_bytes());
        term.process(&repaint);
        let rows = all_rows(&term);
        for want in HISTORY.into_iter().chain(["PS> echo one", "one"]) {
            assert_eq!(count(&rows, want), 1, "{want:?}: {rows:?}");
        }
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (ROWS - 1, 4));
        assert_eq!(
            term.row_text(usize::from(ROWS - 2)).unwrap().trim_end(),
            "one"
        );
    }

    #[test]
    fn without_conhost_the_prompt_returns_to_the_row_1049_saved() {
        let term = recovered_on_the_bottom_row(false);
        assert_eq!(term.grid().scrollback_lines(), 0);
        for (row, want) in HISTORY.into_iter().enumerate() {
            assert_eq!(term.row_text(row).unwrap().trim_end(), want);
        }
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (3, 4));
    }

    #[test]
    fn a_live_enter_or_a_reset_ends_the_state() {
        for end in [&b"\x1b[?1049h\x1b[?1049l"[..], b"\x1bc"] {
            let mut term = recovered_with_two_commands(true);
            term.process(end);
            let kept = term.grid().scrollback_lines();
            term.process(&blank_repaint_then_enter());
            term.process(b"\x1b[?1049l");
            assert_eq!(term.grid().scrollback_lines(), kept, "{end:?}");
        }
    }

    #[test]
    fn a_partial_scroll_region_is_left_alone() {
        let mut term = recovered_with_two_commands(true);
        term.process(b"\x1b[1;5r");
        let before = all_rows(&term);
        term.process(b"\x1b[?1049l");
        assert_eq!(all_rows(&term), before);
        term.process(b"\x1b[r\x1b[?1049l");
        assert_eq!(all_rows(&term), before, "spent by the first switch");
    }
}

/// `?1049l` on the primary screen moves nothing (see `exit_alternate_screen`:
/// xterm restores the DECSC slot there, Alacritty and WezTerm ignore it, and
/// aterm's own orphaned-screen recovery makes the restore dangerous).
#[cfg(test)]
mod stray_1049_reset_tests {
    use crate::grid::CellFlags;
    use crate::terminal::Terminal;

    fn cursor(term: &Terminal) -> (u16, u16) {
        let c = term.cursor();
        (c.row, c.col)
    }

    #[test]
    fn a_stray_1049l_leaves_the_cursor_and_the_pen_put() {
        let mut term = Terminal::new(4, 20);
        term.process(b"\x1b[2;6H\x1b7\x1b[4;12H\x1b[1m\x1b[?1049l");
        assert!(!term.is_alternate_screen());
        assert_eq!(cursor(&term), (3, 11), "not back to the DECSC position");
        assert!(
            term.style().flags.contains(CellFlags::BOLD),
            "the pen stays"
        );
        let mut term = Terminal::new(4, 20);
        term.process(b"\t\x1b[1;31m\x1b[?1049l");
        assert_eq!(cursor(&term), (0, 8), "nor home with nothing saved");
        assert!(term.style().flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn a_1049l_from_the_alternate_screen_restores_the_slot_as_decrc_does() {
        type Between = fn(&mut Terminal);
        let cases: [(&str, &[u8], Between); 5] = [
            ("pen", b"\x1b[1;58;5;3m", |t| {
                t.process(b"\x1b[0;32;58;5;4m")
            }),
            ("G0", b"", |t| t.process(b"\x1b(0")),
            ("origin mode and clamp", b"\x1b[?6h", |t| {
                t.process(b"\x1b[2;3r\x1b[?6l");
            }),
            (
                "origin clamp to DECLRMM margins",
                b"\x1b[?6h\x1b[?69h",
                |t| {
                    t.process(b"\x1b[3;4s");
                },
            ),
            ("wrap after a widen", b"ABCDE", |t| t.resize(3, 8)),
        ];
        for (label, before, between) in cases {
            let run = |save: &[u8], restore: &[u8]| {
                let mut term = Terminal::new(3, 5);
                term.process(before);
                term.process(save);
                between(&mut term);
                term.process(restore);
                term.process(b"q");
                let style = term.style();
                let rows: Vec<String> = (0..term.grid().rows())
                    .map(|r| term.grid().row_text(r).unwrap_or_default())
                    .collect();
                let grid = term.grid();
                let underline: Vec<(u16, u16, u32)> = (0..grid.rows())
                    .flat_map(|r| (0..grid.cols()).map(move |c| (r, c)))
                    .filter_map(|(r, c)| {
                        let color = grid.cell_extra(r, c)?.underline_color_u32()?;
                        Some((r, c, color))
                    })
                    .collect();
                (
                    cursor(&term),
                    term.modes().origin_mode,
                    (style.fg, style.bg, style.flags),
                    rows,
                    underline,
                )
            };
            assert_eq!(
                run(b"\x1b[?1049h", b"\x1b[?1049l"),
                run(b"\x1b7", b"\x1b8"),
                "{label}"
            );
        }
    }

    #[test]
    fn a_rmcup_after_the_orphaned_screen_recovery_keeps_the_cursor() {
        let mut term = Terminal::new(8, 30);
        term.process(b"out1\r\nout2\r\n$ vim\r\n\x1b[?1049h\x1b[H\x1b[2Jvim screen");
        term.process(b"\x1b]133;A\x07$ ");
        assert!(
            !term.is_alternate_screen(),
            "the prompt mark left the dead screen"
        );
        assert_eq!(
            cursor(&term),
            (3, 2),
            "the slot vim's ?1049h saved, plus the prompt"
        );
        term.process(b"ls\r\nfileA fileB\r\n\x1b]133;A\x07$ echo hi\r\nhi\r\n\x1b]133;A\x07$ ");
        term.process(b"tput rmcup\r\n\x1b[?1049l\x1b[23;0;0t\x1b]133;A\x07$ ");
        assert_eq!(cursor(&term), (7, 2), "the prompt lands below the output");
        let row = |r: u16| term.grid().row_text(r).unwrap_or_default();
        assert_eq!(
            row(3).trim_end(),
            "fileA fileB",
            "nothing overwrote the output"
        );
        assert_eq!(row(7).trim_end(), "$");
    }

    #[test]
    fn a_47_screen_left_with_1049l_restores_like_xterm() {
        let mut term = Terminal::new(4, 20);
        term.process(b"\x1b[3;9H\x1b[?47h\x1b[2;2Hx\x1b[?1049l");
        assert!(!term.is_alternate_screen());
        assert_eq!(cursor(&term), (0, 0));
    }

    #[test]
    fn a_restored_session_leaves_a_dead_1047_screen_where_the_live_one_would() {
        let mut live = Terminal::new(8, 30);
        live.process(
            b"out1\r\nout2\r\nout3\r\n$ app\r\n\x1b[?1047h\x1b[H\x1b[2Jfull screen\x1b[5;3H",
        );
        let mut restored = Terminal::from_checkpoint(&live.checkpoint());
        for term in [&mut live, &mut restored] {
            assert!(term.is_alternate_screen());
            term.process(b"\x1b]133;A\x07$ ");
            assert!(
                !term.is_alternate_screen(),
                "the prompt mark left the dead screen"
            );
        }
        assert_eq!(cursor(&restored), cursor(&live));
        let row = |t: &Terminal, r: u16| t.grid().row_text(r).unwrap_or_default();
        assert_eq!(
            row(&restored, 0).trim_end(),
            "out1",
            "nothing overwrote the output"
        );
        for r in 0..8 {
            assert_eq!(row(&restored, r), row(&live, r), "row {r}");
        }
    }
}

/// The BCE template across `?47`/`?1047` switches: the pen stays, so the
/// buffer swapped in must erase and scroll with the current background
/// (xterm's `ClearCells`/`ScrnClearLines` read the one colour pair).
#[cfg(test)]
mod alt_switch_bce_tests {
    use crate::terminal::Terminal;

    fn bg_index(term: &Terminal, row: u16, col: u16) -> Option<u8> {
        let colors = term.grid().cell(row, col)?.colors();
        colors.bg_is_indexed().then(|| colors.bg_index())
    }

    #[test]
    fn a_line_scrolled_in_on_the_switched_in_buffer_takes_the_current_background() {
        for enter in [&b"\x1b[?47h"[..], b"\x1b[?1047h"] {
            let mut term = Terminal::new(3, 10);
            term.process(b"\x1b[44m");
            term.process(enter);
            term.process(b"\x1b[3;1H\n");
            assert_eq!(bg_index(&term, 2, 0), Some(4), "entering with {enter:?}");
        }
        let mut term = Terminal::new(3, 10);
        term.process(b"\x1b[?47h\x1b[42m\x1b[?47l\x1b[3;1H\n");
        assert_eq!(bg_index(&term, 2, 0), Some(2), "leaving ?47");

        let mut live = Terminal::new(3, 10);
        live.process(b"\x1b[44m\x1b[?1049h\x1b[0m");
        let mut restored = Terminal::from_checkpoint(&live.checkpoint());
        for term in [&mut live, &mut restored] {
            term.process(b"\x1b[?1049l\x1b[3;1H\n");
        }
        assert_eq!(bg_index(&live, 2, 0), Some(4), "leaving ?1049");
        assert_eq!(
            bg_index(&restored, 2, 0),
            Some(4),
            "leaving ?1049, restored"
        );
    }

    #[test]
    fn an_open_link_ends_at_every_screen_switch() {
        for (on, switch) in [
            ("", "?1049h"),
            ("?1049h", "?1049l"),
            ("", "?1047h"),
            ("?1047h", "?1047l"),
        ] {
            let mut term = Terminal::new(3, 10);
            if !on.is_empty() {
                term.process(format!("\x1b[{on}").as_bytes());
            }
            term.process(b"\x1b]8;;https://example.com/\x07");
            term.process(format!("\x1b[{switch}").as_bytes());
            let at = term.cursor();
            term.process(b"x");
            assert_eq!(term.hyperlink_at(at.row, at.col), None, "{switch}");
        }
    }
}

#[cfg(test)]
#[path = "conpty_dead_alt_screen_replay_tests.rs"]
mod conpty_dead_alt_screen_replay_tests;

#[cfg(test)]
mod in_band_size_tests {
    use crate::terminal::Terminal;

    #[test]
    fn mode_2048_reports_no_pixels_before_a_host_measures() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;0;0t"
        );
    }

    #[test]
    fn mode_2048_reports_a_new_cell_box_at_the_same_size() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        let _ = term.take_response();
        term.set_cell_pixel_size(9, 19);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;456;720t"
        );
        term.set_cell_pixel_size(18, 38);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;912;1440t"
        );
        term.set_cell_pixel_size(18, 38);
        assert!(
            term.take_response().is_none(),
            "an unchanged box (a host's next layout pass) reports nothing"
        );
    }

    #[test]
    fn mode_2048_repeated_enable_emits_once() {
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        for _ in 0..10_000 {
            input.extend_from_slice(b"\x1b[?2048h");
        }
        term.process(&input);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;0;0t",
            "10k repeated enables emit exactly one report"
        );
    }

    #[test]
    fn mode_2048_toggle_flood_is_capped() {
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        for _ in 0..200_000 {
            input.extend_from_slice(b"\x1b[?2048l\x1b[?2048h");
        }
        term.process(&input);
        let resp = term.take_response().unwrap_or_default();
        assert!(
            resp.len() <= crate::terminal::MAX_RESPONSE_BUFFER_SIZE,
            "toggle flood must stay within MAX_RESPONSE_BUFFER_SIZE, got {}",
            resp.len()
        );
    }

    #[test]
    fn mode_2048_emits_on_resize_and_honors_cell_px() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(b"\x1b[?2048h");
        let _ = term.take_response();
        term.resize(30, 100);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;600;1000t"
        );
    }

    #[test]
    fn a_resize_report_is_taken_apart_from_the_replies() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        let _ = term.take_response();
        term.resize(30, 100);
        term.process(b"\x1b[6n");
        assert_eq!(
            term.take_reports().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[1;1R");
        term.resize(24, 80);
        term.process(b"\x1b[6n");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[1;1R\x1b[48;24;80;0;0t"
        );
        assert!(term.take_reports().is_none(), "taken with the replies");
    }

    #[test]
    fn no_report_when_2048_disabled() {
        let mut term = Terminal::new(24, 80);
        term.resize(30, 100);
        assert!(
            term.take_response().is_none(),
            "no in-band report unless mode 2048 is set"
        );
    }

    fn console_host_tab() -> Terminal {
        let mut term = Terminal::new(30, 100);
        term.set_console_host_posture(crate::grid::ResizePolicy::ConPty);
        term
    }

    #[test]
    fn behind_a_console_host_the_enable_report_waits_for_the_alternate_screen() {
        for (enter, leave) in [
            (&b"\x1b[?1049h"[..], &b"\x1b[?1049l"[..]),
            (b"\x1b[?1047h", b"\x1b[?1047l"),
            (b"\x1b[?47h", b"\x1b[?47l"),
        ] {
            let mut term = console_host_tab();
            term.process(b"\x1b[?2048h");
            assert!(term.take_response().is_none(), "{enter:?}: held");
            assert!(term.modes().in_band_size_reports, "the mode is on");
            term.resize(30, 90);
            let _ = term.take_response();
            term.process(enter);
            assert_eq!(
                term.take_response().unwrap_or_default(),
                b"\x1b[48;30;90;0;0t",
                "{enter:?}"
            );
            term.process(leave);
            term.process(enter);
            assert!(term.take_response().is_none(), "{enter:?}: sent once");
        }
    }

    #[test]
    fn behind_a_console_host_an_enable_on_the_alternate_screen_reports_at_once() {
        let mut term = console_host_tab();
        term.process(b"\x1b[?1049h\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
    }

    #[test]
    fn a_held_report_is_dropped_when_its_program_is_done() {
        for end in [
            &b"\x1b[?2048l"[..],
            b"\x1b]133;A\x07PS> ",
            b"\x1b]633;A\x07PS> ",
            b"\x1bc",
        ] {
            let mut term = console_host_tab();
            term.process(b"\x1b[?2048h");
            term.process(end);
            term.process(b"\x1b[?1049h");
            assert!(term.take_response().is_none(), "{end:?}");
        }
        let mut term = console_host_tab();
        term.process(b"\x1b[?2048h\x1b]133;A\x07PS> \x1b[?1049h\x1b[?2048l\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
    }

    #[test]
    fn a_native_session_reports_the_enable_at_once() {
        let mut term = Terminal::new(30, 100);
        term.set_console_host_posture(crate::grid::ResizePolicy::Native);
        term.process(b"\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
    }
}

#[cfg(test)]
mod mode_and_query_agreement_tests {
    use crate::terminal::Terminal;
    use aterm_types::CursorStyle;

    fn decrqm(term: &mut Terminal, mode: u16) -> Vec<u8> {
        term.process(format!("\x1b[?{mode}$p").as_bytes());
        term.take_response().unwrap_or_default()
    }

    #[test]
    fn mode_12_moves_the_cursor_the_user_sees_and_reports_it() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");

        term.process(b"\x1b[?12l\x1b[?25h");
        assert_eq!(
            term.cursor_style(),
            CursorStyle::SteadyBlock,
            "?12l must steady the cursor, not just set a flag"
        );
        assert!(term.cursor_visible());
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

        term.process(b"\x1b[6 q");
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");
        term.process(b"\x1b[?12;25h");
        assert_eq!(
            term.cursor_style(),
            CursorStyle::BlinkingBar,
            "?12h must blink the CURRENT shape"
        );
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");
    }

    #[test]
    fn decscusr_moves_mode_12_with_the_style() {
        for (param, style, want_set) in [
            (1u16, CursorStyle::BlinkingBlock, true),
            (2, CursorStyle::SteadyBlock, false),
            (3, CursorStyle::BlinkingUnderline, true),
            (4, CursorStyle::SteadyUnderline, false),
            (5, CursorStyle::BlinkingBar, true),
            (6, CursorStyle::SteadyBar, false),
        ] {
            let mut term = Terminal::new(24, 80);
            term.process(format!("\x1b[{param} q").as_bytes());
            assert_eq!(term.cursor_style(), style);
            let want = if want_set {
                b"\x1b[?12;1$y".to_vec()
            } else {
                b"\x1b[?12;2$y".to_vec()
            };
            assert_eq!(decrqm(&mut term, 12), want, "DECSCUSR {param}");
            term.process(b"\x1bP$q?12\x1b\\");
            let decrqss = term.take_response().unwrap_or_default();
            let expect_decrqss: &[u8] = if want_set {
                b"\x1bP1$r?12h\x1b\\"
            } else {
                b"\x1bP1$r?12l\x1b\\"
            };
            assert_eq!(decrqss, expect_decrqss, "DECRQSS after DECSCUSR {param}");
        }
    }

    #[test]
    fn mode_12_under_conhost_keeps_the_configured_cursor() {
        const VIM_START: &[u8] = b"\x1b[?1049h\x1b[?12h\x1b[?12l";
        const VIM_EXIT: &[u8] = b"\x1b[?25h\x1b[?1049l";

        let mut term = Terminal::new(24, 80);
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h");
        assert!(
            term.modes().win32_input_mode,
            "the console-host mode is active"
        );
        term.process(VIM_START);
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");
        term.process(VIM_EXIT);
        assert_eq!(
            term.cursor_style(),
            CursorStyle::BlinkingBlock,
            "the prompt cursor still blinks after the alternate screen closes"
        );

        term.process(b"\x1b[?25l");
        assert!(!term.cursor_visible());
        term.process(b"\x1b[?12;25h");
        assert!(term.cursor_visible());
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);

        term.process(b"\x1b[6 q");
        term.process(b"\x1b[?12h");
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

        let mut term = Terminal::new(24, 80);
        term.set_default_cursor_style(CursorStyle::SteadyUnderline);
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h\x1b[?12h");
        assert!(term.modes().win32_input_mode);
        assert_eq!(term.cursor_style(), CursorStyle::SteadyUnderline);
    }

    #[test]
    fn mode_12_still_applies_when_unix_ignores_mode_9001() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001h");
        assert!(!term.modes().win32_input_mode);
        term.process(b"\x1b[?1049h\x1b[?12h\x1b[?12l");
        term.process(b"\x1b[?25h\x1b[?1049l");
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBlock);
        assert!(term.cursor_visible());
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

        let mut term = Terminal::new(24, 80);
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h");
        assert!(term.modes().win32_input_active());
        term.set_win32_input_enabled(false);
        assert!(term.modes().win32_input_mode, "the negotiated bit is stale");
        assert!(!term.modes().win32_input_active());
        term.process(b"\x1b[?12l");
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBlock);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");
    }

    #[test]
    fn xtsave_covers_every_mode_decrqm_reports() {
        const REPORTED: &[u16] = &[
            1, 3, 5, 6, 7, 9, 12, 25, 40, 45, 66, 67, 69, 80, 95, 1000, 1002, 1003, 1004, 1005,
            1006, 1007, 1015, 1016, 1035, 1036, 1039, 1045, 1243, 2004, 2026, 2031, 2048, 2500,
            2501, 9001,
        ];
        let mut lost = Vec::new();
        for &mode in REPORTED {
            let mut term = Terminal::new(24, 80);
            if mode == 3 {
                term.process(b"\x1b[?40h");
            }
            if mode == 9001 {
                term.set_win32_input_enabled(true);
            }
            term.process(format!("\x1b[?{mode}h").as_bytes());
            let _ = term.take_response();
            let armed = decrqm(&mut term, mode);
            assert_eq!(
                armed,
                format!("\x1b[?{mode};1$y").into_bytes(),
                "mode {mode} did not arm"
            );
            term.process(format!("\x1b[?{mode}s").as_bytes());
            term.process(format!("\x1b[?{mode}l").as_bytes());
            let _ = term.take_response();
            term.process(format!("\x1b[?{mode}r").as_bytes());
            let _ = term.take_response();
            if decrqm(&mut term, mode) != format!("\x1b[?{mode};1$y").into_bytes() {
                lost.push(mode);
            }
        }
        assert!(
            lost.is_empty(),
            "XTSAVE/XTRESTORE silently dropped modes DECRQM reports: {lost:?}"
        );
    }
}

include!("handler_dec_refinement.rs");
