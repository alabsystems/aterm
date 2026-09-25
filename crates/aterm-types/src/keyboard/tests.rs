// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for the shared keyboard encoding module.

use super::*;

// =========================================================================
// Table rows
// =========================================================================

/// One labelled encoder case: `(what, key, mods, mode, via, want)`.
///
/// `via` is the entry point the case drives: [`KEY`] calls [`encode_key`];
/// [`PRESS`] / [`REPEAT`] / [`RELEASE`] call [`encode_key_with_event`] with that
/// event. Each row therefore makes exactly the call the one-case test it
/// replaced made, and keeps that test's expected bytes.
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

/// No modifiers / no mode flags, spelled short so a row fits on one line.
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

// =========================================================================
// KeyboardMode helpers
// =========================================================================

#[test]
fn keyboard_mode_xterm_accessors() {
    let level1 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    for (what, mode, level) in [
        ("default", LEGACY, 0),
        ("level 1", level1, 1),
        ("level 2", level2, 2),
        // Level 2 takes precedence when both flags are set.
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

// =========================================================================
// TermMode::from_keyboard_state / to_keyboard_mode (#3732)
// =========================================================================

#[test]
fn term_mode_from_keyboard_state_empty() {
    use crate::{KittyKeyboardFlags, XtermKeyboardState};
    let tm = TermMode::from_keyboard_state(
        false,
        false,
        false,
        KittyKeyboardFlags::none(),
        XtermKeyboardState::new(),
    );
    assert_eq!(tm, TermMode::empty());
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
    // Non-keyboard flags should be absent
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
fn term_mode_from_keyboard_state_xterm_format() {
    use crate::{KittyKeyboardFlags, XtermKeyboardState};
    let mut xterm = XtermKeyboardState::new();
    xterm.set_format_other_keys(1);
    let tm = TermMode::from_keyboard_state(false, false, false, KittyKeyboardFlags::none(), xterm);
    assert!(tm.contains(TermMode::XTERM_FORMAT_OTHER_KEYS));
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
    // Manually set non-keyboard flags; to_keyboard_mode should not include them.
    let tm = TermMode::SHOW_CURSOR
        | TermMode::ALT_SCREEN
        | TermMode::BRACKETED_PASTE
        | TermMode::APP_CURSOR;
    let km = tm.to_keyboard_mode();
    assert!(km.contains(KeyboardMode::APP_CURSOR));
    // KeyboardMode has no SHOW_CURSOR, ALT_SCREEN, or BRACKETED_PASTE — only keyboard flags.
    assert_eq!(km, KeyboardMode::APP_CURSOR);
}

// =========================================================================
// Modifiers encoding
// =========================================================================

#[test]
fn modifier_wire_values() {
    for (mods, kitty) in [
        (NO_MODS, 1),
        (Modifiers::SHIFT, 2),
        (Modifiers::CTRL | Modifiers::ALT, 7),
        // WIRE-MODIFIERS: Caps Lock (bit 64) reports as kitty value 65, and
        // combines with chord modifiers (Shift+CapsLock -> 1 + 64 + shift(1) = 66).
        (Modifiers::CAPS_LOCK, 65),
        (Modifiers::SHIFT | Modifiers::CAPS_LOCK, 66),
        // Num Lock (bit 128) reports as kitty value 129.
        (Modifiers::NUM_LOCK, 129),
    ] {
        assert_eq!(mods.kitty_encoded(), kitty, "kitty {mods:?}");
    }
    for (mods, xterm) in [
        // Legacy xterm modifier encoding does NOT report Caps/Num Lock.
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

// =========================================================================
// KeyEventType
// =========================================================================

#[test]
fn key_event_type_default_is_press() {
    assert_eq!(KeyEventType::default(), KeyEventType::Press);
}

#[test]
fn key_event_type_kitty_values() {
    assert_eq!(KeyEventType::Press.kitty_value(), 1);
    assert_eq!(KeyEventType::Repeat.kitty_value(), 2);
    assert_eq!(KeyEventType::Release.kitty_value(), 3);
}

// Legacy keyboard encoding tests extracted to dedicated file
// to keep this module under the 1000-line limit.
#[path = "tests_legacy.rs"]
mod tests_legacy;

// =========================================================================
// Kitty keyboard protocol
// =========================================================================

#[test]
#[rustfmt::skip]
fn kitty_disambiguate_encodings() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_cases(&[
        // Disambiguate must NOT over-escape a plain printable key: it stays text.
        ("plain char", ch('a'), NO_MODS, mode, KEY, b"a"),
        // Shift+key is NOT in the spec's disambiguation list ("the Esc, alt+key,
        // ctrl+key, ctrl+alt+key, shift+alt+key keys") — it composes text, and
        // kitty sends the shifted glyph in every text-preserving mode.
        ("shift char", ch('a'), Modifiers::SHIFT, mode, KEY, b"A"),
        ("shifted symbol", ch('2'), Modifiers::SHIFT, mode, KEY, b"@"),
        // CSI 99;5 u (ctrl = mod value 5)
        ("ctrl char", ch('c'), Modifiers::CTRL, mode, KEY, b"\x1b[99;5u"),
        // Enter has a legacy text byte and keeps it under disambiguate (#audit).
        ("enter", named(NamedKey::Enter), NO_MODS, mode, KEY, b"\r"),
        // Escape kitty code = 27
        ("escape", named(NamedKey::Escape), NO_MODS, mode, KEY, b"\x1b[27u"),
        // ArrowUp and F1 retain legacy CSI format under Kitty protocol (#7474)
        ("arrow up", named(NamedKey::ArrowUp), NO_MODS, mode, KEY, b"\x1b[A"),
        ("f1", named(NamedKey::F1), NO_MODS, mode, KEY, b"\x1b[P"),
        ("f25", named(NamedKey::F25), NO_MODS, mode, KEY, b"\x1b[57388u"),
        // Modifier/lock keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC
        // (kitty's is_modifier_key gate covers ScrollLock/CapsLock/NumLock too).
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

// =========================================================================
// Kitty with event types
// =========================================================================

#[test]
#[rustfmt::skip]
fn kitty_event_type_encodings() {
    let both = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let events = KeyboardMode::REPORT_EVENT_TYPES;
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    assert_cases(&[
        // Text-producing presses AND repeats stay plain UTF-8 under 0b11 — the
        // event-type subfield only decorates events that already need an escape
        // form (kitty sends 'a' here; its plain-text release stays silent).
        ("0b11 text repeat", ch('a'), NO_MODS, both, REPEAT, b"a"),
        // A chord that disambiguates gets the full CSI-u with `:2` (repeat).
        ("0b11 ctrl repeat", ch('a'), Modifiers::CTRL, both, REPEAT, b"\x1b[97;5:2u"),
        // Text-producing events have no release representation unless the app
        // also requests REPORT_ALL_KEYS_AS_ESC.
        ("0b11 text release", ch('a'), NO_MODS, both, RELEASE, b""),
        // Spec: event-type reporting adds repeat/release reports WITHOUT changing
        // press encoding — a plain printable press is still text under 0b11.
        ("0b11 text press", ch('a'), NO_MODS, both, PRESS, b"a"),
        // Kitty spec: releases are reported ONLY under REPORT_EVENT_TYPES. Under
        // disambiguate-only a release must encode to NOTHING — the CSI-u encoder
        // would omit the `:3` subfield in this mode, making the release
        // byte-identical to a press and doubling every key.
        (
            "disambiguate-only release",
            ch('a'),
            NO_MODS,
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
            RELEASE,
            b"",
        ),
        // ArrowUp release retains legacy CSI format with event type (#7474)
        ("0b10 up release", named(NamedKey::ArrowUp), NO_MODS, events, RELEASE, b"\x1b[1;1:3A"),
        ("0b10 esc repeat", named(NamedKey::Escape), NO_MODS, events, REPEAT, b"\x1b[27;1:2u"),
        // REPORT_EVENT_TYPES cannot add an event type to plain UTF-8 text. The
        // release exists only once REPORT_ALL_KEYS_AS_ESC makes the key a report.
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
        // CSI 97;5:3 u (ctrl=5, release=3)
        ("0b11 ctrl release", ch('a'), Modifiers::CTRL, both, RELEASE, b"\x1b[97;5:3u"),
        // Modifier keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC; under
        // 0b11 both press and release must be silent.
        ("0b11 bare shift press", named(NamedKey::ShiftLeft), NO_MODS, both, PRESS, b""),
        (
            "0b11 bare shift release",
            named(NamedKey::ShiftLeft),
            Modifiers::SHIFT,
            both,
            RELEASE,
            b"",
        ),
        // The bit reflects the state INCLUDING the current event: set on press.
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
            Modifiers::SHIFT,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_EVENT_TYPES,
            RELEASE,
            b"\x1b[57441;1:3u",
        ),
    ]);
}

// =========================================================================
// xterm modifyOtherKeys
// =========================================================================

#[test]
#[rustfmt::skip]
fn xterm_modify_other_keys_encodings() {
    let level1 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let level2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_cases(&[
        // Level 1: only Alt triggers modifyOtherKeys encoding.
        // Default format: CSI 27;3;97 ~ (mod=3=alt, code=97='a')
        ("L1 alt char", ch('a'), Modifiers::ALT, level1, KEY, b"\x1b[27;3;97~"),
        // Level 1 with only Ctrl: does NOT use modifyOtherKeys, falls to legacy
        // Ctrl+C = 0x03.
        ("L1 ctrl char", ch('c'), Modifiers::CTRL, level1, KEY, &[0x03]),
        // Level 2: any modifier triggers modifyOtherKeys encoding.
        // Default format: CSI 27;5;99 ~ (mod=5=ctrl, code=99='c')
        ("L2 ctrl char", ch('c'), Modifiers::CTRL, level2, KEY, b"\x1b[27;5;99~"),
        // Default format: CSI 27;2;97 ~ (mod=2=shift, code=97='a')
        ("L2 shift char", ch('a'), Modifiers::SHIFT, level2, KEY, b"\x1b[27;2;97~"),
        // formatOtherKeys=1: CSI code ; modifier u (code=97='a', mod=5=ctrl)
        (
            "L2 formatOtherKeys",
            ch('a'),
            Modifiers::CTRL,
            level2 | KeyboardMode::XTERM_FORMAT_OTHER_KEYS,
            KEY,
            b"\x1b[97;5u",
        ),
        // modifyOtherKeys applies to specific named keys too (Enter → code 13):
        // CSI 27;3;13 ~
        ("L1 alt enter", named(NamedKey::Enter), Modifiers::ALT, level1, KEY, b"\x1b[27;3;13~"),
        // CSI 27;2;9 ~
        ("L2 shift tab", named(NamedKey::Tab), Modifiers::SHIFT, level2, KEY, b"\x1b[27;2;9~"),
        // Arrow keys are NOT in the modifyOtherKeys subset — falls through to
        // legacy Ctrl+Up: CSI 1;5 A
        ("L2 ctrl up", named(NamedKey::ArrowUp), Modifiers::CTRL, level2, KEY, b"\x1b[1;5A"),
        // No modifiers → modifyOtherKeys doesn't apply.
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
        b"\x1b[27;6;99~"
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
        b"\x1b[27;2;50~",
        "Shift+2 produces '@', which is in xterm's control-input range"
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

// =========================================================================
// Key and NamedKey type constructors
// =========================================================================

#[test]
fn key_character_constructor() {
    assert_eq!(Key::character('x'), Key::Character('x'));
}

#[test]
fn key_named_constructor() {
    assert_eq!(Key::named(NamedKey::Enter), Key::Named(NamedKey::Enter));
}

// =========================================================================
// NamedKey kitty codes: spot check a range of keys
// =========================================================================

#[test]
fn named_key_kitty_codes_control_keys() {
    assert_eq!(NamedKey::Escape.kitty_code(), 27);
    assert_eq!(NamedKey::Enter.kitty_code(), 13);
    assert_eq!(NamedKey::Tab.kitty_code(), 9);
    assert_eq!(NamedKey::Backspace.kitty_code(), 127);
    assert_eq!(NamedKey::Space.kitty_code(), 32);
}

#[test]
fn named_key_kitty_codes_navigation() {
    assert_eq!(NamedKey::ArrowUp.kitty_code(), 57352);
    assert_eq!(NamedKey::ArrowDown.kitty_code(), 57353);
    assert_eq!(NamedKey::ArrowLeft.kitty_code(), 57350);
    assert_eq!(NamedKey::ArrowRight.kitty_code(), 57351);
    assert_eq!(NamedKey::Home.kitty_code(), 57356);
    assert_eq!(NamedKey::End.kitty_code(), 57357);
    assert_eq!(NamedKey::PageUp.kitty_code(), 57354);
    assert_eq!(NamedKey::PageDown.kitty_code(), 57355);
}

#[test]
fn named_key_kitty_codes_editing() {
    assert_eq!(NamedKey::Insert.kitty_code(), 57348);
    assert_eq!(NamedKey::Delete.kitty_code(), 57349);
}

#[test]
fn named_key_kitty_codes_function_keys() {
    assert_eq!(NamedKey::F1.kitty_code(), 57364);
    assert_eq!(NamedKey::F12.kitty_code(), 57375);
    assert_eq!(NamedKey::F24.kitty_code(), 57387);
    assert_eq!(NamedKey::F35.kitty_code(), 57398);
}

#[test]
fn named_key_kitty_codes_system_media_and_modifiers() {
    assert_eq!(NamedKey::CapsLock.kitty_code(), 57358);
    assert_eq!(NamedKey::ScrollLock.kitty_code(), 57359);
    assert_eq!(NamedKey::MediaPlay.kitty_code(), 57428);
    assert_eq!(NamedKey::ShiftLeft.kitty_code(), 57441);
    assert_eq!(NamedKey::MetaRight.kitty_code(), 57452);
}

#[test]
fn named_key_kitty_codes_numpad() {
    assert_eq!(NamedKey::Numpad0.kitty_code(), 57399);
    assert_eq!(NamedKey::Numpad9.kitty_code(), 57408);
    assert_eq!(NamedKey::NumpadEnter.kitty_code(), 57414);
    assert_eq!(NamedKey::NumpadAdd.kitty_code(), 57413);
    assert_eq!(NamedKey::NumpadEqual.kitty_code(), 57415);
    assert_eq!(NamedKey::NumpadArrowUp.kitty_code(), 57419);
    assert_eq!(NamedKey::NumpadDelete.kitty_code(), 57426);
}

// =========================================================================
// encode_key delegates to encode_key_with_event(Press)
// =========================================================================

#[test]
fn encode_key_equals_encode_key_with_event_press() {
    let keys = [
        Key::Character('a'),
        Key::Named(NamedKey::Enter),
        Key::Named(NamedKey::ArrowUp),
        Key::Named(NamedKey::F5),
    ];
    let modes = [
        KeyboardMode::empty(),
        KeyboardMode::DISAMBIGUATE_ESC_CODES,
        KeyboardMode::APP_CURSOR,
    ];
    for key in &keys {
        for mode in modes {
            let a = encode_key(key, Modifiers::empty(), mode);
            let b = encode_key_with_event(key, Modifiers::empty(), mode, KeyEventType::Press);
            assert_eq!(a, b, "mismatch for {key:?} mode={mode:?}");
        }
    }
}

// Kitty progressive-enhancement flag tests (flag presence, encoding behavior,
// edge cases, and combined flag interactions) extracted to a dedicated file
// to keep this module under the 1000-line limit.
#[path = "kitty_progressive_tests.rs"]
mod kitty_progressive;

#[test]
fn disambiguate_plain_keys_are_not_over_escaped() {
    // Regression (HIGH): with only DISAMBIGUATE_ESC_CODES set, unmodified
    // printable keys and the legacy text keys must emit their normal bytes
    // instead of being routed through the CSI-u encoder.
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
    // Keys that genuinely need disambiguation still use CSI-u.
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
    // Regression (HIGH): CapsLock/NumLock folded into the modifier set must not
    // turn a bare navigation/function key into its modified CSI form
    // (e.g. Up -> ESC[1;1A, F5 -> ESC[15;1~).
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
    // A real chord modifier alongside the lock bit still encodes normally.
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::ArrowUp),
            Modifiers::SHIFT | Modifiers::CAPS_LOCK,
            mode
        ),
        b"\x1b[1;2A"
    );
}
