// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal mode bitflags used for keyboard encoding.
//!
//! This type captures the subset/superset of mode flags that influence keyboard
//! sequence generation (legacy VT/xterm + Kitty keyboard protocol).

bitflags! {
    /// Terminal mode flags used by keyboard encoding.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TermMode: u32 {
        /// Cursor is visible (DECTCEM).
        const SHOW_CURSOR = 1 << 0;
        /// Application cursor keys mode (DECCKM).
        const APP_CURSOR = 1 << 1;
        /// Application keypad mode (DECKPAM).
        const APP_KEYPAD = 1 << 2;
        /// Any mouse tracking mode is enabled.
        const MOUSE_REPORT_CLICK = 1 << 3;
        /// Bracketed paste mode.
        const BRACKETED_PASTE = 1 << 4;
        /// SGR mouse mode (1006).
        const SGR_MOUSE = 1 << 5;
        /// Mouse motion tracking (1003).
        const MOUSE_MOTION = 1 << 6;
        /// Auto-wrap mode (DECAWM).
        const LINE_WRAP = 1 << 7;
        /// LNM mode - LF also does CR.
        const LINE_FEED_NEW_LINE = 1 << 8;
        /// Origin mode (DECOM).
        const ORIGIN = 1 << 9;
        /// Insert mode (IRM).
        const INSERT = 1 << 10;
        /// Focus in/out reporting (1004).
        const FOCUS_IN_OUT = 1 << 11;
        /// Alternate screen buffer active.
        const ALT_SCREEN = 1 << 12;
        /// Mouse button and drag tracking (1002).
        const MOUSE_DRAG = 1 << 13;
        /// UTF-8 mouse encoding (1005).
        const UTF8_MOUSE = 1 << 14;
        /// Alternate scroll mode (1007).
        const ALTERNATE_SCROLL = 1 << 15;
        /// Vi mode is active.
        const VI = 1 << 16;
        /// Urgency hints mode.
        const URGENCY_HINTS = 1 << 17;
        /// Synchronized output mode (2026).
        const SYNCHRONIZED_OUTPUT = 1 << 18;
        /// Reverse video mode (DECSET 5).
        const REVERSE_VIDEO = 1 << 19;
        /// Cursor blink mode (DECSET 12).
        const CURSOR_BLINK = 1 << 20;
        /// 132 column mode (DECSET 3).
        const COLUMN_MODE_132 = 1 << 21;
        /// Reverse wraparound mode (DECSET 45).
        const REVERSE_WRAPAROUND = 1 << 22;
        /// VT52 compatibility mode.
        const VT52_MODE = 1 << 23;

        // Kitty keyboard protocol flags (CSI > u).
        /// Disambiguate escape codes - send Esc, Alt+key, Ctrl+key using CSI u.
        const DISAMBIGUATE_ESC_CODES = 1 << 24;
        /// Report key repeat and release events.
        const REPORT_EVENT_TYPES = 1 << 25;
        /// Report alternate key values (shifted keys, base layout key).
        const REPORT_ALTERNATE_KEYS = 1 << 26;
        /// Report all keys as escape sequences (including Enter, Tab, Backspace).
        const REPORT_ALL_KEYS_AS_ESC = 1 << 27;
        /// Report associated text with key events.
        const REPORT_ASSOCIATED_TEXT = 1 << 28;

        // xterm XTMODKEYS/XTFMTKEYS (modifyOtherKeys/formatOtherKeys).
        /// modifyOtherKeys level 1 (CSI > 4 ; 1 m).
        const XTERM_MODIFY_OTHER_KEYS_LEVEL1 = 1 << 29;
        /// modifyOtherKeys level 2 (CSI > 4 ; 2 m).
        const XTERM_MODIFY_OTHER_KEYS_LEVEL2 = 1 << 30;
        /// formatOtherKeys enabled (CSI > 4 ; 1 f).
        const XTERM_FORMAT_OTHER_KEYS = 1 << 31;

        /// Aggregate: any Kitty keyboard protocol flag is active.
        const KITTY_KEYBOARD_PROTOCOL = Self::DISAMBIGUATE_ESC_CODES.bits()
            | Self::REPORT_EVENT_TYPES.bits()
            | Self::REPORT_ALTERNATE_KEYS.bits()
            | Self::REPORT_ALL_KEYS_AS_ESC.bits()
            | Self::REPORT_ASSOCIATED_TEXT.bits();

        /// Aggregate: any mouse mode is active.
        const MOUSE_MODE = Self::MOUSE_REPORT_CLICK.bits()
            | Self::MOUSE_MOTION.bits()
            | Self::MOUSE_DRAG.bits();

        /// All flags.
        const ANY = !0;
    }
}

impl TermMode {
    /// Build a `TermMode` from the keyboard-relevant state only.
    ///
    /// This is the canonical constructor for keyboard-related flags:
    /// `APP_CURSOR`, `APP_KEYPAD`, `VT52_MODE`, 5 Kitty flags, and 3 xterm flags.
    /// Both `aterm-core` and `aterm-alacritty-bridge` should delegate to this
    /// rather than maintaining their own handwritten flag lists.
    #[must_use]
    pub fn from_keyboard_state(
        app_cursor: bool,
        app_keypad: bool,
        vt52: bool,
        kitty: crate::KittyKeyboardFlags,
        xterm: crate::XtermKeyboardState,
    ) -> Self {
        let mut flags = Self::empty();

        if app_cursor {
            flags |= Self::APP_CURSOR;
        }
        if app_keypad {
            flags |= Self::APP_KEYPAD;
        }
        if vt52 {
            flags |= Self::VT52_MODE;
        }

        // Kitty keyboard protocol flags
        if kitty.disambiguate() {
            flags |= Self::DISAMBIGUATE_ESC_CODES;
        }
        if kitty.report_events() {
            flags |= Self::REPORT_EVENT_TYPES;
        }
        if kitty.report_alternates() {
            flags |= Self::REPORT_ALTERNATE_KEYS;
        }
        if kitty.report_all_keys() {
            flags |= Self::REPORT_ALL_KEYS_AS_ESC;
        }
        if kitty.report_text() {
            flags |= Self::REPORT_ASSOCIATED_TEXT;
        }

        // xterm modifyOtherKeys / formatOtherKeys
        match xterm.modify_other_keys().unwrap_or(0) {
            1 => flags |= Self::XTERM_MODIFY_OTHER_KEYS_LEVEL1,
            2 => flags |= Self::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
            _ => {}
        }
        if xterm.format_other_keys() == 1 {
            flags |= Self::XTERM_FORMAT_OTHER_KEYS;
        }

        flags
    }

    /// Project the keyboard-relevant flags into `KeyboardMode`.
    ///
    /// This is the canonical projection from the wide `TermMode` bitset to
    /// the narrow encoder-facing `KeyboardMode`. Both `aterm-core` and
    /// `aterm-alacritty-bridge` should delegate to this instead of maintaining
    /// handwritten flag-by-flag mappings.
    #[must_use]
    pub fn to_keyboard_mode(self) -> super::KeyboardMode {
        use super::KeyboardMode;
        let mut mode = KeyboardMode::empty();

        if self.contains(Self::APP_CURSOR) {
            mode |= KeyboardMode::APP_CURSOR;
        }
        if self.contains(Self::APP_KEYPAD) {
            mode |= KeyboardMode::APP_KEYPAD;
        }
        if self.contains(Self::DISAMBIGUATE_ESC_CODES) {
            mode |= KeyboardMode::DISAMBIGUATE_ESC_CODES;
        }
        if self.contains(Self::REPORT_EVENT_TYPES) {
            mode |= KeyboardMode::REPORT_EVENT_TYPES;
        }
        if self.contains(Self::REPORT_ALTERNATE_KEYS) {
            mode |= KeyboardMode::REPORT_ALTERNATE_KEYS;
        }
        if self.contains(Self::REPORT_ALL_KEYS_AS_ESC) {
            mode |= KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
        }
        if self.contains(Self::REPORT_ASSOCIATED_TEXT) {
            mode |= KeyboardMode::REPORT_ASSOCIATED_TEXT;
        }
        if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL1) {
            mode |= KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
        }
        if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL2) {
            mode |= KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
        }
        if self.contains(Self::XTERM_FORMAT_OTHER_KEYS) {
            mode |= KeyboardMode::XTERM_FORMAT_OTHER_KEYS;
        }
        if self.contains(Self::VT52_MODE) {
            mode |= KeyboardMode::VT52_MODE;
        }

        mode
    }

    /// Get the xterm modifyOtherKeys level (0, 1, or 2).
    #[must_use]
    pub fn xterm_modify_other_keys_level(self) -> u8 {
        if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL2) {
            2
        } else if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL1) {
            1
        } else {
            0
        }
    }

    /// Check if xterm formatOtherKeys is enabled.
    #[must_use]
    pub fn xterm_format_other_keys(self) -> bool {
        self.contains(Self::XTERM_FORMAT_OTHER_KEYS)
    }
}
