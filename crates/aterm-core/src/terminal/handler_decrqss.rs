// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DECRQSS (Request Selection or Setting) handler for the terminal.
//!
//! This module handles DECRQSS queries which allow applications to query
//! the terminal's current settings. The terminal responds with DECRPSS
//! (Report Selection or Setting).
//!
//! Supported queries:
//! - SGR (m): Current text attributes
//! - DECSCUSR ( q): Cursor style
//! - DECSTBM (r): Scroll region
//! - DECSLRM (s): Left/right margins
//! - DECSACE (*x): Attribute change extent (rect vs stream)
//! - DECSCL ("p): Conformance level
//! - DECSCA ("q): Character protection attribute
//! - DECSLPP (t): Lines per page (terminal height)
//! - DECTCEM (?25): Text cursor enable mode
//! - DECOM (?6): Origin mode
//!
//! Extracted from handler.rs as part of large files refactor.

use crate::grid::{CellFlags, PackedColor};

use super::CursorStyle;
use super::handler::TerminalHandler;

/// DECSCL second parameter for 7-bit C1 control transmission (S7C1T).
///
/// aterm has a fixed 7-bit C1 *transmission* policy: it implements no S7C1T/S8C1T
/// switch and `handle_decscl` ignores the requested C1 mode, so the terminal never
/// emits 8-bit C1 bytes. The DECSCL report is therefore invariantly `2` here, named
/// rather than a magic literal. (`1` would be the 8-bit / S8C1T value, which aterm
/// never reports.)
const DECSCL_C1_TRANSMISSION_7BIT: u8 = 2;

/// Describes how a DECRQSS payload is assembled into the final response.
///
/// Most queries return a parameter string that is followed by the original
/// query mnemonic (e.g. `"0"` + `"m"` -> `"0m"`).  DEC private-mode queries
/// return a self-contained string that already includes the trailing `h`/`l`
/// final character and must NOT have the mnemonic appended again.
enum DecrqssPayload {
    /// Payload is followed by the filtered original query mnemonic `Pt`.
    WithPt(String),
    /// Payload is the complete response body. No `Pt` suffix appended.
    /// Used for DEC private mode queries where the response includes `h`/`l`.
    Full(String),
}

/// Push SGR color parameters for a `PackedColor` onto `params`.
///
/// `base` is 30 for foreground, 40 for background. Bright colors use `base + 60`,
/// and extended indexed/RGB colors use `base + 8`.
fn push_color_sgr(params: &mut Vec<String>, color: PackedColor, base: u8) {
    if color.is_indexed() {
        let idx = color.index();
        if idx < 8 {
            params.push(format!("{}", base + idx));
        } else if idx < 16 {
            params.push(format!("{}", base + 60 + idx - 8));
        } else {
            // ISO 8613-3 colon subparameters for extended indexed color
            params.push(format!("{}:5:{idx}", base + 8));
        }
    } else if color.is_rgb() {
        let (r, g, b) = color.rgb_components();
        // ISO 8613-3 colon subparameters: base:2::r:g:b (empty colorspace)
        params.push(format!("{}:2::{r}:{g}:{b}", base + 8));
    }
}

impl TerminalHandler<'_> {
    /// Handle DECRQSS (Request Selection or Setting).
    ///
    /// DECRQSS allows applications to query the terminal's current settings.
    /// The terminal responds with DECRPSS (Report Selection or Setting).
    ///
    /// Format: `DCS $ q <Pt> ST`
    /// - Pt is the "mnemonic" identifying what to query
    ///
    /// Response: `DCS <validity> $ r <payload> <Pt> ST`
    /// - validity = 1 for valid request, 0 for invalid
    pub(super) fn handle_decrqss(&mut self, cap: &super::response_capability::ResponseCapability) {
        let Ok(pt) = std::str::from_utf8(&self.dcs.data[..]) else {
            self.send_response(cap, b"\x1bP0$r\x1b\\");
            return;
        };

        // Match the setting mnemonic.
        //
        // Most queries produce `WithPt` -- the payload precedes the original
        // query mnemonic in the response.  DEC private-mode queries produce
        // `Full` because the response already contains the `h`/`l` final
        // character and the mnemonic must not be appended again.
        let response: Option<DecrqssPayload> = match pt {
            // SGR - Select Graphic Rendition
            "m" => Some(DecrqssPayload::WithPt(self.decrqss_sgr())),

            // DECSCUSR - Set Cursor Style
            // Note: Pt is space followed by q
            " q" => Some(DecrqssPayload::WithPt(self.decrqss_decscusr())),

            // DECSTBM - Set Top and Bottom Margins
            "r" => Some(DecrqssPayload::WithPt(self.decrqss_decstbm())),

            // DECSLRM - Set Left and Right Margins (mode 69)
            "s" => Some(DecrqssPayload::WithPt(self.decrqss_decslrm())),

            // DECSCL - Set Conformance Level
            "\"p" => Some(DecrqssPayload::WithPt(self.decrqss_decscl())),

            // DECSCA - Select Character Protection Attribute
            "\"q" => Some(DecrqssPayload::WithPt(self.decrqss_decsca())),

            // DECSLPP - Set Lines Per Page (terminal height)
            "t" => Some(DecrqssPayload::WithPt(self.decrqss_decslpp())),

            // DECSACE - Select Attribute Change Extent
            "*x" => Some(DecrqssPayload::WithPt(self.decrqss_decsace())),

            // DECTCEM - Text Cursor Enable Mode (DEC private mode ?25)
            "?25" => Some(DecrqssPayload::Full(self.decrqss_dectcem())),

            // DECOM - Origin Mode (DEC private mode ?6)
            "?6" => Some(DecrqssPayload::Full(self.decrqss_decom())),

            // DECAWM - Autowrap Mode (DEC private mode ?7)
            "?7" => Some(DecrqssPayload::Full(self.decrqss_decawm())),

            // DECSCNM - Screen Mode / Reverse Video (DEC private mode ?5)
            "?5" => Some(DecrqssPayload::Full(self.decrqss_decscnm())),

            // DECLRMM - Left Right Margin Mode (DEC private mode ?69)
            "?69" => Some(DecrqssPayload::Full(self.decrqss_declrmm())),

            // DECCKM - Cursor Key Mode (DEC private mode ?1)
            "?1" => Some(DecrqssPayload::Full(self.decrqss_decckm())),

            // Bracketed Paste Mode (xterm private mode ?2004)
            "?2004" => Some(DecrqssPayload::Full(self.decrqss_bracketed_paste())),

            // Alternate Screen Mode (xterm private mode ?1049)
            "?1049" => Some(DecrqssPayload::Full(self.decrqss_alt_screen())),

            // IRM - Insert/Replace Mode (ANSI mode 4, queried as ?4)
            "?4" => Some(DecrqssPayload::Full(self.decrqss_irm())),

            // Cursor Blink Mode (DEC private mode ?12)
            "?12" => Some(DecrqssPayload::Full(self.decrqss_cursor_blink())),

            // Reverse Wraparound Mode (DEC private mode ?45)
            "?45" => Some(DecrqssPayload::Full(self.decrqss_reverse_wraparound())),

            // Synchronized Output Mode (xterm private mode ?2026)
            "?2026" => Some(DecrqssPayload::Full(self.decrqss_synchronized_output())),

            // Alternate Scroll Mode (xterm private mode ?1007)
            "?1007" => Some(DecrqssPayload::Full(self.decrqss_alternate_scroll())),

            // DECCOLM - 132 Column Mode (DEC private mode ?3)
            "?3" => Some(DecrqssPayload::Full(self.decrqss_deccolm())),

            // Grapheme Cluster Mode (xterm private mode ?2027)
            "?2027" => Some(DecrqssPayload::Full(self.decrqss_grapheme_cluster())),

            // Unknown mnemonic
            _ => None,
        };

        match response {
            Some(payload) => {
                // Success response: DCS 1 $ r <body> ST
                //
                // Security: Filter dcs_data to remove escape sequences and control
                // characters that could be interpreted when the response is echoed
                // back to a shell. Only printable ASCII is safe to echo.
                let mut response_bytes = Vec::new();
                response_bytes.extend_from_slice(b"\x1bP1$r");
                match payload {
                    DecrqssPayload::WithPt(p) => {
                        // Standard queries: <payload> <filtered_pt>
                        response_bytes.extend_from_slice(p.as_bytes());
                        let filtered_pt =
                            Self::filter_dcs_data_for_response(self.dcs.data.as_slice());
                        response_bytes.extend_from_slice(&filtered_pt);
                    }
                    DecrqssPayload::Full(p) => {
                        // DEC private-mode queries: payload already contains
                        // the mode number and h/l suffix.
                        response_bytes.extend_from_slice(p.as_bytes());
                    }
                }
                response_bytes.extend_from_slice(b"\x1b\\");
                self.send_response(cap, &response_bytes);
            }
            None => {
                // Error response: DCS 0 $ r ST
                self.send_response(cap, b"\x1bP0$r\x1b\\");
            }
        }
    }

    /// Generate SGR (Select Graphic Rendition) response.
    ///
    /// Returns the current text attributes as SGR parameters.
    /// Uses ISO 8613-3 format for underline subparameters (colon-separated).
    fn decrqss_sgr(&self) -> String {
        let mut params: Vec<String> = Vec::new();

        // Collect the active attributes; the leading default-reset "0" is
        // prepended at the end (xterm parity) so replaying the report
        // reproduces this exact style over ANY current state.
        if self.style.flags.contains(CellFlags::BOLD) {
            params.push("1".to_string());
        }
        if self.style.flags.contains(CellFlags::DIM) {
            params.push("2".to_string());
        }
        if self.style.flags.contains(CellFlags::ITALIC) {
            params.push("3".to_string());
        }

        // Underline styles - check extended styles before basic UNDERLINE
        // because DOTTED_UNDERLINE = UNDERLINE | CURLY_UNDERLINE
        if self.style.flags.contains(CellFlags::DASHED_UNDERLINE) {
            // SGR 4:5 - dashed underline (DOUBLE_UNDERLINE | CURLY_UNDERLINE)
            params.push("4:5".to_string());
        } else if self.style.flags.contains(CellFlags::DOTTED_UNDERLINE) {
            // SGR 4:4 - dotted underline (UNDERLINE | CURLY_UNDERLINE)
            params.push("4:4".to_string());
        } else if self.style.flags.contains(CellFlags::CURLY_UNDERLINE) {
            // SGR 4:3 - curly underline
            params.push("4:3".to_string());
        } else if self.style.flags.contains(CellFlags::DOUBLE_UNDERLINE) {
            // SGR 4:2 - double underline (ISO 8613-3 colon subparameters)
            params.push("4:2".to_string());
        } else if self.style.flags.contains(CellFlags::UNDERLINE) {
            // SGR 4 - single underline
            params.push("4".to_string());
        }

        if self.style.flags.contains(CellFlags::BLINK) {
            params.push("5".to_string());
        }
        if self.style.flags.contains(CellFlags::INVERSE) {
            params.push("7".to_string());
        }
        if self.style.flags.contains(CellFlags::HIDDEN) {
            params.push("8".to_string());
        }
        if self.style.flags.contains(CellFlags::STRIKETHROUGH) {
            params.push("9".to_string());
        }

        // Overline (SGR 53) -- encoded as SUPERSCRIPT | SUBSCRIPT
        // Check overline FIRST since it contains both bits
        if self.style.flags.contains(CellFlags::OVERLINE) {
            params.push("53".to_string());
        } else if self.style.flags.contains(CellFlags::SUPERSCRIPT) {
            // Superscript (SGR 73 - ECMA-48)
            params.push("73".to_string());
        } else if self.style.flags.contains(CellFlags::SUBSCRIPT) {
            // Subscript (SGR 74 - ECMA-48)
            params.push("74".to_string());
        }

        // Underline color (SGR 58)
        // Indexed colors are stored as 0x02_0000NN, RGB as 0x01_RRGGBB (#7445).
        if let Some(color) = self.transient.current_underline_color {
            let type_byte = (color >> 24) & 0xFF;
            if type_byte == 0x02 {
                // Indexed color: SGR 58:5:N
                let index = color & 0xFF;
                params.push(format!("58:5:{index}"));
            } else {
                // RGB color: SGR 58:2::R:G:B
                let r = (color >> 16) & 0xFF;
                let g = (color >> 8) & 0xFF;
                let b = color & 0xFF;
                params.push(format!("58:2::{r}:{g}:{b}"));
            }
        }

        // Foreground and background colors
        push_color_sgr(&mut params, self.style.fg, 30);
        push_color_sgr(&mut params, self.style.bg, 40);

        // If no attributes are active, report "0" (default state)
        if params.is_empty() {
            return "0".to_string();
        }

        // Lead with "0" so the report is a self-contained replayable style
        // (xterm answers "0;1;31" for bold red, never a bare "1;31").
        format!("0;{}", params.join(";"))
    }

    /// Generate DECSCUSR (Set Cursor Style) response.
    ///
    /// Returns the current cursor style (1-6).
    /// The Pt mnemonic (" q") is appended by the caller, so the payload is just
    /// the style number -- not the intermediate SP byte.
    fn decrqss_decscusr(&self) -> String {
        // Cursor style values:
        // 0/1 = blinking block, 2 = steady block
        // 3 = blinking underline, 4 = steady underline
        // 5 = blinking bar, 6 = steady bar
        let style_num = match self.modes.cursor_style {
            CursorStyle::BlinkingBlock => 1,
            CursorStyle::SteadyBlock => 2,
            CursorStyle::BlinkingUnderline => 3,
            CursorStyle::SteadyUnderline => 4,
            CursorStyle::BlinkingBar => 5,
            CursorStyle::SteadyBar => 6,
            _ => 1, // Default to blinking block for unknown styles
        };
        style_num.to_string()
    }

    /// Generate DECSTBM (Set Top and Bottom Margins) response.
    ///
    /// Returns the current scroll region.
    fn decrqss_decstbm(&self) -> String {
        let region = self.grid.scroll_region();
        // Convert from 0-indexed to 1-indexed
        format!("{};{}", region.top + 1, region.bottom + 1)
    }

    /// Generate DECSLRM (Set Left and Right Margins) response.
    ///
    /// Returns the current horizontal margins.
    fn decrqss_decslrm(&self) -> String {
        let margins = self.grid.horizontal_margins();
        // Convert from 0-indexed to 1-indexed
        format!("{};{}", margins.left + 1, margins.right + 1)
    }

    /// Generate DECSCL (Set Conformance Level) response.
    ///
    /// Reports the current VT conformance level and control mode.
    /// The Pt mnemonic (`"p`) is appended by the caller, so the payload
    /// contains just the parameters -- not the intermediate `"` byte.
    fn decrqss_decscl(&self) -> String {
        let level = self.modes.vt_level.decscl_param();
        // Second parameter reports the C1 control *transmission* mode:
        // 1 = 8-bit (S8C1T), 2 = 7-bit (S7C1T).
        //
        // This is the engine's REAL, invariant state — not a placeholder. aterm
        // implements no S7C1T/S8C1T switch and `handle_decscl` deliberately ignores
        // the requested C1 mode (see handler_state.rs::handle_decscl), so the
        // terminal never transmits 8-bit C1 bytes. 7-bit is also the safe posture:
        // 8-bit C1 *interpretation* is off by default for injection safety (see
        // state.rs), and transmitting 8-bit C1 would be the mirror hazard. So the
        // report is fixed at 7-bit and sourced from one named constant rather than a
        // magic literal.
        format!("{level};{DECSCL_C1_TRANSMISSION_7BIT}")
    }

    /// Generate DECSCA (Select Character Protection Attribute) response.
    ///
    /// Reports character protection status:
    /// - 0 or 2: Not protected (characters can be erased by DECSED/DECSEL)
    /// - 1: Protected (characters are protected from selective erase)
    ///
    /// The Pt mnemonic (`"q`) is appended by the caller, so the payload
    /// contains just the parameter value -- not the intermediate `"` byte.
    fn decrqss_decsca(&self) -> String {
        if self.style.protected {
            "1".to_string()
        } else {
            "0".to_string()
        }
    }

    /// Generate DECSLPP (Set Lines Per Page) response.
    ///
    /// Returns the terminal height.
    fn decrqss_decslpp(&self) -> String {
        let rows = self.grid.rows();
        rows.to_string()
    }

    /// Generate DECSACE (Select Attribute Change Extent) response.
    ///
    /// Reports whether DECCARA/DECRARA use stream (1) or rectangular (2)
    /// extent, matching the DECSACE parameter values (VT520 EK-VT520-RM /
    /// xterm ctlseqs: Ps = 0/1 = wrapped stream, Ps = 2 = exact rectangle).
    /// The Pt mnemonic (`*x`) is appended by the caller, so the payload
    /// contains just the parameter value.
    fn decrqss_decsace(&self) -> String {
        if self.modes.stream_attribute_extent {
            "1".to_string()
        } else {
            "2".to_string()
        }
    }

    /// Generate DECTCEM (Text Cursor Enable Mode) response.
    ///
    /// Reports whether the text cursor is visible. DEC private mode ?25:
    /// - `?25h` (set): cursor visible
    /// - `?25l` (reset): cursor hidden
    ///
    /// Returns the full response body including mode number and `h`/`l`.
    fn decrqss_dectcem(&self) -> String {
        if self.modes.cursor_visible {
            "?25h".to_string()
        } else {
            "?25l".to_string()
        }
    }

    /// Generate DECOM (Origin Mode) response.
    ///
    /// Reports whether origin mode is active. DEC private mode ?6:
    /// - `?6h` (set): cursor addressing relative to scroll margins
    /// - `?6l` (reset): cursor addressing relative to screen origin
    ///
    /// Returns the full response body including mode number and `h`/`l`.
    fn decrqss_decom(&self) -> String {
        if self.modes.origin_mode {
            "?6h".to_string()
        } else {
            "?6l".to_string()
        }
    }

    /// Generate DECAWM (Autowrap Mode) response.
    ///
    /// Reports whether autowrap mode is active. DEC private mode ?7:
    /// - `?7h` (set): writing past the right margin wraps to next line
    /// - `?7l` (reset): writing past the right margin is clamped
    fn decrqss_decawm(&self) -> String {
        if self.modes.auto_wrap {
            "?7h".to_string()
        } else {
            "?7l".to_string()
        }
    }

    /// Generate DECSCNM (Screen Mode) response.
    ///
    /// Reports whether reverse video mode is active. DEC private mode ?5:
    /// - `?5h` (set): light background / dark text
    /// - `?5l` (reset): dark background / light text (normal)
    fn decrqss_decscnm(&self) -> String {
        if self.modes.reverse_video {
            "?5h".to_string()
        } else {
            "?5l".to_string()
        }
    }

    /// Generate DECLRMM (Left Right Margin Mode) response.
    ///
    /// Reports whether left/right margin mode is active. DEC private mode ?69:
    /// - `?69h` (set): horizontal margins active
    /// - `?69l` (reset): horizontal margins inactive
    fn decrqss_declrmm(&self) -> String {
        if self.modes.left_right_margin_mode {
            "?69h".to_string()
        } else {
            "?69l".to_string()
        }
    }

    /// Generate DECCKM (Cursor Key Mode) response.
    ///
    /// Reports whether application cursor keys are active. DEC private mode ?1:
    /// - `?1h` (set): cursor keys send application sequences
    /// - `?1l` (reset): cursor keys send ANSI sequences
    fn decrqss_decckm(&self) -> String {
        if self.modes.application_cursor_keys {
            "?1h".to_string()
        } else {
            "?1l".to_string()
        }
    }

    /// Generate Bracketed Paste Mode response.
    ///
    /// Reports whether bracketed paste mode is active. Xterm private mode ?2004:
    /// - `?2004h` (set): paste is bracketed with ESC[200~/ESC[201~
    /// - `?2004l` (reset): paste sent as raw input
    fn decrqss_bracketed_paste(&self) -> String {
        if self.modes.bracketed_paste {
            "?2004h".to_string()
        } else {
            "?2004l".to_string()
        }
    }

    /// Generate Alternate Screen Mode response.
    ///
    /// Reports whether the alternate screen buffer is active. Xterm mode ?1049:
    /// - `?1049h` (set): alternate screen buffer active
    /// - `?1049l` (reset): primary screen buffer active
    fn decrqss_alt_screen(&self) -> String {
        if self.modes.alternate_screen {
            "?1049h".to_string()
        } else {
            "?1049l".to_string()
        }
    }

    /// Generate IRM (Insert/Replace Mode) response.
    ///
    /// Reports whether insert mode is active. Mode ?4:
    /// - `?4h` (set): insert mode (characters shift existing text right)
    /// - `?4l` (reset): replace mode (characters overwrite)
    fn decrqss_irm(&self) -> String {
        if self.modes.insert_mode {
            "?4h".to_string()
        } else {
            "?4l".to_string()
        }
    }

    /// Generate Cursor Blink Mode response.
    ///
    /// Reports whether cursor blink is active. DEC private mode ?12:
    /// - `?12h` (set): cursor blinks
    /// - `?12l` (reset): cursor steady
    fn decrqss_cursor_blink(&self) -> String {
        if self.modes.cursor_blink {
            "?12h".to_string()
        } else {
            "?12l".to_string()
        }
    }

    /// Generate Reverse Wraparound Mode response.
    ///
    /// Reports whether reverse wraparound is active. DEC private mode ?45:
    /// - `?45h` (set): reverse wraparound enabled
    /// - `?45l` (reset): reverse wraparound disabled
    fn decrqss_reverse_wraparound(&self) -> String {
        if self.modes.reverse_wraparound {
            "?45h".to_string()
        } else {
            "?45l".to_string()
        }
    }

    /// Generate Synchronized Output Mode response.
    ///
    /// Reports whether synchronized output is active. Xterm private mode ?2026:
    /// - `?2026h` (set): synchronized output active
    /// - `?2026l` (reset): synchronized output inactive
    fn decrqss_synchronized_output(&self) -> String {
        if self.modes.synchronized_output {
            "?2026h".to_string()
        } else {
            "?2026l".to_string()
        }
    }

    /// Generate Alternate Scroll Mode response.
    ///
    /// Reports whether alternate scroll mode is active. Xterm private mode ?1007:
    /// - `?1007h` (set): scroll events in alternate screen send up/down arrows
    /// - `?1007l` (reset): normal scroll behavior
    fn decrqss_alternate_scroll(&self) -> String {
        if self.modes.alternate_scroll {
            "?1007h".to_string()
        } else {
            "?1007l".to_string()
        }
    }

    /// Generate DECCOLM (132 Column Mode) response.
    ///
    /// Reports whether 132-column mode is active. DEC private mode ?3:
    /// - `?3h` (set): 132-column mode
    /// - `?3l` (reset): 80-column mode
    fn decrqss_deccolm(&self) -> String {
        if self.modes.column_mode_132 {
            "?3h".to_string()
        } else {
            "?3l".to_string()
        }
    }

    /// Generate Grapheme Cluster Mode response.
    ///
    /// Reports whether grapheme cluster mode is active. Xterm private mode ?2027:
    /// - `?2027h` (set): grapheme cluster mode active
    /// - `?2027l` (reset): legacy character-by-character mode
    fn decrqss_grapheme_cluster(&self) -> String {
        if self.modes.grapheme_cluster_mode {
            "?2027h".to_string()
        } else {
            "?2027l".to_string()
        }
    }
}
