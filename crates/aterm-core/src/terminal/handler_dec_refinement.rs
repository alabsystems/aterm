// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Included from `handler_dec.rs` (`include!`), sharing its private helpers.

// The mode-2026 sync deadline runs on the injected instant, so no `#[expect]` may excuse a
// clock read here.
#[forbid(clippy::disallowed_methods)]
impl TerminalHandler<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetApplicationCursorKeys",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_application_cursor_keys(&mut self) {
        self.modes.application_cursor_keys = true;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetApplicationCursorKeys",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_application_cursor_keys(&mut self) {
        self.modes.application_cursor_keys = false;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetOriginMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetOriginMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_origin_mode(&mut self) {
        self.modes.origin_mode = false;
        self.grid.set_cursor(0, 0);
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetAutoWrap",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_auto_wrap(&mut self) {
        self.modes.auto_wrap = true;
        // pending_wrap is untouched: xterm srm_DECAWM only flips the flag (charproc.c). A
        // margin-filling print arms it even with autowrap off, and turning DECAWM back on
        // lets the next printable wrap.
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetAutoWrap",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_auto_wrap(&mut self) {
        self.modes.auto_wrap = false;
        // pending_wrap is untouched (see enable_auto_wrap): with DECAWM off the next print
        // consumes it without wrapping.
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetCursorVisible",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn show_cursor(&mut self) {
        if !self.modes.cursor_visible {
            self.modes.cursor_visible = true;
            // A visibility change alters the frame but no cell: damage the cursor cell so the
            // frontend's redraw early-out does not swallow a bare DECTCEM, and only that cell
            // is refilled (TUIs bracket every repaint in `?25l` … `?25h`).
            self.grid.mark_cursor_damage();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetCursorVisible",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn hide_cursor(&mut self) {
        if self.modes.cursor_visible {
            self.modes.cursor_visible = false;
            // The cell the cursor is leaving is the cell that must repaint.
            self.grid.mark_cursor_damage(); // see `show_cursor`
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetFocusReporting",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_focus_reporting(&mut self) {
        self.modes.focus_reporting = true;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetFocusReporting",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_focus_reporting(&mut self) {
        self.modes.focus_reporting = false;
    }

    // Ends the model's SGR encoding (see `enable_utf8_mouse_encoding`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetSgrMouseEncoding",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_utf8_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Utf8;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "clears only its own UTF-8 encoding; the SGR 1006 fact the model tracks is unchanged"
        )
    )]
    #[inline]
    fn disable_utf8_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Utf8 {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    // Ends the model's SGR encoding (see `enable_utf8_mouse_encoding`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetSgrMouseEncoding",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_urxvt_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::Urxvt;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "clears only its own URXVT encoding; the SGR 1006 fact the model tracks is unchanged"
        )
    )]
    #[inline]
    fn disable_urxvt_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::Urxvt {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    // Ends the model's SGR encoding (see `enable_utf8_mouse_encoding`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetSgrMouseEncoding",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_sgr_pixel_mouse_encoding(&mut self) {
        self.modes.mouse_encoding = MouseEncoding::SgrPixel;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "clears only its own SGR pixel encoding; the SGR 1006 fact the model tracks is unchanged"
        )
    )]
    #[inline]
    fn disable_sgr_pixel_mouse_encoding(&mut self) {
        if self.modes.mouse_encoding == MouseEncoding::SgrPixel {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetSynchronizedOutput",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    fn enable_synchronized_output(&mut self) {
        let opening = !self.modes.synchronized_output;
        self.modes.synchronized_output = true;
        // Clock seam: the timeout runs from the batch's logical instant, so it replays.
        self.transient.sync_start = Some(self.transient.process_now);
        // Only a false->true edge starts a clean episode: a redundant DECSET 2026 must not
        // erase the evidence of a partial frame already on the grid.
        if opening {
            self.transient.sync_open_dirty = false;
        }
        // The program's own episode opens only when none is open: a `?2026h` after the
        // terminal's timeout force-clear reopens the mode, but the program's frame is still
        // unfinished on the grid.
        if !self.transient.app_sync_open {
            self.transient.app_sync_open = true;
            self.transient.app_sync_dirty = false;
            self.transient.app_sync_open_mark =
                Some((self.grid.content_gen(), self.transient.screen_replaced));
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetSynchronizedOutput",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    pub(super) fn disable_synchronized_output(&mut self) {
        // The program finished a frame it drew into: until something is drawn after it,
        // the screen is that frame (`Terminal::screen_is_closed_frame`). A stray `?2026l`
        // with no frame of the program's open, or one that drew nothing, finishes nothing:
        // the screen is what it was before the `?2026h`.
        let drawn = (self.grid.content_gen(), self.transient.screen_replaced);
        if self.transient.app_sync_open && self.transient.app_sync_open_mark != Some(drawn) {
            self.transient.app_sync_closed = Some(super::transient_state::ClosedFrame {
                content_gen: self.grid.content_gen(),
                screen: self.transient.screen_replaced,
                grid: (self.grid.rows(), self.grid.cols()),
            });
        }
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_open_dirty = false;
        self.transient.app_sync_open = false;
        self.transient.app_sync_dirty = false;
        self.transient.app_sync_open_mark = None;
        // ESU: a complete frame is ready; the host's present-hold releases on this edge
        // (`Terminal::sync_end_seq`).
        self.transient.sync_end_seq += 1;
        let cursor = self.grid.cursor();
        self.transient.sync_end_caret = Some(super::transient_state::SyncEndCaret {
            seq: self.transient.sync_end_seq,
            screen: self.transient.screen_replaced,
            grid: (self.grid.rows(), self.grid.cols()),
            caret: (cursor.row, cursor.col),
        });
    }

    // --- ANSI modes with TLA+ actions ---

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetInsertMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_insert_mode(&mut self) {
        self.modes.insert_mode = true;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetInsertMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_insert_mode(&mut self) {
        self.modes.insert_mode = false;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SetNewLineMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn enable_new_line_mode(&mut self) {
        self.modes.new_line_mode = true;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "ResetNewLineMode",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    #[inline]
    fn disable_new_line_mode(&mut self) {
        self.modes.new_line_mode = false;
    }

    // --- DEC modes not modeled in TLA+ ---

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model VT52 compatibility (DECANM mode 2)"
        )
    )]
    #[inline]
    pub(super) fn set_vt52_mode(&mut self, enabled: bool) {
        // Leaving VT52 (ESC <) resets the character sets (VT220), so G0 line drawing from
        // VT52 graphics mode (ESC F) does not persist into ANSI mode.
        if !enabled && self.modes.vt52_mode {
            self.charset.reset();
        }
        self.modes.vt52_mode = enabled;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model 132-column mode (DECCOLM mode 3)"
        )
    )]
    fn set_column_mode_132(&mut self, enabled: bool) {
        let changed = self.modes.column_mode_132 != enabled;
        self.modes.column_mode_132 = enabled;
        // Toggling DECCOLM clears the screen, resets both margins and homes the cursor
        // (VT420, xterm); the column resize itself is the host's.
        if changed && !self.modes.decncsm {
            self.grid.reset_scroll_region();
            self.modes.left_right_margin_mode = false;
            self.grid.reset_horizontal_margins();
            self.grid.set_cursor_template(
                crate::grid::Cell::bce_blank(self.style.cached_colors()),
                self.style.bce_bg_rgb(),
            );
            self.grid.erase_screen();
            // DECCOLM's own guarantee, though the erase above clears DECDWL/DECDHL too.
            self.grid.clear_line_attributes();
            // DECCOLM resets tab stops to every 8 columns (VT420, xterm).
            self.grid.reset_tab_stops();
            self.grid.set_cursor(0, 0);
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model reverse video (DECSCNM mode 5)"
        )
    )]
    #[inline]
    fn set_reverse_video(&mut self, enabled: bool) {
        // Unchanged: no full-screen redraw.
        if self.modes.reverse_video == enabled {
            return;
        }
        self.modes.reverse_video = enabled;
        self.grid.damage_mut().mark_full();
    }

    // Mode 12 moves the cursor style's blink bit, so it refines the cursor-shape actions
    // beside DECSCUSR: the resulting shape is the host's (`RestoreCursorStyle`) or not
    // (`SetCursorStyle`).
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
    fn set_cursor_blink(&mut self, enabled: bool) {
        // Mode 12 is the blink bit of `cursor_style`, not a second flag beside it: the
        // renderer blinks from `cursor_style` alone, so moving only `modes.cursor_blink` would
        // change nothing on screen and make `CSI ? 12 $ p` lie. Every ncurses `curs_set()`
        // drives it (`xterm-256color`'s `cnorm` is `\E[?12l\E[?25h`, `cvvis` `\E[?12;25h`).
        self.modes.cursor_blink = enabled;
        let style = self.modes.cursor_style.with_blink(enabled);
        if self.modes.cursor_style != style {
            self.modes.cursor_style = style;
            // As for a DECSCUSR shape change: damage the cursor cell so a frontend keyed by
            // `damage_epoch` sees the change, and the host sees its cursor change class.
            self.grid.mark_cursor_damage();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model reverse wraparound (DECSET 45)"
        )
    )]
    #[inline]
    fn set_reverse_wraparound(&mut self, enabled: bool) {
        self.modes.reverse_wraparound = enabled;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model left/right margin mode (DECLRMM mode 69)"
        )
    )]
    #[inline]
    fn set_left_right_margin_mode(&mut self, enabled: bool) {
        self.modes.left_right_margin_mode = enabled;
        if !enabled {
            self.grid.reset_horizontal_margins();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model DECSDM (Sixel Display Mode, mode 80)"
        )
    )]
    #[inline]
    fn set_sixel_display_mode(&mut self, enabled: bool) {
        self.modes.sixel_display_mode = enabled;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model alternate scroll (DECSET 1007)"
        )
    )]
    #[inline]
    fn set_alternate_scroll(&mut self, enabled: bool) {
        self.modes.alternate_scroll = enabled;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model application keypad (DECKPAM/DECKPNM)"
        )
    )]
    #[inline]
    pub(super) fn set_application_keypad(&mut self, enabled: bool) {
        self.modes.application_keypad = enabled;
    }

    // --- BiDi DEC private modes (Terminal WG):
    // https://terminal-wg.pages.freedesktop.org/bidi/recommendation/escape-sequences.html

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model BiDi arrow swap (DECSET 1243)"
        )
    )]
    #[inline]
    fn set_bidi_arrow_swap(&mut self, enabled: bool) {
        self.modes.bidi_arrow_swap = enabled;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model BiDi box mirroring (DECSET 2500)"
        )
    )]
    #[inline]
    fn set_bidi_box_mirroring(&mut self, enabled: bool) {
        if self.modes.bidi_box_mirroring != enabled {
            self.modes.bidi_box_mirroring = enabled;
            // Mirroring changes the box-drawing glyphs chosen in RTL runs.
            self.invalidate_bidi_all();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model BiDi autodetection (DECSET 2501)"
        )
    )]
    #[inline]
    fn set_bidi_autodetection(&mut self, enabled: bool) {
        if self.modes.bidi_autodetection != enabled {
            self.modes.bidi_autodetection = enabled;
            self.invalidate_bidi_all();
        }
    }

    // --- ANSI mode 8: BDSM (Bidirectional Support Mode) ---

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "the TerminalModes model does not model BDSM (ANSI mode 8, Bidirectional Support Mode)"
        )
    )]
    #[inline]
    fn set_bidi_support_mode(&mut self, set: bool) {
        use crate::config::BiDiMode;
        let new_mode = if set {
            BiDiMode::Implicit
        } else {
            BiDiMode::Explicit
        };
        if self.modes.bidi_mode != new_mode {
            self.modes.bidi_mode = new_mode;
            self.invalidate_bidi_all();
        }
    }

    // --- XTSAVE/XTRESTORE (CSI ? Ps s / CSI ? Ps r): per-mode save and restore (xterm) ---

    /// The current state of a DEC private mode, for XTSAVE/XTRESTORE. It must cover every mode
    /// `handle_decrqm` reports, or `CSI ? Ps s`/`r` is a silent no-op answering like a working
    /// one; `xtsave_covers_every_mode_decrqm_reports` walks the pair.
    #[inline]
    pub(super) fn query_dec_mode(&self, mode: u16) -> Option<bool> {
        Some(match mode {
            1 => self.modes.application_cursor_keys,
            2 => !self.modes.vt52_mode,
            3 => self.modes.column_mode_132,
            5 => self.modes.reverse_video,
            6 => self.modes.origin_mode,
            7 => self.modes.auto_wrap,
            // Mode 12 reads the style's blink bit, the fact the renderer acts on (see
            // `set_cursor_blink`), not the shadow flag.
            12 => self.modes.cursor_style.blinks(),
            25 => self.modes.cursor_visible,
            40 => self.modes.deccolm_enable,
            45 => self.modes.reverse_wraparound,
            66 => self.modes.application_keypad,
            67 => self.modes.backarrow_sends_bs,
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
            // The xterm keyboard modes and the tracked-only 1045: DECRQM reports them.
            1035 => self.modes.special_modifiers,
            1036 => self.modes.meta_send_escape,
            1039 => self.modes.alt_send_escape,
            1045 => self.modes.mode_1045,
            1049 | 47 | 1047 => self.modes.alternate_screen,
            1243 => self.modes.bidi_arrow_swap,
            2004 => self.modes.bracketed_paste,
            2026 => self.modes.synchronized_output,
            2031 => self.modes.report_color_scheme,
            2048 => self.modes.in_band_size_reports,
            2500 => self.modes.bidi_box_mirroring,
            2501 => self.modes.bidi_autodetection,
            9001 if self.modes.win32_input_enabled => self.modes.win32_input_mode,
            _ => return None,
        })
    }

    /// XTSAVE (`CSI ? Ps s`): save each listed mode into its slot; no params saves nothing.
    pub(super) fn handle_xtsave(&mut self, params: &[u16]) {
        for &mode in params {
            if let Some(value) = self.query_dec_mode(mode) {
                self.transient.xtsave_modes.insert(mode, value);
            }
        }
    }

    /// XTRESTORE (`CSI ? Ps r`): restore each listed mode that was saved, through
    /// `handle_dec_mode` so side effects run (origin mode homes the cursor, DECLRMM resets
    /// margins).
    pub(super) fn handle_xtrestore(&mut self, params: &[u16]) {
        for &mode in params {
            if let Some(&saved_value) = self.transient.xtsave_modes.get(&mode) {
                // Skip a mode already at its saved value: modes share state (mouse
                // tracking), and resetting an inactive one can disable an active other.
                if self.query_dec_mode(mode) != Some(saved_value) {
                    self.handle_dec_mode(&[mode], saved_value);
                }
            }
        }
    }
}
