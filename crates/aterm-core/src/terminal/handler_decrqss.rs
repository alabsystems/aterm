// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DECRQSS (Request Selection or Setting): applications query a setting (SGR, DECSCUSR,
//! margins, DECSCL, DECSCA, DECSLPP, DECSACE, or a DEC private mode `?N`) and the terminal
//! answers with DECRPSS.

use crate::grid::{CellFlags, PackedColor};

use super::CursorStyle;
use super::handler::TerminalHandler;

/// DECSCL's second parameter for 7-bit C1 transmission (S7C1T; `1` is S8C1T). aterm has no
/// C1 switch (`handle_decscl` ignores the request) and never emits 8-bit C1.
const DECSCL_C1_TRANSMISSION_7BIT: u8 = 2;

/// How a DECRQSS answer is assembled: settings answer parameters then the mnemonic
/// (`"0"` + `"m"` -> `"0m"`), DEC private modes a body already ending in `h`/`l`.
enum DecrqssPayload {
    /// Followed by the filtered query mnemonic `Pt`.
    WithPt(String),
    /// The complete body (DEC private modes, ending in `h`/`l`).
    Full(String),
}

/// Push the SGR parameters for `color`: `base` is 30 (fg) or 40 (bg); bright colours use
/// `base + 60`, extended indexed and RGB `base + 8`.
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
    /// DECRQSS (`DCS $ q Pt ST`): answer `DCS 1 $ r <payload> ST` for a known mnemonic `Pt`,
    /// `DCS 0 $ r ST` otherwise.
    pub(super) fn handle_decrqss(&mut self, cap: &super::response_capability::ResponseCapability) {
        let Ok(pt) = std::str::from_utf8(&self.dcs.data[..]) else {
            self.send_response(cap, b"\x1bP0$r\x1b\\");
            return;
        };

        let response: Option<DecrqssPayload> = match pt {
            "m" => Some(DecrqssPayload::WithPt(self.decrqss_sgr())),
            " q" => Some(DecrqssPayload::WithPt(self.decrqss_decscusr())),
            "r" => Some(DecrqssPayload::WithPt(self.decrqss_decstbm())),
            "s" => Some(DecrqssPayload::WithPt(self.decrqss_decslrm())),
            "\"p" => Some(DecrqssPayload::WithPt(self.decrqss_decscl())),
            "\"q" => Some(DecrqssPayload::WithPt(self.decrqss_decsca())),
            "t" => Some(DecrqssPayload::WithPt(self.decrqss_decslpp())),
            "*x" => Some(DecrqssPayload::WithPt(self.decrqss_decsace())),
            "?25" => Some(DecrqssPayload::Full(self.decrqss_dectcem())),
            "?6" => Some(DecrqssPayload::Full(self.decrqss_decom())),
            "?7" => Some(DecrqssPayload::Full(self.decrqss_decawm())),
            "?5" => Some(DecrqssPayload::Full(self.decrqss_decscnm())),
            "?69" => Some(DecrqssPayload::Full(self.decrqss_declrmm())),
            "?1" => Some(DecrqssPayload::Full(self.decrqss_decckm())),
            "?2004" => Some(DecrqssPayload::Full(self.decrqss_bracketed_paste())),
            "?1049" => Some(DecrqssPayload::Full(self.decrqss_alt_screen())),
            "?4" => Some(DecrqssPayload::Full(self.decrqss_irm())),
            "?12" => Some(DecrqssPayload::Full(self.decrqss_cursor_blink())),
            "?45" => Some(DecrqssPayload::Full(self.decrqss_reverse_wraparound())),
            "?2026" => Some(DecrqssPayload::Full(self.decrqss_synchronized_output())),
            "?1007" => Some(DecrqssPayload::Full(self.decrqss_alternate_scroll())),
            "?3" => Some(DecrqssPayload::Full(self.decrqss_deccolm())),
            // Grapheme cluster mode: permanently reset, as DECRQM answers.
            "?2027" => Some(DecrqssPayload::Full("?2027l".to_string())),
            _ => None,
        };

        match response {
            Some(payload) => {
                // The echoed `Pt` is filtered to printable ASCII: the response may be
                // echoed back to a shell.
                let mut response_bytes = Vec::new();
                response_bytes.extend_from_slice(b"\x1bP1$r");
                match payload {
                    DecrqssPayload::WithPt(p) => {
                        response_bytes.extend_from_slice(p.as_bytes());
                        let filtered_pt =
                            Self::filter_dcs_data_for_response(self.dcs.data.as_slice());
                        response_bytes.extend_from_slice(&filtered_pt);
                    }
                    DecrqssPayload::Full(p) => {
                        response_bytes.extend_from_slice(p.as_bytes());
                    }
                }
                response_bytes.extend_from_slice(b"\x1b\\");
                self.send_response(cap, &response_bytes);
            }
            None => {
                self.send_response(cap, b"\x1bP0$r\x1b\\");
            }
        }
    }

    /// SGR: the current attributes, with ISO 8613-3 colon subparameters for underline styles
    /// and extended colours.
    fn decrqss_sgr(&self) -> String {
        let mut params: Vec<String> = Vec::new();

        // The report leads with "0" (as xterm's does: "0;1;31" for bold red), so replaying
        // it reproduces this style over any current state.
        if self.style.flags.contains(CellFlags::BOLD) {
            params.push("1".to_string());
        }
        if self.style.flags.contains(CellFlags::DIM) {
            params.push("2".to_string());
        }
        if self.style.flags.contains(CellFlags::ITALIC) {
            params.push("3".to_string());
        }

        // The composite underline styles first: DASHED is DOUBLE | CURLY, DOTTED is
        // UNDERLINE | CURLY.
        if self.style.flags.contains(CellFlags::DASHED_UNDERLINE) {
            params.push("4:5".to_string());
        } else if self.style.flags.contains(CellFlags::DOTTED_UNDERLINE) {
            params.push("4:4".to_string());
        } else if self.style.flags.contains(CellFlags::CURLY_UNDERLINE) {
            params.push("4:3".to_string());
        } else if self.style.flags.contains(CellFlags::DOUBLE_UNDERLINE) {
            params.push("4:2".to_string());
        } else if self.style.flags.contains(CellFlags::UNDERLINE) {
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

        // OVERLINE is SUPERSCRIPT | SUBSCRIPT, so it is checked first.
        if self.style.flags.contains(CellFlags::OVERLINE) {
            params.push("53".to_string());
        } else if self.style.flags.contains(CellFlags::SUPERSCRIPT) {
            params.push("73".to_string());
        } else if self.style.flags.contains(CellFlags::SUBSCRIPT) {
            params.push("74".to_string());
        }

        // Underline colour (SGR 58) is stored as 0x02_0000NN indexed, 0x01_RRGGBB RGB.
        if let Some(color) = self.transient.current_underline_color {
            let type_byte = (color >> 24) & 0xFF;
            if type_byte == 0x02 {
                let index = color & 0xFF;
                params.push(format!("58:5:{index}"));
            } else {
                let r = (color >> 16) & 0xFF;
                let g = (color >> 8) & 0xFF;
                let b = color & 0xFF;
                params.push(format!("58:2::{r}:{g}:{b}"));
            }
        }

        push_color_sgr(&mut params, self.style.fg, 30);
        push_color_sgr(&mut params, self.style.bg, 40);

        if params.is_empty() {
            return "0".to_string();
        }

        format!("0;{}", params.join(";"))
    }

    /// DECSCUSR: the style number 1-6 (the caller appends ` q`).
    fn decrqss_decscusr(&self) -> String {
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

    /// DECSTBM: the scroll region, 1-indexed.
    fn decrqss_decstbm(&self) -> String {
        let region = self.grid.scroll_region();
        format!("{};{}", region.top + 1, region.bottom + 1)
    }

    /// DECSLRM: the horizontal margins, 1-indexed.
    fn decrqss_decslrm(&self) -> String {
        let margins = self.grid.horizontal_margins();
        format!("{};{}", margins.left + 1, margins.right + 1)
    }

    /// DECSCL: conformance level and C1 transmission mode.
    fn decrqss_decscl(&self) -> String {
        let level = self.modes.vt_level.decscl_param();
        format!("{level};{DECSCL_C1_TRANSMISSION_7BIT}")
    }

    /// DECSCA: `1` protected from selective erase, `0` not.
    fn decrqss_decsca(&self) -> String {
        if self.style.protected {
            "1".to_string()
        } else {
            "0".to_string()
        }
    }

    /// DECSLPP: the terminal height.
    fn decrqss_decslpp(&self) -> String {
        let rows = self.grid.rows();
        rows.to_string()
    }

    /// DECSACE: `1` stream, `2` rectangle, the parameter values DECSACE takes.
    fn decrqss_decsace(&self) -> String {
        if self.modes.stream_attribute_extent {
            "1".to_string()
        } else {
            "2".to_string()
        }
    }

    // DEC private-mode answers are the full body: `?Nh` when set, `?Nl` when reset.
    fn decrqss_dectcem(&self) -> String {
        if self.modes.cursor_visible {
            "?25h".to_string()
        } else {
            "?25l".to_string()
        }
    }

    fn decrqss_decom(&self) -> String {
        if self.modes.origin_mode {
            "?6h".to_string()
        } else {
            "?6l".to_string()
        }
    }

    fn decrqss_decawm(&self) -> String {
        if self.modes.auto_wrap {
            "?7h".to_string()
        } else {
            "?7l".to_string()
        }
    }

    fn decrqss_decscnm(&self) -> String {
        if self.modes.reverse_video {
            "?5h".to_string()
        } else {
            "?5l".to_string()
        }
    }

    fn decrqss_declrmm(&self) -> String {
        if self.modes.left_right_margin_mode {
            "?69h".to_string()
        } else {
            "?69l".to_string()
        }
    }

    fn decrqss_decckm(&self) -> String {
        if self.modes.application_cursor_keys {
            "?1h".to_string()
        } else {
            "?1l".to_string()
        }
    }

    fn decrqss_bracketed_paste(&self) -> String {
        if self.modes.bracketed_paste {
            "?2004h".to_string()
        } else {
            "?2004l".to_string()
        }
    }

    fn decrqss_alt_screen(&self) -> String {
        if self.modes.alternate_screen {
            "?1049h".to_string()
        } else {
            "?1049l".to_string()
        }
    }

    /// IRM is ANSI mode 4, queried here as `?4`.
    fn decrqss_irm(&self) -> String {
        if self.modes.insert_mode {
            "?4h".to_string()
        } else {
            "?4l".to_string()
        }
    }

    fn decrqss_cursor_blink(&self) -> String {
        // The cursor style's blink bit, the fact the renderer acts on (`set_cursor_blink`).
        if self.modes.cursor_style.blinks() {
            "?12h".to_string()
        } else {
            "?12l".to_string()
        }
    }

    fn decrqss_reverse_wraparound(&self) -> String {
        if self.modes.reverse_wraparound {
            "?45h".to_string()
        } else {
            "?45l".to_string()
        }
    }

    fn decrqss_synchronized_output(&self) -> String {
        if self.modes.synchronized_output {
            "?2026h".to_string()
        } else {
            "?2026l".to_string()
        }
    }

    fn decrqss_alternate_scroll(&self) -> String {
        if self.modes.alternate_scroll {
            "?1007h".to_string()
        } else {
            "?1007l".to_string()
        }
    }

    fn decrqss_deccolm(&self) -> String {
        if self.modes.column_mode_132 {
            "?3h".to_string()
        } else {
            "?3l".to_string()
        }
    }
}
