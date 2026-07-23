// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal state transition methods for `TerminalHandler`.
//!
//! This module implements the **state transition layer** of the terminal handler
//! concern separation (#2157). Methods here apply parsed operations to the
//! terminal grid and mode state. They depend on `grid`, `bidi`, `config`, and
//! `vt_level` — but never invoke platform services, callbacks, or I/O.
//!
//! ## Concern layers
//!
//! - **Parser actions** (`handler_actions.rs`): `ActionSink` dispatch from parser events
//! - **State transitions** (this file): grid/mode mutations from typed operations
//! - **Side-effects**: callbacks and external service activation (inline in handler files)

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

    /// Restore cursor state (DECRC).
    ///
    /// Restores cursor position, style, origin mode, and charset state.
    /// DECAWM is never touched (xterm DECSC_FLAGS excludes WRAPAROUND).
    /// If no cursor was saved, moves cursor to home position.
    ///
    /// Per VT510 specification: When origin mode is restored as enabled, the cursor
    /// position is clamped to the current scroll region. The saved cursor position
    /// is always absolute (not relative to scroll region), but when origin mode is
    /// active the cursor must remain within the scroll region bounds.
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
            // Restore modes first so we know if origin mode will be active.
            // NOTE: DECAWM is deliberately NOT restored — xterm's
            // CursorRestoreFlags only applies DECSC_FLAGS =
            // (ATTRIBUTES|ORIGIN|PROTECTED) (cursor.c); WRAPAROUND is
            // untouched by DECSC/DECRC. The VT510 "wrap flag" DECSC saves is
            // the PENDING-wrap state (xterm sc->wrap_flag = do_wrap), which
            // IS restored below.
            self.modes.origin_mode = state.origin_mode;
            *self.style = state.style;
            *self.charset = state.charset;
            *self.underline_color = state.underline_color;
            // Refresh the cached extras flag — the restored underline_color
            // may differ from the pre-restore value (#7403).
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
            // Restore pending_wrap after set_cursor (which clears it) (#7283).
            // Only restore if cursor is still at the right edge — after terminal
            // resize the saved position may no longer be at the margin, making
            // a deferred wrap invalid (#7645).
            if state.pending_wrap {
                let max_col = self.grid.effective_cols_for_row(row).saturating_sub(1);
                self.grid.set_pending_wrap(col >= max_col);
            }
            // Update BCE cursor template from restored style (#7522).
            self.style.update_cached_colors();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        } else {
            // Per VT510 spec (DEC STD 070): if no cursor was previously
            // saved, DECRC moves to home position, resets origin mode,
            // and turns off all character attributes (#7493).
            // DECAWM is left UNCHANGED: xterm CursorRestoreFlags clears only
            // DECSC_FLAGS = (ATTRIBUTES|ORIGIN|PROTECTED) — WRAPAROUND is
            // not part of the DECSC/DECRC state.
            self.modes.origin_mode = false;
            *self.style = CurrentStyle::default();
            *self.charset = CharacterSetState::default();
            *self.underline_color = None;
            *self.has_transient_extras = self.has_hyperlink;
            self.grid.set_cursor(0, 0);
            // Reset BCE cursor template to default (#7522).
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
                if self.modes.grapheme_cluster_mode {
                    self.cursor_forward_graphemes(n);
                } else {
                    self.grid
                        .cursor_forward_margin(n, self.modes.left_right_margin_mode);
                }
            }
            b'D' => {
                // Cursor Backward — respects left margin (DECLRMM)
                if self.modes.grapheme_cluster_mode {
                    self.cursor_backward_graphemes(n);
                } else {
                    self.grid
                        .cursor_backward_margin(n, self.modes.left_right_margin_mode);
                }
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
                // Cursor Character Absolute (CHA).
                // Per xterm: CHA is NOT affected by DECLRMM — it always
                // uses absolute column positions regardless of origin mode
                // and horizontal margins (#7498).
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

    /// Move cursor forward by n grapheme clusters (Mode 2027).
    ///
    /// Instead of moving by n cells, moves to the start of the n-th next grapheme.
    /// A grapheme cluster includes wide characters and their continuation cells.
    ///
    /// Implementation: Skip over WIDE_CONTINUATION cells (spacers after wide chars).
    /// The COMPLEX flag indicates multi-codepoint storage but doesn't affect movement.
    ///
    /// Note: WIDE_CONTINUATION and PROTECTED share bit 10 in CellFlags.
    /// To disambiguate, we check that the preceding cell has WIDE set — a true
    /// continuation cell is always to the right of a WIDE cell.
    pub(super) fn cursor_forward_graphemes(&mut self, n: u16) {
        use crate::grid::CellFlags;

        let row = self.grid.cursor_row();
        let mut col = self.grid.cursor_col();
        // When DECLRMM is active and cursor is within margins, clamp to right margin.
        let max_col = if self.modes.left_right_margin_mode {
            let margins = self.grid.horizontal_margins();
            let grid_max = self.grid.cols().saturating_sub(1);
            if col >= margins.left && col <= margins.right {
                margins.right.min(grid_max)
            } else {
                grid_max
            }
        } else {
            self.grid.cols().saturating_sub(1)
        };
        let mut graphemes_passed = 0u16;

        // Move forward, counting graphemes (non-continuation cells)
        while graphemes_passed < n && col < max_col {
            col += 1;
            // Check if this is a true wide continuation cell (not just PROTECTED).
            // WIDE_CONTINUATION and PROTECTED share bit 10 — disambiguate by
            // verifying the cell to the left has the WIDE flag.
            if let Some(cell) = self.grid.cell(row, col) {
                let is_continuation = cell.flags().contains(CellFlags::WIDE_CONTINUATION)
                    && col > 0
                    && self
                        .grid
                        .cell(row, col - 1)
                        .is_some_and(|prev| prev.flags().contains(CellFlags::WIDE));
                if !is_continuation {
                    graphemes_passed += 1;
                }
            } else {
                // Past end of row content - count as grapheme boundary
                graphemes_passed += 1;
            }
        }

        self.grid.set_cursor(row, col);
    }

    /// Move cursor backward by n grapheme clusters (Mode 2027).
    ///
    /// Instead of moving by n cells, moves to the start of the n-th previous grapheme.
    ///
    /// Implementation: Skip over WIDE_CONTINUATION cells when counting graphemes.
    /// See `cursor_forward_graphemes` for the WIDE_CONTINUATION/PROTECTED disambiguation.
    pub(super) fn cursor_backward_graphemes(&mut self, n: u16) {
        use crate::grid::CellFlags;

        let row = self.grid.cursor_row();
        let mut col = self.grid.cursor_col();
        // When DECLRMM is active and cursor is within margins, clamp to left margin.
        let min_col = if self.modes.left_right_margin_mode {
            let margins = self.grid.horizontal_margins();
            if col >= margins.left && col <= margins.right {
                margins.left
            } else {
                0
            }
        } else {
            0
        };
        let mut graphemes_passed = 0u16;

        // Move backward, counting graphemes (non-continuation cells)
        // Loop exits when we've passed N graphemes or hit left bound
        while graphemes_passed < n && col > min_col {
            col -= 1;
            // Check if this is a true wide continuation cell (not just PROTECTED).
            if let Some(cell) = self.grid.cell(row, col) {
                let is_continuation = cell.flags().contains(CellFlags::WIDE_CONTINUATION)
                    && col > 0
                    && self
                        .grid
                        .cell(row, col - 1)
                        .is_some_and(|prev| prev.flags().contains(CellFlags::WIDE));
                if !is_continuation {
                    graphemes_passed += 1;
                }
            } else {
                // Empty cell counts as grapheme boundary
                graphemes_passed += 1;
            }
        }

        self.grid.set_cursor(row, col);
    }

    /// Handle erase CSI sequences.
    ///
    /// Updates the BCE cursor template from current SGR background before
    /// dispatching to the grid, per VT420/xterm BCE spec (#7522).
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

    /// Handle scroll operations.
    ///
    /// CSI Ps S (SU) - Scroll Up: Scroll text within scroll region up by Ps lines.
    /// CSI Ps T (SD) - Scroll Down: Scroll text within scroll region down by Ps lines.
    ///
    /// Per VT510: These sequences scroll within the scroll region, not the entire screen.
    /// New blank lines appear at the bottom (SU) or top (SD) of the scroll region.
    /// Per VT420: When DECLRMM is active with horizontal margins, only the cells
    /// within the margin region are scrolled (rectangular scroll).
    ///
    /// Updates the BCE cursor template before dispatching (#7522).
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

    /// Handle insert/delete operations.
    ///
    /// ICH/DCH respect horizontal margins when DECLRMM is active (#7320).
    /// Updates the BCE cursor template before dispatching (#7522).
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
                // Insert characters (ICH) — margin-aware (#7320)
                self.grid.insert_chars_margin(n, lrmm);
            }
            b'P' => {
                // Delete characters (DCH) — margin-aware (#7320)
                self.grid.delete_chars_margin(n, lrmm);
            }
            b'L' => {
                // Insert lines (IL) — margin-aware (#7408)
                self.grid.insert_lines_margined(n as usize, lrmm);
            }
            b'M' => {
                // Delete lines (DL) — margin-aware (#7408)
                self.grid.delete_lines_margined(n as usize, lrmm);
            }
            _ => {}
        }
    }

    /// Handle DECIC — Insert Column (VT420+).
    ///
    /// CSI Pn ' }
    /// Insert Pn blank columns at cursor column for each row in the scroll
    /// region. Content shifts right within horizontal margins; rightmost
    /// columns at the margin are lost.
    ///
    /// Updates the BCE cursor template before dispatching (#7522).
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

    /// Handle DECDC — Delete Column (VT420+).
    ///
    /// CSI Pn ' ~
    /// Delete Pn columns at cursor column for each row in the scroll
    /// region. Content shifts left within horizontal margins; blank
    /// columns appear at the right margin.
    ///
    /// Updates the BCE cursor template before dispatching (#7522).
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

    /// Handle DECSCL - Set Conformance Level.
    ///
    /// CSI Pl ; Pc " p
    /// - Pl: Level (61=VT100, 62=VT200, 63=VT300, 64=VT400, 65=VT500)
    /// - Pc: C1 mode (0/2=8-bit, 1=7-bit), ignored for VT100
    ///
    /// Per VT510 spec, changing conformance level performs a soft reset.
    /// We follow xterm's behavior: just update the level without full reset.
    pub(super) fn handle_decscl(&mut self, params: &[u16]) {
        // Get first parameter (level)
        let level_param = match params.first().copied() {
            Some(p) => match u8::try_from(p) {
                Ok(v) => v,
                Err(_) => return,
            },
            _ => return, // Required parameter
        };

        // Parse level
        let Some(level) = VtLevel::from_decscl_param(level_param) else {
            return; // Invalid level, ignore
        };

        // Update conformance level
        self.modes.vt_level = level;

        // Second parameter (C1 control transmission mode) is deliberately ignored:
        // aterm has a FIXED 7-bit (S7C1T) transmission policy and never emits 8-bit
        // C1 bytes, regardless of what's requested. This is the safe posture (8-bit
        // C1 interpretation is off by default for injection safety; transmitting
        // 8-bit C1 would be the mirror hazard). The DECRQSS DECSCL report mirrors
        // this exactly — it reports 7-bit (see handler_decrqss.rs's
        // DECSCL_C1_TRANSMISSION_7BIT), so the set-side and report-side agree.
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
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetCursorStyle",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    pub(super) fn handle_decscusr(&mut self, params: &[u16]) {
        let mode = params.first().copied().unwrap_or(0);
        if let Some(style) = CursorStyle::from_param(mode) {
            let changed = self.modes.cursor_style != style;
            self.modes.cursor_style = style;
            if changed {
                // A cursor-shape change repaints the cursor cell without changing any
                // cell's content; mark grid damage so `damage_epoch` advances and the
                // new shape isn't swallowed by the frontend's redraw early-out (cf.
                // DECTCEM `show_cursor`). The blink-class transition (steady<->blinking)
                // already self-heals via the frontend blink timer; this covers a
                // within-class change (e.g. steady block -> steady bar) with no grid write.
                self.grid.damage_mut().mark_full();
                if let Some(callback) = self.cursor_style_callback {
                    callback(style);
                }
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
    /// - 0: Reset to default character direction (Auto)
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

        self.modes.bidi_direction = new_direction;
    }
}
