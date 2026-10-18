// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CSI dispatch. Every CSI sequence goes through the perfect hash in `csi_dispatch_table`,
//! mapping `(intermediate_encoding, final_byte)` to a `CsiHandler`; misses fall to
//! `csi_dispatch_untabled`.

use super::super::csi_dispatch_table::{CsiHandler, lookup_csi_handler};
use super::super::sgr_color_u8;
use super::TerminalHandler;
use crate::grid::CellFlags;
use aterm_types::ScreenBuffer;

impl TerminalHandler<'_> {
    /// CSI with intermediates, via the dispatch table; unrecognized ones are ignored. Returns
    /// `true` if consumed.
    pub(super) fn csi_dispatch_with_intermediates(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        params: &[u16],
        intermediates: &[u8],
        final_byte: u8,
    ) -> bool {
        if intermediates.is_empty() {
            return false;
        }
        let Some(handler) = lookup_csi_handler(intermediates, final_byte) else {
            return self.csi_dispatch_untabled(params, intermediates, final_byte);
        };
        self.dispatch_csi_handler(cap, handler, params, final_byte);
        true
    }

    /// CSI without intermediates, via the dispatch table. Returns `true` if consumed.
    #[allow(clippy::too_many_lines)]
    pub(super) fn csi_dispatch_standard_core(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        params: &[u16],
        final_byte: u8,
    ) -> bool {
        let Some(handler) = lookup_csi_handler(&[], final_byte) else {
            return self.csi_dispatch_untabled(params, &[], final_byte);
        };
        self.dispatch_csi_handler(cap, handler, params, final_byte);
        true
    }

    /// The CSI sequences the table does not carry, reached only on a table miss. The table's
    /// hash parameters are fixed, so later sequences land here instead of forcing a re-solve;
    /// both dispatch paths share this fallback. Returns `true` if consumed.
    fn csi_dispatch_untabled(
        &mut self,
        params: &[u16],
        intermediates: &[u8],
        final_byte: u8,
    ) -> bool {
        match (intermediates, final_byte) {
            // DECSACE (CSI Ps * x): other values leave the extent unchanged, as in xterm.
            (b"*", b'x') => {
                match params.first().copied().unwrap_or(0) {
                    0 | 1 => self.modes.stream_attribute_extent = true,
                    2 => self.modes.stream_attribute_extent = false,
                    _ => {}
                }
                true
            }
            // SL / SR — Scroll Left / Right (CSI Ps SP @ / CSI Ps SP A), ECMA-48.
            (b" ", b'@') => {
                self.cursor_state().handle_sl(params);
                true
            }
            (b" ", b'A') => {
                self.cursor_state().handle_sr(params);
                true
            }
            // DECRARA, Reverse Attributes in Rectangular Area
            // (CSI Pt ; Pl ; Pb ; Pr ; Pm $ t), VT400 and up: the reverse half of
            // DECCARA (`$ r`). The rest of the DA1 code-28 rectangular family
            // (`$ v`, `$ x`, `$ z`, `$ {`) is in the table.
            (b"$", b't') => {
                self.handle_decrara(params);
                true
            }
            // XTPUSHSGR / XTPOPSGR spelled `# p` / `# q`: xterm documents them as
            // aliases of `CSI # {` / `CSI # }` for emitters that cannot produce
            // braces. The brace spellings are in the table.
            (b"#", b'p') => {
                self.handle_xtpushsgr(params);
                true
            }
            (b"#", b'q') => {
                self.handle_xtpopsgr();
                true
            }
            // SD in the ECMA-48 spelling (CSI Ps ^), routed to the table's `CSI Ps T` handler.
            (b"", b'^') => {
                self.cursor_state().handle_scroll(params, b'T');
                true
            }
            _ => false,
        }
    }

    /// Run the handler a dispatch-table hit names.
    #[allow(clippy::too_many_lines, reason = "dispatch table with 73 handler arms")]
    fn dispatch_csi_handler(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        handler: CsiHandler,
        params: &[u16],
        final_byte: u8,
    ) {
        match handler {
            // --- Standard cursor movement (no intermediates) ---
            CsiHandler::CursorUp => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.cursor_up(n);
            }
            CsiHandler::CursorDown => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.cursor_down(n);
            }
            CsiHandler::CursorForward => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid
                    .cursor_forward_margin(n, self.modes.left_right_margin_mode);
            }
            CsiHandler::CursorBackward => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid
                    .cursor_backward_margin(n, self.modes.left_right_margin_mode);
            }
            CsiHandler::CursorPosition | CsiHandler::CursorPositionAlt => {
                // CUP - inline to avoid cursor_state() for this high-frequency op.
                let row = params.first().copied().unwrap_or(1).saturating_sub(1);
                let col = params.get(1).copied().unwrap_or(1).saturating_sub(1);
                let (actual_row, actual_col) = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    let r = region.top.saturating_add(row).min(region.bottom);
                    if self.modes.left_right_margin_mode {
                        let margins = self.grid.horizontal_margins();
                        let c = margins.left.saturating_add(col).min(margins.right);
                        (r, c)
                    } else {
                        (r, col)
                    }
                } else {
                    (row, col)
                };
                self.grid.set_cursor(actual_row, actual_col);
            }
            CsiHandler::CursorNextLine
            | CsiHandler::CursorPrevLine
            | CsiHandler::CursorHorizAbs
            | CsiHandler::LinePositionAbs
            | CsiHandler::CharPositionRelative
            | CsiHandler::LinePositionRel
            | CsiHandler::CharPositionAbs => {
                self.cursor_state()
                    .handle_cursor_movement(params, final_byte);
            }

            // --- Erase ---
            CsiHandler::EraseDisplay | CsiHandler::EraseLine => {
                // A full clear (ED 2, or ED 0 from home) is the app repainting from
                // scratch; counted for the resize journal (ED 3 erases only
                // history). The cursor is read where the ED was issued.
                let cursor = self.grid.cursor();
                let from_home = cursor.row == 0 && cursor.col == 0;
                let full_clear = final_byte == b'J'
                    && super::super::resize_journal::is_full_clear(
                        params.first().copied().unwrap_or(0),
                        cursor.row,
                        cursor.col,
                    );
                self.cursor_state().handle_erase(params, final_byte);
                if full_clear {
                    self.transient.full_clears = self.transient.full_clears.wrapping_add(1);
                    self.transient.render_debt = (0, 0);
                    // The archived blocks on the main screen lost their text,
                    // and the block being built moves onto the cleared screen
                    // the rows the clear blanked (`clear`, Ctrl-L; a prompt
                    // only when the clear came from home, where its line
                    // editor redraws it); an alternate screen clears only its
                    // own grid, and no anchor names an alt-grid row.
                    if !self.modes.alternate_screen {
                        super::note_full_clear(
                            self.grid.visible_to_absolute(0),
                            from_home,
                            self.shell,
                        );
                    }
                }
                // ED 3 erases the active grid's history and renumbers no row:
                // the anchors wholly in that history go with it, the command
                // still being entered or run starts at the oldest row left if
                // it took that command's first rows, and the rest stay where
                // they are.
                // On the alternate screen it erased the alt grid's history,
                // and every anchor names a main-grid row, which it left alone.
                if final_byte == b'J'
                    && params.first().copied().unwrap_or(0) == 3
                    && !self.modes.alternate_screen
                {
                    super::forget_erased_history(
                        self.grid.oldest_absolute_row(),
                        self.shell,
                        self.marks_state,
                    );
                }
            }

            // --- SGR ---
            CsiHandler::Sgr => {
                self.sgr_style().handle_sgr(params);
            }

            // --- Scroll ---
            CsiHandler::ScrollUp | CsiHandler::ScrollDown => {
                self.cursor_state().handle_scroll(params, final_byte);
            }

            // --- Insert/Delete ---
            CsiHandler::InsertChars
            | CsiHandler::DeleteChars
            | CsiHandler::InsertLines
            | CsiHandler::DeleteLines => {
                self.cursor_state().handle_insert_delete(params, final_byte);
            }

            CsiHandler::EraseChars => {
                // ECH, Erase Character (CSI Ps X): erase Ps characters from the
                // cursor without shifting; erased cells take the SGR background.
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // ECH is non-selective: it ignores DECSCA protection. Only DECSED
                // (CSI ? J) and DECSEL (CSI ? K) skip protected cells (VT420/VT510).
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.erase_chars(n);
            }

            CsiHandler::RepeatChar => {
                // REP, Repeat Preceding Graphic Character (CSI Ps b). The stored
                // char is raw and re-translated through the current GL charset,
                // as xterm does, via write_char's normal path. Clear ZWJ state: a
                // repeat is a new cell and must not join a preceding ZWJ.
                self.transient.last_combining_was_zwj = false;
                let count = params.first().copied().unwrap_or(1).max(1);
                if let Some(c) = self.transient.last_graphic_char {
                    // Fast path: printable ASCII with simple style (no RGB,
                    // hyperlinks, extras) and a passthrough GL charset uses a bulk
                    // cell fill.
                    let cp = c as u32;
                    let is_ascii = (0x20..0x7F).contains(&cp);
                    let has_extras =
                        self.style.has_style_extras() || self.transient.has_transient_extras;
                    // Not from outside a DECSLRM span: the fill clamps at the
                    // right margin only from a start inside it
                    // (`Grid::cursor_outside_margin_span`).
                    if is_ascii
                        && self.charset.is_ascii_passthrough()
                        && !has_extras
                        && self.modes.auto_wrap
                        && !self.modes.insert_mode
                        && count >= 4
                        && !self.grid.cursor_outside_margin_span()
                    {
                        let flags = if self.style.protected {
                            self.style.flags.union(CellFlags::PROTECTED)
                        } else {
                            self.style.flags
                        };
                        let mut last_byte: Option<u8> = None;
                        // is_ascii guard above ensures cp is in 0x20..0x7F
                        #[allow(clippy::cast_possible_truncation)]
                        let ascii_byte = cp as u8;
                        // REP's bulk arm reaches the grid without `write_char`, so it
                        // needs its own output-damage bracket; the else-arm uses
                        // `write_char`'s.
                        let origin = self.grid.output_damage_origin();
                        self.grid.write_cell_run(
                            ascii_byte,
                            count as usize,
                            self.style.cached_colors(),
                            flags,
                            &mut last_byte,
                        );
                        self.grid.damage_selection_output(origin);
                    } else {
                        for _ in 0..count {
                            self.write_char(c);
                        }
                    }
                }
            }

            CsiHandler::BackwardTab => {
                // CBT, Cursor Backward Tabulation (CSI Ps Z). The left margin
                // binds a back tab only under origin mode (VT510's CBT and DECOM
                // pages; xterm and Ghostty agree). See `Grid::back_tab_margin`.
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.back_tab_n_margin(
                    n,
                    self.modes.left_right_margin_mode,
                    self.modes.origin_mode,
                );
            }

            CsiHandler::ForwardTab => {
                // CHT (CSI Ps I): forward Ps tab stops.
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.tab_n_margin(n, self.modes.left_right_margin_mode);
            }

            // --- Save/restore cursor, scroll region, modes ---
            CsiHandler::SaveCursor => {
                // With DECLRMM (mode 69) on, CSI s is DECSLRM; otherwise SCOSC.
                if self.modes.left_right_margin_mode {
                    // DECSLRM, Set Left and Right Margins (CSI Pl ; Pr s, VT420+).
                    // Params are 1-indexed; 0 means default.
                    let left = params
                        .first()
                        .copied()
                        .unwrap_or(1)
                        .max(1) // Treat 0 as 1 (default: first column)
                        .saturating_sub(1);
                    let right = params
                        .get(1)
                        .copied()
                        .map(|p| if p == 0 { self.grid.cols() } else { p })
                        .unwrap_or(self.grid.cols())
                        .saturating_sub(1);
                    // As in xterm CASE_DECSLRM, an invalid margin set (left >= right
                    // after defaulting and clamping) is ignored entirely, keeping the
                    // previous margins and the cursor, like DECSTBM below.
                    let max_right = self.grid.cols().saturating_sub(1);
                    if left < right.min(max_right) {
                        self.grid.set_horizontal_margins(left, right.min(max_right));
                        // DECSLRM homes the cursor, as DECSTBM does (VT510).
                        if self.modes.origin_mode {
                            let region = self.grid.scroll_region();
                            let h_margins = self.grid.horizontal_margins();
                            self.grid.set_cursor(region.top, h_margins.left);
                        } else {
                            self.grid.set_cursor(0, 0);
                        }
                    }
                } else {
                    // SCOSC saves the full cursor state, as DECSC does.
                    self.cursor_state().save_cursor_state();
                }
            }

            CsiHandler::RestoreCursor => {
                // SCORC — restore full cursor state exactly like DECRC.
                self.cursor_state().restore_cursor_state();
            }

            CsiHandler::SetScrollRegion => {
                // DECSTBM, Set scrolling region (CSI Ps ; Ps r). Params are
                // 1-indexed; 0 means default.
                let top = params
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_sub(1);
                let bottom = params
                    .get(1)
                    .copied()
                    .map(|p| if p == 0 { self.grid.rows() } else { p })
                    .unwrap_or(self.grid.rows())
                    .saturating_sub(1);
                // As in xterm CASE_DECSTBM, an invalid region (bottom <= top after
                // defaulting and clamping, e.g. CSI 10;10r) is ignored entirely:
                // margins and cursor stay put.
                let max_bottom = self.grid.rows().saturating_sub(1);
                if top < bottom.min(max_bottom) {
                    self.grid.set_scroll_region(top, bottom);
                    // Per VT510: DECSTBM moves cursor to home position.
                    if self.modes.origin_mode {
                        let region = self.grid.scroll_region();
                        let col = if self.modes.left_right_margin_mode {
                            self.grid.horizontal_margins().left
                        } else {
                            0
                        };
                        self.grid.set_cursor(region.top, col);
                    } else {
                        self.grid.set_cursor(0, 0);
                    }
                }
            }

            CsiHandler::TabClear => {
                let mode = params.first().copied().unwrap_or(0);
                match mode {
                    0 => self.grid.clear_tab_stop(),      // Clear tab stop at cursor
                    3 => self.grid.clear_all_tab_stops(), // Clear all tab stops
                    _ => {}
                }
            }

            CsiHandler::SetMode => {
                self.handle_ansi_mode(params, true);
            }

            CsiHandler::ResetMode => {
                self.handle_ansi_mode(params, false);
            }

            CsiHandler::DeviceStatusReport => {
                self.handle_dsr(cap, params);
            }

            CsiHandler::PrimaryDA => {
                // DA1: answered only for param 0 or omitted.
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_primary_da(cap);
                }
            }

            CsiHandler::Xtwinops => {
                self.handle_xtwinops(cap, params);
            }

            // --- DEC private modes (? intermediate) ---
            CsiHandler::DecModeSet => self.handle_dec_mode(params, true),
            CsiHandler::DecModeReset => self.handle_dec_mode(params, false),
            CsiHandler::SelectiveEraseDisplay => {
                self.cursor_state().handle_selective_erase_display(params);
            }
            CsiHandler::SelectiveEraseLine => {
                self.cursor_state().handle_selective_erase_line(params);
            }
            CsiHandler::KittyKeyboardQuery => {
                // Capability off: no reply (as an unsupporting terminal), so apps use legacy
                // encoding. aterm-gui turns it off for ConPTY sessions
                // (`spawn::apply_pty_keyboard_posture`): conhost drops CSI-u key reports.
                if self.modes.kitty_keyboard_enabled {
                    self.handle_kitty_keyboard_query(cap);
                }
            }
            CsiHandler::XtermModkeysQuery => {
                // XTMODKEYS query (CSI ? Pp m). Pp=4 (modifyOtherKeys) is tracked;
                // Pp=0..3 answer the default `CSI > Pp m` (disabled) so programs do
                // not wait for a missing response.
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    let response = self.xterm_keyboard.query_modify_other_keys_response();
                    self.send_response(cap, response.as_bytes());
                } else if pp <= 3 {
                    let response = format!("\x1b[>{pp}m");
                    self.send_response(cap, response.as_bytes());
                }
            }
            CsiHandler::XtermFmtkeysQuery => {
                // XTFMTKEYS query (CSI ? Pp g), answered as for XTMODKEYS.
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    let response = self.xterm_keyboard.query_format_other_keys_response();
                    self.send_response(cap, response.as_bytes());
                } else if pp <= 3 {
                    let response = format!("\x1b[>{pp}g");
                    self.send_response(cap, response.as_bytes());
                }
            }
            CsiHandler::Xtsmgraphics => self.handle_xtsmgraphics(cap, params),
            CsiHandler::DecDsr => self.handle_decdsr(cap, params),
            CsiHandler::Xtsave => self.handle_xtsave(params),
            CsiHandler::Xtrestore => self.handle_xtrestore(params),
            CsiHandler::Decst8c => {
                // DECST8C: Set tab stops at every 8 columns.
                if params.first().copied() == Some(5) {
                    self.grid.reset_tab_stops();
                }
            }

            // --- DEC Request Mode (?$ intermediates) ---
            CsiHandler::Decrqm => self.handle_decrqm(cap, params),

            // --- xterm extensions (> intermediate) ---
            CsiHandler::SecondaryDA => {
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_secondary_da(cap);
                }
            }
            CsiHandler::XtmodkeysSet => {
                // XTMODKEYS: CSI > Pp ; Pv m (set) or CSI > Pp m (reset). An
                // empty Pv (`CSI > 4 ; m`, vim's t_TE) arrives as 0, which is
                // the value the reset restores.
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    if let Some(&pv) = params.get(1) {
                        self.xterm_keyboard.set_modify_other_keys(sgr_color_u8(pv));
                        if pv != 0 {
                            *self.evidence_asserted |=
                                super::super::program_evidence::MODIFY_OTHER_KEYS;
                        }
                    } else {
                        self.xterm_keyboard.reset_modify_other_keys();
                    }
                }
            }
            CsiHandler::XtmodkeysDisable => {
                // XTMODKEYS disable: CSI > Ps n (resource value -1)
                let ps = params.first().copied().unwrap_or(0);
                if ps == 4 {
                    self.xterm_keyboard.disable_modify_other_keys();
                }
            }
            CsiHandler::XtfmtkeysSet => {
                // XTFMTKEYS: CSI > Pp ; Pv f (set) or CSI > Pp f (reset); an
                // empty Pv arrives as 0, the reset value, as for XTMODKEYS.
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    if let Some(&pv) = params.get(1) {
                        self.xterm_keyboard.set_format_other_keys(sgr_color_u8(pv));
                        if pv != 0 {
                            *self.evidence_asserted |=
                                super::super::program_evidence::FORMAT_OTHER_KEYS;
                        }
                    } else {
                        self.xterm_keyboard.reset_format_other_keys();
                    }
                }
            }
            CsiHandler::Xtversion => {
                // XTVERSION: CSI > Ps q — Query terminal version.
                let ps = params.first().copied().unwrap_or(0);
                if ps == 0 {
                    let response = format!(
                        "\x1bP>|aterm({})\x1b\\",
                        aterm_types::version::app_version()
                    );
                    self.send_response(cap, response.as_bytes());
                }
            }

            // --- Kitty keyboard (>, =, < intermediates) ---
            // With the capability off (the platform consumes kitty sequences
            // itself), push/set/pop are consumed and ignored like an unrecognized
            // CSI.
            CsiHandler::KittyKeyboardPush => {
                // CSI > flags u: push current flags, set new; omitted flags are 0.
                // Undefined high bits are masked off (only the five defined flags
                // survive), not saturated, so `CSI > 256 u` means flags 0.
                if self.modes.kitty_keyboard_enabled {
                    let flags = if params.is_empty() {
                        0
                    } else {
                        (params[0] & 0b1_1111) as u8
                    };
                    self.kitty_keyboard.push_flags_for_buffer(
                        flags,
                        ScreenBuffer::from(self.modes.alternate_screen),
                    );
                    // A push is kitty evidence whatever its flags (the stack).
                    *self.evidence_asserted |= super::super::program_evidence::KITTY;
                }
            }
            CsiHandler::KittyKeyboardSet => {
                if self.modes.kitty_keyboard_enabled {
                    // Flags are masked, not saturated; the mode param saturates
                    // (every value >= 4 is undefined and `apply` ignores it).
                    let flags = (params.first().copied().unwrap_or(0) & 0b1_1111) as u8;
                    let mode = sgr_color_u8(params.get(1).copied().unwrap_or(1));
                    self.kitty_keyboard.set_flags(flags, mode);
                    if self.kitty_keyboard.snapshot().flags.bits() != 0 {
                        *self.evidence_asserted |= super::super::program_evidence::KITTY;
                    }
                }
            }
            CsiHandler::KittyKeyboardPop => {
                if self.modes.kitty_keyboard_enabled {
                    let count = params.first().copied().unwrap_or(1);
                    self.kitty_keyboard.pop_flags_for_buffer(
                        count,
                        ScreenBuffer::from(self.modes.alternate_screen),
                    );
                }
            }

            // --- Device attributes (= intermediate) ---
            CsiHandler::TertiaryDA => {
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_tertiary_da(cap);
                }
            }

            // --- Misc intermediates ---
            CsiHandler::Decstr => self.handle_decstr(),
            CsiHandler::Decscusr => self.cursor_state().handle_decscusr(params),
            CsiHandler::Scp => self.cursor_state().handle_scp(params),
            CsiHandler::Decsca => self.cursor_state().handle_decsca(params),
            CsiHandler::Decscl => self.cursor_state().handle_decscl(params),
            CsiHandler::Xtpushsgr => self.handle_xtpushsgr(params),
            CsiHandler::Xtpopsgr => self.handle_xtpopsgr(),
            CsiHandler::AnsiDecrqm => self.handle_ansi_decrqm(cap, params),
            CsiHandler::Deccara => self.handle_deccara(params),
            CsiHandler::Deccra => self.handle_deccra(params),
            CsiHandler::Decfra => self.handle_decfra(params),
            CsiHandler::Decera => self.handle_decera(params),
            CsiHandler::Decsera => self.handle_decsera(params),
            CsiHandler::KittyUnscroll => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                if !self.modes.alternate_screen {
                    // `Terminal::kitty_unscrolls`: what it pulls depends on the history.
                    self.transient.kitty_unscrolls = self.transient.kitty_unscrolls.wrapping_add(1);
                }
                self.grid.unscroll_from_scrollback(n);
            }
            CsiHandler::Decic => {
                // DECIC operates within the horizontal margins, which are the full
                // width with DECLRMM (mode 69) off, so it always applies (xterm, DEC
                // STD 070). insert_columns enforces the cursor-in-margins bound.
                self.cursor_state().handle_decic(params);
            }
            CsiHandler::Decdc => {
                self.cursor_state().handle_decdc(params);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// An invalid DECSLRM (left >= right) is a complete no-op, as in xterm and
    /// like DECSTBM: an existing valid margin region and the cursor are kept.
    #[test]
    fn decslrm_invalid_left_ge_right_is_noop() {
        use crate::terminal::Terminal;

        let mut term = Terminal::new(24, 80);
        // DECLRMM on, so `CSI s` is DECSLRM; a valid [10, 50] also homes the cursor.
        term.process(b"\x1b[?69h\x1b[11;51s");
        assert_eq!(
            term.grid().horizontal_margins(),
            aterm_grid::HorizontalMargins {
                left: 10,
                right: 50,
            },
            "valid DECSLRM sets [10,50]"
        );

        // Position the cursor inside the region: row 5, col 30 (1-indexed 6;31).
        term.process(b"\x1b[6;31H");
        assert_eq!((term.cursor().row, term.cursor().col), (5, 30));

        // Invalid DECSLRM: left=40, right=30 (1-indexed 41;31), left >= right.
        term.process(b"\x1b[41;31s");

        // A complete no-op: margins and cursor kept.
        assert_eq!(
            term.grid().horizontal_margins(),
            aterm_grid::HorizontalMargins {
                left: 10,
                right: 50,
            },
            "invalid DECSLRM must NOT clobber the prior margins"
        );
        assert_eq!(
            (term.cursor().row, term.cursor().col),
            (5, 30),
            "invalid DECSLRM must NOT home the cursor"
        );
    }

    /// The valid default-param DECSLRM (`CSI s` -> left=0, right=cols-1) still
    /// passes the guard: it resets to full width and homes the cursor.
    #[test]
    fn decslrm_default_params_reset_full_and_home() {
        use crate::terminal::Terminal;

        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?69h\x1b[11;51s\x1b[6;31H");
        term.process(b"\x1b[s");
        assert_eq!(
            term.grid().horizontal_margins(),
            aterm_grid::HorizontalMargins { left: 0, right: 79 },
            "default DECSLRM resets to full width"
        );
        assert_eq!((term.cursor().row, term.cursor().col), (0, 0));
    }
}
