// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy keyboard encoding tests.

use super::*;

// =========================================================================
// Legacy encoding: character keys
// =========================================================================

#[test]
#[rustfmt::skip]
fn legacy_character_encodings() {
    let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
    assert_cases(&[
        ("plain", ch('a'), NO_MODS, LEGACY, KEY, b"a"),
        ("shift", ch('a'), Modifiers::SHIFT, LEGACY, KEY, b"A"),
        // Meta (Alt) + Shift on a symbol sends ESC + the SHIFTED glyph, not ESC +
        // the base symbol. Same root cause as `legacy_encode_shift_symbol`, the
        // ALT branch.
        ("alt+shift symbol", ch('2'), alt_shift, LEGACY, KEY, &[0x1b, b'@']),
        // Mode 1039 SET (default): Alt+key -> ESC + key. Matches the historical
        // `empty()` contract (no ALT_NO_ESC flag present).
        ("alt", ch('a'), Modifiers::ALT, LEGACY, KEY, &[0x1b, b'a']),
        ("alt+shift", ch('a'), alt_shift, LEGACY, KEY, &[0x1b, b'A']),
        // Ctrl+Alt+C = ESC + Ctrl-C
        ("ctrl+alt", ch('c'), Modifiers::CTRL | Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x03]),
        // Without META_SENDS_ESC, a Meta-modified key is unhandled in the legacy
        // path and falls through to the plain glyph (prior behavior).
        ("meta, 1036 reset", ch('a'), Modifiers::META, LEGACY, KEY, b"a"),
        // Release events in legacy mode encode to nothing.
        ("release", ch('a'), NO_MODS, LEGACY, RELEASE, b""),
    ]);
}

/// Regression (owner, 2026-07-28: "does control-_ work as undo like in emacs
/// now? it doesn't seem to work"): Ctrl-`_` must emit US (0x1F), the byte
/// readline and emacs bind to `undo`.
///
/// `Key::Character` carries the UNSHIFTED base, so the chord arrives as
/// `Ctrl+Shift+'-'`. `ctrl_character` keys on `'_'`, never `'-'`, so the lookup
/// missed and the chord fell through to the plain-glyph path — emitting a
/// literal `_` (0x5F). Undo silently did nothing.
///
/// The sibling chords are asserted alongside it because they are the reason the
/// gap survived this long: each has its UNSHIFTED character in the table too, so
/// they resolved before shift ever mattered and made the area look covered.
/// They also pin that the fallback is ADDITIVE — `Ctrl+Shift+'/'` must stay
/// 0x1F, not silently become `Ctrl-?` (0x7F).
#[test]
fn legacy_encode_ctrl_underscore_is_us_for_emacs_undo() {
    let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
    assert_eq!(
        encode_key(&Key::Character('-'), ctrl_shift, KeyboardMode::empty()),
        vec![0x1f],
        "Ctrl-_ is US (0x1F) — the readline/emacs undo byte"
    );

    // Unchanged siblings: the fallback must not renegotiate any of these.
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

    // Without SHIFT there is no shifted glyph to fall back to, so `-` keeps its
    // ordinary self — the fallback is reached only when the chord asks for it.
    assert_eq!(
        encode_key(&Key::Character('-'), Modifiers::CTRL, KeyboardMode::empty()),
        vec![b'-'],
        "plain Ctrl+- is untouched by the shifted fallback"
    );
}

/// Regression (K-1 "Shift doesn't work"): SHIFT on a NON-letter must yield the
/// shifted glyph in legacy mode. The old `to_ascii_uppercase` no-op'd on every
/// digit/symbol, so Shift+2 emitted '2' instead of '@' and shifted symbols were
/// impossible to type. Letters already worked (`legacy_character_encodings`),
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

// =========================================================================
// Legacy encoding: named keys
// =========================================================================

#[test]
#[rustfmt::skip]
fn legacy_control_and_editing_key_encodings() {
    assert_cases(&[
        ("enter", named(NamedKey::Enter), NO_MODS, LEGACY, KEY, &[0x0d]),
        ("alt enter", named(NamedKey::Enter), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x0d]),
        ("tab", named(NamedKey::Tab), NO_MODS, LEGACY, KEY, &[0x09]),
        // Shift+Tab = CSI Z (back-tab)
        ("shift tab", named(NamedKey::Tab), Modifiers::SHIFT, LEGACY, KEY, &[0x1b, b'[', b'Z']),
        ("escape", named(NamedKey::Escape), NO_MODS, LEGACY, KEY, &[0x1b]),
        ("backspace", named(NamedKey::Backspace), NO_MODS, LEGACY, KEY, &[0x7f]),
        ("ctrl backspace", named(NamedKey::Backspace), Modifiers::CTRL, LEGACY, KEY, &[0x08]),
        ("space", named(NamedKey::Space), NO_MODS, LEGACY, KEY, &[0x20]),
        // Ctrl+Space = NUL
        ("ctrl space", named(NamedKey::Space), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        ("home", named(NamedKey::Home), NO_MODS, LEGACY, KEY, b"\x1b[H"),
        ("end", named(NamedKey::End), NO_MODS, LEGACY, KEY, b"\x1b[F"),
        ("page up", named(NamedKey::PageUp), NO_MODS, LEGACY, KEY, b"\x1b[5~"),
        ("delete", named(NamedKey::Delete), NO_MODS, LEGACY, KEY, b"\x1b[3~"),
        ("insert", named(NamedKey::Insert), NO_MODS, LEGACY, KEY, b"\x1b[2~"),
    ]);
}

// =========================================================================
// Legacy encoding: arrows with and without APP_CURSOR, and function keys
// =========================================================================

#[test]
#[rustfmt::skip]
fn legacy_cursor_and_function_key_encodings() {
    let app_cursor = KeyboardMode::APP_CURSOR;
    assert_cases(&[
        // Normal mode: CSI A
        ("up", named(NamedKey::ArrowUp), NO_MODS, LEGACY, KEY, b"\x1b[A"),
        // Application cursor mode: SS3 A
        ("up, DECCKM", named(NamedKey::ArrowUp), NO_MODS, app_cursor, KEY, b"\x1bOA"),
        ("down", named(NamedKey::ArrowDown), NO_MODS, LEGACY, KEY, b"\x1b[B"),
        ("left", named(NamedKey::ArrowLeft), NO_MODS, LEGACY, KEY, b"\x1b[D"),
        ("right", named(NamedKey::ArrowRight), NO_MODS, LEGACY, KEY, b"\x1b[C"),
        // Shift+Up: CSI 1;2 A
        ("shift up", named(NamedKey::ArrowUp), Modifiers::SHIFT, LEGACY, KEY, b"\x1b[1;2A"),
        // Ctrl+Up: CSI 1;5 A
        ("ctrl up", named(NamedKey::ArrowUp), Modifiers::CTRL, LEGACY, KEY, b"\x1b[1;5A"),
        // Alt+Right: CSI 1;3 C — the exact sequence that caused #6631 when the
        // shell lacked bindings for xterm-style modified arrow keys.
        ("alt right", named(NamedKey::ArrowRight), Modifiers::ALT, LEGACY, KEY, b"\x1b[1;3C"),
        // Alt+Left: CSI 1;3 D
        ("alt left", named(NamedKey::ArrowLeft), Modifiers::ALT, LEGACY, KEY, b"\x1b[1;3D"),
        // F1: SS3 P
        ("f1", named(NamedKey::F1), NO_MODS, LEGACY, KEY, b"\x1bOP"),
        // Shift+F1: CSI 1;2 P
        ("shift f1", named(NamedKey::F1), Modifiers::SHIFT, LEGACY, KEY, b"\x1b[1;2P"),
        // F5: CSI 15 ~
        ("f5", named(NamedKey::F5), NO_MODS, LEGACY, KEY, b"\x1b[15~"),
        // F12: CSI 24 ~
        ("f12", named(NamedKey::F12), NO_MODS, LEGACY, KEY, b"\x1b[24~"),
    ]);
}

/// Shift+F10 is a REAL legacy sequence — `CSI 21;2 ~`, terminfo `kf22` (xterm
/// numbers Shift+F1..F12 as F13..F24) — which is why a host chord may not
/// claim it by default: an application binds it expecting these bytes.
#[test]
fn legacy_encode_shift_f10_is_terminfo_kf22() {
    // F10: CSI 21 ~
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::F10),
            Modifiers::empty(),
            KeyboardMode::empty(),
        ),
        b"\x1b[21~"
    );
    // Shift+F10: CSI 21;2 ~ — kf22
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::F10),
            Modifiers::SHIFT,
            KeyboardMode::empty(),
        ),
        b"\x1b[21;2~"
    );
}

// =========================================================================
// Legacy encoding: numpad
// =========================================================================

#[test]
#[rustfmt::skip]
fn legacy_numpad_encodings() {
    let app_keypad = KeyboardMode::APP_KEYPAD;
    assert_cases(&[
        ("kp0", named(NamedKey::Numpad0), NO_MODS, LEGACY, KEY, b"0"),
        // SS3 p
        ("kp0, DECKPAM", named(NamedKey::Numpad0), NO_MODS, app_keypad, KEY, b"\x1bOp"),
        // NumpadEnter in legacy = same as Enter (0x0d)
        ("kp enter", named(NamedKey::NumpadEnter), NO_MODS, LEGACY, KEY, &[0x0d]),
        // NumpadEnter in DECKPAM sends SS3 M, distinguishing from main Enter (#7558).
        ("kp enter, DECKPAM", named(NamedKey::NumpadEnter), NO_MODS, app_keypad, KEY, b"\x1bOM"),
        // Shift cancels application keypad mode (#7558) — and what is left is the
        // main Shift+Enter, aterm's LF imposition, NOT a bare CR: a physical
        // Shift+KP_Enter typed LF before the keypad seam told KP_Enter apart, and
        // the keypad's Enter is a second Return to the hand on it.
        (
            "shift kp enter, DECKPAM",
            named(NamedKey::NumpadEnter),
            Modifiers::SHIFT,
            app_keypad,
            KEY,
            &[0x0a],
        ),
        // Alt+NumpadEnter sends ESC+CR, same as Alt+Enter.
        ("alt kp enter", named(NamedKey::NumpadEnter), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x0d]),
        // Matches the character fallback for '='.
        ("alt kp equal", named(NamedKey::NumpadEqual), Modifiers::ALT, LEGACY, KEY, b"\x1b="),
    ]);
}

/// Outside application keypad mode KP_Enter is the main Enter byte for byte:
/// plain CR, Shift's LF, Ctrl's CR, Alt's ESC CR — the same table
/// `encode_control_named_legacy` keeps for Return, so the two cannot drift.
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
    // Application mode keeps its own forms — SS3 M, and ESC ? M under VT52.
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadEnter),
            Modifiers::empty(),
            KeyboardMode::APP_KEYPAD | KeyboardMode::VT52_MODE
        ),
        b"\x1b?M"
    );
}

// =========================================================================
// Emacs keybindings — complete coverage
// =========================================================================

#[test]
#[rustfmt::skip]
fn legacy_emacs_bindings() {
    assert_cases(&[
        // Ctrl+Space = NUL (0x00) — set mark in emacs
        ("C-SPC", ch(' '), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        // Ctrl+/ = US (0x1F) — undo in readline
        ("C-/", ch('/'), Modifiers::CTRL, LEGACY, KEY, &[0x1f]),
        // Ctrl+2 = NUL (0x00) — alias for Ctrl-@
        ("C-2", ch('2'), Modifiers::CTRL, LEGACY, KEY, &[0x00]),
        // Ctrl+6 = RS (0x1E) — alias for Ctrl-^
        ("C-6", ch('6'), Modifiers::CTRL, LEGACY, KEY, &[0x1e]),
        // Ctrl+8 = DEL (0x7F) — alias for Ctrl-?
        ("C-8", ch('8'), Modifiers::CTRL, LEGACY, KEY, &[0x7f]),
        // Meta-Backspace = ESC + DEL (0x1B 0x7F) — kill word backward in readline
        ("M-DEL", named(NamedKey::Backspace), Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x7f]),
        // Ctrl+Alt+a = ESC + 0x01 (used in some emacs modes)
        ("C-M-a", ch('a'), Modifiers::CTRL | Modifiers::ALT, LEGACY, KEY, &[0x1b, 0x01]),
        // Ctrl+X = CAN (0x18) — prefix key in emacs readline
        ("C-x", ch('x'), Modifiers::CTRL, LEGACY, KEY, &[0x18]),
    ]);

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

/// THE CENTRE KEY NEVER TYPES A DIGIT. KP_Begin is the NumLock-off centre of
/// the keypad: xterm's `kb2`, `CSI E` in normal mode and `SS3 E` under DECKPAM
/// — which this arm's own comment always said, while it emitted a bare `5`.
/// Nothing could reach it until the winit seam gave the GUI a road to
/// `NumpadBegin`; shipping the road and the digit together would have started
/// typing a stray `5` at the shell prompt where a NumLock-off KP_5 wrote
/// nothing before. The key has no glyph at all — that is why xkb calls it
/// `Unidentified` and why `main_block_twin` refuses it.
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
    // Shift cancels application keypad mode, as it does for every keypad key.
    assert_eq!(
        encode_key(&begin, Modifiers::SHIFT, KeyboardMode::APP_KEYPAD),
        b"\x1b[E"
    );
    // ALT is `altSendsEscape` on the sequence the key would otherwise send.
    assert_eq!(
        encode_key(&begin, Modifiers::ALT, KeyboardMode::empty()),
        b"\x1b\x1b[E"
    );
    // VT52 application keypad keeps its keypad-character form.
    assert_eq!(
        encode_key(
            &begin,
            Modifiers::empty(),
            KeyboardMode::APP_KEYPAD | KeyboardMode::VT52_MODE
        ),
        b"\x1b?5"
    );
    // The legacy byte and the kitty letter form agree, which is the point.
    assert_eq!(
        encode_key(
            &begin,
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES
        ),
        b"\x1b[E"
    );
}

/// NUMLOCK DOES NOT CANCEL DECKPAM — decided, not inherited. NumLock is a LOCK,
/// not a chord: `encode_named_legacy` already masks it out of the modifier
/// parameter (xterm's `numLock` resource does the same), and it must not reach
/// into application keypad mode either. The alternative was tried on paper and
/// refutes itself: a physical keypad only ever produces DIGITS while NumLock is
/// ON (with it off the keys are End/Down/PageDown/…), so "NumLock cancels
/// application mode" would put DECKPAM's `SS3 p..y` permanently out of reach of
/// a keyboard — reinstating the exact unreachability this change removes.
///
/// The visible consequence, accepted: inside a program that sets DECKPAM
/// (`smkx` — vim, less, tmux) the keypad digits now send `SS3 p..y` instead of
/// `0..9`. vim translates unmapped `<k0>`..`<k9>` back to their digits, so it
/// is unaffected; `less` does not, so a line number typed on the keypad no
/// longer reaches it. That is what xterm does with the same key, and the SS3
/// forms are what an application asked for when it set the mode.
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
    // SHIFT remains the one thing that cancels application keypad mode.
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::NUM_LOCK | Modifiers::SHIFT,
            KeyboardMode::APP_KEYPAD
        ),
        b"5"
    );
}
