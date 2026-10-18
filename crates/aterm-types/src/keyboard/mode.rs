// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The terminal mode flags that affect keyboard encoding.

bitflags! {
    /// Terminal mode flags relevant to keyboard encoding: a subset of the full terminal modes.
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
        /// DECBKM (DEC private mode 67): Backspace sends BS (0x08) instead of DEL (0x7f), and
        /// Ctrl+Backspace the other (xterm `backarrowKey`). Legacy encoding only.
        const BACKARROW_SENDS_BS = 1 << 11;
        /// xterm `altSendsEscape` reset (DEC private mode 1039): an Alt-modified key in legacy
        /// encoding goes without the ESC prefix. A negative flag, so `empty()` keeps xterm's
        /// default ESC-prefixed Alt.
        const ALT_NO_ESC = 1 << 12;
        /// xterm `metaSendsEscape` set (DEC private mode 1036): a Meta-modified key in legacy
        /// encoding is ESC-prefixed like Alt. Off by default.
        const META_SENDS_ESC = 1 << 13;
        /// xterm `numLock` reset (DEC private mode 1035): the NumLock modifier bit is stripped
        /// before encoding. Absent (the default, xterm's power-on state) keeps it.
        const NO_SPECIAL_MODIFIERS = 1 << 14;
        /// ConPTY win32-input-mode (DEC private mode 9001) is set: conhost, which requests it
        /// at every ConPTY start, wants INPUT_RECORD key reports
        /// (`CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, microsoft/terminal spec #4999). It is the only
        /// way a key carries Windows modifiers across the pipe: legacy VT has no Shift+Enter or
        /// Ctrl+Enter, and conhost reads aterm's Shift+Enter LF as Ctrl+Enter. Once conhost has
        /// read one record it also holds a lone or dangling ESC, so Escape, Ctrl+[, Alt+[,
        /// Alt+Backspace and the like go out as records too (`conhost_holds_once_switched` in
        /// `encode.rs`). Ctrl+H and Ctrl+Shift+H go out as the same record, the H key with
        /// Ctrl alone: conhost reads their legacy BS as Ctrl+Backspace, which PSReadLine and
        /// cmd.exe take as a word delete, and PSReadLine inserts a literal BS on a record that
        /// carries Shift (measured 2026-10-16; `win32_ctrl_character_record`). Ctrl+Backspace
        /// keeps its BS.
        ///
        /// A negotiation bit with the host, not a kitty flag, in this word only because the
        /// encoder reads one word: it is never part of the `CSI ? flags u` answer, the kitty
        /// stack, or any projection an application sees (`kitty_suppresses_predictive_echo`,
        /// `kitty_reports_functional_keys`). In the encoder it outranks a kitty push for the
        /// keys it covers, because conhost drops the CSI-u reports written back; every other key
        /// follows the kitty flags.
        const WIN32_INPUT = 1 << 15;

        /// Aggregate: any kitty progressive-enhancement flag. With one set the app negotiated
        /// kitty semantics, which supersede xterm modifyOtherKeys.
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
