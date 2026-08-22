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
    /// The THUMB "back" button (XButton1 on Windows, `BTN_SIDE` on Linux,
    /// winit's `MouseButton::Back`) — xterm's button 8.
    Back,
    /// The THUMB "forward" button (XButton2 / `BTN_EXTRA`, winit's
    /// `MouseButton::Forward`) — xterm's button 9.
    Forward,
}

impl MouseButton {
    /// Return the xterm button code that goes into the report's `Cb` byte.
    ///
    /// Buttons 1-3 are 0..=2 (xterm's `Cb` base). The THUMB buttons are xterm's
    /// buttons 8 and 9, whose `Cb` base is `128 + (button - 8)` — the second
    /// extension block, above the wheel's 64..=67 — so they are 128 and 129, NOT
    /// 3 and 4. Getting that wrong would not have been a silent mislabel: `Cb`
    /// 3 is the LEGACY RELEASE code, so a "back" press would have arrived at a
    /// TUI as a button-up for whatever it last saw pressed.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        }
    }

    /// A DENSE 0-based index for per-button bookkeeping (bitsets, arrays).
    ///
    /// NOT the wire code, and the distinction is load-bearing: callers that kept
    /// a "this press was reported" bitset as `1 << (code & 7)` were correct only
    /// while every code was 0..=2. With the thumb buttons' 128/129, `& 7` folds
    /// Back onto Left's bit and Forward onto Middle's — a thumb release would
    /// then clear the LEFT button's bit and orphan a live drag's release. The
    /// dense index has no such aliasing and needs no mask.
    #[must_use]
    pub fn slot(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }
}

/// The direction of ONE wheel notch / trackpad flick — the full 4-WAY axis.
///
/// Deliberately NOT `#[non_exhaustive]`: unlike [`MouseButton`] (whose device
/// space genuinely keeps growing — xterm already reserves buttons 10/11) a wheel
/// has exactly two axes and two senses on each, and every consumer wants an
/// EXHAUSTIVE match so a fifth variant would be a compile error rather than a
/// silently-dropped gesture. That is the bug this type exists to kill: the
/// direction used to be a bare `dir_up: bool`, which cannot represent the
/// horizontal axis at all, so a tilt-wheel notch and a two-finger horizontal
/// trackpad swipe were dropped before they could ever be encoded.
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
    ///
    /// xterm ctlseqs, Mouse Tracking: "Wheel mice may return buttons 4 and 5 …
    /// the event codes are 64 and 65"; the horizontal pair continues the block
    /// as buttons 6 and 7, i.e. 66 (left) and 67 (right) — the codes X11 has
    /// carried for tilt wheels since XFree86, and what Neovim/tmux/less read for
    /// horizontal scrolling.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            WheelDir::Up => 64,
            WheelDir::Down => 65,
            WheelDir::Left => 66,
            WheelDir::Right => 67,
        }
    }

    /// `Some(true)` for [`WheelDir::Up`], `Some(false)` for [`WheelDir::Down`],
    /// and `None` for the horizontal axis.
    ///
    /// The `Option` is the point: every consumer that owns a VERTICAL viewport
    /// (aterm's scrollback, the palette list, a native view's scroll offset) must
    /// answer "what do I do with a horizontal flick?" explicitly, and the answer
    /// is always "nothing" — a terminal grid has no horizontal viewport. Handing
    /// those call sites a bool would resurrect the exact defect the guard in
    /// `on_mouse_wheel` was added to fix: a horizontal swipe scrolling DOWN.
    #[must_use]
    pub fn vertical_up(self) -> Option<bool> {
        match self {
            WheelDir::Up => Some(true),
            WheelDir::Down => Some(false),
            WheelDir::Left | WheelDir::Right => None,
        }
    }

    /// Whether this is the horizontal (tilt-wheel / two-finger-swipe) axis.
    #[must_use]
    pub fn is_horizontal(self) -> bool {
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

    /// xterm's `Cb` bases: buttons 1-3 are 0..=2, the thumb pair (xterm buttons
    /// 8/9) is 128/129. In particular NOT 3 — that is the legacy RELEASE code.
    #[test]
    fn thumb_buttons_encode_xterm_128_and_129() {
        assert_eq!(MouseButton::Left.code(), 0);
        assert_eq!(MouseButton::Middle.code(), 1);
        assert_eq!(MouseButton::Right.code(), 2);
        assert_eq!(MouseButton::Back.code(), 128);
        assert_eq!(MouseButton::Forward.code(), 129);
    }

    /// The bookkeeping index is DENSE and collision-free — the property
    /// `1 << (code & 7)` loses once the thumb buttons exist.
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
        // The aliasing the dense index exists to avoid.
        assert_eq!(MouseButton::Back.code() & 7, MouseButton::Left.code() & 7);
    }

    /// xterm wheel button codes: 64/65 vertical, 66/67 horizontal.
    #[test]
    fn wheel_directions_map_to_xterm_64_through_67() {
        assert_eq!(WheelDir::Up.code(), 64);
        assert_eq!(WheelDir::Down.code(), 65);
        assert_eq!(WheelDir::Left.code(), 66);
        assert_eq!(WheelDir::Right.code(), 67);
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
