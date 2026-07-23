// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! ESC dispatch helpers for terminal escape sequence handling.
//!
//! This module contains VT52 and ANSI ESC routing logic used by
//! `ActionSink::esc_dispatch`.

use crate::grid::LineSize;

use super::TerminalHandler;
use aterm_types::charset::{CharacterSet, CharacterSet96, SingleShift};

impl TerminalHandler<'_> {
    /// Handle VT52 mode escape sequences.
    ///
    /// VT52 uses simpler escape sequences than ANSI mode:
    /// - ESC A: Cursor up
    /// - ESC B: Cursor down
    /// - ESC C: Cursor right
    /// - ESC D: Cursor left
    /// - ESC H: Cursor home
    /// - ESC I: Reverse line feed
    /// - ESC J: Erase to end of screen
    /// - ESC K: Erase to end of line
    /// - ESC Y row col: Direct cursor addressing (row/col encoded as +32)
    /// - ESC Z: Identify (respond with ESC / Z)
    /// - ESC <: Exit VT52 mode (return to ANSI)
    /// - ESC F: Enter graphics mode (VT52 special graphics)
    /// - ESC G: Exit graphics mode
    /// - ESC =: Enter alternate keypad mode
    /// - ESC >: Exit alternate keypad mode
    fn vt52_esc_dispatch(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        intermediates: &[u8],
        final_byte: u8,
    ) {
        // VT52 sequences don't use intermediates
        if !intermediates.is_empty() {
            return;
        }

        match final_byte {
            b'A' => {
                // Cursor up
                let cursor = self.grid.cursor();
                if cursor.row > 0 {
                    self.grid.set_cursor(cursor.row - 1, cursor.col);
                }
            }
            b'B' => {
                // Cursor down
                let cursor = self.grid.cursor();
                let rows = self.grid.rows();
                if cursor.row + 1 < rows {
                    self.grid.set_cursor(cursor.row + 1, cursor.col);
                }
            }
            b'C' => {
                // Cursor right
                let cursor = self.grid.cursor();
                let cols = self.grid.cols();
                if cursor.col + 1 < cols {
                    self.grid.set_cursor(cursor.row, cursor.col + 1);
                }
            }
            b'D' => {
                // Cursor left
                let cursor = self.grid.cursor();
                if cursor.col > 0 {
                    self.grid.set_cursor(cursor.row, cursor.col - 1);
                }
            }
            b'H' => {
                // Cursor home
                self.grid.set_cursor(0, 0);
            }
            b'I' => {
                // Reverse line feed
                self.grid.reverse_line_feed();
            }
            b'J' => {
                // Erase to end of screen (BCE: inherit SGR background, #7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid.erase_to_end_of_screen();
            }
            b'K' => {
                // Erase to end of line (BCE: inherit SGR background, #7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid.erase_to_end_of_line();
            }
            b'Y' => {
                // Direct cursor addressing - need two more bytes
                self.transient.vt52_cursor_state = super::Vt52CursorState::WaitingRow;
            }
            b'Z' => {
                // Identify - respond with ESC / Z (VT52 identification)
                self.send_response(cap, b"\x1b/Z");
            }
            b'<' => {
                // Exit VT52 mode (return to ANSI mode)
                self.set_vt52_mode(false);
            }
            b'F' => {
                // Enter graphics mode (use special graphics character set)
                // In VT52, this maps to G0 = special graphics
                self.charset.designate(0, CharacterSet::DecLineDrawing);
            }
            b'G' => {
                // Exit graphics mode (use ASCII character set)
                self.charset.designate(0, CharacterSet::Ascii);
            }
            b'=' => {
                // Enter alternate keypad mode
                self.set_application_keypad(true);
            }
            b'>' => {
                // Exit alternate keypad mode
                self.set_application_keypad(false);
            }
            _ => {} // Unknown VT52 sequence
        }
    }

    /// Dispatch ESC (Escape) sequences.
    ///
    /// ESC sequences have the format: `ESC [<intermediates>] <final_byte>`
    ///
    /// # Dispatch Structure
    ///
    /// 1. **VT52 mode**: When enabled, delegates to `vt52_esc_dispatch`
    /// 2. **No intermediates**: Direct ESC sequences like:
    ///    - `ESC 7/8`: DECSC/DECRC (save/restore cursor)
    ///    - `ESC D/M/E`: IND/RI/NEL (index, reverse index, next line)
    ///    - `ESC H`: HTS (horizontal tab set)
    ///    - `ESC N/O`: SS2/SS3 (single shift)
    ///    - `ESC c`: RIS (full reset)
    ///    - `ESC =/>`: DECKPAM/DECKPNM (keypad modes)
    /// 3. **With `#` intermediate**: DEC line attributes (DECDHL, DECDWL, DECALN)
    /// 4. **With `()*+` intermediates**: Character set designation (SCS)
    ///
    /// See `docs/ESCAPE_SEQUENCE_MATRIX.md` for complete ESC coverage.
    pub(super) fn esc_dispatch_core(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        intermediates: &[u8],
        final_byte: u8,
    ) {
        // Handle VT52 mode escape sequences
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
                // IND (Index) - Capture newline for CopyToClipboard
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\n');
                }
                // Defensive BCE template refresh (consistent with CSI S/T, #7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // Per VT510: when DECLRMM is active, IND at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            b'M' => {
                // RI (Reverse Index)
                // Defensive BCE template refresh (consistent with CSI S/T, #7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                // Per VT510: when DECLRMM is active, RI at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            b'E' => {
                // NEL (Next Line) - Capture newline for CopyToClipboard
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\n');
                }
                // Defensive BCE template refresh (consistent with CSI S/T, #7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
                // Per VT510: when DECLRMM is active, NEL at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            b'H' => {
                // HTS - Horizontal Tab Set
                self.grid.set_tab_stop();
            }
            b'N' => {
                // SS2 - Single Shift 2 (use G2 for next character)
                self.charset.single_shift = SingleShift::Ss2;
            }
            b'O' => {
                // SS3 - Single Shift 3 (use G3 for next character)
                self.charset.single_shift = SingleShift::Ss3;
            }
            b'n' => {
                // LS2 - Locking Shift 2 (invoke G2 into GL)
                self.charset.gl = aterm_types::charset::GlMapping::G2;
            }
            b'o' => {
                // LS3 - Locking Shift 3 (invoke G3 into GL)
                self.charset.gl = aterm_types::charset::GlMapping::G3;
            }
            b'~' => {
                // LS1R - Locking Shift 1 Right (invoke G1 into GR)
                self.charset.gr = aterm_types::charset::GrMapping::G1;
            }
            b'}' => {
                // LS2R - Locking Shift 2 Right (invoke G2 into GR)
                self.charset.gr = aterm_types::charset::GrMapping::G2;
            }
            b'|' => {
                // LS3R - Locking Shift 3 Right (invoke G3 into GR)
                self.charset.gr = aterm_types::charset::GrMapping::G3;
            }
            b'6' => {
                // DECBI — Back Index (VT420+).
                // If cursor is at the left margin, scroll content within the
                // scroll region right by one column within horizontal margins,
                // erasing the leftmost column. Otherwise, move cursor left by 1.
                self.handle_decbi();
            }
            b'9' => {
                // DECFI — Forward Index (VT420+).
                // If cursor is at the right margin, scroll content within the
                // scroll region left by one column within horizontal margins,
                // erasing the rightmost column. Otherwise, move cursor right by 1.
                self.handle_decfi();
            }
            b'Z' => {
                // DECID — Identify terminal (ANSI mode).
                // Per VT510 spec, ESC Z in ANSI mode sends the same response
                // as DA1 (Primary Device Attributes). This is the legacy
                // identification request predating CSI c.
                self.handle_primary_da(cap);
            }
            b'c' => self.reset_terminal_state(), // RIS
            b'=' => {
                // DECKPAM — application keypad mode.
                self.set_application_keypad(true);
            }
            b'>' => {
                // DECKPNM — normal keypad mode.
                self.set_application_keypad(false);
            }
            _ => {}
        }
    }

    fn esc_dispatch_hash_intermediate(&mut self, final_byte: u8) {
        match final_byte {
            b'3' | b'4' | b'5' | b'6' => {
                // DECDHL/DECSWL/DECDWL - Double-height/width line support
                let size = match final_byte {
                    b'3' => LineSize::DoubleHeightTop,    // DECDHL top half
                    b'4' => LineSize::DoubleHeightBottom, // DECDHL bottom half
                    b'5' => LineSize::SingleWidth,        // DECSWL
                    // b'6' is the only remaining value from the outer arm
                    _ => LineSize::DoubleWidth, // DECDWL
                };
                let row = self.grid.cursor().row;
                if let Some(row_data) = self.grid.row_mut(row) {
                    row_data.set_line_size(size);
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
                        // Erase cell content in the second half — these cells
                        // are not visible in double-width mode. Without this,
                        // stale content reappears if the line is later switched
                        // back to single-width via DECSWL (#7463).
                        self.grid.erase_rect(row, half, row, cols.saturating_sub(1));
                        self.grid.extras_mut().clear_range(row, half, cols);
                    }
                }
                let col = self.grid.cursor().col;
                self.grid.set_cursor(row, col);
            }
            b'8' => {
                // DECALN - Screen Alignment Pattern
                self.grid.screen_alignment_pattern();
            }
            _ => {}
        }
    }

    fn esc_dispatch_charset_designation(&mut self, intermediates: &[u8], final_byte: u8) {
        match intermediates.len() {
            1 => {}
            2 => {
                // Two-byte intermediate SCS: ESC I % F designates character
                // sets with % as a second intermediate byte (VT420/VT510).
                // Example: ESC ( % 5 = DEC Supplemental into G0.
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
                            // % 5 = DEC Supplemental Graphic
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
        // SCS - Select Character Set
        // ESC ( C - designate G0
        // ESC ) C - designate G1
        // ESC * C - designate G2
        // ESC + C - designate G3
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

        // 96-character set designation (ESC - / ESC . / ESC /) (#7547).
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
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "FullReset",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn reset_terminal_state(&mut self) {
        // RIS — delegates to shared reset_common_fields (#4114).
        // Capture pre-reset state for callback notification.
        let was_alt_screen = self.modes.alternate_screen;
        let old_cursor_style = self.modes.cursor_style;
        // Preserve host-configured policy flags across mode reset (#7336, #7878, #7898, #7937).
        let allow_osc52_query = self.modes.allow_osc52_query;
        let allow_osc52_set = self.modes.allow_osc52_set;
        let allow_window_ops = self.modes.allow_window_ops;
        let allow_notifications = self.modes.allow_notifications;
        // #7878 CF-010: session-memory recording is host policy; RIS
        // must not let a rogue program clear it (DoS against AI
        // retrieval) or set it (re-enable the poisoning channel
        // after the host revoked).
        let allow_session_memory = self.modes.allow_session_memory;
        // #7937 F01-3: palette reconfigure is host policy; RIS must not
        // let a rogue program clear it (DoS) or set it (privilege
        // escalation / palette takeover).
        let allow_palette_reconfigure = self.modes.allow_palette_reconfigure;
        // #7937 F01-2/#7960: the shell-integration nonce requirement is HOST
        // policy. Byte-stream RIS (ESC c) must NOT clear it — otherwise a rogue
        // program writes `\x1b c` then forges OSC 133/633 prompt/command markers
        // WITHOUT a nonce (shell_nonce_gate_ok returns true once the flag is
        // false). `Terminal::reset()` already preserves this; the two reset paths
        // had drifted, defeating exactly the anti-spoofing control.
        let require_shell_integration_nonce = self.modes.require_shell_integration_nonce;
        // Capability toggle, not runtime negotiation state: byte-stream RIS must
        // not let a program re-enable a kitty protocol the host declared
        // unsupported (mirrors Terminal::reset()).
        let kitty_keyboard_enabled = self.modes.kitty_keyboard_enabled;
        // Host/config-derived display state with no wire sequence to restore it:
        // CJK ambiguous-width policy + OS light/dark appearance (see Terminal::reset).
        let ambiguous_width_double = self.modes.ambiguous_width_double;
        let color_scheme = self.modes.color_scheme;
        // Note: CWD is NOT cleared on RIS as it represents actual filesystem state.
        let mut groups = super::super::ResetGroups {
            kitty_keyboard: self.kitty_keyboard,
            xterm_keyboard: self.xterm_keyboard,
            iterm2: self.iterm2,
            shell: self.shell,
            semantic: self.semantic,
            #[cfg(feature = "sixel")]
            sixel: self.sixel,
            title: self.title,
            dcs: self.dcs,
            notifications: self.notifications,
            clipboard: self.clipboard,
            marks_state: self.marks_state,
            taskbar_progress: self.taskbar_progress,
        };
        super::super::reset_common_fields(
            self.grid,
            self.modes,
            self.style,
            self.current_style_id,
            self.charset,
            self.alt_grid,
            self.cursor_save,
            self.transient,
            self.color,
            self.secure_keyboard_entry,
            &mut groups,
        );

        // Invalidate BiDi render cache — mode flags are reset but the cache
        // may hold stale resolutions from pre-reset content (#7488).
        self.invalidate_bidi_all();

        // Signal the parser to reset after advance_fast completes (#7153).
        // Must be set AFTER reset_common_fields (which calls transient.reset()
        // and would clear the flag if set beforehand).
        self.transient.pending_parser_reset = true;

        // Restore host policy flags that should not be affected by ESC c
        // (#7336, #7878, #7898, #7937).
        self.modes.allow_osc52_query = allow_osc52_query;
        self.modes.allow_osc52_set = allow_osc52_set;
        self.modes.allow_window_ops = allow_window_ops;
        self.modes.allow_notifications = allow_notifications;
        self.modes.allow_session_memory = allow_session_memory;
        self.modes.allow_palette_reconfigure = allow_palette_reconfigure;
        self.modes.require_shell_integration_nonce = require_shell_integration_nonce;
        self.modes.kitty_keyboard_enabled = kitty_keyboard_enabled;
        self.modes.ambiguous_width_double = ambiguous_width_double;
        self.modes.color_scheme = color_scheme;

        // Restore the host-preferred default cursor style: reset_common_fields wiped
        // modes back to CursorStyle::default(), but the host's configured default must
        // survive RIS (host preference, not app state, like the policy flags above).
        self.modes.cursor_style = *self.default_cursor_style;

        // Fire callbacks for state that changed and has UI side-effects.
        if old_cursor_style != self.modes.cursor_style {
            if let Some(callback) = self.cursor_style_callback {
                callback(self.modes.cursor_style);
            }
        }
        if was_alt_screen {
            if let Some(callback) = self.buffer_activation_callback {
                callback(false);
            }
        }
    }

    /// DECBI — Back Index (ESC 6, VT420+).
    ///
    /// When the cursor is at the left margin (or column 0 when DECLRMM is not
    /// active) AND within the vertical scroll region, scrolls content within
    /// the scroll region and horizontal margins right by one column, erasing
    /// the leftmost margin column with the BCE template. The cursor does not
    /// move.
    ///
    /// When the cursor is at the left margin but outside the scroll region,
    /// the operation is a no-op (per xterm).
    ///
    /// When the cursor is not at the left margin, moves the cursor left by one
    /// column (equivalent to CUB 1).
    fn handle_decbi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let left_bound = if self.modes.left_right_margin_mode {
            margins.left
        } else {
            0
        };

        // Defensive BCE template refresh (consistent with DECIC/DECDC, #7522).
        if col == left_bound {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }

        if col == left_bound {
            // At the left margin — scroll content right, but only
            // if the cursor row is within the scroll region. Per xterm,
            // DECBI at the left margin is a no-op when the cursor is
            // outside the scroll region rows (#7692).
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
            // Not at left margin — move cursor left by 1.
            self.grid
                .cursor_backward_margin(1, self.modes.left_right_margin_mode);
        }
    }

    /// DECFI — Forward Index (ESC 9, VT420+).
    ///
    /// When the cursor is at the right margin (or last column when DECLRMM is
    /// not active) AND within the vertical scroll region, scrolls content
    /// within the scroll region and horizontal margins left by one column,
    /// erasing the rightmost margin column with the BCE template. The cursor
    /// does not move.
    ///
    /// When the cursor is at the right margin but outside the scroll region,
    /// the operation is a no-op (per xterm).
    ///
    /// When the cursor is not at the right margin, moves the cursor right by
    /// one column (equivalent to CUF 1).
    fn handle_decfi(&mut self) {
        let col = self.grid.cursor_col();
        let margins = self.grid.horizontal_margins();
        let right_bound = if self.modes.left_right_margin_mode {
            margins.right
        } else {
            self.grid.cols().saturating_sub(1)
        };

        // Defensive BCE template refresh (consistent with DECIC/DECDC, #7522).
        if col == right_bound {
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
        }

        if col == right_bound {
            // At the right margin — scroll content left, but only
            // if the cursor row is within the scroll region. Per xterm,
            // DECFI at the right margin is a no-op when the cursor is
            // outside the scroll region rows (#7692).
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
            // Not at right margin — move cursor right by 1.
            self.grid
                .cursor_forward_margin(1, self.modes.left_right_margin_mode);
        }
    }
}
