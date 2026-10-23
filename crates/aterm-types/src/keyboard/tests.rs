// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for the shared keyboard encoding module.

use super::*;

type Case = (
    &'static str,
    Key,
    Modifiers,
    KeyboardMode,
    Option<KeyEventType>,
    &'static [u8],
);

const KEY: Option<KeyEventType> = None;
const PRESS: Option<KeyEventType> = Some(KeyEventType::Press);
const REPEAT: Option<KeyEventType> = Some(KeyEventType::Repeat);
const RELEASE: Option<KeyEventType> = Some(KeyEventType::Release);

const NO_MODS: Modifiers = Modifiers::empty();
const LEGACY: KeyboardMode = KeyboardMode::empty();

fn ch(c: char) -> Key {
    Key::Character(c)
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

fn assert_cases(cases: &[Case]) {
    for (what, key, mods, mode, via, want) in cases {
        let got = match via {
            None => encode_key(key, *mods, *mode),
            Some(event) => encode_key_with_event(key, *mods, *mode, *event),
        };
        assert_eq!(got, *want, "{what}: {key:?} {mods:?} {mode:?} {via:?}");
    }
}

#[test]
fn keyboard_mode_xterm_accessors() {
    let level1 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    for (what, mode, level) in [
        ("default", LEGACY, 0),
        ("level 1", level1, 1),
        ("level 2", level2, 2),
        ("both levels", level1 | level2, 2),
    ] {
        assert_eq!(mode.xterm_modify_other_keys_level(), level, "{what}");
    }
    for (mode, format) in [
        (LEGACY, false),
        (KeyboardMode::XTERM_FORMAT_OTHER_KEYS, true),
    ] {
        assert_eq!(mode.xterm_format_other_keys(), format, "{mode:?}");
    }
}

#[test]
fn term_mode_from_keyboard_state_all_kitty() {
    use crate::{KittyKeyboardFlags, XtermKeyboardState};
    let kitty = KittyKeyboardFlags::from_bits(0x1F); // all 5 flags
    let tm = TermMode::from_keyboard_state(false, false, false, kitty, XtermKeyboardState::new());
    assert!(tm.contains(TermMode::DISAMBIGUATE_ESC_CODES));
    assert!(tm.contains(TermMode::REPORT_EVENT_TYPES));
    assert!(tm.contains(TermMode::REPORT_ALTERNATE_KEYS));
    assert!(tm.contains(TermMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(tm.contains(TermMode::REPORT_ASSOCIATED_TEXT));
    assert!(!tm.contains(TermMode::SHOW_CURSOR));
    assert!(!tm.contains(TermMode::ALT_SCREEN));
}

#[test]
fn term_mode_from_keyboard_state_xterm_levels() {
    use crate::{KittyKeyboardFlags, XtermKeyboardState};
    let mut xterm = XtermKeyboardState::new();
    xterm.set_modify_other_keys(1);
    let tm = TermMode::from_keyboard_state(false, false, false, KittyKeyboardFlags::none(), xterm);
    assert!(tm.contains(TermMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1));
    assert!(!tm.contains(TermMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2));

    xterm.set_modify_other_keys(2);
    let tm = TermMode::from_keyboard_state(false, false, false, KittyKeyboardFlags::none(), xterm);
    assert!(!tm.contains(TermMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1));
    assert!(tm.contains(TermMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2));
}

#[test]
fn term_mode_to_keyboard_mode_roundtrip_mixed() {
    use crate::{KittyKeyboardFlags, XtermKeyboardState};
    let kitty = KittyKeyboardFlags::from_bits(
        KittyKeyboardFlags::DISAMBIGUATE | KittyKeyboardFlags::REPORT_TEXT,
    );
    let mut xterm = XtermKeyboardState::new();
    xterm.set_modify_other_keys(2);
    xterm.set_format_other_keys(1);
    let tm = TermMode::from_keyboard_state(true, true, false, kitty, xterm);
    let km = tm.to_keyboard_mode();

    assert!(km.contains(KeyboardMode::APP_CURSOR));
    assert!(km.contains(KeyboardMode::APP_KEYPAD));
    assert!(km.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES));
    assert!(km.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT));
    assert!(!km.contains(KeyboardMode::REPORT_EVENT_TYPES));
    assert!(km.contains(KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2));
    assert!(km.contains(KeyboardMode::XTERM_FORMAT_OTHER_KEYS));
}

#[test]
fn term_mode_to_keyboard_mode_ignores_non_keyboard_flags() {
    let tm = TermMode::SHOW_CURSOR
        | TermMode::ALT_SCREEN
        | TermMode::BRACKETED_PASTE
        | TermMode::APP_CURSOR;
    let km = tm.to_keyboard_mode();
    assert!(km.contains(KeyboardMode::APP_CURSOR));
    assert_eq!(km, KeyboardMode::APP_CURSOR);
}

#[test]
fn modifier_wire_values() {
    for (mods, kitty) in [
        (NO_MODS, 1),
        (Modifiers::SHIFT, 2),
        (Modifiers::CTRL | Modifiers::ALT, 7),
        (Modifiers::CAPS_LOCK, 65),
        (Modifiers::SHIFT | Modifiers::CAPS_LOCK, 66),
        (Modifiers::NUM_LOCK, 129),
    ] {
        assert_eq!(mods.kitty_encoded(), kitty, "kitty {mods:?}");
    }
    for (mods, xterm) in [
        (Modifiers::CAPS_LOCK, 1),
        (Modifiers::NUM_LOCK, 1),
        (NO_MODS, 1),
        (Modifiers::SHIFT, 2),
        (Modifiers::ALT, 3),
        (Modifiers::CTRL, 5),
        (Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL, 8),
    ] {
        assert_eq!(mods.xterm_encoded(), xterm, "xterm {mods:?}");
    }
}

#[path = "tests_legacy.rs"]
mod tests_legacy;

#[test]
#[rustfmt::skip]
fn kitty_disambiguate_encodings() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_cases(&[
        ("plain char", ch('a'), NO_MODS, mode, KEY, b"a"),
        ("shift char", ch('a'), Modifiers::SHIFT, mode, KEY, b"A"),
        ("shifted symbol", ch('2'), Modifiers::SHIFT, mode, KEY, b"@"),
        ("ctrl char", ch('c'), Modifiers::CTRL, mode, KEY, b"\x1b[99;5u"),
        ("enter", named(NamedKey::Enter), NO_MODS, mode, KEY, b"\r"),
        ("escape", named(NamedKey::Escape), NO_MODS, mode, KEY, b"\x1b[27u"),
        ("arrow up", named(NamedKey::ArrowUp), NO_MODS, mode, KEY, b"\x1b[A"),
        ("f1", named(NamedKey::F1), NO_MODS, mode, KEY, b"\x1b[P"),
        ("f25", named(NamedKey::F25), NO_MODS, mode, KEY, b"\x1b[57388u"),
        ("scroll lock", named(NamedKey::ScrollLock), NO_MODS, mode, KEY, b""),
        (
            "scroll lock, report-all",
            named(NamedKey::ScrollLock),
            NO_MODS,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC,
            KEY,
            b"\x1b[57359u",
        ),
        ("media play", named(NamedKey::MediaPlay), NO_MODS, mode, KEY, b"\x1b[57428u"),
        ("numpad equal", named(NamedKey::NumpadEqual), NO_MODS, mode, KEY, b"\x1b[57415u"),
        ("numpad up", named(NamedKey::NumpadArrowUp), NO_MODS, mode, KEY, b"\x1b[57419u"),
    ]);
}

#[test]
#[rustfmt::skip]
fn kitty_event_type_encodings() {
    let both = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let events = KeyboardMode::REPORT_EVENT_TYPES;
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    assert_cases(&[
        ("0b11 text repeat", ch('a'), NO_MODS, both, REPEAT, b"a"),
        ("0b11 ctrl repeat", ch('a'), Modifiers::CTRL, both, REPEAT, b"\x1b[97;5:2u"),
        ("0b11 text release", ch('a'), NO_MODS, both, RELEASE, b""),
        ("0b11 text press", ch('a'), NO_MODS, both, PRESS, b"a"),
        (
            "disambiguate-only release",
            ch('a'),
            NO_MODS,
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            RELEASE,
            b"",
        ),
        ("0b10 up release", named(NamedKey::ArrowUp), NO_MODS, events, RELEASE, b"\x1b[1;1:3A"),
        ("0b10 esc repeat", named(NamedKey::Escape), NO_MODS, events, REPEAT, b"\x1b[27;1:2u"),
        ("0b10 text release", ch('a'), NO_MODS, events, RELEASE, b""),
        ("0b10 ctrl release", ch('a'), Modifiers::CTRL, events, RELEASE, b"\x1b[97;5:3u"),
        ("0b10 ctrl+shift repeat", ch('a'), ctrl_shift, events, REPEAT, b"\x1b[97;6:2u"),
        ("0b10 ctrl+shift release", ch('a'), ctrl_shift, events, RELEASE, b"\x1b[97;6:3u"),
        (
            "0b10 shift+tab repeat",
            named(NamedKey::Tab),
            Modifiers::SHIFT,
            events,
            REPEAT,
            b"\x1b[9;2:2u",
        ),
        ("0b10 shift+tab release", named(NamedKey::Tab), Modifiers::SHIFT, events, RELEASE, b""),
        ("0b10 enter release", named(NamedKey::Enter), NO_MODS, events, RELEASE, b""),
        ("0b10 press stays legacy", named(NamedKey::ArrowUp), NO_MODS, events, KEY, b"\x1b[A"),
        ("0b11 ctrl release", ch('a'), Modifiers::CTRL, both, RELEASE, b"\x1b[97;5:3u"),
        ("0b11 bare shift press", named(NamedKey::ShiftLeft), NO_MODS, both, PRESS, b""),
        (
            "0b11 bare shift release",
            named(NamedKey::ShiftLeft),
            Modifiers::SHIFT,
            both,
            RELEASE,
            b"",
        ),
        (
            "report-all shift press",
            named(NamedKey::ShiftLeft),
            NO_MODS,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC,
            PRESS,
            b"\x1b[57441;2u",
        ),
        (
            "report-all shift release",
            named(NamedKey::ShiftLeft),
            NO_MODS,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_EVENT_TYPES,
            RELEASE,
            b"\x1b[57441;1:3u",
        ),
        (
            "report-all shift release, the other Shift held",
            named(NamedKey::ShiftLeft),
            Modifiers::SHIFT,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_EVENT_TYPES,
            RELEASE,
            b"\x1b[57441;2:3u",
        ),
    ]);
}

#[test]
#[rustfmt::skip]
fn xterm_modify_other_keys_encodings() {
    let level1 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_cases(&[
        ("L1 alt char", ch('a'), Modifiers::ALT, level1, KEY, b"\x1b[27;3;97~"),
        ("L1 ctrl char", ch('c'), Modifiers::CTRL, level1, KEY, &[0x03]),
        ("L2 ctrl char", ch('c'), Modifiers::CTRL, level2, KEY, b"\x1b[27;5;99~"),
        ("L2 shift char", ch('a'), Modifiers::SHIFT, level2, KEY, b"\x1b[27;2;65~"),
        (
            "L2 formatOtherKeys",
            ch('a'),
            Modifiers::CTRL,
            level2 | KeyboardMode::XTERM_FORMAT_OTHER_KEYS,
            KEY,
            b"\x1b[97;5u",
        ),
        ("L1 alt enter", named(NamedKey::Enter), Modifiers::ALT, level1, KEY, b"\x1b[27;3;13~"),
        ("L2 shift tab", named(NamedKey::Tab), Modifiers::SHIFT, level2, KEY, b"\x1b[27;2;9~"),
        ("L2 ctrl up", named(NamedKey::ArrowUp), Modifiers::CTRL, level2, KEY, b"\x1b[1;5A"),
        ("L2 no mods", ch('a'), NO_MODS, level2, KEY, b"a"),
    ]);
}

#[test]
fn xterm_modify_other_keys_level1_preserves_backspace_legacy_forms() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::ALT, mode),
        vec![0x1b, 0x7f],
        "Alt+Backspace is not an xterm modifyOtherKeys packet"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::CTRL, mode),
        vec![0x08],
        "Ctrl+Backspace keeps the DECBKM toggle"
    );
}

#[test]
fn xterm_modify_other_keys_level1_reports_only_ambiguous_ctrl_shift_chars() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    assert_eq!(
        encode_key(
            &Key::Character('c'),
            Modifiers::CTRL | Modifiers::SHIFT,
            mode
        ),
        b"\x1b[27;6;67~"
    );
    assert_eq!(
        encode_key(&Key::Character('1'), Modifiers::CTRL, mode),
        b"\x1b[27;5;49~",
        "Ctrl+1 has no traditional C0 mapping, so level 1 must retain Ctrl"
    );
}

#[test]
fn xterm_modify_other_keys_level2_preserves_unambiguous_shifted_text() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Character('1'), Modifiers::SHIFT, mode),
        b"!",
        "Shift+1 is ordinary shifted text, not a numeric CSI packet"
    );
    assert_eq!(
        encode_key(&Key::Character('='), Modifiers::SHIFT, mode),
        b"+"
    );
    assert_eq!(
        encode_key(&Key::Character('é'), Modifiers::SHIFT, mode),
        "é".as_bytes()
    );
    assert_eq!(
        encode_key(&Key::Character('2'), Modifiers::SHIFT, mode),
        b"\x1b[27;2;64~",
        "Shift+2 produces '@', which is in xterm's control-input range, and is reported as '@'"
    );
}

#[test]
fn xterm_modify_other_keys_level2_ctrl_backspace_uses_legacy_toggle() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::CTRL, mode),
        vec![0x08]
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::SHIFT, mode),
        b"\x1b[27;2;127~"
    );
}

#[test]
fn xterm_modify_other_keys_does_not_capture_keypad_keys() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadEqual), Modifiers::ALT, mode),
        b"\x1b="
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadEnter), Modifiers::ALT, mode),
        b"\x1b\r"
    );
}

#[test]
fn named_key_kitty_codes() {
    for (key, code) in [
        (NamedKey::Escape, 27),
        (NamedKey::Enter, 13),
        (NamedKey::Tab, 9),
        (NamedKey::Backspace, 127),
        (NamedKey::Space, 32),
        (NamedKey::ArrowUp, 57352),
        (NamedKey::ArrowDown, 57353),
        (NamedKey::ArrowLeft, 57350),
        (NamedKey::ArrowRight, 57351),
        (NamedKey::Home, 57356),
        (NamedKey::End, 57357),
        (NamedKey::PageUp, 57354),
        (NamedKey::PageDown, 57355),
        (NamedKey::Insert, 57348),
        (NamedKey::Delete, 57349),
        (NamedKey::F1, 57364),
        (NamedKey::F12, 57375),
        (NamedKey::F24, 57387),
        (NamedKey::F35, 57398),
        (NamedKey::CapsLock, 57358),
        (NamedKey::ScrollLock, 57359),
        (NamedKey::MediaPlay, 57428),
        (NamedKey::ShiftLeft, 57441),
        (NamedKey::MetaRight, 57452),
        (NamedKey::Numpad0, 57399),
        (NamedKey::Numpad9, 57408),
        (NamedKey::NumpadEnter, 57414),
        (NamedKey::NumpadAdd, 57413),
        (NamedKey::NumpadEqual, 57415),
        (NamedKey::NumpadArrowUp, 57419),
        (NamedKey::NumpadDelete, 57426),
    ] {
        assert_eq!(key.kitty_code(), code, "{key:?}");
    }
}

#[path = "kitty_progressive_tests.rs"]
mod kitty_progressive;

#[test]
fn disambiguate_plain_keys_are_not_over_escaped() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::empty(), mode),
        b"a"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode),
        b"\r"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Tab), Modifiers::empty(), mode),
        b"\t"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Space), Modifiers::empty(), mode),
        b" "
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Backspace), Modifiers::empty(), mode),
        b"\x7f"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Escape), Modifiers::empty(), mode),
        b"\x1b[27u"
    );
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL, mode),
        b"\x1b[99;5u"
    );
}

#[test]
fn legacy_lock_bits_do_not_corrupt_navigation_keys() {
    let mode = KeyboardMode::empty();
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::CAPS_LOCK, mode),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F5), Modifiers::CAPS_LOCK, mode),
        b"\x1b[15~"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Home), Modifiers::NUM_LOCK, mode),
        b"\x1b[H"
    );
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::ArrowUp),
            Modifiers::SHIFT | Modifiers::CAPS_LOCK,
            mode
        ),
        b"\x1b[1;2A"
    );
}
