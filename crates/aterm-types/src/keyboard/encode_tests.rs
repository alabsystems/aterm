// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard encoding tests covering shift refinement, mode sweeps, kitty/keypad, and win32 input.

use super::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, conhost_holds_once_switched, encode_key,
    encode_key_with_event, encode_key_with_layout, encode_kitty, shifted_character, write_u32,
};

/// The US-QWERTY glyph Shift gives the key whose unshifted character is `c`, or `None`.
fn spec_shifted(c: char) -> Option<char> {
    Some(match c {
        'a'..='z' => c.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '`' => '~',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => return None,
    })
}

/// The engine's shift map equals the independent spec for every shiftable key.
#[test]
fn shifted_character_refines_independent_spec() {
    for b in 0u8..=127 {
        let c = b as char;
        if let Some(want) = spec_shifted(c) {
            assert_eq!(
                shifted_character(c, Modifiers::SHIFT),
                Some(want),
                "shift map diverges from the ANSI spec at {c:?}"
            );
        }
    }
}

/// Shift changes every shiftable key's legacy bytes (`shift_is_effective.smt2`'s twin).
#[test]
fn legacy_shift_changes_every_shiftable_key() {
    for b in 0u8..=127 {
        let c = b as char;
        if spec_shifted(c).is_some() {
            let shifted = encode_key(&Key::Character(c), Modifiers::SHIFT, KeyboardMode::empty());
            let plain = encode_key(
                &Key::Character(c),
                Modifiers::empty(),
                KeyboardMode::empty(),
            );
            assert_ne!(
                shifted, plain,
                "Shift on a shiftable key {c:?} must not emit the unshifted byte"
            );
        }
    }
}

/// `write_u32` renders every magnitude, zero and `u32::MAX` included (#2775).
#[test]
fn write_u32_renders_every_magnitude() {
    for (val, want) in [
        (0, "0"),
        (1, "1"),
        (97, "97"),
        (999_999_999, "999999999"),
        (2_000_000_000, "2000000000"),
        (u32::MAX, "4294967295"),
    ] {
        let mut buf = Vec::new();
        write_u32(&mut buf, val);
        assert_eq!(buf, want.as_bytes(), "write_u32({val})");
    }
}

/// The `KeyboardMode` bits the sweeps enumerate, 0..=14 (2^15 modes); WIN32_INPUT, bit 15, has
/// its own tests below.
const KEYBOARD_MODE_DEFINED_BITS: u32 = 15;

fn encode_enter(mode: KeyboardMode, mods: Modifiers) -> Vec<u8> {
    encode_key(&Key::Named(NamedKey::Enter), mods, mode)
}

/// In every swept mode Shift+Enter differs from plain Enter, and neither encodes to nothing.
#[test]
fn shift_enter_distinct_from_enter_in_every_mode() {
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits);
        let plain = encode_enter(mode, Modifiers::empty());
        let shift = encode_enter(mode, Modifiers::SHIFT);
        assert_ne!(
            plain, shift,
            "Shift+Enter must differ from plain Enter — mode={mode:?} bits={bits:#06x}"
        );
        assert!(
            !plain.is_empty() && !shift.is_empty(),
            "Enter must always encode >= 1 byte — mode={mode:?}"
        );
    }
}

/// The key grid: one representative per encoder path.
fn release_obligation_keys() -> Vec<Key> {
    vec![
        Key::Character('a'),
        Key::Character('1'),
        Key::Named(NamedKey::Enter),
        Key::Named(NamedKey::Tab),
        Key::Named(NamedKey::Backspace),
        Key::Named(NamedKey::Space),
        Key::Named(NamedKey::Escape),
        Key::Named(NamedKey::ArrowUp),
        Key::Named(NamedKey::F5),
        Key::Named(NamedKey::ShiftLeft),
    ]
}

/// THEOREM (silence): in EVERY mode without REPORT_EVENT_TYPES — all 2^14 of
/// them — a key release encodes to NOTHING, for every key and chord in the
/// grid.
#[test]
fn release_encodes_to_nothing_without_report_event_types() {
    let mods_grid = [
        Modifiers::empty(),
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::CTRL | Modifiers::SHIFT,
    ];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits);
        if mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
            continue;
        }
        for key in release_obligation_keys() {
            for mods in mods_grid {
                let released = encode_key_with_event(&key, mods, mode, KeyEventType::Release);
                assert!(
                    released.is_empty(),
                    "release must encode to NOTHING without REPORT_EVENT_TYPES — \
                     key={key:?} mods={mods:?} mode={mode:?} got={released:?}"
                );
            }
        }
    }
}

/// Without REPORT_ALL_KEYS_AS_ESC, keys whose press is plain UTF-8 (bare and Shift-only
/// characters and Space) and the recovery keys Enter/Tab/Backspace (any modifiers) have no
/// release, even under REPORT_EVENT_TYPES; report-all opts them in.
#[test]
fn text_and_recovery_key_releases_require_report_all_keys() {
    let text_keys = [Key::Character('a'), Key::Named(NamedKey::Space)];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits);
        if !mode.contains(KeyboardMode::REPORT_EVENT_TYPES)
            || mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        {
            continue;
        }
        for key in &text_keys {
            for mods in [Modifiers::empty(), Modifiers::SHIFT] {
                let released = encode_key_with_event(key, mods, mode, KeyEventType::Release);
                assert!(
                    released.is_empty(),
                    "text release requires REPORT_ALL_KEYS_AS_ESC — \
                     key={key:?} mods={mods:?} mode={mode:?} got={released:?}"
                );
            }
        }
        for key in [NamedKey::Enter, NamedKey::Tab, NamedKey::Backspace] {
            for mods in [
                Modifiers::empty(),
                Modifiers::SHIFT,
                Modifiers::CTRL,
                Modifiers::ALT,
                Modifiers::CTRL | Modifiers::SHIFT,
            ] {
                let released =
                    encode_key_with_event(&Key::Named(key), mods, mode, KeyEventType::Release);
                assert!(
                    released.is_empty(),
                    "recovery-key release requires REPORT_ALL_KEYS_AS_ESC — \
                     key={key:?} mods={mods:?} mode={mode:?} got={released:?}"
                );
            }
        }
    }
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES
                | KeyboardMode::REPORT_EVENT_TYPES
                | KeyboardMode::REPORT_ALL_KEYS_AS_ESC,
            KeyEventType::Release,
        ),
        b"\x1b[13;1:3u",
        "report-all opts Enter releases in"
    );
}

/// In every mode a reported release differs from the same key's press.
#[test]
fn reported_release_is_never_byte_identical_to_press() {
    let mods_grid = [Modifiers::empty(), Modifiers::SHIFT, Modifiers::CTRL];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits);
        for key in release_obligation_keys() {
            for mods in mods_grid {
                let released = encode_key_with_event(&key, mods, mode, KeyEventType::Release);
                if released.is_empty() {
                    continue;
                }
                let pressed = encode_key_with_event(&key, mods, mode, KeyEventType::Press);
                assert_ne!(
                    released, pressed,
                    "a reported release must not read as a press — \
                     key={key:?} mods={mods:?} mode={mode:?}"
                );
            }
        }
    }
}

/// Negative control: the raw CSI-u encoder cannot tell a disambiguate-only release from a press
/// (the `:3` subfield needs REPORT_EVENT_TYPES), so only the gate keeps releases silent.
#[test]
fn ungated_release_would_collide_with_a_press_report() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let ungated_release = encode_kitty(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Release,
        None,
        None,
    );
    let press_report = encode_kitty(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Press,
        None,
        None,
    );
    assert_eq!(ungated_release, b"\x1b[97u");
    assert_eq!(
        ungated_release, press_report,
        "the CSI-u encoder cannot mark releases without REPORT_EVENT_TYPES; \
         only the should_encode_kitty_event gate keeps them silent"
    );
}

/// Kitty flag combinations that keep text as text: the non-empty subsets of the four kitty
/// bits other than REPORT_ALL_KEYS_AS_ESC.
fn kitty_text_preserving_modes() -> Vec<KeyboardMode> {
    let kitty_bits = [
        KeyboardMode::DISAMBIGUATE_ESC_CODES,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyboardMode::REPORT_ALTERNATE_KEYS,
        KeyboardMode::REPORT_ASSOCIATED_TEXT,
    ];
    (1u16..(1 << kitty_bits.len()))
        .map(|bits| {
            kitty_bits
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .fold(KeyboardMode::empty(), |acc, (_, flag)| acc | *flag)
        })
        .collect()
}

/// THEOREM (text refinement): for every kitty mode without report-all-keys,
/// every text-producing PRESS and REPEAT — plain and shift-only characters,
/// Space, unmodified Enter/Tab/Backspace, and Shift+Backspace — encodes
/// byte-identically to the pure-legacy encoding of the same event.
#[test]
fn kitty_text_events_encode_exactly_like_legacy() {
    let text_events: Vec<(Key, Modifiers)> = vec![
        (Key::Character('a'), Modifiers::empty()),
        (Key::Character('a'), Modifiers::SHIFT),
        (Key::Character('2'), Modifiers::SHIFT),
        (Key::Character('a'), Modifiers::CAPS_LOCK),
        (Key::Named(NamedKey::Space), Modifiers::empty()),
        (Key::Named(NamedKey::Space), Modifiers::SHIFT),
        (Key::Named(NamedKey::Enter), Modifiers::empty()),
        (Key::Named(NamedKey::Tab), Modifiers::empty()),
        (Key::Named(NamedKey::Backspace), Modifiers::empty()),
        (Key::Named(NamedKey::Backspace), Modifiers::SHIFT),
    ];
    for kitty_mode in kitty_text_preserving_modes() {
        for extra in [
            KeyboardMode::empty(),
            KeyboardMode::BACKARROW_SENDS_BS,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
        ] {
            let mode = kitty_mode | extra;
            let legacy_mode = mode
                - KeyboardMode::KITTY_PROTOCOL_FLAGS
                - KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1
                - KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2
                - KeyboardMode::XTERM_FORMAT_OTHER_KEYS;
            for (key, mods) in &text_events {
                for event in [KeyEventType::Press, KeyEventType::Repeat] {
                    let got = encode_key_with_event(key, *mods, mode, event);
                    let legacy = encode_key_with_event(key, *mods, legacy_mode, event);
                    assert_eq!(
                        got, legacy,
                        "text event must keep its legacy bytes — \
                         key={key:?} mods={mods:?} mode={mode:?} event={event:?}"
                    );
                    assert!(!got.is_empty(), "text press must never be silent");
                }
            }
        }
    }
}

#[test]
fn a_modifier_release_carries_its_bit_only_as_the_caller_states_it() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_EVENT_TYPES;
    for (key, flag, code) in [
        (NamedKey::ShiftLeft, Modifiers::SHIFT, 57441u32),
        (NamedKey::ShiftRight, Modifiers::SHIFT, 57447),
        (NamedKey::ControlRight, Modifiers::CTRL, 57448),
        (NamedKey::AltLeft, Modifiers::ALT, 57443),
        (NamedKey::SuperRight, Modifiers::SUPER, 57450),
    ] {
        let k = Key::Named(key);
        let bit = flag.kitty_encoded();
        assert_eq!(
            encode_key_with_event(&k, Modifiers::empty(), mode, KeyEventType::Press),
            format!("\x1b[{code};{bit}u").into_bytes(),
            "{key:?} press"
        );
        assert_eq!(
            encode_key_with_event(&k, flag, mode, KeyEventType::Release),
            format!("\x1b[{code};{bit}:3u").into_bytes(),
            "{key:?} release, other side held"
        );
        assert_eq!(
            encode_key_with_event(&k, Modifiers::empty(), mode, KeyEventType::Release),
            format!("\x1b[{code};1:3u").into_bytes(),
            "{key:?} release, nothing left held"
        );
        assert_eq!(key.modifier_twin().map(|(f, _)| f), Some(flag));
        let (_, twin) = key.modifier_twin().expect("sided");
        assert_eq!(twin.modifier_twin(), Some((flag, key)), "the twin's twin");
    }
    assert_eq!(NamedKey::CapsLock.modifier_twin(), None);
    assert_eq!(NamedKey::Enter.modifier_twin(), None);
}

#[test]
fn modifier_keys_silent_in_every_mode_without_report_all() {
    let modifier_keys = [
        NamedKey::ShiftLeft,
        NamedKey::ShiftRight,
        NamedKey::ControlLeft,
        NamedKey::ControlRight,
        NamedKey::AltLeft,
        NamedKey::AltRight,
        NamedKey::SuperLeft,
        NamedKey::SuperRight,
        NamedKey::HyperLeft,
        NamedKey::HyperRight,
        NamedKey::MetaLeft,
        NamedKey::MetaRight,
        NamedKey::CapsLock,
        NamedKey::ScrollLock,
        NamedKey::NumLock,
    ];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits);
        if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
            continue;
        }
        for key in modifier_keys {
            for event in [
                KeyEventType::Press,
                KeyEventType::Repeat,
                KeyEventType::Release,
            ] {
                let got = encode_key_with_event(&Key::Named(key), Modifiers::SHIFT, mode, event);
                assert!(
                    got.is_empty(),
                    "bare {key:?} {event:?} must be silent without report-all — \
                     mode={mode:?} got={got:?}"
                );
            }
        }
    }
}

#[test]
fn functional_form_witnesses() {
    let disamb = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;
    let report_all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;

    assert_eq!(
        encode_key(&Key::Named(NamedKey::F3), Modifiers::empty(), disamb),
        b"\x1b[13~"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F3), Modifiers::SHIFT, disamb),
        b"\x1b[13;2~"
    );

    assert_eq!(
        encode_key(&Key::Named(NamedKey::F10), Modifiers::SHIFT, disamb),
        b"\x1b[21;2~"
    );
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::F3),
            Modifiers::empty(),
            disamb | events_only,
            KeyEventType::Release,
        ),
        b"\x1b[13;1:3~"
    );

    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::ArrowUp),
            Modifiers::empty(),
            report_all
        ),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::ArrowUp),
            Modifiers::empty(),
            report_all | events_only,
            KeyEventType::Release,
        ),
        b"\x1b[1;1:3A"
    );

    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadBegin),
            Modifiers::empty(),
            disamb
        ),
        b"\x1b[E"
    );

    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::NumpadArrowUp),
            Modifiers::empty(),
            events_only,
            KeyEventType::Release,
        ),
        b"\x1b[1;1:3A"
    );
    assert!(
        encode_key_with_event(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::empty(),
            events_only,
            KeyEventType::Release,
        )
        .is_empty(),
        "folded NumpadEnter inherits Enter's release escape hatch"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::empty(),
            disamb
        ),
        b"\x1b[57414u"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadArrowUp),
            Modifiers::empty(),
            disamb
        ),
        b"\x1b[57419u"
    );
}

/// The kitty protocol supersedes xterm modifyOtherKeys when an app sets both.
#[test]
fn kitty_flags_suppress_modify_other_keys() {
    let both = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::SHIFT, both),
        b"A"
    );
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::CTRL, both),
        b"\x1b[97;5u",
        "chords take the kitty form, not CSI 27;5;97~"
    );
    let mok_only = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::SHIFT, mok_only),
        b"\x1b[27;2;65~"
    );
}

/// Mode `1|2|4` (disambiguate, event types, alternate keys), as Codex pushes it: presses and
/// repeats keep their text bytes, and text and recovery-key releases are silent.
#[test]
fn codex_progressive_keyboard_mode_witnesses() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        | KeyboardMode::REPORT_EVENT_TYPES
        | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let a = Key::Character('a');
    let enter = Key::Named(NamedKey::Enter);
    let none = Modifiers::empty();

    assert_eq!(
        encode_key_with_event(&a, none, mode, KeyEventType::Press),
        b"a"
    );
    assert_eq!(
        encode_key_with_event(&enter, none, mode, KeyEventType::Press),
        b"\r"
    );
    assert_eq!(
        encode_key_with_event(&a, none, mode, KeyEventType::Repeat),
        b"a"
    );
    assert_eq!(
        encode_key_with_event(&enter, none, mode, KeyEventType::Repeat),
        b"\r"
    );
    assert!(encode_key_with_event(&a, none, mode, KeyEventType::Release).is_empty());
    assert!(encode_key_with_event(&enter, none, mode, KeyEventType::Release).is_empty());
    assert_eq!(
        encode_key_with_event(&a, Modifiers::CTRL, mode, KeyEventType::Press),
        b"\x1b[97;5u"
    );
    assert_eq!(
        encode_key_with_event(&a, Modifiers::CTRL, mode, KeyEventType::Release),
        b"\x1b[97;5:3u"
    );
}

/// Shift+Enter in the modes apps actually use.
#[test]
fn shift_enter_concrete_witnesses() {
    assert_eq!(
        encode_enter(KeyboardMode::empty(), Modifiers::empty()),
        vec![0x0d]
    );
    assert_eq!(
        encode_enter(KeyboardMode::empty(), Modifiers::SHIFT),
        vec![0x0a]
    );
    assert_eq!(encode_enter(KeyboardMode::empty(), Modifiers::CTRL), b"\r");
    let kitty = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_eq!(encode_enter(kitty, Modifiers::empty()), vec![0x0d]);
    assert_eq!(encode_enter(kitty, Modifiers::SHIFT), b"\x1b[13;2u");
    let mok2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(encode_enter(mok2, Modifiers::SHIFT), b"\x1b[27;2;13~");
}

/// `Key::main_block_twin`, the one keypad fold: every keypad key has a twin except KP_Begin,
/// and a main-block key never folds.
#[test]
fn main_block_twin_table() {
    use Key::{Character, Named};
    use NamedKey as N;
    let twins = [
        (N::Numpad0, Character('0')),
        (N::Numpad5, Character('5')),
        (N::Numpad9, Character('9')),
        (N::NumpadDecimal, Character('.')),
        (N::NumpadSeparator, Character(',')),
        (N::NumpadDivide, Character('/')),
        (N::NumpadMultiply, Character('*')),
        (N::NumpadSubtract, Character('-')),
        (N::NumpadAdd, Character('+')),
        (N::NumpadEqual, Character('=')),
        (N::NumpadEnter, Named(N::Enter)),
        (N::NumpadArrowUp, Named(N::ArrowUp)),
        (N::NumpadArrowDown, Named(N::ArrowDown)),
        (N::NumpadArrowLeft, Named(N::ArrowLeft)),
        (N::NumpadArrowRight, Named(N::ArrowRight)),
        (N::NumpadHome, Named(N::Home)),
        (N::NumpadEnd, Named(N::End)),
        (N::NumpadPageUp, Named(N::PageUp)),
        (N::NumpadPageDown, Named(N::PageDown)),
        (N::NumpadInsert, Named(N::Insert)),
        (N::NumpadDelete, Named(N::Delete)),
    ];
    for (keypad, twin) in twins {
        assert_eq!(Named(keypad).main_block_twin(), Some(twin), "{keypad:?}");
    }
    assert_eq!(Named(N::NumpadBegin).main_block_twin(), None);
    for main in [Character('5'), Named(N::Enter), Named(N::End), Named(N::F5)] {
        assert_eq!(main.main_block_twin(), None, "{main:?} is not a keypad key");
    }
}

/// Under a kitty mode that has not asked to tell the keypad apart, a KP_5 (`Numpad5`) press or
/// repeat is the `5` kitty sends.
#[test]
fn kitty_legacy_folds_keypad_digits_to_their_glyph() {
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;
    let kp5 = Key::Named(NamedKey::Numpad5);
    assert_eq!(encode_key(&kp5, Modifiers::empty(), events_only), b"5");
    assert_eq!(
        encode_key_with_event(&kp5, Modifiers::empty(), events_only, KeyEventType::Repeat),
        b"5",
        "a keypad digit repeat is text under kitty legacy semantics"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadAdd),
            Modifiers::empty(),
            events_only
        ),
        b"+"
    );
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[57404u"
    );
    assert_eq!(
        encode_key(&kp5, Modifiers::empty(), KeyboardMode::APP_KEYPAD),
        b"\x1bOu",
        "no kitty flag: the legacy encoder's DECKPAM form is untouched"
    );
}

/// DECKPAM outranks the kitty legacy fold: application keypad mode is the legacy spelling of
/// "tell the keypad apart".
#[test]
fn kitty_legacy_fold_yields_to_application_keypad_mode() {
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;
    let kp5 = Key::Named(NamedKey::Numpad5);
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::empty(),
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"\x1bOu",
        "a kitty flag must not launder DECKPAM into plain text"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::empty(),
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"\x1bOM"
    );
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::SHIFT,
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"5"
    );
    assert_eq!(encode_key(&kp5, Modifiers::empty(), events_only), b"5");
}

/// `REPORT_ASSOCIATED_TEXT` carries a keypad glyph key's glyph (from its twin); the code stays
/// the keypad's own.
#[test]
fn kitty_associated_text_carries_the_keypad_glyph() {
    let full = KeyboardMode::DISAMBIGUATE_ESC_CODES
        | KeyboardMode::REPORT_EVENT_TYPES
        | KeyboardMode::REPORT_ALTERNATE_KEYS
        | KeyboardMode::REPORT_ALL_KEYS_AS_ESC
        | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Numpad5), Modifiers::empty(), full),
        b"\x1b[57404;1;53u",
        "KP_5 reports the text `5` alongside its own key code"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadDecimal),
            Modifiers::empty(),
            full
        ),
        b"\x1b[57409;1;46u"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadSeparator),
            Modifiers::empty(),
            full
        ),
        b"\x1b[57416;1;44u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadEnd), Modifiers::empty(), full),
        b"\x1b[57424u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadBegin), Modifiers::empty(), full),
        b"\x1b[E",
        "KP_Begin keeps its legacy letter form in every kitty mode"
    );
    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::empty(), full),
        b"\x1b[53;1;53u"
    );
}

/// The spacebar's associated text is a space: kitty's one exclusion is control codes (below
/// U+0020, and C0/C1), which drops Enter, Tab and Backspace but not U+0020.
#[test]
fn kitty_associated_text_carries_the_spacebars_space() {
    let report_all_text = KeyboardMode::REPORT_ALL_KEYS_AS_ESC
        | KeyboardMode::REPORT_ASSOCIATED_TEXT
        | KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let full =
        report_all_text | KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::REPORT_ALTERNATE_KEYS;

    for mode in [report_all_text, full] {
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), Modifiers::empty(), mode),
            b"\x1b[32;1;32u",
            "the spacebar reports the space it typed alongside its key code"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), Modifiers::SHIFT, mode),
            b"\x1b[32;2;32u"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode),
            b"\x1b[13u",
            "Enter types `\\r`, a control code"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Tab), Modifiers::empty(), mode),
            b"\x1b[9u"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Backspace), Modifiers::empty(), mode),
            b"\x1b[127u"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), Modifiers::CTRL, mode),
            b"\x1b[32;5u"
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), Modifiers::ALT, mode),
            b"\x1b[32;3u"
        );
    }

    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::Space),
            Modifiers::empty(),
            full,
            KeyEventType::Release
        ),
        b"\x1b[32;1:3u"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Space),
            Modifiers::empty(),
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[32u"
    );
}

/// A keypad key does not compose, and a kitty reporting flag cannot make it: under
/// `REPORT_EVENT_TYPES` Shift+KP_5 and Ctrl+KP_5 are `5`, not the main row's `%` and 0x1D.
#[test]
fn a_modified_keypad_key_never_composes_the_main_rows_glyph() {
    use NamedKey as N;
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;

    for (named, want) in [
        (N::Numpad0, &b"0"[..]),
        (N::Numpad1, b"1"),
        (N::Numpad5, b"5"),
        (N::Numpad9, b"9"),
        (N::NumpadDecimal, b"."),
        (N::NumpadSeparator, b","),
        (N::NumpadDivide, b"/"),
        (N::NumpadSubtract, b"-"),
    ] {
        assert_eq!(
            encode_key(&Key::Named(named), Modifiers::SHIFT, events_only),
            want,
            "{named:?} under SHIFT"
        );
    }

    for (named, mods) in [
        (N::Numpad1, Modifiers::SHIFT),
        (N::Numpad5, Modifiers::SHIFT),
        (N::Numpad5, Modifiers::CTRL),
        (N::Numpad2, Modifiers::CTRL),
        (N::NumpadDivide, Modifiers::CTRL),
        (N::NumpadSubtract, Modifiers::CTRL | Modifiers::SHIFT),
        (N::NumpadEnd, Modifiers::SHIFT),
        (N::NumpadArrowUp, Modifiers::CTRL),
        (N::NumpadEnter, Modifiers::SHIFT),
    ] {
        let key = Key::Named(named);
        assert_eq!(
            encode_key(&key, mods, events_only),
            encode_key(&key, mods, KeyboardMode::empty()),
            "{named:?}+{mods:?}: a kitty REPORTING flag must not change what the keypad types",
        );
    }

    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::SHIFT, events_only),
        b"%"
    );
    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::CTRL, events_only),
        b"\x1d"
    );

    assert_eq!(
        encode_key(&Key::Named(N::Numpad5), Modifiers::empty(), events_only),
        b"5"
    );
    assert_eq!(
        encode_key(&Key::Named(N::NumpadAdd), Modifiers::empty(), events_only),
        b"+"
    );

    assert_eq!(
        encode_key(&Key::Named(N::NumpadDivide), Modifiers::CTRL, events_only),
        b"/"
    );
    assert_eq!(
        encode_key(
            &Key::Named(N::NumpadDivide),
            Modifiers::CTRL,
            KeyboardMode::empty()
        ),
        b"/"
    );

    assert_eq!(
        encode_key_with_event(
            &Key::Named(N::Numpad5),
            Modifiers::CTRL | Modifiers::SHIFT,
            events_only,
            KeyEventType::Release,
        ),
        b"\x1b[53;6:3u",
        "the kitty report keeps the folded code and the unmodified modifiers"
    );

    assert_eq!(
        encode_key(
            &Key::Named(N::Numpad5),
            Modifiers::empty(),
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"\x1bOu"
    );
    assert_eq!(
        encode_key(
            &Key::Named(N::Numpad5),
            Modifiers::CTRL,
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"\x1bOu",
        "only SHIFT cancels application keypad mode; CTRL leaves the SS3 form"
    );
}

/// The key-down/key-up record pair `CSI vk;sc;uc;Kd;cs;1 _` — the oracle the
/// assertions below compare against, written out from the spec rather than
/// from the encoder's own builder.
fn win32_pair(vk: u32, sc: u32, uc: u32, cs: u32) -> Vec<u8> {
    format!("\x1b[{vk};{sc};{uc};1;{cs};1_\x1b[{vk};{sc};{uc};0;{cs};1_").into_bytes()
}

/// The pair for `VK_RETURN` (scan 28) with `UnicodeChar = uc` and
/// `dwControlKeyState = cs`.
fn win32_enter_pair(uc: u32, cs: u32) -> Vec<u8> {
    win32_pair(13, 28, uc, cs)
}

const WIN32: KeyboardMode = KeyboardMode::WIN32_INPUT;

#[test]
fn win32_input_is_not_a_kitty_flag() {
    assert!(!KeyboardMode::KITTY_PROTOCOL_FLAGS.contains(WIN32));
    assert!(!WIN32.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS));
    assert!(!KeyboardMode::OPENCONSOLE_HOST.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS));
    assert_eq!(WIN32.bits(), 1 << 15);
    assert_eq!(KeyboardMode::OPENCONSOLE_HOST.bits(), 1 << 16);
}

#[test]
fn win32_enter_chords_are_record_pairs_and_plain_enter_stays_legacy() {
    let enter = Key::Named(NamedKey::Enter);
    for (mods, want) in [
        (Modifiers::SHIFT, win32_enter_pair(13, 0x10)),
        (Modifiers::CTRL, win32_enter_pair(10, 0x08)),
        (
            Modifiers::CTRL | Modifiers::SHIFT,
            win32_enter_pair(10, 0x18),
        ),
        (
            Modifiers::SHIFT | Modifiers::ALT,
            win32_enter_pair(13, 0x12),
        ),
        (Modifiers::CTRL | Modifiers::ALT, win32_enter_pair(10, 0x0a)),
        (
            Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
            win32_enter_pair(10, 0x1a),
        ),
        (Modifiers::empty(), b"\r".to_vec()),
        (Modifiers::ALT, b"\x1b\r".to_vec()),
        (Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK, b"\r".to_vec()),
    ] {
        assert_eq!(encode_key(&enter, mods, WIN32), want, "{mods:?}+Enter");
    }
    assert_eq!(
        encode_key(&enter, Modifiers::SHIFT, WIN32),
        b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_"
    );
}

#[test]
fn win32_release_emits_nothing_and_repeat_emits_the_pair() {
    assert!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            WIN32,
            KeyEventType::Release,
        )
        .is_empty()
    );
    assert!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::CTRL,
            WIN32,
            KeyEventType::Release,
        )
        .is_empty()
    );
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            WIN32,
            KeyEventType::Repeat,
        ),
        win32_enter_pair(13, 0x10)
    );
}

#[test]
fn win32_outranks_a_negotiated_kitty_mode() {
    let disambiguate = WIN32 | KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::SHIFT, disambiguate),
        win32_enter_pair(13, 0x10)
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::CTRL, disambiguate),
        win32_enter_pair(10, 0x08)
    );
    let report_all = WIN32 | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::SHIFT, report_all),
        win32_enter_pair(13, 0x10)
    );
    let events_only = WIN32 | KeyboardMode::REPORT_EVENT_TYPES;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::SHIFT, events_only),
        win32_enter_pair(13, 0x10)
    );
    assert!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            events_only,
            KeyEventType::Release,
        )
        .is_empty()
    );
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::empty(),
            events_only,
            KeyEventType::Release,
        ),
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::empty(),
            KeyboardMode::REPORT_EVENT_TYPES,
            KeyEventType::Release,
        )
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[13;2u"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            KeyboardMode::REPORT_EVENT_TYPES
        ),
        b"\n"
    );
}

#[test]
fn win32_outranks_xterm_modify_other_keys() {
    let l2 = WIN32 | KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::SHIFT, l2),
        win32_enter_pair(13, 0x10)
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2
        ),
        b"\x1b[27;2;13~"
    );
}

#[test]
fn win32_keys_conhost_still_reads_keep_their_bytes() {
    let cases: &[(Key, Modifiers)] = &[
        (Key::Named(NamedKey::Tab), Modifiers::SHIFT),
        (Key::Named(NamedKey::Tab), Modifiers::CTRL),
        (Key::Named(NamedKey::Tab), Modifiers::ALT),
        (Key::Named(NamedKey::Backspace), Modifiers::CTRL),
        (Key::Named(NamedKey::Space), Modifiers::ALT),
        (Key::Named(NamedKey::ArrowUp), Modifiers::empty()),
        (Key::Named(NamedKey::ArrowUp), Modifiers::SHIFT),
        (Key::Named(NamedKey::ArrowUp), Modifiers::ALT),
        (Key::Named(NamedKey::ArrowRight), Modifiers::CTRL),
        (Key::Named(NamedKey::Home), Modifiers::empty()),
        (Key::Named(NamedKey::End), Modifiers::empty()),
        (Key::Named(NamedKey::PageUp), Modifiers::empty()),
        (Key::Named(NamedKey::Delete), Modifiers::empty()),
        (Key::Named(NamedKey::F1), Modifiers::empty()),
        (Key::Named(NamedKey::F5), Modifiers::empty()),
        (Key::Named(NamedKey::F5), Modifiers::CTRL),
        (Key::Named(NamedKey::F12), Modifiers::empty()),
        (Key::Character('a'), Modifiers::CTRL),
        (Key::Character('a'), Modifiers::SHIFT),
        (Key::Character('a'), Modifiers::ALT),
        (Key::Character('a'), Modifiers::ALT | Modifiers::SHIFT),
        (Key::Character('a'), Modifiers::ALT | Modifiers::CTRL),
        (Key::Character('c'), Modifiers::CTRL),
        (Key::Character('.'), Modifiers::ALT),
        (Key::Character('-'), Modifiers::ALT),
        (Key::Character('/'), Modifiers::ALT),
        (Key::Character('\\'), Modifiers::ALT),
        (Key::Character('1'), Modifiers::ALT | Modifiers::SHIFT),
        (Key::Character('['), Modifiers::ALT | Modifiers::SHIFT),
        (Key::Character('é'), Modifiers::ALT),
        (Key::Named(NamedKey::NumpadEnter), Modifiers::empty()),
        (Key::Named(NamedKey::NumpadEnter), Modifiers::ALT),
        (Key::Named(NamedKey::Numpad5), Modifiers::ALT),
        (Key::Named(NamedKey::NumpadBegin), Modifiers::ALT),
    ];
    for (key, mods) in cases {
        assert_eq!(
            encode_key(key, *mods, WIN32),
            encode_key(key, *mods, KeyboardMode::empty()),
            "{key:?}+{mods:?} must not change under win32-input-mode"
        );
    }
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL, WIN32),
        b"\x03"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::ArrowUp),
            Modifiers::empty(),
            WIN32 | KeyboardMode::APP_CURSOR
        ),
        b"\x1bOA"
    );
}

#[test]
fn conhost_holds_exactly_the_measured_forms() {
    for held in [
        &b"\x1b"[..],
        b"\x1b\x1b",
        b"\x1b\x7f",
        b"\x1b[",
        b"\x1bO",
        b"\x1bP",
        b"\x1b]",
        b"\x1bX",
        b"\x1b^",
        b"\x1b_",
    ] {
        assert!(conhost_holds_once_switched(held), "{held:?} is held");
    }
    for read in [
        &b"\x1ba"[..],
        b"\x1bQ",
        b"\x1b ",
        b"\x1b.",
        b"\x1b-",
        b"\x1b!",
        b"\x1b/",
        b"\x1b\\",
        b"\x1b`",
        b"\x1b{",
        b"\x1b~",
        b"\x1b\r",
        b"\x1b\t",
        b"\x1b\x08",
        b"\x1b\x00",
        b"\x1b\x01",
        b"\x1b\x1f",
        "\x1bé".as_bytes(),
        b"\x1b[A",
        b"\x1bOP",
        b"\x1b[1;5C",
        b"\x1b[1;3A",
        b"\x1b[H",
        b"\x1b[15~",
        b"\x1b[Z",
        b"\x1b\x1b[E",
        b"a",
        b"\r",
        b"",
    ] {
        assert!(!conhost_holds_once_switched(read), "{read:?} is read");
    }
}

/// Parse a win32-input-mode record pair into its two six-field records, so a
/// sweep can check shape without restating every key's fields.
fn parse_win32_pair(bytes: &[u8]) -> Option<[[u32; 6]; 2]> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut records = text.strip_suffix('_')?.split('_');
    let mut parse = || -> Option<[u32; 6]> {
        let body = records.next()?.strip_prefix("\x1b[")?;
        let fields: Vec<u32> = body
            .split(';')
            .map(|f| f.parse().ok())
            .collect::<Option<_>>()?;
        fields.try_into().ok()
    };
    let pair = [parse()?, parse()?];
    records.next().is_none().then_some(pair)
}

#[test]
fn win32_escape_is_a_record_pair() {
    let esc = Key::Named(NamedKey::Escape);
    assert_eq!(
        encode_key(&esc, Modifiers::empty(), WIN32),
        b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_"
    );
    assert_eq!(
        encode_key(&esc, Modifiers::SHIFT, WIN32),
        win32_pair(27, 1, 27, 0x10),
        "measured: Escape MODS=Shift CHAR=0x1B"
    );
    assert_eq!(
        encode_key(&esc, Modifiers::CTRL, WIN32),
        win32_pair(27, 1, 27, 0x08)
    );
    assert_eq!(
        encode_key(&esc, Modifiers::ALT, WIN32),
        win32_pair(27, 1, 27, 0x02)
    );
    assert_eq!(
        encode_key(&esc, Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK, WIN32),
        win32_pair(27, 1, 27, 0)
    );
    assert!(
        encode_key_with_event(&esc, Modifiers::empty(), WIN32, KeyEventType::Release).is_empty()
    );
    assert_eq!(
        encode_key_with_event(&esc, Modifiers::empty(), WIN32, KeyEventType::Repeat),
        win32_pair(27, 1, 27, 0)
    );
    assert_eq!(
        encode_key(&esc, Modifiers::empty(), KeyboardMode::empty()),
        b"\x1b"
    );
    assert_eq!(
        encode_key(&esc, Modifiers::ALT, KeyboardMode::empty()),
        b"\x1b\x1b"
    );
}

#[test]
fn win32_ctrl_bracket_and_ctrl_3_are_records_of_their_own_key() {
    let bracket = Key::Character('[');
    assert_eq!(
        encode_key_with_layout(
            &bracket,
            Modifiers::CTRL,
            WIN32,
            KeyEventType::Press,
            Some('[')
        ),
        b"\x1b[219;26;27;1;8;1_\x1b[219;26;27;0;8;1_"
    );
    assert_eq!(
        encode_key(&bracket, Modifiers::CTRL, WIN32),
        win32_pair(219, 26, 27, 0x08)
    );
    assert_eq!(
        encode_key(&bracket, Modifiers::CTRL | Modifiers::SHIFT, WIN32),
        win32_pair(219, 26, 27, 0x18)
    );
    assert_eq!(
        encode_key(&bracket, Modifiers::CTRL | Modifiers::ALT, WIN32),
        win32_pair(219, 26, 27, 0x0a)
    );
    assert_eq!(
        encode_key(&Key::Character('3'), Modifiers::CTRL, WIN32),
        win32_pair(51, 4, 27, 0x08)
    );
    assert_eq!(
        encode_key(&bracket, Modifiers::CTRL, KeyboardMode::empty()),
        b"\x1b"
    );
    assert_eq!(
        encode_key(&Key::Character('3'), Modifiers::CTRL, KeyboardMode::empty()),
        b"\x1b"
    );
}

#[test]
fn win32_alt_chords_that_open_a_sequence_are_records() {
    let alt = Modifiers::ALT;
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    let rec = |key: Key, mods: Modifiers, base: Option<char>| {
        encode_key_with_layout(&key, mods, WIN32, KeyEventType::Press, base)
    };
    assert_eq!(
        rec(Key::Character('['), alt, Some('[')),
        b"\x1b[219;26;91;1;2;1_\x1b[219;26;91;0;2;1_"
    );
    assert_eq!(
        rec(Key::Character(']'), alt, Some(']')),
        win32_pair(221, 27, 93, 0x02)
    );
    for key in [Key::Character('o'), Key::Character('O')] {
        assert_eq!(rec(key, alt_shift, Some('o')), win32_pair(79, 24, 79, 0x12));
    }
    assert_eq!(
        rec(Key::Character('p'), alt_shift, Some('p')),
        win32_pair(80, 25, 80, 0x12)
    );
    assert_eq!(
        rec(Key::Character('x'), alt_shift, Some('x')),
        win32_pair(88, 45, 88, 0x12)
    );
    assert_eq!(
        rec(Key::Character('6'), alt_shift, Some('6')),
        win32_pair(54, 7, 94, 0x12)
    );
    assert_eq!(
        rec(Key::Character('-'), alt_shift, Some('-')),
        win32_pair(189, 12, 95, 0x12)
    );
    assert_eq!(
        rec(Key::Character('_'), alt, None),
        win32_pair(189, 12, 95, 0x12)
    );
    assert_eq!(
        rec(Key::Character('_'), alt, Some('8')),
        win32_pair(56, 9, 95, 0x02)
    );
    let bksp = Key::Named(NamedKey::Backspace);
    assert_eq!(rec(bksp.clone(), alt, None), win32_pair(8, 14, 8, 0x02));
    assert_eq!(
        rec(Key::Character('8'), Modifiers::CTRL | alt, Some('8')),
        win32_pair(56, 9, 127, 0x0a)
    );
    assert_eq!(
        encode_key(&bksp, alt, WIN32 | KeyboardMode::BACKARROW_SENDS_BS),
        b"\x1b\x08"
    );
    assert_eq!(
        encode_key(&Key::Character('['), alt, WIN32 | KeyboardMode::ALT_NO_ESC),
        b"["
    );
    assert_eq!(
        encode_key(&Key::Character('['), alt, KeyboardMode::empty()),
        b"\x1b["
    );
    assert_eq!(encode_key(&bksp, alt, KeyboardMode::empty()), b"\x1b\x7f");
}

#[test]
fn win32_escape_records_outrank_kitty_and_modify_other_keys() {
    let esc = Key::Named(NamedKey::Escape);
    let disambiguate = WIN32 | KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_eq!(
        encode_key(&esc, Modifiers::empty(), disambiguate),
        win32_pair(27, 1, 27, 0)
    );
    assert_eq!(
        encode_key(&Key::Character('['), Modifiers::CTRL, disambiguate),
        win32_pair(219, 26, 27, 0x08)
    );
    let events = WIN32 | KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert!(
        encode_key_with_event(&esc, Modifiers::empty(), events, KeyEventType::Release).is_empty()
    );
    let l1 = WIN32 | KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    assert_eq!(
        encode_key(&esc, Modifiers::CTRL, l1),
        win32_pair(27, 1, 27, 0x08)
    );
    assert_eq!(
        encode_key(
            &esc,
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[27u"
    );
    assert_eq!(
        encode_key(
            &Key::Character('['),
            Modifiers::CTRL,
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[91;5u"
    );
    assert_eq!(
        encode_key(
            &esc,
            Modifiers::CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1
        ),
        b"\x1b[27;5;27~"
    );
}

#[test]
fn win32_ctrl_h_is_a_record_of_the_h_key() {
    let h = Key::Character('h');
    let ctrl = Modifiers::CTRL;
    let rec = |key: &Key, mods: Modifiers, base: Option<char>| {
        encode_key_with_layout(key, mods, WIN32, KeyEventType::Press, base)
    };
    assert_eq!(
        rec(&h, ctrl, Some('h')),
        b"\x1b[72;35;8;1;8;1_\x1b[72;35;8;0;8;1_"
    );
    assert_eq!(rec(&h, ctrl, None), win32_pair(72, 35, 8, 0x08));
    assert_eq!(
        rec(&h, ctrl | Modifiers::SHIFT, Some('h')),
        b"\x1b[72;35;8;1;8;1_\x1b[72;35;8;0;8;1_"
    );
    assert_eq!(
        rec(&Key::Character('H'), ctrl, None),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(
        rec(&Key::Character('H'), ctrl | Modifiers::CAPS_LOCK, Some('h')),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(
        rec(&Key::Character('H'), ctrl | Modifiers::SHIFT, None),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(
        rec(
            &h,
            ctrl | Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
            Some('h')
        ),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(
        rec(&Key::Character('р'), ctrl, Some('h')),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(rec(&h, ctrl, Some('j')), win32_pair(72, 36, 8, 0x08));
    assert!(encode_key_with_event(&h, ctrl, WIN32, KeyEventType::Release).is_empty());
    assert_eq!(
        encode_key_with_event(&h, ctrl, WIN32, KeyEventType::Repeat),
        win32_pair(72, 35, 8, 0x08)
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), ctrl, WIN32),
        b"\x08"
    );
    assert_eq!(encode_key(&Key::Character('c'), ctrl, WIN32), b"\x03");
    assert_eq!(encode_key(&h, ctrl | Modifiers::ALT, WIN32), b"\x1b\x08");
    assert_eq!(encode_key(&Key::Character('\u{8}'), ctrl, WIN32), b"\x08");
    assert_eq!(
        encode_key(&h, ctrl | Modifiers::ALT, WIN32 | KeyboardMode::ALT_NO_ESC),
        win32_pair(72, 35, 8, 0x08)
    );
    let legacy = KeyboardMode::empty();
    assert_eq!(encode_key(&h, ctrl, legacy), b"\x08");
    assert_eq!(encode_key(&h, ctrl | Modifiers::SHIFT, legacy), b"\x08");
    assert_eq!(encode_key(&Key::Character('H'), ctrl, legacy), b"\x08");
    assert_eq!(
        encode_key_with_layout(
            &Key::Character('р'),
            ctrl,
            legacy,
            KeyEventType::Press,
            Some('h')
        ),
        b"\x08"
    );
}

#[test]
fn win32_ctrl_j_is_a_record_of_the_j_key() {
    let j = Key::Character('j');
    let ctrl = Modifiers::CTRL;
    let rec = |key: &Key, mods: Modifiers, base: Option<char>| {
        encode_key_with_layout(key, mods, WIN32, KeyEventType::Press, base)
    };
    assert_eq!(
        rec(&j, ctrl, Some('j')),
        b"\x1b[74;36;10;1;8;1_\x1b[74;36;10;0;8;1_"
    );
    assert_eq!(rec(&j, ctrl, None), win32_pair(74, 36, 10, 0x08));
    for (key, mods) in [
        (j.clone(), ctrl | Modifiers::SHIFT),
        (Key::Character('J'), ctrl),
        (Key::Character('J'), ctrl | Modifiers::CAPS_LOCK),
        (Key::Character('о'), ctrl),
    ] {
        assert_eq!(
            rec(&key, mods, Some('j')),
            win32_pair(74, 36, 10, 0x08),
            "{key:?}+{mods:?}"
        );
    }
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), ctrl, WIN32),
        win32_enter_pair(10, 0x08)
    );
    assert_eq!(encode_key(&Key::Character('\n'), ctrl, WIN32), b"\x0a");
    assert_eq!(encode_key(&j, ctrl | Modifiers::ALT, WIN32), b"\x1b\x0a");
    assert_eq!(encode_key(&j, ctrl, KeyboardMode::empty()), b"\x0a");
}

#[test]
fn win32_ctrl_h_record_outranks_kitty_and_modify_other_keys() {
    let h = Key::Character('h');
    for mode in [
        KeyboardMode::DISAMBIGUATE_ESC_CODES,
        KeyboardMode::REPORT_ALL_KEYS_AS_ESC,
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1,
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2 | KeyboardMode::XTERM_FORMAT_OTHER_KEYS,
    ] {
        assert_eq!(
            encode_key(&h, Modifiers::CTRL, WIN32 | mode),
            win32_pair(72, 35, 8, 0x08),
            "{mode:?}"
        );
    }
    let events = WIN32 | KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert!(encode_key_with_event(&h, Modifiers::CTRL, events, KeyEventType::Release).is_empty());
    assert_eq!(
        encode_key(&h, Modifiers::CTRL, KeyboardMode::DISAMBIGUATE_ESC_CODES),
        b"\x1b[104;5u"
    );
    assert_eq!(
        encode_key(
            &h,
            Modifiers::CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2
        ),
        b"\x1b[27;5;104~"
    );
    assert_eq!(
        encode_key(
            &h,
            Modifiers::CTRL,
            KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1
        ),
        b"\x08"
    );
}

#[test]
fn win32_changes_only_the_held_forms_across_the_whole_ascii_keyboard() {
    let mods_list = [
        Modifiers::empty(),
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::SHIFT | Modifiers::ALT,
        Modifiers::CTRL | Modifiers::ALT,
        Modifiers::CTRL | Modifiers::SHIFT,
        Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
    ];
    let mut keys: Vec<Key> = (' '..='~').map(Key::Character).collect();
    keys.extend(
        [
            NamedKey::Escape,
            NamedKey::Backspace,
            NamedKey::Tab,
            NamedKey::Space,
            NamedKey::ArrowUp,
            NamedKey::Home,
            NamedKey::F1,
            NamedKey::F5,
            NamedKey::Insert,
            NamedKey::Numpad5,
            NamedKey::NumpadBegin,
            NamedKey::NumpadDecimal,
        ]
        .map(Key::Named),
    );
    let mut routed = 0;
    let mut ctrl_h = 0;
    let mut ctrl_j = 0;
    let mut named = 0;
    for key in &keys {
        for mods in mods_list {
            let legacy = encode_key(key, mods, KeyboardMode::empty());
            let win = encode_key(key, mods, WIN32);
            if matches!(key, Key::Character('h' | 'H'))
                && mods.contains(Modifiers::CTRL)
                && !mods.contains(Modifiers::ALT)
            {
                ctrl_h += 1;
                assert_eq!(legacy, b"\x08", "{key:?}+{mods:?}: legacy BS");
                assert_eq!(win, win32_pair(72, 35, 8, 0x08), "{key:?}+{mods:?}");
                continue;
            }
            if matches!(key, Key::Character('j' | 'J'))
                && mods.contains(Modifiers::CTRL)
                && !mods.contains(Modifiers::ALT)
            {
                ctrl_j += 1;
                assert_eq!(legacy, b"\x0a", "{key:?}+{mods:?}: legacy LF");
                assert_eq!(win, win32_pair(74, 36, 10, 0x08), "{key:?}+{mods:?}");
                continue;
            }
            let named_record = match key {
                Key::Named(NamedKey::Space) => {
                    mods == Modifiers::SHIFT
                        || mods == Modifiers::CTRL
                        || mods.contains(Modifiers::CTRL | Modifiers::ALT)
                }
                Key::Named(NamedKey::Backspace) => mods == Modifiers::SHIFT,
                Key::Named(NamedKey::Numpad5 | NamedKey::NumpadDecimal) => {
                    !mods.contains(Modifiers::ALT)
                }
                _ => false,
            };
            if named_record {
                named += 1;
                assert!(
                    parse_win32_pair(&win).is_some(),
                    "{key:?}+{mods:?}: {win:?} is not a record pair"
                );
                continue;
            }
            if !conhost_holds_once_switched(&legacy) {
                assert_eq!(win, legacy, "{key:?}+{mods:?} must keep its bytes");
                continue;
            }
            routed += 1;
            let [down, up] = parse_win32_pair(&win)
                .unwrap_or_else(|| panic!("{key:?}+{mods:?}: {win:?} is not a record pair"));
            assert_eq!(down[3], 1, "{key:?}+{mods:?}: key-down first");
            assert_eq!(up[3], 0, "{key:?}+{mods:?}: key-up second");
            assert_eq!(
                (down[0], down[1], down[2], down[4], down[5]),
                (up[0], up[1], up[2], up[4], up[5]),
                "{key:?}+{mods:?}: both halves name the same key"
            );
            if *key != Key::Named(NamedKey::Backspace) {
                assert_eq!(
                    down[2],
                    u32::from(*legacy.last().unwrap()),
                    "{key:?}+{mods:?}: UnicodeChar"
                );
            }
            assert_eq!(
                down[4] & 0x02 != 0,
                legacy.len() == 2,
                "{key:?}+{mods:?}: ALT bit"
            );
        }
    }
    assert!(routed > 20, "only {routed} keys were routed");
    assert_eq!(ctrl_h, 4, "Ctrl(+Shift)+h/H");
    assert_eq!(ctrl_j, 4, "Ctrl(+Shift)+j/J");
    assert_eq!(named, 4 + 1 + 8, "named-key records");
}

#[test]
fn win32_numpad_enter_chord_is_an_enhanced_key_record() {
    let kp = Key::Named(NamedKey::NumpadEnter);
    assert_eq!(
        encode_key(&kp, Modifiers::SHIFT, WIN32),
        win32_enter_pair(13, 0x110)
    );
    assert_eq!(
        encode_key(&kp, Modifiers::SHIFT, WIN32),
        b"\x1b[13;28;13;1;272;1_\x1b[13;28;13;0;272;1_"
    );
    assert_eq!(
        encode_key(&kp, Modifiers::CTRL, WIN32),
        win32_enter_pair(10, 0x108)
    );
    assert_eq!(
        encode_key(
            &kp,
            Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
            WIN32
        ),
        win32_enter_pair(10, 0x11a)
    );
    let app = WIN32 | KeyboardMode::APP_KEYPAD;
    assert_eq!(
        encode_key(&kp, Modifiers::SHIFT, app),
        win32_enter_pair(13, 0x110)
    );
    assert_eq!(
        encode_key(&kp, Modifiers::CTRL, app),
        win32_enter_pair(10, 0x108)
    );
    assert_eq!(
        encode_key(&kp, Modifiers::ALT, app),
        encode_key(&kp, Modifiers::ALT, KeyboardMode::APP_KEYPAD)
    );
    assert_eq!(encode_key(&kp, Modifiers::empty(), WIN32), b"\r");
    assert_eq!(
        encode_key(&kp, Modifiers::empty(), app),
        win32_enter_pair(13, 0x100)
    );
    assert!(encode_key_with_event(&kp, Modifiers::SHIFT, WIN32, KeyEventType::Release).is_empty());
}
