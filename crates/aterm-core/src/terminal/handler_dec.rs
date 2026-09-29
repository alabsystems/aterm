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
                // ConPTY win32-input-mode (microsoft/terminal spec #4999). conhost
                // requests it at every ConPTY start; until it was honoured the
                // request fell into the unknown-mode arm below, so no key could
                // ever carry a Windows modifier and aterm's legacy Shift+Enter LF
                // reached PowerShell as Ctrl+Enter (measured 2026-09-22). Folded
                // into the keyboard encoding as `KeyboardMode::WIN32_INPUT`; the
                // encoder decides which keys need a win32 record (Enter chords)
                // and which stay legacy VT — conhost accepts the mixed stream.
                9001 => {
                    self.modes.win32_input_mode = set;
                }
                _ => {} // Unknown DEC mode
            }
            // The foreground handback's attribution: a setter parsed now is
            // this holder's, even over a mode already in force.
            *self.evidence_asserted |= super::program_evidence::asserted_by_dec_mode(param, set);
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
            // The batch prologue's entry point, for the batch prologue's reason: this
            // forced 0 is the machine's, undone by the epilogue's re-pin, and must not
            // register as a reader gesture in `reader_live_bottom_gen`. See
            // `Terminal::process`'s prologue.
            self.grid.pin_viewport_to_live_for_output_batch();
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
        // A parked alt buffer was resized with the primary; a flap it took while
        // parked is not the screen the app is about to see, so its resize undo
        // (`Grid::drop_resize_undo`) must not hand rows back into it.
        new_grid.drop_resize_undo();
        // Per xterm: tab stops are global, shared between main and alt screens.
        // Copy main screen tab stops to the new alt screen (#7494).
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
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
        self.note_screen_replaced();
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
    }

    /// Count a whole-screen replacement for the resize journal's readers
    /// (`Terminal::screen_replaced_count`): the screen the app painted before it
    /// is gone, so a displacement a resize left on it is gone too.
    fn note_screen_replaced(&mut self) {
        self.transient.screen_replaced = self.transient.screen_replaced.wrapping_add(1);
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
        let tab_suppressed = self.grid.tab_defaults_suppressed();
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
            let mut alt = std::mem::replace(self.grid, main_grid);
            alt.drop_resize_undo();
            *self.alt_grid = Some(alt);
            // The whole visible surface just changed — see
            // `exit_alternate_screen`'s note on why the restored grid is
            // marked fully damaged.
            self.grid.damage_mut().mark_full();
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops, tab_suppressed);
        self.modes.alternate_screen = false;
        self.note_screen_replaced();
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
    }

    /// Exit alternate screen for mode 1047 — clear alt screen before switching
    /// back. No cursor restore.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            // A SECOND 1049 SET IS NOT A NO-OP. xterm's arm is three calls —
            // `CursorSave(xw); ToAlternate(xw, True); ClearScreen(xw);`
            // (charproc.c `srm_OPT_ALTBUF_CURSOR`) — and only the MIDDLE one
            // stands down when the alt screen is already up (`ToAlternate`:
            // `if (screen->whichBuf == 0)`). The save still runs, into the
            // CURRENT buffer's slot (`screen->sc[whichBuf]`, the alt slot), so
            // the main slot that 1049 RESET restores is left alone; and the
            // clear still runs. Returning early here left a second smcup
            // showing the PREVIOUS app's screen — a full-screen app restarting,
            // or one that re-arms 1049 defensively, inherited the old content.
            self.cursor_save.alt = Some(self.snapshot_cursor_state());
            // `ClearScreen` is an ordinary BCE clear that ends in `ResetWrap`
            // and moves no cursor — the same shape the fresh-grid path below
            // uses for the first entry.
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
            self.grid.erase_screen();
            self.note_screen_replaced();
            return;
        }

        // Save main cursor state, swap to fresh alt grid.
        // xterm srm_OPT_ALTBUF_CURSOR SET does CursorSave(xw) into the SAME
        // per-buffer slot DECSC uses (screen->sc[whichBuf]) — 1049's save IS
        // the DECSC save, observable by a later bare DECRC on the main
        // screen restoring the position 1049 saved.
        self.cursor_save.main = Some(self.snapshot_cursor_state());
        self.transient.alt_entered_without_cursor_save = false;
        self.transient.conhost_alt_screen_left_up = false;
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
        new_grid.restore_tab_stops(self.grid.tab_stops(), self.grid.tab_defaults_suppressed());
        // Per xterm the cursor is shared by both screen buffers and 1049 SET
        // is CursorSave + ToAlternate + ClearScreen — none of which moves the
        // cursor (charproc.c `srm_OPT_ALTBUF_CURSOR`). Entering must NOT home it.
        // ClearScreen runs LAST and calls `ResetWrap` (util.c `ClearScreen`), so
        // the alt screen starts with NO pending wrap even when the main screen had
        // one: the wrap is deliberately not copied here (modes 47/1047, which do
        // not clear, still carry it over in `enter_alternate_screen_raw`). The
        // main screen's wrap is not lost — `snapshot_cursor_state` above saved it
        // (xterm `CursorSave` records `do_wrap`), and 1049 RESET restores it.
        let cursor = self.grid.cursor();
        new_grid.set_cursor(cursor.row, cursor.col);
        debug_assert!(
            !new_grid.pending_wrap(),
            "1049 SET ends in ClearScreen, which resets the deferred wrap"
        );
        // Margins are shared TScreen state in xterm — they persist onto the
        // alternate screen.
        Self::copy_margins(self.grid, &mut new_grid);
        self.park_main_row_counter();
        let old_grid = std::mem::replace(self.grid, new_grid);
        *self.alt_grid = Some(old_grid);
        self.modes.alternate_screen = true;
        self.note_screen_replaced();
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            self.keep_screen_from_conhost_repaint();
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
        let tab_suppressed = self.grid.tab_defaults_suppressed();
        if let Some(mut main_grid) = self.alt_grid.take() {
            // Margins are shared TScreen state in xterm: whatever DECSTBM/
            // DECSLRM set while in the alt screen stays in force after exit.
            Self::copy_margins(self.grid, &mut main_grid);
            *self.grid = main_grid;
            // THE RESTORED GRID IS A WHOLE-SCREEN CHANGE. Its damage tracker
            // was consumed by the last present before the program entered the
            // alt screen and has recorded nothing since, so without this mark
            // `has_damage()` read false and `damage_epoch()` did not advance
            // on the swap back — and every consumer that keys a repaint on the
            // epoch (both browser hosts' WF-1 frame gates) kept the ALT screen
            // on glass until some later write happened to damage the main
            // grid. Entering already marks full (the alt buffer is erased or
            // fresh); leaving must too.
            self.grid.damage_mut().mark_full();
        }
        self.flatten_restored_display_offset();
        self.grid.restore_tab_stops(&tab_stops, tab_suppressed);
        // Restore from the shared DECSC slot WITHOUT consuming it: xterm
        // CursorRestore leaves sc->saved set, so a later bare DECRC restores
        // the same state again.
        let saved = self.cursor_save.main;
        self.restore_cursor_snapshot(saved);
        self.modes.alternate_screen = false;
        self.note_screen_replaced();
        // SELECTION CUSTODY: bump the host-coordinate epoch on the INCOMING grid, but
        // do NOT record `SelectionDamage::All`. A switch no longer destroys the
        // outgoing screen's selection — `post_process` parks it — and the `All` this
        // used to record would clear it on the way back in.
        self.grid.invalidate_host_coordinates();
    }

    /// Leave an alternate screen NO APP IS BEHIND any more — the `?1049l` that
    /// should have ended it never came. A no-op on the primary screen.
    ///
    /// MEASURED (2026-09-22, Windows 11, ConPTY, the `cast` tap): `less` from Git
    /// for Windows entered with a lone `ESC[?1049h` frame; killed from another tab
    /// (`Stop-Process`), conhost sent NO `?1049l` and no repaint — the next bytes
    /// were pwsh's `133;D;-1`, `133;A`, its prompt and `133;B`, painted onto the
    /// pager rows. Commands typed there scrolled the alt grid, which has no
    /// scrollback, and a later `less` + `q` restored a main screen that never held
    /// them. A bare ConPTY probe reproduces it without aterm (2026-09-27):
    /// conhost forwards ANY client's switch, as `?1049h` plus its own full
    /// repaint of the new buffer — MSYS `printf`, cmd's `echo`, .NET's
    /// `Console.Out` alike — and a killed client leaves its alt buffer up for
    /// good. `Write-Host` from pwsh never switches only because PowerShell strips
    /// the sequence first: `$PSStyle.OutputRendering` read `PlainText` in an
    /// aterm tab. On unix the same happens to a full-screen app that gets
    /// SIGKILL: nothing emits rmcup on its behalf.
    ///
    /// Two callers name the same fact: the shell painting a PROMPT on the alt
    /// screen (`prompt_leaves_orphaned_alt_screen` — a prompt there is never
    /// intended, the shell believes it is on the main screen), and the host's
    /// explicit recovery (`Terminal::leave_alternate_screen`, the `mainscreen`
    /// verb) for a shell that paints no integration marks.
    ///
    /// In order: commit the alt grid as its final frame and flush it WHOLE to the
    /// archive (`AltArchiveState::leave_orphaned` — everything typed on the stuck
    /// screen stays readable through `offscreen`), then the exit the dead app's
    /// own reset would have been. After `?1049h`: the 1049 exit — the main grid,
    /// its scrollback and the cursor `?1049h` saved come back, so the prompt paints
    /// where it would have after a clean exit. After `?47h`/`?1047h`, which save no
    /// cursor: the 1047 exit — the cursor stays where the alt screen had it, as
    /// both modes' exits leave it, and the dead screen is blanked rather than kept
    /// for a later `?47h` to show again (its rows are in the archive). The 1049
    /// exit there would restore the main slot, which can still hold an earlier
    /// app's save. Then the modes the dead app owned go (`release_dead_app_modes`).
    ///
    /// Under conhost (`win32_input_mode`) the primary screen is laid out the way
    /// conhost sees it instead, because conhost never leaves ITS alternate buffer
    /// (`TransientState::conhost_alt_screen_left_up`): everything the shell prints
    /// from here on is conhost's alternate buffer, rendered with ABSOLUTE cursor
    /// moves. MEASURED (2026-09-27, 80x24, `less` killed after four history
    /// rows): with the prompt put back on the row `?1049h` saved (row 8) while
    /// conhost's cursor was on row 23, a command typed there that wrapped showed
    /// its head on the prompt's row and a second, prompt-less copy on rows 19-20
    /// (conhost re-anchors a wrap with `ESC[23;80H`, the `cast` tap); and a
    /// resize (`resize 24 90`, `resize 24 80`) made conhost repaint its whole
    /// alternate buffer over the primary screen, erasing the history rows the
    /// 1049 exit had put back on it (`search 'history row'`: no results). So the
    /// cursor stays where the alternate screen had it — conhost's cursor, both
    /// sides parsed the same bytes — and every used primary row goes into the
    /// scrollback first: a later conhost repaint can only overwrite rows conhost
    /// itself painted, and the history is out of its reach. The same two runs
    /// with this layout: the wrapped command is one line under its prompt, and
    /// after the resize every history row is in the scrollback, with the dead
    /// pager's rows repainted on the blank rows above the prompt.
    ///
    /// What stays wrong after that is conhost's, and no byte a terminal can send
    /// corrects it. MEASURED (2026-09-27, 80x24, the re-verifier's recipe: 40
    /// history rows, `less`, `Stop-Process`, two commands, `less` on win.ini,
    /// `q`; `conpty_dead_alt_screen_replay_tests` replays the reads): conhost's
    /// next `?1049h` REPLACES the dead buffer, and everything the shell printed
    /// on it goes with it; its `?1049l` returns to the primary buffer it froze
    /// when the dead app entered, repainted in full, with the cursor on the row
    /// that app's `?1049h` saved. So that exit shows the screen from before the
    /// kill, the rows printed since are in our scrollback just above it
    /// (`keep_screen_from_conhost_repaint`), and the frozen rows are in the
    /// history twice: once from this leave, once from that repaint. Our half of
    /// the ConPTY pipe carries input — keys, focus, the replies to conhost's
    /// queries — and none of it switches conhost's buffers; a console CLIENT
    /// writing `?1049l` does. `[Console]::Write` of it from the shell brought
    /// conhost back at once (its primary repainted, the shell's next prompt
    /// under it), and the next pager's exit then repainted the live screen; a
    /// separate process attached with `AttachConsole` that read the dead buffer,
    /// wrote `?1049l` and put the prompt's row and the cursor back on the
    /// primary did the same with PSReadLine's pending prompt left where it was.
    /// Painting our own rows over conhost's frozen repaint instead would hold
    /// only until conhost's next full repaint (any resize, the next full-screen
    /// exit), and rows taken off the scrollback to do it would be lost to it.
    pub(super) fn leave_orphaned_alternate_screen(&mut self) {
        if !self.modes.alternate_screen {
            return;
        }
        self.alt_archive
            .leave_orphaned(self.grid, self.transient.sync_end_seq);
        let conhost = self.modes.win32_input_mode;
        let conhost_cursor = (self.grid.cursor(), self.grid.pending_wrap());
        if self.transient.alt_entered_without_cursor_save {
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
        // Under conhost our leave is ours alone: its own alternate buffer stays
        // up, and its next switch repaints over this screen (the field has the
        // measurement). Unix has no such second screen to fall out of step with.
        self.transient.conhost_alt_screen_left_up = conhost;
    }

    /// The modes a dead full-screen app owned, which its own exit would have
    /// reset and the exits above carry over (xterm keeps them across a switch
    /// for a LIVE app): its scroll region and horizontal margins — the shell's
    /// output would scroll inside them and never reach the scrollback the leave
    /// brought back; mouse tracking — the wheel and clicks would go on feeding a
    /// dead app instead of scrolling that scrollback; application cursor keys
    /// and keypad — `less` sends `?1h` and `ESC =`, and conhost forwarded both
    /// (the `cast` tap, 2026-09-27); and an open 2026 window, which would hold
    /// every present until its timeout. The shell set none of them. Focus
    /// reporting stays (conhost asks for it at every ConPTY start, beside
    /// `?9001h`), and so do bracketed paste and the keyboard protocols, which a
    /// shell sets for itself — the kitty flags are per screen already.
    fn release_dead_app_modes(&mut self) {
        self.grid.reset_scroll_region();
        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();
        self.modes.mouse_mode = MouseMode::None;
        self.modes.mouse_encoding = MouseEncoding::X10;
        self.modes.application_cursor_keys = false;
        self.modes.application_keypad = false;
        if self.modes.synchronized_output {
            self.disable_synchronized_output();
        }
    }

    /// conhost is about to repaint over our primary screen — it is switching the
    /// alternate buffer it was left on (`TransientState::conhost_alt_screen_left_up`
    /// has the measurement) — so move the screen's rows into the scrollback
    /// first: the rows the shell printed since the recovery live nowhere else.
    /// No-op unless that state is set, which this consumes. The screen is left
    /// blank with the cursor where it was: both repaints start by homing it.
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
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetBracketedPaste",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn enable_mouse_any_event_tracking(&mut self) {
        self.modes.mouse_mode = MouseMode::AnyEvent;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetMouseMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
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
            // Mode 12 is the blink bit of the cursor STYLE (see
            // `set_cursor_blink`): report the fact the renderer acts on, so the
            // answer cannot disagree with the cursor the user is looking at.
            12 => state(self.modes.cursor_style.blinks()),
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
            // ConPTY win32-input-mode — answered to the console host that set it.
            9001 => state(self.modes.win32_input_mode),
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
mod cursor_damage_scope_tests {
    use crate::terminal::Terminal;

    /// THE FIX THE 2026-09-21 RESPONSIVENESS AUDIT ASKED FOR, as a law: a
    /// cursor-only change damages the CURSOR CELL and not the grid.
    ///
    /// Both halves matter. It must still damage SOMETHING — a bare DECTCEM with
    /// no grid write is otherwise swallowed by the frontend's redraw early-out
    /// and the cursor does not appear until the next write (the regression
    /// `mark_full` was taken for). And it must not damage EVERYTHING: a TUI that
    /// brackets each repaint in `?25l` … `?25h` — Claude Code does, ten times a
    /// second — handed the frontend a `Damage::Full` on every frame, so 99 % of
    /// the owner's content frames took the full-refill arm and re-extracted the
    /// whole viewport (measured: 8403 of 8431 full refills were `full_damage`).
    #[test]
    fn a_cursor_only_change_damages_the_cursor_cell_not_the_whole_grid() {
        // Each case must be a real TRANSITION: a mode already in the requested
        // state damages nothing at all (the handlers early-return), which is
        // correct and is not what this test is about.
        for (name, setup, seq) in [
            ("DECTCEM hide", &b""[..], &b"\x1b[?25l"[..]),
            ("DECTCEM show", &b"\x1b[?25l"[..], &b"\x1b[?25h"[..]),
            ("DECSCUSR shape", &b"\x1b[2 q"[..], &b"\x1b[6 q"[..]),
            ("mode 12 blink", &b"\x1b[?12l"[..], &b"\x1b[?12h"[..]),
        ] {
            let mut term = Terminal::new(24, 80);
            // Put the cursor somewhere that is not the origin, so "the cursor
            // cell" is a claim with content: a full mark would damage row 0 too.
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

    /// Leaving the alternate screen swaps the whole visible surface back to the
    /// main buffer, so the restored grid must carry FULL damage and the damage
    /// epoch must advance — for every exit mode. Before this, the restored
    /// grid's tracker was still the one consumed before the program entered
    /// the alt screen: `has_damage()` read false, the epoch stood still, and
    /// an epoch-gated host (the browser hosts' WF-1 frame gates) kept
    /// presenting the alt screen after `vim`/`less` had quit.
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

    /// The control: a change that really does repaint every cell still marks the
    /// whole grid. Reverse video (DECSCNM) and the bidi direction are resolved at
    /// render-snapshot time over cells already stored, so they own the full mark
    /// — this test is what keeps the narrowing above from spreading to them.
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
mod win32_input_mode_tests {
    //! ConPTY win32-input-mode (DEC 9001): the mode table half. The bytes the
    //! encoder emits under it are pinned in `keyboard_mode::shift_enter_e2e_tests`.
    use crate::terminal::Terminal;
    use aterm_types::keyboard::KeyboardMode;

    #[test]
    fn mode_9001_is_off_by_default_and_toggles_the_win32_input_bit() {
        let mut term = Terminal::new(24, 80);
        assert!(!term.modes().win32_input_mode);
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
        term.process(b"\x1b[?9001h");
        assert!(term.modes().win32_input_mode);
        assert!(term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
        term.process(b"\x1b[?9001l");
        assert!(!term.modes().win32_input_mode);
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    }

    #[test]
    fn mode_9001_is_consumed_not_echoed_to_the_grid() {
        // conhost sends it at session start, before the prompt: the sequence
        // must be consumed whole and leave nothing on the grid — pinned so the
        // mode arm can never regress to printing any part of it.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001h");
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (0, 0));
    }

    #[test]
    fn decrqm_reports_mode_9001_state() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;2$y");
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[?9001$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;1$y");
    }

    #[test]
    fn xtsave_and_xtrestore_carry_mode_9001() {
        // The DECRQM table and the XTSAVE table are one fact (see
        // `query_dec_mode`); a mode reported by one must round-trip the other.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[?9001s"); // XTSAVE
        term.process(b"\x1b[?9001l");
        assert!(!term.modes().win32_input_mode);
        term.process(b"\x1b[?9001r"); // XTRESTORE
        assert!(term.modes().win32_input_mode);
    }

    #[test]
    fn ris_clears_mode_9001() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001h");
        term.process(b"\x1bc");
        assert!(!term.modes().win32_input_mode);
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    }

    #[test]
    fn mode_9001_survives_decstr() {
        // DECSTR is the application's soft reset (cursor keys, keypad, margins,
        // SGR — the xterm list `soft_reset` walks); the console host's input
        // negotiation is not the application's to undo, so only RIS clears it.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[!p");
        assert!(term.modes().win32_input_mode);
    }
}

impl super::Terminal {
    /// Force the PRIMARY screen: the host's recovery for an alternate screen a
    /// killed app left up (`TerminalHandler::leave_orphaned_alternate_screen` has
    /// the measurement). `true` when the alternate screen was active; its last
    /// frame is then in the archive ([`Self::alt_archive`]), whole.
    ///
    /// Runs as an EMPTY output batch rather than swapping the grids here: the
    /// batch prologue and epilogue own the viewport pin, the selection park and
    /// restore, the archive's boundary, the observation kernel and the mode
    /// mirror, and every one of them keys on a swap that happens INSIDE the
    /// batch — the road a `?1049l` from the app takes.
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
    //! A killed full-screen app leaves the alternate screen up with no `?1049l`
    //! (measured 2026-09-22: `less` under ConPTY, `Stop-Process` from another tab).
    //! The prompt half is in `handler_osc_shell.rs`; this is the host's recovery
    //! and the mode-table facts both halves rest on.
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
        // The cursor `?1049h` saved: the row after the command, column 0 — where the
        // prompt paints after a clean exit.
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
        // The request was consumed by the batch that served it: the next output
        // batch does not leave a screen the next app enters.
        term.process(b"\x1b[?1049h\x1b[Hsecond app");
        assert!(term.modes().alternate_screen);
        assert_eq!(term.row_text(0).unwrap().trim_end(), "second app");
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
    }

    #[test]
    fn host_leave_of_a_mode_47_screen_keeps_the_cursor_where_the_app_had_it() {
        // `?47h` saves no cursor, so the leave restores none: the cursor stays
        // where the alt screen left it, as the app's own `?47l` would leave it —
        // even with the main slot still holding an EARLIER 1049 app's save (a
        // 1049 exit restores without consuming it).
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
        // The dead screen's rows went to the archive, not back onto the screen a
        // later `?47h` shows: the persistent alt buffer comes back blank.
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
        // The entry mode is the LATEST entry's: a 47 app that exited cleanly does
        // not turn the next 1049 app's leave into a 47 one.
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
        // An editor killed mid-session leaves its scroll region, mouse tracking,
        // cursor-key and keypad modes and an open 2026 window behind; the exit
        // carries the region over (xterm does, for a live app), so without the
        // release the shell's output scrolled inside rows 1-5 and never reached
        // the scrollback — commands lost again. Focus reporting is conhost's
        // (and the shell's) and stays.
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
        term.process(b"\x1b[2;4r\x1b[?1000h\x1b[?1h");
        assert!(term.leave_alternate_screen());
        let region = term.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (0, 4));
        assert_eq!(term.modes().mouse_mode, crate::terminal::MouseMode::None);
        assert!(!term.modes().application_cursor_keys);
    }
}

#[cfg(test)]
mod conhost_left_alt_screen_tests {
    //! After an orphaned leave under conhost, conhost itself is still on the
    //! alternate buffer the dead app entered, and its next buffer switch repaints
    //! over our primary screen. The streams are the `cast` tap's, measured
    //! 2026-09-27 on Windows 11 (`less` from Git for Windows, `Stop-Process`,
    //! then two commands, a second `less` and `q`), scaled to a 10-row grid.
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

    /// conhost's blank repaint of the viewport and its `?1049h`, in ONE read.
    fn blank_repaint_then_enter() -> Vec<u8> {
        let mut read = b"\x1b[?25l\x1b[H".to_vec();
        for _ in 1..ROWS {
            read.extend_from_slice(b"\x1b[K\r\n");
        }
        read.extend_from_slice(b"\x1b[K\x1b[?25h\x1b[?1049h");
        read
    }

    /// conhost's repaint of its PRIMARY buffer after `?1049l`: frozen at the
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
        // Negative control: the loss the state exists to prevent. With no console
        // host in between, no orphaned leave marks one, and the blank repaint
        // overwrites the rows in place — on unix nothing sends those bytes.
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
        // Something writes `?1049l` to the console while conhost is still on the
        // dead app's buffer: conhost switches back and repaints its frozen
        // primary right after — our screen is already primary, so the reset
        // itself is the only warning.
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
        // cmd.exe paints no marks: the `mainscreen` verb is the recovery, and
        // conhost is left on the dead app's buffer exactly the same way. The
        // shape is the `cast` tap's for cmd (2026-09-27): after the kill, cmd's
        // `\r\n` and its prompt under the pager's last row, a command typed
        // there, and only then the verb.
        let mut term = Terminal::new(ROWS, 40);
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

    /// A pager that filled the screen after a short history, killed, and the
    /// shell's prompt: conhost scrolled its alternate buffer for the prompt's
    /// `\r\n`, so its cursor is on the BOTTOM row, far below the row `?1049h`
    /// saved — the geometry the reviewer measured (conhost on row 23, the 1049
    /// save on row 8).
    fn recovered_on_the_bottom_row(conhost: bool) -> Terminal {
        let mut term = Terminal::new(ROWS, 40);
        if conhost {
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
        // A command that wraps, as conhost renders it (the `cast` tap: `\r\n
        // ESC[23;80H` on 24 rows): fill the row, scroll, re-anchor on the head's
        // row by ABSOLUTE position and re-print its last cell, so the tail wraps.
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
        // A resize makes conhost repaint its whole ALTERNATE buffer — the dead
        // pager's rows and everything the shell printed since — over our
        // primary screen (measured by the reviewer: `resize 24 90`, `resize 24
        // 80`, and no history row was findable). These are those rows.
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
        // Negative control: with no console host the prompt recovery is the
        // clean exit — the history stays on the screen, the cursor goes back to
        // the row `?1049h` saved, and nothing is moved into the scrollback.
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
        // A new app's enter, however it arrives, means conhost's alternate buffer
        // is a live one again; RIS forgets the state outright. Either way the
        // next switch moves nothing.
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

#[cfg(test)]
#[path = "conpty_dead_alt_screen_replay_tests.rs"]
mod conpty_dead_alt_screen_replay_tests;

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

#[cfg(test)]
mod mode_and_query_agreement_tests {
    //! The query and the glass are one fact, walked.
    use crate::terminal::Terminal;
    use aterm_types::CursorStyle;

    fn decrqm(term: &mut Terminal, mode: u16) -> Vec<u8> {
        term.process(format!("\x1b[?{mode}$p").as_bytes());
        term.take_response().unwrap_or_default()
    }

    /// DEC MODE 12 IS THE BLINK BIT OF THE CURSOR STYLE.
    ///
    /// The renderer arms its blink clock off the `Blinking*` styles and nothing
    /// else, so a mode 12 that only moved a flag beside them reached the code
    /// and never the glass — while `CSI ? 12 $ p` answered that flag. Both
    /// halves are checked here, and they matter on the most ordinary path
    /// there is: aterm spawns shells with `TERM=xterm-256color`, whose terminfo
    /// spells `cnorm` as `\E[?12l\E[?25h` and `cvvis` as `\E[?12;25h`, so every
    /// ncurses `curs_set()` drives this mode.
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

        // terminfo `cvvis`: blink again. The SHAPE is untouched by mode 12, so
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

    /// The other direction: DECSCUSR carries the blink bit too, so it moves
    /// mode 12 with it. Otherwise a `vim` that set a steady bar left the query
    /// claiming the cursor blinks.
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

    /// A mode DECRQM can REPORT is a mode XTSAVE/XTRESTORE must be able to
    /// CARRY. The two tables are two spellings of one fact (`query_dec_mode`
    /// and `handle_decrqm`) and had drifted by seven modes, each of which made
    /// `CSI ? Ps s` / `CSI ? Ps r` a silent no-op that looked exactly like a
    /// working one.
    #[test]
    fn xtsave_covers_every_mode_decrqm_reports() {
        // Every mode `handle_decrqm` answers 1/2 for (the 3/4 "permanently
        // set/reset" and 0 "unknown" answers are not save/restorable state).
        const REPORTED: &[u16] = &[
            1, 3, 5, 6, 7, 9, 12, 25, 40, 45, 66, 67, 69, 80, 95, 1000, 1002, 1003, 1004, 1005,
            1006, 1007, 1015, 1016, 1035, 1036, 1039, 1045, 1243, 2004, 2026, 2027, 2031, 2048,
            2500, 2501, 9001,
        ];
        let mut lost = Vec::new();
        for &mode in REPORTED {
            let mut term = Terminal::new(24, 80);
            // Mode 3 (DECCOLM) is only honored while mode 40 is set.
            if mode == 3 {
                term.process(b"\x1b[?40h");
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
