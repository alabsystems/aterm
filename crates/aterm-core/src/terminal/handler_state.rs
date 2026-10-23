// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! State transitions for `TerminalHandler`.

use super::handler::CursorStateHandler;
use super::types::CurrentStyle;
use super::{CursorStyle, SavedCursorState};
use crate::vt_level::VtLevel;
use aterm_types::CharacterSetState;

impl CursorStateHandler<'_> {
    /// Save cursor state (DECSC) for the current screen.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "TLA+ SaveModes saves all 13 mode variables; DECSC only saves cursor + origin_mode + auto_wrap + charset + style"
        )
    )]
    pub(super) fn save_cursor_state(&mut self) {
        let saved_cursor = if self.modes.alternate_screen {
            &mut self.cursor_save.alt
        } else {
            &mut self.cursor_save.main
        };
        *saved_cursor = Some(SavedCursorState {
            cursor: self.grid.cursor(),
            style: *self.style,
            origin_mode: self.modes.origin_mode,
            auto_wrap: self.modes.auto_wrap,
            charset: *self.charset,
            pending_wrap: self.grid.pending_wrap(),
            underline_color: *self.underline_color,
        });
    }

    /// Restore cursor state (DECRC): position, style, origin mode and charset state.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "TLA+ RestoreModes restores all 13 mode variables; DECRC only restores cursor + origin_mode + charset + style"
        )
    )]
    pub(super) fn restore_cursor_state(&mut self) {
        let saved_cursor = if self.modes.alternate_screen {
            &mut self.cursor_save.alt
        } else {
            &mut self.cursor_save.main
        };
        if let Some(state) = *saved_cursor {
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            *self.underline_color = state.underline_color;
            *self.has_transient_extras = self.has_hyperlink || self.underline_color.is_some();

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
                let max_col = self.grid.effective_cols_for_row(row).saturating_sub(1);
                self.grid.set_pending_wrap(col >= max_col);
            }
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        } else {
            self.modes.origin_mode = false;
            *self.style = CurrentStyle::default();
            *self.charset = CharacterSetState::default();
            *self.underline_color = None;
            *self.has_transient_extras = self.has_hyperlink;
            self.grid.set_cursor(0, 0);
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
        }
    }

    /// Handle cursor movement CSI sequences.
    #[inline]
    pub(super) fn handle_cursor_movement(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1);

        match final_byte {
            b'A' => self.grid.cursor_up(n),   // Cursor Up - respects top margin
            b'B' => self.grid.cursor_down(n), // Cursor Down - respects bottom margin
            b'e' => self.grid.line_position_relative(n), // VPR - clamps to screen edge, not margin
            b'C' | b'a' => {
                self.grid
                    .cursor_forward_margin(n, self.modes.left_right_margin_mode);
            }
            b'D' => {
                self.grid
                    .cursor_backward_margin(n, self.modes.left_right_margin_mode);
            }
            b'E' => {
                self.grid.cursor_down(n);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            b'F' => {
                self.grid.cursor_up(n);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            b'G' => {
                let col = params.first().copied().unwrap_or(1).saturating_sub(1);
                self.grid.set_cursor(self.grid.cursor_row(), col);
            }
            b'`' => {
                let col = params.first().copied().unwrap_or(1).saturating_sub(1);
                let actual_col = if self.modes.origin_mode && self.modes.left_right_margin_mode {
                    let margins = self.grid.horizontal_margins();
                    margins.left.saturating_add(col).min(margins.right)
                } else {
                    col
                };
                self.grid.set_cursor(self.grid.cursor_row(), actual_col);
            }
            b'd' => {
                let row = params.first().copied().unwrap_or(1).saturating_sub(1);
                let actual_row = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    region.top.saturating_add(row).min(region.bottom)
                } else {
                    row
                };
                self.grid.set_cursor(actual_row, self.grid.cursor_col());
            }
            _ => {}
        }
    }

    /// Handle erase CSI sequences after updating the BCE cursor template.
    #[inline]
    pub(super) fn handle_erase(&mut self, params: &[u16], final_byte: u8) {
        let mode = params.first().copied().unwrap_or(0);

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        match final_byte {
            b'J' => {
                // Erase in Display
                match mode {
                    0 => self.grid.erase_to_end_of_screen(),
                    1 => self.grid.erase_from_start_of_screen(),
                    2 => self.grid.erase_screen(),
                    3 => self.grid.erase_scrollback(),
                    _ => {}
                }
            }
            b'K' => {
                // Erase in Line
                match mode {
                    0 => self.grid.erase_to_end_of_line(),
                    1 => self.grid.erase_from_start_of_line(),
                    2 => self.grid.erase_line(),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Handle scroll operations SU (`CSI Ps S`) and SD (`CSI Ps T`) within the scroll region.
    #[inline]
    pub(super) fn handle_scroll(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1) as usize;

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        let margins = self.grid.horizontal_margins();
        let is_margined = !margins.is_full(self.grid.cols());
        match final_byte {
            b'S' => {
                if is_margined {
                    self.grid
                        .scroll_region_up_margined(n, margins.left, margins.right);
                } else {
                    self.grid.scroll_region_up(n);
                }
            }
            b'T' => {
                if is_margined {
                    self.grid
                        .scroll_region_down_margined(n, margins.left, margins.right);
                } else {
                    self.grid.scroll_region_down(n);
                }
            }
            _ => {}
        }
    }

    /// Handle ICH (`@`), DCH (`P`), IL (`L`), and DL (`M`) within DECLRMM horizontal margins.
    #[inline]
    pub(super) fn handle_insert_delete(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1);
        let lrmm = self.modes.left_right_margin_mode;

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        match final_byte {
            b'@' => {
                self.grid.insert_chars_margin(n, lrmm);
            }
            b'P' => {
                self.grid.delete_chars_margin(n, lrmm);
            }
            b'L' => {
                self.grid.insert_lines_margined(n as usize, lrmm);
            }
            b'M' => {
                self.grid.delete_lines_margined(n as usize, lrmm);
            }
            _ => {}
        }
    }

    /// Handle DECIC, Insert Column (`CSI Pn ' }`, VT420+).
    #[inline]
    pub(super) fn handle_decic(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        self.grid.insert_columns(n);
    }

    /// Handle DECDC, Delete Column (`CSI Pn ' ~`, VT420+).
    #[inline]
    pub(super) fn handle_decdc(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);

        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        self.grid.delete_columns(n);
    }

    /// Handle SL — Scroll Left (CSI Ps SP @).
    #[inline]
    pub(super) fn handle_sl(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.scroll_left(n);
    }

    /// Handle SR — Scroll Right (CSI Ps SP A).
    #[inline]
    pub(super) fn handle_sr(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.scroll_right(n);
    }

    /// Handle DECSCA (`CSI Ps " q`): Ps 1 protects the characters written next from DECSED/DECSEL.
    #[inline]
    pub(super) fn handle_decsca(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        self.style.protected = mode == 1;
        self.style.update_cached_colors();
    }

    /// Handle DECSCL, Set Conformance Level (`CSI Pl ; Pc " p`).
    #[inline]
    pub(super) fn handle_decscl(&mut self, params: &[u16]) {
        let Some(&first) = params.first() else {
            return;
        };
        let Ok(level_param) = u8::try_from(first) else {
            return;
        };
        let Some(level) = VtLevel::from_decscl_param(level_param) else {
            return; // Invalid level, ignore
        };
        self.modes.vt_level = level;
    }

    /// Handle DECSED (`CSI ? Ps J`, VT220+).
    #[inline]
    pub(super) fn handle_selective_erase_display(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        match mode {
            0 => self.grid.selective_erase_to_end_of_screen(),
            1 => self.grid.selective_erase_from_start_of_screen(),
            2 => self.grid.selective_erase_screen(),
            _ => {}
        }
    }

    /// Handle DECSEL (`CSI ? Ps K`, VT220+).
    #[inline]
    pub(super) fn handle_selective_erase_line(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        match mode {
            0 => self.grid.selective_erase_to_end_of_line(),
            1 => self.grid.selective_erase_from_start_of_line(),
            2 => self.grid.selective_erase_line(),
            _ => {}
        }
    }

    /// Handle DECSCUSR (`CSI Ps SP q`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetCursorStyle",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "RestoreCursorStyle",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    pub(super) fn handle_decscusr(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        if let Some(style) = CursorStyle::from_param(mode) {
            let changed = self.modes.cursor_style != style;
            self.modes.cursor_style = style;
            self.modes.cursor_blink = style.blinks();
            if changed {
                self.grid.mark_cursor_damage();
            }
        }
    }

    /// Handle SCP (`CSI Ps SP k`) BiDi paragraph direction (`0` Auto, `1` LTR, `2` RTL).
    #[inline]
    pub(super) fn handle_scp(&mut self, params: &[u16]) {
        use aterm_types::ParagraphDirection;

        let mode = params.first().copied().unwrap_or(0);
        let new_direction = match mode {
            0 => ParagraphDirection::Auto,
            1 => ParagraphDirection::Ltr,
            2 => ParagraphDirection::Rtl,
            _ => return, // Invalid parameter, ignore
        };

        if self.modes.bidi_direction != new_direction {
            self.modes.bidi_direction = new_direction;
            self.grid.damage_mut().mark_full();
        }
    }
}

#[cfg(test)]
mod erase_pending_wrap_tests {
    use crate::terminal::Terminal;

    fn row_glyphs(term: &Terminal, row: u16) -> String {
        let grid = term.grid();
        (0..grid.cols())
            .map(|col| grid.cell(row, col).map_or(' ', |c| c.char()))
            .collect()
    }

    fn cursor(term: &Terminal) -> (u16, u16) {
        let c = term.cursor();
        (c.row, c.col)
    }

    fn glyph(term: &Terminal, row: u16, col: u16) -> char {
        term.grid().cell(row, col).map_or(' ', |c| c.char())
    }

    fn row_is_blank(term: &Terminal, row: u16) -> bool {
        row_glyphs(term, row).trim().is_empty()
    }

    #[test]
    fn el0_at_pending_wrap_ghostty_golden() {
        let mut term = Terminal::new(1, 5);
        term.process(b"ABCDE\x1b[KB");
        assert_eq!(row_glyphs(&term, 0), "ABCDB");
        assert_eq!(cursor(&term), (0, 4));
        assert!(term.grid().pending_wrap(), "the B re-arms the wrap");
    }

    #[test]
    fn el0_at_pending_wrap_with_decawm_off() {
        let mut term = Terminal::new(1, 5);
        term.process(b"\x1b[?7lABCDE\x1b[KB");
        assert_eq!(row_glyphs(&term, 0), "ABCDB");
        assert_eq!(cursor(&term), (0, 4));
    }

    #[test]
    fn vt52_esc_k_at_pending_wrap_clears_parked_glyph() {
        let mut term = Terminal::new(2, 5);
        term.process(b"\x1b[?2lABCDE");
        assert!(
            term.grid().pending_wrap(),
            "precondition: VT52 print armed the wrap"
        );
        term.process(b"\x1bK");
        assert_eq!(row_glyphs(&term, 0), "ABCD ");
        assert!(!term.grid().pending_wrap(), "ESC K resets the wrap");
    }

    #[test]
    fn vt52_esc_j_at_pending_wrap_clears_parked_glyph() {
        let mut term = Terminal::new(2, 5);
        term.process(b"\x1b[?2l\x1bY! xyz\x1bH");
        term.process(b"ABCDE");
        assert!(
            term.grid().pending_wrap(),
            "precondition: VT52 print armed the wrap"
        );
        term.process(b"\x1bJ");
        assert_eq!(row_glyphs(&term, 0), "ABCD ");
        assert!(row_is_blank(&term, 1), "ESC J clears the rows below");
        assert!(!term.grid().pending_wrap(), "ESC J resets the wrap");
    }

    #[test]
    fn ed_at_pending_wrap_then_print_overwrites_last_column() {
        for ed in [b"\x1b[J".as_slice(), b"\x1b[1J", b"\x1b[2J"] {
            let mut term = Terminal::new(24, 80);
            term.process(b"\x1b[1;80H!");
            term.process(ed);
            term.process(b"?");
            let label = String::from_utf8_lossy(ed);
            assert_eq!(glyph(&term, 0, 79), '?', "{label}");
            assert!(row_is_blank(&term, 1), "{label}: nothing wrapped to row 1");
            assert_eq!(cursor(&term), (0, 79), "{label}");
        }
    }

    #[test]
    fn ed0_without_a_print_clears_the_parked_glyph() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1;80H!\x1b[0J");
        assert_eq!(glyph(&term, 0, 79), ' ');
        assert_eq!(cursor(&term), (0, 79));
        assert!(!term.grid().pending_wrap());
    }

    #[test]
    fn ed1_at_bottom_right_pending_wrap_reduces_to_full_clear() {
        let mut term = Terminal::new(24, 80);
        term.process(b"top\x1b[24;80H!\x1b[1J?");
        for row in 0..23 {
            assert!(row_is_blank(&term, row), "row {row} cleared");
        }
        assert_eq!(row_glyphs(&term, 23).trim(), "?");
        assert_eq!(glyph(&term, 23, 79), '?');
        assert_eq!(cursor(&term), (23, 79));
    }

    #[test]
    fn el1_and_el2_at_pending_wrap_then_print_overwrite_the_last_column() {
        for el in ["1K", "2K"] {
            let mut term = Terminal::new(2, 5);
            term.process(format!("ABCDE\x1b[{el}B").as_bytes());
            assert_eq!(row_glyphs(&term, 0), "    B", "EL {el}");
            assert_eq!(cursor(&term), (0, 4), "EL {el}");
            assert!(row_is_blank(&term, 1), "EL {el}");
        }
    }

    #[test]
    fn el1_at_pending_wrap_four_columns_is_xterms_result_not_xterm_js() {
        let mut term = Terminal::new(2, 4);
        term.process(b"ABCD\x1b[1KZ");
        assert_eq!(row_glyphs(&term, 0), "   Z");
        assert!(row_is_blank(&term, 1));
    }

    #[test]
    fn decsel0_protected_parked_glyph_survives_but_wrap_resets() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1\"q\x1b[1;80H!\x1b[0\"q\x1b[?K");
        assert_eq!(
            glyph(&term, 0, 79),
            '!',
            "DECSCA-protected glyph survives DECSEL"
        );
        assert!(
            !term.grid().pending_wrap(),
            "ClearRight's ResetWrap still runs"
        );
        term.process(b"?");
        assert_eq!(
            glyph(&term, 0, 79),
            '?',
            "the print overwrites the parked column"
        );
        assert!(row_is_blank(&term, 1), "nothing wrapped to row 1");
    }

    #[test]
    fn alt_1047_exit_clears_the_wrap_it_hands_back() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?1047h\x1b[1;80H!\x1b[?1047l?");
        assert!(!term.is_alternate_screen(), "back on the main screen");
        assert_eq!(glyph(&term, 0, 79), '?');
        assert!(row_is_blank(&term, 1), "nothing wrapped to row 1");
    }

    #[test]
    fn alt_1049_enter_resets_wrap_and_exit_restores_it() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1;80H!\x1b[?1049h?");
        assert!(term.is_alternate_screen());
        assert_eq!(
            glyph(&term, 0, 79),
            '?',
            "alt screen: the print overwrites col 79"
        );
        assert_eq!(cursor(&term), (0, 79));
        assert!(row_is_blank(&term, 1), "alt screen: nothing wrapped");
        term.process(b"\x1b[?1049lZ");
        assert!(!term.is_alternate_screen());
        assert_eq!(
            glyph(&term, 0, 79),
            '!',
            "main screen keeps its parked glyph"
        );
        assert_eq!(
            glyph(&term, 1, 0),
            'Z',
            "the restored wrap takes the next glyph"
        );
    }

    #[test]
    fn decrc_restores_the_deferred_wrap_decsc_saved() {
        let mut term = Terminal::new(2, 5);
        term.process(b"ABCDE\x1b7\rx\x1b8Z");
        assert_eq!(row_glyphs(&term, 0), "xBCDE", "the parked glyph stays");
        assert_eq!(row_glyphs(&term, 1), "Z    ", "the restored wrap takes Z");
        assert_eq!(cursor(&term), (1, 1));
    }

    #[test]
    fn a_second_1049_enter_clears_the_alt_screen_it_finds() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[5;10H\x1b[?1049h");
        assert!(term.is_alternate_screen());
        term.process(b"\x1b[1;1HFIRST APP");
        assert_eq!(
            glyph(&term, 0, 0),
            'F',
            "the first app painted the alt screen"
        );

        term.process(b"\x1b[?1049h");
        assert!(term.is_alternate_screen(), "still on the alt screen");
        assert!(
            row_is_blank(&term, 0),
            "a second 1049 SET runs ClearScreen — the previous app's row must be gone"
        );

        term.process(b"\x1b[?1049l");
        assert!(!term.is_alternate_screen(), "back on the main screen");
        assert_eq!(
            cursor(&term),
            (4, 9),
            "the main cursor came from the FIRST save, not the second"
        );
    }

    #[test]
    fn alt_47_enter_carries_the_wrap_over() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1;80H!\x1b[?47hZ");
        assert!(term.is_alternate_screen());
        assert_eq!(glyph(&term, 1, 0), 'Z', "the carried wrap moves Z to row 1");
    }
}

#[cfg(test)]
mod motion_counts_columns_tests {
    use crate::terminal::Terminal;

    fn terminals() -> [(&'static str, Terminal); 3] {
        let plain = Terminal::new(2, 20);
        let mut asked = Terminal::new(2, 20);
        asked.process(b"\x1b[?2027h");
        let mut carried = Terminal::new(2, 20);
        carried.modes_mut().grapheme_cluster_mode = true;
        [("plain", plain), ("?2027h", asked), ("carried", carried)]
    }

    #[test]
    fn cuf_cub_hpr_and_bs_move_by_columns_over_wide_text() {
        let moves: [(&str, &[u8], u16); 5] = [
            ("CR CUF CUF", b"\r\x1b[C\x1b[C", 2),
            ("CR CUF 3", b"\r\x1b[3C", 3),
            ("CR HPR 2", b"\r\x1b[2a", 2),
            ("col 5, BS x3", b"\x1b[6G\x08\x08\x08", 2),
            ("col 5, CUB 3", b"\x1b[6G\x1b[3D", 2),
        ];
        for (label, mut term) in terminals() {
            term.process("中文ab".as_bytes());
            for (what, bytes, col) in moves {
                term.process(bytes);
                assert_eq!(term.cursor().col, col, "{label}: {what}");
            }
            term.process(b"\r\x1b[C\x1b[CX");
            let grid = term.grid();
            let row: Vec<char> = (0..6)
                .map(|col| grid.cell(0, col).map_or('?', |c| c.char()))
                .collect();
            assert_eq!(row[0], '中', "{label}");
            assert_eq!(row[2], 'X', "{label}: X written at column 2");
            assert_eq!(row[3], ' ', "{label}: 文's second half is blanked");
            assert_eq!((row[4], row[5]), ('a', 'b'), "{label}");
            assert_eq!(term.cursor().col, 3, "{label}");
        }
    }
}
