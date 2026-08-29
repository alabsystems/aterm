// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DEC private mode handler for the terminal.
//!
//! This module contains handlers for DEC private mode escape sequences:
//! - DECSET/DECRST (CSI ? Ps h/l) - Enable/disable DEC private modes
//! - DECRQM (CSI ? Ps $ p) - Request DEC mode state
//! - Standard ANSI modes (CSI Ps h/l)
//!
//! Modes include: cursor keys, VT52 mode, column mode, reverse video,
//! origin mode, auto-wrap, cursor visibility, alternate screen buffer,
//! mouse tracking, bracketed paste, synchronized output, and more.
//!
//! Extracted from handler.rs as part of large files refactor.

use crate::grid::{CellFlags, Grid};

use super::handler::TerminalHandler;
use super::stack_response::StackResponse;
use super::{MouseEncoding, MouseMode, SavedCursorState};
use core::fmt::Write as _;

impl TerminalHandler<'_> {
    /// Handle DEC private mode set/reset.
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
                    // DECCOLM — flag only, host resizes if desired.
                    // Per xterm spec, DECCOLM is only honored when mode 40
                    // (deccolm_enable) is set. Otherwise CSI ?3h/l is ignored.
                    // Arm-local `if`, not a match guard (which would couple this
                    // to the unknown-mode default arm's fallthrough).
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
                    self.set_cursor_blink(set);
                }
                25 => {
                    if set {
                        self.show_cursor();
                    } else {
                        // REPAINT-BLINK epoch: a DECTCEM hide issued INSIDE a
                        // DEC-2026 synchronized update is the per-keystroke
                        // full-redraw choreography (Claude Code brackets every
                        // repaint in `?2026h · ?25l · … · ?25h · ?2026l`).
                        // vim/less hide WITHOUT sync and ConPTY hides per echo
                        // without sync — neither bumps. MULTIPLEXERS (negative
                        // law, source+capture-verified in review): tmux 3.x
                        // never probes DECRQM 2026 and coalesces cursor
                        // visibility per event-loop pass, so vim-inside-tmux
                        // yields at most one-off edges, never a per-keystroke
                        // blink — its jumps keep their meteors. Counted on
                        // every processed hide (not just visible→hidden edges)
                        // so a burst that re-hides an already-hidden cursor
                        // still registers as repaint choreography.
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
                    // DECNKM — Numeric Keypad Mode.
                    // CSI ? 66 h = application keypad (same as ESC =)
                    // CSI ? 66 l = numeric keypad (same as ESC >)
                    // Used by dialog(1), nano, and others as alternative to ESC =/>.
                    self.set_application_keypad(set);
                }
                69 => {
                    self.set_left_right_margin_mode(set);
                }
                40 => {
                    // Mode 40 — Enable 80/132 column switching.
                    // When reset, DECCOLM (mode 3) is ignored. Per xterm spec.
                    self.modes.deccolm_enable = set;
                }
                80 => {
                    // DECSDM — Sixel Display Mode.
                    // SET: Sixel output scrolls at bottom margin.
                    // RESET: Sixel output clips at bottom margin.
                    self.set_sixel_display_mode(set);
                }
                95 => {
                    // DECNCSM — No Clearing Screen on Column Change.
                    // When set, DECCOLM toggle does not clear screen/margins/cursor.
                    // Per xterm/VT510 spec.
                    self.modes.decncsm = set;
                }
                47 => {
                    // Mode 47: Switch buffer only. No save/restore cursor. No clear.
                    if set {
                        self.enter_alternate_screen_raw();
                    } else {
                        self.exit_alternate_screen_raw();
                    }
                }
                1047 => {
                    // Mode 1047: Switch buffer. Clear alt screen on exit. No cursor save/restore.
                    if set {
                        self.enter_alternate_screen_raw();
                    } else {
                        self.exit_alternate_screen_1047();
                    }
                }
                1049 => {
                    // Mode 1049: Switch buffer + save/restore cursor + clear alt on enter.
                    if set {
                        self.enter_alternate_screen();
                    } else {
                        self.exit_alternate_screen();
                    }
                }
                2004 => {
                    if set {
                        self.enable_bracketed_paste();
                    } else {
                        self.disable_bracketed_paste();
                    }
                }
                // Mouse tracking modes (mutually exclusive — reset to None on DECRST)
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
                2027 => {
                    self.set_grapheme_cluster_mode(set);
                }
                // DEC mode 2031: color-scheme change reporting. When set, the
                // terminal emits CSI ? 997 ; Ps n on every OS appearance change
                // (driven by Terminal::set_color_scheme). Apps query the current
                // state via DSR CSI ? 996 n regardless of this mode.
                2031 => {
                    self.modes.report_color_scheme = set;
                }
                // DEC mode 2048: in-band size reports. On enable, emit the current
                // geometry immediately so the app has it before the first resize;
                // Terminal::resize emits on every subsequent change.
                2048 => {
                    let was = self.modes.in_band_size_reports;
                    self.modes.in_band_size_reports = set;
                    // Emit the geometry ONLY on a real disabled->enabled transition
                    // (idempotent enable, matching xterm/ghostty) AND only while the
                    // response buffer is under its cap. This path writes the buffer
                    // DIRECTLY rather than via the capped/rate-limited send_response
                    // sink, so without both guards a PTY `?2048h` flood — or a
                    // `?2048l?2048h` toggle flood that defeats the transition guard
                    // alone — would grow response_buffer without bound, bypassing
                    // MAX_RESPONSE_BUFFER_SIZE. The report is <= 48 bytes (the
                    // StackResponse capacity in push_in_band_size_report).
                    const IN_BAND_SIZE_REPORT_MAX_LEN: usize = 48;
                    if set && !was && self.can_append_response_bytes(IN_BAND_SIZE_REPORT_MAX_LEN) {
                        let (rows, cols) = (self.grid.rows(), self.grid.cols());
                        let (cw, ch) = self.iterm2.cell_px;
                        super::state_accessors::push_in_band_size_report(
                            &mut self.transient.response_buffer,
                            rows,
                            cols,
                            cw,
                            ch,
                        );
                    }
                }
                // === BiDi DEC Private Modes (Terminal WG specification) ===
                // See: https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html
                1243 => {
                    self.set_bidi_arrow_swap(set);
                }
                2500 => {
                    self.set_bidi_box_mirroring(set);
                }
                2501 => {
                    self.set_bidi_autodetection(set);
                }
                // DECBKM (mode 67): backspace sends BS (0x08) when set, DEL (0x7f)
                // when reset (default). Folded into the legacy keyboard encoding.
                67 => {
                    self.modes.backarrow_sends_bs = set;
                }
                // xterm `numLock` (mode 1035): special modifiers for Alt/NumLock.
                // When reset, NumLock is no longer treated as a real modifier.
                // Folded into the legacy keyboard encoding.
                1035 => {
                    self.modes.special_modifiers = set;
                }
                // xterm `metaSendsEscape` (mode 1036): when set, Meta-modified
                // keys are ESC-prefixed. Folded into the legacy keyboard encoding.
                1036 => {
                    self.modes.meta_send_escape = set;
                }
                // xterm `altSendsEscape` (mode 1039): when set (default), an
                // Alt-modified key is ESC-prefixed; when reset, the prefix is
                // suppressed. Folded into the legacy keyboard encoding.
                1039 => {
                    self.modes.alt_send_escape = set;
                }
                // Mode 1045: tracked-only xterm private mode (no encoding effect).
                // xterm does not assign it a keyboard-input meaning, so we track
                // the flag for DECRQM fidelity without changing key encoding.
                1045 => {
                    self.modes.mode_1045 = set;
                }
                // Mode 1048: xterm save/restore cursor (equivalent to DECSC/DECRC)
                1048 => {
                    if set {
                        self.cursor_state().save_cursor_state();
                    } else {
                        self.cursor_state().restore_cursor_state();
                    }
                }
                _ => {} // Unknown DEC mode
            }
        }
    }

    /// SELECTION CUSTODY Phase 3 — record the MAIN grid's absolute row counter at
    /// the instant this batch parks it (smcup), for the SCR-1 epilogue's re-pin.
    ///
    /// The epilogue used to re-pin the parked grid with `lines_added = 0`, on the
    /// stated invariant that "a grid that has been swapped out stopped receiving
    /// output". It stopped receiving it only AFTER the swap: a read that delivers
    /// `"a\r\nb\r\nc\r\n\x1b[?1049h"` — job output followed by an app's smcup, and
    /// batch boundaries are just `read()` boundaries — pushed three lines into the
    /// MAIN grid's scrollback first. Dropping that advance left the restored reading
    /// position three rows off the line the user was reading, while the selection
    /// (whose `content_scroll_delta` IS drained on the exit batch) moved with the
    /// content, so highlight and viewport disagreed.
    ///
    /// Called before the swap, while `self.grid` is still the main grid.
    fn park_main_row_counter(&mut self) {
        self.transient.alt_park_main_row_counter = Some(self.grid.absolute_row_counter());
    }

    /// SELECTION CUSTODY Phase 3 — take the parked reading position OFF a grid that
    /// has just been swapped back in mid-batch, and hand it to the SCR-1 epilogue.
    ///
    /// A parked main grid carries the user's `display_offset` (that is how the
    /// reading position survives a pager), and an rmcup makes it active again with
    /// the rest of the batch still to process. Every write and erase after that
    /// point goes through `row_index`, which SUBTRACTS `display_offset` — so
    /// `\x1b[?1049l\r\x1b[KHELLO\n` in one read erased and wrote 40 rows back in the
    /// user's history instead of on the live screen, recorded its damage band
    /// against the LIVE rows it never touched, and tripped `row_index_base`'s
    /// `debug_assert_eq!(display_offset, 0)` on the first region scroll.
    ///
    /// This is the batch prologue's rule applied at the second place a grid can
    /// become active: processing runs at offset 0, and the epilogue re-pins.
    fn flatten_restored_display_offset(&mut self) {
        // Every exit path funnels here, so this is also the one place that learns the
        // alt screen was left — `post_process` needs it for a batch that exits and
        // re-enters, where its start/end comparison sees no change at all.
        self.transient.alt_screen_left_in_batch = true;
        let restored = self.grid.display_offset();
        if restored > 0 {
            self.grid.scroll_to_bottom();
            self.transient.alt_restore_pin = Some((restored, self.grid.absolute_row_counter()));
        }
        // The rest of the batch writes against THIS grid now, so it owes the same
        // precondition the batch prologue forces on the one it found active.
        debug_assert_eq!(
            self.grid.display_offset(),
            0,
            "a grid swapped in mid-batch must process at offset 0"
        );
    }

    /// Enter alternate screen for mode 47/1047 — buffer swap only, no cursor
    /// save, no clear.
    fn enter_alternate_screen_raw(&mut self) {
        if self.modes.alternate_screen {
            return;
        }

        self.kitty_keyboard.switch_screen(true);
        // Clear hyperlink state — hyperlinks should not leak across screen
        // boundaries. A hyperlink opened on main should not apply to alt
        // screen text and vice versa. (#7414)
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        // Clear pending Sixel image — a Sixel rendered on main should not
        // leak into the alt screen context (#7484).
        #[cfg(feature = "sixel")]
        {
            self.sixel.pending_image = None;
        }
        // Per xterm there is one persistent alternate buffer: modes 47/1047
        // never clear it on entry (only 1049 does), so content from a
        // previous alt session must survive re-entry. Reuse the buffer kept
        // by the last exit, or allocate one lazily. Alt screen has no
        // scrollback per xterm spec — lines scrolled off the top of the
        // alternate buffer are discarded, not accumulated.
        let mut new_grid = self
            .alt_grid
            .take()
            .unwrap_or_else(|| Grid::with_scrollback(self.grid.rows(), self.grid.cols(), 0));
        // Per xterm: tab stops are global, shared between main and alt screens.
        // Copy main screen tab stops to the new alt screen (#7494).
        new_grid.restore_tab_stops(self.grid.tab_stops());
        // Per xterm the cursor is shared by both screen buffers: modes 47 and
        // 1047 swap the buffer without moving it (only 1048/1049 save/restore).
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        new_grid.set_pending_wrap(self.grid.pending_wrap());
        // Per xterm DECSTBM/DECSLRM margins live on the shared TScreen
        // (top_marg/bot_marg/lft_marg/rt_marg) — they persist across buffer
        // switches rather than belonging to either buffer.
        Self::copy_margins(self.grid, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
        if let Some(cb) = self.buffer_activation_callback {
            cb(true);
        }
    }

    /// Copy the scroll region and horizontal margins from one grid to
    /// another. Per xterm, DECSTBM/DECSLRM margins are fields of the shared
    /// `TScreen` (top_marg/bot_marg/lft_marg/rt_marg): switching between the
    /// main and alternate buffers (modes 47/1047/1049) does not reset them.
    fn copy_margins(from: &Grid, to: &mut Grid) {
        let region = from.scroll_region();
        to.set_scroll_region(region.top, region.bottom);
        let margins = from.horizontal_margins();
        to.set_horizontal_margins(margins.left, margins.right);
    }

    /// Exit alternate screen for mode 47 — buffer swap only, no cursor
    /// restore, no clear.
    fn exit_alternate_screen_raw(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }

        self.kitty_keyboard.switch_screen(false);
        // Clear hyperlink state on screen exit (#7414).
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        // Clear pending Sixel image — a Sixel rendered on alt should not
        // leak back to the main screen context (#7484).
        #[cfg(feature = "sixel")]
        {
            self.sixel.pending_image = None;
        }
        // Per xterm: tab stops are global. Preserve alt screen tab stop
        // changes back to the main screen (#7494).
        let tab_stops = self.grid.tab_stops().to_vec();
        if let Some(mut main_grid) = self.alt_grid.take() {
            // Per xterm the cursor is shared by both screen buffers: modes 47
            // and 1047 exit performs no cursor restore — the cursor stays
            // where the alt screen left it (only 1048/1049 save/restore).
            let cursor = self.grid.cursor();
            main_grid.set_cursor(cursor.row, cursor.col);
            main_grid.set_pending_wrap(self.grid.pending_wrap());
            // Margins are shared TScreen state in xterm: whatever DECSTBM/
            // DECSLRM set while in the alt screen stays in force after exit.
            Self::copy_margins(self.grid, &mut main_grid);
            // Keep the alternate buffer: it is persistent in xterm and mode
            // 47 exit does not clear it — a later re-entry shows it again.
            let alt = std::mem::replace(self.grid, main_grid);
            *self.alt_grid = Some(alt);
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops);
        self.modes.alternate_screen = false;
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
        if let Some(cb) = self.buffer_activation_callback {
            cb(false);
        }
    }

    /// Exit alternate screen for mode 1047 — clear alt screen before switching
    /// back. No cursor restore.
    fn exit_alternate_screen_1047(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }

        // Clear the alt screen (current grid) before switching back.
        // Set BCE template so erased cells inherit SGR background (#7522).
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.erase_screen();
        // Per xterm 1047 reset is otherwise identical to mode 47: a buffer
        // swap with no cursor save/restore.
        self.exit_alternate_screen_raw();
    }

    /// Enter alternate screen for mode 1049 — save cursor + clear alt screen
    /// on enter.
    ///
    /// SPEC: also the real `Enter` action of the external `AltScreen.tla` model
    /// (TRUST_NATIVE_TLA Phase 2, terminal-emulator CORRECTNESS family). It saves the
    /// cursor (`savedCursor' = cursor`), switches the active buffer to alt
    /// (`active' = "alt"`), and clears alt to blanks (`altCell' = Blank`) — exactly
    /// the spec's `Enter`. Its lossless round-trip with `Leave`
    /// (`MainRestoredAfterRoundTrip`) is Tier-1 conformance-checked by driving the
    /// real `Terminal` through `process(b"\x1b[?1049h … \x1b[?1049l")`
    /// (`aterm-core/tests/conformance_altscreen.rs`).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetAlternateScreen",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    // PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): `conformance_altscreen.rs`
    // projects the real `Terminal` onto the spec's `<<active, mainCell, altCell,
    // cursor, savedCursor, entered, mainSaved>>` (named `aterm_core::terminal::
    // project_altscreen`). L2 requires the projection NAME be present; Trust does not
    // execute it (the per-transition ty validation is the aterm-side Tier-1 binding).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "alt_screen",
            action = "Enter",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    fn enter_alternate_screen(&mut self) {
        if self.modes.alternate_screen {
            return;
        }

        // Save main cursor state, swap to fresh alt grid.
        // xterm srm_OPT_ALTBUF_CURSOR SET does CursorSave(xw) into the SAME
        // per-buffer slot DECSC uses (screen->sc[whichBuf]) — 1049's save IS
        // the DECSC save, observable by a later bare DECRC on the main
        // screen restoring the position 1049 saved.
        self.cursor_save.main = Some(self.snapshot_cursor_state());
        self.kitty_keyboard.switch_screen(true);
        // Clear pending Sixel image to prevent in-progress images from
        // leaking into the alt screen context (#7469).
        #[cfg(feature = "sixel")]
        {
            self.sixel.pending_image = None;
        }
        // Clear hyperlink state — hyperlinks should not leak across screen
        // boundaries (#7451).
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        // Alt screen has no scrollback per xterm spec — lines scrolled off the top
        // of the alternate buffer are discarded, not accumulated.
        let mut new_grid = Grid::with_scrollback(self.grid.rows(), self.grid.cols(), 0);
        // Honor background-color-erase (BCE): xterm's ClearScreen on 1049-enter
        // fills the alt screen with the CURRENT SGR background, just like every
        // other clear. A fresh Grid is default-bg blank, so set the BCE template
        // and erase to apply the active background (#7522 parity with the
        // 1047-exit clear in exit_alternate_screen_1047).
        new_grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        new_grid.erase_screen();
        // Per xterm: tab stops are global, shared between main and alt screens.
        // Copy main screen tab stops to the new alt screen (#7494).
        new_grid.restore_tab_stops(self.grid.tab_stops());
        // Per xterm the cursor is shared by both screen buffers and 1049 SET
        // is CursorSave + ToAlternate + ClearScreen — none of which moves the
        // cursor (srm_OPT_ALTBUF_CURSOR). Entering must NOT home it.
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        new_grid.set_pending_wrap(self.grid.pending_wrap());
        // Margins are shared TScreen state in xterm — they persist onto the
        // alternate screen.
        Self::copy_margins(self.grid, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
        if let Some(cb) = self.buffer_activation_callback {
            cb(true);
        }
    }

    /// Exit alternate screen for mode 1049 — restore cursor on exit.
    ///
    /// SPEC: also the real `Leave` action of the external `AltScreen.tla` model. It
    /// switches the active buffer back to main (`active' = "main"`) and RESTORES the
    /// cursor to the saved value (`cursor' = savedCursor`) — exactly the spec's
    /// `Leave`. The whole point of `MainRestoredAfterRoundTrip` is that this restore
    /// is not dropped and the alt scribbles never aliased main; Tier-1 conformance
    /// drives the real round-trip and validates it.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetAlternateScreen",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    // PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): the same `Terminal` →
    // `<<active, mainCell, altCell, cursor, savedCursor, entered, mainSaved>>`
    // projection as `Enter` (`aterm_core::terminal::project_altscreen`); L2 requires
    // a non-empty projection NAME, executed by `conformance_altscreen.rs` (Tier-1).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "alt_screen",
            action = "Leave",
            project = "aterm_core::terminal::project_altscreen"
        )
    )]
    fn exit_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }

        // Swap back to the main grid. xterm 1049 RESET (FromAlternate +
        // CursorRestore) does NOT save the alt-screen cursor anywhere.
        self.kitty_keyboard.switch_screen(false);
        // Clear pending Sixel image to prevent in-progress images from
        // leaking back to the main screen context (#7469).
        #[cfg(feature = "sixel")]
        {
            self.sixel.pending_image = None;
        }
        // Clear hyperlink state on screen exit (#7451).
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();
        // Per xterm: tab stops are global. Preserve alt screen tab stop
        // changes back to the main screen (#7494).
        let tab_stops = self.grid.tab_stops().to_vec();
        if let Some(mut main_grid) = self.alt_grid.take() {
            // Margins are shared TScreen state in xterm: whatever DECSTBM/
            // DECSLRM set while in the alt screen stays in force after exit.
            Self::copy_margins(self.grid, &mut main_grid);
            *self.grid = main_grid;
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops);
        // Restore from the shared DECSC slot WITHOUT consuming it: xterm
        // CursorRestore leaves sc->saved set, so a later bare DECRC restores
        // the same state again.
        let saved = self.cursor_save.main;
        self.restore_cursor_snapshot(saved);
        self.modes.alternate_screen = false;
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
        if let Some(cb) = self.buffer_activation_callback {
            cb(false);
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetBracketedPaste",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_bracketed_paste(&mut self) {
        self.modes.bracketed_paste = true;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetBracketedPaste",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_bracketed_paste(&mut self) {
        self.modes.bracketed_paste = false;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_mouse_x10_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::X10;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_mouse_normal_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::Normal;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_mouse_button_event_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::ButtonEvent;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_mouse_any_event_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::AnyEvent;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetMouseMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_mouse_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::None;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetSgrMouseEncoding",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_sgr_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Sgr;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetSgrMouseEncoding",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_sgr_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Sgr {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    /// Capture current cursor position, style, and mode flags.
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

    /// Restore cursor position, style, and mode flags from a snapshot (if any).
    ///
    /// When origin mode is restored as enabled, the cursor row is clamped to
    /// the current scroll region — matching the DECRC behavior in
    /// `restore_cursor_state`. Without this, a cursor saved outside the
    /// scroll region would be placed outside it on restore, violating VT510.
    fn restore_cursor_snapshot(&mut self, state: Option<SavedCursorState>) {
        if let Some(state) = state {
            // Restore modes first so origin_mode is known for clamping.
            // DECAWM is deliberately NOT restored — 1049 exit restores "as in
            // DECRC", and xterm's CursorRestoreFlags only applies DECSC_FLAGS
            // = (ATTRIBUTES|ORIGIN|PROTECTED) (cursor.c): WRAPAROUND is never
            // part of the saved-cursor state.
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            self.transient.current_underline_color = state.underline_color;
            // Refresh the cached extras flag — the restored underline_color
            // may differ from the pre-restore value (#7311).
            self.transient.update_has_transient_extras();
            // Update BCE cursor template from restored style's background color.
            // Without this, the first scroll after mode 1049 exit uses the alt
            // screen's cursor template, producing wrong-colored blank lines.
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );

            // Clamp cursor to scroll region when origin mode is active.
            // Per VT510: when DECOM is active, cursor is clamped to the scroll
            // region vertically and to horizontal margins when DECLRMM is active.
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
            self.grid.set_cursor(row, col);
            // Restore pending_wrap after set_cursor (which clears it) (#7283).
            // Only restore if cursor is still at the right edge — after terminal
            // resize the saved position may no longer be at the margin, making
            // a deferred wrap invalid (#7645).
            if state.pending_wrap {
                let max_col = self.grid.effective_cols_for_row(row).saturating_sub(1);
                self.grid.set_pending_wrap(col >= max_col);
            }
            // Update BCE cursor template from restored style so that the first
            // scroll after returning to the main screen uses the correct
            // background colors (#7658).
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }
    }

    /// Handle DECRQM - DEC Request Mode.
    ///
    /// CSI ? Ps $ p - Request DEC private mode state.
    /// Response: CSI ? Ps ; Pm $ y
    /// Where Pm is:
    ///   0 - Not recognized (mode not known)
    ///   1 - Set (mode is enabled)
    ///   2 - Reset (mode is disabled)
    ///   3 - Permanently set
    ///   4 - Permanently reset
    pub(super) fn handle_decrqm(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let mode = params.first().copied().unwrap_or(0);

        // DECRQM state values: 1=set, 2=reset, 0=unknown
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
            12 => state(self.modes.cursor_blink),
            25 => state(self.modes.cursor_visible),
            40 => state(self.modes.deccolm_enable),
            45 => state(self.modes.reverse_wraparound),
            66 => state(self.modes.application_keypad),
            67 => state(self.modes.backarrow_sends_bs),
            69 => state(self.modes.left_right_margin_mode),
            // xterm keyboard private modes (numLock / metaSendsEscape /
            // altSendsEscape) and the tracked-only mode 1045.
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
                // Mode 1048 is an action (save/restore cursor), not a tracked mode.
                // Report whether a cursor state has been saved for the current screen.
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
            2027 => state(self.modes.grapheme_cluster_mode),
            2031 => state(self.modes.report_color_scheme),
            // 2048 (in-band size reports) is now implemented (emits on enable +
            // every resize), so DECRQM reports its real state honestly.
            2048 => state(self.modes.in_band_size_reports),
            2500 => state(self.modes.bidi_box_mirroring),
            2501 => state(self.modes.bidi_autodetection),
            // Recognized but permanently reset: modes with no effect in a modern
            // terminal emulator.  Pm=4 is more spec-correct than Pm=0 (unknown).
            4 | 8 => 4,   // DECSCLM (smooth scroll), DECARM (auto repeat) — OS-managed
            18 | 19 => 4, // DECPFF (print form feed), DECPEX (print extent) — no printer
            _ => 0,       // Unknown mode
        };

        // Send response: CSI ? <mode> ; <state> $ y
        // Build off the heap like the sibling DA/DSR/CPR probe responders.
        // mode is u16 (≤5 digits), mode_state is single-digit (0-4), so the
        // worst case `ESC[?` + 5 + `;` + 1 + `$y` = 12 bytes fits in 16.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle ANSI DECRQM — Request Mode (non-private).
    ///
    /// CSI Ps $ p — Report current state of an ANSI (non-DEC-private) mode.
    /// Response: CSI Ps ; Pm $ y  (no `?` prefix — this is ANSI, not DEC)
    /// Where Pm is: 0=not recognized, 1=set, 2=reset, 3=perm set, 4=perm reset.
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
            // SRM (Send/Receive Mode) — local echo control, always full-duplex
            // (permanently reset) in a modern terminal.  Pm=4 is more spec-correct
            // than Pm=0 (unknown).
            12 => 4,
            _ => 0, // Unknown ANSI mode
        };

        // Response format: CSI <mode> ; <state> $ y (no `?` — ANSI, not DEC).
        // Allocation-free build, matching the sibling probe responders. Same
        // bound as DEC DECRQM minus the `?`, so 16 bytes is ample.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[{mode};{mode_state}$y");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle ANSI mode set/reset.
    ///
    /// CSI Ps h - Set Mode
    /// CSI Ps l - Reset Mode
    ///
    /// Standard ANSI modes:
    /// - 4: Insert Mode (IRM) - when set, characters shift existing text right
    /// - 8: BDSM (BiDi Mode) - when set, implicit BiDi; when reset, explicit BiDi
    /// - 20: Line Feed/New Line Mode (LNM) - when set, LF also does CR
    ///
    /// See Terminal WG BiDi spec: <https://terminal-wg.pages.freedesktop.org/bidi/>
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
                    // BDSM - Bidirectional Support Mode
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

    /// Parse rectangular area coordinates (Pt;Pl;Pb;Pr) for VT420 rect ops.
    ///
    /// Handles 1-indexed to 0-indexed conversion, default-value resolution,
    /// and DECOM (origin mode) offset. Per DEC STD 070 Section 5.5.2,
    /// when DECOM is set, coordinates are relative to the scroll region
    /// and horizontal margins.
    ///
    /// Returns `Some((top, left, bottom, right))` with 0-indexed absolute
    /// coordinates, or `None` if the rectangle is invalid (top > bottom
    /// or left > right).
    fn parse_rect_coords(&self, params: &[u16]) -> Option<(u16, u16, u16, u16)> {
        let rows = self.grid.rows();
        let cols = self.grid.cols();

        // When DECOM is active, coordinates are relative to the scroll region
        // (and horizontal margins when DECLRMM is active).
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

        // Parse parameters (1-indexed, convert to 0-indexed within origin)
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

    /// Handle DECERA - Erase Rectangular Area (VT420+).
    ///
    /// CSI Pt ; Pl ; Pb ; Pr $ z
    ///
    /// Erases all characters in the rectangular area defined by:
    /// - Pt: top row (1-indexed, default: 1)
    /// - Pl: left column (1-indexed, default: 1)
    /// - Pb: bottom row (1-indexed, default: number of rows)
    /// - Pr: right column (1-indexed, default: number of columns)
    ///
    /// Per VT420 spec, parameter 0 is treated as the default value.
    /// When DECOM is active, coordinates are relative to scroll region.
    /// The erase fills cells with spaces using default attributes.
    pub(super) fn handle_decera(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        // Erase the rectangular area
        self.grid.erase_rect(top, left, bottom, right);
    }

    /// Handle DECCARA - Change Attributes in Rectangular Area (VT420+).
    ///
    /// CSI Pt ; Pl ; Pb ; Pr ; Ps... $ r
    ///
    /// Applies SGR attributes (Ps parameters) to all characters in the
    /// rectangular area defined by:
    /// - Pt: top row (1-indexed, default: 1)
    /// - Pl: left column (1-indexed, default: 1)
    /// - Pb: bottom row (1-indexed, default: number of rows)
    /// - Pr: right column (1-indexed, default: number of columns)
    /// - Ps...: SGR attribute parameters (from params[4..])
    ///
    /// Per VT420 spec, parameter 0 is treated as the default value.
    /// Only a subset of SGR attributes are supported: bold, dim, italic,
    /// underline, blink, inverse, hidden, strikethrough, and their resets.
    ///
    /// When DECSACE stream mode is active (`stream_attribute_extent`), the
    /// operation covers a contiguous character stream from (top,left) to
    /// (bottom,right) instead of a rectangle.
    pub(super) fn handle_deccara(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        // Parse SGR parameters from params[4..]
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

    /// Handle DECRARA - Reverse Attributes in Rectangular Area (VT400+).
    ///
    /// CSI Pt ; Pl ; Pb ; Pr ; Pm $ t
    ///
    /// xterm ctlseqs: "Reverse Attributes in Rectangular Area (DECRARA), VT400
    /// and up. Pt ; Pl ; Pb ; Pr denotes the rectangle. Pm denotes the
    /// attributes to reverse, i.e., 0, 1, 4, 5, 7, 8. Reversing SGR 0 reverses
    /// modes 1, 4, 5, 7. Reversing SGR 8 is an xterm extension. See DECSACE."
    ///
    /// The reverse half of the DECCARA pair: same rectangle parsing (DECOM-aware,
    /// 0 means the default per VT420) and the same DECSACE extent choice, but
    /// each named attribute is reversed per cell, so the outcome depends on what
    /// each cell already carried. DA1 advertises code 28 (rectangular editing),
    /// which is the claim this arm has to make true.
    pub(super) fn handle_decrara(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        // Pm defaults to 0 — "reverse modes 1, 4, 5, 7" — exactly as DECCARA's
        // attribute list defaults to SGR 0.
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

    /// Split a DECRARA `Pm` list into (single-bit flags to XOR, reverse-underline).
    ///
    /// Underline comes back separately because it is not one bit: `UNDERLINE`,
    /// `DOUBLE_UNDERLINE` and `CURLY_UNDERLINE` combine to spell dotted and
    /// dashed, so only the grid can decide it per cell (`reverse_cell_attrs`).
    ///
    /// Codes outside the list DECRARA names (0, 1, 4, 5, 7, 8) are ignored
    /// rather than mapped onto DECCARA's wider set: an SGR reset code like 22 has
    /// no defined reverse, and reversing an attribute the application never named
    /// would corrupt cells it asked to leave alone.
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

    /// Handle DECCRA - Copy Rectangular Area (VT420+).
    ///
    /// CSI Pts ; Pls ; Pbs ; Prs ; Pps ; Ptd ; Pld ; Ppd $ v
    ///
    /// Copies the rectangular area from source page to destination:
    /// - Pts: source top row (1-indexed, default: 1)
    /// - Pls: source left column (1-indexed, default: 1)
    /// - Pbs: source bottom row (1-indexed, default: number of rows)
    /// - Prs: source right column (1-indexed, default: number of columns)
    /// - Pps: source page (ignored - single page)
    /// - Ptd: destination top row (1-indexed, default: 1)
    /// - Pld: destination left column (1-indexed, default: 1)
    /// - Ppd: destination page (ignored - single page)
    ///
    /// Per VT420 spec, parameter 0 is treated as the default value.
    pub(super) fn handle_deccra(&mut self, params: &[u16]) {
        // Parse source rectangle (params[0..4]) with DECOM support
        let Some((src_top, src_left, src_bottom, src_right)) = self.parse_rect_coords(params)
        else {
            return;
        };
        // params[4] = source page (ignored)

        // Parse destination coordinates (params[5..6]) with DECOM offset
        let dst_params = if params.len() > 5 {
            &params[5..]
        } else {
            &[1u16][..]
        };
        // Reuse parse_rect_coords for destination: only top-left matters,
        // bottom-right are derived from source rectangle dimensions.
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
        // params[7] = destination page (ignored)

        self.grid
            .copy_rect(src_top, src_left, src_bottom, src_right, dst_top, dst_left);
    }

    /// Convert a slice of SGR parameter values into (flags_to_set, flags_to_clear).
    ///
    /// Processes the SGR subset relevant to DECCARA:
    /// bold, dim, italic, underline, blink, inverse, hidden, strikethrough,
    /// and their corresponding reset codes.
    fn sgr_params_to_flags(sgr_params: &[u16]) -> (CellFlags, CellFlags) {
        let mut flags_to_set = CellFlags::empty();
        let mut flags_to_clear = CellFlags::empty();

        for &param in sgr_params {
            match param {
                0 => {
                    // SGR 0 = reset all SGR attributes.
                    // Must NOT clear structural WIDE (bit 9) or WIDE_CONTINUATION (bit 10)
                    // flags — those track cell geometry, not visual attributes.
                    // VISUAL_FLAGS_MASK (0x3FFF) includes those bits; use the
                    // narrower SGR_ATTRIBUTE_MASK that excludes them.
                    flags_to_clear = CellFlags::from_bits(CellFlags::VISUAL_FLAGS_MASK & !0x0600);
                    flags_to_set = CellFlags::empty();
                }
                1 => flags_to_set = flags_to_set.union(CellFlags::BOLD),
                2 => flags_to_set = flags_to_set.union(CellFlags::DIM),
                3 => flags_to_set = flags_to_set.union(CellFlags::ITALIC),
                4 => {
                    // SGR 4 = single underline — must clear other underline
                    // styles first, matching apply_sgr_param behavior (#7464).
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
                    // SGR 24 = remove underline — must clear ALL underline
                    // styles, matching apply_sgr_param behavior (#7464).
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

    /// Handle DECFRA - Fill Rectangular Area (VT420+).
    ///
    /// CSI Pch ; Pt ; Pl ; Pb ; Pr $ x
    ///
    /// Fills the rectangular area with character Pch:
    /// - Pch: character code to fill (default: none / no-op)
    /// - Pt: top row (1-indexed, default: 1)
    /// - Pl: left column (1-indexed, default: 1)
    /// - Pb: bottom row (1-indexed, default: number of rows)
    /// - Pr: right column (1-indexed, default: number of columns)
    ///
    /// Per VT420 spec, only printable characters (0x20..=0x7E and 0xA0..=0xFF)
    /// are accepted. Non-printable character codes are silently ignored.
    /// Parameter 0 is treated as the default value for coordinates.
    pub(super) fn handle_decfra(&mut self, params: &[u16]) {
        // First parameter is the character code
        let ch_code = params.first().copied().unwrap_or(0);

        // Per VT420 spec, only printable characters are accepted
        let printable = matches!(ch_code, 0x20..=0x7E | 0xA0..=0xFF);
        if !printable {
            return;
        }

        // Construct fill cell with current SGR attributes (per VT420 spec,
        // DECFRA fills with the specified character using current video attrs).
        // ch_code is in 0x20..=0x7E or 0xA0..=0xFF (validated above), fits in u16.
        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let fill = crate::grid::Cell::from_raw_parts(ch_code, colors, flags);

        // Truecolor fg/bg are stored out-of-line: `cached_colors()` only sets
        // the RGB *markers*, so resolve the actual bytes and hand them to
        // fill_rect, which repopulates the CellExtras RGB ring (otherwise the
        // filled cells report bg_is_rgb()/fg_is_rgb() but read back no color).
        // Indexed/default colors are inline in the cell, so these stay None.
        let fg_rgb = self.style.fg.is_rgb().then(|| {
            let (r, g, b) = self.style.fg.rgb_components();
            [r, g, b]
        });
        let bg_rgb = self.style.bg.is_rgb().then(|| {
            let (r, g, b) = self.style.bg.rgb_components();
            [r, g, b]
        });

        // Parse rectangle coordinates from params[1..5] with DECOM support.
        let rect_params = if params.len() > 1 { &params[1..] } else { &[] };
        let Some((top, left, bottom, right)) = self.parse_rect_coords(rect_params) else {
            return;
        };

        self.grid
            .fill_rect(fill, top, left, bottom, right, fg_rgb, bg_rgb);
    }

    /// Handle DECSERA - Selective Erase Rectangular Area (VT420+).
    ///
    /// CSI Pt ; Pl ; Pb ; Pr $ {
    ///
    /// Erases characters in the rectangular area that are NOT protected by DECSCA:
    /// - Pt: top row (1-indexed, default: 1)
    /// - Pl: left column (1-indexed, default: 1)
    /// - Pb: bottom row (1-indexed, default: number of rows)
    /// - Pr: right column (1-indexed, default: number of columns)
    ///
    /// Per VT420 spec, parameter 0 is treated as the default value.
    /// Protected cells (set via DECSCA) are preserved.
    pub(super) fn handle_decsera(&mut self, params: &[u16]) {
        let Some((top, left, bottom, right)) = self.parse_rect_coords(params) else {
            return;
        };

        self.grid.selective_erase_rect(top, left, bottom, right);
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

    #[test]
    fn decrqm_reports_mode_67_state() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?67$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?67;2$y");
        term.process(b"\x1b[?67h");
        term.process(b"\x1b[?67$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?67;1$y");
    }
}

#[cfg(test)]
mod xterm_keyboard_mode_tests {
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, encode_key};

    // --- DECSET/DECRST -> KeyboardMode projection ---

    #[test]
    fn mode_1039_alt_send_escape_default_set() {
        let term = Terminal::new(24, 80);
        // altSendsEscape is ON by default, so ALT_NO_ESC must be absent.
        assert!(!term.keyboard_mode().contains(KeyboardMode::ALT_NO_ESC));
    }

    #[test]
    fn mode_1039_reset_sets_alt_no_esc_flag() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?1039l");
        assert!(term.keyboard_mode().contains(KeyboardMode::ALT_NO_ESC));
        term.process(b"\x1b[?1039h");
        assert!(!term.keyboard_mode().contains(KeyboardMode::ALT_NO_ESC));
    }

    #[test]
    fn mode_1036_meta_send_escape_toggles_flag() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.keyboard_mode().contains(KeyboardMode::META_SENDS_ESC));
        term.process(b"\x1b[?1036h");
        assert!(term.keyboard_mode().contains(KeyboardMode::META_SENDS_ESC));
        term.process(b"\x1b[?1036l");
        assert!(!term.keyboard_mode().contains(KeyboardMode::META_SENDS_ESC));
    }

    #[test]
    fn mode_1035_special_modifiers_toggles_flag() {
        let mut term = Terminal::new(24, 80);
        // numLock special modifiers ON by default -> NO_SPECIAL_MODIFIERS absent.
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

    #[test]
    fn mode_1045_tracked_but_does_not_affect_encoding() {
        let mut term = Terminal::new(24, 80);
        let before = term.keyboard_mode();
        term.process(b"\x1b[?1045h");
        // Mode 1045 is tracked-only: the encoder-facing KeyboardMode is unchanged.
        assert_eq!(term.keyboard_mode(), before);
    }
}

#[cfg(test)]
mod in_band_size_tests {
    use crate::terminal::Terminal;

    #[test]
    fn mode_2048_emits_on_enable_with_default_cell_px() {
        let mut term = Terminal::new(24, 80);
        // Default cell px is 8x16 -> pixH = 24*16 = 384, pixW = 80*8 = 640.
        term.process(b"\x1b[?2048h");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;384;640t"
        );
    }

    #[test]
    fn mode_2048_repeated_enable_emits_once() {
        // `?2048h` is idempotent (xterm/ghostty semantics): only the disabled->
        // enabled transition emits. A `?2048h` flood must NOT emit a report per
        // sequence (that was an uncapped response-buffer amplification DoS).
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        for _ in 0..10_000 {
            input.extend_from_slice(b"\x1b[?2048h");
        }
        term.process(&input);
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[48;24;80;384;640t",
            "10k repeated enables emit exactly one report"
        );
    }

    #[test]
    fn mode_2048_toggle_flood_is_capped() {
        // A `?2048l?2048h` toggle flood defeats the transition guard (each `h`
        // is a real re-enable), so the emit must also honor the response-buffer
        // cap — this path writes the buffer directly, not via send_response.
        let mut term = Terminal::new(24, 80);
        let mut input = Vec::new();
        for _ in 0..200_000 {
            input.extend_from_slice(b"\x1b[?2048l\x1b[?2048h");
        }
        term.process(&input);
        let resp = term.take_response().unwrap_or_default();
        // Uncapped this would be ~200k * 19 = ~3.8 MB; the cap bounds it to 1 MiB.
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

    #[test]
    fn no_report_when_2048_disabled() {
        let mut term = Terminal::new(24, 80);
        term.resize(30, 100);
        assert!(
            term.take_response().is_none(),
            "no in-band report unless mode 2048 is set"
        );
    }

    #[test]
    fn decrqm_reports_mode_2048_state() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2048$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?2048;2$y");
        term.process(b"\x1b[?2048h");
        let _ = term.take_response(); // enable report
        term.process(b"\x1b[?2048$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?2048;1$y");
    }
}

include!("handler_dec_refinement.rs");
