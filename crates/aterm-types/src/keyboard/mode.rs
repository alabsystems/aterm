// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The terminal mode flags that affect keyboard encoding.

bitflags! {
    /// Terminal mode flags relevant to keyboard encoding: a subset of the full terminal modes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct KeyboardMode: u32 {
        /// Kitty keyboard protocol: disambiguate escape codes (CSI u encoding).
        const DISAMBIGUATE_ESC_CODES = 1 << 0;
        /// Kitty keyboard protocol: report key repeat/release events.
        const REPORT_EVENT_TYPES = 1 << 1;
        /// Application cursor keys mode (DECCKM) — arrows use SS3 instead of CSI.
        const APP_CURSOR = 1 << 2;
        /// Application keypad mode (DECKPAM) — numpad uses SS3 sequences.
        const APP_KEYPAD = 1 << 3;
        /// xterm modifyOtherKeys level 1.
        const XTERM_MODIFY_OTHER_KEYS_LEVEL1 = 1 << 4;
        /// xterm modifyOtherKeys level 2.
        const XTERM_MODIFY_OTHER_KEYS_LEVEL2 = 1 << 5;
        /// xterm formatOtherKeys enabled (CSI u format instead of CSI 27;mod;code~).
        const XTERM_FORMAT_OTHER_KEYS = 1 << 6;
        /// Kitty keyboard protocol: report alternate key values (shifted/base layout).
        const REPORT_ALTERNATE_KEYS = 1 << 7;
        /// Kitty keyboard protocol: report all keys as escape sequences.
        const REPORT_ALL_KEYS_AS_ESC = 1 << 8;
        /// Kitty keyboard protocol: report associated text with key events.
        const REPORT_ASSOCIATED_TEXT = 1 << 9;
        /// VT52 compatibility mode — arrow keys use `ESC A`..`ESC D` (no CSI/SS3).
        const VT52_MODE = 1 << 10;
        /// DECBKM (DEC private mode 67): Backspace sends BS (`0x08`) instead of DEL (`0x7f`).
        const BACKARROW_SENDS_BS = 1 << 11;
        /// Disables the ESC prefix on Alt-modified keys in legacy mode (DEC mode 1039 reset).
        const ALT_NO_ESC = 1 << 12;
        /// Prefixes Meta-modified keys with ESC in legacy encoding (DEC private mode 1036).
        const META_SENDS_ESC = 1 << 13;
        /// Strips the NumLock modifier bit before encoding (DEC private mode 1035 reset).
        const NO_SPECIAL_MODIFIERS = 1 << 14;
        /// ConPTY win32-input-mode (DEC private mode 9001) `INPUT_RECORD` key encoding.
        const WIN32_INPUT = 1 << 15;
        /// Marks the [`Self::WIN32_INPUT`] console host as bundled OpenConsole.
        const OPENCONSOLE_HOST = 1 << 16;

        /// Mask of all Kitty progressive-enhancement keyboard flags.
        const KITTY_PROTOCOL_FLAGS = Self::DISAMBIGUATE_ESC_CODES.bits()
            | Self::REPORT_EVENT_TYPES.bits()
            | Self::REPORT_ALTERNATE_KEYS.bits()
            | Self::REPORT_ALL_KEYS_AS_ESC.bits()
            | Self::REPORT_ASSOCIATED_TEXT.bits();
    }
}

impl KeyboardMode {
    /// Get the xterm modifyOtherKeys level (0, 1, or 2).
    #[inline]
    #[must_use]
    pub const fn xterm_modify_other_keys_level(self) -> u8 {
        if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL2) {
            2
        } else if self.contains(Self::XTERM_MODIFY_OTHER_KEYS_LEVEL1) {
            1
        } else {
            0
        }
    }

    /// Check if xterm formatOtherKeys is enabled.
    #[inline]
    #[must_use]
    pub const fn xterm_format_other_keys(self) -> bool {
        self.contains(Self::XTERM_FORMAT_OTHER_KEYS)
    }
}
