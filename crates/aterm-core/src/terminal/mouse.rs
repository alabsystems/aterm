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

    /// Encode a mouse wheel event.
    ///
    /// Returns the escape sequence to send to the application, or `None` if
    /// mouse reporting is disabled. Coordinates are 0-indexed.
    #[must_use]
    pub fn encode_mouse_wheel(
        &self,
        up: bool,
        col: u16,
        row: u16,
        modifiers: u8,
    ) -> Option<Vec<u8>> {
        // X10 mode (9) is press-only — no wheel events.
        if self.modes.mouse_mode == MouseMode::None || self.modes.mouse_mode == MouseMode::X10 {
            return None;
        }

        let button = if up { 64u8 } else { 65 };
        let cb = button | modifiers;
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
