// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for Kitty progressive-enhancement keyboard flags.
//!
//! Validates behavioral encoding differences when `REPORT_ALTERNATE_KEYS`,
//! `REPORT_ALL_KEYS_AS_ESC`, and `REPORT_ASSOCIATED_TEXT` are enabled,
//! including edge cases (named keys, modifier suppression, event type
//! interactions, and flag combinations).

use super::*;

// =========================================================================
// Kitty progressive-enhancement flags: flag presence
// =========================================================================

#[test]
fn mode_new_kitty_flags_default_off() {
    let mode = KeyboardMode::empty();
    assert!(!mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS));
    assert!(!mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(!mode.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT));
}

#[test]
fn mode_new_kitty_flags_independent() {
    let alt = KeyboardMode::REPORT_ALTERNATE_KEYS;
    assert!(alt.contains(KeyboardMode::REPORT_ALTERNATE_KEYS));
    assert!(!alt.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(!alt.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT));

    let all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert!(!all.contains(KeyboardMode::REPORT_ALTERNATE_KEYS));
    assert!(all.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));

    let text = KeyboardMode::REPORT_ASSOCIATED_TEXT;
    assert!(text.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT));
    assert!(!text.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
}

#[test]
fn mode_all_kitty_flags_combinable() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        | KeyboardMode::REPORT_EVENT_TYPES
        | KeyboardMode::REPORT_ALTERNATE_KEYS
        | KeyboardMode::REPORT_ALL_KEYS_AS_ESC
        | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    assert!(mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES));
    assert!(mode.contains(KeyboardMode::REPORT_EVENT_TYPES));
    assert!(mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS));
    assert!(mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(mode.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT));
}

// =========================================================================
// REPORT_ALL_KEYS_AS_ESC: forces CSI-u encoding path
// =========================================================================

#[test]
#[rustfmt::skip]
fn report_all_keys_encodings() {
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_cases(&[
        // With REPORT_ALL_KEYS_AS_ESC alone (no DISAMBIGUATE), a plain 'a' should
        // still use the CSI-u path (not legacy single-byte output):
        // CSI-u for 'a' (U+0061 = 97): ESC [ 97 u
        ("plain char", ch('a'), NO_MODS, mode, KEY, b"\x1b[97u"),
        // CSI-u for Enter and Tab uses their Kitty codes (13, 9), not legacy.
        ("enter", named(NamedKey::Enter), NO_MODS, mode, KEY, b"\x1b[13u"),
        ("tab", named(NamedKey::Tab), NO_MODS, mode, KEY, b"\x1b[9u"),
    ]);
}

/// The spec's functional-key table assigns arrows/F1-F12 their legacy CSI
/// forms permanently — kitty's rewrite is NOT gated on report-all-keys, so
/// even full-mode kitty puts ESC[A / ESC[1;2P on the wire, never the internal
/// PUA numbers (57352/57364).
#[test]
fn kitty_report_all_keys_keeps_legacy_functional_forms() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F1), Modifiers::SHIFT, mode),
        b"\x1b[1;2P"
    );
    // Keys with NO legacy alternative in the spec's table keep CSI-u: F13+
    // (57376+) and Enter (13).
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F13), Modifiers::empty(), mode),
        b"\x1b[57376u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode),
        b"\x1b[13u"
    );
}

// =========================================================================
// REPORT_ALTERNATE_KEYS: shifted alternate codepoints
// =========================================================================

#[test]
#[rustfmt::skip]
fn report_alternate_keys_encodings() {
    let disamb = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ALTERNATE_KEYS;
    assert_cases(&[
        // REPORT_ALTERNATE_KEYS is a PURE enhancement: "only key events
        // represented as escape codes due to the other enhancements in effect
        // will be affected". Shift-only text is not escaped under disambiguate,
        // so there is no escape code to enhance — kitty sends plain 'A'.
        ("shifted letter", ch('a'), Modifiers::SHIFT, disamb, KEY, b"A"),
        ("shifted symbol", ch('1'), Modifiers::SHIFT, disamb, KEY, b"!"),
        // With REPORT_ALL_KEYS_AS_ESC the event IS an escape code, so the
        // alternate-key subparam appears: primary 97, shifted alternate 65.
        ("report-all shift", ch('a'), Modifiers::SHIFT, all, KEY, b"\x1b[97:65;2u"),
        // Named keys (arrows, function keys) never have alternate codepoints.
        // ArrowUp retains legacy CSI format under Kitty protocol (#7474), with
        // Kitty modifier encoding: CSI 1;2 A
        ("named key", named(NamedKey::ArrowUp), Modifiers::SHIFT, disamb, KEY, b"\x1b[1;2A"),
        // Without Shift there is no alternate codepoint, and an unmodified
        // printable key is not escaped under disambiguate — it stays plain text.
        ("unshifted", ch('a'), NO_MODS, disamb, KEY, b"a"),
        // Ctrl+Shift+'a': alternate codepoint 'A' should still appear even with
        // Ctrl. shifted_character only checks Shift — Ctrl doesn't suppress the
        // alternate. primary=97, alternate=65, mod=6 (Shift=1 + Ctrl=4 → 5+1=6)
        ("ctrl+shift", ch('a'), Modifiers::CTRL | Modifiers::SHIFT, disamb, KEY, b"\x1b[97:65;6u"),
        // Shift-only text stays text (pure-enhancement rule); Shift+'~' is '~'.
        ("unmappable", ch('~'), Modifiers::SHIFT, disamb, KEY, b"~"),
        // Under REPORT_ALL the event is an escape code; shifted '~' is '~'
        // itself (identity), so no alternate subparam appears.
        ("identity shift, report-all", ch('~'), Modifiers::SHIFT, all, KEY, b"\x1b[126;2u"),
    ]);
}

// =========================================================================
// REPORT_ASSOCIATED_TEXT: text-as-codepoints third parameter
// =========================================================================

#[test]
#[rustfmt::skip]
fn report_associated_text_encodings() {
    let text = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let text_events = text | KeyboardMode::REPORT_EVENT_TYPES;
    let three = text | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let five = three | KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        // Associated text only appears under REPORT_ALL_KEYS_AS_ESC; without it
        // a plain key under disambiguate is just its text byte (no CSI-u, no
        // payload).
        (
            "needs report-all",
            ch('a'),
            NO_MODS,
            KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ASSOCIATED_TEXT,
            KEY,
            b"a",
        ),
        ("appends codepoint", ch('a'), NO_MODS, text, KEY, b"\x1b[97;1;97u"),
        ("shifted character", ch('a'), Modifiers::SHIFT, text, KEY, b"\x1b[97;2;65u"),
        ("no payload on release", ch('a'), NO_MODS, text_events, RELEASE, b"\x1b[97;1:3u"),
        // Repeat events (unlike Release) should carry associated text per Kitty
        // spec: mod=1 (to carry event+text), event=2(repeat), text=97('a')
        ("payload on repeat", ch('a'), NO_MODS, text_events, REPEAT, b"\x1b[97;1:2;97u"),
        ("no payload with alt", ch('a'), Modifiers::ALT, text, KEY, b"\x1b[97;3u"),
        // Ctrl modifier suppresses text payload (matches Kitty/Terminal behavior).
        ("no payload with ctrl", ch('a'), Modifiers::CTRL, text, KEY, b"\x1b[97;5u"),
        // Super modifier suppresses text payload.
        ("no payload with super", ch('a'), Modifiers::SUPER, text, KEY, b"\x1b[97;9u"),
        // Shift+'1' produces '!' (U+0021 = 33) as associated text:
        // primary=49('1'), mod=2(shift), text=33('!')
        ("shifted symbol", ch('1'), Modifiers::SHIFT, text, KEY, b"\x1b[49;2;33u"),
        // Combined progressive flags (alternate + report-all + text). Shift+'a':
        // primary=97, alternate=65('A'), mod=2, text=65('A')
        ("three flags, shift", ch('a'), Modifiers::SHIFT, three, KEY, b"\x1b[97:65;2;65u"),
        // All flags on, no shift: primary=97, no alternate, mod=1 (to carry
        // text), text=97
        ("three flags, unshifted", ch('a'), NO_MODS, three, KEY, b"\x1b[97;1;97u"),
        // All 5 Kitty flags enabled, release event: text is omitted per spec.
        // primary=97, alternate=65, mod=2, event=3(release), no text
        ("five flags, release", ch('a'), Modifiers::SHIFT, five, RELEASE, b"\x1b[97:65;2:3u"),
    ]);
}

#[test]
fn report_associated_text_omits_payload_for_named_key() {
    // Enter and Tab type CONTROL CODES (`\r`, `\t`), and the spec's one
    // exclusion drops those — not "named keys carry no text", which would
    // also silence the spacebar (see
    // `encode_tests::kitty_associated_text_carries_the_spacebars_space`).
    let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
    let result = encode_key(&Key::Named(NamedKey::Enter), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[13u");
    let result = encode_key(&Key::Named(NamedKey::Tab), Modifiers::empty(), mode);
    assert_eq!(result, b"\x1b[9u");
}

// =========================================================================
// Modifier keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC.
//
// Spec: "Additionally, with this mode [Report all keys as escape codes],
// events for pressing modifier keys are reported." Under every other flag
// combination — including REPORT_EVENT_TYPES — kitty's is_modifier_key gate
// drops bare modifier events entirely, press AND release. (An earlier
// revision reported modifier releases under 0b10-only, citing kitty #5996;
// that issue is an unrelated IBus/Neovim display bug and never justified
// the divergence.)
// =========================================================================

#[test]
#[rustfmt::skip]
fn modifier_key_encodings() {
    let events = KeyboardMode::REPORT_EVENT_TYPES;
    assert_cases(&[
        ("bare shift release", named(NamedKey::ShiftLeft), Modifiers::SHIFT, events, RELEASE, b""),
        ("bare ctrl release", named(NamedKey::ControlLeft), Modifiers::CTRL, events, RELEASE, b""),
        ("bare alt release", named(NamedKey::AltLeft), Modifiers::ALT, events, RELEASE, b""),
        // ControlLeft=57442; release removes CTRL → mod=1; event=3(release)
        (
            "report-all ctrl release",
            named(NamedKey::ControlLeft),
            Modifiers::CTRL,
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | events,
            RELEASE,
            b"\x1b[57442;1:3u",
        ),
        // Modifier press has no legacy escape sequence; REPORT_EVENT_TYPES-only
        // does not promote press events to Kitty encoding.
        ("bare shift press", named(NamedKey::ShiftLeft), NO_MODS, events, PRESS, b""),
        ("bare ctrl press", named(NamedKey::ControlLeft), NO_MODS, events, PRESS, b""),
    ]);
}
