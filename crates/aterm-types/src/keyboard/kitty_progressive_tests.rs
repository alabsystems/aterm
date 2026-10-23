// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for Kitty progressive-enhancement keyboard flags.

use super::*;

#[test]
#[rustfmt::skip]
fn report_all_keys_encodings() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_cases(&[
        ("plain char", ch('a'), NO_MODS, mode, KEY, b"\x1b[97u"),
        ("enter", named(NamedKey::Enter), NO_MODS, mode, KEY, b"\x1b[13u"),
        ("tab", named(NamedKey::Tab), NO_MODS, mode, KEY, b"\x1b[9u"),
    ]);
}

#[test]
fn kitty_report_all_keys_keeps_legacy_functional_forms() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F1), Modifiers::SHIFT, mode),
        b"\x1b[1;2P"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F13), Modifiers::empty(), mode),
        b"\x1b[57376u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode),
        b"\x1b[13u"
    );
}

#[test]
#[rustfmt::skip]
fn report_alternate_keys_encodings() {
    let disamb = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ALTERNATE_KEYS;
    assert_cases(&[
        ("shifted letter", ch('a'), Modifiers::SHIFT, disamb, KEY, b"A"),
        ("shifted symbol", ch('1'), Modifiers::SHIFT, disamb, KEY, b"!"),
        ("report-all shift", ch('a'), Modifiers::SHIFT, all, KEY, b"\x1b[97:65;2u"),
        ("named key", named(NamedKey::ArrowUp), Modifiers::SHIFT, disamb, KEY, b"\x1b[1;2A"),
        ("unshifted", ch('a'), NO_MODS, disamb, KEY, b"a"),
        ("ctrl+shift", ch('a'), Modifiers::CTRL | Modifiers::SHIFT, disamb, KEY, b"\x1b[97:65;6u"),
        ("unmappable", ch('~'), Modifiers::SHIFT, disamb, KEY, b"~"),
        ("identity shift, report-all", ch('~'), Modifiers::SHIFT, all, KEY, b"\x1b[126;2u"),
    ]);
}

#[test]
#[rustfmt::skip]
fn report_associated_text_encodings() {
    let text = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let text_events = text | KeyboardMode::REPORT_EVENT_TYPES;
    let three = text | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let five = three | KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        (
            "needs report-all",
            ch('a'),
            NO_MODS,
            KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ASSOCIATED_TEXT,
            KEY,
            b"a",
        ),
        ("appends codepoint", ch('a'), NO_MODS, text, KEY, b"\x1b[97;1;97u"),
        ("shifted character", ch('a'), Modifiers::SHIFT, text, KEY, b"\x1b[97;2;65u"),
        ("no payload on release", ch('a'), NO_MODS, text_events, RELEASE, b"\x1b[97;1:3u"),
        ("payload on repeat", ch('a'), NO_MODS, text_events, REPEAT, b"\x1b[97;1:2;97u"),
        ("no payload with alt", ch('a'), Modifiers::ALT, text, KEY, b"\x1b[97;3u"),
        ("no payload with ctrl", ch('a'), Modifiers::CTRL, text, KEY, b"\x1b[97;5u"),
        ("no payload with super", ch('a'), Modifiers::SUPER, text, KEY, b"\x1b[97;9u"),
        ("shifted symbol", ch('1'), Modifiers::SHIFT, text, KEY, b"\x1b[49;2;33u"),
        ("three flags, shift", ch('a'), Modifiers::SHIFT, three, KEY, b"\x1b[97:65;2;65u"),
        ("three flags, unshifted", ch('a'), NO_MODS, three, KEY, b"\x1b[97;1;97u"),
        ("five flags, release", ch('a'), Modifiers::SHIFT, five, RELEASE, b"\x1b[97:65;2:3u"),
    ]);
}

#[test]
fn report_associated_text_omits_payload_for_named_key() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let result = encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[13u");
    let result = encode_key(&Key::Named(NamedKey::Tab), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[9u");
}

#[test]
#[rustfmt::skip]
fn modifier_key_encodings() {
    let events = KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        ("bare shift release", named(NamedKey::ShiftLeft), Modifiers::SHIFT, events, RELEASE, b""),
        ("bare ctrl release", named(NamedKey::ControlLeft), Modifiers::CTRL, events, RELEASE, b""),
        ("bare alt release", named(NamedKey::AltLeft), Modifiers::ALT, events, RELEASE, b""),
        (
            "report-all ctrl release",
            named(NamedKey::ControlLeft),
            NO_MODS,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | events,
            RELEASE,
            b"\x1b[57442;1:3u",
        ),
        (
            "report-all ctrl release, the other Control held",
            named(NamedKey::ControlLeft),
            Modifiers::CTRL,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | events,
            RELEASE,
            b"\x1b[57442;5:3u",
        ),
        ("bare shift press", named(NamedKey::ShiftLeft), NO_MODS, events, PRESS, b""),
        ("bare ctrl press", named(NamedKey::ControlLeft), NO_MODS, events, PRESS, b""),
    ]);
}
