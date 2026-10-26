// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! ESC dispatch (VT52 and ANSI) behind `ActionSink::esc_dispatch`.

use crate::grid::LineSize;

use super::TerminalHandler;
use aterm_types::charset::{CharacterSet, CharacterSet96, SingleShift};

impl TerminalHandler<'_> {
    fn vt52_esc_dispatch(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        intermediates: &[u8],
        final_byte: u8,
    ) {
        if !intermediates.is_empty() {
            return;
        }

        match final_byte {
            b'A' => {
                let cursor = self.grid.cursor();
                if cursor.row > 0 {
                    self.grid
                        .set_cursor_following_content(cursor.row - 1, cursor.col);
                }
            }
            b'B' => {
                let cursor = self.grid.cursor();
                let rows = self.grid.rows();
                if cursor.row + 1 < rows {
                    self.grid
                        .set_cursor_following_content(cursor.row + 1, cursor.col);
                }
            }
            b'C' => {
                let cursor = self.grid.cursor();
                let cols = self.grid.cols();
                if cursor.col + 1 < cols {
                    self.grid.set_cursor(cursor.row, cursor.col + 1);
                }
            }
            b'D' => {
                let cursor = self.grid.cursor();
                if cursor.col > 0 {
                    self.grid.set_cursor(cursor.row, cursor.col - 1);
                }
            }
            b'H' => {
                self.grid.set_cursor(0, 0);
            }
            b'I' => {
                self.grid.reverse_line_feed();
            }
            b'J' => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid.erase_to_end_of_screen();
            }
            b'K' => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid.erase_to_end_of_line();
            }
            b'Y' => {
                self.transient.vt52_cursor_state = super::Vt52CursorState::WaitingRow;
            }
            b'Z' => {
                self.send_response(cap, b"\x1b/Z");
            }
            b'<' => {
                self.set_vt52_mode(false);
            }
            b'F' => {
                self.charset.designate(0, CharacterSet::DecLineDrawing);
            }
            b'G' => {
                self.charset.designate(0, CharacterSet::Ascii);
            }
            b'=' => {
                self.set_application_keypad(true);
            }
            b'>' => {
                self.set_application_keypad(false);
            }
            _ => {} // Unknown VT52 sequence
        }
    }

    /// Dispatches `ESC [<intermediates>] <final>` across VT52, ANSI, `#`, and SCS forms.
    pub(super) fn esc_dispatch_core(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        intermediates: &[u8],
        final_byte: u8,
    ) {
        if self.modes.vt52_mode {
            self.vt52_esc_dispatch(cap, intermediates, final_byte);
            return;
        }

        if intermediates.is_empty() {
            self.esc_dispatch_no_intermediates(cap, final_byte);
            return;
        }
        if intermediates == *b"#" {
            self.esc_dispatch_hash_intermediate(final_byte);
            return;
        }
        self.esc_dispatch_charset_designation(intermediates, final_byte);
    }

    fn esc_dispatch_no_intermediates(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        final_byte: u8,
    ) {
        match final_byte {
            b'7' => self.cursor_state().save_cursor_state(), // DECSC
            b'8' => self.cursor_state().restore_cursor_state(), // DECRC
            b'D' => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            b'M' => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            b'E' => {
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // xterm `CASE_NEL`: the index reads the cursor's column before the
                // carriage return, so past the right margin nothing scrolls.
                self.margined_line_feed(self.modes.left_right_margin_mode);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            b'H' => {
                self.grid.set_tab_stop();
            }
            b'N' => {
                self.charset.single_shift = SingleShift::Ss2;
            }
            b'O' => {
                self.charset.single_shift = SingleShift::Ss3;
            }
            b'n' => {
                self.charset.gl = aterm_types::charset::GlMapping::G2;
            }
            b'o' => {
                self.charset.gl = aterm_types::charset::GlMapping::G3;
            }
            b'~' => {
                self.charset.gr = aterm_types::charset::GrMapping::G1;
            }
            b'}' => {
                self.charset.gr = aterm_types::charset::GrMapping::G2;
            }
            b'|' => {
                self.charset.gr = aterm_types::charset::GrMapping::G3;
            }
            b'6' => {
                self.handle_decbi();
            }
            b'9' => {
                self.handle_decfi();
            }
            b'Z' => {
                self.handle_primary_da(cap);
            }
            b'c' => self.reset_terminal_state(), // RIS
            b'=' => {
                self.set_application_keypad(true);
            }
            b'>' => {
                self.set_application_keypad(false);
            }
            _ => {}
        }
    }

    fn esc_dispatch_hash_intermediate(&mut self, final_byte: u8) {
        match final_byte {
            b'3' | b'4' | b'5' | b'6' => {
                let size = match final_byte {
                    b'3' => LineSize::DoubleHeightTop,    // DECDHL top half
                    b'4' => LineSize::DoubleHeightBottom, // DECDHL bottom half
                    b'5' => LineSize::SingleWidth,        // DECSWL
                    _ => LineSize::DoubleWidth,           // DECDWL
                };
                let row = self.grid.cursor().row;
                let mut changed = false;
                if let Some(row_data) = self.grid.row_mut(row) {
                    changed = row_data.line_size() != size;
                    row_data.set_line_size(size);
                }
                if changed {
                    self.grid.mark_content_cell(row, 0);
                }
                let cols = self.grid.cols();
                if matches!(
                    size,
                    LineSize::DoubleWidth
                        | LineSize::DoubleHeightTop
                        | LineSize::DoubleHeightBottom
                ) {
                    self.grid.mark_has_double_width();
                    let half = (cols / 2).max(1);
                    if half < cols {
                        self.grid.erase_rect(row, half, row, cols.saturating_sub(1));
                        self.grid.extras_mut().clear_range(row, half, cols);
                    }
                }
                let col = self.grid.cursor().col;
                self.grid.set_cursor(row, col);
            }
            b'8' => {
                self.grid.screen_alignment_pattern();
            }
            _ => {}
        }
    }

    const fn esc_dispatch_charset_designation(&mut self, intermediates: &[u8], final_byte: u8) {
        match intermediates.len() {
            1 => {}
            2 => {
                if intermediates[1] == b'%' {
                    let g_set = match intermediates[0] {
                        b'(' => Some(0u8),
                        b')' => Some(1u8),
                        b'*' => Some(2u8),
                        b'+' => Some(3u8),
                        _ => None,
                    };
                    if let Some(g) = g_set {
                        let charset = match final_byte {
                            b'5' => Some(CharacterSet::DecSupplemental),
                            _ => None,
                        };
                        if let Some(charset) = charset {
                            self.charset.designate(g, charset);
                        }
                    }
                }
                return;
            }
            _ => return,
        }
        let g_set = match intermediates[0] {
            b'(' => Some(0u8),
            b')' => Some(1u8),
            b'*' => Some(2u8),
            b'+' => Some(3u8),
            _ => None,
        };
        if let Some(g) = g_set {
            if let Some(charset) = CharacterSet::from_final_byte(final_byte) {
                self.charset.designate(g, charset);
            }
            return;
        }

        match intermediates[0] {
            b'-' => {
                if let Some(cs96) = CharacterSet96::from_final_byte(final_byte) {
                    self.charset.designate_96(1, cs96);
                }
            }
            b'.' => {
                if let Some(cs96) = CharacterSet96::from_final_byte(final_byte) {
                    self.charset.designate_96(2, cs96);
                }
            }
            b'/' => {
                if let Some(cs96) = CharacterSet96::from_final_byte(final_byte) {
                    self.charset.designate_96(3, cs96);
                }
            }
            _ => {}
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "FullReset",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn reset_terminal_state(&mut self) {
        let mut groups = super::super::ResetGroups {
            kitty_keyboard: self.kitty_keyboard,
            xterm_keyboard: self.xterm_keyboard,
            iterm2: self.iterm2,
            shell: self.shell,
            sixel: self.sixel,
            title: self.title,
            dcs: self.dcs,
            notifications: self.notifications,
            marks_state: self.marks_state,
            taskbar_progress: self.taskbar_progress,
            program_state: self.program_state,
        };
        super::super::reset_common_fields(
            self.grid,
            self.modes,
            self.style,
            self.charset,
            self.alt_grid,
            self.cursor_save,
            self.transient,
            self.color,
            self.secure_keyboard_entry,
            &mut groups,
        );
        self.holder_fence.note_alt_exit();
        self.invalidate_bidi_all();
        self.transient.pending_parser_reset = true;
        self.modes.cursor_style = *self.default_cursor_style;
        self.modes.cursor_blink = self.modes.cursor_style.blinks();
        self.modes.alternate_scroll = *self.host_alternate_scroll;
    }

    fn handle_decbi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let left_bound = if self.modes.left_right_margin_mode {
            margins.left
        } else {
            0
        };

        if col == left_bound {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }

        if col == left_bound {
            let region = self.grid.scroll_region();
            let row = self.grid.cursor_row();
            if row >= region.top && row <= region.bottom {
                let right = if self.modes.left_right_margin_mode {
                    margins.right
                } else {
                    self.grid.cols().saturating_sub(1)
                };
                self.grid.back_index(left_bound, right);
            }
        } else {
            self.grid
                .cursor_backward_margin(1, self.modes.left_right_margin_mode);
        }
    }

    fn handle_decfi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let right_bound = if self.modes.left_right_margin_mode {
            margins.right
        } else {
            self.grid.cols().saturating_sub(1)
        };

        if col == right_bound {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }

        if col == right_bound {
            let region = self.grid.scroll_region();
            let row = self.grid.cursor_row();
            if row >= region.top && row <= region.bottom {
                let left = if self.modes.left_right_margin_mode {
                    margins.left
                } else {
                    0
                };
                self.grid.forward_index(left, right_bound);
            }
        } else {
            self.grid
                .cursor_forward_margin(1, self.modes.left_right_margin_mode);
        }
    }
}
