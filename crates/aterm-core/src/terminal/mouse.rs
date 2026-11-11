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

/// Whether tracking mode `mode` reports a pointer motion with `button` held
/// (`3`: none held): button-event tracking (DEC 1002) while a button is
/// held, any-event tracking (1003) always, and never X10 (9), normal (1000)
/// or none. The one rule [`Terminal::encode_mouse_motion`] encodes by and the
/// lock-free mirror answers ([`super::ModeMirror::reports_motion`]), so a
/// host that asks the mirror whether a drag reaches the program is told what
/// the encoder will do (its per-cell deduplication aside).
#[inline]
pub(crate) const fn motion_reported(mode: MouseMode, button: u8) -> bool {
    match mode {
        MouseMode::ButtonEvent => button != 3,
        MouseMode::AnyEvent => true,
        // None, X10, Normal, and a variant added later: not reported.
        _ => false,
    }
}

/// Whether tracking mode `mode` reports a wheel notch (xterm buttons 4-7):
/// every tracking mode but X10 (9), which reports presses only, and never
/// none. The one rule [`Terminal::encode_mouse_wheel`] encodes by and the
/// lock-free mirror answers ([`super::ModeMirror::reports_wheel`]), so a host
/// that asks the mirror whether a wheel on the report route reaches the
/// program is told what the encoder will do.
#[inline]
pub(crate) const fn wheel_reported(mode: MouseMode) -> bool {
    match mode {
        MouseMode::Normal | MouseMode::ButtonEvent | MouseMode::AnyEvent => true,
        // None, X10, and a variant added later: not reported.
        _ => false,
    }
}

impl Terminal {
    /// Whether a motion report carrying `(x, y)` would only repeat the last report's position,
    /// and so is not to be sent. xterm's DEC 1002/1003 report motion "only if the mouse pointer
    /// has moved to a different character cell", with one memo set by every press, release and
    /// reported motion; kitty and Alacritty gate the same way. `(x, y)` are the report's own
    /// coordinates, so under DEC 1016 (SGR-pixel) a sub-cell move is new, as in kitty. A memo
    /// written under another tracking mode or encoding never matches: the program has seen no
    /// report in the new terms.
    #[inline]
    #[must_use]
    pub fn mouse_motion_repeats_last_report(&self, x: u16, y: u16) -> bool {
        self.mouse_report_memo == Some(self.mouse_report_at(x, y))
    }

    /// Record that the host just produced a mouse report (press, release, wheel notch or motion)
    /// carrying `(x, y)`, for [`Self::mouse_motion_repeats_last_report`]. The input seam writes
    /// it under this terminal's lock.
    #[inline]
    pub const fn note_mouse_report(&mut self, x: u16, y: u16) {
        self.mouse_report_memo = Some(self.mouse_report_at(x, y));
    }

    #[inline]
    const fn mouse_report_at(&self, x: u16, y: u16) -> MouseReportMemo {
        MouseReportMemo {
            mode: self.modes.mouse_mode,
            encoding: self.modes.mouse_encoding,
            x,
            y,
        }
    }

    /// Encode a mouse button press event.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    #[inline]
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
    #[inline]
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

        // Pass the original button: the encoder substitutes the button-3 release code only for
        // the legacy formats, so an SGR release keeps the button and the 'm' terminator.
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
    #[inline]
    #[must_use]
    pub fn encode_mouse_motion(
        &self,
        button: u8,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        if !motion_reported(self.modes.mouse_mode, button) {
            return None;
        }

        // Motion events have bit 32 set
        let cb = button | modifiers | 32;
        Some(encode_mouse(cb, col, row, self.modes.mouse_encoding, false))
    }

    /// Encode a mouse wheel event in any of the four wheel directions.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    /// [`WheelDir::code`](aterm_types::mouse::WheelDir::code) carries the button table
    /// (xterm buttons 4-7).
    #[inline]
    #[must_use]
    pub fn encode_mouse_wheel(
        &self,
        dir: aterm_types::mouse::WheelDir,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        // X10 mode (9) is press-only — no wheel events.
        if !wheel_reported(self.modes.mouse_mode) {
            return None;
        }

        let cb = dir.code() | modifiers;
        Some(encode_mouse(cb, col, row, self.modes.mouse_encoding, false))
    }

    /// Check if mouse tracking is enabled.
    #[inline]
    #[must_use]
    pub const fn mouse_tracking_enabled(&self) -> bool {
        !matches!(self.modes.mouse_mode, MouseMode::None)
    }

    /// Get the current mouse tracking mode.
    #[inline]
    #[must_use]
    pub const fn mouse_mode(&self) -> MouseMode {
        self.modes.mouse_mode
    }

    /// Get the current mouse encoding format.
    #[inline]
    #[must_use]
    pub const fn mouse_encoding(&self) -> MouseEncoding {
        self.modes.mouse_encoding
    }

    /// Check if focus reporting is enabled.
    #[inline]
    #[must_use]
    pub const fn focus_reporting_enabled(&self) -> bool {
        self.modes.focus_reporting
    }

    /// How many times the program has turned DEC 1004 on from off (`CSI ? 1004 h`,
    /// XTRESTORE), wrapping: a host that compares it with the value it last saw learns of
    /// an enable it has not answered yet. The host's own arming (`apply_config`) is not
    /// counted. Never rewinds, not even at a reset.
    #[inline]
    #[must_use]
    pub const fn focus_arms(&self) -> u32 {
        self.transient.focus_arms
    }

    /// Check if DEC mode 2031 (color-scheme update notifications) is enabled.
    #[inline]
    #[must_use]
    pub const fn report_color_scheme_enabled(&self) -> bool {
        self.modes.report_color_scheme
    }
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;
