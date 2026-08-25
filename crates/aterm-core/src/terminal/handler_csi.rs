// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CSI dispatch helpers for terminal escape sequence handling.
//!
//! This module routes all CSI sequences through a z4-verified minimal
//! perfect hash table (`csi_dispatch_table`). The table maps
//! `(intermediate_encoding, final_byte)` pairs to `CsiHandler` variants
//! using a collision-free hash function verified by the z4 SMT solver.
//!
//! Both standard CSI sequences (no intermediates) and extended sequences
//! (DEC private, xterm, kitty, etc.) are dispatched through the same
//! table for uniform O(1) routing.

use super::super::csi_dispatch_table::{CsiHandler, lookup_csi_handler};
use super::super::sgr_color_u8;
use super::TerminalHandler;
use crate::grid::CellFlags;
use aterm_types::ScreenBuffer;

impl TerminalHandler<'_> {
    /// Handle CSI sequences with intermediates via z4-verified table lookup.
    ///
    /// Uses `lookup_csi_handler` from `csi_dispatch_table` for O(1) dispatch.
    /// Unrecognized sequences are silently ignored per VT spec.
    ///
    /// Returns `true` if the sequence was consumed.
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
            // DECSACE — Select Attribute Change Extent (CSI Ps * x).
            // Not in the z4 dispatch table; handled inline until table regen.
            // xterm ctlseqs / VT520 (EK-VT520-RM): Ps = 0 (default) or 1
            // selects the wrapped character stream; Ps = 2 selects the exact
            // rectangle. Out-of-range values leave the extent unchanged
            // (xterm CASE_DECSACE has no default arm).
            if intermediates == *b"*" && final_byte == b'x' {
                match params.first().copied().unwrap_or(0) {
                    0 | 1 => self.modes.stream_attribute_extent = true,
                    2 => self.modes.stream_attribute_extent = false,
                    _ => {}
                }
                return true;
            }
            // SL / SR — Scroll Left / Right (CSI Ps SP @ / CSI Ps SP A). Not in
            // the z4 dispatch table; handled inline until table regen (same
            // pattern as DECSACE above).
            if intermediates == *b" " {
                match final_byte {
                    b'@' => {
                        self.cursor_state().handle_sl(params);
                        return true;
                    }
                    b'A' => {
                        self.cursor_state().handle_sr(params);
                        return true;
                    }
                    _ => {}
                }
            }
            return false;
        };
        self.dispatch_csi_handler(cap, handler, params, final_byte);
        true
    }

    /// Handle standard CSI sequences without intermediates.
    ///
    /// Uses the z4-verified dispatch table for O(1) lookup, then dispatches
    /// to the corresponding handler. Returns `true` if consumed.
    #[allow(clippy::too_many_lines)]
    pub(super) fn csi_dispatch_standard_core(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        params: &[u16],
        final_byte: u8,
    ) -> bool {
        let Some(handler) = lookup_csi_handler(&[], final_byte) else {
            return false;
        };
        self.dispatch_csi_handler(cap, handler, params, final_byte);
        true
    }

    /// Unified dispatch for all 73 CSI handler variants via z4-verified table.
    ///
    /// Each `CsiHandler` variant maps to exactly one handler method call.
    /// The handler methods themselves are unchanged — only the routing is
    /// table-driven instead of match-chain-driven.
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
                if self.modes.grapheme_cluster_mode {
                    self.cursor_state().cursor_forward_graphemes(n);
                } else {
                    self.grid
                        .cursor_forward_margin(n, self.modes.left_right_margin_mode);
                }
            }
            CsiHandler::CursorBackward => {
                let n = params.first().copied().unwrap_or(1).max(1);
                if self.modes.grapheme_cluster_mode {
                    self.cursor_state().cursor_backward_graphemes(n);
                } else {
                    self.grid
                        .cursor_backward_margin(n, self.modes.left_right_margin_mode);
                }
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
            // Less frequent cursor movements go through cursor_state()
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
                self.cursor_state().handle_erase(params, final_byte);
                if final_byte == b'J' {
                    let mode = params.first().copied().unwrap_or(0);
                    // ED 3 (CSI 3 J) erases scrollback — clear shell integration
                    // marks and annotations that contain absolute row numbers,
                    // which become dangling references after erase (#7667).
                    if mode == 3 {
                        self.shell.command_marks.clear();
                        self.shell.output_blocks.clear();
                        self.shell.current_block = None;
                        self.shell.current_mark = None;
                        self.marks_state.marks.clear();
                        self.marks_state.annotations.clear();
                    }
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
                // ECH - Erase Character
                // CSI Ps X - Erase Ps characters starting at cursor, without shifting
                // Set BCE template so erased cells inherit SGR background (#7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // ECH is a non-selective erase: it does NOT check DECSCA character
                // protection. Per VT420/VT510, only DECSED (CSI ? J) and DECSEL
                // (CSI ? K) are selective operations that skip protected cells.
                // Reverts incorrect selective dispatch from #7523.
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.erase_chars(n);
            }

            CsiHandler::RepeatChar => {
                // REP - Repeat Preceding Graphic Character
                // CSI Ps b - Repeat the preceding graphic character Ps times
                // The stored char is RAW: per xterm CASE_REP it is re-translated
                // through the GL charset current at repeat time
                // (dotext(xw, screen->gsets[curgl], lastchar)), so the repeats
                // go through write_char's normal translation path.
                // Clear ZWJ state: REP repeats the last *graphic* char as a new
                // cell; it must not combine with a preceding ZWJ (#7548).
                self.transient.last_combining_was_zwj = false;
                let count = params.first().copied().unwrap_or(1).max(1);
                if let Some(c) = self.transient.last_graphic_char {
                    // FAST PATH: For printable ASCII with simple style (no RGB,
                    // hyperlinks, extras) and a passthrough GL charset (the
                    // bytes ARE the glyphs), use bulk cell fill instead of
                    // per-char.
                    let cp = c as u32;
                    let is_ascii = (0x20..0x7F).contains(&cp);
                    let has_extras =
                        self.style.has_style_extras() || self.transient.has_transient_extras;
                    if is_ascii
                        && self.charset.is_ascii_passthrough()
                        && !has_extras
                        && self.modes.auto_wrap
                        && !self.modes.insert_mode
                        && count >= 4
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
                        // SELECTION CUSTODY — REP's bulk arm is the one printing path
                        // that reaches the grid without going through `write_char`,
                        // so it needs its own output-damage bracket; the else-arm
                        // below is covered by `write_char`'s.
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
                            // Re-translate through the CURRENT GL charset
                            // (xterm CASE_REP semantics).
                            self.write_char(c);
                        }
                    }
                }
            }

            CsiHandler::BackwardTab => {
                // CBT - Cursor Backward Tabulation
                // CSI Ps Z - Move cursor backward Ps tab stops (default 1)
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid
                    .back_tab_n_margin(n, self.modes.left_right_margin_mode);
            }

            CsiHandler::ForwardTab => {
                // CHT - Cursor Horizontal Tab (Forward Tabulation)
                // CSI Ps I - Move cursor forward Ps tab stops (default 1)
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.tab_n_margin(n, self.modes.left_right_margin_mode);
            }

            // --- Save/restore cursor, scroll region, modes ---
            CsiHandler::SaveCursor => {
                // When DECLRMM (mode 69) is enabled, CSI s is DECSLRM.
                // Otherwise, it's SCOSC (save cursor).
                if self.modes.left_right_margin_mode {
                    // DECSLRM - Set Left and Right Margins (VT420+)
                    // CSI Pl ; Pr s - Set left and right margins
                    // Params are 1-indexed; convert to 0-indexed
                    // Per VT420 spec, parameter 0 means "use default"
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
                    // Per xterm CASE_DECSLRM: an INVALID margin set
                    // (left >= right after defaulting/clamping to the
                    // screen) is ignored ENTIRELY — both set_lr_margins
                    // and the CursorSet(home) are guarded behind
                    // `if (left < right)`, mirroring the DECSTBM sibling
                    // below. This keeps a previously-set valid margin and
                    // the cursor untouched on a stray/invalid sequence,
                    // instead of letting set_horizontal_margins reset to
                    // full width and homing the cursor.
                    let max_right = self.grid.cols().saturating_sub(1);
                    if left < right.min(max_right) {
                        self.grid.set_horizontal_margins(left, right.min(max_right));
                        // Per VT510: DECSLRM moves cursor to home position,
                        // just like DECSTBM. Origin-mode aware.
                        if self.modes.origin_mode {
                            let region = self.grid.scroll_region();
                            let h_margins = self.grid.horizontal_margins();
                            self.grid.set_cursor(region.top, h_margins.left);
                        } else {
                            self.grid.set_cursor(0, 0);
                        }
                    }
                } else {
                    // SCOSC — save full cursor state (style, charset, origin_mode,
                    // auto_wrap, pending_wrap, underline_color) exactly like DECSC.
                    self.cursor_state().save_cursor_state();
                }
            }

            CsiHandler::RestoreCursor => {
                // SCORC — restore full cursor state exactly like DECRC.
                self.cursor_state().restore_cursor_state();
            }

            CsiHandler::SetScrollRegion => {
                // Set scrolling region (DECSTBM)
                // CSI Ps ; Ps r - Set top and bottom margins
                // Params are 1-indexed; convert to 0-indexed.
                // Per VT510 spec, param 0 means "use default" (same as DECSLRM).
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
                // Per xterm CASE_DECSTBM: an INVALID region (bottom <= top
                // after defaulting and clamping to the screen, e.g.
                // CSI 10;10r or CSI 28r) is ignored ENTIRELY — both
                // set_tb_margins and the CursorSet(0,0) home are guarded
                // behind `if (bot > top)`, so margins AND cursor stay put.
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
                // TBC - Tab Clear
                let mode = params.first().copied().unwrap_or(0);
                match mode {
                    0 => self.grid.clear_tab_stop(),      // Clear tab stop at cursor
                    3 => self.grid.clear_all_tab_stops(), // Clear all tab stops
                    _ => {}
                }
            }

            CsiHandler::SetMode => {
                // SM - Set Mode (ANSI)
                self.handle_ansi_mode(params, true);
            }

            CsiHandler::ResetMode => {
                // RM - Reset Mode (ANSI)
                self.handle_ansi_mode(params, false);
            }

            CsiHandler::DeviceStatusReport => {
                // DSR - Device Status Report
                self.handle_dsr(cap, params);
            }

            CsiHandler::PrimaryDA => {
                // DA1 - Primary Device Attributes
                // Only respond if param is 0 or omitted
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_primary_da(cap);
                }
            }

            CsiHandler::Xtwinops => {
                // XTWINOPS - Window manipulation (CSI Ps ; Ps ; Ps t)
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
                // Capability off: NO reply (an unsupporting terminal ignores the
                // query), so apps fall back to legacy encoding — never a `?0u` ack.
                if self.modes.kitty_keyboard_enabled {
                    self.handle_kitty_keyboard_query(cap);
                }
            }
            CsiHandler::XtermModkeysQuery => {
                // xterm XTMODKEYS query: CSI ? Pp m
                // We track Pp=4 (modifyOtherKeys). For Pp=0..3 (modifyCursorKeys,
                // modifyFunctionKeys, modifyKeyboard, modifyStringKeys), we respond
                // with the default indicator `CSI > Pp m` (no value = disabled)
                // to prevent programs from hanging on a missing response (#7696).
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    let response = self.xterm_keyboard.query_modify_other_keys_response();
                    self.send_response(cap, response.as_bytes());
                } else if pp <= 3 {
                    // Unimplemented modifier key resource — respond with default
                    let response = format!("\x1b[>{pp}m");
                    self.send_response(cap, response.as_bytes());
                }
            }
            CsiHandler::XtermFmtkeysQuery => {
                // xterm XTFMTKEYS query: CSI ? Pp g
                // Same pattern: Pp=4 is tracked, others get default response (#7696).
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
                // DECST8C: Set tab stops at every 8 columns (#7553).
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
                // XTMODKEYS: CSI > Pp ; Pv m (set) or CSI > Pp m (reset)
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    if let Some(&pv) = params.get(1) {
                        self.xterm_keyboard.set_modify_other_keys(sgr_color_u8(pv));
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
                // XTFMTKEYS: CSI > Pp ; Pv f (set) or CSI > Pp f (reset)
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    if let Some(&pv) = params.get(1) {
                        self.xterm_keyboard.set_format_other_keys(sgr_color_u8(pv));
                    } else {
                        self.xterm_keyboard.reset_format_other_keys();
                    }
                }
            }
            CsiHandler::Xtversion => {
                // XTVERSION: CSI > Ps q — Query terminal version.
                let ps = params.first().copied().unwrap_or(0);
                if ps == 0 {
                    let response =
                        format!("\x1bP>|aterm({})\x1b\\", aterm_types::version::APP_VERSION);
                    self.send_response(cap, response.as_bytes());
                }
            }

            // --- Kitty keyboard (>, =, < intermediates) ---
            // With the capability toggled off (host policy: the platform consumes
            // kitty sequences itself), push/set/pop are consumed-and-ignored — no
            // state change, nothing forwarded — exactly how an unrecognized CSI
            // is treated ("silently ignored per VT spec", see module doc).
            CsiHandler::KittyKeyboardPush => {
                // CSI > flags u: push current flags, set new. Per Kitty
                // spec, if flags omitted default to zero (#7475). Undefined
                // high bits are MASKED, never saturated: kitty computes
                // `val & 0x7f`, so `CSI > 256 u` means flags 0 — the
                // sgr_color_u8 saturation turned it into "all enhancements".
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
                }
            }
            CsiHandler::KittyKeyboardSet => {
                if self.modes.kitty_keyboard_enabled {
                    // Same mask-don't-saturate rule for the flags; the mode
                    // param keeps saturation (every value >= 4 is equally
                    // undefined and `apply` no-ops it uniformly).
                    let flags = (params.first().copied().unwrap_or(0) & 0b1_1111) as u8;
                    let mode = sgr_color_u8(params.get(1).copied().unwrap_or(1));
                    self.kitty_keyboard.set_flags(flags, mode);
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
                self.grid.unscroll_from_scrollback(n);
            }
            CsiHandler::Decic => {
                // DECIC operates within the horizontal margins; with DECLRMM
                // (mode 69) off the margins are the full width, so it still
                // applies (matches xterm + DEC STD 070). insert_columns enforces
                // the cursor-in-margins bound internally.
                self.cursor_state().handle_decic(params);
            }
            CsiHandler::Decdc => {
                self.cursor_state().handle_decdc(params);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: verify z4-verified dispatch table routes every sequence correctly
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify all 73 z4 table entries resolve to the expected `CsiHandler`.
    ///
    /// This ensures the z4 dispatch table in `csi_dispatch_table.rs` is
    /// correctly wired into the handler dispatch path.
    #[test]
    fn test_z4_table_all_standard_sequences_resolve() {
        // Standard CSI sequences (no intermediates)
        let standard_cases: &[(&[u8], u8, CsiHandler)] = &[
            (&[], b'A', CsiHandler::CursorUp),
            (&[], b'B', CsiHandler::CursorDown),
            (&[], b'C', CsiHandler::CursorForward),
            (&[], b'D', CsiHandler::CursorBackward),
            (&[], b'E', CsiHandler::CursorNextLine),
            (&[], b'F', CsiHandler::CursorPrevLine),
            (&[], b'G', CsiHandler::CursorHorizAbs),
            (&[], b'H', CsiHandler::CursorPosition),
            (&[], b'I', CsiHandler::ForwardTab),
            (&[], b'J', CsiHandler::EraseDisplay),
            (&[], b'K', CsiHandler::EraseLine),
            (&[], b'L', CsiHandler::InsertLines),
            (&[], b'M', CsiHandler::DeleteLines),
            (&[], b'P', CsiHandler::DeleteChars),
            (&[], b'S', CsiHandler::ScrollUp),
            (&[], b'T', CsiHandler::ScrollDown),
            (&[], b'X', CsiHandler::EraseChars),
            (&[], b'Z', CsiHandler::BackwardTab),
            (&[], b'@', CsiHandler::InsertChars),
            (&[], b'`', CsiHandler::CharPositionAbs),
            (&[], b'a', CsiHandler::CharPositionRelative),
            (&[], b'b', CsiHandler::RepeatChar),
            (&[], b'c', CsiHandler::PrimaryDA),
            (&[], b'd', CsiHandler::LinePositionAbs),
            (&[], b'e', CsiHandler::LinePositionRel),
            (&[], b'f', CsiHandler::CursorPositionAlt),
            (&[], b'g', CsiHandler::TabClear),
            (&[], b'h', CsiHandler::SetMode),
            (&[], b'l', CsiHandler::ResetMode),
            (&[], b'm', CsiHandler::Sgr),
            (&[], b'n', CsiHandler::DeviceStatusReport),
            (&[], b'r', CsiHandler::SetScrollRegion),
            (&[], b's', CsiHandler::SaveCursor),
            (&[], b't', CsiHandler::Xtwinops),
            (&[], b'u', CsiHandler::RestoreCursor),
        ];

        for &(intermediates, final_byte, expected) in standard_cases {
            let result = lookup_csi_handler(intermediates, final_byte);
            assert_eq!(
                result,
                Some(expected),
                "standard: lookup({intermediates:?}, 0x{final_byte:02X}) = {result:?}, expected {expected:?}"
            );
        }
    }

    /// Verify all intermediate-based CSI sequences resolve correctly.
    #[test]
    fn test_z4_table_all_intermediate_sequences_resolve() {
        let intermediate_cases: &[(&[u8], u8, CsiHandler)] = &[
            // DEC private modes (? intermediate)
            (&[b'?'], b'h', CsiHandler::DecModeSet),
            (&[b'?'], b'l', CsiHandler::DecModeReset),
            (&[b'?'], b'J', CsiHandler::SelectiveEraseDisplay),
            (&[b'?'], b'K', CsiHandler::SelectiveEraseLine),
            (&[b'?'], b'u', CsiHandler::KittyKeyboardQuery),
            (&[b'?'], b'm', CsiHandler::XtermModkeysQuery),
            (&[b'?'], b'g', CsiHandler::XtermFmtkeysQuery),
            (&[b'?'], b'S', CsiHandler::Xtsmgraphics),
            (&[b'?'], b'n', CsiHandler::DecDsr),
            (&[b'?'], b's', CsiHandler::Xtsave),
            (&[b'?'], b'r', CsiHandler::Xtrestore),
            (&[b'?'], b'W', CsiHandler::Decst8c),
            // Two-byte intermediate ?$
            (&[b'?', b'$'], b'p', CsiHandler::Decrqm),
            // xterm extensions (> intermediate)
            (&[b'>'], b'c', CsiHandler::SecondaryDA),
            (&[b'>'], b'm', CsiHandler::XtmodkeysSet),
            (&[b'>'], b'n', CsiHandler::XtmodkeysDisable),
            (&[b'>'], b'f', CsiHandler::XtfmtkeysSet),
            (&[b'>'], b'q', CsiHandler::Xtversion),
            (&[b'>'], b'u', CsiHandler::KittyKeyboardPush),
            // Device attributes (= intermediate)
            (&[b'='], b'c', CsiHandler::TertiaryDA),
            (&[b'='], b'u', CsiHandler::KittyKeyboardSet),
            // Kitty pop (< intermediate)
            (&[b'<'], b'u', CsiHandler::KittyKeyboardPop),
            // Misc intermediates
            (&[b'!'], b'p', CsiHandler::Decstr),
            (&[b' '], b'q', CsiHandler::Decscusr),
            (&[b' '], b'k', CsiHandler::Scp),
            (&[b'"'], b'q', CsiHandler::Decsca),
            (&[b'"'], b'p', CsiHandler::Decscl),
            (&[b'#'], b'{', CsiHandler::Xtpushsgr),
            (&[b'#'], b'}', CsiHandler::Xtpopsgr),
            (&[b'$'], b'p', CsiHandler::AnsiDecrqm),
            (&[b'$'], b'r', CsiHandler::Deccara),
            (&[b'$'], b'v', CsiHandler::Deccra),
            (&[b'$'], b'x', CsiHandler::Decfra),
            (&[b'$'], b'z', CsiHandler::Decera),
            (&[b'$'], b'{', CsiHandler::Decsera),
            (&[b'+'], b'T', CsiHandler::KittyUnscroll),
            (&[b'\''], b'}', CsiHandler::Decic),
            (&[b'\''], b'~', CsiHandler::Decdc),
        ];

        for &(intermediates, final_byte, expected) in intermediate_cases {
            let result = lookup_csi_handler(intermediates, final_byte);
            assert_eq!(
                result,
                Some(expected),
                "intermediate: lookup({intermediates:?}, 0x{final_byte:02X}) = {result:?}, expected {expected:?}"
            );
        }
    }

    /// Verify unrecognized sequences return `None` from the z4 table.
    #[test]
    fn test_z4_table_unrecognized_returns_none() {
        // Unknown final byte with no intermediates
        assert_eq!(lookup_csi_handler(&[], b'Q'), None);
        // Unknown intermediate byte
        assert_eq!(lookup_csi_handler(&[b'%'], b'A'), None);
        // Known intermediate, unknown final byte
        assert_eq!(lookup_csi_handler(&[b'?'], b'z'), None);
        // Three-byte intermediates (unsupported)
        assert_eq!(lookup_csi_handler(&[b'?', b'$', b'!'], b'p'), None);
        // Two-byte intermediate that isn't ?$
        assert_eq!(lookup_csi_handler(&[b'>', b'$'], b'p'), None);
    }

    /// Verify total entry count matches expected 73 handlers.
    #[test]
    fn test_z4_table_total_handler_count() {
        // 35 standard (no intermediates) + 38 with intermediates = 73 total
        // Verified by the csi_dispatch_table::tests::test_entry_count test.
        // Here we just verify both paths resolve the expected count.
        let standard_count = [
            b'A', b'B', b'C', b'D', b'E', b'F', b'G', b'H', b'I', b'J', b'K', b'L', b'M', b'P',
            b'S', b'T', b'X', b'Z', b'@', b'`', b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h',
            b'l', b'm', b'n', b'r', b's', b't', b'u',
        ]
        .iter()
        .filter(|&&fb| lookup_csi_handler(&[], fb).is_some())
        .count();
        assert_eq!(standard_count, 35, "expected 35 standard CSI handlers");
    }

    /// Regression: an INVALID DECSLRM (left >= right) must be a complete no-op,
    /// mirroring the DECSTBM validity guard. Previously the handler called
    /// `set_horizontal_margins(40, 30)` unconditionally — which resets margins to
    /// FULL WIDTH when `left >= right` — and then homed the cursor, silently
    /// destroying a previously-set valid margin region and repositioning the
    /// cursor on a stray/invalid sequence (xterm CASE_DECSLRM ignores it).
    #[test]
    fn decslrm_invalid_left_ge_right_is_noop() {
        use crate::terminal::Terminal;

        let mut term = Terminal::new(24, 80);
        // DECLRMM on (mode 69) so `CSI s` is DECSLRM, then set a VALID region
        // [10, 50] (1-indexed 11;51) — this also homes the cursor.
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

        // Must be a COMPLETE no-op: margins and cursor both preserved.
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
        // Default-param DECSLRM resets to full width and homes.
        term.process(b"\x1b[s");
        assert_eq!(
            term.grid().horizontal_margins(),
            aterm_grid::HorizontalMargins { left: 0, right: 79 },
            "default DECSLRM resets to full width"
        );
        assert_eq!((term.cursor().row, term.cursor().col), (0, 0));
    }
}
