// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse encoding types shared across aterm crates.

/// Mouse tracking mode.
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

/// Mouse coordinate encoding format for reports.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEncoding {
    /// X10 compatibility mode - coordinates encoded as single bytes (limited to 223).
    #[default]
    X10 = 0,
    /// UTF-8 encoding (1005).
    Utf8 = 1,
    /// SGR encoding (1006) - coordinates as decimal parameters, supports larger values.
    Sgr = 2,
    /// URXVT encoding (1015) - decimal parameters without the '<' prefix.
    Urxvt = 3,
    /// SGR pixel mode (1016) - like SGR but coordinates are in pixels, not cells.
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
    /// The thumb "back" button.
    Back,
    /// The thumb "forward" button.
    Forward,
}

impl MouseButton {
    /// The xterm button code for the report's `Cb` byte.
    #[inline]
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        }
    }

    /// A dense 0-based index for per-button bookkeeping (bitsets, arrays).
    #[inline]
    #[must_use]
    pub const fn slot(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }
}

/// The direction of one wheel notch or trackpad flick, on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WheelDir {
    /// Wheel up / content scrolled toward older lines (xterm button 4).
    Up,
    /// Wheel down (xterm button 5).
    Down,
    /// Wheel tilted / swiped LEFT (xterm button 6).
    Left,
    /// Wheel tilted / swiped RIGHT (xterm button 7).
    Right,
}

impl WheelDir {
    /// The xterm wheel button code for the report's `Cb` byte.
    #[inline]
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            WheelDir::Up => 64,
            WheelDir::Down => 65,
            WheelDir::Left => 66,
            WheelDir::Right => 67,
        }
    }

    /// `Some(true)` for [`WheelDir::Up`], `Some(false)` for [`WheelDir::Down`], `None` for the
    /// horizontal axis.
    #[inline]
    #[must_use]
    pub const fn vertical_up(self) -> Option<bool> {
        match self {
            WheelDir::Up => Some(true),
            WheelDir::Down => Some(false),
            WheelDir::Left | WheelDir::Right => None,
        }
    }

    /// Whether this is the horizontal (tilt-wheel / two-finger-swipe) axis.
    #[inline]
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        self.vertical_up().is_none()
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

    #[test]
    fn button_slots_are_dense_and_distinct() {
        let slots: Vec<u8> = [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            MouseButton::Back,
            MouseButton::Forward,
        ]
        .iter()
        .map(|b| b.slot())
        .collect();
        assert_eq!(slots, vec![0, 1, 2, 3, 4]);
        assert_eq!(MouseButton::Back.code() & 7, MouseButton::Left.code() & 7);
    }

    #[test]
    fn only_vertical_wheel_directions_carry_a_viewport_sense() {
        assert_eq!(WheelDir::Up.vertical_up(), Some(true));
        assert_eq!(WheelDir::Down.vertical_up(), Some(false));
        assert_eq!(WheelDir::Left.vertical_up(), None);
        assert_eq!(WheelDir::Right.vertical_up(), None);
        assert!(!WheelDir::Up.is_horizontal());
        assert!(!WheelDir::Down.is_horizontal());
        assert!(WheelDir::Left.is_horizontal());
        assert!(WheelDir::Right.is_horizontal());
    }
}
