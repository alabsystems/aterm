// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration tests for the public `input::KeyCode` <-> `keyboard::Key` bridge.
//!
//! Part of #5681: closes the remaining bridge coverage gaps after the initial
//! `TryFrom` implementation landed in `input.rs`.

use aterm_types::input::KeyCode;
use aterm_types::keyboard::{Key, NamedKey};

#[test]
fn test_key_bridge_unicode_char_roundtrip() {
    let key = Key::try_from(KeyCode::Char('\u{1F980}')).expect("unicode char should bridge");
    assert_eq!(key, Key::Character('\u{1F980}'));

    let back = KeyCode::try_from(key).expect("unicode char should round-trip");
    assert_eq!(back, KeyCode::Char('\u{1F980}'));
}

#[test]
fn test_key_bridge_supported_named_keys_roundtrip() {
    for (app_key, protocol_key) in [
        (KeyCode::Backspace, NamedKey::Backspace),
        (KeyCode::Tab, NamedKey::Tab),
        (KeyCode::Escape, NamedKey::Escape),
        (KeyCode::Down, NamedKey::ArrowDown),
        (KeyCode::Left, NamedKey::ArrowLeft),
        (KeyCode::Right, NamedKey::ArrowRight),
        (KeyCode::Home, NamedKey::Home),
        (KeyCode::End, NamedKey::End),
        (KeyCode::PageUp, NamedKey::PageUp),
        (KeyCode::PageDown, NamedKey::PageDown),
        (KeyCode::Insert, NamedKey::Insert),
    ] {
        let key = Key::try_from(app_key).expect("named key should bridge");
        assert_eq!(key, Key::Named(protocol_key), "{app_key:?} -> protocol");

        let back = KeyCode::try_from(key).expect("named key should round-trip");
        assert_eq!(back, app_key, "{protocol_key:?} -> application");
    }
}

#[test]
fn test_key_bridge_function_keys_roundtrip() {
    const FUNCTION_KEYS: [NamedKey; 24] = [
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

    for (index, protocol_key) in FUNCTION_KEYS.into_iter().enumerate() {
        let function_key = KeyCode::F((index + 1) as u8);

        let key = Key::try_from(function_key).expect("function key should bridge");
        assert_eq!(key, Key::Named(protocol_key), "F{} -> protocol", index + 1);

        let back = KeyCode::try_from(key).expect("function key should round-trip");
        assert_eq!(back, function_key, "F{} -> application", index + 1);
    }
}
