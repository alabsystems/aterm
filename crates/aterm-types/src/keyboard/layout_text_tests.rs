// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for layout-produced text presses, Caps Lock capitalization, and non-Latin Ctrl encoding.

use super::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key, encode_key_with_layout,
    encode_key_with_text, layout_letter_twin, typed_character,
};

const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL: Modifiers = Modifiers::CTRL;
const ALT: Modifiers = Modifiers::ALT;
const CAPS: Modifiers = Modifiers::CAPS_LOCK;

const DISAMBIGUATE: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
const ALTERNATES: KeyboardMode = KeyboardMode::REPORT_ALTERNATE_KEYS;
const REPORT_ALL: KeyboardMode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
const TEXT: KeyboardMode = KeyboardMode::REPORT_ASSOCIATED_TEXT;

fn press(c: char, mods: Modifiers, mode: KeyboardMode, base: char, text: char) -> Vec<u8> {
    encode_key_with_text(
        &Key::Character(c),
        mods,
        mode,
        KeyEventType::Press,
        Some(base),
        Some(text),
    )
}

fn kitty(flags: u8) -> KeyboardMode {
    [
        (1, DISAMBIGUATE),
        (2, KeyboardMode::REPORT_EVENT_TYPES),
        (4, ALTERNATES),
        (8, REPORT_ALL),
        (16, TEXT),
    ]
    .into_iter()
    .filter(|(bit, _)| flags & bit != 0)
    .fold(KeyboardMode::empty(), |acc, (_, flag)| acc | flag)
}

#[test]
fn caps_lock_types_the_capital_when_no_layout_says() {
    let a = Key::Character('a');
    for mode in [KeyboardMode::empty(), kitty(1), kitty(5)] {
        assert_eq!(encode_key(&a, CAPS, mode), b"A", "caps+a under {mode:?}");
        assert_eq!(encode_key(&a, CAPS | SHIFT, mode), b"A", "caps+shift+a");
    }
    assert_eq!(
        encode_key(&Key::Character('ф'), CAPS, KeyboardMode::empty()),
        "Ф".as_bytes()
    );
    assert_eq!(encode_key(&a, CAPS | CTRL, KeyboardMode::empty()), [0x01]);
    assert_eq!(encode_key(&a, CAPS | ALT, KeyboardMode::empty()), b"\x1ba");
    assert_eq!(encode_key(&a, CAPS, kitty(24)), b"\x1b[97;65;65u");
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), CAPS, KeyboardMode::empty()),
        b"\x1b[A"
    );
}

#[test]
fn typed_character_is_the_us_derivation() {
    assert_eq!(typed_character('a', Modifiers::empty()), 'a');
    assert_eq!(typed_character('a', SHIFT), 'A');
    assert_eq!(typed_character('2', SHIFT), '@');
    assert_eq!(typed_character('a', CAPS), 'A');
    assert_eq!(typed_character('2', CAPS), '2');
    assert_eq!(typed_character('ф', CAPS), 'Ф');
    assert_eq!(typed_character('ß', CAPS), 'ß');
    assert_eq!(typed_character('ф', SHIFT), 'ф');
}

#[test]
fn a_text_press_writes_the_layouts_character() {
    for mode in [
        KeyboardMode::empty(),
        kitty(1),
        kitty(3),
        kitty(5),
        kitty(7),
    ] {
        assert_eq!(
            press('2', SHIFT, mode, '2', '"'),
            b"\"",
            "de Shift+2 {mode:?}"
        );
        assert_eq!(
            press('7', SHIFT, mode, '7', '/'),
            b"/",
            "de Shift+7 {mode:?}"
        );
        assert_eq!(
            press('&', SHIFT, mode, '1', '1'),
            b"1",
            "AZERTY Shift+& {mode:?}"
        );
        assert_eq!(
            press('é', SHIFT, mode, '2', '2'),
            b"2",
            "AZERTY Shift+é {mode:?}"
        );
        assert_eq!(press('ф', SHIFT, mode, 'a', 'Ф'), "Ф".as_bytes());
        assert_eq!(press('ü', SHIFT, mode, '[', 'Ü'), "Ü".as_bytes());
        assert_eq!(press('a', Modifiers::empty(), mode, 'a', 'A'), b"A");
    }
    let us = encode_key_with_layout(
        &Key::Character('2'),
        SHIFT,
        KeyboardMode::empty(),
        KeyEventType::Press,
        Some('2'),
    );
    assert_eq!(us, b"@");
}

#[test]
fn kitty_reports_the_layouts_character_beside_the_unshifted_key_code() {
    let de = |flags| press('2', SHIFT, kitty(flags), '2', '"');
    assert_eq!(de(8), b"\x1b[50;2u");
    assert_eq!(de(12), b"\x1b[50:34;2u");
    assert_eq!(de(24), b"\x1b[50;2;34u");
    assert_eq!(de(31), b"\x1b[50:34;2;34u");
    assert_eq!(press('7', SHIFT, kitty(24), '7', '/'), b"\x1b[55;2;47u");
    assert_eq!(press('&', SHIFT, kitty(13), '1', '1'), b"\x1b[38:49:49;2u");
    assert_eq!(
        press('ф', SHIFT, kitty(13), 'a', 'Ф'),
        b"\x1b[1092:1060:97;2u"
    );
    assert_eq!(press('ф', SHIFT, kitty(24), 'a', 'Ф'), b"\x1b[1092;2;1060u");
    assert_eq!(press('a', CAPS, kitty(29), 'a', 'A'), b"\x1b[97;65;65u");
    assert_eq!(press('a', SHIFT, KeyboardMode::empty(), 'a', 'a'), b"a");
    assert_eq!(press('a', SHIFT, kitty(13), 'a', 'a'), b"\x1b[97:65;2u");
    assert_eq!(press('a', SHIFT, kitty(29), 'a', 'a'), b"\x1b[97:65;2;97u");
    assert_eq!(
        press('ф', SHIFT, kitty(13), 'a', 'ф'),
        b"\x1b[1092:1060:97;2u"
    );
    assert_eq!(
        press('ö', SHIFT, kitty(12), ';', 'é'),
        b"\x1b[246:233:59;2u"
    );
    assert_eq!(
        press('ö', SHIFT, kitty(31), ';', 'é'),
        b"\x1b[246:233:59;2;233u"
    );
    assert_eq!(
        press('*', SHIFT, kitty(12), '\\', 'µ'),
        b"\x1b[42:181:92;2u"
    );
}

#[test]
fn xterm_modify_other_keys_reads_the_layouts_shifted_keysym() {
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(press('2', SHIFT, level2, '2', '"'), b"\"");
    assert_eq!(
        encode_key(&Key::Character('2'), SHIFT, level2),
        b"\x1b[27;2;64~"
    );
    assert_eq!(press('\'', SHIFT, level2, '\'', '@'), b"\x1b[27;2;64~");
}

#[test]
fn a_chord_writes_its_control_or_escape_form() {
    let none = KeyboardMode::empty();
    assert_eq!(press('2', SHIFT | CTRL, none, '2', '"'), [0x00]);
    assert_eq!(press('a', ALT, none, 'a', 'a'), b"\x1ba");
    assert_eq!(press('a', CTRL, none, 'a', 'A'), [0x01]);
    assert_eq!(press('a', CTRL, kitty(1), 'a', 'A'), b"\x1b[97;5u");
    assert_eq!(press('a', Modifiers::empty(), none, 'a', '\r'), b"a");
    let enter = encode_key_with_text(
        &Key::Named(NamedKey::Enter),
        Modifiers::empty(),
        none,
        KeyEventType::Press,
        None,
        Some('x'),
    );
    assert_eq!(enter, b"\r");
    let release = encode_key_with_text(
        &Key::Character('2'),
        SHIFT,
        kitty(31),
        KeyEventType::Release,
        Some('2'),
        Some('"'),
    );
    assert_eq!(release, b"\x1b[50:34;2:3u");
}

#[test]
fn ctrl_on_a_non_latin_layout_sends_the_us_letters_control_byte() {
    let ctrl = |c: char, mods, mode, base| {
        encode_key_with_layout(&Key::Character(c), mods, mode, KeyEventType::Press, base)
    };
    let none = KeyboardMode::empty();
    assert_eq!(ctrl('с', CTRL, none, Some('c')), [0x03]);
    assert_eq!(ctrl('в', CTRL, none, Some('d')), [0x04]);
    assert_eq!(ctrl('с', CTRL | SHIFT, none, Some('c')), [0x03]);
    assert_eq!(ctrl('с', CTRL | ALT, none, Some('c')), [0x1b, 0x03]);
    assert_eq!(ctrl('ψ', CTRL, none, Some('c')), [0x03]);
    assert_eq!(
        ctrl(
            'с',
            CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1,
            Some('c')
        ),
        [0x03]
    );
    assert_eq!(
        ctrl(
            'с',
            CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
            Some('c')
        ),
        b"\x1b[27;5;99~"
    );
    assert_eq!(ctrl('с', CTRL, kitty(1), Some('c')), b"\x1b[1089;5u");
    assert_eq!(ctrl('с', CTRL, kitty(5), Some('c')), b"\x1b[1089::99;5u");
    assert_eq!(ctrl('с', CTRL, none, None), "с".as_bytes());
    assert_eq!(ctrl('a', CTRL, none, Some('q')), [0x01]);
}

#[test]
fn a_chord_on_a_non_us_layout_takes_the_layouts_character() {
    let none = KeyboardMode::empty();
    assert_eq!(press('2', ALT | SHIFT, none, '2', '"'), b"\x1b\"");
    assert_eq!(press('7', ALT | SHIFT, none, '7', '/'), b"\x1b/");
    assert_eq!(press('&', ALT | SHIFT, none, '1', '1'), b"\x1b1");
    assert_eq!(press('ф', ALT | SHIFT, none, 'a', 'Ф'), "\x1bФ".as_bytes());
    assert_eq!(
        encode_key(&Key::Character('2'), ALT | SHIFT, none),
        b"\x1b@"
    );

    let ctrl = |c: char, mods, mode, base| {
        encode_key_with_layout(&Key::Character(c), mods, mode, KeyEventType::Press, base)
    };
    assert_eq!(ctrl('х', CTRL, none, Some('[')), [0x1b]);
    assert_eq!(ctrl('ü', CTRL, none, Some('[')), [0x1b]);
    assert_eq!(ctrl('ъ', CTRL, none, Some(']')), [0x1d]);
    assert_eq!(ctrl('х', CTRL | ALT, none, Some('[')), [0x1b, 0x1b]);
    assert_eq!(press('х', CTRL | SHIFT, none, '[', 'Х'), [0x1b]);
    assert_eq!(ctrl('é', CTRL, none, Some('2')), [0x00]);
    assert_eq!(press('ß', CTRL | SHIFT, none, '-', '?'), [0x7f]);
    assert_eq!(press('2', CTRL | SHIFT, none, '2', '"'), [0x00]);
    assert_eq!(ctrl('ö', CTRL, none, Some(';')), "ö".as_bytes());
    assert_eq!(press('ö', CTRL | SHIFT, none, ';', 'Ö'), "Ö".as_bytes());
    assert_eq!(
        press('2', ALT | SHIFT, KeyboardMode::ALT_NO_ESC, '2', '"'),
        b"\""
    );
    assert_eq!(ctrl('#', CTRL, none, Some('\\')), b"#");
    assert_eq!(
        ctrl(
            'х',
            CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
            Some('[')
        ),
        b"\x1b[27;5;91~"
    );
    assert_eq!(
        press(
            'х',
            CTRL | SHIFT,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
            '[',
            'Х'
        ),
        b"\x1b[27;6;123~"
    );

    assert_eq!(
        press('2', ALT | SHIFT, kitty(5), '2', '"'),
        b"\x1b[50:34;4u"
    );
    assert_eq!(
        press('2', CTRL | SHIFT, kitty(5), '2', '"'),
        b"\x1b[50:34;6u"
    );
    assert_eq!(ctrl('х', CTRL, kitty(1), Some('[')), b"\x1b[1093;5u");
    assert_eq!(ctrl('х', CTRL, kitty(5), Some('[')), b"\x1b[1093::91;5u");
}

#[test]
fn layout_letter_twin_matches_non_ascii_letters_with_ascii_letter_base_keys() {
    assert_eq!(layout_letter_twin('с', Some('c')), Some('c'));
    assert_eq!(layout_letter_twin('ψ', Some('c')), Some('c'));
    assert_eq!(layout_letter_twin('ב', Some('C')), Some('C'));
    assert_eq!(layout_letter_twin('a', Some('q')), None);
    assert_eq!(layout_letter_twin('Z', Some('y')), None);
    assert_eq!(layout_letter_twin('№', Some('n')), None);
    assert_eq!(layout_letter_twin('é', Some('2')), None);
    assert_eq!(layout_letter_twin('б', Some(',')), None);
    assert_eq!(layout_letter_twin('с', None), None);
}
