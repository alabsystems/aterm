// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lock-free mirror of the terminal's input-encoding modes.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use aterm_types::keyboard::KeyboardMode;
use aterm_types::mouse::MouseMode;

/// The lock-free input-mode word.
#[derive(Debug, Default)]
pub struct ModeMirror {
    keyboard: AtomicU32,
    mouse: AtomicU8,
    focus: AtomicBool,
    focus_arms: AtomicU32,
    alternate_screen: AtomicBool,
    alternate_scroll: AtomicBool,
    bracketed_paste: AtomicBool,
}

impl ModeMirror {
    /// The keyboard encoding mode the terminal last published.
    #[inline]
    #[must_use]
    pub fn keyboard_mode(&self) -> KeyboardMode {
        KeyboardMode::from_bits_truncate(self.keyboard.load(Ordering::Acquire))
    }

    /// The mouse tracking mode the terminal last published.
    #[inline]
    #[must_use]
    pub fn mouse_mode(&self) -> MouseMode {
        mouse_mode_from_u8(self.mouse.load(Ordering::Acquire))
    }

    /// Lock-free twin of `Terminal::mouse_tracking_enabled()`.
    #[inline]
    #[must_use]
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.mouse.load(Ordering::Acquire) != 0
    }

    /// Whether the published tracking mode reports a pointer motion with `button` held (`3`: none):
    /// the rule `Terminal::encode_mouse_motion` encodes by (`super::mouse::motion_reported`),
    /// without the lock.
    #[inline]
    #[must_use]
    pub fn reports_motion(&self, button: u8) -> bool {
        super::mouse::motion_reported(self.mouse_mode(), button)
    }

    /// Whether the published tracking mode reports a wheel notch.
    #[inline]
    #[must_use]
    pub fn reports_wheel(&self) -> bool {
        super::mouse::wheel_reported(self.mouse_mode())
    }

    /// Lock-free twin of `Terminal::focus_reporting_enabled()` (DEC 1004), for the focus report
    /// written on the winit thread, which must never take an engine mutex.
    #[inline]
    #[must_use]
    pub fn focus_reporting_enabled(&self) -> bool {
        self.focus.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::focus_arms()`.
    #[inline]
    #[must_use]
    pub fn focus_arms(&self) -> u32 {
        self.focus_arms.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::is_alternate_screen()`.
    #[inline]
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::modes().alternate_scroll` (DEC 1007).
    #[inline]
    #[must_use]
    pub fn alternate_scroll(&self) -> bool {
        self.alternate_scroll.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::modes().bracketed_paste` (DEC 2004).
    #[inline]
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste.load(Ordering::Acquire)
    }

    #[inline]
    fn publish(&self, term: &super::Terminal) {
        self.keyboard
            .store(term.keyboard_mode().bits(), Ordering::Release);
        self.mouse.store(term.mouse_mode() as u8, Ordering::Release);
        self.focus
            .store(term.focus_reporting_enabled(), Ordering::Release);
        self.focus_arms.store(term.focus_arms(), Ordering::Release);
        self.alternate_screen
            .store(term.is_alternate_screen(), Ordering::Release);
        self.alternate_scroll
            .store(term.modes.alternate_scroll, Ordering::Release);
        self.bracketed_paste
            .store(term.modes.bracketed_paste, Ordering::Release);
    }
}

#[inline]
const fn mouse_mode_from_u8(v: u8) -> MouseMode {
    match v {
        1 => MouseMode::Normal,
        2 => MouseMode::ButtonEvent,
        3 => MouseMode::AnyEvent,
        4 => MouseMode::X10,
        _ => MouseMode::None,
    }
}

impl super::Terminal {
    /// The shared lock-free mirror of this terminal's input-encoding modes.
    #[inline]
    #[must_use]
    pub const fn mode_mirror(&self) -> &std::sync::Arc<ModeMirror> {
        &self.mode_mirror
    }

    /// Re-publish the fold.
    #[inline]
    pub(crate) fn refresh_mode_mirror(&self) {
        self.mode_mirror.publish(self);
    }

    /// Asserts the published word equals the live fold.
    #[cfg(debug_assertions)]
    pub(crate) fn debug_assert_mode_mirror_in_sync(&self) {
        debug_assert_eq!(
            self.mode_mirror.keyboard_mode(),
            self.keyboard_mode(),
            "keyboard-mode mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.mouse_mode(),
            self.mouse_mode(),
            "mouse-mode mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.focus_reporting_enabled(),
            self.focus_reporting_enabled(),
            "focus-report mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.focus_arms(),
            self.focus_arms(),
            "focus-arm mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.alternate_screen(),
            self.is_alternate_screen(),
            "alternate-screen mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.alternate_scroll(),
            self.modes.alternate_scroll,
            "alternate-scroll mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.bracketed_paste(),
            self.modes.bracketed_paste,
            "bracketed-paste mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
    }
}

#[cfg(test)]
#[path = "mode_mirror_tests.rs"]
mod tests;
