// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse event encoding for terminal emulation.
//!
//! Delegates to shared encoding primitives in `aterm_types::mouse`.

use super::Terminal;
use super::types::{MouseEncoding, MouseMode};
use aterm_types::mouse::encode_mouse;

/// Focus transition state for terminal focus reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FocusState {
    /// Terminal focus was gained.
    Focused,
    /// Terminal focus was lost.
    Unfocused,
}

impl From<bool> for FocusState {
    fn from(focused: bool) -> Self {
        if focused {
            Self::Focused
        } else {
            Self::Unfocused
        }
    }
}

impl Terminal {
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

        // Pass the ORIGINAL button: the encoder substitutes the legacy
        // button-3 release code only for the formats that need it, so the SGR
        // fallback for out-of-range X10 coordinates keeps the button identity
        // and the 'm' terminator (#7473).
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

    /// Encode a mouse wheel event on any of the FOUR wheel directions.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    ///
    /// `dir` used to be a bare `up: bool` with the button hardcoded to 64/65,
    /// which made the horizontal axis INEXPRESSIBLE: a tilt wheel and a
    /// two-finger horizontal trackpad swipe had nowhere to go, so Neovim, tmux,
    /// and every other mouse-tracking app never saw xterm's buttons 6/7 (SGR
    /// 66/67) from aterm on ANY platform. [`WheelDir::code`] carries the whole
    /// button table now, so this function has no direction policy left in it.
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

    /// Encode a focus state transition.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// focus reporting is disabled.
    #[must_use]
    pub fn encode_focus_state(&self, focus_state: FocusState) -> Option<Vec<u8>> {
        if !self.modes.focus_reporting {
            return None;
        }
        Some(match focus_state {
            FocusState::Focused => vec![0x1b, b'[', b'I'],
            FocusState::Unfocused => vec![0x1b, b'[', b'O'],
        })
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

    /// All FOUR directions, not just the vertical pair: 64/65 up/down and
    /// 66/67 left/right (xterm's buttons 4-7). The horizontal half is the
    /// capability this widening added — it was previously unrepresentable.
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

    /// TRACKING OFF: every direction — the new horizontal pair included —
    /// reports NOTHING. The wheel then belongs to aterm's own viewport, and a
    /// horizontal flick has no viewport to move.
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
}
