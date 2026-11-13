// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Device status reports, device attributes, soft reset, and SGR stack operations.

use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;
use super::stack_response::StackResponse;
use super::{
    MouseEncoding, MouseMode, SGR_STACK_MAX_DEPTH, SgrPushMask, SgrStackEntry, Vt52CursorState,
};
use core::fmt::Write as _;

impl TerminalHandler<'_> {
    /// Handle DECSTR (`CSI ! p`), the soft reset.
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

        self.modes.cursor_style = *self.default_cursor_style;

        self.modes.origin_mode = false;

        self.modes.auto_wrap = true;

        self.modes.insert_mode = false;

        self.modes.application_cursor_keys = false;

        self.modes.application_keypad = false;

        self.modes.cursor_blink = self.modes.cursor_style.blinks();

        self.modes.vt52_mode = false;

        self.modes.column_mode_132 = false;

        self.modes.deccolm_enable = false;

        self.modes.decncsm = false;

        self.modes.reverse_wraparound = false;

        self.modes.left_right_margin_mode = false;
        self.grid.reset_horizontal_margins();

        if self.modes.reverse_video {
            self.modes.reverse_video = false;
            self.grid.damage_mut().mark_full();
        }

        if self.modes.synchronized_output {
            self.transient.sync_end_seq += 1;
        }
        self.modes.synchronized_output = false;
        self.transient.sync_start = None;
        self.transient.sync_end_caret = None;
        self.transient.sync_open_dirty = false;
        self.transient.app_sync_open = false;
        self.transient.app_sync_dirty = false;

        self.modes.sixel_display_mode = false;

        self.modes.alternate_scroll = *self.host_alternate_scroll;

        self.modes.new_line_mode = false;

        self.modes.focus_reporting = false;

        self.modes.bracketed_paste = false;

        self.modes.mouse_mode = MouseMode::None;

        self.modes.mouse_encoding = MouseEncoding::X10;

        self.modes.bidi_mode = self.host_bidi.mode;
        self.modes.bidi_direction = self.host_bidi.direction;
        self.modes.bidi_box_mirroring = false;
        self.modes.bidi_autodetection = false;
        self.modes.bidi_arrow_swap = true;
        self.invalidate_bidi_all();

        self.style.reset_sgr();
        self.style.protected = false;
        self.sgr_style().apply_style_change();
        self.transient.current_underline_color = None;
        self.transient.current_hyperlink = None;
        self.transient.update_has_transient_extras();

        self.transient.last_graphic_char = None;

        self.transient.vt52_cursor_state = Vt52CursorState::default();

        self.transient.sgr_stack.clear();

        self.transient.xtsave_modes.clear();

        self.xterm_keyboard.reset();

        self.kitty_keyboard
            .reset_current_screen(aterm_types::ScreenBuffer::from(self.modes.alternate_screen));

        self.charset.reset();

        self.grid.reset_scroll_region();

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
                let mut response = StackResponse::<24>::new();
                let _ = write!(response, "\x1b[{row};{col}R");
                self.send_response(cap, response.as_bytes());
            }
            _ => {} // Unknown DSR parameter
        }
    }

    /// Handle DECDSR (`CSI ? Ps n`):.
    pub(super) fn handle_decdsr(&mut self, cap: &ResponseCapability, params: &[u16]) {
        let param = params.first().copied().unwrap_or(0);
        match param {
            6 => {
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
                // What this read did to the scheme is settled first, as a read that ended before
                // the query would have settled it: the answer then re-latches the same scheme.
                super::state_accessors::settle_color_scheme_after_output(
                    self.modes,
                    self.color,
                    self.policy,
                    self.transient,
                );
                let scheme = self.color.reported_scheme().0;
                let mut response = StackResponse::<16>::new();
                let _ = write!(response, "\x1b[?997;{}n", scheme.dsr_code());
                let queued_before = self.transient.response_buffer.len();
                self.send_response(cap, response.as_bytes());
                if self.transient.response_buffer.len() > queued_before {
                    self.modes.color_scheme = scheme;
                }
            }
            _ => {} // Unknown DECDSR parameter
        }
    }

    /// Handle DA1 (`CSI c`).
    pub(super) fn handle_primary_da(&mut self, cap: &ResponseCapability) {
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
        let mut response = StackResponse::<24>::new();
        let _ = write!(response, "\x1b[>{da2};100;0c");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle DA3 (`CSI = c`).
    pub(super) fn handle_tertiary_da(&mut self, cap: &ResponseCapability) {
        self.send_response(cap, b"\x1bP!|30\x1b\\");
    }

    /// Handle the Kitty keyboard query (`CSI ? u`): answer `CSI ? flags u`.
    pub(super) fn handle_kitty_keyboard_query(&mut self, cap: &ResponseCapability) {
        let flags = self.kitty_keyboard.query_flags();
        let mut response = StackResponse::<16>::new();
        let _ = write!(response, "\x1b[?{flags}u");
        self.send_response(cap, response.as_bytes());
    }

    /// Handle XTPUSHSGR (`CSI # { Ps...`).
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

    /// Handle XTPOPSGR (`CSI # }`).
    pub(super) fn handle_xtpopsgr(&mut self) {
        if let Some(entry) = self.transient.sgr_stack.pop_back() {
            if entry.mask.is_all() {
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
            self.sgr_style().apply_style_change();
        }
    }
}

#[cfg(test)]
#[path = "handler_report_color_scheme_tests.rs"]
mod color_scheme_tests;

#[cfg(test)]
#[path = "handler_report_default_cursor_style_tests.rs"]
mod default_cursor_style_tests;
