// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! State transitions for `TerminalHandler`.

use super::handler::CursorStateHandler;
use super::types::CurrentStyle;
use super::{CursorStyle, SavedCursorState};
use crate::vt_level::VtLevel;
use aterm_types::CharacterSetState;

/// Whether `col` of `row` is a line's end, where a printed glyph arms the deferred wrap: the
/// row's last column, or the right margin under DECSLRM.
fn at_a_line_end(grid: &crate::grid::Grid, row: u16, col: u16) -> bool {
    let max_col = grid.effective_cols_for_row(row).saturating_sub(1);
    col >= max_col || col == grid.horizontal_margins().right
}

/// Put the cursor where DECRC or `?1049l` restores `state`, at `(row, col)` (the slot after any
/// origin-mode clamp), and put the slot's pending wrap back wherever the column lands, as xterm
/// (`do_wrap = sc->wrap_flag`) and Ghostty (`restoreCursor`) do: a wrap saved before the margins
/// moved still sends the next glyph to the next line. The exception is a slot a resize marked
/// (`SavedCursorState::resized`) whose cursor lands off a line end. The reflow moved that wrap
/// mid-line, so the cursor moves on to the next cell of its row with no wrap pending, as
/// Ghostty's `Screen.resize` moves a saved wrap, and the next glyph neither overwrites the one
/// the wrap was armed on nor joins the next row to the line.
pub(super) fn place_restored_cursor(
    grid: &mut crate::grid::Grid,
    state: &SavedCursorState,
    row: u16,
    col: u16,
) {
    grid.set_cursor_following_content(row, col);
    if !state.pending_wrap {
        return;
    }
    let landed = grid.cursor();
    if state.resized && !at_a_line_end(grid, landed.row, landed.col) {
        grid.set_cursor_following_content(landed.row, landed.col.saturating_add(1));
    } else {
        grid.set_pending_wrap(true);
    }
}

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
            resized: false,
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
            place_restored_cursor(self.grid, &state, row, col);
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
            b'G' | b'`' => {
                // CHA and HPA are both xterm's `CASE_HPA`: under DECOM and DECLRMM
                // the column counts from the left margin and stops at the right.
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
#[path = "handler_state_erase_pending_wrap_tests.rs"]
mod erase_pending_wrap_tests;

#[cfg(test)]
#[path = "handler_state_motion_counts_columns_tests.rs"]
mod motion_counts_columns_tests;
