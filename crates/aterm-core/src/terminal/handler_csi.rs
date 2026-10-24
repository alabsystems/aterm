// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! CSI sequence dispatch through `csi_dispatch_table` and `csi_dispatch_untabled`.

use super::super::csi_dispatch_table::{CsiHandler, lookup_csi_handler};
use super::super::sgr_color_u8;
use super::TerminalHandler;
use crate::grid::CellFlags;
use aterm_types::ScreenBuffer;

impl TerminalHandler<'_> {
    /// CSI with intermediates, via the dispatch table.
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

    /// CSI without intermediates, via the dispatch table.
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

    /// The CSI sequences the table does not carry, reached only on a table miss.
    fn csi_dispatch_untabled(
        &mut self,
        params: &[u16],
        intermediates: &[u8],
        final_byte: u8,
    ) -> bool {
        match (intermediates, final_byte) {
            (b"*", b'x') => {
                match params.first().copied().unwrap_or(0) {
                    0 | 1 => self.modes.stream_attribute_extent = true,
                    2 => self.modes.stream_attribute_extent = false,
                    _ => {}
                }
                true
            }
            (b" ", b'@') => {
                self.cursor_state().handle_sl(params);
                true
            }
            (b" ", b'A') => {
                self.cursor_state().handle_sr(params);
                true
            }
            (b"$", b't') => {
                self.handle_decrara(params);
                true
            }
            (b"#", b'p') => {
                self.handle_xtpushsgr(params);
                true
            }
            (b"#", b'q') => {
                self.handle_xtpopsgr();
                true
            }
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

            CsiHandler::EraseDisplay | CsiHandler::EraseLine => {
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
                    self.transient.render_debt = crate::terminal::RenderDebt::default();
                    if self.modes.alternate_screen {
                        self.holder_fence.note_alt_cleared();
                    } else {
                        let top = self.grid.visible_to_absolute(0);
                        super::note_full_clear(top, from_home, self.shell);
                        self.holder_fence.note_full_clear(top, self.grid);
                    }
                } else if !self.modes.alternate_screen && !self.holder_fence.is_empty() {
                    let lo = match (final_byte, params.first().copied().unwrap_or(0)) {
                        (b'K', _) | (b'J', 0) => Some(self.grid.visible_to_absolute(cursor.row)),
                        (b'J', 1) => Some(self.grid.visible_to_absolute(0)),
                        _ => None,
                    };
                    if let Some(lo) = lo {
                        self.holder_fence.note_erase(lo, self.grid);
                    }
                }
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

            CsiHandler::Sgr => {
                self.sgr_style().handle_sgr(params);
            }

            CsiHandler::ScrollDown if params.len() > 1 || params == [0] => {}
            CsiHandler::ScrollUp | CsiHandler::ScrollDown => {
                self.cursor_state().handle_scroll(params, final_byte);
            }

            CsiHandler::InsertChars
            | CsiHandler::DeleteChars
            | CsiHandler::InsertLines
            | CsiHandler::DeleteLines => {
                self.cursor_state().handle_insert_delete(params, final_byte);
            }

            CsiHandler::EraseChars => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.erase_chars(n);
            }

            CsiHandler::RepeatChar => {
                self.transient.last_combining_was_zwj = false;
                let count = params.first().copied().unwrap_or(1).max(1);
                if let Some(c) = self.transient.last_graphic_char {
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
                        && !self.grid.cursor_outside_margin_span()
                    {
                        let flags = if self.style.protected {
                            self.style.flags.union(CellFlags::PROTECTED)
                        } else {
                            self.style.flags
                        };
                        let mut last_byte: Option<u8> = None;
                        #[allow(clippy::cast_possible_truncation)]
                        let ascii_byte = cp as u8;
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
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.back_tab_n_margin(
                    n,
                    self.modes.left_right_margin_mode,
                    self.modes.origin_mode,
                );
            }

            CsiHandler::ForwardTab => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.tab_n_margin(n, self.modes.left_right_margin_mode);
            }

            CsiHandler::SaveCursor => {
                if self.modes.left_right_margin_mode {
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
                    let max_right = self.grid.cols().saturating_sub(1);
                    if left < right.min(max_right) {
                        self.grid.set_horizontal_margins(left, right.min(max_right));
                        if self.modes.origin_mode {
                            let region = self.grid.scroll_region();
                            let h_margins = self.grid.horizontal_margins();
                            self.grid.set_cursor(region.top, h_margins.left);
                        } else {
                            self.grid.set_cursor(0, 0);
                        }
                    }
                } else {
                    self.cursor_state().save_cursor_state();
                }
            }

            CsiHandler::RestoreCursor => {
                self.cursor_state().restore_cursor_state();
            }

            CsiHandler::SetScrollRegion => {
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
                let max_bottom = self.grid.rows().saturating_sub(1);
                if top < bottom.min(max_bottom) {
                    self.grid.set_scroll_region(top, bottom);
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
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_primary_da(cap);
                }
            }

            CsiHandler::Xtwinops => {
                self.handle_xtwinops(cap, params);
            }

            CsiHandler::DecModeSet => self.handle_dec_mode(params, true),
            CsiHandler::DecModeReset => self.handle_dec_mode(params, false),
            CsiHandler::SelectiveEraseDisplay => {
                self.cursor_state().handle_selective_erase_display(params);
            }
            CsiHandler::SelectiveEraseLine => {
                self.cursor_state().handle_selective_erase_line(params);
            }
            CsiHandler::KittyKeyboardQuery => {
                if self.modes.kitty_keyboard_enabled {
                    self.handle_kitty_keyboard_query(cap);
                }
            }
            CsiHandler::XtermModkeysQuery => {
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    let response = crate::terminal::keyboard_mode::offered_xterm_keyboard(
                        self.modes,
                        *self.xterm_keyboard,
                    )
                    .query_modify_other_keys_response();
                    self.send_response(cap, response.as_bytes());
                } else if pp <= 3 {
                    let response = format!("\x1b[>{pp}m");
                    self.send_response(cap, response.as_bytes());
                }
            }
            CsiHandler::XtermFmtkeysQuery => {
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 {
                    let response = crate::terminal::keyboard_mode::offered_xterm_keyboard(
                        self.modes,
                        *self.xterm_keyboard,
                    )
                    .query_format_other_keys_response();
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
                if params.first().copied() == Some(5) {
                    self.grid.reset_tab_stops();
                }
            }

            CsiHandler::Decrqm => self.handle_decrqm(cap, params),

            CsiHandler::SecondaryDA => {
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_secondary_da(cap);
                }
            }
            CsiHandler::XtmodkeysSet => {
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 && !self.modes.win32_input_enabled {
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
                let ps = params.first().copied().unwrap_or(0);
                if ps == 4 && !self.modes.win32_input_enabled {
                    self.xterm_keyboard.disable_modify_other_keys();
                }
            }
            CsiHandler::XtfmtkeysSet => {
                let pp = params.first().copied().unwrap_or(0);
                if pp == 4 && !self.modes.win32_input_enabled {
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
                let ps = params.first().copied().unwrap_or(0);
                if ps == 0 {
                    let response = format!(
                        "\x1bP>|aterm({})\x1b\\",
                        aterm_types::version::app_version()
                    );
                    self.send_response(cap, response.as_bytes());
                }
            }

            CsiHandler::KittyKeyboardPush => {
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
                    *self.evidence_asserted |= super::super::program_evidence::KITTY;
                }
            }
            CsiHandler::KittyKeyboardSet => {
                if self.modes.kitty_keyboard_enabled {
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

            CsiHandler::TertiaryDA => {
                let param = params.first().copied().unwrap_or(0);
                if param == 0 {
                    self.handle_tertiary_da(cap);
                }
            }

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
                    self.transient.kitty_unscrolls = self.transient.kitty_unscrolls.wrapping_add(1);
                }
                self.grid.unscroll_from_scrollback(n);
            }
            CsiHandler::Decic => {
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
    #[test]
    fn sd_with_a_zero_or_several_parameters_is_mouse_tracking_not_a_scroll() {
        use crate::terminal::Terminal;

        let row0 = |bytes: &[u8]| {
            let mut term = Terminal::new(3, 4);
            term.process(bytes);
            term.grid().row_text(0).unwrap_or_default()
        };
        assert_eq!(row0(b"a\x1b[0T").trim_end(), "a", "CSI 0 T");
        assert_eq!(row0(b"a\x1b[1;2;3;4;5T").trim_end(), "a", "five params");
        assert_eq!(row0(b"a\x1b[2;1T").trim_end(), "a", "two params");
        assert_eq!(row0(b"a\x1b[T").trim_end(), "", "CSI T scrolls one line");
        assert_eq!(row0(b"a\x1b[1T").trim_end(), "", "CSI 1 T");
        assert_eq!(row0(b"a\x1b[0^").trim_end(), "", "CSI 0 ^ is plain SD");
    }

    #[test]
    fn decslrm_invalid_left_ge_right_is_noop() {
        use crate::terminal::Terminal;

        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?69h\x1b[11;51s");
        assert_eq!(
            term.grid().horizontal_margins(),
            aterm_grid::HorizontalMargins {
                left: 10,
                right: 50,
            },
            "valid DECSLRM sets [10,50]"
        );

        term.process(b"\x1b[6;31H");
        assert_eq!((term.cursor().row, term.cursor().col), (5, 30));

        term.process(b"\x1b[41;31s");

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
