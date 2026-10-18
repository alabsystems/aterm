// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty progressive-enhancement flags: REPORT_ALTERNATE_KEYS, REPORT_ALL_KEYS_AS_ESC and
//! REPORT_ASSOCIATED_TEXT, alone and combined.

use super::*;

// REPORT_ALL_KEYS_AS_ESC: forces the CSI-u path

#[test]
#[rustfmt::skip]
fn report_all_keys_encodings() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_cases(&[
        // Report-all alone sends even a plain 'a' as CSI u.
        ("plain char", ch('a'), NO_MODS, mode, KEY, b"\x1b[97u"),
        // Enter and Tab by their kitty codes (13, 9).
        ("enter", named(NamedKey::Enter), NO_MODS, mode, KEY, b"\x1b[13u"),
        ("tab", named(NamedKey::Tab), NO_MODS, mode, KEY, b"\x1b[9u"),
    ]);
}

/// Arrows and F1-F12 keep their legacy CSI forms even under report-all, never the PUA numbers.
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
    // Keys with no legacy alternative in the spec's table keep CSI-u: F13+
    // (57376+) and Enter (13).
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F13), Modifiers::empty(), mode),
        b"\x1b[57376u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode),
        b"\x1b[13u"
    );
}

// REPORT_ALTERNATE_KEYS: shifted alternate codepoints

#[test]
#[rustfmt::skip]
fn report_alternate_keys_encodings() {
    let disamb = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ALTERNATE_KEYS;
    assert_cases(&[
        // REPORT_ALTERNATE_KEYS only enhances events already sent as escape codes (spec), so
        // Shift-only text under disambiguate stays plain.
        ("shifted letter", ch('a'), Modifiers::SHIFT, disamb, KEY, b"A"),
        ("shifted symbol", ch('1'), Modifiers::SHIFT, disamb, KEY, b"!"),
        // With REPORT_ALL_KEYS_AS_ESC the event is an escape code, so the
        // alternate-key subparam appears: primary 97, shifted alternate 65.
        ("report-all shift", ch('a'), Modifiers::SHIFT, all, KEY, b"\x1b[97:65;2u"),
        // Named keys have no alternate codepoints; ArrowUp keeps its legacy form (#7474).
        ("named key", named(NamedKey::ArrowUp), Modifiers::SHIFT, disamb, KEY, b"\x1b[1;2A"),
        // Without Shift there is no alternate, and a plain key stays text.
        ("unshifted", ch('a'), NO_MODS, disamb, KEY, b"a"),
        // Ctrl does not suppress the alternate: primary 97, alternate 65, mod 6.
        ("ctrl+shift", ch('a'), Modifiers::CTRL | Modifiers::SHIFT, disamb, KEY, b"\x1b[97:65;6u"),
        // Shift-only text stays text (pure-enhancement rule); Shift+'~' is '~'.
        ("unmappable", ch('~'), Modifiers::SHIFT, disamb, KEY, b"~"),
        // Under report-all, shifted '~' is '~' itself: no alternate subparam.
        ("identity shift, report-all", ch('~'), Modifiers::SHIFT, all, KEY, b"\x1b[126;2u"),
    ]);
}

// REPORT_ASSOCIATED_TEXT: the text's codepoint as the third parameter

#[test]
#[rustfmt::skip]
fn report_associated_text_encodings() {
    let text = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let text_events = text | KeyboardMode::REPORT_EVENT_TYPES;
    let three = text | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let five = three | KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        // Associated text needs REPORT_ALL_KEYS_AS_ESC; without it a plain key is its text byte.
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
        // A repeat, unlike a release, carries the text.
        ("payload on repeat", ch('a'), NO_MODS, text_events, REPEAT, b"\x1b[97;1:2;97u"),
        ("no payload with alt", ch('a'), Modifiers::ALT, text, KEY, b"\x1b[97;3u"),
        // Ctrl and Super suppress the payload too, as kitty does.
        ("no payload with ctrl", ch('a'), Modifiers::CTRL, text, KEY, b"\x1b[97;5u"),
        ("no payload with super", ch('a'), Modifiers::SUPER, text, KEY, b"\x1b[97;9u"),
        // Shift+'1' reports the text '!' (33).
        ("shifted symbol", ch('1'), Modifiers::SHIFT, text, KEY, b"\x1b[49;2;33u"),
        // Alternate + report-all + text, Shift+'a': alternate 65, text 65.
        ("three flags, shift", ch('a'), Modifiers::SHIFT, three, KEY, b"\x1b[97:65;2;65u"),
        // No Shift: no alternate, mod=1 to carry the text.
        ("three flags, unshifted", ch('a'), NO_MODS, three, KEY, b"\x1b[97;1;97u"),
        // All five flags, release: alternate 65, event 3, no text.
        ("five flags, release", ch('a'), Modifiers::SHIFT, five, RELEASE, b"\x1b[97:65;2:3u"),
    ]);
}

#[test]
fn report_associated_text_omits_payload_for_named_key() {
    // Enter and Tab type control codes, which the spec's one exclusion drops.
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let result = encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[13u");
    let result = encode_key(&Key::Named(NamedKey::Tab), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[9u");
}

// Modifier keys report only under REPORT_ALL_KEYS_AS_ESC (spec: "with this mode, events for
// pressing modifier keys are reported"); under every other flag set kitty's is_modifier_key
// gate drops them, press and release.

#[test]
#[rustfmt::skip]
fn modifier_key_encodings() {
    let events = KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        ("bare shift release", named(NamedKey::ShiftLeft), Modifiers::SHIFT, events, RELEASE, b""),
        ("bare ctrl release", named(NamedKey::ControlLeft), Modifiers::CTRL, events, RELEASE, b""),
        ("bare alt release", named(NamedKey::AltLeft), Modifiers::ALT, events, RELEASE, b""),
        // ControlLeft=57442; a release carries the state it leaves: mod=1 alone, 5 with the
        // other Control held.
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
        // REPORT_EVENT_TYPES alone does not report a modifier press.
        ("bare shift press", named(NamedKey::ShiftLeft), NO_MODS, events, PRESS, b""),
        ("bare ctrl press", named(NamedKey::ControlLeft), NO_MODS, events, PRESS, b""),
    ]);
}
