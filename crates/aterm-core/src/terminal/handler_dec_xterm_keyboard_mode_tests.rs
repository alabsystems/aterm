// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;
use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, encode_key};

#[test]
fn mode_1035_special_modifiers_toggles_flag() {
    let mut term = Terminal::new(24, 80);
    assert!(
        !term
            .keyboard_mode()
            .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
    );
    term.process(b"\x1b[?1035l");
    assert!(
        term.keyboard_mode()
            .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
    );
    term.process(b"\x1b[?1035h");
    assert!(
        !term
            .keyboard_mode()
            .contains(KeyboardMode::NO_SPECIAL_MODIFIERS)
    );
}

#[test]
fn mode_1039_reset_drops_alt_escape_prefix_in_encoding() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
        vec![0x1b, b'a']
    );
    term.process(b"\x1b[?1039l");
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::ALT, term.keyboard_mode()),
        vec![b'a']
    );
}

#[test]
fn mode_1036_set_adds_meta_escape_prefix_in_encoding() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
        vec![b'a']
    );
    term.process(b"\x1b[?1036h");
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::META, term.keyboard_mode()),
        vec![0x1b, b'a']
    );
}

#[test]
fn decrqm_reports_keyboard_mode_states() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?1035$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1035;1$y");
    term.process(b"\x1b[?1036$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1036;2$y");
    term.process(b"\x1b[?1039$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1039;1$y");
    term.process(b"\x1b[?1045$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1045;2$y");

    term.process(b"\x1b[?1035l");
    term.process(b"\x1b[?1036h");
    term.process(b"\x1b[?1039l");
    term.process(b"\x1b[?1045h");
    term.process(b"\x1b[?1035$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1035;2$y");
    term.process(b"\x1b[?1036$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1036;1$y");
    term.process(b"\x1b[?1039$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1039;2$y");
    term.process(b"\x1b[?1045$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1045;1$y");
}
