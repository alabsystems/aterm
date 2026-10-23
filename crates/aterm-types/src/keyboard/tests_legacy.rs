// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy keyboard encoding tests.

use super::*;

#[test]
#[rustfmt::skip]
fn legacy_character_encodings() {
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    assert_cases(&[
        ("plain", ch('a'), NO_MODS, LEGACY, KEY, b"a"),
        ("shift", ch('a'), Modifiers::SHIFT, LEGACY, KEY, b"A"),
        ("alt+shift symbol", ch('2'), alt_shift, LEGACY, KEY, &[0x1b, b'@']),
        ("alt", ch('a'), Modifiers::ALT, LEGACY, KEY, &[0x1b, b'a']),
        ("alt+shift", ch('a'), alt_shift, LEGACY, KEY, &[0x1b, b'A']),
        ("ctrl+alt", ch('c'), Modifiers::CTRL | Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x03]),
        ("meta, 1036 reset", ch('a'), Modifiers::META, LEGACY, KEY, b"a"),
        ("release", ch('a'), NO_MODS, LEGACY, RELEASE, b""),
    ]);
}

#[test]
fn legacy_encode_ctrl_underscore_is_us_for_emacs_undo() {
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    assert_eq!(
        encode_key(&Key::Character('-'), ctrl_shift, KeyboardMode::empty()),
        vec![0x1f],
        "Ctrl-_ is US (0x1F) — the readline/emacs undo byte"
    );

    for (base, want, name) in [
        ('/', 0x1f_u8, "Ctrl+Shift+/ stays US, not DEL"),
        ('2', 0x00, "Ctrl+Shift+2 is NUL"),
        ('6', 0x1e, "Ctrl+Shift+6 is RS"),
    ] {
        assert_eq!(
            encode_key(&Key::Character(base), ctrl_shift, KeyboardMode::empty()),
            vec![want],
            "{name}"
        );
    }

    assert_eq!(
        encode_key(&Key::Character('-'), Modifiers::CTRL, KeyboardMode::empty()),
        vec![b'-'],
        "plain Ctrl+- is untouched by the shifted fallback"
    );
}

#[test]
fn legacy_encode_shift_symbol() {
    let cases: &[(char, u8)] = &[
        ('1', b'!'),
        ('2', b'@'),
        ('3', b'#'),
        ('4', b'$'),
        ('5', b'%'),
        ('6', b'^'),
        ('7', b'&'),
        ('8', b'*'),
        ('9', b'('),
        ('0', b')'),
        ('-', b'_'),
        ('=', b'+'),
        ('[', b'{'),
        (']', b'}'),
        ('\\', b'|'),
        (';', b':'),
        ('\'', b'"'),
        (',', b'<'),
        ('.', b'>'),
        ('/', b'?'),
        ('`', b'~'),
    ];
    for &(base, shifted) in cases {
        let result = encode_key(
            &Key::Character(base),
            Modifiers::SHIFT,
            KeyboardMode::empty(),
        );
        assert_eq!(
            result,
            vec![shifted],
            "Shift+{base:?} must encode the shifted glyph {:?}",
            shifted as char
        );
    }
}

#[test]
#[rustfmt::skip]
fn legacy_control_and_editing_key_encodings() {
    assert_cases(&[
        ("enter", named(NamedKey::Enter), NO_MODS, LEGACY, KEY, &[0x0d]),
        ("alt enter", named(NamedKey::Enter), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x0d]),
        ("tab", named(NamedKey::Tab), NO_MODS, LEGACY, KEY, &[0x09]),
        ("shift tab", named(NamedKey::Tab), Modifiers::SHIFT, LEGACY, KEY, &[0x1b, b'[', b'Z']),
        ("escape", named(NamedKey::Escape), NO_MODS, LEGACY, KEY, &[0x1b]),
        ("backspace", named(NamedKey::Backspace), NO_MODS, LEGACY, KEY, &[0x7f]),
        ("ctrl backspace", named(NamedKey::Backspace), Modifiers::CTRL, LEGACY, KEY, &[0x08]),
        ("space", named(NamedKey::Space), NO_MODS, LEGACY, KEY, &[0x20]),
        ("ctrl space", named(NamedKey::Space), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        ("home", named(NamedKey::Home), NO_MODS, LEGACY, KEY, b"\x1b[H"),
        ("end", named(NamedKey::End), NO_MODS, LEGACY, KEY, b"\x1b[F"),
        ("page up", named(NamedKey::PageUp), NO_MODS, LEGACY, KEY, b"\x1b[5~"),
        ("delete", named(NamedKey::Delete), NO_MODS, LEGACY, KEY, b"\x1b[3~"),
        ("insert", named(NamedKey::Insert), NO_MODS, LEGACY, KEY, b"\x1b[2~"),
    ]);
}

#[test]
#[rustfmt::skip]
fn legacy_cursor_and_function_key_encodings() {
    let app_cursor = KeyboardMode::APP_CURSOR;
    assert_cases(&[
        ("up", named(NamedKey::ArrowUp), NO_MODS, LEGACY, KEY, b"\x1b[A"),
        ("up, DECCKM", named(NamedKey::ArrowUp), NO_MODS, app_cursor, KEY, b"\x1bOA"),
        ("down", named(NamedKey::ArrowDown), NO_MODS, LEGACY, KEY, b"\x1b[B"),
        ("left", named(NamedKey::ArrowLeft), NO_MODS, LEGACY, KEY, b"\x1b[D"),
        ("right", named(NamedKey::ArrowRight), NO_MODS, LEGACY, KEY, b"\x1b[C"),
        ("shift up", named(NamedKey::ArrowUp), Modifiers::SHIFT, LEGACY, KEY, b"\x1b[1;2A"),
        ("ctrl up", named(NamedKey::ArrowUp), Modifiers::CTRL, LEGACY, KEY, b"\x1b[1;5A"),
        ("alt right", named(NamedKey::ArrowRight), Modifiers::ALT, LEGACY, KEY, b"\x1b[1;3C"),
        ("alt left", named(NamedKey::ArrowLeft), Modifiers::ALT, LEGACY, KEY, b"\x1b[1;3D"),
        ("f1", named(NamedKey::F1), NO_MODS, LEGACY, KEY, b"\x1bOP"),
        ("shift f1", named(NamedKey::F1), Modifiers::SHIFT, LEGACY, KEY, b"\x1b[1;2P"),
        ("f5", named(NamedKey::F5), NO_MODS, LEGACY, KEY, b"\x1b[15~"),
        ("f12", named(NamedKey::F12), NO_MODS, LEGACY, KEY, b"\x1b[24~"),
    ]);
}

#[test]
fn legacy_encode_shift_f10_is_terminfo_kf22() {
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::F10),
            Modifiers::empty(),
            KeyboardMode::empty(),
        ),
        b"\x1b[21~"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::F10),
            Modifiers::SHIFT,
            KeyboardMode::empty(),
        ),
        b"\x1b[21;2~"
    );
}

#[test]
#[rustfmt::skip]
fn legacy_numpad_encodings() {
    let app_keypad = KeyboardMode::APP_KEYPAD;
    assert_cases(&[
        ("kp0", named(NamedKey::Numpad0), NO_MODS, LEGACY, KEY, b"0"),
        ("kp0, DECKPAM", named(NamedKey::Numpad0), NO_MODS, app_keypad, KEY, b"\x1bOp"),
        ("kp enter", named(NamedKey::NumpadEnter), NO_MODS, LEGACY, KEY, &[0x0d]),
        ("kp enter, DECKPAM", named(NamedKey::NumpadEnter), NO_MODS, app_keypad, KEY, b"\x1bOM"),
        (
            "shift kp enter, DECKPAM",
            named(NamedKey::NumpadEnter),
            Modifiers::SHIFT,
            app_keypad,
            KEY,
            &[0x0a],
        ),
        ("alt kp enter", named(NamedKey::NumpadEnter), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x0d]),
        ("alt kp equal", named(NamedKey::NumpadEqual), Modifiers::ALT, LEGACY, KEY, b"\x1b="),
    ]);
}

#[test]
fn legacy_numpad_enter_outside_app_keypad_is_the_main_enter() {
    for mods in [
        Modifiers::empty(),
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::SHIFT | Modifiers::CTRL,
        Modifiers::SHIFT | Modifiers::ALT,
    ] {
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::NumpadEnter),
                mods,
                KeyboardMode::empty()
            ),
            encode_key(&Key::Named(NamedKey::Enter), mods, KeyboardMode::empty()),
            "{mods:?}: KP_Enter and Return differ outside DECKPAM"
        );
    }
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::SHIFT,
            KeyboardMode::empty()
        ),
        vec![0x0a],
        "the witness: Shift+KP_Enter is the LF imposition"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::empty(),
            KeyboardMode::APP_KEYPAD | KeyboardMode::VT52_MODE
        ),
        b"\x1b?M"
    );
}

#[test]
#[rustfmt::skip]
fn legacy_emacs_bindings() {
    assert_cases(&[
        ("C-SPC", ch(' '), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        ("C-/", ch('/'), Modifiers::CTRL, LEGACY, KEY, &[0x1f]),
        ("C-2", ch('2'), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        ("C-6", ch('6'), Modifiers::CTRL, LEGACY, KEY, &[0x1e]),
        ("C-8", ch('8'), Modifiers::CTRL, LEGACY, KEY, &[0x7f]),
        ("M-DEL", named(NamedKey::Backspace), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x7f]),
        ("C-M-a", ch('a'), Modifiers::CTRL | Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x01]),
        ("C-x", ch('x'), Modifiers::CTRL, LEGACY, KEY, &[0x18]),
    ]);

    for (letter, expected) in [
        ('a', 0x01u8), // beginning of line
        ('b', 0x02),   // backward char
        ('c', 0x03),   // interrupt
        ('d', 0x04),   // delete / EOF
        ('e', 0x05),   // end of line
        ('f', 0x06),   // forward char
        ('g', 0x07),   // abort
        ('h', 0x08),   // backspace
        ('k', 0x0b),   // kill to end of line
        ('l', 0x0c),   // clear screen
        ('n', 0x0e),   // next history
        ('p', 0x10),   // prev history
        ('r', 0x12),   // reverse search
        ('s', 0x13),   // forward search
        ('t', 0x14),   // transpose chars
        ('u', 0x15),   // kill line backward
        ('w', 0x17),   // kill word backward
        ('y', 0x19),   // yank
        ('z', 0x1a),   // suspend (SIGTSTP)
    ] {
        let result = encode_key(
            &Key::Character(letter),
            Modifiers::CTRL,
            KeyboardMode::empty(),
        );
        assert_eq!(
            result,
            vec![expected],
            "Ctrl+{letter} should be 0x{expected:02x}"
        );
    }
}

#[test]
fn legacy_meta_word_movement() {
    let result = encode_key(&Key::Character('f'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'f']);

    let result = encode_key(&Key::Character('b'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'b']);

    let result = encode_key(&Key::Character('d'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'd']);
}

#[test]
fn test_vt52_cursor_keys() {
    let mode = KeyboardMode::VT52_MODE;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1bA"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowDown), Modifiers::empty(), mode),
        b"\x1bB"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowRight), Modifiers::empty(), mode),
        b"\x1bC"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::empty(), mode),
        b"\x1bD"
    );
}

#[test]
fn test_vt52_mode_overrides_decckm() {
    let mode = KeyboardMode::VT52_MODE | KeyboardMode::APP_CURSOR;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1bA"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowDown), Modifiers::empty(), mode),
        b"\x1bB"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowRight), Modifiers::empty(), mode),
        b"\x1bC"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::empty(), mode),
        b"\x1bD"
    );
}

#[test]
fn test_vt52_cursor_keys_ignore_modifiers() {
    let mode = KeyboardMode::VT52_MODE;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::SHIFT, mode),
        b"\x1bA"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::CTRL, mode),
        b"\x1bD"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowRight), Modifiers::ALT, mode),
        b"\x1bC"
    );
}

#[test]
fn vt52_function_keys_and_home_end() {
    let mode = KeyboardMode::VT52_MODE;
    let named = |k| encode_key(&Key::Named(k), Modifiers::empty(), mode);
    assert_eq!(named(NamedKey::F1), b"\x1bP");
    assert_eq!(named(NamedKey::F4), b"\x1bS");
    assert_eq!(named(NamedKey::Home), b"\x1bH");
    assert_eq!(named(NamedKey::End), b"\x1bF");
    assert_eq!(named(NamedKey::F5), b"\x1b[15~");
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F2), Modifiers::SHIFT, mode),
        b"\x1bQ"
    );
}

#[test]
fn ctrl_alt_space_and_backspace_keep_the_alt() {
    let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
    let named = |k, m| encode_key(&Key::Named(k), m, KeyboardMode::empty());
    assert_eq!(named(NamedKey::Space, ctrl_alt), [0x1b, 0x00]);
    assert_eq!(named(NamedKey::Backspace, ctrl_alt), [0x1b, 0x08]);
    assert_eq!(named(NamedKey::Space, Modifiers::CTRL), [0x00]);
    assert_eq!(named(NamedKey::Backspace, Modifiers::ALT), [0x1b, 0x7f]);
    assert_eq!(
        named(NamedKey::Tab, Modifiers::SHIFT | Modifiers::ALT),
        b"\x1b[Z"
    );
}

#[test]
fn ctrl_tilde_is_record_separator() {
    let mode = KeyboardMode::empty();
    assert_eq!(encode_key(&ch('~'), Modifiers::CTRL, mode), [0x1e]);
    assert_eq!(
        encode_key(&ch('`'), Modifiers::CTRL | Modifiers::SHIFT, mode),
        [0x1e]
    );
    assert_eq!(
        encode_key(&ch('6'), Modifiers::CTRL | Modifiers::SHIFT, mode),
        [0x1e]
    );
}

#[test]
fn test_vt52_numpad_application_mode() {
    let mode = KeyboardMode::VT52_MODE | KeyboardMode::APP_KEYPAD;
    for digit in 0..=9 {
        let key = match digit {
            0 => NamedKey::Numpad0,
            1 => NamedKey::Numpad1,
            2 => NamedKey::Numpad2,
            3 => NamedKey::Numpad3,
            4 => NamedKey::Numpad4,
            5 => NamedKey::Numpad5,
            6 => NamedKey::Numpad6,
            7 => NamedKey::Numpad7,
            8 => NamedKey::Numpad8,
            9 => NamedKey::Numpad9,
            _ => unreachable!(),
        };
        let expected = vec![0x1b, b'?', b'0' + digit];
        assert_eq!(
            encode_key(&Key::Named(key), Modifiers::empty(), mode),
            expected,
            "VT52 app keypad digit {digit}"
        );
    }
}

#[test]
fn test_vt52_numpad_without_app_keypad_sends_digits() {
    let mode = KeyboardMode::VT52_MODE;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Numpad0), Modifiers::empty(), mode),
        b"0"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Numpad5), Modifiers::empty(), mode),
        b"5"
    );
}

#[test]
fn legacy_backspace_default_sends_del() {
    let del = encode_key(
        &Key::Named(NamedKey::Backspace),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(del, vec![0x7f]);
    let ctrl = encode_key(
        &Key::Named(NamedKey::Backspace),
        Modifiers::CTRL,
        KeyboardMode::empty(),
    );
    assert_eq!(ctrl, vec![0x08]);
}

#[test]
fn legacy_backspace_decbkm_sends_bs() {
    let mode = KeyboardMode::BACKARROW_SENDS_BS;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::empty(), mode),
        vec![0x08]
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::CTRL, mode),
        vec![0x7f]
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::ALT, mode),
        vec![0x1b, 0x08]
    );
}

#[test]
fn legacy_alt_no_esc_suppresses_esc_prefix() {
    let mode = KeyboardMode::ALT_NO_ESC;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::ALT, mode),
        vec![b'a']
    );
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL | Modifiers::ALT, mode),
        vec![0x03]
    );
}

#[test]
fn legacy_meta_sends_escape_on_prefixes_esc() {
    let mode = KeyboardMode::META_SENDS_ESC;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::META, mode),
        vec![0x1b, b'a']
    );
    assert_eq!(
        encode_key(
            &Key::Character('c'),
            Modifiers::CTRL | Modifiers::META,
            mode
        ),
        vec![0x1b, 0x03]
    );
}

#[test]
fn legacy_special_modifiers_off_strips_numlock() {
    let mode = KeyboardMode::NO_SPECIAL_MODIFIERS;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::NUM_LOCK, mode),
        vec![b'a']
    );
    assert_eq!(
        encode_key(
            &Key::Character('a'),
            Modifiers::NUM_LOCK | Modifiers::ALT,
            mode,
        ),
        vec![0x1b, b'a']
    );
}

#[test]
fn legacy_numpad_begin_is_the_xterm_letter_form_never_a_digit() {
    let begin = Key::Named(NamedKey::NumpadBegin);
    assert_eq!(
        encode_key(&begin, Modifiers::empty(), KeyboardMode::empty()),
        b"\x1b[E",
        "outside application keypad mode KP_Begin is CSI E"
    );
    assert_eq!(
        encode_key(&begin, Modifiers::empty(), KeyboardMode::APP_KEYPAD),
        b"\x1bOE"
    );
    assert_eq!(
        encode_key(&begin, Modifiers::SHIFT, KeyboardMode::APP_KEYPAD),
        b"\x1b[E"
    );
    assert_eq!(
        encode_key(&begin, Modifiers::ALT, KeyboardMode::empty()),
        b"\x1b\x1b[E"
    );
    assert_eq!(
        encode_key(
            &begin,
            Modifiers::empty(),
            KeyboardMode::APP_KEYPAD | KeyboardMode::VT52_MODE
        ),
        b"\x1b?5"
    );
    assert_eq!(
        encode_key(
            &begin,
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[E"
    );
}

#[test]
fn legacy_numpad_digit_under_app_keypad_ignores_num_lock() {
    let kp5 = Key::Named(NamedKey::Numpad5);
    assert_eq!(
        encode_key(&kp5, Modifiers::NUM_LOCK, KeyboardMode::APP_KEYPAD),
        b"\x1bOu",
        "NumLock must not demote DECKPAM's SS3 form to a digit"
    );
    assert_eq!(
        encode_key(&kp5, Modifiers::empty(), KeyboardMode::APP_KEYPAD),
        b"\x1bOu",
        "…and the lock bit changes nothing either way"
    );
    assert_eq!(
        encode_key(&kp5, Modifiers::NUM_LOCK, KeyboardMode::empty()),
        b"5",
        "with no application keypad mode the keypad still types its glyph"
    );
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::NUM_LOCK | Modifiers::SHIFT,
            KeyboardMode::APP_KEYPAD
        ),
        b"5"
    );
}
