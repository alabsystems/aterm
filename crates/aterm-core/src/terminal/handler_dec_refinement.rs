// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Included from `handler_dec.rs` to keep the DEC dispatcher under the 500-line
// limit while preserving private helper visibility.

impl TerminalHandler<'_> {
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetApplicationCursorKeys",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_application_cursor_keys(&mut self) {
        self.modes.application_cursor_keys = true;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetApplicationCursorKeys",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_application_cursor_keys(&mut self) {
        self.modes.application_cursor_keys = false;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetOriginMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_origin_mode(&mut self) {
        self.modes.origin_mode = true;
        let region = self.grid.scroll_region();
        let col = if self.modes.left_right_margin_mode {
            self.grid.horizontal_margins().left
        } else {
            0
        };
        self.grid.set_cursor(region.top, col);
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetOriginMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_origin_mode(&mut self) {
        self.modes.origin_mode = false;
        self.grid.set_cursor(0, 0);
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetAutoWrap",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_auto_wrap(&mut self) {
        self.modes.auto_wrap = true;
        // pending_wrap is deliberately NOT touched: xterm srm_DECAWM only
        // flips the WRAPAROUND flag bit (charproc.c). The do_wrap flag is
        // mode-independent — a margin-filling print arms it even with
        // autowrap off, and turning DECAWM back on lets the next printable
        // consume it and wrap (revises the earlier #7552 reading).
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetAutoWrap",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_auto_wrap(&mut self) {
        self.modes.auto_wrap = false;
        // pending_wrap is deliberately NOT touched (xterm srm_DECAWM; see
        // enable_auto_wrap). A pending wrap armed while DECAWM was on is
        // consumed flag-only by the next print (no wrap) when off.
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetCursorVisible",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn show_cursor(&mut self) {
        if !self.modes.cursor_visible {
            self.modes.cursor_visible = true;
            // Cursor visibility changes the rendered frame without changing any
            // cell's content, so mark grid damage (like DECSCNM / a recolor) — else
            // `damage_epoch` doesn't advance and a bare DECTCEM with no grid write is
            // swallowed by the frontend's redraw early-out, delaying the cursor's
            // (dis)appearance until the next grid write.
            self.grid.damage_mut().mark_full();
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetCursorVisible",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn hide_cursor(&mut self) {
        if self.modes.cursor_visible {
            self.modes.cursor_visible = false;
            self.grid.damage_mut().mark_full(); // see `show_cursor`
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetFocusReporting",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_focus_reporting(&mut self) {
        self.modes.focus_reporting = true;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetFocusReporting",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_focus_reporting(&mut self) {
        self.modes.focus_reporting = false;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model UTF-8 mouse encoding"
        )
    )]
    fn enable_utf8_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Utf8;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model UTF-8 mouse encoding"
        )
    )]
    fn disable_utf8_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Utf8 {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model URXVT mouse encoding"
        )
    )]
    fn enable_urxvt_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Urxvt;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model URXVT mouse encoding"
        )
    )]
    fn disable_urxvt_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Urxvt {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model SGR pixel mouse encoding"
        )
    )]
    fn enable_sgr_pixel_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::SgrPixel;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model SGR pixel mouse encoding"
        )
    )]
    fn disable_sgr_pixel_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::SgrPixel {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetSynchronizedOutput",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_synchronized_output(&mut self) {
        let opening = !self.modes.synchronized_output;
        self.modes.synchronized_output = true;
        // Clock seam: arm the timeout from the batch's logical instant so the
        // mode-2026 timeout replays deterministically.
        self.transient.sync_start = Some(self.transient.process_now);
        // Only a REAL false->true edge starts a clean episode. A redundant
        // DECSET 2026 after writes must not erase evidence that the current
        // mutable grid already contains a partial synchronized frame.
        if opening {
            self.transient.sync_open_dirty = false;
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetSynchronizedOutput",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_synchronized_output(&mut self) {
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_open_dirty = false;
        // ESU: one sync window closed — a complete frame is ready. The host's
        // present-hold releases on this edge (see `Terminal::sync_end_seq`).
        self.transient.sync_end_seq += 1;
    }

    // --- ANSI modes with TLA+ actions ---

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetInsertMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_insert_mode(&mut self) {
        self.modes.insert_mode = true;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetInsertMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_insert_mode(&mut self) {
        self.modes.insert_mode = false;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SetNewLineMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn enable_new_line_mode(&mut self) {
        self.modes.new_line_mode = true;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "ResetNewLineMode",
            project = "aterm_core::terminal::project_modes"
        )
    )]
    fn disable_new_line_mode(&mut self) {
        self.modes.new_line_mode = false;
    }

    // --- DEC modes not modeled in TLA+ ---

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model VT52 compatibility (DECANM mode 2)"
        )
    )]
    pub(super) fn set_vt52_mode(&mut self, enabled: bool) {
        // Per VT220 spec: exiting VT52 mode (ESC <) should reset character
        // sets to defaults. Without this, G0=DecLineDrawing from VT52 graphics
        // mode (ESC F) persists into ANSI mode, causing garbled output. (#7509)
        if !enabled && self.modes.vt52_mode {
            self.charset.reset();
        }
        self.modes.vt52_mode = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model 132-column mode (DECCOLM mode 3)"
        )
    )]
    fn set_column_mode_132(&mut self, enabled: bool) {
        let changed = self.modes.column_mode_132 != enabled;
        self.modes.column_mode_132 = enabled;
        // Per VT420/xterm spec, toggling DECCOLM must clear screen,
        // reset scroll margins (both vertical and horizontal), and
        // move cursor to home (#7286).
        // The actual column resize is left to the host (flag-only).
        if changed && !self.modes.decncsm {
            self.grid.reset_scroll_region();
            self.modes.left_right_margin_mode = false;
            self.grid.reset_horizontal_margins();
            // Update BCE template from current SGR background (#7522).
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
            self.grid.erase_screen();
            // DECCOLM toggle clears DECDWL/DECDHL line attributes (#7497).
            self.grid.clear_line_attributes();
            // Per VT420 spec and xterm, DECCOLM toggle resets tab stops
            // to default every-8-column pattern.
            self.grid.reset_tab_stops();
            self.grid.set_cursor(0, 0);
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model reverse video (DECSCNM mode 5)"
        )
    )]
    fn set_reverse_video(&mut self, enabled: bool) {
        // No-op when value is unchanged — avoids unnecessary full-screen redraw.
        if self.modes.reverse_video == enabled {
            return;
        }
        self.modes.reverse_video = enabled;
        self.grid.damage_mut().mark_full();
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model cursor blink (mode 12)"
        )
    )]
    fn set_cursor_blink(&mut self, enabled: bool) {
        self.modes.cursor_blink = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model reverse wraparound (DECSET 45)"
        )
    )]
    fn set_reverse_wraparound(&mut self, enabled: bool) {
        self.modes.reverse_wraparound = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model left/right margin mode (DECLRMM mode 69)"
        )
    )]
    fn set_left_right_margin_mode(&mut self, enabled: bool) {
        self.modes.left_right_margin_mode = enabled;
        if !enabled {
            self.grid.reset_horizontal_margins();
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model DECSDM (Sixel Display Mode, mode 80)"
        )
    )]
    fn set_sixel_display_mode(&mut self, enabled: bool) {
        self.modes.sixel_display_mode = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model alternate scroll (DECSET 1007)"
        )
    )]
    fn set_alternate_scroll(&mut self, enabled: bool) {
        self.modes.alternate_scroll = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model grapheme cluster mode (mode 2027)"
        )
    )]
    fn set_grapheme_cluster_mode(&mut self, enabled: bool) {
        self.modes.grapheme_cluster_mode = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model application keypad (DECKPAM/DECKPNM)"
        )
    )]
    pub(super) fn set_application_keypad(&mut self, enabled: bool) {
        self.modes.application_keypad = enabled;
    }

    // --- BiDi DEC Private Modes (Terminal WG specification) ---
    // See: https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model BiDi arrow swap (DECSET 1243)"
        )
    )]
    fn set_bidi_arrow_swap(&mut self, enabled: bool) {
        self.modes.bidi_arrow_swap = enabled;
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model BiDi box mirroring (DECSET 2500)"
        )
    )]
    fn set_bidi_box_mirroring(&mut self, enabled: bool) {
        if self.modes.bidi_box_mirroring != enabled {
            self.modes.bidi_box_mirroring = enabled;
            // Invalidate BiDi cache — mirroring affects rendered glyph selection
            // for box-drawing characters in RTL contexts.
            self.invalidate_bidi_all();
        }
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model BiDi autodetection (DECSET 2501)"
        )
    )]
    fn set_bidi_autodetection(&mut self, enabled: bool) {
        if self.modes.bidi_autodetection != enabled {
            self.modes.bidi_autodetection = enabled;
            // Invalidate cache since resolution logic changes
            self.invalidate_bidi_all();
        }
    }

    // --- ANSI mode 8: BDSM (Bidirectional Support Mode) ---

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "TerminalModes.tla does not model BDSM (ANSI mode 8, Bidirectional Support Mode)"
        )
    )]
    fn set_bidi_support_mode(&mut self, set: bool) {
        use crate::config::BiDiMode;
        let new_mode = if set {
            BiDiMode::Implicit
        } else {
            BiDiMode::Explicit
        };
        if self.modes.bidi_mode != new_mode {
            self.modes.bidi_mode = new_mode;
            // Invalidate cache since BiDi algorithm behavior may differ
            self.invalidate_bidi_all();
        }
    }

    // --- XTSAVE/XTRESTORE (CSI ? Ps s / CSI ? Ps r) ---
    // Per xterm specification, saves and restores individual DEC private mode values.
    // Part of #7318.

    /// Query the current boolean state of a DEC private mode.
    fn query_dec_mode(&self, mode: u16) -> Option<bool> {
        Some(match mode {
            1 => self.modes.application_cursor_keys,
            2 => !self.modes.vt52_mode,
            3 => self.modes.column_mode_132,
            5 => self.modes.reverse_video,
            6 => self.modes.origin_mode,
            7 => self.modes.auto_wrap,
            12 => self.modes.cursor_blink,
            25 => self.modes.cursor_visible,
            40 => self.modes.deccolm_enable,
            45 => self.modes.reverse_wraparound,
            66 => self.modes.application_keypad,
            69 => self.modes.left_right_margin_mode,
            80 => self.modes.sixel_display_mode,
            95 => self.modes.decncsm,
            9 => self.modes.mouse_mode == MouseMode::X10,
            1000 => self.modes.mouse_mode == MouseMode::Normal,
            1002 => self.modes.mouse_mode == MouseMode::ButtonEvent,
            1003 => self.modes.mouse_mode == MouseMode::AnyEvent,
            1004 => self.modes.focus_reporting,
            1005 => self.modes.mouse_encoding == MouseEncoding::Utf8,
            1006 => self.modes.mouse_encoding == MouseEncoding::Sgr,
            1007 => self.modes.alternate_scroll,
            1015 => self.modes.mouse_encoding == MouseEncoding::Urxvt,
            1016 => self.modes.mouse_encoding == MouseEncoding::SgrPixel,
            1049 | 47 | 1047 => self.modes.alternate_screen,
            1243 => self.modes.bidi_arrow_swap,
            2004 => self.modes.bracketed_paste,
            2026 => self.modes.synchronized_output,
            2027 => self.modes.grapheme_cluster_mode,
            2500 => self.modes.bidi_box_mirroring,
            2501 => self.modes.bidi_autodetection,
            _ => return None,
        })
    }

    /// Handle XTSAVE (CSI ? Ps s) - Save DEC Private Mode Values.
    ///
    /// Saves the current state of each listed mode into a per-mode save slot.
    /// If no parameters are provided, no modes are saved.
    pub(super) fn handle_xtsave(&mut self, params: &[u16]) {
        for &mode in params {
            if let Some(value) = self.query_dec_mode(mode) {
                self.transient.xtsave_modes.insert(mode, value);
            }
        }
    }

    /// Handle XTRESTORE (CSI ? Ps r) - Restore DEC Private Mode Values.
    ///
    /// Restores each listed mode to its previously saved value. If a mode
    /// was never saved, it is ignored. Restored modes are set/reset through
    /// `handle_dec_mode` to trigger any required side effects (e.g., origin
    /// mode homes the cursor, DECLRMM resets margins).
    pub(super) fn handle_xtrestore(&mut self, params: &[u16]) {
        for &mode in params {
            if let Some(&saved_value) = self.transient.xtsave_modes.get(&mode) {
                // Skip if current value already matches saved — avoids collateral
                // effects on shared-state modes like mouse tracking where resetting
                // one inactive mode can disable a different active mode (#7501).
                if self.query_dec_mode(mode) != Some(saved_value) {
                    self.handle_dec_mode(&[mode], saved_value);
                }
            }
        }
    }
}
