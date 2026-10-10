// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse event encoding for terminal emulation.
//!
//! Delegates to shared encoding primitives in `aterm_types::mouse`.

use super::Terminal;
use super::types::{MouseEncoding, MouseMode};
use aterm_types::mouse::encode_mouse;

/// What the last mouse report the host encoded for a terminal carried: its
/// coordinates as the report wrote them (cells, or pixels under DEC 1016), and
/// the tracking mode and encoding it was produced under. See
/// [`Terminal::note_mouse_report`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MouseReportMemo {
    mode: MouseMode,
    encoding: MouseEncoding,
    x: u16,
    y: u16,
}

impl Terminal {
    /// Whether a MOTION report carrying `(x, y)` would only repeat the position
    /// the last mouse report carried, and so is not to be sent.
    ///
    /// xterm ctlseqs, button-event tracking (DEC 1002): "Motion events are
    /// reported only if the mouse pointer has moved to a different character
    /// cell"; any-event tracking (1003) is the same with buttonless motion added.
    /// xterm keeps one `mouse_row`/`mouse_col` memo, set by every press, release
    /// and reported motion, and kitty and Alacritty gate on the same cell change.
    /// `(x, y)` are the coordinates the report would carry, so under DEC 1016
    /// (SGR-pixel) a sub-cell move is a new position, as kitty treats it. A memo
    /// written under another tracking mode or encoding never matches: a program
    /// that changed either has not seen a report in the new terms.
    #[must_use]
    pub fn mouse_motion_repeats_last_report(&self, x: u16, y: u16) -> bool {
        self.mouse_report_memo == Some(self.mouse_report_at(x, y))
    }

    /// Record that the host just produced a mouse report carrying `(x, y)` for
    /// this terminal: a press, a release, a wheel notch or a motion report. Read
    /// back by [`Self::mouse_motion_repeats_last_report`]. Host bookkeeping kept
    /// beside the mode it is judged under, like the text selection; the input
    /// seam writes it under this terminal's lock.
    pub fn note_mouse_report(&mut self, x: u16, y: u16) {
        self.mouse_report_memo = Some(self.mouse_report_at(x, y));
    }

    fn mouse_report_at(&self, x: u16, y: u16) -> MouseReportMemo {
        MouseReportMemo {
            mode: self.modes.mouse_mode,
            encoding: self.modes.mouse_encoding,
            x,
            y,
        }
    }

    // =========================================================================
    // Mouse event encoding — delegates to aterm_types::mouse for byte encoding
    // =========================================================================

    /// Encode a mouse button press event.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    #[must_use]
    pub fn encode_mouse_press(
        &self,
        button: u8,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        if self.modes.mouse_mode == MouseMode::None {
            return None;
        }

        let cb = button | modifiers;
        Some(encode_mouse(cb, col, row, self.modes.mouse_encoding, false))
    }

    /// Encode a mouse button release event.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    #[must_use]
    pub fn encode_mouse_release(
        &self,
        button: u8,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        // X10 mode (9) is press-only — no release events.
        if self.modes.mouse_mode == MouseMode::None || self.modes.mouse_mode == MouseMode::X10 {
            return None;
        }

        // Pass the original button: the encoder substitutes the button-3 release
        // code only for the single-byte formats, so an SGR release keeps the
        // button identity and the 'm' terminator.
        Some(encode_mouse(
            button | modifiers,
            col,
            row,
            self.modes.mouse_encoding,
            true,
        ))
    }

    /// Encode a mouse motion event.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// motion tracking is not enabled. Coordinates are 0-indexed.
    #[must_use]
    pub fn encode_mouse_motion(
        &self,
        button: u8,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        match self.modes.mouse_mode {
            MouseMode::None | MouseMode::X10 | MouseMode::Normal => return None,
            MouseMode::ButtonEvent => {
                if button == 3 {
                    return None;
                }
            }
            MouseMode::AnyEvent => {}
            _ => return None, // future variants default to no-op
        }

        // Motion events have bit 32 set
        let cb = button | modifiers | 32;
        Some(encode_mouse(cb, col, row, self.modes.mouse_encoding, false))
    }

    /// Encode a mouse wheel event in any of the four wheel directions.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    /// [`WheelDir::code`] carries the button table (xterm buttons 4-7).
    #[must_use]
    pub fn encode_mouse_wheel(
        &self,
        dir: aterm_types::mouse::WheelDir,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        // X10 mode (9) is press-only — no wheel events.
        if self.modes.mouse_mode == MouseMode::None || self.modes.mouse_mode == MouseMode::X10 {
            return None;
        }

        let cb = dir.code() | modifiers;
        Some(encode_mouse(cb, col, row, self.modes.mouse_encoding, false))
    }

    /// Check if mouse tracking is enabled.
    #[must_use]
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.modes.mouse_mode != MouseMode::None
    }

    /// Get the current mouse tracking mode.
    #[must_use]
    pub fn mouse_mode(&self) -> MouseMode {
        self.modes.mouse_mode
    }

    /// Get the current mouse encoding format.
    #[must_use]
    pub fn mouse_encoding(&self) -> MouseEncoding {
        self.modes.mouse_encoding
    }

    /// Check if focus reporting is enabled.
    #[must_use]
    pub fn focus_reporting_enabled(&self) -> bool {
        self.modes.focus_reporting
    }

    /// Check if DEC mode 2031 (color-scheme update notifications) is enabled.
    #[must_use]
    pub fn report_color_scheme_enabled(&self) -> bool {
        self.modes.report_color_scheme
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;
    use aterm_types::mouse::WheelDir;

    /// SGR (DECSET 1006) so the button code is readable as decimal text rather
    /// than a `+32` byte — the encoding a modern TUI actually asks for.
    fn sgr_tracking() -> Terminal {
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?1000h\x1b[?1006h");
        t
    }

    fn wheel(t: &Terminal, dir: WheelDir) -> String {
        String::from_utf8(t.encode_mouse_wheel(dir, 10, 5, 0).expect("wheel encodes"))
            .expect("SGR reports are ASCII")
    }

    /// All four directions: 64/65 up/down and 66/67 left/right (xterm's
    /// buttons 4-7).
    #[test]
    fn wheel_encodes_all_four_xterm_buttons() {
        let t = sgr_tracking();
        assert_eq!(wheel(&t, WheelDir::Up), "\x1b[<64;11;6M");
        assert_eq!(wheel(&t, WheelDir::Down), "\x1b[<65;11;6M");
        assert_eq!(wheel(&t, WheelDir::Left), "\x1b[<66;11;6M");
        assert_eq!(wheel(&t, WheelDir::Right), "\x1b[<67;11;6M");
    }

    /// Modifiers OR into the horizontal codes exactly as they do into the
    /// vertical ones (shift=4, alt=8, ctrl=16) — one code path, no special case.
    #[test]
    fn wheel_modifiers_or_into_the_horizontal_codes() {
        let t = sgr_tracking();
        let bytes = t
            .encode_mouse_wheel(WheelDir::Right, 10, 5, aterm_types::mouse::CTRL_MASK)
            .expect("wheel encodes");
        assert_eq!(String::from_utf8(bytes).expect("ascii"), "\x1b[<83;11;6M");
    }

    /// Tracking off: every direction reports nothing; the wheel belongs to
    /// aterm's own viewport.
    #[test]
    fn wheel_reports_nothing_without_tracking() {
        let t = Terminal::new(24, 80);
        for dir in [
            WheelDir::Up,
            WheelDir::Down,
            WheelDir::Left,
            WheelDir::Right,
        ] {
            assert_eq!(t.encode_mouse_wheel(dir, 10, 5, 0), None, "{dir:?}");
        }
        // …and after tracking is turned on and off again.
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?1000h\x1b[?1000l");
        for dir in [WheelDir::Left, WheelDir::Right] {
            assert_eq!(t.encode_mouse_wheel(dir, 10, 5, 0), None, "{dir:?}");
        }
    }

    /// X10 (DECSET 9) is press-only: no wheel on either axis.
    #[test]
    fn wheel_reports_nothing_in_x10_mode() {
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?9h");
        for dir in [
            WheelDir::Up,
            WheelDir::Down,
            WheelDir::Left,
            WheelDir::Right,
        ] {
            assert_eq!(t.encode_mouse_wheel(dir, 10, 5, 0), None, "{dir:?}");
        }
    }

    /// The report memo repeats only the position last recorded, under the mode
    /// and encoding it was recorded in: a new position, a new tracking mode or a
    /// new encoding (cells to DEC 1016 pixels) is a fresh report.
    #[test]
    fn the_report_memo_repeats_only_the_same_position_in_the_same_terms() {
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?1003h\x1b[?1006h");
        assert!(
            !t.mouse_motion_repeats_last_report(7, 4),
            "nothing reported yet"
        );
        t.note_mouse_report(7, 4);
        assert!(t.mouse_motion_repeats_last_report(7, 4));
        assert!(!t.mouse_motion_repeats_last_report(8, 4));
        assert!(!t.mouse_motion_repeats_last_report(7, 5));
        t.process(b"\x1b[?1002h");
        assert!(
            !t.mouse_motion_repeats_last_report(7, 4),
            "a mode change starts a new conversation"
        );
        t.note_mouse_report(7, 4);
        t.process(b"\x1b[?1016h");
        assert!(
            !t.mouse_motion_repeats_last_report(7, 4),
            "cell and pixel coordinates are never the same position"
        );
    }
}
