// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Key types and modifier definitions for keyboard input encoding.

/// A keyboard key, either a named special key or a character.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Key {
    /// A named special key (Enter, Tab, arrows, function keys, etc.)
    Named(NamedKey),
    /// A character key (letters, numbers, symbols)
    Character(char),
}

impl Key {
    /// Create a character key.
    #[must_use]
    pub fn character(c: char) -> Self {
        Key::Character(c)
    }

    /// Create a named key.
    #[must_use]
    pub fn named(key: NamedKey) -> Self {
        Key::Named(key)
    }
}

/// Named special keys.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NamedKey {
    // Navigation
    /// Up arrow key
    ArrowUp,
    /// Down arrow key
    ArrowDown,
    /// Left arrow key
    ArrowLeft,
    /// Right arrow key
    ArrowRight,
    /// Home key
    Home,
    /// End key
    End,
    /// Page Up key
    PageUp,
    /// Page Down key
    PageDown,

    // Editing
    /// Backspace key
    Backspace,
    /// Delete key
    Delete,
    /// Insert key
    Insert,
    /// Enter/Return key
    Enter,
    /// Tab key
    Tab,
    /// Escape key
    Escape,
    /// Space key (when sent as named key)
    Space,

    // Locks and system keys
    /// Caps Lock key
    CapsLock,
    /// Num Lock key
    NumLock,
    /// Scroll Lock key
    ScrollLock,
    /// Print Screen key
    PrintScreen,
    /// Pause/Break key
    Pause,
    /// Context menu key
    ContextMenu,

    // Function keys
    /// F1 function key
    F1,
    /// F2 function key
    F2,
    /// F3 function key
    F3,
    /// F4 function key
    F4,
    /// F5 function key
    F5,
    /// F6 function key
    F6,
    /// F7 function key
    F7,
    /// F8 function key
    F8,
    /// F9 function key
    F9,
    /// F10 function key
    F10,
    /// F11 function key
    F11,
    /// F12 function key
    F12,
    /// F13 function key
    F13,
    /// F14 function key
    F14,
    /// F15 function key
    F15,
    /// F16 function key
    F16,
    /// F17 function key
    F17,
    /// F18 function key
    F18,
    /// F19 function key
    F19,
    /// F20 function key
    F20,
    /// F21 function key
    F21,
    /// F22 function key
    F22,
    /// F23 function key
    F23,
    /// F24 function key
    F24,
    /// F25 function key
    F25,
    /// F26 function key
    F26,
    /// F27 function key
    F27,
    /// F28 function key
    F28,
    /// F29 function key
    F29,
    /// F30 function key
    F30,
    /// F31 function key
    F31,
    /// F32 function key
    F32,
    /// F33 function key
    F33,
    /// F34 function key
    F34,
    /// F35 function key
    F35,

    // Media and audio keys
    /// Media play key
    MediaPlay,
    /// Media pause key
    MediaPause,
    /// Media play/pause toggle key
    MediaPlayPause,
    /// Media reverse key
    MediaReverse,
    /// Media stop key
    MediaStop,
    /// Media fast-forward key
    MediaFastForward,
    /// Media rewind key
    MediaRewind,
    /// Media next-track key
    MediaTrackNext,
    /// Media previous-track key
    MediaTrackPrevious,
    /// Media record key
    MediaRecord,
    /// Audio volume down key
    AudioVolumeDown,
    /// Audio volume up key
    AudioVolumeUp,
    /// Audio mute key
    AudioVolumeMute,

    // Modifier keys reported as key events
    /// Left Shift key
    ShiftLeft,
    /// Right Shift key
    ShiftRight,
    /// Left Control key
    ControlLeft,
    /// Right Control key
    ControlRight,
    /// Left Alt/Option key
    AltLeft,
    /// Right Alt/Option key
    AltRight,
    /// Left Super/Command key
    SuperLeft,
    /// Right Super/Command key
    SuperRight,
    /// Left Hyper key
    HyperLeft,
    /// Right Hyper key
    HyperRight,
    /// Left Meta key
    MetaLeft,
    /// Right Meta key
    MetaRight,

    // Numpad keys (when numlock is off or in app keypad mode)
    /// Numpad 0
    Numpad0,
    /// Numpad 1
    Numpad1,
    /// Numpad 2
    Numpad2,
    /// Numpad 3
    Numpad3,
    /// Numpad 4
    Numpad4,
    /// Numpad 5
    Numpad5,
    /// Numpad 6
    Numpad6,
    /// Numpad 7
    Numpad7,
    /// Numpad 8
    Numpad8,
    /// Numpad 9
    Numpad9,
    /// Numpad decimal point
    NumpadDecimal,
    /// Numpad divide
    NumpadDivide,
    /// Numpad multiply
    NumpadMultiply,
    /// Numpad subtract
    NumpadSubtract,
    /// Numpad add
    NumpadAdd,
    /// Numpad enter
    NumpadEnter,
    /// Numpad equals
    NumpadEqual,
    /// Numpad separator (comma on some keyboards)
    NumpadSeparator,
    /// Numpad left arrow
    NumpadArrowLeft,
    /// Numpad right arrow
    NumpadArrowRight,
    /// Numpad up arrow
    NumpadArrowUp,
    /// Numpad down arrow
    NumpadArrowDown,
    /// Numpad Page Up
    NumpadPageUp,
    /// Numpad Page Down
    NumpadPageDown,
    /// Numpad Home
    NumpadHome,
    /// Numpad End
    NumpadEnd,
    /// Numpad Insert
    NumpadInsert,
    /// Numpad Delete
    NumpadDelete,
    /// Numpad Begin (center key, KP_BEGIN)
    NumpadBegin,
}

impl NamedKey {
    /// Get the Kitty keyboard protocol key code for this named key.
    ///
    /// Returns the Unicode code point used in CSI u encoding.
    // Trust L0: one exhaustive match instead of the former five-table
    // `or_else` combinator chain terminated by an `.expect(...)`. The tables
    // (control/function/numpad/media/modifier) were disjoint and together
    // covered every variant, so the expect never fired — but the verifier saw
    // four opaque closure envs plus a reachable panic edge it could not
    // discharge. Every variant maps to exactly the same code as before
    // (values verified against the kitty functional-key table), so this is
    // behavior-identical with no panic path left to prove.
    #[must_use]
    pub fn kitty_code(self) -> u32 {
        match self {
            // Control and navigation keys
            NamedKey::Escape => 27,
            NamedKey::Enter => 13,
            NamedKey::Tab => 9,
            NamedKey::Backspace => 127,
            NamedKey::Insert => 57348,
            NamedKey::Delete => 57349,
            NamedKey::ArrowLeft => 57350,
            NamedKey::ArrowRight => 57351,
            NamedKey::ArrowUp => 57352,
            NamedKey::ArrowDown => 57353,
            NamedKey::PageUp => 57354,
            NamedKey::PageDown => 57355,
            NamedKey::Home => 57356,
            NamedKey::End => 57357,
            NamedKey::CapsLock => 57358,
            NamedKey::ScrollLock => 57359,
            NamedKey::NumLock => 57360,
            NamedKey::PrintScreen => 57361,
            NamedKey::Pause => 57362,
            NamedKey::ContextMenu => 57363,
            NamedKey::Space => 32,
            // Function keys
            NamedKey::F1 => 57364,
            NamedKey::F2 => 57365,
            NamedKey::F3 => 57366,
            NamedKey::F4 => 57367,
            NamedKey::F5 => 57368,
            NamedKey::F6 => 57369,
            NamedKey::F7 => 57370,
            NamedKey::F8 => 57371,
            NamedKey::F9 => 57372,
            NamedKey::F10 => 57373,
            NamedKey::F11 => 57374,
            NamedKey::F12 => 57375,
            NamedKey::F13 => 57376,
            NamedKey::F14 => 57377,
            NamedKey::F15 => 57378,
            NamedKey::F16 => 57379,
            NamedKey::F17 => 57380,
            NamedKey::F18 => 57381,
            NamedKey::F19 => 57382,
            NamedKey::F20 => 57383,
            NamedKey::F21 => 57384,
            NamedKey::F22 => 57385,
            NamedKey::F23 => 57386,
            NamedKey::F24 => 57387,
            NamedKey::F25 => 57388,
            NamedKey::F26 => 57389,
            NamedKey::F27 => 57390,
            NamedKey::F28 => 57391,
            NamedKey::F29 => 57392,
            NamedKey::F30 => 57393,
            NamedKey::F31 => 57394,
            NamedKey::F32 => 57395,
            NamedKey::F33 => 57396,
            NamedKey::F34 => 57397,
            NamedKey::F35 => 57398,
            // Numpad keys
            NamedKey::Numpad0 => 57399,
            NamedKey::Numpad1 => 57400,
            NamedKey::Numpad2 => 57401,
            NamedKey::Numpad3 => 57402,
            NamedKey::Numpad4 => 57403,
            NamedKey::Numpad5 => 57404,
            NamedKey::Numpad6 => 57405,
            NamedKey::Numpad7 => 57406,
            NamedKey::Numpad8 => 57407,
            NamedKey::Numpad9 => 57408,
            NamedKey::NumpadDecimal => 57409,
            NamedKey::NumpadDivide => 57410,
            NamedKey::NumpadMultiply => 57411,
            NamedKey::NumpadSubtract => 57412,
            NamedKey::NumpadAdd => 57413,
            NamedKey::NumpadEnter => 57414,
            NamedKey::NumpadEqual => 57415,
            NamedKey::NumpadSeparator => 57416,
            NamedKey::NumpadArrowLeft => 57417,
            NamedKey::NumpadArrowRight => 57418,
            NamedKey::NumpadArrowUp => 57419,
            NamedKey::NumpadArrowDown => 57420,
            NamedKey::NumpadPageUp => 57421,
            NamedKey::NumpadPageDown => 57422,
            NamedKey::NumpadHome => 57423,
            NamedKey::NumpadEnd => 57424,
            NamedKey::NumpadInsert => 57425,
            NamedKey::NumpadDelete => 57426,
            NamedKey::NumpadBegin => 57427,
            // Media and audio keys
            NamedKey::MediaPlay => 57428,
            NamedKey::MediaPause => 57429,
            NamedKey::MediaPlayPause => 57430,
            NamedKey::MediaReverse => 57431,
            NamedKey::MediaStop => 57432,
            NamedKey::MediaFastForward => 57433,
            NamedKey::MediaRewind => 57434,
            NamedKey::MediaTrackNext => 57435,
            NamedKey::MediaTrackPrevious => 57436,
            NamedKey::MediaRecord => 57437,
            NamedKey::AudioVolumeDown => 57438,
            NamedKey::AudioVolumeUp => 57439,
            NamedKey::AudioVolumeMute => 57440,
            // Modifier keys
            NamedKey::ShiftLeft => 57441,
            NamedKey::ControlLeft => 57442,
            NamedKey::AltLeft => 57443,
            NamedKey::SuperLeft => 57444,
            NamedKey::HyperLeft => 57445,
            NamedKey::MetaLeft => 57446,
            NamedKey::ShiftRight => 57447,
            NamedKey::ControlRight => 57448,
            NamedKey::AltRight => 57449,
            NamedKey::SuperRight => 57450,
            NamedKey::HyperRight => 57451,
            NamedKey::MetaRight => 57452,
        }
    }
}

bitflags! {
    /// Keyboard modifier flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifiers: u8 {
        /// Shift modifier
        const SHIFT = 0b0000_0001;
        /// Alt/Option modifier
        const ALT = 0b0000_0010;
        /// Control modifier
        const CTRL = 0b0000_0100;
        /// Super/Cmd/Win modifier
        const SUPER = 0b0000_1000;
        /// Hyper modifier
        const HYPER = 0b0001_0000;
        /// Meta modifier
        const META = 0b0010_0000;
        /// Caps Lock modifier
        const CAPS_LOCK = 0b0100_0000;
        /// Num Lock modifier
        const NUM_LOCK = 0b1000_0000;
    }
}

impl Modifiers {
    /// Get the Kitty keyboard protocol modifier value.
    ///
    /// Kitty uses `modifiers + 1` format (1 = no modifiers).
    ///
    /// `saturating_add` guards the latent overflow: all eight modifier bits set
    /// (`bits() == 255`, reachable only if a future path ever sets CAPS_LOCK +
    /// NUM_LOCK together with the six chord bits) would otherwise wrap to 0 in
    /// release / panic in debug. Saturating to 255 is a clamped value, never a wrap.
    #[must_use]
    pub fn kitty_encoded(self) -> u8 {
        self.bits().saturating_add(1)
    }

    /// Get the legacy xterm modifier value for CSI sequences.
    ///
    /// Xterm encoding: `1 + shift(1) + alt(2) + ctrl(4) + meta(8)`.
    #[must_use]
    pub fn xterm_encoded(self) -> u8 {
        // Trust L0: saturating adds — exact here (the sum is at most
        // 1+1+2+4+8 = 16), but the native model checker falsely refutes the
        // plain `+=` overflow asserts; saturating has no panic path.
        let mut val = 1u8;
        if self.contains(Modifiers::SHIFT) {
            val = val.saturating_add(1);
        }
        if self.contains(Modifiers::ALT) {
            val = val.saturating_add(2);
        }
        if self.contains(Modifiers::CTRL) {
            val = val.saturating_add(4);
        }
        // Super/Meta is bit 3 in xterm modifier encoding (#7475).
        if self.contains(Modifiers::SUPER) {
            val = val.saturating_add(8);
        }
        val
    }
}

/// Key event type for Kitty keyboard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum KeyEventType {
    /// Key press event (default)
    #[default]
    Press,
    /// Key repeat event
    Repeat,
    /// Key release event
    Release,
}

impl KeyEventType {
    /// Get the Kitty protocol event type value.
    #[must_use]
    pub fn kitty_value(self) -> u8 {
        match self {
            KeyEventType::Press => 1,
            KeyEventType::Repeat => 2,
            KeyEventType::Release => 3,
        }
    }
}
