// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for the shared keyboard encoding module.

use super::*;

// =========================================================================
// KeyboardMode helpers
// =========================================================================

#[test]
fn mode_xterm_modify_other_keys_level_default() {
    assert_eq!(KeyboardMode::empty().xterm_modify_other_keys_level(), 0);
}

#[test]
fn mode_xterm_modify_other_keys_level1() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    assert_eq!(mode.xterm_modify_other_keys_level(), 1);
}

#[test]
fn mode_xterm_modify_other_keys_level2() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(mode.xterm_modify_other_keys_level(), 2);
}

#[test]
fn mode_xterm_modify_other_keys_both_levels_returns_2() {
    // Level 2 takes precedence when both flags are set.
    let mode =
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1 | KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(mode.xterm_modify_other_keys_level(), 2);
}

#[test]
fn mode_xterm_format_other_keys_false_by_default() {
    assert!(!KeyboardMode::empty().xterm_format_other_keys());
}

#[test]
fn mode_xterm_format_other_keys_true() {
    assert!(KeyboardMode::XTERM_FORMAT_OTHER_KEYS.xterm_format_other_keys());
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
fn modifiers_kitty_encoded_no_mods() {
    assert_eq!(Modifiers::empty().kitty_encoded(), 1);
}

#[test]
fn modifiers_kitty_encoded_shift() {
    assert_eq!(Modifiers::SHIFT.kitty_encoded(), 2);
}

#[test]
fn modifiers_kitty_encoded_ctrl_alt() {
    assert_eq!((Modifiers::CTRL | Modifiers::ALT).kitty_encoded(), 7);
}

#[test]
fn modifiers_kitty_encoded_caps_lock() {
    // WIRE-MODIFIERS: Caps Lock (bit 64) reports as kitty value 65, and combines
    // with chord modifiers (Shift+CapsLock -> 1 + 64 + shift(1) = 66).
    assert_eq!(Modifiers::CAPS_LOCK.kitty_encoded(), 65);
    assert_eq!(
        (Modifiers::SHIFT | Modifiers::CAPS_LOCK).kitty_encoded(),
        66
    );
}

#[test]
fn modifiers_kitty_encoded_num_lock() {
    // Num Lock (bit 128) reports as kitty value 129.
    assert_eq!(Modifiers::NUM_LOCK.kitty_encoded(), 129);
}

#[test]
fn modifiers_xterm_ignores_locks() {
    // Legacy xterm modifier encoding does NOT report Caps/Num Lock.
    assert_eq!(Modifiers::CAPS_LOCK.xterm_encoded(), 1);
    assert_eq!(Modifiers::NUM_LOCK.xterm_encoded(), 1);
}

#[test]
fn modifiers_xterm_encoded_no_mods() {
    assert_eq!(Modifiers::empty().xterm_encoded(), 1);
}

#[test]
fn modifiers_xterm_encoded_shift() {
    assert_eq!(Modifiers::SHIFT.xterm_encoded(), 2);
}

#[test]
fn modifiers_xterm_encoded_alt() {
    assert_eq!(Modifiers::ALT.xterm_encoded(), 3);
}

#[test]
fn modifiers_xterm_encoded_ctrl() {
    assert_eq!(Modifiers::CTRL.xterm_encoded(), 5);
}

#[test]
fn modifiers_xterm_encoded_shift_alt_ctrl() {
    assert_eq!(
        (Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL).xterm_encoded(),
        8
    );
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
fn kitty_encode_character_no_mods() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Character('a'), Modifiers::empty(), mode);
    // Disambiguate must NOT over-escape a plain printable key: it stays text.
    assert_eq!(result, b"a");
}

#[test]
fn kitty_encode_character_with_shift_stays_text() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Character('a'), Modifiers::SHIFT, mode);
    // Shift+key is NOT in the spec's disambiguation list ("the Esc, alt+key,
    // ctrl+key, ctrl+alt+key, shift+alt+key keys") — it composes text, and
    // kitty sends the shifted glyph in every text-preserving mode.
    assert_eq!(result, b"A");
}

#[test]
fn kitty_encode_shifted_symbol_stays_text() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Character('2'), Modifiers::SHIFT, mode);
    assert_eq!(result, b"@");
}

#[test]
fn kitty_encode_character_with_ctrl() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Character('c'), Modifiers::CTRL, mode);
    // CSI 99;5 u (ctrl = mod value 5)
    assert_eq!(result, b"\x1b[99;5u");
}

#[test]
fn kitty_encode_named_enter() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode);
    // Enter has a legacy text byte and keeps it under disambiguate (#audit).
    assert_eq!(result, b"\r");
}

#[test]
fn kitty_encode_named_escape() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::Escape), Modifiers::empty(), mode);
    // Escape kitty code = 27
    assert_eq!(result, b"\x1b[27u");
}

#[test]
fn kitty_encode_named_arrow_up() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode);
    // ArrowUp retains legacy CSI format under Kitty protocol (#7474)
    assert_eq!(result, b"\x1b[A");
}

#[test]
fn kitty_encode_f1() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::F1), Modifiers::empty(), mode);
    // F1 retains legacy CSI format under Kitty protocol (#7474)
    assert_eq!(result, b"\x1b[P");
}

#[test]
fn kitty_encode_f25() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::F25), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[57388u");
}

#[test]
fn kitty_encode_scroll_lock_silent_without_report_all() {
    // Modifier/lock keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC
    // (kitty's is_modifier_key gate covers ScrollLock/CapsLock/NumLock too).
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::ScrollLock), Modifiers::empty(), mode);
    assert!(result.is_empty());
}

#[test]
fn kitty_encode_scroll_lock_reported_under_report_all() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    let result = encode_key(&Key::Named(NamedKey::ScrollLock), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[57359u");
}

#[test]
fn kitty_encode_media_play() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::MediaPlay), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[57428u");
}

#[test]
fn kitty_encode_numpad_equal() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(&Key::Named(NamedKey::NumpadEqual), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[57415u");
}

#[test]
fn kitty_encode_numpad_arrow_up() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key(
        &Key::Named(NamedKey::NumpadArrowUp),
        Modifiers::empty(),
        mode,
    );
    assert_eq!(result, b"\x1b[57419u");
}

// =========================================================================
// Kitty with event types
// =========================================================================

#[test]
fn kitty_encode_repeat_event_with_report_flag_stays_text() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Repeat,
    );
    // Text-producing presses AND repeats stay plain UTF-8 under 0b11 — the
    // event-type subfield only decorates events that already need an escape
    // form (kitty sends 'a' here; its plain-text release stays silent).
    assert_eq!(result, b"a");
}

#[test]
fn kitty_encode_ctrl_chord_repeat_with_report_flag_carries_event_type() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::CTRL,
        mode,
        KeyEventType::Repeat,
    );
    // A chord that disambiguates gets the full CSI-u with `:2` (repeat).
    assert_eq!(result, b"\x1b[97;5:2u");
}

#[test]
fn kitty_text_release_with_report_flag_stays_silent_without_report_all() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Release,
    );
    // Text-producing events have no release representation unless the app
    // also requests REPORT_ALL_KEYS_AS_ESC.
    assert!(result.is_empty());
}

#[test]
fn kitty_encode_press_event_with_report_flag_stays_text() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Press,
    );
    // Spec: event-type reporting adds repeat/release reports WITHOUT changing
    // press encoding — a plain printable press is still text under 0b11.
    assert_eq!(result, b"a");
}

#[test]
fn kitty_encode_release_without_report_flag_is_empty() {
    // Kitty spec: releases are reported ONLY under REPORT_EVENT_TYPES. Under
    // disambiguate-only a release must encode to
    // NOTHING — the CSI-u encoder would omit the `:3` subfield in this mode,
    // making the release byte-identical to a press and doubling every key.
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Release,
    );
    assert!(result.is_empty());
}

#[test]
fn kitty_encode_named_release_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::empty(),
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    // ArrowUp release retains legacy CSI format with event type (#7474)
    assert_eq!(result, b"\x1b[1;1:3A");
}

#[test]
fn kitty_encode_named_repeat_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Named(NamedKey::Escape),
        Modifiers::empty(),
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Repeat,
    );
    assert_eq!(result, b"\x1b[27;1:2u");
}

#[test]
fn kitty_encode_character_release_with_report_flag_only_is_empty() {
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    // REPORT_EVENT_TYPES cannot add an event type to plain UTF-8 text. The
    // release exists only once REPORT_ALL_KEYS_AS_ESC makes the key a report.
    assert!(result.is_empty());
}

#[test]
fn kitty_encode_ctrl_character_release_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::CTRL,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    assert_eq!(result, b"\x1b[97;5:3u");
}

#[test]
fn kitty_encode_modified_character_repeat_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::CTRL | Modifiers::SHIFT,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Repeat,
    );
    assert_eq!(result, b"\x1b[97;6:2u");
}

#[test]
fn kitty_encode_modified_character_release_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::CTRL | Modifiers::SHIFT,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    assert_eq!(result, b"\x1b[97;6:3u");
}

#[test]
fn kitty_encode_shift_tab_repeat_event_with_report_flag_only() {
    let result = encode_key_with_event(
        &Key::Named(NamedKey::Tab),
        Modifiers::SHIFT,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Repeat,
    );
    assert_eq!(result, b"\x1b[9;2:2u");
}

#[test]
fn kitty_encode_shift_tab_release_with_report_flag_only_is_empty() {
    let result = encode_key_with_event(
        &Key::Named(NamedKey::Tab),
        Modifiers::SHIFT,
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    assert!(result.is_empty());
}

#[test]
fn kitty_encode_enter_release_event_with_report_flag_only_is_empty() {
    let result = encode_key_with_event(
        &Key::Named(NamedKey::Enter),
        Modifiers::empty(),
        KeyboardMode::REPORT_EVENT_TYPES,
        KeyEventType::Release,
    );
    assert!(result.is_empty());
}

#[test]
fn legacy_press_remains_legacy_with_report_flag_only() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::empty(),
        KeyboardMode::REPORT_EVENT_TYPES,
    );
    assert_eq!(result, b"\x1b[A");
}

#[test]
fn kitty_encode_modifier_plus_release() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::CTRL,
        mode,
        KeyEventType::Release,
    );
    // CSI 97;5:3 u (ctrl=5, release=3)
    assert_eq!(result, b"\x1b[97;5:3u");
}

#[test]
fn kitty_encode_shift_left_silent_without_report_all() {
    // Modifier keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC; under
    // 0b11 both press and release must be silent.
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    for (mods, event) in [
        (Modifiers::empty(), KeyEventType::Press),
        (Modifiers::SHIFT, KeyEventType::Release),
    ] {
        let result = encode_key_with_event(&Key::Named(NamedKey::ShiftLeft), mods, mode, event);
        assert!(result.is_empty(), "bare Shift {event:?} must be silent");
    }
}

#[test]
fn kitty_encode_shift_left_press_updates_modifiers_under_report_all() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    let result = encode_key_with_event(
        &Key::Named(NamedKey::ShiftLeft),
        Modifiers::empty(),
        mode,
        KeyEventType::Press,
    );
    // The bit reflects the state INCLUDING the current event: set on press.
    assert_eq!(result, b"\x1b[57441;2u");
}

#[test]
fn kitty_encode_shift_left_release_updates_modifiers_under_report_all() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_EVENT_TYPES;
    let result = encode_key_with_event(
        &Key::Named(NamedKey::ShiftLeft),
        Modifiers::SHIFT,
        mode,
        KeyEventType::Release,
    );
    assert_eq!(result, b"\x1b[57441;1:3u");
}

// =========================================================================
// xterm modifyOtherKeys
// =========================================================================

#[test]
fn xterm_modify_other_keys_level1_alt_char() {
    // Level 1: only Alt triggers modifyOtherKeys encoding
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let result = encode_key(&Key::Character('a'), Modifiers::ALT, mode);
    // Default format: CSI 27;3;97 ~ (mod=3=alt, code=97='a')
    assert_eq!(result, b"\x1b[27;3;97~");
}

#[test]
fn xterm_modify_other_keys_level1_ctrl_char_falls_through() {
    // Level 1 with only Ctrl: does NOT use modifyOtherKeys, falls to legacy
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let result = encode_key(&Key::Character('c'), Modifiers::CTRL, mode);
    // Legacy Ctrl+C = 0x03
    assert_eq!(result, vec![0x03]);
}

#[test]
fn xterm_modify_other_keys_level2_ctrl_char() {
    // Level 2: any modifier triggers modifyOtherKeys encoding
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Character('c'), Modifiers::CTRL, mode);
    // Default format: CSI 27;5;99 ~ (mod=5=ctrl, code=99='c')
    assert_eq!(result, b"\x1b[27;5;99~");
}

#[test]
fn xterm_modify_other_keys_level2_shift_char() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Character('a'), Modifiers::SHIFT, mode);
    // Default format: CSI 27;2;97 ~ (mod=2=shift, code=97='a')
    assert_eq!(result, b"\x1b[27;2;97~");
}

#[test]
fn xterm_modify_other_keys_format_other_keys() {
    // formatOtherKeys=1: CSI code ; modifier u
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2 | KeyboardMode::XTERM_FORMAT_OTHER_KEYS;
    let result = encode_key(&Key::Character('a'), Modifiers::CTRL, mode);
    // CSI 97;5 u (code=97='a', mod=5=ctrl)
    assert_eq!(result, b"\x1b[97;5u");
}

#[test]
fn xterm_modify_other_keys_named_enter_with_alt() {
    // modifyOtherKeys applies to specific named keys too (Enter → code 13)
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL1;
    let result = encode_key(&Key::Named(NamedKey::Enter), Modifiers::ALT, mode);
    // CSI 27;3;13 ~
    assert_eq!(result, b"\x1b[27;3;13~");
}

#[test]
fn xterm_modify_other_keys_tab_level2_shift() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Named(NamedKey::Tab), Modifiers::SHIFT, mode);
    // CSI 27;2;9 ~
    assert_eq!(result, b"\x1b[27;2;9~");
}

#[test]
fn xterm_modify_other_keys_no_effect_on_arrows() {
    // Arrow keys are NOT in the modifyOtherKeys subset — falls through to legacy
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::CTRL, mode);
    // Legacy Ctrl+Up: CSI 1;5 A
    assert_eq!(result, b"\x1b[1;5A");
}

#[test]
fn xterm_modify_other_keys_no_mods_falls_through() {
    // No modifiers → modifyOtherKeys doesn't apply
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Character('a'), Modifiers::empty(), mode);
    assert_eq!(result, b"a");
}

#[test]
fn xterm_modify_other_keys_numpad_equal_preserves_character_behavior() {
    let mode = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    let result = encode_key(&Key::Named(NamedKey::NumpadEqual), Modifiers::ALT, mode);
    assert_eq!(result, b"\x1b[27;3;61~");
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
