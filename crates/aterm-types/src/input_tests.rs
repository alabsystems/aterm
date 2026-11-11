// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn bit_layout_matches_keyboard_modifiers() {
    use crate::keyboard::Modifiers;

    assert_eq!(KeyModifiers::SHIFT.bits(), Modifiers::SHIFT.bits());
    assert_eq!(KeyModifiers::ALT.bits(), Modifiers::ALT.bits());
    assert_eq!(KeyModifiers::CTRL.bits(), Modifiers::CTRL.bits());
    assert_eq!(KeyModifiers::SUPER.bits(), Modifiers::SUPER.bits());
}

/// `KeyCode` <-> `keyboard::Key` (#5681): every application key
/// round-trips, and what has no application spelling is refused.
#[test]
fn key_bridge_round_trips_and_refuses_what_it_cannot_spell() {
    use crate::keyboard::{Key, NamedKey};
    let pairs = [
        // A space is a character, NOT NamedKey::Space.
        (KeyCode::Char(' '), Key::Character(' ')),
        (KeyCode::Char('\u{1F980}'), Key::Character('\u{1F980}')),
        (KeyCode::Enter, Key::Named(NamedKey::Enter)),
        (KeyCode::Up, Key::Named(NamedKey::ArrowUp)),
        (KeyCode::Down, Key::Named(NamedKey::ArrowDown)),
        (KeyCode::Left, Key::Named(NamedKey::ArrowLeft)),
        (KeyCode::Right, Key::Named(NamedKey::ArrowRight)),
        (KeyCode::Delete, Key::Named(NamedKey::Delete)),
        (KeyCode::Backspace, Key::Named(NamedKey::Backspace)),
        (KeyCode::Tab, Key::Named(NamedKey::Tab)),
        (KeyCode::Escape, Key::Named(NamedKey::Escape)),
        (KeyCode::Home, Key::Named(NamedKey::Home)),
        (KeyCode::End, Key::Named(NamedKey::End)),
        (KeyCode::PageUp, Key::Named(NamedKey::PageUp)),
        (KeyCode::PageDown, Key::Named(NamedKey::PageDown)),
        (KeyCode::Insert, Key::Named(NamedKey::Insert)),
        (KeyCode::F(1), Key::Named(NamedKey::F1)),
        (KeyCode::F(12), Key::Named(NamedKey::F12)),
        (KeyCode::F(24), Key::Named(NamedKey::F24)),
    ];
    for (code, key) in pairs {
        assert_eq!(Key::try_from(code), Ok(key.clone()), "{code:?} -> protocol");
        assert_eq!(KeyCode::try_from(key), Ok(code), "{code:?} <- protocol");
    }
    assert_eq!(
        KeyCode::try_from(Key::Named(NamedKey::Space)),
        Ok(KeyCode::Char(' '))
    );
    for n in [0, 25] {
        assert_eq!(
            Key::try_from(KeyCode::F(n)),
            Err(KeyConversionError::FunctionKeyOutOfRange(n))
        );
    }
    for named in [NamedKey::Numpad5, NamedKey::NumpadEnter] {
        assert_eq!(
            KeyCode::try_from(Key::Named(named)),
            Err(KeyConversionError::ProtocolOnlyKey(named))
        );
    }
}
