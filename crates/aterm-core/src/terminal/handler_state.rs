// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal state transition methods for `TerminalHandler`: parsed operations
//! applied to the grid and mode state, using `grid`, `bidi`, `config` and
//! `vt_level` but never platform services, callbacks or I/O.
//!
//! Layers:
//!
//! - Parser actions (`handler_actions.rs`): `ActionSink` dispatch from parser events
//! - State transitions (this file): grid/mode mutations from typed operations
//! - Side effects: callbacks and external services (inline in handler files)

use super::handler::CursorStateHandler;
use super::types::CurrentStyle;
use super::{CursorStyle, SavedCursorState};
use crate::vt_level::VtLevel;
use aterm_types::CharacterSetState;

impl CursorStateHandler<'_> {
    /// Save cursor state (DECSC).
    ///
    /// Saves cursor position, style, origin mode, auto-wrap mode, and charset state.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
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

    /// Restore cursor state (DECRC): position, style, origin mode and charset
    /// state. DECAWM is never touched (xterm's DECSC_FLAGS excludes
    /// wraparound). With nothing saved, the cursor goes home.
    ///
    /// The saved position is absolute; when origin mode is restored as
    /// enabled it is clamped to the current scroll region (VT510).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
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
            // Restore modes first so origin mode is known for clamping. DECAWM is
            // not restored (xterm's CursorRestoreFlags applies only
            // ATTRIBUTES|ORIGIN|PROTECTED, cursor.c). The VT510 "wrap flag" DECSC
            // saves is the pending-wrap state, restored below.
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            *self.underline_color = state.underline_color;
            // Refresh the cached extras flag — the restored underline_color
            // may differ from the pre-restore value.
            *self.has_transient_extras = self.has_hyperlink || self.underline_color.is_some();

            // Clamp cursor position to scroll region if origin mode is enabled.
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
            // Restore pending_wrap after set_cursor (which clears it), only if
            // the cursor is still at the right edge: after a resize the saved
            // position may no longer be at the margin.
            if state.pending_wrap {
                let max_col = self.grid.effective_cols_for_row(row).saturating_sub(1);
                self.grid.set_pending_wrap(col >= max_col);
            }
            // Update BCE cursor template from restored style.
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        } else {
            // With nothing saved, DECRC homes the cursor, resets origin mode and
            // clears character attributes (DEC STD 070). DECAWM is unchanged:
            // wraparound is not part of the DECSC/DECRC state in xterm.
            self.modes.origin_mode = false;
            *self.style = CurrentStyle::default();
            *self.charset = CharacterSetState::default();
            *self.underline_color = None;
            *self.has_transient_extras = self.has_hyperlink;
            self.grid.set_cursor(0, 0);
            // Reset BCE cursor template to default.
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
        }
    }

    /// Handle cursor movement CSI sequences.
    ///
    /// CUU (A), CUD (B), VPR (e), CNL (E), CPL (F): Per VT510, these respect scroll region margins.
    /// The cursor stops at the margin if within the scroll region, otherwise at screen edge.
    #[inline]
    pub(super) fn handle_cursor_movement(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1);

        match final_byte {
            b'A' => self.grid.cursor_up(n),   // Cursor Up - respects top margin
            b'B' => self.grid.cursor_down(n), // Cursor Down - respects bottom margin
            b'e' => self.grid.line_position_relative(n), // VPR - clamps to screen edge, not margin
            b'C' | b'a' => {
                // Cursor Forward / HPR — respects right margin (DECLRMM)
                self.grid
                    .cursor_forward_margin(n, self.modes.left_right_margin_mode);
            }
            b'D' => {
                // Cursor Backward — respects left margin (DECLRMM)
                self.grid
                    .cursor_backward_margin(n, self.modes.left_right_margin_mode);
            }
            b'E' => {
                // Cursor Next Line - respects bottom margin
                self.grid.cursor_down(n);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            b'F' => {
                // Cursor Previous Line - respects top margin
                self.grid.cursor_up(n);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            b'G' => {
                // Cursor Character Absolute (CHA): absolute column regardless of
                // origin mode and horizontal margins, as in xterm.
                let col = params.first().copied().unwrap_or(1).saturating_sub(1);
                self.grid.set_cursor(self.grid.cursor_row(), col);
            }
            b'`' => {
                // Horizontal Position Absolute (HPA).
                // Per VT510: When DECOM is set and DECLRMM is active,
                // the column is relative to the left margin.
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
                // Line Position Absolute (VPA)
                // Per VT510: Affected by origin mode (DECOM)
                let row = params.first().copied().unwrap_or(1).saturating_sub(1);
                let actual_row = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    // Clamp row to scroll region bounds (saturating to prevent u16 overflow)
                    region.top.saturating_add(row).min(region.bottom)
                } else {
                    row
                };
                self.grid.set_cursor(actual_row, self.grid.cursor_col());
            }
            // Note: b'H' | b'f' (CUP) is handled inline in handler_csi.rs
            // csi_dispatch_no_intermediates for performance. Not dispatched here.
            _ => {}
        }
    }

    /// Handle erase CSI sequences, first updating the BCE cursor template from
    /// the current SGR background.
    #[inline]
    pub(super) fn handle_erase(&mut self, params: &[u16], final_byte: u8) {
        let mode = params.first().copied().unwrap_or(0);

        // Set BCE template so erased cells inherit the current SGR background.
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

    /// Handle scroll operations: SU (`CSI Ps S`) and SD (`CSI Ps T`) scroll
    /// within the scroll region, inserting blank lines at its bottom or top
    /// (VT510). With DECLRMM margins only the cells inside them move
    /// (rectangular scroll, VT420). Updates the BCE cursor template first.
    #[inline]
    pub(super) fn handle_scroll(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1) as usize;

        // Set BCE template so blank lines inherit the current SGR background.
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

    /// Handle insert/delete operations. ICH/DCH respect horizontal margins when
    /// DECLRMM is active. Updates the BCE cursor template first.
    #[inline]
    pub(super) fn handle_insert_delete(&mut self, params: &[u16], final_byte: u8) {
        let n = params.first().copied().unwrap_or(1).max(1);
        let lrmm = self.modes.left_right_margin_mode;

        // Set BCE template so blank cells inherit the current SGR background.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        match final_byte {
            b'@' => {
                // Insert characters (ICH) — margin-aware
                self.grid.insert_chars_margin(n, lrmm);
            }
            b'P' => {
                // Delete characters (DCH) — margin-aware
                self.grid.delete_chars_margin(n, lrmm);
            }
            b'L' => {
                // Insert lines (IL) — margin-aware
                self.grid.insert_lines_margined(n as usize, lrmm);
            }
            b'M' => {
                // Delete lines (DL) — margin-aware
                self.grid.delete_lines_margined(n as usize, lrmm);
            }
            _ => {}
        }
    }

    /// Handle DECIC, Insert Column (`CSI Pn ' }`, VT420+): insert Pn blank
    /// columns at the cursor column in each scroll-region row; content shifts
    /// right within the horizontal margins. Updates the BCE template first.
    #[inline]
    pub(super) fn handle_decic(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);

        // Set BCE template so blank cells inherit the current SGR background.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        self.grid.insert_columns(n);
    }

    /// Handle DECDC, Delete Column (`CSI Pn ' ~`, VT420+): delete Pn columns at
    /// the cursor column in each scroll-region row; content shifts left and
    /// blanks appear at the right margin. Updates the BCE template first.
    #[inline]
    pub(super) fn handle_decdc(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);

        // Set BCE template so blank cells inherit the current SGR background.
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );

        self.grid.delete_columns(n);
    }

    /// Handle SL — Scroll Left (CSI Ps SP @): scroll the scroll region left by
    /// Pn columns within the horizontal margins; blanks fill at the right.
    pub(super) fn handle_sl(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.scroll_left(n);
    }

    /// Handle SR — Scroll Right (CSI Ps SP A): scroll the scroll region right by
    /// Pn columns within the horizontal margins; blanks fill at the left.
    pub(super) fn handle_sr(&mut self, params: &[u16]) {
        let n = params.first().copied().unwrap_or(1).max(1);
        self.grid.set_cursor_template(
            crate::grid::Cell::bce_blank(self.style.cached_colors()),
            self.style.bce_bg_rgb(),
        );
        self.grid.scroll_right(n);
    }

    /// Handle DECSCA (Select Character Protection Attribute).
    ///
    /// CSI Ps " q
    /// - Ps = 0 or 2: Characters can be erased by DECSED/DECSEL (default)
    /// - Ps = 1: Characters cannot be erased by DECSED/DECSEL (protected)
    pub(super) fn handle_decsca(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        self.style.protected = mode == 1;
        self.style.update_cached_colors();
    }

    /// Handle DECSCL, Set Conformance Level (`CSI Pl ; Pc " p`).
    /// - Pl: level (61=VT100, 62=VT200, 63=VT300, 64=VT400, 65=VT500)
    /// - Pc: C1 mode (0/2=8-bit, 1=7-bit), ignored
    ///
    /// VT510 performs a soft reset on a level change; like xterm, this only
    /// updates the level.
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

        // The C1 transmission parameter is ignored: aterm always transmits
        // 7-bit (S7C1T) and never emits 8-bit C1 bytes. The DECRQSS DECSCL report
        // says the same (DECSCL_C1_TRANSMISSION_7BIT in handler_decrqss.rs).
    }

    /// Handle DECSED (Selective Erase in Display).
    ///
    /// CSI ? Ps J
    /// - Ps = 0: Erase from cursor to end of screen (only unprotected cells)
    /// - Ps = 1: Erase from start of screen to cursor (only unprotected cells)
    /// - Ps = 2: Erase entire screen (only unprotected cells)
    pub(super) fn handle_selective_erase_display(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        match mode {
            0 => self.grid.selective_erase_to_end_of_screen(),
            1 => self.grid.selective_erase_from_start_of_screen(),
            2 => self.grid.selective_erase_screen(),
            _ => {}
        }
    }

    /// Handle DECSEL (Selective Erase in Line).
    ///
    /// CSI ? Ps K
    /// - Ps = 0: Erase from cursor to end of line (only unprotected cells)
    /// - Ps = 1: Erase from start of line to cursor (only unprotected cells)
    /// - Ps = 2: Erase entire line (only unprotected cells)
    pub(super) fn handle_selective_erase_line(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        match mode {
            0 => self.grid.selective_erase_to_end_of_line(),
            1 => self.grid.selective_erase_from_start_of_line(),
            2 => self.grid.selective_erase_line(),
            _ => {}
        }
    }

    /// Handle DECSCUSR (Set Cursor Style).
    ///
    /// CSI Ps SP q
    // The parameter decides which: a shape other than the host's is
    // `SetCursorStyle`, the host's own shape `RestoreCursorStyle`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetCursorStyle",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "RestoreCursorStyle",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    pub(super) fn handle_decscusr(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        if let Some(style) = CursorStyle::from_param(mode) {
            let changed = self.modes.cursor_style != style;
            self.modes.cursor_style = style;
            // DECSCUSR carries the blink bit (1/3/5 blink, 2/4/6 steady), so it
            // moves DEC mode 12 with it and `CSI ? 12 $ p` stays accurate.
            self.modes.cursor_blink = style.blinks();
            if changed {
                // A shape change repaints only the cursor cell: mark that cell's
                // damage so `damage_epoch` advances and the change is presented
                // (steady<->blinking transitions are also covered by the blink
                // timer; this covers e.g. steady block -> steady bar).
                self.grid.mark_cursor_damage();
            }
        }
    }

    /// Handle SCP - Set Character Path (BiDi direction control).
    ///
    /// CSI Ps SP k
    ///
    /// Terminal WG BiDi escape sequence for setting paragraph direction.
    /// See: <https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html>
    ///
    /// Parameters:
    /// - 0: the terminal's default character direction (`Auto`: LTR; first-strong
    ///   detection applies only while DECSET 2501 is set)
    /// - 1: Set LTR (left-to-right) character direction
    /// - 2: Set RTL (right-to-left) character direction
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
            // Direction is applied at render-snapshot time to stored cells, so
            // mark the whole grid damaged.
            self.grid.damage_mut().mark_full();
        }
    }
}

/// Terminal-level goldens for the deferred wrap at an erase, fed as real bytes
/// through the CSI / VT52 / DECSET dispatch to the grid rule
/// (`aterm-grid` `grid/erase.rs`). Expected screens are xterm's: `ClearRight`,
/// `ClearInLine2` and `ClearScreen` (util.c) call `ResetWrap`, so the glyph
/// after the erase overwrites the parked last column instead of wrapping.
/// Several are Ghostty's goldens (`Terminal.zig` "eraseLine resets pending
/// wrap", "eraseLine left resets wrap").
#[cfg(test)]
mod erase_pending_wrap_tests {
    use crate::terminal::Terminal;

    /// The glyphs of visible row `row` of the active screen, one char per column.
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
        // VTPrsTbl.c maps VT52 'K' to CASE_EL, so ESC K takes ClearRight too.
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
        // VTPrsTbl.c maps VT52 'J' to CASE_ED.
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
        // ED 0, 1 and 2 each reset the deferred wrap, so the next print lands
        // on the parked last column instead of wrapping.
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
        // The differential seed's shape: the parked glyph is erased (alacritty
        // agrees on the cells; only the wrap differs).
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
    fn el1_at_pending_wrap_ghostty_golden() {
        let mut term = Terminal::new(2, 5);
        term.process(b"ABCDE\x1b[1KB");
        assert_eq!(row_glyphs(&term, 0), "    B");
        assert_eq!(cursor(&term), (0, 4));
        assert!(row_is_blank(&term, 1));
    }

    #[test]
    fn el1_at_pending_wrap_four_columns_is_xterms_result_not_xterm_js() {
        // xterm resets the wrap in ClearInLine2, so `Z` overwrites col 3
        // (xterm.js differs and wraps to row 1).
        let mut term = Terminal::new(2, 4);
        term.process(b"ABCD\x1b[1KZ");
        assert_eq!(row_glyphs(&term, 0), "   Z");
        assert!(row_is_blank(&term, 1));
    }

    #[test]
    fn el2_at_pending_wrap_then_print_overwrites_last_column() {
        let mut term = Terminal::new(2, 5);
        term.process(b"ABCDE\x1b[2KB");
        assert_eq!(row_glyphs(&term, 0), "    B");
        assert_eq!(cursor(&term), (0, 4));
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
        // xterm srm_OPT_ALTBUF reset: `ClearScreen` (ResetWrap) then
        // `FromAlternate`, which never writes do_wrap.
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
        // CursorSave; ToAlternate; ClearScreen — the clear runs last.
        assert_eq!(
            glyph(&term, 0, 79),
            '?',
            "alt screen: the print overwrites col 79"
        );
        assert_eq!(cursor(&term), (0, 79));
        assert!(row_is_blank(&term, 1), "alt screen: nothing wrapped");
        // CursorRestore brings back the wrap CursorSave recorded (cursor.c).
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

    /// A second `CSI ?1049h` clears the screen it finds: xterm runs
    /// `CursorSave; ToAlternate; ClearScreen` (charproc.c
    /// `srm_OPT_ALTBUF_CURSOR`) and only `ToAlternate` stands down when alt is
    /// already up.
    ///
    /// The exit assertion: `CursorSave` writes the current (alt) buffer's slot,
    /// so the main cursor 1049 reset restores is the one the first enter
    /// recorded.
    #[test]
    fn a_second_1049_enter_clears_the_alt_screen_it_finds() {
        let mut term = Terminal::new(24, 80);
        // Park the main cursor somewhere identifiable, then enter.
        term.process(b"\x1b[5;10H\x1b[?1049h");
        assert!(term.is_alternate_screen());
        term.process(b"\x1b[1;1HFIRST APP");
        assert_eq!(
            glyph(&term, 0, 0),
            'F',
            "the first app painted the alt screen"
        );

        // The second smcup must not show the first app's screen.
        term.process(b"\x1b[?1049h");
        assert!(term.is_alternate_screen(), "still on the alt screen");
        assert!(
            row_is_blank(&term, 0),
            "a second 1049 SET runs ClearScreen — the previous app's row must be gone"
        );

        // ...and it saved into the ALT slot, so the main restore is unharmed.
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
        // Mode 47 is `ToAlternate(xw, False)` with no ClearScreen: the wrap survives.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1;80H!\x1b[?47hZ");
        assert!(term.is_alternate_screen());
        assert_eq!(glyph(&term, 1, 0), 'Z', "the carried wrap moves Z to row 1");
    }
}

/// CUF, CUB, HPR and BS count columns, never grapheme clusters, whatever mode
/// 2027 says. The mode's definition (contour's terminal-unicode-core) leaves
/// "cursor movement semantics of the terminal ... unchanged", and Ghostty's
/// `cursorRight`/`cursorLeft` never read it. Neovim 0.11+ moves along a row
/// with `cuf1`/`cub1` (BS) and `CSI n C`/`CSI n D`, counting cells.
#[cfg(test)]
mod motion_counts_columns_tests {
    use crate::terminal::Terminal;

    /// Three ways a terminal can come to hold mode 2027: never asked, asked
    /// over the wire (`CSI ? 2027 h`), or a mode flag carried in from a
    /// checkpoint an older build wrote while the mode was on.
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
        // `中文ab`: 中 in columns 0-1, 文 in 2-3, a in 4, b in 5.
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
            // The program's next glyph lands where it counted: on 文's first
            // half, which erases 文, as xterm draws `中X ab`.
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
