// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy keyboard encoding tests.

use super::*;

// =========================================================================
// Legacy encoding: character keys
// =========================================================================

#[test]
fn legacy_encode_plain_character() {
    let result = encode_key(
        &Key::Character('a'),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"a");
}

#[test]
fn legacy_encode_shift_character() {
    let result = encode_key(
        &Key::Character('a'),
        Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"A");
}

#[test]
fn legacy_encode_ctrl_c() {
    // Ctrl+C = ETX (0x03)
    let result = encode_key(&Key::Character('c'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x03]);
}

#[test]
fn legacy_encode_ctrl_a() {
    // Ctrl+A = SOH (0x01)
    let result = encode_key(&Key::Character('a'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x01]);
}

/// Regression (K-1 "Shift doesn't work"): SHIFT on a NON-letter must yield the
/// shifted glyph in legacy mode. The old `to_ascii_uppercase` no-op'd on every
/// digit/symbol, so Shift+2 emitted '2' instead of '@' and shifted symbols were
/// impossible to type. Letters already worked (`legacy_encode_shift_character`),
/// which is exactly why the gap survived — no test ever pressed Shift on a symbol.
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

/// Meta (Alt) + Shift on a symbol sends ESC + the SHIFTED glyph, not ESC + the
/// base symbol. Same root cause as `legacy_encode_shift_symbol`, the ALT branch.
#[test]
fn legacy_encode_alt_shift_symbol() {
    let result = encode_key(
        &Key::Character('2'),
        Modifiers::ALT | Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, b'@']);
}

#[test]
fn legacy_encode_alt_character() {
    let result = encode_key(&Key::Character('a'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'a']);
}

#[test]
fn legacy_encode_alt_shift_character() {
    let result = encode_key(
        &Key::Character('a'),
        Modifiers::ALT | Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, b'A']);
}

#[test]
fn legacy_encode_ctrl_alt_character() {
    // Ctrl+Alt+C = ESC + Ctrl-C
    let result = encode_key(
        &Key::Character('c'),
        Modifiers::CTRL | Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, 0x03]);
}

// =========================================================================
// Legacy encoding: named keys
// =========================================================================

#[test]
fn legacy_encode_enter() {
    let result = encode_key(
        &Key::Named(NamedKey::Enter),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x0d]);
}

#[test]
fn legacy_encode_alt_enter() {
    let result = encode_key(
        &Key::Named(NamedKey::Enter),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, 0x0d]);
}

#[test]
fn legacy_encode_tab() {
    let result = encode_key(
        &Key::Named(NamedKey::Tab),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x09]);
}

#[test]
fn legacy_encode_shift_tab() {
    // Shift+Tab = CSI Z (back-tab)
    let result = encode_key(
        &Key::Named(NamedKey::Tab),
        Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, b'[', b'Z']);
}

#[test]
fn legacy_encode_escape() {
    let result = encode_key(
        &Key::Named(NamedKey::Escape),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b]);
}

#[test]
fn legacy_encode_backspace() {
    let result = encode_key(
        &Key::Named(NamedKey::Backspace),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x7f]);
}

#[test]
fn legacy_encode_ctrl_backspace() {
    let result = encode_key(
        &Key::Named(NamedKey::Backspace),
        Modifiers::CTRL,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x08]);
}

#[test]
fn legacy_encode_space() {
    let result = encode_key(
        &Key::Named(NamedKey::Space),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x20]);
}

#[test]
fn legacy_encode_ctrl_space() {
    // Ctrl+Space = NUL
    let result = encode_key(
        &Key::Named(NamedKey::Space),
        Modifiers::CTRL,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x00]);
}

// =========================================================================
// Legacy encoding: arrows with and without APP_CURSOR
// =========================================================================

#[test]
fn legacy_encode_arrow_up_normal() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    // Normal mode: CSI A
    assert_eq!(result, b"\x1b[A");
}

#[test]
fn legacy_encode_arrow_up_app_cursor() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::empty(),
        KeyboardMode::APP_CURSOR,
    );
    // Application cursor mode: SS3 A
    assert_eq!(result, b"\x1bOA");
}

#[test]
fn legacy_encode_arrow_down_normal() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowDown),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[B");
}

#[test]
fn legacy_encode_arrow_left_normal() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowLeft),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[D");
}

#[test]
fn legacy_encode_arrow_right_normal() {
    let result = encode_key(
        &Key::Named(NamedKey::ArrowRight),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[C");
}

#[test]
fn legacy_encode_arrow_with_shift_modifier() {
    // Shift+Up: CSI 1;2 A
    let result = encode_key(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[1;2A");
}

#[test]
fn legacy_encode_arrow_with_ctrl_modifier() {
    // Ctrl+Up: CSI 1;5 A
    let result = encode_key(
        &Key::Named(NamedKey::ArrowUp),
        Modifiers::CTRL,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[1;5A");
}

#[test]
fn legacy_encode_arrow_with_alt_modifier() {
    // Alt+Right: CSI 1;3 C — the exact sequence that caused #6631 when the
    // shell lacked bindings for xterm-style modified arrow keys.
    let result = encode_key(
        &Key::Named(NamedKey::ArrowRight),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[1;3C");
}

#[test]
fn legacy_encode_arrow_with_alt_left() {
    // Alt+Left: CSI 1;3 D
    let result = encode_key(
        &Key::Named(NamedKey::ArrowLeft),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[1;3D");
}

// =========================================================================
// Legacy encoding: function keys
// =========================================================================

#[test]
fn legacy_encode_f1_no_modifiers() {
    // F1: SS3 P
    let result = encode_key(
        &Key::Named(NamedKey::F1),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1bOP");
}

#[test]
fn legacy_encode_f1_with_shift() {
    // Shift+F1: CSI 1;2 P
    let result = encode_key(
        &Key::Named(NamedKey::F1),
        Modifiers::SHIFT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[1;2P");
}

#[test]
fn legacy_encode_f5() {
    // F5: CSI 15 ~
    let result = encode_key(
        &Key::Named(NamedKey::F5),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[15~");
}

#[test]
fn legacy_encode_f12() {
    // F12: CSI 24 ~
    let result = encode_key(
        &Key::Named(NamedKey::F12),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[24~");
}

// =========================================================================
// Legacy encoding: editing keys
// =========================================================================

#[test]
fn legacy_encode_home() {
    let result = encode_key(
        &Key::Named(NamedKey::Home),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[H");
}

#[test]
fn legacy_encode_end() {
    let result = encode_key(
        &Key::Named(NamedKey::End),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[F");
}

#[test]
fn legacy_encode_page_up() {
    let result = encode_key(
        &Key::Named(NamedKey::PageUp),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[5~");
}

#[test]
fn legacy_encode_delete() {
    let result = encode_key(
        &Key::Named(NamedKey::Delete),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[3~");
}

#[test]
fn legacy_encode_insert() {
    let result = encode_key(
        &Key::Named(NamedKey::Insert),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b[2~");
}

// =========================================================================
// Legacy encoding: numpad
// =========================================================================

#[test]
fn legacy_encode_numpad0_normal() {
    let result = encode_key(
        &Key::Named(NamedKey::Numpad0),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"0");
}

#[test]
fn legacy_encode_numpad0_app_keypad() {
    let result = encode_key(
        &Key::Named(NamedKey::Numpad0),
        Modifiers::empty(),
        KeyboardMode::APP_KEYPAD,
    );
    // SS3 p
    assert_eq!(result, b"\x1bOp");
}

#[test]
fn legacy_encode_numpad_enter() {
    let result = encode_key(
        &Key::Named(NamedKey::NumpadEnter),
        Modifiers::empty(),
        KeyboardMode::empty(),
    );
    // NumpadEnter in legacy = same as Enter (0x0d)
    assert_eq!(result, vec![0x0d]);
}

#[test]
fn legacy_encode_numpad_enter_app_keypad() {
    // NumpadEnter in DECKPAM sends SS3 M, distinguishing from main Enter (#7558).
    let result = encode_key(
        &Key::Named(NamedKey::NumpadEnter),
        Modifiers::empty(),
        KeyboardMode::APP_KEYPAD,
    );
    assert_eq!(result, b"\x1bOM");
}

#[test]
fn legacy_encode_numpad_enter_app_keypad_shift_cancels() {
    // Shift cancels application keypad mode — NumpadEnter reverts to CR (#7558).
    let result = encode_key(
        &Key::Named(NamedKey::NumpadEnter),
        Modifiers::SHIFT,
        KeyboardMode::APP_KEYPAD,
    );
    assert_eq!(result, vec![0x0d]);
}

#[test]
fn legacy_encode_numpad_enter_alt() {
    // Alt+NumpadEnter sends ESC+CR, same as Alt+Enter.
    let result = encode_key(
        &Key::Named(NamedKey::NumpadEnter),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, 0x0d]);
}

#[test]
fn legacy_encode_numpad_equal_matches_character_fallback() {
    let result = encode_key(
        &Key::Named(NamedKey::NumpadEqual),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, b"\x1b=");
}

// =========================================================================
// Emacs keybindings — complete coverage
// =========================================================================

#[test]
fn legacy_ctrl_space_sends_nul() {
    // Ctrl+Space = NUL (0x00) — set mark in emacs
    let result = encode_key(&Key::Character(' '), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x00]);
}

#[test]
fn legacy_ctrl_slash_sends_us() {
    // Ctrl+/ = US (0x1F) — undo in readline
    let result = encode_key(&Key::Character('/'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x1f]);
}

#[test]
fn legacy_ctrl_2_sends_nul() {
    // Ctrl+2 = NUL (0x00) — alias for Ctrl-@
    let result = encode_key(&Key::Character('2'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x00]);
}

#[test]
fn legacy_ctrl_6_sends_rs() {
    // Ctrl+6 = RS (0x1E) — alias for Ctrl-^
    let result = encode_key(&Key::Character('6'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x1e]);
}

#[test]
fn legacy_ctrl_8_sends_del() {
    // Ctrl+8 = DEL (0x7F) — alias for Ctrl-?
    let result = encode_key(&Key::Character('8'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x7f]);
}

#[test]
fn legacy_all_emacs_ctrl_chars() {
    // Comprehensive: every Ctrl+letter produces the correct control character
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
    // Meta+f = ESC f (forward word)
    let result = encode_key(&Key::Character('f'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'f']);

    // Meta+b = ESC b (backward word)
    let result = encode_key(&Key::Character('b'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'b']);

    // Meta+d = ESC d (kill word forward)
    let result = encode_key(&Key::Character('d'), Modifiers::ALT, KeyboardMode::empty());
    assert_eq!(result, vec![0x1b, b'd']);
}

#[test]
fn legacy_meta_backspace_sends_esc_del() {
    // Meta-Backspace = ESC + DEL (0x1B 0x7F) — kill word backward in readline
    let result = encode_key(
        &Key::Named(NamedKey::Backspace),
        Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, 0x7f]);
}

#[test]
fn legacy_ctrl_alt_combination() {
    // Ctrl+Alt+a = ESC + 0x01 (used in some emacs modes)
    let result = encode_key(
        &Key::Character('a'),
        Modifiers::CTRL | Modifiers::ALT,
        KeyboardMode::empty(),
    );
    assert_eq!(result, vec![0x1b, 0x01]);
}

#[test]
fn legacy_ctrl_x_sends_can() {
    // Ctrl+X = CAN (0x18) — prefix key in emacs readline
    let result = encode_key(&Key::Character('x'), Modifiers::CTRL, KeyboardMode::empty());
    assert_eq!(result, vec![0x18]);
}

// =========================================================================
// VT52 mode: cursor keys (#7712)
// =========================================================================

#[test]
fn test_vt52_cursor_keys() {
    let mode = KeyboardMode::VT52_MODE;
    // Up: ESC A
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1bA"
    );
    // Down: ESC B
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowDown), Modifiers::empty(), mode),
        b"\x1bB"
    );
    // Right: ESC C
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowRight), Modifiers::empty(), mode),
        b"\x1bC"
    );
    // Left: ESC D
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowLeft), Modifiers::empty(), mode),
        b"\x1bD"
    );
}

#[test]
fn test_vt52_mode_overrides_decckm() {
    // VT52 mode takes priority over DECCKM (APP_CURSOR).
    // With APP_CURSOR alone, arrows use SS3 (ESC O A), but VT52 forces ESC A.
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
    // VT52 mode ignores modifiers — always produces bare ESC + letter.
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
fn test_vt52_numpad_application_mode() {
    // VT52 application keypad: ESC ? followed by the digit character.
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
    // VT52 mode without APP_KEYPAD: numpad sends normal digits (no ESC ?).
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

// Release events in legacy mode
// =========================================================================

#[test]
fn legacy_release_event_returns_empty() {
    let result = encode_key_with_event(
        &Key::Character('a'),
        Modifiers::empty(),
        KeyboardMode::empty(),
        KeyEventType::Release,
    );
    assert!(result.is_empty());
}

// =========================================================================
// DECBKM (mode 67): Backspace sends BS (0x08) vs DEL (0x7f)
// =========================================================================

#[test]
fn legacy_backspace_default_sends_del() {
    // Default (DECBKM reset): Backspace -> DEL (0x7f), Ctrl+Backspace -> BS.
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
    // DECBKM set: Backspace -> BS (0x08), Ctrl inverts to DEL, Alt ESC-prefixes.
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

// =========================================================================
// xterm keyboard private modes 1035/1036/1039 (numLock / metaSendsEscape /
// altSendsEscape). These are folded into KeyboardMode by aterm-core's
// keyboard_mode_from_state; here we exercise the encoder-facing flags directly.
// =========================================================================

#[test]
fn legacy_alt_sends_escape_default_prefixes_esc() {
    // Mode 1039 SET (default): Alt+key -> ESC + key. Matches the historical
    // `empty()` contract (no ALT_NO_ESC flag present).
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::ALT, KeyboardMode::empty()),
        vec![0x1b, b'a']
    );
}

#[test]
fn legacy_alt_no_esc_suppresses_esc_prefix() {
    // Mode 1039 RESET (ALT_NO_ESC): Alt+key -> bare key, no ESC prefix.
    let mode = KeyboardMode::ALT_NO_ESC;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::ALT, mode),
        vec![b'a']
    );
    // Ctrl+Alt still folds to the control byte, but without the ESC prefix.
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL | Modifiers::ALT, mode),
        vec![0x03]
    );
}

#[test]
fn legacy_meta_sends_escape_off_by_default() {
    // Without META_SENDS_ESC, a Meta-modified key is unhandled in the legacy
    // path and falls through to the plain glyph (prior behavior).
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::META, KeyboardMode::empty()),
        vec![b'a']
    );
}

#[test]
fn legacy_meta_sends_escape_on_prefixes_esc() {
    // Mode 1036 SET (META_SENDS_ESC): Meta+key -> ESC + key, mirroring Alt.
    let mode = KeyboardMode::META_SENDS_ESC;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::META, mode),
        vec![0x1b, b'a']
    );
    // Ctrl+Meta folds to the control byte WITH the ESC prefix.
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
    // Mode 1035 RESET (NO_SPECIAL_MODIFIERS): the NumLock modifier bit is
    // dropped before encoding, so NumLock alone behaves like no modifier.
    let mode = KeyboardMode::NO_SPECIAL_MODIFIERS;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::NUM_LOCK, mode),
        vec![b'a']
    );
    // With NumLock kept as a special modifier (default), it would NOT be
    // stripped — but it is also not a legacy chord, so the bare glyph results
    // either way for a plain character; the load-bearing case is that the flag
    // does not corrupt an accompanying real modifier:
    assert_eq!(
        encode_key(
            &Key::Character('a'),
            Modifiers::NUM_LOCK | Modifiers::ALT,
            mode,
        ),
        vec![0x1b, b'a']
    );
}
