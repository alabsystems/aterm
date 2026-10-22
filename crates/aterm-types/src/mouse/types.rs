// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Mouse encoding types shared across aterm crates.

/// Mouse tracking mode: which mouse events the terminal reports to the application. Only
/// one is active at a time.
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
    /// UTF-8 encoding (1005): like X10, with values over 127 UTF-8 encoded, up to 2015.
    /// Format: CSI M Cb Cx Cy.
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
    /// The thumb "back" button (XButton1 on Windows, `BTN_SIDE` on Linux, winit's
    /// `MouseButton::Back`), xterm's button 8.
    Back,
    /// The thumb "forward" button (XButton2 / `BTN_EXTRA`, winit's `MouseButton::Forward`),
    /// xterm's button 9.
    Forward,
}

impl MouseButton {
    /// The xterm button code for the report's `Cb` byte: 0..=2 for buttons 1-3, and
    /// `128 + (button - 8)` for the thumb buttons 8 and 9 (the second extension block, above
    /// the wheel's 64..=67). Not 3 and 4: `Cb` 3 is the legacy release code.
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

    /// A dense 0-based index for per-button bookkeeping (bitsets, arrays). Not the wire code:
    /// `1 << (code & 7)` folds Back (128) onto Left's bit and Forward onto Middle's.
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
///
/// Not `#[non_exhaustive]`, unlike [`MouseButton`]: a wheel has exactly two axes and two
/// senses, and consumers match exhaustively so a new variant is a compile error, not a
/// dropped gesture.
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
    /// The xterm wheel button code for the report's `Cb` byte: 64/65 for buttons 4/5
    /// (ctlseqs), and 66/67 for the horizontal pair, buttons 6/7, as Neovim, tmux and less
    /// read them.
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

    /// `Some(true)` for [`WheelDir::Up`], `Some(false)` for [`WheelDir::Down`], and `None`
    /// for the horizontal axis. An `Option`, not a bool, so every vertical viewport
    /// (scrollback, the palette list, a native view) handles a horizontal flick explicitly,
    /// by doing nothing, rather than scrolling down.
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

    /// The bookkeeping index is dense and collision-free, which `1 << (code & 7)` is not.
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
        // The aliasing the dense index avoids.
        assert_eq!(MouseButton::Back.code() & 7, MouseButton::Left.code() & 7);
    }

    /// Only the vertical axis answers "which way does the viewport move".
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
