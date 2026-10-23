// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Bridge from winit's `Key`/`NamedKey`/`PhysicalKey` to the engine's [`Key`]/[`NamedKey`].

use aterm_window::keyboard::{
    Key as WinitKey, KeyCode, KeyLocation, NamedKey as WinitNamed, PhysicalKey,
};

use aterm_types::keyboard::{Key, NamedKey};

/// Map a winit logical [`WinitKey`] (e.g. `ev.logical_key` or `key_without_modifiers()`).
#[inline]
#[must_use]
pub fn map_logical_key(key: &WinitKey) -> Option<Key> {
    match key {
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Not a single-codepoint key press (IME / composed text).
                return None;
            }
            Some(Key::Character(c))
        }
        WinitKey::Named(named) => map_named_key(*named).map(Key::Named),
        WinitKey::Dead(_) | WinitKey::Unidentified(_) => None,
    }
}

/// Map a winit [`WinitNamed`] into the engine's [`NamedKey`].
#[inline]
#[must_use]
pub(crate) const fn map_named_key(named: WinitNamed) -> Option<NamedKey> {
    Some(match named {
        WinitNamed::ArrowUp => NamedKey::ArrowUp,
        WinitNamed::ArrowDown => NamedKey::ArrowDown,
        WinitNamed::ArrowLeft => NamedKey::ArrowLeft,
        WinitNamed::ArrowRight => NamedKey::ArrowRight,
        WinitNamed::Home => NamedKey::Home,
        WinitNamed::End => NamedKey::End,
        WinitNamed::PageUp => NamedKey::PageUp,
        WinitNamed::PageDown => NamedKey::PageDown,
        WinitNamed::Backspace => NamedKey::Backspace,
        WinitNamed::Delete => NamedKey::Delete,
        WinitNamed::Insert => NamedKey::Insert,
        WinitNamed::Enter => NamedKey::Enter,
        WinitNamed::Tab => NamedKey::Tab,
        WinitNamed::Escape => NamedKey::Escape,
        WinitNamed::Space => NamedKey::Space,
        WinitNamed::CapsLock => NamedKey::CapsLock,
        WinitNamed::NumLock => NamedKey::NumLock,
        WinitNamed::ScrollLock => NamedKey::ScrollLock,
        WinitNamed::PrintScreen => NamedKey::PrintScreen,
        WinitNamed::Pause => NamedKey::Pause,
        WinitNamed::ContextMenu => NamedKey::ContextMenu,
        WinitNamed::F1 => NamedKey::F1,
        WinitNamed::F2 => NamedKey::F2,
        WinitNamed::F3 => NamedKey::F3,
        WinitNamed::F4 => NamedKey::F4,
        WinitNamed::F5 => NamedKey::F5,
        WinitNamed::F6 => NamedKey::F6,
        WinitNamed::F7 => NamedKey::F7,
        WinitNamed::F8 => NamedKey::F8,
        WinitNamed::F9 => NamedKey::F9,
        WinitNamed::F10 => NamedKey::F10,
        WinitNamed::F11 => NamedKey::F11,
        WinitNamed::F12 => NamedKey::F12,
        WinitNamed::F13 => NamedKey::F13,
        WinitNamed::F14 => NamedKey::F14,
        WinitNamed::F15 => NamedKey::F15,
        WinitNamed::F16 => NamedKey::F16,
        WinitNamed::F17 => NamedKey::F17,
        WinitNamed::F18 => NamedKey::F18,
        WinitNamed::F19 => NamedKey::F19,
        WinitNamed::F20 => NamedKey::F20,
        WinitNamed::F21 => NamedKey::F21,
        WinitNamed::F22 => NamedKey::F22,
        WinitNamed::F23 => NamedKey::F23,
        WinitNamed::F24 => NamedKey::F24,
        WinitNamed::F25 => NamedKey::F25,
        WinitNamed::F26 => NamedKey::F26,
        WinitNamed::F27 => NamedKey::F27,
        WinitNamed::F28 => NamedKey::F28,
        WinitNamed::F29 => NamedKey::F29,
        WinitNamed::F30 => NamedKey::F30,
        WinitNamed::F31 => NamedKey::F31,
        WinitNamed::F32 => NamedKey::F32,
        WinitNamed::F33 => NamedKey::F33,
        WinitNamed::F34 => NamedKey::F34,
        WinitNamed::F35 => NamedKey::F35,
        WinitNamed::MediaPlay => NamedKey::MediaPlay,
        WinitNamed::MediaPause => NamedKey::MediaPause,
        WinitNamed::MediaPlayPause => NamedKey::MediaPlayPause,
        WinitNamed::MediaStop => NamedKey::MediaStop,
        WinitNamed::MediaFastForward => NamedKey::MediaFastForward,
        WinitNamed::MediaRewind => NamedKey::MediaRewind,
        WinitNamed::MediaTrackNext => NamedKey::MediaTrackNext,
        WinitNamed::MediaTrackPrevious => NamedKey::MediaTrackPrevious,
        WinitNamed::MediaRecord => NamedKey::MediaRecord,
        WinitNamed::AudioVolumeDown => NamedKey::AudioVolumeDown,
        WinitNamed::AudioVolumeUp => NamedKey::AudioVolumeUp,
        WinitNamed::AudioVolumeMute => NamedKey::AudioVolumeMute,
        WinitNamed::Shift => NamedKey::ShiftLeft,
        WinitNamed::Control => NamedKey::ControlLeft,
        WinitNamed::Alt => NamedKey::AltLeft,
        WinitNamed::Super => NamedKey::SuperLeft,
        WinitNamed::Hyper => NamedKey::HyperLeft,
        WinitNamed::Meta => NamedKey::MetaLeft,
        _ => return None,
    })
}

/// A modifier key on the side it was pressed.
#[inline]
#[must_use]
pub fn sided_modifier(key: Key, location: KeyLocation) -> Key {
    if location != KeyLocation::Right {
        return key;
    }
    let right = match key {
        Key::Named(NamedKey::ShiftLeft) => NamedKey::ShiftRight,
        Key::Named(NamedKey::ControlLeft) => NamedKey::ControlRight,
        Key::Named(NamedKey::AltLeft) => NamedKey::AltRight,
        Key::Named(NamedKey::SuperLeft) => NamedKey::SuperRight,
        Key::Named(NamedKey::HyperLeft) => NamedKey::HyperRight,
        Key::Named(NamedKey::MetaLeft) => NamedKey::MetaRight,
        other => return other,
    };
    Key::Named(right)
}

/// The physical key on the OTHER side of a sided modifier, or `None` for any other key.
#[inline]
#[must_use]
pub const fn physical_modifier_twin(physical: PhysicalKey) -> Option<PhysicalKey> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(PhysicalKey::Code(match code {
        KeyCode::ShiftLeft => KeyCode::ShiftRight,
        KeyCode::ShiftRight => KeyCode::ShiftLeft,
        KeyCode::ControlLeft => KeyCode::ControlRight,
        KeyCode::ControlRight => KeyCode::ControlLeft,
        KeyCode::AltLeft => KeyCode::AltRight,
        KeyCode::AltRight => KeyCode::AltLeft,
        KeyCode::SuperLeft => KeyCode::SuperRight,
        KeyCode::SuperRight => KeyCode::SuperLeft,
        _ => return None,
    }))
}

/// Map a keypad press (`KeyLocation::Numpad`) to the engine's `Numpad*` [`Key`].
#[must_use]
pub fn map_numpad_key(
    physical: PhysicalKey,
    logical: &WinitKey,
    location: KeyLocation,
) -> Option<Key> {
    if location != KeyLocation::Numpad {
        return None;
    }
    let named = match logical {
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Not a single-codepoint key press (IME / composed text).
                return None;
            }
            match c {
                '0' => NamedKey::Numpad0,
                '1' => NamedKey::Numpad1,
                '2' => NamedKey::Numpad2,
                '3' => NamedKey::Numpad3,
                '4' => NamedKey::Numpad4,
                '5' => NamedKey::Numpad5,
                '6' => NamedKey::Numpad6,
                '7' => NamedKey::Numpad7,
                '8' => NamedKey::Numpad8,
                '9' => NamedKey::Numpad9,
                '.' => NamedKey::NumpadDecimal,
                ',' => NamedKey::NumpadSeparator,
                '/' => NamedKey::NumpadDivide,
                '*' => NamedKey::NumpadMultiply,
                '-' => NamedKey::NumpadSubtract,
                '+' => NamedKey::NumpadAdd,
                '=' => NamedKey::NumpadEqual,
                _ => return None,
            }
        }
        WinitKey::Named(WinitNamed::Enter) => NamedKey::NumpadEnter,
        WinitKey::Named(WinitNamed::ArrowUp) => NamedKey::NumpadArrowUp,
        WinitKey::Named(WinitNamed::ArrowDown) => NamedKey::NumpadArrowDown,
        WinitKey::Named(WinitNamed::ArrowLeft) => NamedKey::NumpadArrowLeft,
        WinitKey::Named(WinitNamed::ArrowRight) => NamedKey::NumpadArrowRight,
        WinitKey::Named(WinitNamed::Home) => NamedKey::NumpadHome,
        WinitKey::Named(WinitNamed::End) => NamedKey::NumpadEnd,
        WinitKey::Named(WinitNamed::PageUp) => NamedKey::NumpadPageUp,
        WinitKey::Named(WinitNamed::PageDown) => NamedKey::NumpadPageDown,
        WinitKey::Named(WinitNamed::Insert) => NamedKey::NumpadInsert,
        WinitKey::Named(WinitNamed::Delete) => NamedKey::NumpadDelete,
        _ if physical == PhysicalKey::Code(KeyCode::Numpad5)
            && map_logical_key(logical).is_none() =>
        {
            NamedKey::NumpadBegin
        }
        _ => return None,
    };
    Some(Key::Named(named))
}

/// Unshifted US-QWERTY character for `physical`.
#[inline]
#[must_use]
pub const fn base_layout_key_for(physical: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Backquote => '`',
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        _ => return None,
    })
}

/// The US-QWERTY letter a chord on a non-Latin layout names.
#[inline]
#[must_use]
pub fn layout_letter_twin_for(logical: &WinitKey, physical: PhysicalKey) -> Option<char> {
    let WinitKey::Character(s) = logical else {
        return None;
    };
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    aterm_types::keyboard::layout_letter_twin(c, base_layout_key_for(physical))
}

/// Return the shifted US-QWERTY ASCII character for an unshifted US-QWERTY character.
#[inline]
#[must_use]
pub const fn us_qwerty_shifted_ascii(base: char) -> Option<char> {
    if base >= 'a' && base <= 'z' {
        return Some((base as u8 - 32) as char);
    }
    Some(match base {
        '0' => ')',
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '`' => '~',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => return None,
    })
}

#[inline]
const fn is_unicode_scalar(v: u32) -> bool {
    v <= 0xD7FF || (v >= 0xE000 && v <= 0x10_FFFF)
}

/// Encode a Kitty key event descriptor `(unicode_key, shifted_key, base_layout_key,
/// modifiers_mask, event_type)` into a 14-byte little-endian wire frame.
#[inline]
#[must_use]
pub const fn encode_kitty_key_wire_le(
    unicode_key: u32,
    shifted_key: u32,
    base_layout_key: u32,
    modifiers_mask: u8,
    event_type: u8,
) -> Option<[u8; 14]> {
    if !is_unicode_scalar(unicode_key)
        || !is_unicode_scalar(shifted_key)
        || !is_unicode_scalar(base_layout_key)
        || event_type == 0
        || event_type > 3
    {
        return None;
    }
    Some([
        unicode_key as u8,
        (unicode_key >> 8) as u8,
        (unicode_key >> 16) as u8,
        (unicode_key >> 24) as u8,
        shifted_key as u8,
        (shifted_key >> 8) as u8,
        (shifted_key >> 16) as u8,
        (shifted_key >> 24) as u8,
        base_layout_key as u8,
        (base_layout_key >> 8) as u8,
        (base_layout_key >> 16) as u8,
        (base_layout_key >> 24) as u8,
        modifiers_mask,
        event_type,
    ])
}

/// Decode a 14-byte little-endian Kitty key event wire frame produced by
/// [`encode_kitty_key_wire_le`].
#[inline]
#[must_use]
pub const fn decode_kitty_key_wire_le(wire: &[u8; 14]) -> Option<(char, char, char, u8, u8)> {
    let u_raw = (wire[0] as u32)
        | ((wire[1] as u32) << 8)
        | ((wire[2] as u32) << 16)
        | ((wire[3] as u32) << 24);
    let s_raw = (wire[4] as u32)
        | ((wire[5] as u32) << 8)
        | ((wire[6] as u32) << 16)
        | ((wire[7] as u32) << 24);
    let b_raw = (wire[8] as u32)
        | ((wire[9] as u32) << 8)
        | ((wire[10] as u32) << 16)
        | ((wire[11] as u32) << 24);
    let modifiers_mask = wire[12];
    let event_type = wire[13];
    if event_type == 0 || event_type > 3 {
        return None;
    }
    let Some(u) = char::from_u32(u_raw) else {
        return None;
    };
    let Some(s) = char::from_u32(s_raw) else {
        return None;
    };
    let Some(b) = char::from_u32(b_raw) else {
        return None;
    };
    Some((u, s, b, modifiers_mask, event_type))
}

/// Encode a Kitty key event descriptor `(unicode_key, shifted_key, base_layout_key,
/// modifiers_mask, event_type)` into a 14-byte big-endian wire frame.
#[inline]
#[must_use]
pub const fn encode_kitty_key_wire_be(
    unicode_key: u32,
    shifted_key: u32,
    base_layout_key: u32,
    modifiers_mask: u8,
    event_type: u8,
) -> Option<[u8; 14]> {
    if !is_unicode_scalar(unicode_key)
        || !is_unicode_scalar(shifted_key)
        || !is_unicode_scalar(base_layout_key)
        || event_type == 0
        || event_type > 3
    {
        return None;
    }
    Some([
        (unicode_key >> 24) as u8,
        (unicode_key >> 16) as u8,
        (unicode_key >> 8) as u8,
        unicode_key as u8,
        (shifted_key >> 24) as u8,
        (shifted_key >> 16) as u8,
        (shifted_key >> 8) as u8,
        shifted_key as u8,
        (base_layout_key >> 24) as u8,
        (base_layout_key >> 16) as u8,
        (base_layout_key >> 8) as u8,
        base_layout_key as u8,
        modifiers_mask,
        event_type,
    ])
}

/// Decode a 14-byte big-endian Kitty key event wire frame produced by [`encode_kitty_key_wire_be`].
#[inline]
#[must_use]
pub const fn decode_kitty_key_wire_be(wire: &[u8; 14]) -> Option<(char, char, char, u8, u8)> {
    let u_raw = ((wire[0] as u32) << 24)
        | ((wire[1] as u32) << 16)
        | ((wire[2] as u32) << 8)
        | (wire[3] as u32);
    let s_raw = ((wire[4] as u32) << 24)
        | ((wire[5] as u32) << 16)
        | ((wire[6] as u32) << 8)
        | (wire[7] as u32);
    let b_raw = ((wire[8] as u32) << 24)
        | ((wire[9] as u32) << 16)
        | ((wire[10] as u32) << 8)
        | (wire[11] as u32);
    let modifiers_mask = wire[12];
    let event_type = wire[13];
    if event_type == 0 || event_type > 3 {
        return None;
    }
    let Some(u) = char::from_u32(u_raw) else {
        return None;
    };
    let Some(s) = char::from_u32(s_raw) else {
        return None;
    };
    let Some(b) = char::from_u32(b_raw) else {
        return None;
    };
    Some((u, s, b, modifiers_mask, event_type))
}

/// Convert a 6-bit Kitty modifier mask into its 1-based CSI parameter (`1 +`).
#[inline]
#[must_use]
pub const fn kitty_csi_modifier_param(modifiers_mask: u8) -> u8 {
    1 + (modifiers_mask & 0x3f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_window::keyboard::SmolStr;

    /// Verify F13–F35 and media/audio keys map to their engine `NamedKey` variants.
    #[test]
    fn maps_function_keys_past_f12_and_media_keys() {
        for (winit, want) in [
            (WinitNamed::F13, NamedKey::F13),
            (WinitNamed::F24, NamedKey::F24),
            (WinitNamed::F35, NamedKey::F35),
            (WinitNamed::MediaPlayPause, NamedKey::MediaPlayPause),
            (WinitNamed::AudioVolumeMute, NamedKey::AudioVolumeMute),
        ] {
            assert_eq!(map_named_key(winit), Some(want), "{winit:?}");
        }
    }

    fn ch(c: &str) -> WinitKey {
        WinitKey::Character(SmolStr::new(c))
    }

    fn unidentified() -> WinitKey {
        WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified)
    }

    /// A digit at the keypad location is the keypad digit — the engine key
    /// that reaches DECKPAM's SS3 and kitty's KP codes — not the main row's
    /// `Character('5')` that the logical key alone would give.
    #[test]
    fn numpad_location_promotes_digit_to_numpad_key() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &ch("5"),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::Numpad5))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadAdd),
                &ch("+"),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadAdd))
        );
    }

    /// KP_Enter arrives from every platform as the main block's `Enter`; on
    /// the keypad it is NumpadEnter (CR / SS3 M / `CSI 57414 u`).
    #[test]
    fn numpad_enter_is_not_main_enter() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadEnter),
                &WinitKey::Named(WinitNamed::Enter),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadEnter))
        );
    }

    /// NumLock off: the navigation keysym is the keypad twin of the main-block
    /// key, and the centre key — which winit names on no platform — is
    /// NumpadBegin by its physical code, whether xkb reports `Unidentified` or
    /// Windows reports `Clear`.
    #[test]
    fn numlock_off_navigation_gets_numpad_twin() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad4),
                &WinitKey::Named(WinitNamed::ArrowLeft),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadArrowLeft))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad1),
                &WinitKey::Named(WinitNamed::End),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadEnd))
        );
        for centre in [unidentified(), WinitKey::Named(WinitNamed::Clear)] {
            assert_eq!(
                map_numpad_key(
                    PhysicalKey::Code(KeyCode::Numpad5),
                    &centre,
                    KeyLocation::Numpad
                ),
                Some(Key::Named(NamedKey::NumpadBegin)),
                "{centre:?} on the physical keypad 5 is KP_Begin"
            );
        }
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad4),
                &unidentified(),
                KeyLocation::Numpad
            ),
            None,
            "an unnamed key elsewhere on the keypad stays unmapped"
        );
    }

    /// The identity is the glyph the LAYOUT typed, not the scancode: the same
    /// physical decimal key is KP_Decimal on a US layout and KP_Separator on a
    /// de-DE `keypad(comma)` layout, and must type `,` there — never `.`.
    #[test]
    fn keypad_identity_is_the_glyph_not_the_scancode() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadDecimal),
                &ch("."),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadDecimal))
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::NumpadDecimal),
                &ch(","),
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadSeparator))
        );
    }

    /// A keypad key with no keypad identity — KP_Tab, or a keypad key the user
    /// remapped to a letter — is left to the logical route.
    #[test]
    fn keypad_key_without_keypad_identity_takes_the_logical_route() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Unidentified(aterm_window::keyboard::NativeKeyCode::Unidentified),
                &WinitKey::Named(WinitNamed::Tab),
                KeyLocation::Numpad
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad1),
                &ch("a"),
                KeyLocation::Numpad
            ),
            None
        );
    }

    /// The negative control: the main block is untouched. `5` above `R` and
    /// the main Return are not keypad keys, and the LOCATION — not the
    /// scancode — is what says a key is on the keypad.
    #[test]
    fn standard_location_is_untouched() {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Digit5),
                &ch("5"),
                KeyLocation::Standard
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Enter),
                &WinitKey::Named(WinitNamed::Enter),
                KeyLocation::Standard
            ),
            None
        );
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &ch("5"),
                KeyLocation::Standard
            ),
            None
        );
    }

    #[test]
    fn character_logical_key_maps_through() {
        assert_eq!(map_logical_key(&ch("a")), Some(Key::Character('a')));
        assert_eq!(map_logical_key(&ch("ab")), None);
    }

    #[test]
    fn base_layout_key_is_us_qwerty() {
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::KeyA)),
            Some('a')
        );
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::Digit1)),
            Some('1')
        );
        assert_eq!(
            base_layout_key_for(PhysicalKey::Code(KeyCode::Slash)),
            Some('/')
        );
        assert_eq!(base_layout_key_for(PhysicalKey::Code(KeyCode::F1)), None);
    }

    #[test]
    fn layout_letter_twin_names_the_us_letter_under_a_non_latin_letter() {
        let ch = |s: &str| WinitKey::Character(SmolStr::new(s));
        let key = |code| PhysicalKey::Code(code);
        assert_eq!(
            layout_letter_twin_for(&ch("с"), key(KeyCode::KeyC)),
            Some('c')
        );
        assert_eq!(
            layout_letter_twin_for(&ch("ψ"), key(KeyCode::KeyC)),
            Some('c')
        );
        assert_eq!(
            layout_letter_twin_for(&ch("ב"), key(KeyCode::KeyC)),
            Some('c')
        );
        assert_eq!(layout_letter_twin_for(&ch("a"), key(KeyCode::KeyQ)), None);
        assert_eq!(layout_letter_twin_for(&ch("é"), key(KeyCode::Digit2)), None);
        assert_eq!(layout_letter_twin_for(&ch("б"), key(KeyCode::Comma)), None);
        assert_eq!(layout_letter_twin_for(&ch("№"), key(KeyCode::Digit3)), None);
        assert_eq!(
            layout_letter_twin_for(&WinitKey::Named(WinitNamed::Enter), key(KeyCode::Enter)),
            None
        );
        assert_eq!(
            layout_letter_twin_for(
                &ch("с"),
                PhysicalKey::Unidentified(aterm_window::keyboard::NativeKeyCode::Unidentified)
            ),
            None
        );
    }

    /// A right-side modifier press is the engine's *Right key (kitty's
    /// `RIGHT_SHIFT` 57447, not `LEFT_SHIFT` 57441); a left or standard one,
    /// and every non-modifier key, is unchanged.
    #[test]
    fn a_right_side_modifier_press_maps_to_its_right_variant() {
        for (winit, left, right) in [
            (WinitNamed::Shift, NamedKey::ShiftLeft, NamedKey::ShiftRight),
            (
                WinitNamed::Control,
                NamedKey::ControlLeft,
                NamedKey::ControlRight,
            ),
            (WinitNamed::Alt, NamedKey::AltLeft, NamedKey::AltRight),
            (WinitNamed::Super, NamedKey::SuperLeft, NamedKey::SuperRight),
            (WinitNamed::Hyper, NamedKey::HyperLeft, NamedKey::HyperRight),
            (WinitNamed::Meta, NamedKey::MetaLeft, NamedKey::MetaRight),
        ] {
            let key = map_logical_key(&WinitKey::Named(winit)).expect("a modifier maps");
            assert_eq!(key, Key::Named(left));
            assert_eq!(
                sided_modifier(key.clone(), KeyLocation::Right),
                Key::Named(right)
            );
            for loc in [KeyLocation::Left, KeyLocation::Standard] {
                assert_eq!(sided_modifier(key.clone(), loc), Key::Named(left));
            }
        }
        let a = Key::Character('a');
        assert_eq!(sided_modifier(a.clone(), KeyLocation::Right), a);
    }

    /// Each sided modifier's physical twin is the other side of the SAME
    /// modifier (the pairing a release's kitty bit is decided by), the map is
    /// its own inverse, and no other key has one.
    #[test]
    fn a_sided_modifier_has_its_other_side_as_twin() {
        for (l, r) in [
            (KeyCode::ShiftLeft, KeyCode::ShiftRight),
            (KeyCode::ControlLeft, KeyCode::ControlRight),
            (KeyCode::AltLeft, KeyCode::AltRight),
            (KeyCode::SuperLeft, KeyCode::SuperRight),
        ] {
            let (l, r) = (PhysicalKey::Code(l), PhysicalKey::Code(r));
            assert_eq!(physical_modifier_twin(l), Some(r));
            assert_eq!(physical_modifier_twin(r), Some(l));
        }
        for other in [KeyCode::KeyA, KeyCode::CapsLock, KeyCode::Meta, KeyCode::Fn] {
            assert_eq!(physical_modifier_twin(PhysicalKey::Code(other)), None);
        }
    }

    #[test]
    fn us_qwerty_shifted_and_kitty_wire_round_trip() {
        assert_eq!(us_qwerty_shifted_ascii('a'), Some('A'));
        assert_eq!(us_qwerty_shifted_ascii('1'), Some('!'));
        assert_eq!(us_qwerty_shifted_ascii('/'), Some('?'));
        assert_eq!(us_qwerty_shifted_ascii('A'), None);

        let wire = encode_kitty_key_wire_le('a' as u32, 'A' as u32, 'a' as u32, 0b0010, 1)
            .expect("valid kitty wire");
        assert_eq!(
            decode_kitty_key_wire_le(&wire),
            Some(('a', 'A', 'a', 0b0010, 1))
        );
        assert!(encode_kitty_key_wire_le(0x11_0000, 0, 0, 0, 1).is_none());
        assert!(encode_kitty_key_wire_le('a' as u32, 0, 0, 0, 0).is_none());
    }
}
