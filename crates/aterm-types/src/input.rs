// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! High-level input event types for editor and plugin systems.
//!
//! These types represent user input events (key presses with modifiers) at a
//! higher abstraction level than the terminal keyboard encoding types in
//! [`super::keyboard`]. The `keyboard` module handles terminal protocol
//! encoding (CSI u, xterm, legacy); this module provides the application-level
//! event model consumed by the editor and plugin systems.
//!
//! Use `input` for editor/plugin/application logic. Use `keyboard` for terminal
//! protocol encoding. Use the [`TryFrom`] bridge when crossing between the two:
//!
//! ```
//! use aterm_types::input::KeyCode;
//! use aterm_types::keyboard::Key;
//! let key: Key = KeyCode::Enter.try_into().expect("Enter is bridgeable");
//! let back: KeyCode = key.try_into().expect("Enter round-trips");
//! assert_eq!(back, KeyCode::Enter);
//! ```
//!
//! Extracted from `aterm-core::plugins::types` as part of the monolith split
//! (#2341) to allow `aterm-editor` and `aterm-core::plugins` to share these
//! types without a circular dependency.

/// Key event from terminal input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// The key code.
    pub key: KeyCode,
    /// Modifier keys held.
    pub modifiers: KeyModifiers,
}

/// Key codes for key events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyCode {
    /// Character key.
    Char(char),
    /// Function key (F1-F24).
    F(u8),
    /// Backspace key.
    Backspace,
    /// Enter/Return key.
    Enter,
    /// Tab key.
    Tab,
    /// Escape key.
    Escape,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Insert key.
    Insert,
    /// Delete key.
    Delete,
}

bitflags! {
    /// Modifier keys for key events.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KeyModifiers: u8 {
        /// Shift key.
        const SHIFT = 0b0000_0001;
        /// Alt/Option key.
        const ALT = 0b0000_0010;
        /// Control key.
        const CTRL = 0b0000_0100;
        /// Super/Command key.
        const SUPER = 0b0000_1000;
    }
}

/// Lossless conversion from `input::KeyModifiers` to `keyboard::Modifiers`.
///
/// Both types share identical bit layout (Kitty protocol ordering:
/// Shift=1, Alt=2, Ctrl=4, Super=8), making this a zero-cost conversion.
impl From<KeyModifiers> for crate::keyboard::Modifiers {
    fn from(m: KeyModifiers) -> Self {
        // SAFETY invariant: bit layouts are identical, enforced by
        // `test_bit_layout_matches_keyboard_modifiers` below.
        crate::keyboard::Modifiers::from_bits_truncate(m.bits())
    }
}

/// Lossless conversion from `keyboard::Modifiers` to `input::KeyModifiers`.
impl From<crate::keyboard::Modifiers> for KeyModifiers {
    fn from(m: crate::keyboard::Modifiers) -> Self {
        KeyModifiers::from_bits_truncate(m.bits())
    }
}

/// Error when converting between `input::KeyCode` and `keyboard::Key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, aterm_error::Error)]
#[non_exhaustive]
pub enum KeyConversionError {
    /// Function key number is outside the supported protocol range F1-F24.
    #[error("function key F{0} is outside the supported protocol range F1-F24")]
    FunctionKeyOutOfRange(u8),
    /// Protocol key has no application-level representation.
    #[error("protocol key {0:?} has no application-level representation")]
    ProtocolOnlyKey(crate::keyboard::NamedKey),
}

/// Bridge from application-level `KeyCode` to protocol-level `keyboard::Key`.
///
/// All `KeyCode` variants map except `F(0)` and `F(n > 24)`.
/// `KeyCode::Char(' ')` stays `Key::Character(' ')` (not `NamedKey::Space`).
impl TryFrom<KeyCode> for crate::keyboard::Key {
    type Error = KeyConversionError;

    fn try_from(kc: KeyCode) -> Result<Self, Self::Error> {
        use crate::keyboard::{Key, NamedKey};
        Ok(match kc {
            KeyCode::Char(c) => Key::Character(c),
            KeyCode::Backspace => Key::Named(NamedKey::Backspace),
            KeyCode::Enter => Key::Named(NamedKey::Enter),
            KeyCode::Tab => Key::Named(NamedKey::Tab),
            KeyCode::Escape => Key::Named(NamedKey::Escape),
            KeyCode::Up => Key::Named(NamedKey::ArrowUp),
            KeyCode::Down => Key::Named(NamedKey::ArrowDown),
            KeyCode::Left => Key::Named(NamedKey::ArrowLeft),
            KeyCode::Right => Key::Named(NamedKey::ArrowRight),
            KeyCode::Home => Key::Named(NamedKey::Home),
            KeyCode::End => Key::Named(NamedKey::End),
            KeyCode::PageUp => Key::Named(NamedKey::PageUp),
            KeyCode::PageDown => Key::Named(NamedKey::PageDown),
            KeyCode::Insert => Key::Named(NamedKey::Insert),
            KeyCode::Delete => Key::Named(NamedKey::Delete),
            KeyCode::F(n @ 1..=24) => {
                // Map F1..F24 to the corresponding NamedKey variant.
                const FKEYS: [NamedKey; 24] = [
                    NamedKey::F1,
                    NamedKey::F2,
                    NamedKey::F3,
                    NamedKey::F4,
                    NamedKey::F5,
                    NamedKey::F6,
                    NamedKey::F7,
                    NamedKey::F8,
                    NamedKey::F9,
                    NamedKey::F10,
                    NamedKey::F11,
                    NamedKey::F12,
                    NamedKey::F13,
                    NamedKey::F14,
                    NamedKey::F15,
                    NamedKey::F16,
                    NamedKey::F17,
                    NamedKey::F18,
                    NamedKey::F19,
                    NamedKey::F20,
                    NamedKey::F21,
                    NamedKey::F22,
                    NamedKey::F23,
                    NamedKey::F24,
                ];
                // The `1..=24` pattern bounds `n - 1` to 0..=23, always a valid
                // index into the 24-entry table; the total `get` proves that to
                // the Trust gate. The `None` arm is unreachable.
                match FKEYS.get(usize::from(n - 1)) {
                    Some(&named) => Key::Named(named),
                    None => return Err(KeyConversionError::FunctionKeyOutOfRange(n)),
                }
            }
            KeyCode::F(n) => return Err(KeyConversionError::FunctionKeyOutOfRange(n)),
        })
    }
}

/// Bridge from protocol-level `keyboard::Key` to application-level `KeyCode`.
///
/// `Key::Named(NamedKey::Space)` maps to `KeyCode::Char(' ')`.
/// Numpad keys return `KeyConversionError::ProtocolOnlyKey`.
impl TryFrom<crate::keyboard::Key> for KeyCode {
    type Error = KeyConversionError;

    fn try_from(key: crate::keyboard::Key) -> Result<Self, Self::Error> {
        use crate::keyboard::{Key, NamedKey};
        match key {
            Key::Character(c) => Ok(KeyCode::Char(c)),
            Key::Named(named) => match named {
                NamedKey::Backspace => Ok(KeyCode::Backspace),
                NamedKey::Enter => Ok(KeyCode::Enter),
                NamedKey::Tab => Ok(KeyCode::Tab),
                NamedKey::Escape => Ok(KeyCode::Escape),
                NamedKey::Space => Ok(KeyCode::Char(' ')),
                NamedKey::ArrowUp => Ok(KeyCode::Up),
                NamedKey::ArrowDown => Ok(KeyCode::Down),
                NamedKey::ArrowLeft => Ok(KeyCode::Left),
                NamedKey::ArrowRight => Ok(KeyCode::Right),
                NamedKey::Home => Ok(KeyCode::Home),
                NamedKey::End => Ok(KeyCode::End),
                NamedKey::PageUp => Ok(KeyCode::PageUp),
                NamedKey::PageDown => Ok(KeyCode::PageDown),
                NamedKey::Insert => Ok(KeyCode::Insert),
                NamedKey::Delete => Ok(KeyCode::Delete),
                NamedKey::F1 => Ok(KeyCode::F(1)),
                NamedKey::F2 => Ok(KeyCode::F(2)),
                NamedKey::F3 => Ok(KeyCode::F(3)),
                NamedKey::F4 => Ok(KeyCode::F(4)),
                NamedKey::F5 => Ok(KeyCode::F(5)),
                NamedKey::F6 => Ok(KeyCode::F(6)),
                NamedKey::F7 => Ok(KeyCode::F(7)),
                NamedKey::F8 => Ok(KeyCode::F(8)),
                NamedKey::F9 => Ok(KeyCode::F(9)),
                NamedKey::F10 => Ok(KeyCode::F(10)),
                NamedKey::F11 => Ok(KeyCode::F(11)),
                NamedKey::F12 => Ok(KeyCode::F(12)),
                NamedKey::F13 => Ok(KeyCode::F(13)),
                NamedKey::F14 => Ok(KeyCode::F(14)),
                NamedKey::F15 => Ok(KeyCode::F(15)),
                NamedKey::F16 => Ok(KeyCode::F(16)),
                NamedKey::F17 => Ok(KeyCode::F(17)),
                NamedKey::F18 => Ok(KeyCode::F(18)),
                NamedKey::F19 => Ok(KeyCode::F(19)),
                NamedKey::F20 => Ok(KeyCode::F(20)),
                NamedKey::F21 => Ok(KeyCode::F(21)),
                NamedKey::F22 => Ok(KeyCode::F(22)),
                NamedKey::F23 => Ok(KeyCode::F(23)),
                NamedKey::F24 => Ok(KeyCode::F(24)),
                other => Err(KeyConversionError::ProtocolOnlyKey(other)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_eq() {
        let a = KeyEvent {
            key: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
        };
        let b = KeyEvent {
            key: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn key_event_with_modifiers() {
        let event = KeyEvent {
            key: KeyCode::Char('s'),
            modifiers: KeyModifiers::CTRL,
        };
        assert_eq!(event.key, KeyCode::Char('s'));
        assert!(event.modifiers.contains(KeyModifiers::CTRL));
    }

    #[test]
    fn modifier_combinations() {
        let mods = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        assert!(mods.contains(KeyModifiers::CTRL));
        assert!(mods.contains(KeyModifiers::SHIFT));
        assert!(!mods.contains(KeyModifiers::ALT));
    }

    #[test]
    fn key_code_variants() {
        assert_eq!(KeyCode::F(1), KeyCode::F(1));
        assert_ne!(KeyCode::F(1), KeyCode::F(2));
        assert_ne!(KeyCode::Enter, KeyCode::Tab);
    }

    #[test]
    fn bit_layout_matches_keyboard_modifiers() {
        use crate::keyboard::Modifiers;

        assert_eq!(KeyModifiers::SHIFT.bits(), Modifiers::SHIFT.bits());
        assert_eq!(KeyModifiers::ALT.bits(), Modifiers::ALT.bits());
        assert_eq!(KeyModifiers::CTRL.bits(), Modifiers::CTRL.bits());
        assert_eq!(KeyModifiers::SUPER.bits(), Modifiers::SUPER.bits());
    }

    #[test]
    fn from_conversion_roundtrip() {
        use crate::keyboard::Modifiers;

        let input_mods = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        let kb_mods: Modifiers = input_mods.into();
        let back: KeyModifiers = kb_mods.into();
        assert_eq!(input_mods, back);
    }

    // ========================================================================
    // Key bridge: TryFrom roundtrip tests (#5681)
    // ========================================================================

    #[test]
    fn key_bridge_char_roundtrip() {
        use crate::keyboard::Key;
        let key: Key = KeyCode::Char('a').try_into().unwrap();
        assert_eq!(key, Key::Character('a'));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::Char('a'));
    }

    #[test]
    fn key_bridge_space_roundtrip() {
        use crate::keyboard::Key;
        // KeyCode::Char(' ') -> Key::Character(' '), NOT NamedKey::Space
        let key: Key = KeyCode::Char(' ').try_into().unwrap();
        assert_eq!(key, Key::Character(' '));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::Char(' '));
    }

    #[test]
    fn key_bridge_named_space_to_char() {
        use crate::keyboard::{Key, NamedKey};
        // NamedKey::Space -> KeyCode::Char(' ')
        let back: KeyCode = Key::Named(NamedKey::Space).try_into().unwrap();
        assert_eq!(back, KeyCode::Char(' '));
    }

    #[test]
    fn key_bridge_enter_roundtrip() {
        use crate::keyboard::{Key, NamedKey};
        let key: Key = KeyCode::Enter.try_into().unwrap();
        assert_eq!(key, Key::Named(NamedKey::Enter));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::Enter);
    }

    #[test]
    fn key_bridge_arrow_up_roundtrip() {
        use crate::keyboard::{Key, NamedKey};
        let key: Key = KeyCode::Up.try_into().unwrap();
        assert_eq!(key, Key::Named(NamedKey::ArrowUp));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::Up);
    }

    #[test]
    fn key_bridge_delete_roundtrip() {
        use crate::keyboard::{Key, NamedKey};
        let key: Key = KeyCode::Delete.try_into().unwrap();
        assert_eq!(key, Key::Named(NamedKey::Delete));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::Delete);
    }

    #[test]
    fn key_bridge_f1_roundtrip() {
        use crate::keyboard::{Key, NamedKey};
        let key: Key = KeyCode::F(1).try_into().unwrap();
        assert_eq!(key, Key::Named(NamedKey::F1));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::F(1));
    }

    #[test]
    fn key_bridge_f24_roundtrip() {
        use crate::keyboard::{Key, NamedKey};
        let key: Key = KeyCode::F(24).try_into().unwrap();
        assert_eq!(key, Key::Named(NamedKey::F24));
        let back: KeyCode = key.try_into().unwrap();
        assert_eq!(back, KeyCode::F(24));
    }

    #[test]
    fn key_bridge_f0_out_of_range() {
        use crate::keyboard::Key;
        let result: Result<Key, _> = KeyCode::F(0).try_into();
        assert_eq!(result, Err(KeyConversionError::FunctionKeyOutOfRange(0)));
    }

    #[test]
    fn key_bridge_f25_out_of_range() {
        use crate::keyboard::Key;
        let result: Result<Key, _> = KeyCode::F(25).try_into();
        assert_eq!(result, Err(KeyConversionError::FunctionKeyOutOfRange(25)));
    }

    #[test]
    fn key_bridge_numpad5_protocol_only() {
        use crate::keyboard::{Key, NamedKey};
        let result: Result<KeyCode, _> = Key::Named(NamedKey::Numpad5).try_into();
        assert_eq!(
            result,
            Err(KeyConversionError::ProtocolOnlyKey(NamedKey::Numpad5))
        );
    }

    #[test]
    fn key_bridge_numpad_enter_protocol_only() {
        use crate::keyboard::{Key, NamedKey};
        let result: Result<KeyCode, _> = Key::Named(NamedKey::NumpadEnter).try_into();
        assert_eq!(
            result,
            Err(KeyConversionError::ProtocolOnlyKey(NamedKey::NumpadEnter))
        );
    }
}
