// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse encoding types shared across aterm crates.

/// Mouse tracking mode.
///
/// Controls what mouse events the terminal reports back to the application.
/// Only one mouse tracking mode can be active at a time (mutually exclusive).
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseMode {
    /// No mouse tracking (default).
    #[default]
    None = 0,
    /// Normal tracking mode (1000) - report button press/release.
    Normal = 1,
    /// Button-event tracking mode (1002) - report press/release and motion while button pressed.
    ButtonEvent = 2,
    /// Any-event tracking mode (1003) - report all motion events.
    AnyEvent = 3,
    /// X10 compatibility mode (9) - report button press only (no release, no motion).
    X10 = 4,
}

/// Mouse coordinate encoding format.
///
/// Controls how mouse coordinates are encoded in reports.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEncoding {
    /// X10 compatibility mode - coordinates encoded as single bytes (limited to 223).
    #[default]
    X10 = 0,
    /// UTF-8 encoding (1005) - coordinates as UTF-8 characters.
    /// Like X10 but uses UTF-8 encoding for coordinates > 127, supporting up to 2015.
    /// Format: CSI M Cb Cx Cy (where Cx, Cy are UTF-8 encoded)
    Utf8 = 1,
    /// SGR encoding (1006) - coordinates as decimal parameters, supports larger values.
    /// Format: CSI < Cb ; Cx ; Cy M (press) or CSI < Cb ; Cx ; Cy m (release)
    Sgr = 2,
    /// URXVT encoding (1015) - decimal parameters without the '<' prefix.
    /// Format: CSI Cb ; Cx ; Cy M
    Urxvt = 3,
    /// SGR pixel mode (1016) - like SGR but coordinates are in pixels, not cells.
    /// Format: CSI < Cb ; Px ; Py M (press) or CSI < Cb ; Px ; Py m (release)
    SgrPixel = 4,
}

/// Mouse buttons used for press/release/motion events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MouseButton {
    /// Left mouse button.
    Left,
    /// Middle mouse button.
    Middle,
    /// Right mouse button.
    Right,
}

impl MouseButton {
    /// Return the X10 button code.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

/// Shift modifier mask for mouse encoding.
pub const SHIFT_MASK: u8 = 4;
/// Alt/Meta modifier mask for mouse encoding.
pub const ALT_MASK: u8 = 8;
/// Ctrl modifier mask for mouse encoding.
pub const CTRL_MASK: u8 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify MouseMode discriminants match checkpoint wire format (#7278).
    #[test]
    fn mouse_mode_discriminants_match_wire_format() {
        assert_eq!(MouseMode::None as u8, 0);
        assert_eq!(MouseMode::Normal as u8, 1);
        assert_eq!(MouseMode::ButtonEvent as u8, 2);
        assert_eq!(MouseMode::AnyEvent as u8, 3);
        assert_eq!(MouseMode::X10 as u8, 4);
    }

    /// Verify MouseEncoding discriminants match checkpoint wire format (#7278).
    #[test]
    fn mouse_encoding_discriminants_match_wire_format() {
        assert_eq!(MouseEncoding::X10 as u8, 0);
        assert_eq!(MouseEncoding::Utf8 as u8, 1);
        assert_eq!(MouseEncoding::Sgr as u8, 2);
        assert_eq!(MouseEncoding::Urxvt as u8, 3);
        assert_eq!(MouseEncoding::SgrPixel as u8, 4);
    }
}
