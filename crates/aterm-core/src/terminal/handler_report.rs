// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Device status reports, device attributes, soft reset and SGR stack
//! operations for `TerminalHandler`: methods that generate responses or perform
//! cross-cutting state resets.

use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;
use super::stack_response::StackResponse;
use super::{
    MouseEncoding, MouseMode, SGR_STACK_MAX_DEPTH, SgrPushMask, SgrStackEntry, Vt52CursorState,
};
use core::fmt::Write as _;

impl TerminalHandler<'_> {
    /// Handle DECSTR (Soft Terminal Reset).
    ///
    /// CSI ! p
    ///
    /// Soft reset performs a partial reset of the terminal, resetting:
    /// - Cursor visibility (DECTCEM) → visible
    /// - Cursor style (DECSCUSR) → default (blinking block)
    /// - Origin mode (DECOM) → disabled
    /// - Auto-wrap mode (DECAWM) → enabled (xterm behavior)
    /// - Insert mode (IRM) → disabled (replace mode)
    /// - Application cursor keys (DECCKM) → disabled
    /// - Application keypad mode (DECKPAM) → disabled
    /// - Reverse wraparound mode (DECSET 45) → disabled
    /// - Left/right margin mode (DECLRMM, DECSET 69) → disabled
    /// - Reverse video mode (DECSCNM) → disabled (per VT220 §4.5.113)
    /// - Synchronized output mode (2026) → disabled
    /// - New line mode (LNM) → disabled (per VT510 spec §4.5.113)
    /// - Focus reporting (mode 1004) → disabled (per xterm)
    /// - Bracketed paste mode (2004) → disabled (per xterm)
    /// - Mouse tracking mode (9/1000/1002/1003) → disabled (per xterm)
    /// - Mouse encoding (1005/1006/1015/1016) → X10 default (per xterm)
    /// - Text attributes (SGR) → default
    /// - Character sets (G0-G3, GL) → defaults
    /// - Scroll margins (DECSTBM) → full screen
    /// - Saved cursor states → cleared
    ///
    /// Unlike RIS (hard reset), DECSTR does not reset:
    /// - Alternate screen buffer (stays on current screen)
    /// - Screen content (not erased)
    /// - Tab stops
    /// - Color palette
    /// - Working directory
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "terminal_modes",
            action = "SoftReset",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    pub(super) fn handle_decstr(&mut self) {
        self.modes.cursor_visible = true;

        // Reset cursor style to the host-preferred default (not the hardcoded
        // CursorStyle::default()), so a soft reset honors the configured default.
        self.modes.cursor_style = *self.default_cursor_style;

        self.modes.origin_mode = false;

        // Reset auto-wrap mode (xterm sets to true, VT510 says false - follow xterm)
        self.modes.auto_wrap = true;

        self.modes.insert_mode = false;

        self.modes.application_cursor_keys = false;

        // Reset application keypad mode (DECKPAM/DECKPNM)
        self.modes.application_keypad = false;

        // Reset cursor blink (mode 12). Mode 12 is the blink bit of the cursor
        // style (see `set_cursor_blink`), so it follows the default style just
        // restored rather than a hardcoded `false`.
        self.modes.cursor_blink = self.modes.cursor_style.blinks();

        // Reset VT52 mode (DECANM, mode 2)
        self.modes.vt52_mode = false;

        // Reset 132-column mode flag (DECCOLM, mode 3)
        self.modes.column_mode_132 = false;

        // Reset DECCOLM enable (mode 40) — per xterm, soft reset disables mode 40
        self.modes.deccolm_enable = false;

        // Reset DECNCSM (mode 95) — per xterm, soft reset clears DECNCSM
        self.modes.decncsm = false;

        // Reset reverse wraparound mode (DECSET 45)
        self.modes.reverse_wraparound = false;

        // Reset left/right margin mode (DECLRMM, DECSET 69) and clear the
        // horizontal margins unconditionally, so stored margins cannot come back
        // when DECLRMM is re-enabled later.
        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();

        // Reset reverse video mode (DECSCNM) per VT220 spec §4.5.113
        if self.modes.reverse_video {
            self.modes.reverse_video = false;
            self.grid.damage_mut().mark_full();
        }

        // Reset synchronized output mode (per synchronized rendering spec).
        // A reset mid-window counts as a close for the host's present-hold.
        if self.modes.synchronized_output {
            self.transient.sync_end_seq += 1;
        }
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_open_dirty = false;
        self.transient.app_sync_open = false;
        self.transient.app_sync_dirty = false;

        // Reset sixel display mode (DECSDM, mode 80) — xterm resets on DECSTR
        self.modes.sixel_display_mode = false;

        // Reset alternate scroll mode (mode 1007) — xterm resets this on DECSTR
        self.modes.alternate_scroll = false;

        // Reset new-line mode (LNM, ANSI mode 20) per VT510 spec §4.5.113
        self.modes.new_line_mode = false;

        // Reset focus reporting (mode 1004) — xterm resets on DECSTR
        self.modes.focus_reporting = false;

        // Reset bracketed paste (mode 2004), as xterm does on DECSTR.
        self.modes.bracketed_paste = false;

        // Reset mouse tracking mode (modes 9/1000/1002/1003) — xterm resets on DECSTR.
        // Without this, mouse events keep being sent after soft reset.
        self.modes.mouse_mode = MouseMode::None;

        // Reset mouse encoding mode (modes 1005/1006/1015/1016) — xterm resets on DECSTR.
        self.modes.mouse_encoding = MouseEncoding::X10;

        // Reset BiDi modes to defaults (Terminal WG spec)
        self.modes.bidi_mode = crate::config::BiDiMode::Implicit;
        self.modes.bidi_direction = aterm_types::ParagraphDirection::Auto;
        self.modes.bidi_box_mirroring = false;
        self.modes.bidi_autodetection = false;
        // Terminal WG spec: default enabled
        self.modes.bidi_arrow_swap = true;
        self.invalidate_bidi_all();

        self.style.reset_sgr();
        // DECSCA protection is reset by DECSTR (VT510); reset_sgr() keeps
        // `protected` (correct for SGR 0), so clear it here.
        self.style.protected = false;
        // Refresh the writer caches and re-arm the BCE cursor template from the
        // default rendition, so the old background is not used for the next
        // erase or scroll.
        self.sgr_style().apply_style_change();
        self.transient.current_underline_color = None;
        // Clear the active hyperlink so text after the soft reset does not
        // inherit it.
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();

        // Clear last graphic character so REP (CSI b) does not repeat
        // stale characters after soft reset.
        self.transient.last_graphic_char = None;

        // Clear VT52 cursor addressing state, in case DECSTR arrived while
        // collecting ESC Y row/col.
        self.transient.vt52_cursor_state = Vt52CursorState::default();

        // Clear SGR stack — DECSTR resets all SGR state, so stale push entries
        // from before the reset should not be restorable via XTPOPSGR.
        self.transient.sgr_stack.clear();

        // Clear XTSAVE'd mode values, as xterm does: DECSTR resets the modes.
        self.transient.xtsave_modes.clear();

        // Reset xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
        // xterm resets modifyOtherKeys on DECSTR.
        self.xterm_keyboard.reset();

        // Reset the current screen's Kitty keyboard state (flags and stack), an
        // xterm-style soft-reset convention also used by foot and ghostty (the
        // kitty spec is silent on DECSTR). The inactive screen's state survives;
        // clearing both is RIS's job.
        self.kitty_keyboard
            .reset_current_screen(aterm_types::ScreenBuffer::from(self.modes.alternate_screen));

        self.charset.reset();

        self.grid.reset_scroll_region();

        // Clear the DECSC saved cursor for the current screen only (VT510
        // §4.5.113; xterm does the same). Mode 1049 saves are an xterm extension
        // separate from DECSC and are kept, so a later 1049 reset still restores
        // the pre-alt-screen cursor.
        if self.modes.alternate_screen {
            self.cursor_save.alt = None;
        } else {
            self.cursor_save.main = None;
        }

        self.grid.set_cursor(0, 0);
    }

    /// Handle DSR (Device Status Report).
    ///
    /// CSI Ps n
    /// - Ps = 5: Status report - responds with CSI 0 n (terminal OK)
    /// - Ps = 6: Cursor Position Report (CPR) - responds with CSI row ; col R
    pub(super) fn handle_dsr(&mut self, cap: &ResponseCapability, params: &[u16]) {
        let param = params.first().copied().unwrap_or(0);
        match param {
            5 => {
                // Terminal OK.
                self.send_response(cap, b"\x1b[0n");
            }
            6 => {
                // Cursor Position Report: CSI row ; col R (1-indexed). With DECOM
                // set, the row is relative to the scroll region top; with DECLRMM
                // also active, the column is relative to the left margin (VT510).
                let (row, col) = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    let r = self.grid.cursor_row().saturating_sub(region.top) + 1;
                    let c = if self.modes.left_right_margin_mode {
                        let margins = self.grid.horizontal_margins();
                        self.grid.cursor_col().saturating_sub(margins.left) + 1
                    } else {
                        self.grid.cursor_col() + 1
                    };
                    (r, c)
                } else {
                    (self.grid.cursor_row() + 1, self.grid.cursor_col() + 1)
                };
                // CPR: ESC [ + up to two u16 (≤5 digits) + ';' + 'R' ≤ 14 bytes.
                let mut response = StackResponse::<24>::new();
                let _ = write!(response, "\x1b[{row};{col}R");
                self.send_response(cap, response.as_bytes());
            }
            _ => {} // Unknown DSR parameter
        }
    }

    /// Handle DECDSR (DEC Device Status Report).
    ///
    /// CSI ? Ps n
    /// - Ps = 6: DECXCPR (Extended Cursor Position Report) - responds with
    ///   CSI ? row ; col ; page R (page always 1 for us)
    /// - Ps = 15: Printer status - responds with CSI ? 13 n (no printer)
    /// - Ps = 25: UDK status - responds with CSI ? 20 n (UDKs unlocked)
    /// - Ps = 26: Keyboard status - responds with CSI ? 27 ; 1 ; 0 ; 0 n
    ///   (North American, ready, no LK201 options)
    pub(super) fn handle_decdsr(&mut self, cap: &ResponseCapability, params: &[u16]) {
        let param = params.first().copied().unwrap_or(0);
        match param {
            6 => {
                // DECXCPR - Extended Cursor Position Report
                // Response: CSI ? row ; col ; page R (1-indexed, page always 1)
                // Per VT510: affected by origin mode (DECOM) and DECLRMM.
                let (row, col) = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    let r = self.grid.cursor_row().saturating_sub(region.top) + 1;
                    let c = if self.modes.left_right_margin_mode {
                        let margins = self.grid.horizontal_margins();
                        self.grid.cursor_col().saturating_sub(margins.left) + 1
                    } else {
                        self.grid.cursor_col() + 1
                    };
                    (r, c)
                } else {
                    (self.grid.cursor_row() + 1, self.grid.cursor_col() + 1)
                };
                // DECXCPR: ESC [ ? + two u16 + ";1R" ≤ 17 bytes.
                let mut response = StackResponse::<24>::new();
                let _ = write!(response, "\x1b[?{row};{col};1R");
                self.send_response(cap, response.as_bytes());
            }
            15 => {
                // Printer status: no printer connected
                self.send_response(cap, b"\x1b[?13n");
            }
            25 => {
                // UDK status: UDKs are unlocked
                self.send_response(cap, b"\x1b[?20n");
            }
            26 => {
                // Keyboard status: North American, ready, no LK201 options
                self.send_response(cap, b"\x1b[?27;1;0;0n");
            }
            996 => {
                // DEC color-scheme query: report the current OS appearance as
                // CSI ? 997 ; Ps n (Ps = 1 dark, 2 light). Apps use this to match
                // their theme to the desktop. Push updates require DEC mode 2031.
                let mut response = StackResponse::<16>::new();
                let _ = write!(
                    response,
                    "\x1b[?997;{}n",
                    self.modes.color_scheme.dsr_code()
                );
                self.send_response(cap, response.as_bytes());
            }
            _ => {} // Unknown DECDSR parameter
        }
    }

    /// Handle Primary Device Attributes (DA1).
    ///
    /// CSI Ps c (where Ps is 0 or omitted)
    ///
    /// Reports terminal type and capabilities. The conformance level is
    /// dynamic based on `modes.vt_level` (changed by DECSCL).
    ///
    /// Default response (VT420): CSI ? 64 ; 6 ; 22 ; 28 c
    /// - 64 = VT420 conformance level
    /// - 6 = Selective erase (DECSCA/DECSED/DECSEL)
    /// - 22 = ANSI color
    /// - 28 = Rectangular editing (DECFRA/DECCARA/DECCRA/DECSERA)
    pub(super) fn handle_primary_da(&mut self, cap: &ResponseCapability) {
        // VT100 has its own format: CSI ? 1 ; 2 c (with the Advanced Video
        // Option).
        if self.modes.vt_level == aterm_types::VtLevel::VT100 {
            self.send_response(cap, b"\x1b[?1;2c");
            return;
        }

        let level = match self.modes.vt_level {
            aterm_types::VtLevel::VT100 => unreachable!(),
            aterm_types::VtLevel::VT220 | aterm_types::VtLevel::VT240 => b"62" as &[u8],
            aterm_types::VtLevel::VT320
            | aterm_types::VtLevel::VT330
            | aterm_types::VtLevel::VT340 => b"63",
            aterm_types::VtLevel::VT420 => b"64",
            aterm_types::VtLevel::VT510
            | aterm_types::VtLevel::VT520
            | aterm_types::VtLevel::VT525 => b"65",
        };
        // DA1 reply: ESC [ ? + 2-digit level + fixed flag list + 'c' ≤ 20 bytes.
        let mut response = StackResponse::<24>::new();
        response.push_bytes(b"\x1b[?");
        response.push_bytes(level);
        // Capability flags are level-dependent per VT510 spec:
        // - VT220+ (62+): 6 = selective erase, 22 = ANSI color
        // - VT420+ (64+): 28 = rectangular editing (DECFRA/DECCARA/DECCRA/DECSERA)
        // - Sixel (4): advertised when `sixel` feature is enabled (VT220+)
        match self.modes.vt_level {
            aterm_types::VtLevel::VT100 => unreachable!(),
            aterm_types::VtLevel::VT220
            | aterm_types::VtLevel::VT240
            | aterm_types::VtLevel::VT320
            | aterm_types::VtLevel::VT330
            | aterm_types::VtLevel::VT340 => {
                response.push_bytes(b";6;22");
                #[cfg(feature = "sixel")]
                response.push_bytes(b";4");
            }
            aterm_types::VtLevel::VT420
            | aterm_types::VtLevel::VT510
            | aterm_types::VtLevel::VT520
            | aterm_types::VtLevel::VT525 => {
                response.push_bytes(b";6;22;28");
                #[cfg(feature = "sixel")]
                response.push_bytes(b";4");
            }
        }
        response.push_bytes(b"c");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle Secondary Device Attributes (DA2).
    ///
    /// CSI > Ps c (where Ps is 0 or omitted)
    ///
    /// Reports terminal type, firmware version, and keyboard type.
    ///
    /// Response: CSI > Pp ; Pv ; Pc c
    /// - Pp = Terminal type (41 = VT420, consistent with DA1 conformance level 64)
    /// - Pv = Firmware version (we use 100 as a version number)
    /// - Pc = ROM cartridge registration number (0 = none)
    pub(super) fn handle_secondary_da(&mut self, cap: &ResponseCapability) {
        // Pp follows the DECSCL level (41 for VT420, 64 for VT520).
        let da2 = self.modes.vt_level.da2_param();
        // DA2: ESC [ > + u16 + ";100;0c" ≤ 14 bytes.
        let mut response = StackResponse::<24>::new();
        let _ = write!(response, "\x1b[>{da2};100;0c");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle Tertiary Device Attributes (DA3).
    ///
    /// CSI = Ps c (where Ps is 0 or omitted)
    ///
    /// Reports the terminal's unit ID (serial number). Per VT510/VT520 spec,
    /// the response is a DCS sequence with the unit identifier hex-encoded.
    ///
    /// Response: DCS ! | <hex-unit-id> ST
    /// - hex-unit-id = "30" (ASCII '0' = 0x30, meaning no unit ID assigned)
    pub(super) fn handle_tertiary_da(&mut self, cap: &ResponseCapability) {
        self.send_response(cap, b"\x1bP!|30\x1b\\");
    }

    /// Handle Kitty keyboard protocol query (CSI ? u).
    ///
    /// Responds with `CSI ? flags u` where flags is the current keyboard flags value.
    pub(super) fn handle_kitty_keyboard_query(&mut self, cap: &ResponseCapability) {
        let flags = self.kitty_keyboard.query_flags();
        // Format: CSI ? flags u — ESC [ ? + u8 (≤3 digits) + 'u' ≤ 7 bytes.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{flags}u");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle XTPUSHSGR - push SGR attributes onto stack (CSI # {).
    ///
    /// Saves the current SGR state (colors, flags, underline color) onto an internal
    /// stack for later restoration with XTPOPSGR. Stack is bounded to
    /// `SGR_STACK_MAX_DEPTH` (10) entries - oldest entry is discarded if full.
    ///
    /// Per xterm spec, `CSI # { Ps...` can selectively push only specific
    /// attributes. If no params (or param 0), all attributes are pushed.
    /// Recognized Ps values:
    /// - 1/2: bold/dim group
    /// - 3: italic
    /// - 4/21: underline group (all underline styles + underline color)
    /// - 5: blink
    /// - 7: inverse
    /// - 8: invisible
    /// - 9: strikethrough
    /// - 30/31/38/39: foreground color
    /// - 40/41/48/49: background color
    /// - 53: overline
    pub(super) fn handle_xtpushsgr(&mut self, params: &[u16]) {
        // Enforce stack depth limit (O(1) eviction via VecDeque)
        if self.transient.sgr_stack.len() >= SGR_STACK_MAX_DEPTH {
            self.transient.sgr_stack.pop_front();
        }

        let mask = if params.is_empty() || (params.len() == 1 && params[0] == 0) {
            SgrPushMask::ALL
        } else {
            SgrPushMask::from_params(params)
        };

        let entry = SgrStackEntry {
            style: *self.style,
            underline_color: self.transient.current_underline_color,
            mask,
        };
        self.transient.sgr_stack.push_back(entry);
    }

    /// Handle XTPOPSGR - pop and restore SGR attributes from stack (CSI # }).
    ///
    /// Restores the most recently pushed SGR state. If the stack is empty,
    /// this is a no-op (per xterm behavior). When selective push was used,
    /// only the masked attributes are restored.
    pub(super) fn handle_xtpopsgr(&mut self) {
        if let Some(entry) = self.transient.sgr_stack.pop_back() {
            if entry.mask.is_all() {
                // Full restore — fast path.
                // Preserve the current DECSCA protection attribute: XTPUSHSGR
                // is an SGR-only stack, and DECSCA is not an SGR attribute
                // (it is controlled by CSI Ps " q, not CSI m).
                let saved_protected = self.style.protected;
                *self.style = entry.style;
                self.style.protected = saved_protected;
                self.transient.current_underline_color = entry.underline_color;
            } else {
                // Selective restore — only restore masked attributes
                use crate::grid::CellFlags;

                if entry.mask.has_bold() {
                    // Bold/dim group: restore BOLD and DIM flags
                    let bold_dim = CellFlags::BOLD.union(CellFlags::DIM);
                    self.style.flags.remove(bold_dim);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & bold_dim.0));
                }
                if entry.mask.has_italic() {
                    self.style.flags.remove(CellFlags::ITALIC);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::ITALIC.0));
                }
                if entry.mask.has_underline() {
                    // Underline group: all underline style flags + underline color.
                    // ALL_UNDERLINES covers UNDERLINE, DOUBLE_UNDERLINE,
                    // CURLY_UNDERLINE, DOTTED_UNDERLINE, and DASHED_UNDERLINE.
                    let ul_flags = CellFlags::ALL_UNDERLINES;
                    self.style.flags.remove(ul_flags);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & ul_flags.0));
                    self.transient.current_underline_color = entry.underline_color;
                }
                if entry.mask.has_blink() {
                    self.style.flags.remove(CellFlags::BLINK);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::BLINK.0));
                }
                if entry.mask.has_inverse() {
                    self.style.flags.remove(CellFlags::INVERSE);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::INVERSE.0));
                }
                if entry.mask.has_invisible() {
                    self.style.flags.remove(CellFlags::HIDDEN);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::HIDDEN.0));
                }
                if entry.mask.has_strikethrough() {
                    self.style.flags.remove(CellFlags::STRIKETHROUGH);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::STRIKETHROUGH.0));
                }
                if entry.mask.has_foreground() {
                    self.style.fg = entry.style.fg;
                }
                if entry.mask.has_background() {
                    self.style.bg = entry.style.bg;
                }
                if entry.mask.has_overline() {
                    self.style.flags.remove(CellFlags::OVERLINE);
                    self.style
                        .flags
                        .insert(CellFlags(entry.style.flags.0 & CellFlags::OVERLINE.0));
                }
                // Recompute cached state after selective modification
                self.style.update_cached_colors();
            }
            self.transient.update_has_transient_extras();
            // Re-arm the BCE cursor template from the popped rendition
            // (`update_cached_colors` above already refreshed the writer caches).
            self.sgr_style().apply_style_change();
        }
        // Empty stack = no-op (xterm behavior)
    }
}

#[cfg(test)]
mod color_scheme_tests {
    use crate::terminal::Terminal;
    use aterm_types::Appearance;

    fn drain(term: &mut Terminal) -> Vec<u8> {
        term.take_response().unwrap_or_default()
    }

    #[test]
    fn dsr_996_reports_current_scheme() {
        let mut term = Terminal::new(24, 80);
        // Default appearance is Dark -> DSR code 1.
        term.process(b"\x1b[?996n");
        assert_eq!(drain(&mut term), b"\x1b[?997;1n");

        // After the host reports Light, the query reports code 2.
        term.set_color_scheme(Appearance::Light);
        let _ = drain(&mut term); // discard any push from the change
        term.process(b"\x1b[?996n");
        assert_eq!(drain(&mut term), b"\x1b[?997;2n");
    }

    #[test]
    fn response_append_alloc_fault_drops_response_fail_closed() {
        use crate::fault;
        let mut term = Terminal::new(24, 80);
        // With the response-append alloc fault armed, a query that would normally
        // reply produces no bytes — the response is dropped (fail closed), not a panic.
        fault::with_armed("response.append_alloc", || {
            term.process(b"\x1b[?996n");
        });
        assert!(
            term.take_response().is_none(),
            "an armed response-append alloc fault must drop the response (fail closed)"
        );
        // After disarming, the same query replies normally — no corrupt buffer state.
        term.process(b"\x1b[?996n");
        assert_eq!(
            drain(&mut term),
            b"\x1b[?997;1n",
            "response path recovers after the fault clears"
        );
    }

    #[test]
    fn mode_2031_pushes_on_change_only_when_set() {
        let mut term = Terminal::new(24, 80);
        // Without mode 2031, a scheme change does not push.
        term.set_color_scheme(Appearance::Light);
        assert!(term.take_response().is_none(), "no push without DEC 2031");

        // Enable mode 2031; a change now pushes CSI ?997;Ps n.
        term.process(b"\x1b[?2031h");
        term.set_color_scheme(Appearance::Dark);
        assert_eq!(drain(&mut term), b"\x1b[?997;1n");

        // No-op change (same scheme) does not push.
        term.set_color_scheme(Appearance::Dark);
        assert!(term.take_response().is_none(), "unchanged scheme = no push");
    }

    #[test]
    fn decrqm_reports_mode_2031_state() {
        let mut term = Terminal::new(24, 80);
        // Not set initially -> DECRQM reports reset (2).
        term.process(b"\x1b[?2031$p");
        assert_eq!(drain(&mut term), b"\x1b[?2031;2$y");
        // Set -> DECRQM reports set (1).
        term.process(b"\x1b[?2031h");
        term.process(b"\x1b[?2031$p");
        assert_eq!(drain(&mut term), b"\x1b[?2031;1$y");
    }
}

#[cfg(test)]
mod default_cursor_style_tests {
    use crate::terminal::{CursorStyle, Terminal};

    #[test]
    fn host_default_takes_effect_before_app_decscusr() {
        let mut term = Terminal::new(24, 80);
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock); // engine default
        // Host configures a bar default before any app DECSCUSR -> live style follows.
        term.set_default_cursor_style(CursorStyle::SteadyBar);
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    }

    #[test]
    fn host_default_does_not_clobber_app_decscusr() {
        let mut term = Terminal::new(24, 80);
        // App takes control (DECSCUSR 3 = blinking underline, e.g. vim insert mode).
        term.process(b"\x1b[3 q");
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingUnderline);
        // A later host preference change must not override the app's live choice.
        term.set_default_cursor_style(CursorStyle::SteadyBar);
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingUnderline);
    }

    #[test]
    fn soft_reset_restores_host_default_not_blinking_block() {
        let mut term = Terminal::new(24, 80);
        term.set_default_cursor_style(CursorStyle::SteadyBar);
        term.process(b"\x1b[3 q"); // app override
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingUnderline);
        term.process(b"\x1b[!p"); // DECSTR soft reset
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    }

    #[test]
    fn full_reset_restores_host_default_not_blinking_block() {
        let mut term = Terminal::new(24, 80);
        term.set_default_cursor_style(CursorStyle::SteadyUnderline);
        term.process(b"\x1b[5 q"); // app override to blinking bar
        assert_eq!(term.cursor_style(), CursorStyle::BlinkingBar);
        term.process(b"\x1bc"); // RIS full reset
        assert_eq!(term.cursor_style(), CursorStyle::SteadyUnderline);
    }

    #[test]
    fn apply_config_cursor_style_persists_as_default() {
        use crate::config::TerminalConfig;
        let mut term = Terminal::new(24, 80);
        let config = TerminalConfig {
            cursor_style: CursorStyle::SteadyBar,
            ..TerminalConfig::default()
        };
        term.apply_config(&config);
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
        // An app override then a soft reset restores the config-set default, not the
        // hardcoded BlinkingBlock — i.e. apply_config persisted the default.
        term.process(b"\x1b[3 q");
        term.process(b"\x1b[!p");
        assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    }
}
