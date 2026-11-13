// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::keyboard::KeyEventType;
use aterm_window::keyboard::{KeyCode, NamedKey as WinitNamed, PhysicalKey, SmolStr};

fn ch(c: &str) -> WinitKey {
    WinitKey::Character(SmolStr::new(c))
}

fn encode_press(
    logical: WinitKey,
    code: KeyCode,
    mods: Modifiers,
    mode: KeyboardMode,
) -> Option<Vec<u8>> {
    let ev = press_at(code, logical, aterm_window::keyboard::KeyLocation::Standard);
    let (key, mods, base, text) = build_key_input(&ev, mods, false)?;
    let bytes = keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text);
    (!bytes.is_empty()).then_some(bytes)
}

#[test]
fn a_press_encodes_to_its_terminal_bytes() {
    let none = KeyboardMode::empty();
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: [(
        &str,
        WinitKey,
        KeyCode,
        Modifiers,
        KeyboardMode,
        Option<&[u8]>,
    ); 8] = [
        (
            "alt+a",
            ch("a"),
            KeyCode::KeyA,
            Modifiers::ALT,
            none,
            Some(b"\x1ba"),
        ),
        (
            "ctrl+space",
            WinitKey::Named(WinitNamed::Space),
            KeyCode::Space,
            Modifiers::CTRL,
            none,
            Some(b"\x00"),
        ),
        (
            "ctrl+\\",
            ch("\\"),
            KeyCode::Backslash,
            Modifiers::CTRL,
            none,
            Some(b"\x1c"),
        ),
        (
            "a under disambiguate",
            ch("a"),
            KeyCode::KeyA,
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            Some(b"a"),
        ),
        (
            "ctrl+c",
            ch("c"),
            KeyCode::KeyC,
            Modifiers::CTRL,
            none,
            Some(b"\x03"),
        ),
        (
            "ctrl+d",
            ch("d"),
            KeyCode::KeyD,
            Modifiers::CTRL,
            none,
            Some(b"\x04"),
        ),
        (
            "a",
            ch("a"),
            KeyCode::KeyA,
            Modifiers::empty(),
            none,
            Some(b"a"),
        ),
        (
            "bare shift",
            WinitKey::Named(WinitNamed::Shift),
            KeyCode::ShiftLeft,
            Modifiers::SHIFT,
            none,
            None,
        ),
    ];
    for (case, logical, code, mods, mode, want) in cases {
        assert_eq!(
            encode_press(logical, code, mods, mode).as_deref(),
            want,
            "{case}"
        );
    }
}

#[test]
fn shift_symbol_encodes_shifted_glyph() {
    let ev = mac_press(KeyCode::Digit2, "@", "2", "@");
    let (key, mods, base, text) = build_key_input(&ev, Modifiers::SHIFT, false).expect("maps");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert_eq!(key, keyboard::Key::Character('2'));
        assert_eq!(text, None, "US Shift+2 is the engine's own '@'");
    }
    let bytes = keyboard::encode_key_with_text(
        &key,
        mods,
        KeyboardMode::empty(),
        KeyEventType::Press,
        base,
        text,
    );
    assert_eq!(bytes, b"@", "Shift+2 must emit '@', not '2'");
}

/// The Windows lock-state query (a real `GetKeyState` round-trip) returns only
/// the two lock bits.
#[cfg(windows)]
#[test]
fn lock_modifiers_reports_only_lock_bits() {
    let locks = lock_modifiers();
    assert!(
        (Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK).contains(locks),
        "lock_modifiers must set nothing beyond CAPS_LOCK/NUM_LOCK, got {locks:?}"
    );
}

fn de_de_shift_state(c: char) -> Option<u8> {
    match c {
        '{' | '[' | '@' | '€' | '}' | ']' | '\\' | '|' | '~' => Some(0b0000_0110),
        'a'..='z' | '0'..='9' => Some(0),
        'A'..='Z' => Some(1),
        _ => None,
    }
}

fn us_qwerty_shift_state(c: char) -> Option<u8> {
    match c {
        '{' | '@' | 'A'..='Z' | '}' | '|' | '~' => Some(1),
        '[' | 'a'..='z' | '0'..='9' | ']' | '\\' => Some(0),
        _ => None,
    }
}

#[test]
fn de_de_altgr_as_left_ctrl_alt_types_the_character() {
    for (composed, base, want) in [
        ("{", '7', &b"{"[..]),
        ("[", '8', &b"["[..]),
        ("@", 'q', &b"@"[..]),
    ] {
        let (key, mods, base_layout) = windows_key_input(
            &ch(composed),
            &ch(&base.to_string()),
            Some(base),
            Modifiers::CTRL | Modifiers::ALT,
            de_de_shift_state,
        )
        .expect("an AltGr-composed character maps");
        assert_eq!(
            key,
            keyboard::Key::Character(composed.chars().next().unwrap())
        );
        assert_eq!(
            mods,
            Modifiers::empty(),
            "AltGr's Ctrl+Alt spelling must not survive into the encoding of {composed}"
        );
        assert_eq!(base_layout, Some(base));
        let bytes = keyboard::encode_key_with_layout(
            &key,
            mods,
            KeyboardMode::empty(),
            KeyEventType::Press,
            base_layout,
        );
        assert_eq!(bytes, want, "de-DE AltGr must type {composed}");
    }
}

#[test]
fn de_de_right_alt_altgr_is_unchanged() {
    let (key, mods, _) = windows_key_input(
        &ch("{"),
        &ch("7"),
        Some('7'),
        Modifiers::empty(),
        de_de_shift_state,
    )
    .expect("the RAlt-composed character maps");
    assert_eq!(key, keyboard::Key::Character('{'));
    assert_eq!(mods, Modifiers::empty());
}

#[test]
fn genuine_ctrl_alt_letter_still_encodes_a_control_sequence() {
    let unidentified = WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified);
    for logical in [&unidentified, &ch("a")] {
        let (key, mods, base_layout) = windows_key_input(
            logical,
            &ch("a"),
            Some('a'),
            Modifiers::CTRL | Modifiers::ALT,
            de_de_shift_state,
        )
        .expect("a genuine Ctrl+Alt chord still resolves a key");
        assert_eq!(key, keyboard::Key::Character('a'));
        assert_eq!(
            mods,
            Modifiers::CTRL | Modifiers::ALT,
            "a real chord must keep its modifiers"
        );
        let bytes = keyboard::encode_key_with_layout(
            &key,
            mods,
            KeyboardMode::empty(),
            KeyEventType::Press,
            base_layout,
        );
        assert_eq!(bytes, b"\x1b\x01", "Ctrl+Alt+A must stay ESC ^A");
    }
}

#[test]
fn us_qwerty_is_unaffected() {
    let (_, mods, _) = windows_key_input(
        &ch("{"),
        &ch("["),
        Some('['),
        Modifiers::CTRL | Modifiers::ALT,
        us_qwerty_shift_state,
    )
    .expect("the chord resolves");
    assert_eq!(
        mods,
        Modifiers::CTRL | Modifiers::ALT,
        "US-QWERTY has no AltGr; nothing may be stripped"
    );
    let (key, mods, base_layout) = windows_key_input(
        &ch("{"),
        &ch("["),
        Some('['),
        Modifiers::SHIFT,
        us_qwerty_shift_state,
    )
    .expect("Shift+[ resolves");
    assert_eq!(key, keyboard::Key::Character('{'));
    assert_eq!(mods, Modifiers::SHIFT);
    assert_eq!(
        keyboard::encode_key_with_layout(
            &key,
            mods,
            KeyboardMode::empty(),
            KeyEventType::Press,
            base_layout,
        ),
        b"{"
    );
}

fn windows_bytes(
    logical: &str,
    text: &str,
    base: char,
    mods: Modifiers,
    locks: Modifiers,
    mode: KeyboardMode,
) -> Vec<u8> {
    let logical = ch(logical);
    let (key, mods, base_layout) =
        windows_key_input(&logical, &ch(&base.to_string()), Some(base), mods, |_| None)
            .expect("the press maps");
    let (mods, text) = windows_composed_text(&logical, &key, mods, Some(text));
    keyboard::encode_key_with_text(
        &key,
        mods | locks,
        mode,
        KeyEventType::Press,
        base_layout,
        text,
    )
}

#[test]
fn windows_non_us_shift_types_the_layouts_character() {
    let shift = Modifiers::SHIFT;
    let none = Modifiers::empty();
    let disambiguate = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    for mode in [
        KeyboardMode::empty(),
        KeyboardMode::WIN32_INPUT,
        disambiguate,
    ] {
        for (logical, base, want) in [
            ("/", '7', "/"),
            ("=", '0', "="),
            (";", ',', ";"),
            ("'", '\\', "'"),
            ("`", '=', "`"),
            ("1", '1', "1"),
            ("2", '2', "2"),
            ("Ф", 'a', "Ф"),
        ] {
            assert_eq!(
                windows_bytes(logical, logical, base, shift, none, mode),
                want.as_bytes(),
                "Shift+{base} composed {logical:?} under {mode:?}"
            );
        }
    }
    for (logical, base) in [("?", '/'), ("A", 'a'), ("@", '2'), ("{", '[')] {
        let key = keyboard::Key::Character(logical.chars().next().unwrap());
        assert_eq!(
            windows_composed_text(&ch(logical), &key, shift, Some(logical)),
            (shift, None),
            "US Shift+{base}"
        );
    }
}

#[test]
fn windows_caps_lock_with_shift_types_lowercase() {
    let caps = Modifiers::CAPS_LOCK;
    let mode = KeyboardMode::WIN32_INPUT;
    assert_eq!(
        windows_bytes("a", "a", 'a', Modifiers::SHIFT, caps, mode),
        b"a"
    );
    assert_eq!(
        windows_bytes("ф", "ф", 'a', Modifiers::SHIFT, caps, mode),
        "ф".as_bytes(),
        "a letter the US table leaves alone is still Caps Lock's to invert"
    );
    assert_eq!(
        windows_bytes("A", "A", 'a', Modifiers::empty(), caps, mode),
        b"A"
    );
}

#[test]
fn windows_chords_keep_the_composed_glyph() {
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    let none = KeyboardMode::empty();
    assert_eq!(
        windows_bytes("/", "/", '7', alt_shift, Modifiers::empty(), none),
        b"\x1b/"
    );
    assert_eq!(
        windows_bytes("1", "1", '1', alt_shift, Modifiers::empty(), none),
        b"\x1b1",
        "AZERTY Alt+Shift+&"
    );
    assert_eq!(
        windows_bytes("?", "?", '/', alt_shift, Modifiers::empty(), none),
        b"\x1b?",
        "US Alt+Shift+/"
    );
    let o = keyboard::Key::Character('O');
    assert_eq!(
        windows_composed_text(&ch("O"), &o, alt_shift, Some("O")),
        (alt_shift, None),
        "Alt+Shift+O keeps its Shift"
    );
    let unidentified = WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified);
    let all = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
    let (key, mods, _) = windows_key_input(
        &unidentified,
        &ch("7"),
        Some('7'),
        all,
        us_qwerty_shift_state,
    )
    .expect("the rescue maps");
    assert_eq!(key, keyboard::Key::Character('7'));
    assert_eq!(
        windows_composed_text(&unidentified, &key, mods, None),
        (all, None)
    );
}

#[test]
fn windows_dead_key_that_did_not_compose_types_both_characters() {
    let none = Modifiers::empty();
    assert!(windows_text_commit(&ch("s"), Some("'s"), none));
    assert!(windows_text_commit(&ch("S"), Some("´S"), Modifiers::SHIFT));
    assert!(windows_text_commit(&ch("´"), Some("´´"), none));
    assert!(!windows_text_commit(&ch("é"), Some("é"), none));
    assert!(!windows_text_commit(&ch("s"), Some("s"), none));
    assert!(!windows_text_commit(&ch("s"), Some("'s"), Modifiers::CTRL));
    assert!(!windows_text_commit(&ch("s"), Some("'s"), Modifiers::ALT));
    assert!(!windows_text_commit(
        &WinitKey::Named(WinitNamed::Enter),
        Some("´\r"),
        none
    ));
}

#[test]
fn windows_dead_key_then_space_types_the_accent() {
    let space = WinitKey::Named(WinitNamed::Space);
    let none = Modifiers::empty();
    assert!(windows_text_commit(&space, Some("^"), none));
    assert!(windows_text_commit(&space, Some("'"), Modifiers::SHIFT));
    assert!(
        !windows_text_commit(&space, Some(" "), none),
        "a plain Space"
    );
    assert!(!windows_text_commit(&space, None, none));
    assert!(!windows_text_commit(&space, Some("^"), Modifiers::CTRL));
    assert!(!windows_text_commit(&space, Some("^"), Modifiers::ALT));
    assert_eq!(
        windows_text_repeat(&space, Some("^"), none).as_deref(),
        Some(" ")
    );
    assert_eq!(windows_text_repeat(&space, Some(" "), none), None);
}

#[test]
fn windows_dead_key_that_did_not_compose_repeats_the_key_alone() {
    let none = Modifiers::empty();
    assert_eq!(
        windows_text_repeat(&ch("s"), Some("'s"), none).as_deref(),
        Some("s")
    );
    assert_eq!(
        windows_text_repeat(&ch("S"), Some("´S"), Modifiers::SHIFT).as_deref(),
        Some("S")
    );
    assert_eq!(windows_text_repeat(&ch("é"), Some("é"), none), None);
    assert_eq!(windows_text_repeat(&ch("s"), Some("s"), none), None);
    assert_eq!(
        windows_text_repeat(&ch("s"), Some("'s"), Modifiers::CTRL),
        None
    );
}

#[test]
fn windows_alt_code_digits_type_nothing() {
    use aterm_window::keyboard::{KeyLocation, NativeKey};
    let pad = KeyLocation::Numpad;
    let alt = Modifiers::ALT;
    let vk = |code: u16| WinitKey::Unidentified(NativeKey::Windows(code));
    assert!(windows_alt_code_digit(&vk(0x60), pad, alt));
    assert!(windows_alt_code_digit(&vk(0x65), pad, alt), "keypad 5");
    assert!(windows_alt_code_digit(
        &vk(0x69),
        pad,
        alt | Modifiers::NUM_LOCK
    ));
    assert!(windows_alt_code_digit(&ch("2"), pad, alt));
    assert!(!windows_alt_code_digit(&ch("5"), pad, Modifiers::empty()));
    assert!(!windows_alt_code_digit(
        &ch("5"),
        pad,
        alt | Modifiers::CTRL
    ));
    assert!(!windows_alt_code_digit(
        &ch("5"),
        pad,
        alt | Modifiers::SHIFT
    ));
    assert!(!windows_alt_code_digit(&vk(0x0c), pad, alt));
    assert!(!windows_alt_code_digit(
        &WinitKey::Named(WinitNamed::ArrowLeft),
        pad,
        alt
    ));
    assert!(!windows_alt_code_digit(&ch("+"), pad, alt));
    assert!(!windows_alt_code_digit(
        &ch("5"),
        KeyLocation::Standard,
        alt
    ));
}

/// The whole Windows builder on real winit events: the same three cases as
/// the pure halves above, through `build_key_input` itself.
#[cfg(windows)]
#[test]
fn windows_build_key_input_carries_the_layouts_text() {
    use aterm_window::keyboard::KeyLocation;
    let press = |code, logical: &str, text: &str| {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            ch(logical),
            Some(SmolStr::new(text)),
            KeyLocation::Standard,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    };
    let de_7 = press(KeyCode::Digit7, "/", "/");
    let (key, mods, _, text) = build_key_input(&de_7, Modifiers::SHIFT, false).expect("maps");
    assert_eq!(
        (key, mods, text),
        (keyboard::Key::Character('/'), Modifiers::SHIFT, Some('/'))
    );
    let dead = press(KeyCode::KeyS, "s", "'s");
    assert!(build_key_input(&dead, Modifiers::empty(), false).is_none());
    let kp = KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::Numpad2),
        ch("2"),
        None,
        KeyLocation::Numpad,
        aterm_window::event::ElementState::Pressed,
        false,
    );
    assert!(build_key_input(&kp, Modifiers::ALT, false).is_none());
    assert!(build_key_input(&kp, Modifiers::empty(), false).is_some());
}

#[test]
fn super_modifier_carried_through() {
    let m = modifiers_from_winit(ModifiersState::SUPER);
    assert!(m.contains(Modifiers::SUPER));
}

#[test]
fn commit_sends_committed_text() {
    assert_eq!(
        encode_committed_text("hi", KeyboardMode::empty()),
        b"hi".to_vec()
    );
    assert_eq!(
        encode_committed_text("日本", KeyboardMode::empty()),
        "日本".as_bytes().to_vec()
    );
    assert!(encode_committed_text("", KeyboardMode::empty()).is_empty());
}

#[test]
fn a_commit_is_a_paste_when_a_control_character_is_not_a_lone_key() {
    for paste in [
        "ls\n",
        "a\u{85}",
        "\u{9b}",
        "\u{85}",
        "\r\n",
        "\n\n",
        "\x1b\x1b",
        "a\t",
        "\x1b[201~",
    ] {
        assert!(
            committed_text_is_paste(paste),
            "{paste:?} is a paste: a C1 character, or a control beside more text"
        );
    }
    for key in ["\n", "\r", "\t", "\x08", "\x1b", "\x7f", "\x03", "\0"] {
        assert!(
            !committed_text_is_paste(key),
            "{key:?} alone is one key's worth and stays typed"
        );
    }
    for typed in ["", "日本", "é", "🙂", " ", "\u{200d}", "\u{2028}", "\u{a0}"] {
        assert!(!committed_text_is_paste(typed), "{typed:?} is typed text");
    }
}

#[test]
fn commit_under_report_all_with_text_is_one_key0_event() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC
        | KeyboardMode::REPORT_ASSOCIATED_TEXT
        | KeyboardMode::REPORT_EVENT_TYPES;
    assert_eq!(
        encode_committed_text("日本", mode),
        b"\x1b[0;1;26085:26412u".to_vec(),
        "one key-0 event for the whole commit, codepoints colon-joined"
    );
    assert_eq!(
        encode_committed_text("日", mode),
        b"\x1b[0;1;26085u".to_vec()
    );
}

#[test]
fn commit_key0_filters_control_codepoints() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    assert_eq!(
        encode_committed_text("\u{8}日", mode),
        b"\x1b[0;1;26085u".to_vec(),
        "control codepoints are dropped from the text field"
    );
    assert!(
        encode_committed_text("\u{8}\u{1b}", mode).is_empty(),
        "an all-control commit emits nothing"
    );
}

#[test]
fn commit_under_report_all_without_text_is_one_keyless_event() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_committed_text("日本", mode),
        b"\x1b[0u".to_vec(),
        "one IME commit is one keyless event"
    );
    assert!(
        encode_committed_text("\u{8}\u{1b}", mode).is_empty(),
        "an all-control commit remains silent"
    );
}

fn press_at(
    code: KeyCode,
    logical: WinitKey,
    location: aterm_window::keyboard::KeyLocation,
) -> KeyEvent {
    let text = match &logical {
        WinitKey::Character(s) => Some(s.clone()),
        _ => None,
    };
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(code),
        logical,
        text,
        location,
        aterm_window::event::ElementState::Pressed,
        false,
    )
}

fn seam_bytes(ev: &KeyEvent, mode: KeyboardMode) -> Vec<u8> {
    let (key, mods, base, text) =
        build_key_input(ev, Modifiers::empty(), false).expect("the key maps");
    keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text)
}

#[test]
fn numpad_digit_reaches_the_numpad_encoder() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let ev = press_at(KeyCode::Numpad5, ch("5"), KeyLocation::Numpad);
    let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_5 maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad5));
    assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"5");
    assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOu");
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[57404u"
    );
}

#[test]
fn shifted_keypad_digit_types_its_digit_never_the_main_rows_glyph() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let ev = press_at(KeyCode::Numpad5, ch("5"), KeyLocation::Numpad);
    let (key, mods, base, _) = build_key_input(&ev, Modifiers::SHIFT, false).expect("KP_5 maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad5));
    assert!(
        mods.contains(Modifiers::SHIFT),
        "the seam keeps the SHIFT bit"
    );
    let bytes =
        |mode| keyboard::encode_key_with_layout(&key, mods, mode, KeyEventType::Press, base);
    assert_eq!(
        bytes(KeyboardMode::REPORT_EVENT_TYPES),
        b"5",
        "a kitty reporting flag must not turn KP_5 into `%`"
    );
    assert_eq!(bytes(KeyboardMode::empty()), b"5");
    assert_eq!(
        bytes(KeyboardMode::APP_KEYPAD),
        b"5",
        "SHIFT cancels application keypad mode, as xterm has it"
    );
    assert_eq!(
        bytes(KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[57404;2u",
        "disambiguate still reports the keypad's own key, SHIFT bit included"
    );
}

#[test]
fn numpad_enter_is_distinguishable_from_enter() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let ev = press_at(
        KeyCode::NumpadEnter,
        WinitKey::Named(WinitNamed::Enter),
        KeyLocation::Numpad,
    );
    let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_Enter maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadEnter));
    assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"\r");
    assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOM");
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[57414u"
    );
}

#[test]
fn numlock_off_keypad_navigation_keeps_its_keypad_identity() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let ev = press_at(
        KeyCode::Numpad1,
        WinitKey::Named(WinitNamed::End),
        KeyLocation::Numpad,
    );
    let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_End maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadEnd));
    assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"\x1b[F");
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[57424u"
    );
}

#[test]
fn main_row_digit_is_unchanged() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let five = press_at(KeyCode::Digit5, ch("5"), KeyLocation::Standard);
    let (key, _, _, _) = build_key_input(&five, Modifiers::empty(), false).expect("5 maps");
    assert_eq!(key, keyboard::Key::Character('5'));
    assert_eq!(seam_bytes(&five, KeyboardMode::APP_KEYPAD), b"5");
    assert_eq!(
        seam_bytes(&five, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"5"
    );
    let enter = press_at(
        KeyCode::Enter,
        WinitKey::Named(WinitNamed::Enter),
        KeyLocation::Standard,
    );
    let (key, _, _, _) = build_key_input(&enter, Modifiers::empty(), false).expect("Enter maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::Enter));
    assert_eq!(seam_bytes(&enter, KeyboardMode::APP_KEYPAD), b"\r");
}

fn press_at_with_base(
    code: KeyCode,
    logical: WinitKey,
    base: WinitKey,
    location: aterm_window::keyboard::KeyLocation,
) -> KeyEvent {
    let text = match &logical {
        WinitKey::Character(s) => Some(s.clone()),
        _ => None,
    };
    KeyEvent::synthetic_for_test_with_base(
        PhysicalKey::Code(code),
        logical,
        base,
        text,
        location,
        aterm_window::event::ElementState::Pressed,
        false,
    )
}

#[test]
fn numlock_on_keypad_digit_is_not_the_level_zero_keysym() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::KeyLocation;
    let ev = press_at_with_base(
        KeyCode::Numpad1,
        ch("1"),
        WinitKey::Named(WinitNamed::End),
        KeyLocation::Numpad,
    );
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
        assert_eq!(
            ev.key_without_modifiers(),
            WinitKey::Named(WinitNamed::End),
            "the harness really does hand the builder a divergent base key"
        );
    }
    let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_1 maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad1));
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::empty()),
        b"1",
        "a NumLock-ON keypad 1 types 1, not End's ESC [ F"
    );
    assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOq");
    let dot = press_at_with_base(
        KeyCode::NumpadDecimal,
        ch("."),
        WinitKey::Named(WinitNamed::Delete),
        KeyLocation::Numpad,
    );
    assert_eq!(seam_bytes(&dot, KeyboardMode::empty()), b".");
    assert_eq!(crate::app_input::base_logical_key(&ev), ch("1"));
    let kp3 = press_at_with_base(
        KeyCode::Numpad3,
        ch("3"),
        WinitKey::Named(WinitNamed::PageDown),
        KeyLocation::Numpad,
    );
    assert_eq!(crate::app_input::base_logical_key(&kp3), ch("3"));
    let off = press_at_with_base(
        KeyCode::Numpad1,
        WinitKey::Named(WinitNamed::End),
        WinitKey::Named(WinitNamed::End),
        KeyLocation::Numpad,
    );
    assert_eq!(seam_bytes(&off, KeyboardMode::empty()), b"\x1b[F");
    assert_eq!(
        crate::app_input::base_logical_key(&off),
        WinitKey::Named(WinitNamed::End)
    );
    let shifted = press_at_with_base(KeyCode::Digit1, ch("!"), ch("1"), KeyLocation::Standard);
    assert_eq!(crate::app_input::base_logical_key(&shifted), ch("1"));
}

#[test]
fn numlock_off_keypad_centre_is_the_letter_form_never_a_digit() {
    use aterm_types::keyboard::NamedKey;
    use aterm_window::keyboard::{KeyLocation, NativeKey};
    let ev = press_at(
        KeyCode::Numpad5,
        WinitKey::Unidentified(NativeKey::Xkb(0xff9d)),
        KeyLocation::Numpad,
    );
    let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_Begin maps");
    assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadBegin));
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::empty()),
        b"\x1b[E",
        "the centre key must never type a digit into the shell"
    );
    assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOE");
    assert_eq!(
        seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[E"
    );
}

fn mac_press(code: KeyCode, logical: &str, base: &str, text: &str) -> KeyEvent {
    KeyEvent::synthetic_for_test_with_base(
        PhysicalKey::Code(code),
        ch(logical),
        ch(base),
        (!text.is_empty()).then(|| SmolStr::new(text)),
        aterm_window::keyboard::KeyLocation::Standard,
        aterm_window::event::ElementState::Pressed,
        false,
    )
}

/// The bytes the seam writes for a macOS-shaped press under `mods`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn mac_bytes(ev: &KeyEvent, mods: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    let (key, mods, base, text) = build_key_input(ev, mods, false).expect("the key maps");
    keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text)
}

/// Bug hunt 8: the window carries the layout's character only when the US
/// derivation from the unshifted key would type something else — so a US
/// press stays the event a controller builds — for a text press and, since
/// , a Ctrl or Alt chord.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn layout_text_rides_a_press_only_when_the_layout_differs() {
    let text = |ev: &KeyEvent, mods| build_key_input(ev, mods, false).expect("maps").3;
    assert_eq!(
        text(&mac_press(KeyCode::KeyA, "a", "a", "a"), Modifiers::empty()),
        None
    );
    assert_eq!(
        text(&mac_press(KeyCode::KeyA, "A", "a", "A"), Modifiers::SHIFT),
        None
    );
    assert_eq!(
        text(&mac_press(KeyCode::Digit2, "@", "2", "@"), Modifiers::SHIFT),
        None
    );
    assert_eq!(
        text(&mac_press(KeyCode::KeyA, "A", "a", "A"), Modifiers::empty()),
        Some('A')
    );
    assert_eq!(
        text(
            &mac_press(KeyCode::Digit2, "\"", "2", "\""),
            Modifiers::SHIFT
        ),
        Some('"')
    );
    assert_eq!(
        text(&mac_press(KeyCode::Digit1, "1", "&", "1"), Modifiers::SHIFT),
        Some('1')
    );
    assert_eq!(
        text(&mac_press(KeyCode::KeyA, "Ф", "ф", "Ф"), Modifiers::SHIFT),
        Some('Ф')
    );
    let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
    assert_eq!(text(&de_2, Modifiers::CTRL | Modifiers::SHIFT), Some('"'));
    assert_eq!(text(&de_2, Modifiers::ALT | Modifiers::SHIFT), Some('"'));
    assert_eq!(
        text(&mac_press(KeyCode::KeyA, "a", "a", "a"), Modifiers::ALT),
        None
    );
    let alt_a = mac_press(KeyCode::KeyA, "å", "a", "å");
    let composes = |ev: &KeyEvent, mods| build_key_input(ev, mods, true).expect("maps").3;
    assert_eq!(
        composes(&alt_a, Modifiers::ALT),
        None,
        "Option composed 'å'"
    );
    assert_eq!(
        composes(&de_2, Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT),
        Some('"'),
        "a Ctrl chord's text is read without Option"
    );
    assert_eq!(text(&de_2, Modifiers::SUPER | Modifiers::SHIFT), None);
    assert_eq!(
        text(&mac_press(KeyCode::KeyE, "e", "e", ""), Modifiers::empty()),
        None
    );
    let caps_b = mac_press(KeyCode::KeyB, "B", "b", "B");
    assert_eq!(text(&caps_b, Modifiers::ALT), None);
    assert_eq!(
        text(&mac_press(KeyCode::KeyC, "C", "c", "C"), Modifiers::CTRL),
        None
    );
    assert_eq!(
        text(&mac_press(KeyCode::KeyB, "И", "и", "И"), Modifiers::ALT),
        None
    );
    assert_eq!(
        text(
            &mac_press(KeyCode::KeyB, "b", "b", "b"),
            Modifiers::CTRL | Modifiers::SHIFT
        ),
        None
    );
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    for (press, mods, want, why) in [
        (("I", "ı"), Modifiers::ALT, None, "tr Caps Lock + Alt+ı"),
        (("İ", "i"), Modifiers::ALT, None, "tr Caps Lock + Alt+i"),
        (("I", "ı"), alt_shift, Some('I'), "tr Alt+Shift+ı"),
        (("İ", "i"), alt_shift, Some('İ'), "tr Alt+Shift+i"),
        (("И", "и"), alt_shift, Some('И'), "ru Alt+Shift+и"),
        (("Ü", "ü"), alt_shift, Some('Ü'), "de Alt+Shift+ü"),
        (
            ("и", "и"),
            alt_shift,
            Some('И'),
            "ru Shift under Caps Lock (xkb)",
        ),
        (("ẞ", "ß"), Modifiers::ALT, None, "de Caps Lock + Alt+ß"),
    ] {
        let (glyph, base) = press;
        let ev = mac_press(KeyCode::KeyI, glyph, base, glyph);
        assert_eq!(text(&ev, mods), want, "{why}");
    }
}

/// Caps Lock + A and Shift on a non-US layout type the layout's character, in
/// legacy mode and every kitty mode that keeps text as text; kitty's report-all
/// forms keep the layout's unshifted key as the key code.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn caps_lock_and_non_us_shift_type_the_layouts_character() {
    let disambiguate = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let alternates = disambiguate | KeyboardMode::REPORT_ALTERNATE_KEYS;
    for mode in [KeyboardMode::empty(), disambiguate, alternates] {
        let caps_a = mac_press(KeyCode::KeyA, "A", "a", "A");
        assert_eq!(
            mac_bytes(&caps_a, Modifiers::empty(), mode),
            b"A",
            "{mode:?}"
        );
        let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
        assert_eq!(mac_bytes(&de_2, Modifiers::SHIFT, mode), b"\"", "{mode:?}");
        let de_7 = mac_press(KeyCode::Digit7, "/", "7", "/");
        assert_eq!(mac_bytes(&de_7, Modifiers::SHIFT, mode), b"/", "{mode:?}");
        let azerty_1 = mac_press(KeyCode::Digit1, "1", "&", "1");
        assert_eq!(
            mac_bytes(&azerty_1, Modifiers::SHIFT, mode),
            b"1",
            "{mode:?}"
        );
        let azerty_2 = mac_press(KeyCode::Digit2, "2", "é", "2");
        assert_eq!(
            mac_bytes(&azerty_2, Modifiers::SHIFT, mode),
            b"2",
            "{mode:?}"
        );
        let ru_f = mac_press(KeyCode::KeyA, "Ф", "ф", "Ф");
        assert_eq!(mac_bytes(&ru_f, Modifiers::SHIFT, mode), "Ф".as_bytes());
    }
    let report_all_text =
        KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let de_7 = mac_press(KeyCode::Digit7, "/", "7", "/");
    assert_eq!(
        mac_bytes(&de_7, Modifiers::SHIFT, report_all_text),
        b"\x1b[55;2;47u",
        "kitty >24: key 7, text '/'"
    );
    let report_all_alternates =
        KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
    assert_eq!(
        mac_bytes(&de_2, Modifiers::SHIFT, report_all_alternates),
        b"\x1b[50:34;2u",
        "kitty >12: the shifted key is the layout's '\"'"
    );
}

/// A CHORD UNDER XKB'S CAPS LOCK TAKES SHIFT'S CASE. On X11 and
/// Wayland xkb cancels Shift against Caps Lock on an alphabetic key, so Turkish Caps
/// Lock + Alt+Shift+i arrives with the text `i`, and the chord took `i`'s default
/// capital, ESC `I`, where Alt+Shift+i without the lock is ESC `İ`.
#[cfg(target_os = "linux")]
#[test]
fn an_xkb_chord_under_caps_lock_takes_shifts_capital() {
    let none = KeyboardMode::empty();
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    let locked = mac_press(KeyCode::Quote, "i", "i", "i")
        .with_text_ignoring_lock_for_test(Some(SmolStr::new("İ")));
    assert_eq!(mac_bytes(&locked, alt_shift, none), "\x1bİ".as_bytes());
    let unlocked = mac_press(KeyCode::Quote, "İ", "i", "İ");
    assert_eq!(mac_bytes(&unlocked, alt_shift, none), "\x1bİ".as_bytes());
    let caps_alt = mac_press(KeyCode::Quote, "İ", "i", "İ")
        .with_text_ignoring_lock_for_test(Some(SmolStr::new("i")));
    assert_eq!(mac_bytes(&caps_alt, Modifiers::ALT, none), b"\x1bi");
    let caps_text = mac_press(KeyCode::Quote, "İ", "i", "İ")
        .with_text_ignoring_lock_for_test(Some(SmolStr::new("i")));
    assert_eq!(
        mac_bytes(&caps_text, Modifiers::empty(), none),
        "İ".as_bytes()
    );
}

/// Ctrl on a Cyrillic layout: the base key 'с' has no control byte, so the
/// physical key's US letter names it; kitty keeps reporting the layout's key.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn ctrl_on_a_cyrillic_layout_sends_the_control_byte() {
    let es = mac_press(KeyCode::KeyC, "с", "с", "с");
    let ve = mac_press(KeyCode::KeyD, "в", "в", "в");
    let none = KeyboardMode::empty();
    assert_eq!(mac_bytes(&es, Modifiers::CTRL, none), [0x03]);
    assert_eq!(mac_bytes(&ve, Modifiers::CTRL, none), [0x04]);
    assert_eq!(
        mac_bytes(&es, Modifiers::CTRL, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[1089;5u"
    );
}
