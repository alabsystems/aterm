// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The layout's own character on a text press (`encode_key_with_text`), Caps Lock's capital
//! when no layout says, and Ctrl on a non-Latin layout. Without layout text the engine derives
//! the glyph from the US table, which is wrong elsewhere (German Shift+2 types `"`).

use super::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key, encode_key_with_layout,
    encode_key_with_text, typed_character,
};

const SHIFT: Modifiers = Modifiers::SHIFT;
const CTRL: Modifiers = Modifiers::CTRL;
const ALT: Modifiers = Modifiers::ALT;
const CAPS: Modifiers = Modifiers::CAPS_LOCK;

const DISAMBIGUATE: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
const ALTERNATES: KeyboardMode = KeyboardMode::REPORT_ALTERNATE_KEYS;
const REPORT_ALL: KeyboardMode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
const TEXT: KeyboardMode = KeyboardMode::REPORT_ASSOCIATED_TEXT;

/// The press of base key `c` under `mods`, with the US key `base` and the
/// layout's character `text`.
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

/// The kitty flag sets a text press is read under: `CSI > 1 u`, `> 5`, `> 8`,
/// `> 12`, `> 24` and `> 31` (every flag, event types included).
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
    // A controller key (no layout text): Caps Lock capitalizes the letter, as xterm's
    // XLookupString and the OS do.
    let a = Key::Character('a');
    for mode in [KeyboardMode::empty(), kitty(1), kitty(5)] {
        assert_eq!(encode_key(&a, CAPS, mode), b"A", "caps+a under {mode:?}");
        assert_eq!(encode_key(&a, CAPS | SHIFT, mode), b"A", "caps+shift+a");
    }
    assert_eq!(
        encode_key(&Key::Character('ф'), CAPS, KeyboardMode::empty()),
        "Ф".as_bytes()
    );
    // A chord keeps its own encoding; report-all keeps the key code, the capital as text.
    assert_eq!(encode_key(&a, CAPS | CTRL, KeyboardMode::empty()), [0x01]);
    assert_eq!(encode_key(&a, CAPS | ALT, KeyboardMode::empty()), b"\x1ba");
    assert_eq!(encode_key(&a, CAPS, kitty(24)), b"\x1b[97;65;65u");
    // The named keys still ignore the lock (no `ESC[1;1A`).
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
    // No single-character capital: left as it is.
    assert_eq!(typed_character('ß', CAPS), 'ß');
    // The US table cannot know a Cyrillic layout's Shift level; the window's
    // layout text can, which is why it is carried.
    assert_eq!(typed_character('ф', SHIFT), 'ф');
}

#[test]
fn a_text_press_writes_the_layouts_character() {
    // de-DE Shift+2 types '"' (the US table says '@'); Shift+7 types '/'.
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
        // AZERTY: Shift on the '&' key types '1', on the 'é' key '2'.
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
        // Russian Shift+ф (the A key) and German Shift+ü type the capitals.
        assert_eq!(press('ф', SHIFT, mode, 'a', 'Ф'), "Ф".as_bytes());
        assert_eq!(press('ü', SHIFT, mode, '[', 'Ü'), "Ü".as_bytes());
        // A Caps Lock the engine was never told of (Linux).
        assert_eq!(press('a', Modifiers::empty(), mode, 'a', 'A'), b"A");
    }
    // Without the layout's character the engine still derives the US glyph.
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
    // The key code stays the layout's unshifted key ('2' = 50). The layout's
    // character is the shifted key under REPORT_ALTERNATE_KEYS and the text
    // under REPORT_ASSOCIATED_TEXT: German Shift+2 is `"` (34), not '@' (64).
    let de = |flags| press('2', SHIFT, kitty(flags), '2', '"');
    assert_eq!(de(8), b"\x1b[50;2u");
    assert_eq!(de(12), b"\x1b[50:34;2u");
    assert_eq!(de(24), b"\x1b[50;2;34u");
    assert_eq!(de(31), b"\x1b[50:34;2;34u");
    // German Shift+7: the associated text is '/' (47), not '&' (38).
    assert_eq!(press('7', SHIFT, kitty(24), '7', '/'), b"\x1b[55;2;47u");
    // AZERTY Shift+& types '1': key 38, shifted 49, the US key 49.
    assert_eq!(press('&', SHIFT, kitty(13), '1', '1'), b"\x1b[38:49:49;2u");
    // Russian Shift+ф: key 1092, shifted 1060 ('Ф'), the US key 'a' (97).
    assert_eq!(
        press('ф', SHIFT, kitty(13), 'a', 'Ф'),
        b"\x1b[1092:1060:97;2u"
    );
    assert_eq!(press('ф', SHIFT, kitty(24), 'a', 'Ф'), b"\x1b[1092;2;1060u");
    // Caps Lock alone composes text but is not Shift: no shifted key.
    assert_eq!(press('a', CAPS, kitty(29), 'a', 'A'), b"\x1b[97;65;65u");
    // Caps Lock + Shift on Linux, where the window is never told of the lock: xkb's Lock
    // cancels Shift on a letter, so the text is 'a', but the shifted key is the Shift level,
    // which no lock touches: 'A' (65), as kitty reports it.
    assert_eq!(press('a', SHIFT, KeyboardMode::empty(), 'a', 'a'), b"a");
    assert_eq!(press('a', SHIFT, kitty(13), 'a', 'a'), b"\x1b[97:65;2u");
    assert_eq!(press('a', SHIFT, kitty(29), 'a', 'a'), b"\x1b[97:65;2;97u");
    assert_eq!(
        press('ф', SHIFT, kitty(13), 'a', 'ф'),
        b"\x1b[1092:1060:97;2u"
    );
    // A layout whose Shift level is another lowercase letter keeps it: Swiss German Shift+ö is
    // é (233), French Linux Shift+* is µ (181).
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
    // modifyOtherKeys level 2 escapes a Shift chord whose shifted keysym is in
    // '@'..DEL. German Shift+2 is '"', ordinary text; US Shift+2 is '@'.
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(press('2', SHIFT, level2, '2', '"'), b"\"");
    assert_eq!(
        encode_key(&Key::Character('2'), SHIFT, level2),
        b"\x1b[27;2;50~"
    );
}

#[test]
fn the_layouts_character_never_replaces_a_chord() {
    // Ctrl, Alt and Super chords encode from the base key on every layout:
    // the window never carries layout text for them, and the encoder refuses
    // it if a caller does.
    let none = KeyboardMode::empty();
    assert_eq!(press('2', SHIFT | CTRL, none, '2', '"'), [0x00]);
    assert_eq!(press('a', ALT, none, 'a', 'å'), b"\x1ba");
    assert_eq!(press('a', CTRL, none, 'a', 'A'), [0x01]);
    assert_eq!(press('a', CTRL, kitty(1), 'a', 'A'), b"\x1b[97;5u");
    // A control code is never text, and a named key never takes any.
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
    // A release under report-all carries the shifted key but never text.
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
    // Russian Ctrl+С (the C key) and Ctrl+В (the D key): ETX and EOT, as Terminal.app and
    // iTerm2 send them.
    let ctrl = |c: char, mods, mode, base| {
        encode_key_with_layout(&Key::Character(c), mods, mode, KeyEventType::Press, base)
    };
    let none = KeyboardMode::empty();
    assert_eq!(ctrl('с', CTRL, none, Some('c')), [0x03]);
    assert_eq!(ctrl('в', CTRL, none, Some('d')), [0x04]);
    assert_eq!(ctrl('с', CTRL | SHIFT, none, Some('c')), [0x03]);
    assert_eq!(ctrl('с', CTRL | ALT, none, Some('c')), [0x1b, 0x03]);
    // Greek Ctrl+ψ (the C key).
    assert_eq!(ctrl('ψ', CTRL, none, Some('c')), [0x03]);
    // xterm modifyOtherKeys: level 1 keeps the traditional byte, level 2
    // reports the key the byte names.
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
    // Kitty reports the layout's key, with the US key in the base-layout field.
    assert_eq!(ctrl('с', CTRL, kitty(1), Some('c')), b"\x1b[1089;5u");
    assert_eq!(ctrl('с', CTRL, kitty(5), Some('c')), b"\x1b[1089::99;5u");
    // Only a letter on a US letter key: AZERTY 'é' sits on the 2 key, and
    // Ctrl+é is not Ctrl+2 (NUL). No US key known (a controller): unchanged.
    assert_eq!(ctrl('é', CTRL, none, Some('2')), "é".as_bytes());
    assert_eq!(ctrl('с', CTRL, none, None), "с".as_bytes());
    // A Latin layout's own letters keep their bytes (AZERTY Ctrl+a on the Q key).
    assert_eq!(ctrl('a', CTRL, none, Some('q')), [0x01]);
}
