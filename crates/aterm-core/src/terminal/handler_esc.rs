// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! ESC dispatch (VT52 and ANSI) behind `ActionSink::esc_dispatch`.

use crate::grid::LineSize;

use super::TerminalHandler;
use aterm_types::charset::{CharacterSet, CharacterSet96, SingleShift};

impl TerminalHandler<'_> {
    /// VT52 escapes: `A`/`B`/`C`/`D` cursor up/down/right/left, `H` home, `I` reverse line
    /// feed, `J`/`K` erase to end of screen/line, `Y row col` direct addressing (each +32),
    /// `Z` identify (answers `ESC / Z`), `<` back to ANSI, `F`/`G` enter/leave graphics (G0
    /// line drawing), `=`/`>` keypad application/numeric. None takes intermediates.
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
                // ESC J and ESC K erase with the SGR background (BCE).
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
                // Row and column follow as the next two printables.
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

    /// Dispatch `ESC [<intermediates>] <final>`: VT52 mode first, then no intermediates
    /// (DECSC/DECRC, IND/RI/NEL, HTS, shifts, RIS, keypad modes), `#` (line sizes, DECALN),
    /// else character set designation (SCS).
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
                // IND. The BCE refresh here and in RI/NEL is defensive, as for CSI S/T.
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // IND, RI and NEL scroll within the horizontal margins under DECLRMM (VT510).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            b'M' => {
                // RI
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            b'E' => {
                // NEL
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            b'H' => {
                // HTS
                self.grid.set_tab_stop();
            }
            b'N' => {
                // SS2: G2 for the next character
                self.charset.single_shift = SingleShift::Ss2;
            }
            b'O' => {
                // SS3: G3 for the next character
                self.charset.single_shift = SingleShift::Ss3;
            }
            b'n' => {
                // LS2: G2 into GL
                self.charset.gl = aterm_types::charset::GlMapping::G2;
            }
            b'o' => {
                // LS3: G3 into GL
                self.charset.gl = aterm_types::charset::GlMapping::G3;
            }
            b'~' => {
                // LS1R: G1 into GR
                self.charset.gr = aterm_types::charset::GrMapping::G1;
            }
            b'}' => {
                // LS2R: G2 into GR
                self.charset.gr = aterm_types::charset::GrMapping::G2;
            }
            b'|' => {
                // LS3R: G3 into GR
                self.charset.gr = aterm_types::charset::GrMapping::G3;
            }
            b'6' => {
                self.handle_decbi();
            }
            b'9' => {
                self.handle_decfi();
            }
            b'Z' => {
                // DECID, the identify request older than DA1, gets DA1's answer (VT510).
                self.handle_primary_da(cap);
            }
            b'c' => self.reset_terminal_state(), // RIS
            b'=' => {
                // DECKPAM
                self.set_application_keypad(true);
            }
            b'>' => {
                // DECKPNM
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
                    // b'6' is the only remaining value from the outer arm
                    _ => LineSize::DoubleWidth, // DECDWL
                };
                let row = self.grid.cursor().row;
                let mut changed = false;
                if let Some(row_data) = self.grid.row_mut(row) {
                    changed = row_data.line_size() != size;
                    row_data.set_line_size(size);
                }
                // A line-size change is a content change of the row, so readers
                // keyed on `content_gen` must see it. DECDWL/DECDHL also erase the
                // hidden half below (which marks it); DECSWL writes only the flag.
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
                        // The hidden second half is erased, or its stale content would
                        // reappear after DECSWL.
                        self.grid.erase_rect(row, half, row, cols.saturating_sub(1));
                        self.grid.extras_mut().clear_range(row, half, cols);
                    }
                }
                let col = self.grid.cursor().col;
                self.grid.set_cursor(row, col);
            }
            b'8' => {
                // DECALN
                self.grid.screen_alignment_pattern();
            }
            _ => {}
        }
    }

    const fn esc_dispatch_charset_designation(&mut self, intermediates: &[u8], final_byte: u8) {
        match intermediates.len() {
            1 => {}
            2 => {
                // Two-intermediate SCS (VT420/VT510): ESC ( % 5 designates DEC Supplemental
                // into G0.
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
        // SCS: ESC ( ) * + C designates G0-G3.
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

        // 96-character set designation (ESC - / ESC . / ESC /).
        // Maps to G1/G2/G3 respectively (G0 cannot hold 96-char sets).
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
        // RIS. `reset_common_fields` keeps the host's policy (`reset_modes`): a program's
        // `ESC c` must not clear an `allow_*` bit or the shell-integration nonce requirement
        // (it could then forge OSC 133/633 marks), nor toggle an input capability. The CWD
        // is kept: it is filesystem state.
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
        // RIS leaves the alternate screen.
        self.holder_fence.note_alt_exit();

        // The BiDi cache may hold resolutions of the pre-reset content.
        self.invalidate_bidi_all();

        // The RIS side effects this handler cannot reach (secure keyboard entry, the parked
        // selection) run in the processing loop after advance_fast returns; the parser itself
        // is not reset. Set after reset_common_fields, whose transient.reset() clears it.
        self.transient.pending_parser_reset = true;

        // The host's default cursor style survives RIS: host preference, not app state.
        self.modes.cursor_style = *self.default_cursor_style;
        // Mode 12 mirrors the style's blink bit.
        self.modes.cursor_blink = self.modes.cursor_style.blinks();
        // So does the host's alternate scroll (1007).
        self.modes.alternate_scroll = *self.host_alternate_scroll;
    }

    /// DECBI, Back Index (ESC 6, VT420+): at the left margin (column 0 without DECLRMM), scroll
    /// the region within the margins right one column, the new column BCE blank, cursor
    /// still; a no-op outside the scroll region rows (xterm). Elsewhere, CUB 1.
    fn handle_decbi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let left_bound = if self.modes.left_right_margin_mode {
            margins.left
        } else {
            0
        };

        // BCE refresh, as for DECIC/DECDC.
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

    /// DECFI, Forward Index (ESC 9, VT420+): at the right margin (the last column without
    /// DECLRMM), scroll the region within the margins left one column, the new column BCE
    /// blank, cursor still; a no-op outside the scroll region rows (xterm). Elsewhere, CUF 1.
    fn handle_decfi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let right_bound = if self.modes.left_right_margin_mode {
            margins.right
        } else {
            self.grid.cols().saturating_sub(1)
        };

        // BCE refresh, as for DECIC/DECDC.
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
