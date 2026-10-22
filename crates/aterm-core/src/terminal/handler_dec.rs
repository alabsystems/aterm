// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DEC private mode handler: DECSET/DECRST (`CSI ? Ps h/l`), DECRQM
//! (`CSI ? Ps $ p`), ANSI modes (`CSI Ps h/l`), the alternate screen and the
//! VT420 rectangular operations.

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
                    // DECANM — CSI ? 2 l enters VT52, CSI ? 2 h exits.
                    self.set_vt52_mode(!set);
                }
                3 => {
                    // DECCOLM: flag only, the host resizes if desired; honored only while mode
                    // 40 is set. An arm-local `if`, not a match guard, so an unhonored one
                    // does not fall to the unknown-mode arm.
                    #[allow(clippy::collapsible_match)]
                    if self.modes.deccolm_enable {
                        self.set_column_mode_132(set);
                    }
                }
                5 => {
                    // DECSCNM — reverse video, forces full redraw.
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
                12 => {
                    // Keep the configured/DECSCUSR style while the current
                    // host uses win32 input mode; a stale negotiated bit is not enough.
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
                    // DECNKM: set is the application keypad (ESC =), reset numeric (ESC >).
                    self.set_application_keypad(set);
                }
                69 => {
                    self.set_left_right_margin_mode(set);
                }
                40 => {
                    // Mode 40: allow DECCOLM (80/132 columns), per xterm.
                    self.modes.deccolm_enable = set;
                }
                80 => {
                    // DECSDM: set, Sixel output scrolls at the bottom margin; reset, it clips.
                    self.set_sixel_display_mode(set);
                }
                95 => {
                    // DECNCSM: a DECCOLM switch does not clear screen, margins or cursor.
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
                            // The host's forwarded switch out of a dead app's screen
                            // is that app's exit: what it left on goes with it.
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
                    // Between the shell's `D` and its next `A` the switch is the shell's own
                    // set-up for the prompt it is drawing (`release_dead_app_paste`).
                    self.transient.bracketed_paste_from_prompt = self.shell.phase
                        == super::grouped_state::ShellIntegrationPhase::CommandFinished;
                }
                // Mouse tracking modes are mutually exclusive; DECRST resets to none.
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
                // Mouse encoding modes (reset to X10 on DECRST)
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
                // DEC 2031: report each scheme change (a theme install, or a background a
                // program set; Terminal::color_scheme) as CSI ? 997 ; Ps n. DSR CSI ? 996 n
                // answers regardless.
                2031 => {
                    self.modes.report_color_scheme = set;
                }
                // DEC 2048: in-band size reports. Enabling emits the current geometry at once;
                // Terminal::resize and a new host cell box (set_cell_pixel_size) emit on each
                // change.
                2048 => {
                    let was = self.modes.in_band_size_reports;
                    self.modes.in_band_size_reports = set;
                    if !set {
                        self.transient.size_report_held = false;
                    }
                    // Emit only on a disabled->enabled transition (as xterm does); the
                    // emitter holds the buffer cap a `?2048l?2048h` flood would test.
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
                // BiDi modes (Terminal WG):
                // https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html
                1243 => {
                    self.set_bidi_arrow_swap(set);
                }
                2500 => {
                    self.set_bidi_box_mirroring(set);
                }
                2501 => {
                    self.set_bidi_autodetection(set);
                }
                // DECBKM (67): backspace sends BS when set, DEL when reset (default). It and
                // the three xterm modes below feed the legacy keyboard encoding.
                67 => {
                    self.modes.backarrow_sends_bs = set;
                }
                // xterm `numLock` (1035): reset, NumLock is no longer a real modifier.
                1035 => {
                    self.modes.special_modifiers = set;
                }
                // xterm `metaSendsEscape` (1036): Meta-modified keys are ESC-prefixed.
                1036 => {
                    self.modes.meta_send_escape = set;
                }
                // xterm `altSendsEscape` (1039): Alt-modified keys are ESC-prefixed while set
                // (the default).
                1039 => {
                    self.modes.alt_send_escape = set;
                }
                // Mode 1045: tracked for DECRQM only; it has no key-encoding effect.
                1045 => {
                    self.modes.mode_1045 = set;
                }
                // Mode 1048: xterm save/restore cursor (DECSC/DECRC).
                1048 => {
                    if set {
                        self.cursor_state().save_cursor_state();
                    } else {
                        self.cursor_state().restore_cursor_state();
                    }
                }
                // ConPTY win32-input-mode (microsoft/terminal spec 4999), which conhost
                // requests at every ConPTY start; the encoder picks which keys need a win32
                // record (Enter chords) and which stay legacy VT. Only a session behind a
                // console host (`win32_input_enabled`, the host's to set) negotiates it;
                // elsewhere the request falls to the unknown-mode arm.
                9001 if self.modes.win32_input_enabled => {
                    self.modes.win32_input_mode = set;
                }
                _ => {} // Unknown DEC mode
            }
            // Handback attribution: a setter parsed now is this holder's, even over a mode
            // already in force.
            *self.evidence_asserted |= super::program_evidence::asserted_by_dec_mode(param, set);
        }
    }

    /// Record the main grid's absolute row counter as this batch parks it (smcup), before the
    /// swap, for the epilogue's re-pin: output earlier in the batch already advanced it, and
    /// the re-pin must include that advance.
    fn park_main_row_counter(&mut self) {
        self.transient.alt_park_main_row_counter = Some(self.grid.absolute_row_counter());
    }

    /// Take the parked reading position off a grid swapped back in mid-batch and hand it to
    /// the epilogue's re-pin. Writes go through `row_index`, which subtracts `display_offset`,
    /// so the rest of the batch must run at offset 0 (the batch prologue's rule).
    fn flatten_restored_display_offset(&mut self) {
        // Every exit path funnels here: `post_process` needs to know when a batch exits and
        // re-enters, which its start/end comparison cannot see.
        self.transient.alt_screen_left_in_batch = true;
        let restored = self.grid.display_offset();
        if restored > 0 {
            // As in the prologue: this forced 0 is undone by the epilogue's re-pin, so it must
            // not register as a reader gesture.
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

        // No cursor saved: an orphaned-screen leave must not restore one either.
        self.transient.alt_entered_without_cursor_save = true;
        // A live app owns the alternate screen now, on both sides of conhost.
        self.transient.conhost_alt_screen_left_up = false;
        self.kitty_keyboard.switch_screen(true);
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        self.sixel.pending_image = None;
        // xterm has one persistent alternate buffer: 47/1047 never clear it on entry (only
        // 1049 does), so reuse the one the last exit kept. It has no scrollback.
        let mut new_grid = self
            .alt_grid
            .take()
            .unwrap_or_else(|| Grid::with_scrollback(self.grid.rows(), self.grid.cols(), 0));
        // A flap the parked alt buffer took while resized with the primary is not the screen
        // the app is about to see: its resize undo must not hand rows back into it.
        new_grid.drop_resize_undo();
        new_grid.set_resize_law(self.grid.resize_law());
        // Tab stops are global in xterm, shared by both screens.
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
        // xterm shares the cursor between buffers: 47/1047 swap without moving it.
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        new_grid.set_pending_wrap(self.grid.pending_wrap());
        Self::copy_margins(self.grid, &mut new_grid);
        // The BCE template lives on each grid but the pen is the terminal's: the
        // buffer swapped in erases and scrolls with the CURRENT background, not
        // the one current when it was last up (or a fresh buffer's default).
        Self::sync_bce_template(self.style, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        self.note_screen_replaced();
        // A swap moves no row of either grid: the main grid's anchors stay.
        self.grid.invalidate_host_coordinates_for_swap();
        self.send_held_size_report();
    }

    /// Point `grid`'s BCE template at the current pen's background. A buffer
    /// switch that keeps the pen (`?47`/`?1047`, either way) must do this for
    /// the grid it swaps in: xterm fills every blank from the one current
    /// colour pair (screen.c `ClearCells`, `ScrnClearLines`), whichever buffer
    /// is up. Found by the astream-term differential oracle
    /// (aterm-conformance `tests/vt_oracle`): a line scrolled in after
    /// `CSI 44 m CSI ? 47 h` came up in the alternate buffer's stale default.
    fn sync_bce_template(style: &mut CurrentStyle, grid: &mut Grid) {
        style.update_cached_colors();
        grid.set_cursor_template(
            crate::grid::Cell::bce_blank(style.cached_colors()),
            style.bce_bg_rgb(),
        );
    }

    /// Count a whole-screen replacement for the resize journal's readers
    /// (`Terminal::screen_replaced_count`): the screen the app painted before it
    /// is gone, so a displacement a resize left on it is gone too.
    fn note_screen_replaced(&mut self) {
        self.transient.screen_replaced = self.transient.screen_replaced.wrapping_add(1);
        self.transient.render_debt = crate::terminal::RenderDebt::default();
    }

    /// Copy the scroll region and horizontal margins from one grid to another. In xterm they
    /// are fields of the shared `TScreen`, so switching buffers (47/1047/1049) keeps them.
    fn copy_margins(from: &Grid, to: &mut Grid) {
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
        // Tab stops set on the alt screen carry back (they are global).
        let tab_stops = self.grid.tab_stops().to_vec();
        let tab_suppressed = self.grid.tab_defaults_suppressed();
        if let Some(mut main_grid) = self.alt_grid.take() {
            // The cursor is shared: it stays where the alt screen left it.
            let cursor = self.grid.cursor();
            main_grid.set_cursor(cursor.row, cursor.col);
            main_grid.set_pending_wrap(self.grid.pending_wrap());
            Self::copy_margins(self.grid, &mut main_grid);
            // The pen carries back unchanged, so does its background.
            Self::sync_bce_template(self.style, &mut main_grid);
            // Keep the alternate buffer: xterm's is persistent, and a re-entry shows it again.
            let mut alt = std::mem::replace(self.grid, main_grid);
            alt.drop_resize_undo();
            *self.alt_grid = Some(alt);
            // A whole-screen change (see `exit_alternate_screen`).
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

        // Clear the alt screen with the SGR background (BCE) before switching back.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.erase_screen();
        // Otherwise identical to mode 47.
        self.exit_alternate_screen_raw();
    }

    /// Enter the alternate screen for mode 1049: save the cursor, switch, clear the alt screen.
    /// [`Self::exit_alternate_screen`] undoes it losslessly.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetAlternateScreen",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    // The `alt_screen` refinement is checked by `tests/conformance_altscreen.rs`, which
    // drives `process` and projects each state onto the spec's variables.
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
            // A second 1049 set is not a no-op: xterm runs `CursorSave; ToAlternate;
            // ClearScreen` (charproc.c `srm_OPT_ALTBUF_CURSOR`) and only `ToAlternate` stands
            // down. The save goes into the alt slot, leaving the main slot 1049 reset restores,
            // and the clear runs so a restarting app does not inherit the previous screen.
            self.cursor_save.alt = Some(self.snapshot_cursor_state());
            // `ClearScreen`: an ordinary BCE clear that ends in `ResetWrap` and moves no cursor.
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
            self.grid.erase_screen();
            self.note_screen_replaced();
            // The clear is the current drawer's: everything on the alt
            // screen is its from here on.
            self.holder_fence.note_alt_cleared();
            return;
        }

        // xterm's 1049 save uses DECSC's per-buffer slot, so a later bare DECRC on the main
        // screen restores the position 1049 saved.
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
        // xterm's 1049-enter ClearScreen fills with the current SGR background (BCE).
        new_grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        new_grid.erase_screen();
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
        // 1049 set moves no cursor (it is shared), and its ClearScreen resets the pending
        // wrap; the main screen's wrap is in the snapshot above, restored by 1049 reset.
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
        // 1049 clears the screen it enters (47/1047 do not).
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
            // A stray `?1049l` moves nothing. xterm's 1049 reset is
            // `FromAlternate; CursorRestore` (charproc.c `srm_OPT_ALTBUF_CURSOR`)
            // and only the first half needs the alternate screen up, so xterm
            // (and Ghostty) restore the main DECSC slot, or home the cursor when
            // none was saved; Alacritty and WezTerm ignore it, and so does aterm,
            // deliberately. aterm leaves an orphaned alternate screen itself
            // (`leave_orphaned_alternate_screen`, on the shell's next prompt
            // mark after a full-screen app died) and restores the slot then
            // without consuming it, as DECRC must keep it; the user's own
            // `tput rmcup` afterwards, or a script's unconditional rmcup in an
            // exit trap, would jump the cursor back to the dead app's launch row
            // (or home) and the next prompt would overwrite live output. The
            // altscreen model agrees: its `Leave` is guarded by `active = alt`,
            // and this arm is a stutter. The oracle records the difference
            // (aterm-conformance `tests/vt_oracle`, `stray_1049l_*`).
            return;
        }

        // xterm's 1049 reset (FromAlternate + CursorRestore) saves the alt cursor nowhere.
        self.kitty_keyboard.switch_screen(false);
        self.sixel.pending_image = None;
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        // Tab stops set on the alt screen carry back (they are global).
        let tab_stops = self.grid.tab_stops().to_vec();
        let tab_suppressed = self.grid.tab_defaults_suppressed();
        if let Some(mut main_grid) = self.alt_grid.take() {
            Self::copy_margins(self.grid, &mut main_grid);
            *self.grid = main_grid;
            // A whole-screen change. The restored grid's damage was consumed before the app
            // entered the alt screen, so without this mark `damage_epoch()` does not advance
            // and epoch-gated hosts keep showing the alt screen.
            self.grid.damage_mut().mark_full();
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops, tab_suppressed);
        // Restore from the DECSC slot without consuming it (xterm leaves `sc->saved` set), so
        // a later bare DECRC restores the same state.
        let saved = self.cursor_save.main;
        self.restore_cursor_snapshot(saved);
        self.modes.alternate_screen = false;
        self.note_screen_replaced();
        self.grid.invalidate_host_coordinates_for_swap();
        self.holder_fence.note_alt_exit();
    }

    /// Leave an alternate screen no app is behind any more (the app died before `?1049l`).
    /// A no-op on the primary screen. Called for a shell prompt painted on the alt screen and
    /// for the host's `mainscreen` recovery (`Terminal::leave_alternate_screen`).
    ///
    /// Flushes the alt grid to the archive, then performs the exit the app would have: 1049
    /// after `?1049h`, 1047 after `?47h`/`?1047h` (a 1049 exit there could restore an earlier
    /// app's save), then `release_dead_app_modes`. Under conhost (`win32_input_active`),
    /// conhost never leaves its own alternate buffer and paints with absolute moves, so the
    /// cursor stays put and the used primary rows move into the scrollback first.
    pub(super) fn leave_orphaned_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }
        self.alt_archive
            .leave_orphaned(self.grid, self.transient.sync_end_seq);
        let conhost = self.modes.win32_input_active();
        let conhost_cursor = (self.grid.cursor(), self.grid.pending_wrap());
        // An empty main slot proves the screen was not entered by `?1049h`, which
        // always fills it (and nothing on the alternate screen empties it): a
        // checkpoint restore forgets the transient flag, and the 1049 exit of an
        // unsaved slot homes the cursor, so the shell's prompt would overwrite the
        // output a `?47h`/`?1047h` app left above it.
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
        // Under conhost the leave is ours alone: its own alternate buffer stays up and its next
        // switch repaints over this screen.
        self.transient.conhost_alt_screen_left_up = conhost;
    }

    /// Whether the `?1049l` about to run leaves the alternate screen of an app that died on it
    /// while its console host was asked to leave it (`TransientState::dead_alt_screen`, a
    /// prompt under the host policy or `Terminal::expect_host_alt_screen_leave`): that is the
    /// host's forwarded switch, and it takes the mark. The switch then releases the dead
    /// app's modes, which the host's leave does not do for the engine: a killed editor's
    /// mouse tracking would keep the wheel and clicks going to the shell as reports, and
    /// its scroll region would keep shell output out of the scrollback (`copy_margins`
    /// carries it onto the primary screen). An app's own exit, on a screen no prompt
    /// proved dead, keeps its modes: they are its own to reset.
    fn take_dead_alt_screen(&mut self) -> bool {
        let dead = self.modes.alternate_screen
            && self.transient.dead_alt_screen == Some(self.transient.screen_replaced);
        if dead {
            self.transient.dead_alt_screen = None;
        }
        dead
    }

    /// Reset the modes a dead full-screen app owned, which its own exit would
    /// have reset and the exits above carry over: its input modes
    /// (`release_dead_app_input`), scroll region and horizontal margins (shell
    /// output would never reach the scrollback), and an open 2026 window, the
    /// program's own frame included after the timeout cleared the mode
    /// (`Terminal::sync_frame_unfinished`). Focus reporting (conhost asks for it
    /// at ConPTY start) and the keyboard protocols stay: a shell sets those
    /// itself. Bracketed paste is the prompt's to decide
    /// (`release_dead_app_paste`): this also runs at the host's forwarded
    /// `?1049l`, which can follow a line editor's own `?2004h`.
    fn release_dead_app_modes(&mut self) {
        self.grid.reset_scroll_region();
        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();
        self.release_dead_app_input();
        if self.modes.synchronized_output || self.transient.app_sync_open {
            self.disable_synchronized_output();
        }
    }

    /// Turn off what a dead app left on that decides what the person's input
    /// becomes: mouse tracking and its encoding (the wheel and clicks would reach
    /// the shell as reports), application cursor keys and keypad, and the reports
    /// no shell asks for (`release_dead_app_reports`). None of them changes what
    /// the screen shows, so the prompt that proves the app dead releases them at
    /// once on either path (`prompt_leaves_orphaned_alt_screen`). The scroll
    /// region and margins wait for the screen to be left: under a host that is
    /// asked to leave (OpenConsole) its dead buffer keeps them until its switch,
    /// and what the shell draws in between must land alike on both sides.
    pub(super) fn release_dead_app_input(&mut self) {
        self.modes.mouse_mode = MouseMode::None;
        self.modes.mouse_encoding = MouseEncoding::X10;
        self.modes.application_cursor_keys = false;
        self.modes.application_keypad = false;
        self.release_dead_app_reports();
    }

    /// Turn off the reports a dead app asked for that no shell does: in-band size
    /// reports (2048), which type `CSI 48 ; rows ; cols ; … t` into the prompt
    /// at every resize, and colour-scheme reports (2031), which type
    /// `CSI ? 997 ; n n` at every theme change. vim and neovim set both; under
    /// OpenConsole, which forwards them, a killed vim left pwsh, cmd and Git
    /// Bash taking a report per resize as typed input (measured 2026-10-18).
    ///
    /// The reports still queued for the dead app go too
    /// (`TransientState::report_buffer`). One queued outside the session's reader
    /// waits for the reader's next take, and after a kill the next output is the
    /// shell's: a size report the window's first cell box queued while vim was
    /// alive went out with the console title the host wrote after the kill, 6 ms
    /// before pwsh's prompt, typed `[` and 83 `;` into its line, and its echo
    /// moved the cursor the host's leave carries the prompt for (measured
    /// 2026-10-18). The window now sends such a report at once (aterm-gui's
    /// `App::sync_cell_pixel_size`); one still queued at the prompt is dropped.
    pub(super) fn release_dead_app_reports(&mut self) {
        self.modes.in_band_size_reports = false;
        self.transient.size_report_held = false;
        self.modes.report_color_scheme = false;
        self.transient.report_buffer.clear();
    }

    /// Whether DEC 2048's enable-time size report waits instead of going out at once:
    /// behind a Windows console host (the `ResizePolicy::ConPty` posture) with the
    /// primary screen up. The report goes into the console's input, and there the reader
    /// is as likely the shell as the program that asked: OpenConsole forwards the
    /// enable, so a pwsh command that wrote `ESC [ ? 2048 h` had `[48;30;100;0;0t`
    /// typed into the next command line (a ParserError, measured 2026-10-18; conhost
    /// does not forward it). A full-screen app (vim, neovim) enables 2048 on its
    /// alternate screen and still gets the report at once; one that enables it first
    /// gets it when that screen comes up ([`Self::send_held_size_report`]). Every later
    /// resize reports as before. A Unix PTY, and an app already on the alternate
    /// screen, get it at once.
    fn hold_size_report(&self) -> bool {
        self.modes.win32_input_enabled && !self.modes.alternate_screen
    }

    /// The alternate screen came up: a size report held at the enable
    /// ([`Self::hold_size_report`]) goes out now, at the size the screen has, while
    /// the mode is still on.
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

    /// A shell prompt (`133;A`): the program that turned 2048 on has ended, and the
    /// shell reads the console now, so a size report still held for it
    /// ([`Self::hold_size_report`]) is dropped.
    pub(super) fn drop_held_size_report(&mut self) {
        self.transient.size_report_held = false;
    }

    /// A shell prompt on a dead app's screen that the console host is asked to leave
    /// (the host policy, `prompt_leaves_orphaned_alt_screen`): the prompt is drawn with
    /// the pen the host's `?1049l` restores (the main slot's rendition and character
    /// sets, DECRC's), as the local leave draws it, not with the one the dead app left
    /// set. A killed vim left bold reverse video on from its ruler, and the pwsh prompt
    /// the leave carries back (`Terminal::dead_alt_screen_carry`) came back in it
    /// (measured 2026-10-18). The cursor stays. Only ours changes: the host draws the
    /// prompt in the dead app's pen on a buffer it is about to drop, and both restore
    /// the same slot at the switch.
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

    /// At the shell prompt that proves a full-screen app dead (`133;A` on its
    /// alternate screen), turn off the bracketed paste it left on (2004), unless
    /// the shell switched paste itself after its `D` for this prompt (fish 3
    /// from its `fish_prompt` event; a readline whose `A` sits in `PS1`). A line
    /// editor that sets it after its `A` (bash and zsh under aterm's
    /// integration) sets it again; pwsh and cmd never do, and under OpenConsole
    /// a killed vim's `?2004h` turned the next line into junk and every paste
    /// into `ESC [ 200 ~ … ESC [ 201 ~` (measured 2026-10-18). Off, as DECSTR
    /// leaves it.
    pub(super) fn release_dead_app_paste(&mut self) {
        if !self.transient.bracketed_paste_from_prompt {
            self.disable_bracketed_paste();
        }
    }

    /// conhost is about to repaint over our primary screen (switching the alternate buffer it
    /// was left on, `TransientState::conhost_alt_screen_left_up`, which this consumes): move
    /// the screen's rows into the scrollback first. The cursor stays: both repaints home it.
    pub(super) fn keep_screen_from_conhost_repaint(&mut self) {
        if std::mem::take(&mut self.transient.conhost_alt_screen_left_up) {
            self.scroll_screen_into_scrollback();
        }
    }

    /// Scroll every used screen row (down to the last non-blank one) into the
    /// scrollback, leaving the screen blank and the cursor where it was. Nothing
    /// moves under a partial scroll region (the scroll would not reach the
    /// scrollback) or from a blank screen.
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
    fn disable_sgr_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Sgr {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    /// Capture cursor position, style, charset and mode flags.
    fn snapshot_cursor_state(&self) -> SavedCursorState {
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

    /// Restore cursor position, style and modes from a snapshot (if any). With origin mode
    /// restored on, the cursor is clamped as DECRC clamps it.
    fn restore_cursor_snapshot(&mut self, state: Option<SavedCursorState>) {
        let Some(state) = state else {
            // xterm's `CursorRestore` of a slot never saved (cursor.c: the
            // zeroed `SavedCursor`): home, origin mode off, rendition and
            // charsets reset, as DECRC does (`restore_cursor_state`). Reached by
            // a stray `?1049l`, and by `?47h`/`?1047h` left with `?1049l`.
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
            // DECAWM is not restored: xterm's CursorRestoreFlags applies only
            // ATTRIBUTES|ORIGIN|PROTECTED (cursor.c).
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            self.transient.current_underline_color = state.underline_color;
            self.transient.update_has_transient_extras();
            // The BCE template follows the restored background, or the first scroll after a
            // 1049 exit would use the alt screen's.
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );

            // Under DECOM the cursor is clamped to the scroll region, and to the horizontal
            // margins with DECLRMM (VT510).
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
            // The slot follows its content, so the restore keeps an offloaded reflow's fill debt;
            // the setter cleared pending_wrap: restore it only if the cursor is still at the
            // right edge (a resize may have moved the margin).
            if state.pending_wrap {
                let max_col = self.grid.effective_cols_for_row(row).saturating_sub(1);
                self.grid.set_pending_wrap(col >= max_col);
            }
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
    }

    /// DECRQM (`CSI ? Ps $ p`): answers `CSI ? Ps ; Pm $ y`, Pm 0 not recognized, 1 set,
    /// 2 reset, 3 permanently set, 4 permanently reset.
    pub(super) fn handle_decrqm(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let mode = params.first().copied().unwrap_or(0);

        #[inline]
        fn state(active: bool) -> u8 {
            if active { 1 } else { 2 }
        }

        let mode_state: u8 = match mode {
            1 => state(self.modes.application_cursor_keys),
            2 => state(!self.modes.vt52_mode), // Inverted: set=ANSI, reset=VT52
            3 => state(self.modes.column_mode_132),
            5 => state(self.modes.reverse_video),
            6 => state(self.modes.origin_mode),
            7 => state(self.modes.auto_wrap),
            // Mode 12 is the cursor style's blink bit (`set_cursor_blink`): report what the
            // renderer acts on.
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
                // 1048 is an action, not a mode: report whether this screen has a saved cursor.
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
            // ConPTY win32-input-mode exists only behind a console host (`win32_input_enabled`).
            9001 if self.modes.win32_input_enabled => state(self.modes.win32_input_mode),
            // Recognized but without effect here: permanently reset (4), not unknown (0).
            4 | 8 => 4,   // DECSCLM (smooth scroll), DECARM (auto repeat) — OS-managed
            18 | 19 => 4, // DECPFF (print form feed), DECPEX (print extent) — no printer
            // Grapheme cluster mode (contour's terminal-unicode-core) needs every UAX #29
            // cluster in one cell, which the print path does not do (a Devanagari conjunct
            // takes two), so it is permanently reset and `CSI ? 2027 h` is ignored. Answering 2
            // makes Neovim 0.11+ set it.
            2027 => 4,
            _ => 0, // Unknown mode
        };

        // `ESC[?` + mode (<= 5 digits) + `;` + state + `$y` <= 12 bytes.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// ANSI DECRQM (`CSI Ps $ p`): answers `CSI Ps ; Pm $ y`, with no `?`, Pm as for
    /// [`handle_decrqm`](Self::handle_decrqm).
    pub(super) fn handle_ansi_decrqm(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let mode = params.first().copied().unwrap_or(0);

        #[inline]
        fn state(active: bool) -> u8 {
            if active { 1 } else { 2 }
        }

        let mode_state: u8 = match mode {
            4 => state(self.modes.insert_mode),
            8 => {
                // BDSM — set means implicit BiDi (the default)
                state(self.modes.bidi_mode == crate::config::BiDiMode::Implicit)
            }
            20 => state(self.modes.new_line_mode),
            // SRM (local echo): permanently reset, always full duplex.
            12 => 4,
            _ => 0, // Unknown ANSI mode
        };

        // Same bound as DEC DECRQM minus the `?`.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// SM/RM (`CSI Ps h/l`): IRM (4), BDSM (8: set is implicit BiDi, reset explicit; see
    /// <https://terminal-wg.pages.freedesktop.org/bidi/>) and LNM (20: LF also does CR).
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
                _ => {} // Unknown ANSI mode
            }
        }
    }

    /// Parse a VT420 rectangle `Pt;Pl;Pb;Pr` (1-indexed, 0 or absent is the default: the whole
    /// area) into 0-indexed absolute `(top, left, bottom, right)`. Under DECOM it is relative
    /// to the scroll region and horizontal margins (DEC STD 070 §5.5.2). `None` when
    /// top > bottom or left > right.
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

    /// DECERA, Erase Rectangular Area (VT420+): `CSI Pt ; Pl ; Pb ; Pr $ z` sets the rectangle
    /// to the BCE blank (see [`parse_rect_coords`](Self::parse_rect_coords)).
    pub(super) fn handle_decera(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        self.grid.erase_rect(top, left, bottom, right);
    }

    /// DECCARA, Change Attributes in Rectangular Area (VT420+): `CSI Pt ; Pl ; Pb ; Pr ; Ps...
    /// $ r` applies the SGR subset [`sgr_params_to_flags`](Self::sgr_params_to_flags) knows to
    /// the rectangle, or to the character stream from (top, left) to (bottom, right) under
    /// DECSACE stream mode (`stream_attribute_extent`).
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

    /// DECRARA, Reverse Attributes in Rectangular Area (VT400+): `CSI Pt ; Pl ; Pb ; Pr ; Pm
    /// $ t`. xterm ctlseqs: "Pm denotes the attributes to reverse, i.e., 0, 1, 4, 5, 7, 8.
    /// Reversing SGR 0 reverses modes 1, 4, 5, 7. Reversing SGR 8 is an xterm extension."
    ///
    /// The reverse half of DECCARA: the same rectangle parsing and DECSACE extent, but each
    /// named attribute is toggled per cell. DA1 advertises code 28 (rectangular editing).
    pub(super) fn handle_decrara(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        // Pm defaults to 0, "reverse modes 1, 4, 5, 7", as DECCARA's list defaults to SGR 0.
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

    /// Split a DECRARA `Pm` list into (single-bit flags to XOR, reverse-underline). Underline
    /// is not one bit (the underline flags combine to spell dotted and dashed), so the grid
    /// decides it per cell (`reverse_cell_attrs`). Codes outside DECRARA's list
    /// (0, 1, 4, 5, 7, 8) are ignored: a reset code like 22 has no defined reverse.
    fn sgr_params_to_reverse_flags(sgr_params: &[u16]) -> (CellFlags, bool) {
        let mut toggle = CellFlags::empty();
        let mut toggle_underline = false;

        for &param in sgr_params {
            match param {
                // "Reversing SGR 0 reverses modes 1, 4, 5, 7."
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
                // SGR 8 (invisible) is xterm's extension to the DEC list.
                8 => toggle = toggle.union(CellFlags::HIDDEN),
                _ => {}
            }
        }

        (toggle, toggle_underline)
    }

    /// DECCRA, Copy Rectangular Area (VT420+): `CSI Pts ; Pls ; Pbs ; Prs ; Pps ; Ptd ; Pld ;
    /// Ppd $ v` copies the source rectangle to the destination top-left (1-indexed, 0 is the
    /// default, DECOM-aware). The page parameters are ignored.
    pub(super) fn handle_deccra(&mut self, params: &[u16]) {
        let Some((src_top, src_left, src_bottom, src_right)) = self.parse_rect_coords(params)
        else {
            return;
        };
        // params[4], the source page, is ignored: there is one page.

        let dst_params = if params.len() > 5 {
            &params[5..]
        } else {
            &[1u16][..]
        };
        // Destination top-left only (origin-relative under DECOM); the size is the source's.
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
        // params[7], the destination page, is ignored.

        self.grid
            .copy_rect(src_top, src_left, src_bottom, src_right, dst_top, dst_left);
    }

    /// The DECCARA subset of SGR (bold, dim, italic, underline, blink, inverse, hidden,
    /// strikethrough and their resets) as `(flags_to_set, flags_to_clear)`.
    fn sgr_params_to_flags(sgr_params: &[u16]) -> (CellFlags, CellFlags) {
        let mut flags_to_set = CellFlags::empty();
        let mut flags_to_clear = CellFlags::empty();

        for &param in sgr_params {
            match param {
                0 => {
                    // SGR 0 clears every visual flag but the structural WIDE /
                    // WIDE_CONTINUATION (0x0600), which VISUAL_FLAGS_MASK includes.
                    flags_to_clear = CellFlags::from_bits(CellFlags::VISUAL_FLAGS_MASK & !0x0600);
                    flags_to_set = CellFlags::empty();
                }
                1 => flags_to_set = flags_to_set.union(CellFlags::BOLD),
                2 => flags_to_set = flags_to_set.union(CellFlags::DIM),
                3 => flags_to_set = flags_to_set.union(CellFlags::ITALIC),
                4 => {
                    // SGR 4 clears the other underline styles first, as apply_sgr_param does.
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
                    // SGR 24 clears every underline style, as apply_sgr_param does.
                    flags_to_clear = flags_to_clear.union(CellFlags::ALL_UNDERLINES);
                }
                25 => flags_to_clear = flags_to_clear.union(CellFlags::BLINK),
                27 => flags_to_clear = flags_to_clear.union(CellFlags::INVERSE),
                28 => flags_to_clear = flags_to_clear.union(CellFlags::HIDDEN),
                29 => flags_to_clear = flags_to_clear.union(CellFlags::STRIKETHROUGH),
                _ => {} // Other SGR codes are not applicable to DECCARA
            }
        }

        (flags_to_set, flags_to_clear)
    }

    /// DECFRA, Fill Rectangular Area (VT420+): `CSI Pch ; Pt ; Pl ; Pb ; Pr $ x` fills the
    /// rectangle with character Pch. Only printable codes (0x20..=0x7E, 0xA0..=0xFF) are
    /// accepted (VT420); any other, or none, is a no-op.
    pub(super) fn handle_decfra(&mut self, params: &[u16]) {
        let ch_code = params.first().copied().unwrap_or(0);

        let printable = matches!(ch_code, 0x20..=0x7E | 0xA0..=0xFF);
        if !printable {
            return;
        }

        // The fill takes the current SGR attributes (VT420). ch_code is validated above.
        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let fill = crate::grid::Cell::from_raw_parts(ch_code, colors, flags);

        // Truecolor fg/bg live out of line (`cached_colors()` holds only the RGB markers), so
        // resolve the bytes for `fill_rect` to write to the CellExtras ring.
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

    /// DECSERA, Selective Erase Rectangular Area (VT420+): `CSI Pt ; Pl ; Pb ; Pr $ {` erases
    /// the rectangle's cells that DECSCA does not protect.
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

    /// A cursor-only change damages the cursor cell: something (a bare DECTCEM must not be
    /// swallowed by the frontend's redraw early-out), but not everything (TUIs bracket every
    /// repaint in `?25l … ?25h`).
    #[test]
    fn a_cursor_only_change_damages_the_cursor_cell_not_the_whole_grid() {
        // Each case is a real transition (a mode already in place damages nothing).
        for (name, setup, seq) in [
            ("DECTCEM hide", &b""[..], &b"\x1b[?25l"[..]),
            ("DECTCEM show", &b"\x1b[?25l"[..], &b"\x1b[?25h"[..]),
            ("DECSCUSR shape", &b"\x1b[2 q"[..], &b"\x1b[6 q"[..]),
            ("mode 12 blink", &b"\x1b[?12l"[..], &b"\x1b[?12h"[..]),
        ] {
            let mut term = Terminal::new(24, 80);
            // Off the origin, so a full mark would show on row 0.
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

    /// Every alt-screen exit mode leaves the restored grid fully damaged and advances the
    /// damage epoch.
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

    /// Control: a change that repaints every cell (reverse video, bidi direction) still marks
    /// the whole grid.
    #[test]
    fn a_whole_screen_change_still_marks_the_whole_grid() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[9;20H");
        term.take_damage();
        term.process(b"\x1b[?5h"); // DECSCNM: reverse video
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
        // Default: DECBKM reset -> keyboard mode lacks the flag.
        assert!(
            !term
                .keyboard_mode()
                .contains(KeyboardMode::BACKARROW_SENDS_BS)
        );
        // Set DECBKM -> the legacy keyboard mode carries the flag.
        term.process(b"\x1b[?67h");
        assert!(
            term.keyboard_mode()
                .contains(KeyboardMode::BACKARROW_SENDS_BS)
        );
        // Reset -> flag clears.
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
        // A program that sets it anyway changes nothing.
        term.process(b"\x1b[?2027h\x1b[?2027$p");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?2027;4$y",
            "after CSI ? 2027 h"
        );
        assert!(!term.modes().grapheme_cluster_mode);
        // DECRQSS, the other spelling of the query, agrees.
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

    // --- DECSET/DECRST -> KeyboardMode projection ---

    #[test]
    fn mode_1035_special_modifiers_toggles_flag() {
        let mut term = Terminal::new(24, 80);
        // numLock special modifiers on by default -> NO_SPECIAL_MODIFIERS absent.
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

    // --- end-to-end: the mode changes the encoded bytes ---

    #[test]
    fn mode_1039_reset_drops_alt_escape_prefix_in_encoding() {
        let mut term = Terminal::new(24, 80);
        // Default: Alt+a -> ESC a.
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
            vec![0x1b, b'a']
        );
        // After DECRST 1039: Alt+a -> bare 'a'.
        term.process(b"\x1b[?1039l");
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
            vec![b'a']
        );
    }

    #[test]
    fn mode_1036_set_adds_meta_escape_prefix_in_encoding() {
        let mut term = Terminal::new(24, 80);
        // Default: Meta+a -> bare 'a' (Meta unhandled in legacy path).
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
            vec![b'a']
        );
        // After DECSET 1036: Meta+a -> ESC a.
        term.process(b"\x1b[?1036h");
        assert_eq!(
            encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
            vec![0x1b, b'a']
        );
    }

    // --- DECRQM reporting ---

    #[test]
    fn decrqm_reports_keyboard_mode_states() {
        let mut term = Terminal::new(24, 80);
        // Defaults: 1035 set (1), 1036 reset (2), 1039 set (1), 1045 reset (2).
        term.process(b"\x1b[?1035$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1035;1$y");
        term.process(b"\x1b[?1036$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1036;2$y");
        term.process(b"\x1b[?1039$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1039;1$y");
        term.process(b"\x1b[?1045$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1045;2$y");

        // Toggle each and re-query.
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
    //! ConPTY win32-input-mode (DEC 9001), the mode-table half; the encoded bytes are pinned
    //! in `keyboard_mode::shift_enter_e2e_tests`.
    use crate::terminal::Terminal;
    use aterm_types::keyboard::KeyboardMode;

    /// A session behind a console host: the posture aterm-gui applies to a
    /// ConPTY session (`spawn::apply_pty_keyboard_posture`).
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
        // DECSTR is the application's soft reset; the console host's negotiation is not the
        // application's to undo, so only RIS clears it.
        let mut term = conpty_term();
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[!p");
        assert!(term.modes().win32_input_mode);
    }
}

impl super::Terminal {
    /// Force the primary screen: the host's recovery for an alternate screen a killed app left
    /// up. `true` when the alternate screen was active; its last frame is then in the archive
    /// ([`Self::alt_archive`]). Runs as an empty output batch, since the batch prologue and
    /// epilogue own the viewport pin, selection park, archive boundary and mode mirror.
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
    //! A killed full-screen app leaves the alternate screen up with no `?1049l`: the host's
    //! recovery and the mode facts it rests on (the prompt half is in `handler_osc_shell.rs`).
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
        // 8 main lines, the command line and the empty row its `\r\n` opened = 10
        // rows on a 5-row grid: 5 scrolled off, reachable again.
        assert_eq!(term.grid().scrollback_lines(), 5);
        let screen: Vec<String> = (0..5).map(|r| term.row_text(r).unwrap()).collect();
        assert_eq!(screen[3].trim_end(), "$ less file");
        assert!(
            screen.iter().all(|r| !r.contains("pager")),
            "the pager rows stay on the alt grid, not the restored screen: {screen:?}"
        );
        // The cursor `?1049h` saved: the row after the command, column 0.
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
        // The request was consumed: the next batch does not leave the next app's screen.
        term.process(b"\x1b[?1049h\x1b[Hsecond app");
        assert!(term.modes().alternate_screen);
        assert_eq!(term.row_text(0).unwrap().trim_end(), "second app");
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
    }

    #[test]
    fn host_leave_of_a_mode_47_screen_keeps_the_cursor_where_the_app_had_it() {
        // `?47h` saves no cursor, so the leave restores none, even with the main slot holding
        // an earlier 1049 app's save.
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
        // The dead screen's rows went to the archive: a later `?47h` shows a blank buffer.
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
        // The latest entry's mode decides: a 47 app that exited cleanly does not turn the
        // next 1049 app's leave into a 47 one.
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
        // A killed editor's scroll region, mouse tracking, cursor-key and keypad modes and
        // open 2026 window are released so shell output reaches the scrollback; focus
        // reporting stays.
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
        // The reports no shell asks for go with the app; bracketed paste is the
        // prompt's to decide (a shell may have set its own since).
        assert!(!term.modes().in_band_size_reports);
        assert!(!term.modes().report_color_scheme);
        assert!(term.modes().bracketed_paste);
    }
}

#[cfg(test)]
mod conhost_left_alt_screen_tests {
    //! After an orphaned leave under conhost, conhost is still on the dead app's alternate
    //! buffer, and its next switch repaints over our primary screen. The streams are ConPTY
    //! captures (`less` killed, two commands, a second `less` and `q`) scaled to 10 rows.
    use crate::terminal::Terminal;

    const ROWS: u16 = 10;
    const PROMPT: &[u8] = b"\x1b]133;A\x07PS> \x1b]133;B\x07";

    /// Every retained row, scrollback first, trailing blanks trimmed.
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

    /// conhost's blank repaint of the viewport and its `?1049h`, in one read.
    fn blank_repaint_then_enter() -> Vec<u8> {
        let mut read = b"\x1b[?25l\x1b[H".to_vec();
        for _ in 1..ROWS {
            read.extend_from_slice(b"\x1b[K\r\n");
        }
        read.extend_from_slice(b"\x1b[K\x1b[?25h\x1b[?1049h");
        read
    }

    /// conhost's repaint of its primary buffer after `?1049l`: frozen at the
    /// dead pager's start, so it knows nothing typed since.
    fn stale_primary_repaint() -> Vec<u8> {
        let mut read = b"\x1b[?25l\x1b[H".to_vec();
        for row in ["history 1", "history 2", "PS> less file"] {
            read.extend_from_slice(row.as_bytes());
            read.extend_from_slice(b"\x1b[K\r\n");
        }
        read.extend_from_slice(b"\x1b[K\x1b[4;1H\x1b[?25h");
        read
    }

    /// A pager killed on the alternate screen, the prompt's recovery, and two
    /// commands typed after it — under conhost (`?9001h`) or not.
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
        // The screen is conhost's frozen primary buffer plus the new prompt; the
        // rows it lost are in the scrollback above it.
        assert_eq!(term.row_text(2).unwrap().trim_end(), "PS> less file");
        assert_eq!(term.row_text(3).unwrap().trim_end(), "PS>");
    }

    #[test]
    fn without_conhost_the_same_bytes_move_nothing() {
        // Negative control: with no console host no orphaned leave marks the
        // state, and the blank repaint overwrites the rows in place.
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
        // A `?1049l` written while conhost is still on the dead app's buffer: conhost
        // switches back and repaints its frozen primary, and the reset is the only warning.
        let mut term = recovered_with_two_commands(true);
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        term.process(&stale_primary_repaint());
        let rows = all_rows(&term);
        for want in TYPED_AFTER {
            assert_eq!(count(&rows, want), 1, "{rows:?}");
        }
        // Consumed: a later exit on the primary screen moves nothing again.
        let kept = term.grid().scrollback_lines();
        term.process(b"\x1b[?1049l");
        assert_eq!(term.grid().scrollback_lines(), kept);
    }

    #[test]
    fn the_host_forced_leave_under_conhost_keeps_the_rows_too() {
        // cmd.exe paints no marks, so the `mainscreen` verb is the recovery; conhost is left
        // on the dead app's buffer the same way.
        let mut term = Terminal::new(ROWS, 40);
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h");
        term.process(b"C:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J");
        for i in 1..ROWS {
            term.process(format!("pager {i}\r\n").as_bytes());
        }
        term.process(b":\r\nC:\\>echo stuck\r\nstuck\r\n\r\nC:\\>");
        assert!(term.leave_alternate_screen());
        // conhost's cursor, after its prompt on the bottom row; the one primary
        // row is in the scrollback, the rows typed while stuck in the archive.
        assert_eq!((term.cursor().row, term.cursor().col), (ROWS - 1, 4));
        assert_eq!(all_rows(&term)[0], "C:\\>less file");
        assert_eq!(term.grid().scrollback_lines(), 1);
        let archived = term.alt_archive().texts();
        for want in ["C:\\>echo stuck", "stuck", "C:\\>"] {
            assert!(archived.iter().any(|r| r == want), "{want:?}: {archived:?}");
        }
        // A command after the verb, then conhost's next pager: its blank repaint
        // and `?1049h`, and the exit.
        term.process(b"echo after\r\nafter\r\n\r\nC:\\>");
        term.process(&blank_repaint_then_enter());
        term.process(b"\x1b[?1049l");
        let rows = all_rows(&term);
        assert_eq!(count(&rows, "C:\\>less file"), 1, "{rows:?}");
        assert_eq!(count(&rows, "after"), 1, "{rows:?}");
        // The prompt it was typed after was painted on the dead screen, so the
        // row holds the command alone, where conhost put it.
        assert_eq!(count(&rows, "    echo after"), 1, "{rows:?}");
    }

    /// Without a console host a program's `?9001h` is just output: the recovery still leaves
    /// the unix way (the 1049 exit restores the saved cursor, the primary rows stay).
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

    /// A pager that filled the screen after a short history, killed, then the
    /// shell's prompt: conhost scrolled its alternate buffer for the prompt, so
    /// its cursor is on the bottom row, far below the row `?1049h` saved.
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
        // A command that wraps, as conhost renders it (`\r\n ESC[23;80H` on 24
        // rows): fill the row, scroll, re-anchor on the head's row by absolute
        // position and re-print its last cell, so the tail wraps.
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
        // A resize makes conhost repaint its whole alternate buffer (the dead
        // pager's rows and everything printed since) over our primary screen.
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
        // The repaint wrote on the rows conhost itself painted, not beside them.
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (ROWS - 1, 4));
        assert_eq!(
            term.row_text(usize::from(ROWS - 2)).unwrap().trim_end(),
            "one"
        );
    }

    #[test]
    fn without_conhost_the_prompt_returns_to_the_row_1049_saved() {
        // Negative control: with no console host the recovery is the clean exit: history
        // stays, the cursor returns to the row `?1049h` saved, nothing moves to scrollback.
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
        // A new app's enter makes conhost's alternate buffer live again, and RIS forgets the
        // state: either way the next switch moves nothing.
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
        // A scroll within DECSTBM does not reach the scrollback, so the rows are
        // not moved at all rather than scrolled into nowhere; the state is spent.
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
/// aterm's own orphaned-screen recovery makes the restore dangerous). A `?47h`
/// screen left with `?1049l` is a real exit and restores as xterm does.
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

    /// The contrast: leaving a screen `?1049h` entered is DECRC from the slot
    /// that set filled (xterm's one `CursorRestore`), not only the cursor the
    /// main grid kept. Whatever changed between the save and the restore (the
    /// pen and its underline colour, G0, origin mode, the margins under a saved
    /// origin mode, a widen that moved the edge away from a saved wrap), the
    /// terminal and a probe glyph end where a DECSC .. DECRC pair on the main
    /// screen leaves them.
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

    /// The case that rules out xterm's restore: a full-screen app dies on the
    /// alternate screen, the shell's prompt mark leaves it for the app
    /// (restoring the slot its `?1049h` saved, which DECRC must keep), output
    /// and prompts follow, and then the user types `tput rmcup`. A restore
    /// there put the cursor back on the dead app's launch row, and the next
    /// prompt overwrote `fileA fileB`.
    #[test]
    fn a_rmcup_after_the_orphaned_screen_recovery_keeps_the_cursor() {
        let mut term = Terminal::new(8, 30);
        term.process(b"out1\r\nout2\r\n$ vim\r\n\x1b[?1049h\x1b[H\x1b[2Jvim screen");
        // vim is SIGKILLed; the shell paints its prompt with a 133 mark.
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
        // The rmcup line's LF scrolls the screen one row, so `fileA fileB`
        // is on row 3, the row the slot names.
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
        // `?47h` saves nothing; the `?1049l` exit restores the main slot (here
        // unsaved, so home) exactly as a 1049 exit does.
        let mut term = Terminal::new(4, 20);
        term.process(b"\x1b[3;9H\x1b[?47h\x1b[2;2Hx\x1b[?1049l");
        assert!(!term.is_alternate_screen());
        assert_eq!(cursor(&term), (0, 0));
    }

    #[test]
    fn a_restored_session_leaves_a_dead_1047_screen_where_the_live_one_would() {
        // An app entered with `?1047h` (no cursor ever saved) when a seamless update
        // checkpointed the session; it then dies and the shell's prompt mark leaves
        // its screen. The checkpoint does not carry which mode entered the screen,
        // so the restored session must reach the live one's 1047 exit from the
        // empty main slot, not a 1049 exit that homes the cursor onto `out1`.
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
/// (xterm's `ClearCells`/`ScrnClearLines` read the one colour pair). Found by
/// the astream-term differential oracle (aterm-conformance `tests/vt_oracle`).
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
        // And back on the main buffer after a background set on the alternate.
        let mut term = Terminal::new(3, 10);
        term.process(b"\x1b[?47h\x1b[42m\x1b[?47l\x1b[3;1H\n");
        assert_eq!(bg_index(&term, 2, 0), Some(2), "leaving ?47");

        // 1049l re-arms the main grid from the pen it restores, which a session
        // restored on the alternate screen needs: the checkpoint rebuilds only
        // the active grid's template, so its main grid comes back plain.
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

    /// An OSC 8 link a program left open does not outlive its screen: every
    /// switch, either way, closes it, so the next glyph on the other screen
    /// carries no link.
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
        // No host has reported a cell box: 0 pixels, as the PTY's winsize says,
        // never the 8x16 placeholder.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;0;0t"
        );
    }

    #[test]
    fn mode_2048_reports_a_new_cell_box_at_the_same_size() {
        // The first measurement, then a DPI move: rows/cols stay, pixels change.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        let _ = term.take_response(); // enable report
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
        // `?2048h` is idempotent: only the disabled->enabled transition emits.
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
        // A `?2048l?2048h` flood defeats the transition guard; the buffer cap still holds.
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        for _ in 0..200_000 {
            input.extend_from_slice(b"\x1b[?2048l\x1b[?2048h");
        }
        term.process(&input);
        let resp = term.take_response().unwrap_or_default();
        // Uncapped this would be ~3.8 MB; the cap bounds it to 1 MiB.
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
        let _ = term.take_response(); // enable report
        term.resize(30, 100);
        // pixH = 30*20 = 600, pixW = 100*10 = 1000.
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;600;1000t"
        );
    }

    // A resize's report is sent unasked: `take_reports` takes it apart from the
    // replies (here the answer to a cursor query read after the resize), which
    // stay for `take_response`; `take_response` alone hands over both, the
    // replies first, as a host that sends them one way has always got them.
    #[test]
    fn a_resize_report_is_taken_apart_from_the_replies() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048h");
        let _ = term.take_response(); // enable report
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

    /// A tab behind a Windows console host, postured as the window postures one.
    fn console_host_tab() -> Terminal {
        let mut term = Terminal::new(30, 100);
        term.set_console_host_posture(crate::grid::ResizePolicy::ConPty);
        term
    }

    // Behind a console host an enable on the primary screen reports nothing yet:
    // the console's reader there is as likely the shell, and pwsh took the report
    // as the start of its next command line (OpenConsole, measured 2026-10-18).
    // The alternate screen coming up with the mode on sends it, once, at the
    // size the screen has then.
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
            let _ = term.take_response(); // a resize reports as before
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

    // A full-screen app that enables 2048 on its own screen (vim, neovim) gets
    // the report at once, as it does without a console host.
    #[test]
    fn behind_a_console_host_an_enable_on_the_alternate_screen_reports_at_once() {
        let mut term = console_host_tab();
        term.process(b"\x1b[?1049h\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
    }

    // What ends the program that asked drops the held report: its `?2048l`, the
    // shell's next prompt (`133;A`), a reset. A later alternate screen (the next
    // app's) is not sent another program's report.
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
        // The app the prompt is followed by asks again, and is answered.
        let mut term = console_host_tab();
        term.process(b"\x1b[?2048h\x1b]133;A\x07PS> \x1b[?1049h\x1b[?2048l\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;30;100;0;0t"
        );
    }

    // Without a console host (a Unix PTY, a native session) the enable reports
    // at once on either screen, as before.
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
    //! Mode queries answer the state the renderer acts on.
    use crate::terminal::Terminal;
    use aterm_types::CursorStyle;

    fn decrqm(term: &mut Terminal, mode: u16) -> Vec<u8> {
        term.process(format!("\x1b[?{mode}$p").as_bytes());
        term.take_response().unwrap_or_default()
    }

    /// DEC mode 12 is the cursor style's blink bit: the renderer blinks only `Blinking*`
    /// styles, and `CSI ? 12 $ p` answers the same fact. ncurses' `curs_set()` drives it via
    /// `xterm-256color`'s `cnorm`/`cvvis`.
    #[test]
    fn mode_12_moves_the_cursor_the_user_sees_and_reports_it() {
        let mut term = Terminal::new(24, 80);
        // Power-on cursor is a blinking block, so mode 12 is SET.
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");

        // terminfo `cnorm`: stop blinking, show the cursor.
        term.process(b"\x1b[?12l\x1b[?25h");
        assert_eq!(
            term.cursor_style(),
            CursorStyle::SteadyBlock,
            "?12l must steady the cursor, not just set a flag"
        );
        assert!(term.cursor_visible());
        assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

        // terminfo `cvvis`: blink again. The shape is untouched by mode 12, so
        // a bar stays a bar.
        term.process(b"\x1b[6 q"); // DECSCUSR steady bar
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

    /// DECSCUSR carries the blink bit too, so it moves mode 12 with it.
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
            // DECRQSS `?12` is the third spelling of the same fact.
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

    /// Replay the recovered mode-12 request with the current explicit ConPTY
    /// capability: mode 12 must not replace the configured or DECSCUSR style.
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

        // DECTCEM in the same request still controls visibility.
        term.process(b"\x1b[?25l");
        assert!(!term.cursor_visible());
        term.process(b"\x1b[?12;25h");
        assert!(term.cursor_visible());
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);

        // DECSCUSR still chooses the style, including its blink bit.
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

        // Host posture can revoke a previously negotiated mode. Its stored
        // bit alone must not suppress ordinary mode 12 on the new host.
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

    /// Every mode DECRQM can report is one XTSAVE/XTRESTORE can carry: the two
    /// tables (`query_dec_mode`, `handle_decrqm`) must agree.
    #[test]
    fn xtsave_covers_every_mode_decrqm_reports() {
        // Every mode `handle_decrqm` answers 1 or 2 for (3, 4 and 0 are not saveable state).
        const REPORTED: &[u16] = &[
            1, 3, 5, 6, 7, 9, 12, 25, 40, 45, 66, 67, 69, 80, 95, 1000, 1002, 1003, 1004, 1005,
            1006, 1007, 1015, 1016, 1035, 1036, 1039, 1045, 1243, 2004, 2026, 2031, 2048, 2500,
            2501, 9001,
        ];
        let mut lost = Vec::new();
        for &mode in REPORTED {
            let mut term = Terminal::new(24, 80);
            // Mode 3 (DECCOLM) is only honored while mode 40 is set.
            if mode == 3 {
                term.process(b"\x1b[?40h");
            }
            // Mode 9001 exists only behind a console host (the host's posture).
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
            term.process(format!("\x1b[?{mode}s").as_bytes()); // XTSAVE
            term.process(format!("\x1b[?{mode}l").as_bytes()); // clear it
            let _ = term.take_response();
            term.process(format!("\x1b[?{mode}r").as_bytes()); // XTRESTORE
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
