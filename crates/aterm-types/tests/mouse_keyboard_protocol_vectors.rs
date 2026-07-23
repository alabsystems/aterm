// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_types::keyboard::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key, encode_key_with_event,
};
use aterm_types::mouse::{MouseEncoding, encode_mouse, encode_sgr, encode_urxvt, encode_x10};

#[test]
fn executable_mouse_protocol_vectors_cover_supported_encodings() {
    assert_eq!(encode_x10(0, 10, 5), b"\x1b[M +&");
    assert_eq!(
        encode_mouse(0, 10, 5, MouseEncoding::Sgr, false),
        b"\x1b[<0;11;6M"
    );
    assert_eq!(
        encode_mouse(0, 10, 5, MouseEncoding::Sgr, true),
        b"\x1b[<0;11;6m"
    );
    assert_eq!(
        encode_mouse(64, 10, 5, MouseEncoding::Urxvt, false),
        encode_urxvt(96, 10, 5)
    );
    assert_eq!(
        encode_mouse(0, 12, 34, MouseEncoding::SgrPixel, false),
        encode_sgr(0, 12, 34, false)
    );
}

#[test]
fn x10_large_coordinates_fall_back_to_sgr() {
    assert_eq!(
        encode_mouse(0, 400, 300, MouseEncoding::X10, false),
        b"\x1b[<0;401;301M"
    );
}

#[test]
fn executable_keyboard_vectors_cover_kitty_and_xterm_modes() {
    let kitty = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    // Text-producing repeats stay plain text under 0b11 (kitty parity: the
    // event-type subfield only decorates events that needed an escape form).
    assert_eq!(
        encode_key_with_event(
            &Key::Character('a'),
            Modifiers::empty(),
            kitty,
            KeyEventType::Repeat
        ),
        b"a"
    );
    assert_eq!(
        encode_key_with_event(
            &Key::Character('a'),
            Modifiers::empty(),
            kitty,
            KeyEventType::Release
        ),
        b""
    );

    let xterm =
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2 | KeyboardMode::XTERM_FORMAT_OTHER_KEYS;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::CTRL, xterm),
        b"\x1b[97;5u"
    );
}

#[test]
fn kitty_report_all_keys_keeps_legacy_functional_forms() {
    // The spec's functional-key table assigns arrows/F1-F12 their legacy CSI
    // forms permanently — kitty's rewrite is NOT gated on report-all-keys, so
    // even full-mode kitty puts ESC[A / ESC[1;2P on the wire, never the
    // internal PUA numbers (57352/57364).
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F1), Modifiers::SHIFT, mode),
        b"\x1b[1;2P"
    );
    // Keys with NO legacy alternative in the spec's table keep CSI-u: F13+
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
