// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard-relevant terminal mode flags.
//!
//! A focused subset of terminal mode flags that affect keyboard encoding.
//! This type is bridge-agnostic and lives in `aterm-types` so both
//! `aterm-core-ffi` and `aterm-alacritty-bridge` can use it without
//! creating a dependency between them.

bitflags! {
    /// Terminal mode flags relevant to keyboard encoding.
    ///
    /// Contains only the flags that affect how key presses are encoded
    /// into escape sequences. This is intentionally a subset of the full
    /// terminal mode flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct KeyboardMode: u16 {
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
        /// DECBKM (DEC private mode 67): the Backspace key sends BS (0x08) instead
        /// of the default DEL (0x7f). Affects only legacy encoding; the Ctrl
        /// modifier inverts it (xterm `backarrowKey`).
        const BACKARROW_SENDS_BS = 1 << 11;
        /// xterm `altSendsEscape` OFF (DEC private mode 1039 reset): when present,
        /// an `Alt`-modified key in legacy encoding is sent WITHOUT the ESC (0x1b)
        /// prefix. Absent (the default) keeps xterm's ESC-prefixed Alt behavior.
        /// Modeled as a negative flag so the default `empty()` mode preserves the
        /// historical "Alt always prefixes ESC" contract.
        const ALT_NO_ESC = 1 << 12;
        /// xterm `metaSendsEscape` ON (DEC private mode 1036 set): when present,
        /// a `Meta`-modified key in legacy encoding is prefixed with ESC (0x1b),
        /// mirroring the `Alt` ESC behavior. Absent (the default) leaves Meta
        /// unhandled in the legacy path, matching prior behavior.
        const META_SENDS_ESC = 1 << 13;
        /// xterm `numLock`/special-modifiers OFF (DEC private mode 1035 reset):
        /// when present, the `NumLock` modifier bit is NOT treated as a real
        /// modifier and is stripped before encoding. Absent (the default) keeps
        /// NumLock as a special modifier, matching xterm's power-on `numLock`.
        const NO_SPECIAL_MODIFIERS = 1 << 14;

        /// Aggregate: any kitty keyboard protocol progressive-enhancement flag.
        /// When any of these is active the app negotiated kitty semantics, which
        /// supersede xterm modifyOtherKeys and the legacy keypad fold.
        const KITTY_PROTOCOL_FLAGS = Self::DISAMBIGUATE_ESC_CODES.bits()
            | Self::REPORT_EVENT_TYPES.bits()
            | Self::REPORT_ALTERNATE_KEYS.bits()
            | Self::REPORT_ALL_KEYS_AS_ESC.bits()
            | Self::REPORT_ASSOCIATED_TEXT.bits();
    }
}

impl KeyboardMode {
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
