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
    /// Handle DECSTR (`CSI ! p`), the soft reset. Modes (cursor, wrap, insert,
    /// keys and keypad, margins, sync output, mouse, focus, paste, BiDi, keyboard
    /// protocols), SGR, charsets, the scroll region and the current screen's
    /// DECSC save return to their defaults, following xterm where it and the
    /// VT510 differ. Screen content, the active screen, tab stops, the palette
    /// and the cwd are kept: clearing them is RIS's job.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "terminal_modes",
            action = "SoftReset",
            project = "aterm_core::terminal::terminal_modes_conformance::project_modes"
        )
    )]
    pub(super) fn handle_decstr(&mut self) {
        self.modes.cursor_visible = true;

        // The host-preferred default, not CursorStyle::default().
        self.modes.cursor_style = *self.default_cursor_style;

        self.modes.origin_mode = false;

        // xterm sets auto-wrap; the VT510 resets it. Follow xterm.
        self.modes.auto_wrap = true;

        self.modes.insert_mode = false;

        self.modes.application_cursor_keys = false;

        self.modes.application_keypad = false;

        // Mode 12 is the cursor style's blink bit (see `set_cursor_blink`), so it
        // follows the style just restored.
        self.modes.cursor_blink = self.modes.cursor_style.blinks();

        self.modes.vt52_mode = false;

        self.modes.column_mode_132 = false;

        self.modes.deccolm_enable = false;

        self.modes.decncsm = false;

        self.modes.reverse_wraparound = false;

        // Clear the horizontal margins too, so stored margins cannot return when
        // DECLRMM is re-enabled.
        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();

        if self.modes.reverse_video {
            self.modes.reverse_video = false;
            self.grid.damage_mut().mark_full();
        }

        // A reset inside a mode-2026 window counts as its close for the host's
        // present-hold.
        if self.modes.synchronized_output {
            self.transient.sync_end_seq += 1;
        }
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_open_dirty = false;
        self.transient.app_sync_open = false;
        self.transient.app_sync_dirty = false;

        self.modes.sixel_display_mode = false;

        // Alternate scroll is a host preference, like the cursor style: back to it.
        self.modes.alternate_scroll = *self.host_alternate_scroll;

        self.modes.new_line_mode = false;

        self.modes.focus_reporting = false;

        self.modes.bracketed_paste = false;

        self.modes.mouse_mode = MouseMode::None;

        self.modes.mouse_encoding = MouseEncoding::X10;

        // BiDi modes to the Terminal WG defaults (arrow swap on).
        self.modes.bidi_mode = crate::config::BiDiMode::Implicit;
        self.modes.bidi_direction = aterm_types::ParagraphDirection::Auto;
        self.modes.bidi_box_mirroring = false;
        self.modes.bidi_autodetection = false;
        self.modes.bidi_arrow_swap = true;
        self.invalidate_bidi_all();

        self.style.reset_sgr();
        // DECSCA protection is reset by DECSTR (VT510); reset_sgr() keeps
        // `protected` (correct for SGR 0), so clear it here.
        self.style.protected = false;
        // Refresh the writer caches and re-arm the BCE cursor template, so the
        // old background is not used for the next erase or scroll.
        self.sgr_style().apply_style_change();
        self.transient.current_underline_color = None;
        self.transient.current_hyperlink = None;
        self.transient.current_hyperlink_id = None;
        self.transient.update_has_transient_extras();

        // REP (CSI b) must not repeat a character from before the reset.
        self.transient.last_graphic_char = None;

        // DECSTR may arrive mid ESC Y row/col.
        self.transient.vt52_cursor_state = Vt52CursorState::default();

        // Pushes from before the reset must not be restorable via XTPOPSGR.
        self.transient.sgr_stack.clear();

        // XTSAVE'd mode values go too, as in xterm: DECSTR resets the modes.
        self.transient.xtsave_modes.clear();

        // xterm resets modifyOtherKeys (XTMODKEYS/XTFMTKEYS) on DECSTR.
        self.xterm_keyboard.reset();

        // Only the current screen's Kitty keyboard state (flags and stack), as
        // foot and ghostty do (the kitty spec is silent on DECSTR); clearing both
        // is RIS's job.
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

    /// Handle DSR (`CSI Ps n`): 5 answers `CSI 0 n` (OK), 6 the cursor position
    /// (CPR) `CSI row ; col R`.
    pub(super) fn handle_dsr(&mut self, cap: &ResponseCapability, params: &[u16]) {
        let param = params.first().copied().unwrap_or(0);
        match param {
            5 => {
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

    /// Handle DECDSR (`CSI ? Ps n`):
    /// - 6: DECXCPR, `CSI ? row ; col ; 1 R` (page always 1)
    /// - 15: printer status, `CSI ? 13 n` (no printer)
    /// - 25: UDK status, `CSI ? 20 n` (unlocked)
    /// - 26: keyboard status, `CSI ? 27 ; 1 ; 0 ; 0 n` (North American, ready, no
    ///   LK201 options)
    /// - 996: color scheme of the background on screen, `CSI ? 997 ; Ps n`
    pub(super) fn handle_decdsr(&mut self, cap: &ResponseCapability, params: &[u16]) {
        let param = params.first().copied().unwrap_or(0);
        match param {
            6 => {
                // Origin-relative like CPR (VT510).
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
                self.send_response(cap, b"\x1b[?13n");
            }
            25 => {
                self.send_response(cap, b"\x1b[?20n");
            }
            26 => {
                self.send_response(cap, b"\x1b[?27;1;0;0n");
            }
            996 => {
                // DEC color-scheme query: report the scheme of the background on
                // screen as CSI ? 997 ; Ps n (Ps = 1 dark, 2 light): the host's
                // baseline while the background is the configured one, else the
                // polarity of the background a program set. Answered from the
                // colours, not the announced latch, so an OSC 11 earlier in this
                // batch is already reflected. Push updates require DEC mode 2031.
                let scheme = self.color.reported_scheme().0;
                let mut response = StackResponse::<16>::new();
                let _ = write!(response, "\x1b[?997;{}n", scheme.dsr_code());
                let queued_before = self.transient.response_buffer.len();
                self.send_response(cap, response.as_bytes());
                // The program has now been told: re-latch so the batch-end
                // reconciliation sends no redundant push. A reply the cap, the
                // policy or the rate limiter dropped told it nothing, so the
                // latch stays and the batch-end push is tried, through the same
                // policy and rate gates.
                if self.transient.response_buffer.len() > queued_before {
                    self.modes.color_scheme = scheme;
                }
            }
            _ => {} // Unknown DECDSR parameter
        }
    }

    /// Handle DA1 (`CSI c`). The conformance level follows `modes.vt_level`
    /// (DECSCL), and the flags with it: 6 (selective erase) and 22 (ANSI color)
    /// from VT220, 28 (rectangular editing) from VT420, and 4 (sixel) from VT220.
    /// The default VT420 answers `CSI ? 64 ; 6 ; 22 ; 28 ; 4 c`.
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
        match self.modes.vt_level {
            aterm_types::VtLevel::VT100 => unreachable!(),
            aterm_types::VtLevel::VT220
            | aterm_types::VtLevel::VT240
            | aterm_types::VtLevel::VT320
            | aterm_types::VtLevel::VT330
            | aterm_types::VtLevel::VT340 => {
                response.push_bytes(b";6;22");
                response.push_bytes(b";4");
            }
            aterm_types::VtLevel::VT420
            | aterm_types::VtLevel::VT510
            | aterm_types::VtLevel::VT520
            | aterm_types::VtLevel::VT525 => {
                response.push_bytes(b";6;22;28");
                response.push_bytes(b";4");
            }
        }
        response.push_bytes(b"c");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle DA2 (`CSI > c`): `CSI > Pp ; 100 ; 0 c`, Pp the terminal type for
    /// the DECSCL level (41 for VT420, 64 for VT520), 100 the firmware version, 0
    /// no ROM cartridge.
    pub(super) fn handle_secondary_da(&mut self, cap: &ResponseCapability) {
        let da2 = self.modes.vt_level.da2_param();
        // DA2: ESC [ > + u16 + ";100;0c" ≤ 14 bytes.
        let mut response = StackResponse::<24>::new();
        let _ = write!(response, "\x1b[>{da2};100;0c");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle DA3 (`CSI = c`): the unit ID, hex-encoded in a DCS, as
    /// `DCS ! | 30 ST` (ASCII `0`: none assigned).
    pub(super) fn handle_tertiary_da(&mut self, cap: &ResponseCapability) {
        self.send_response(cap, b"\x1bP!|30\x1b\\");
    }

    /// Handle the Kitty keyboard query (`CSI ? u`): answer `CSI ? flags u`.
    pub(super) fn handle_kitty_keyboard_query(&mut self, cap: &ResponseCapability) {
        let flags = self.kitty_keyboard.query_flags();
        // ESC [ ? + u8 (≤3 digits) + 'u' ≤ 7 bytes.
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{flags}u");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle XTPUSHSGR (`CSI # { Ps...`): push the SGR state for XTPOPSGR, at
    /// most `SGR_STACK_MAX_DEPTH` (10) entries, the oldest dropped when full. No
    /// Ps (or 0) pushes everything; otherwise only the named groups:
    /// - 1/2: bold/dim
    /// - 3: italic
    /// - 4/21: underline (every style, and the underline color)
    /// - 5: blink
    /// - 7: inverse
    /// - 8: invisible
    /// - 9: strikethrough
    /// - 30/31/38/39: foreground color
    /// - 40/41/48/49: background color
    /// - 53: overline
    pub(super) fn handle_xtpushsgr(&mut self, params: &[u16]) {
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

    /// Handle XTPOPSGR (`CSI # }`): restore the most recent push (only its
    /// masked attributes after a selective push); an empty stack is a no-op, as
    /// in xterm.
    pub(super) fn handle_xtpopsgr(&mut self) {
        if let Some(entry) = self.transient.sgr_stack.pop_back() {
            if entry.mask.is_all() {
                // DECSCA protection (CSI Ps " q) is not an SGR attribute, so it
                // stays.
                let saved_protected = self.style.protected;
                *self.style = entry.style;
                self.style.protected = saved_protected;
                self.transient.current_underline_color = entry.underline_color;
            } else {
                use crate::grid::CellFlags;

                if entry.mask.has_bold() {
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
                    // Every underline style plus the underline color.
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
                self.style.update_cached_colors();
            }
            self.transient.update_has_transient_extras();
            // Re-arm the BCE cursor template from the popped rendition
            // (`update_cached_colors` above already refreshed the writer caches).
            self.sgr_style().apply_style_change();
        }
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
        // An armed response-append alloc fault drops the reply instead of panicking.
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
}

#[cfg(test)]
mod default_cursor_style_tests {
    use crate::terminal::{CursorStyle, Terminal};

    #[test]
    fn resets_return_alternate_scroll_to_the_hosts() {
        // `reset`/`tput reset` send RIS then DECSTR; the wheel must keep scrolling
        // less and man in that tab afterwards (DEC 1007 on, as the host set it).
        for reset in [
            &b"\x1bc"[..],
            b"\x1b[!p",
            b"\x1bc\x1b[!p\x1b[?3;4l\x1b[4l\x1b>",
        ] {
            let mut term = Terminal::new(24, 80);
            term.set_host_alternate_scroll(true);
            term.process(b"\x1b[?1007l");
            term.process(reset);
            assert!(term.modes().alternate_scroll, "{reset:?}");
            assert!(
                term.mode_mirror().alternate_scroll(),
                "{reset:?}: the wheel's word"
            );
        }
        let mut term = Terminal::new(24, 80);
        term.set_host_alternate_scroll(true);
        term.reset();
        assert!(term.modes().alternate_scroll, "Terminal::reset");
    }

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
