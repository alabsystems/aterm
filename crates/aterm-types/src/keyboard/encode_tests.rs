// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Regression tests for keyboard encoding internals (write_u32), plus the
//! KEYBOARD-SHIFT REFINEMENT obligation (the always-on twin of the Trust
//! `a7_keyboard_shift` SMT bundle / `clean/keyboard_shift.lean`).

use super::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key, encode_key_with_event,
    encode_kitty, shifted_character, write_u32,
};

// =========================================================================
// KEYBOARD-SHIFT SPEC — the single source of truth, authored INDEPENDENTLY of
// the implementation (from the ANSI US-QWERTY layout), so this is a genuine
// refinement check, not a tautology against the code's own table.
//
// This is the spec the "Shift doesn't work" regression violated: the legacy
// encoder applied Shift with `to_ascii_uppercase`, which is the identity on
// every non-letter, so Shift+2 emitted '2' instead of '@'. The two load-bearing
// properties below — REFINEMENT (the encoder equals this spec) and EFFECTIVENESS
// (Shift changes every shiftable key) — each independently forbid that bug.
// =========================================================================

/// SPEC: the US-QWERTY glyph produced by holding Shift on the key whose
/// unshifted character is `c`. `None` for keys with no distinct shifted form.
/// Authored from the physical ANSI layout, NOT from `shifted_character`.
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

/// THEOREM (refinement, `a7_keyboard_shift/shift_refines_spec.smt2` twin): for
/// every key with a distinct shifted glyph, the engine's single shift map
/// `shifted_character` — the one BOTH the legacy and Kitty paths now use —
/// equals the independent spec. A second shift table that disagreed (the
/// original `to_ascii_uppercase` legacy branch) cannot pass this.
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

/// THEOREM (effectiveness, `shift_is_effective.smt2` twin): Shift must CHANGE
/// every shiftable key end-to-end in legacy mode. This is the property the bug
/// most directly broke and the one that needs no knowledge of the exact glyph:
/// `encode(Shift+c) != encode(c)` for every key that has a shifted form.
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

/// PROVE-AND-CATCH (`catches_uppercase_bug_sat.smt2` twin): the obligation has
/// teeth — the ORIGINAL buggy `to_ascii_uppercase`-only shift map FAILS the spec
/// on at least one shiftable key. If this ever finds zero counterexamples the
/// refinement test above has gone vacuous.
#[test]
fn uppercase_only_shift_is_caught_by_the_spec() {
    let buggy = |c: char| c.to_ascii_uppercase(); // the pre-fix legacy branch
    let mut caught = Vec::new();
    for b in 0u8..=127 {
        let c = b as char;
        if let Some(spec) = spec_shifted(c)
            && buggy(c) != spec
        {
            caught.push(c);
        }
    }
    assert!(
        caught.contains(&'2') && caught.len() >= 20,
        "the spec must reject the to_ascii_uppercase bug on the digit/symbol rows; caught={caught:?}"
    );
}

/// Regression test: write_u32 with values >= 2,000,000,000 must not
/// infinite-loop or produce incorrect output.
///
/// Bug #2775: The original implementation used `u32` for the divisor.
/// When val >= 2B, `divisor * 10` overflowed u32, causing an infinite
/// loop in the digit-extraction while loop. Fix: use u64 for divisor.
#[test]
fn write_u32_two_billion_boundary() {
    let mut buf = Vec::new();
    write_u32(&mut buf, 2_000_000_000);
    assert_eq!(buf, b"2000000000");
}

#[test]
fn write_u32_max() {
    let mut buf = Vec::new();
    write_u32(&mut buf, u32::MAX); // 4294967295
    assert_eq!(buf, b"4294967295");
}

#[test]
fn write_u32_zero() {
    let mut buf = Vec::new();
    write_u32(&mut buf, 0);
    assert_eq!(buf, b"0");
}

#[test]
fn write_u32_small_values() {
    let mut buf = Vec::new();
    write_u32(&mut buf, 1);
    assert_eq!(buf, b"1");

    buf.clear();
    write_u32(&mut buf, 97); // 'a' codepoint
    assert_eq!(buf, b"97");

    buf.clear();
    write_u32(&mut buf, 999_999_999);
    assert_eq!(buf, b"999999999");
}

// =========================================================================
// SHIFT-ENTER DISTINGUISHABILITY obligation — the always-on, exhaustively
// MODEL-CHECKED proof behind "Shift+Enter inserts a newline".
//
// THE property: in EVERY keyboard mode, Shift+Enter encodes to DIFFERENT bytes
// than a plain Enter — so an app can ALWAYS tell the two apart. aterm guarantees
// this two ways: via the negotiated protocol (ESC[13;2u / ESC[27;2;13~) when one
// is active, and via aterm's INPUT POLICY (Shift+Enter -> LF 0x0a) in plain legacy
// mode. The legacy half is aterm FORCING the useful behaviour from the terminal
// side — no protocol handshake, and no faking aterm's identity — so Shift+Enter is
// a newline for cooperating apps (agent TUIs, readline, vim) out of the box.
//
// A genuine PROOF, not a sample: KeyboardMode defines 15 flag bits, so the whole
// 2^15 = 32768-point mode space is enumerated below. For a finite, fully-enumerated
// input space, exhaustive checking is sound AND complete (the same guarantee the
// symbolic trust-mc/Kani gate gives, here under stock `cargo test`). It has teeth:
// it FAILS on the pre-policy encoder (legacy Enter == legacy Shift+Enter == CR).
// =========================================================================

/// Number of defined `KeyboardMode` flag bits (see `mode.rs`: bits 0..=14).
const KEYBOARD_MODE_DEFINED_BITS: u32 = 15;

fn encode_enter(mode: KeyboardMode, mods: Modifiers) -> Vec<u8> {
    encode_key(&Key::Named(NamedKey::Enter), mods, mode)
}

/// EXHAUSTIVE: over all 2^15 keyboard modes, Shift+Enter is byte-DISTINCT from a
/// plain Enter, and neither ever encodes to nothing — so "Shift+Enter = newline"
/// works in EVERY mode (protocol-negotiated OR aterm's legacy LF policy).
#[test]
fn shift_enter_distinct_from_enter_in_every_mode() {
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
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

// =========================================================================
// KEY-RELEASE REPORTING obligation — the always-on, exhaustively MODEL-CHECKED
// proof behind "keys are never doubled in a progressive-keyboard TUI".
//
// THE property (kitty spec): a key RELEASE is reported ONLY when the app
// negotiated REPORT_EVENT_TYPES (progressive flag 0b10). No other enhancement
// flag opts into releases. The pre-fix encoder violated this: under
// DISAMBIGUATE-only — and likewise
// under REPORT_ALL_KEYS_AS_ESC without event types — a release was routed
// through the CSI-u encoder, which (correctly) only emits the `:3` event-type
// subfield when REPORT_EVENT_TYPES is on. The release therefore came out as a
// press-meaning CSI-u report (`ESC[97u` for releasing 'a', `ESC[13u` for
// releasing Enter), and the app received every keystroke TWICE — once from the
// real press bytes, once from the phantom release. Both the aterm GUI keyup
// path (app_input.rs) and Orca's helper-textarea keyup path forward releases
// unconditionally, relying on THIS contract: "the engine emits nothing for a
// release outside kitty event-type reporting".
//
// A genuine PROOF over the whole 2^15 mode space (see the Shift+Enter
// obligation above for why exhaustive = sound + complete here), crossed with a
// representative key/modifier grid spanning every encoder path: printable text,
// digits, the legacy text keys (Enter/Tab/Backspace/Space), Escape, arrows,
// F-keys, and bare modifier keys.
// =========================================================================

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
/// grid. This is the exact contract the GUI / Orca keyup forwarders rely on,
/// and the property whose violation doubled every key in the active Codex TUI.
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
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
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

/// THEOREM (text-release silence): REPORT_EVENT_TYPES alone cannot represent
/// releases for keys whose press/repeat is delivered as plain UTF-8. Across the
/// complete mode space without REPORT_ALL_KEYS_AS_ESC, bare and Shift-only
/// Characters/Space therefore emit no release bytes. This is the exact Codex
/// mode (1|2|4) regression: `ESC[99;1:3u` must never be fabricated for `c`.
#[test]
fn text_and_recovery_key_releases_require_report_all_keys() {
    let text_keys = [Key::Character('a'), Key::Named(NamedKey::Space)];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
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
}

/// THEOREM (distinguishability): in EVERY mode, a reported (non-empty) release
/// is byte-DISTINCT from the same key's press — so no app, whatever it
/// negotiated, can mistake a release report for another keystroke.
#[test]
fn reported_release_is_never_byte_identical_to_press() {
    let mods_grid = [Modifiers::empty(), Modifiers::SHIFT, Modifiers::CTRL];
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
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

/// PROVE-AND-CATCH (teeth): the hazard the silence gate defends against is
/// REAL — the raw CSI-u encoder genuinely CANNOT distinguish a release from a
/// press under disambiguate-only, because the `:3` subfield is (correctly)
/// gated on REPORT_EVENT_TYPES. Routing a release into it (the pre-fix
/// behavior) yields bytes any kitty-protocol app parses as a PRESS of 'a'.
/// If this collision ever stops holding, the silence theorem above has lost
/// its justification and the gate should be re-derived from the spec.
#[test]
fn ungated_release_would_collide_with_a_press_report() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let ungated_release = encode_kitty(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Release,
        None,
    );
    let press_report = encode_kitty(
        &Key::Character('a'),
        Modifiers::empty(),
        mode,
        KeyEventType::Press,
        None,
    );
    assert_eq!(ungated_release, b"\x1b[97u");
    assert_eq!(
        ungated_release, press_report,
        "the CSI-u encoder cannot mark releases without REPORT_EVENT_TYPES; \
         only the should_encode_kitty_event gate keeps them silent"
    );
}

// =========================================================================
// KITTY TEXT-STAYS-TEXT obligation — exhaustively MODEL-CHECKED refinement
// of the spec's disambiguation list.
//
// THE property: kitty's disambiguate flag escapes exactly "the Esc, alt+key,
// ctrl+key, ctrl+alt+key, shift+alt+key keys" — a bare or SHIFT-only chord on
// a text-producing key COMPOSES TEXT and must arrive as exactly the bytes the
// pure-legacy encoder would send ('a', 'A', '@', ' ', \r, \t, \x7f). The
// pre-fix encoder escaped Shift+a to `ESC[97;2u` under disambiguate mode, and
// escaped every press once REPORT_EVENT_TYPES was on.
//
// Quantified over every kitty-flag combination WITHOUT report-all-keys
// (which legitimately escapes text), crossed with the legacy mode bits that
// affect these keys' byte forms — the refinement says: kitty-text modes
// encode text events byte-identically to the same mode with kitty (and the
// superseded modifyOtherKeys) stripped.
// =========================================================================

/// Kitty flag combinations that must keep text as text (all subsets of the
/// five kitty bits minus those containing REPORT_ALL_KEYS_AS_ESC).
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
/// Space, and unmodified Enter/Tab/Backspace — encodes byte-identically to
/// the pure-legacy encoding of the same event. This is what makes capitals
/// arrive as 'A' (not `ESC[97;2u`) in text-preserving TUIs.
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
    ];
    for kitty_mode in kitty_text_preserving_modes() {
        // Cross with the legacy bits that change these keys' byte forms, and
        // with modifyOtherKeys (which kitty flags supersede).
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

// =========================================================================
// MODIFIER-KEY SILENCE obligation — modifier and lock keys are reported ONLY
// under REPORT_ALL_KEYS_AS_ESC (spec: "Additionally, with this mode, events
// for pressing modifier keys are reported"). Exhaustive over the whole 2^15
// mode space minus report-all modes, all modifier/lock keys, all event types.
// =========================================================================

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
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
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

// =========================================================================
// ENTER/TAB/BACKSPACE RELEASE escape hatch — the spec's "type reset" clause:
// releases of these legacy recovery keys are silent unless report-all is set,
// in every modifier form.
// =========================================================================

#[test]
fn escape_hatch_key_releases_silent_without_report_all() {
    for bits in 0u32..(1u32 << KEYBOARD_MODE_DEFINED_BITS) {
        let mode = KeyboardMode::from_bits_truncate(bits as u16);
        if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
            continue;
        }
        for key in [NamedKey::Enter, NamedKey::Tab, NamedKey::Backspace] {
            for mods in [
                Modifiers::empty(),
                Modifiers::SHIFT,
                Modifiers::CTRL,
                Modifiers::ALT,
                Modifiers::CTRL | Modifiers::SHIFT,
            ] {
                let got =
                    encode_key_with_event(&Key::Named(key), mods, mode, KeyEventType::Release);
                assert!(
                    got.is_empty(),
                    "{key:?} release must be silent — mods={mods:?} mode={mode:?} got={got:?}"
                );
            }
        }
    }
    // The same key is reported once report-all crosses the explicit boundary.
    let events = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    assert!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::SHIFT,
            events,
            KeyEventType::Release,
        )
        .is_empty(),
        "modified recovery-key releases stay silent without report-all"
    );
    let report_all = events | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_key_with_event(
            &Key::Named(NamedKey::Enter),
            Modifiers::empty(),
            report_all,
            KeyEventType::Release,
        ),
        b"\x1b[13;1:3u",
        "report-all opts Enter releases in"
    );
}

// =========================================================================
// Functional-form witnesses: the spec's functional table forms are permanent
// (not gated on report-all), F3 is tilde-only (its CSI R form was REMOVED for
// colliding with the Cursor Position Report), and keypad keys fold onto their
// plain equivalents unless disambiguate/report-all distinguishes them.
// =========================================================================

#[test]
fn functional_form_witnesses() {
    let disamb = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;
    let report_all = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;

    // F3: tilde form in every kitty mode (never ESC[R / ESC[1;mR).
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F3), Modifiers::empty(), disamb),
        b"\x1b[13~"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::F3), Modifiers::SHIFT, disamb),
        b"\x1b[13;2~"
    );

    // Shift+F10 keeps its legacy tilde form (terminfo kf22) under disambiguate:
    // the host tab-menu chord defers it only under report-all, so this is the
    // sequence a kitty-aware TUI that binds it receives.
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

    // Report-all keeps arrows on their legacy-functional forms.
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

    // KP_BEGIN's dedicated letter form.
    assert_eq!(
        encode_key(
            &Key::Named(NamedKey::NumpadBegin),
            Modifiers::empty(),
            disamb
        ),
        b"\x1b[E"
    );

    // Keypad fold: without disambiguate/report-all, KP keys are their plain
    // equivalents (KP_Up release reports as an ArrowUp release)…
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
    // …and WITH disambiguate they are dedicated CSI-u keys (incl. NumpadEnter,
    // which kitty does NOT convert to Enter once the app can tell them apart).
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

/// The kitty protocol supersedes xterm modifyOtherKeys: agent TUIs can set BOTH
/// (`CSI > ... u` and `CSI > 4;2 m`), and a key the kitty gate deliberately
/// leaves as text must not be re-escaped in the xterm dialect.
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
    // Without kitty flags, modifyOtherKeys still owns the chord.
    let mok_only = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::SHIFT, mok_only),
        b"\x1b[27;2;97~"
    );
}

/// Concrete witnesses for the Codex mode observed in the incident (`1|2|4`):
/// presses keep their text bytes, repeats stay text, and text/recovery-key
/// releases are silent because report-all (`0b1000`) is not set.
#[test]
fn codex_progressive_keyboard_mode_witnesses() {
    let mode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        | KeyboardMode::REPORT_EVENT_TYPES
        | KeyboardMode::REPORT_ALTERNATE_KEYS;
    let a = Key::Character('a');
    let enter = Key::Named(NamedKey::Enter);
    let none = Modifiers::empty();

    // Presses: plain text and legacy text keys keep their legacy bytes.
    assert_eq!(
        encode_key_with_event(&a, none, mode, KeyEventType::Press),
        b"a"
    );
    assert_eq!(
        encode_key_with_event(&enter, none, mode, KeyEventType::Press),
        b"\r"
    );
    // Text repeats stay text until report-all is negotiated.
    assert_eq!(
        encode_key_with_event(&a, none, mode, KeyEventType::Repeat),
        b"a"
    );
    assert_eq!(
        encode_key_with_event(&enter, none, mode, KeyEventType::Repeat),
        b"\r"
    );
    // Releases: SILENT. A CSI-u packet here would be an extra input packet for
    // every character typed into Codex.
    assert!(encode_key_with_event(&a, none, mode, KeyEventType::Release).is_empty());
    assert!(encode_key_with_event(&enter, none, mode, KeyEventType::Release).is_empty());
    // The protocol still works: a non-text chord reports both its press and
    // the negotiated release event.
    assert_eq!(
        encode_key_with_event(&a, Modifiers::CTRL, mode, KeyEventType::Press),
        b"\x1b[97;5u"
    );
    assert_eq!(
        encode_key_with_event(&a, Modifiers::CTRL, mode, KeyEventType::Release),
        b"\x1b[97;5:3u"
    );
}

/// The concrete witnesses the proof guarantees, for the modes an app actually hits.
#[test]
fn shift_enter_concrete_witnesses() {
    // Legacy (no protocol): plain Enter is CR; Shift+Enter is aterm's imposed LF —
    // This is the compatibility case for apps that have not negotiated kitty.
    assert_eq!(
        encode_enter(KeyboardMode::empty(), Modifiers::empty()),
        vec![0x0d]
    );
    assert_eq!(
        encode_enter(KeyboardMode::empty(), Modifiers::SHIFT),
        vec![0x0a]
    );
    // Kitty disambiguate: Shift+Enter is the CSI-u report (what an app that DOES
    // negotiate the protocol — like Ghostty/Kitty — receives).
    let kitty = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    assert_eq!(encode_enter(kitty, Modifiers::empty()), vec![0x0d]);
    assert_eq!(encode_enter(kitty, Modifiers::SHIFT), b"\x1b[13;2u");
    // xterm modifyOtherKeys level 2: Shift+Enter is the CSI 27 form.
    let mok2 = KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2;
    assert_eq!(encode_enter(mok2, Modifiers::SHIFT), b"\x1b[27;2;13~");
}

/// `Key::main_block_twin` is the ONE keypad fold — the kitty legacy rule's
/// "equivalent non-keypad key", shared with the GUI's native pages and its
/// press classifier. Every keypad key has a twin except KP_Begin, which the
/// main block cannot name; a main-block key never folds.
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

/// A physical KP_5 reaches the encoder as `Numpad5` now (the winit seam keeps
/// the keypad identity). Under a kitty mode that has NOT asked to tell the
/// keypad apart it is the `5` kitty sends — a text press AND a text repeat —
/// where the unfolded key reported a dedicated `CSI 57404;1:2 u` repeat no
/// kitty client emits. Disambiguate keeps the dedicated key, and with no kitty
/// flag at all DECKPAM still owns the keypad (SS3 u).
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

/// DECKPAM OUTRANKS THE KITTY LEGACY FOLD. The fold stands in for "the app has
/// not asked to tell the keypad from the main block" — and application keypad
/// mode IS that request, spelled in the very legacy encoding the fold folds
/// into. Folding there threw the mode away and answered an SS3-reading app the
/// main row's `5` for its KP_5.
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
    // SHIFT cancels application keypad mode inside the legacy encoder, so the
    // unfolded key lands on the same byte the fold would have produced.
    assert_eq!(
        encode_key(
            &kp5,
            Modifiers::SHIFT,
            events_only | KeyboardMode::APP_KEYPAD
        ),
        b"5"
    );
    // …and without DECKPAM the fold is exactly as it was.
    assert_eq!(encode_key(&kp5, Modifiers::empty(), events_only), b"5");
}

/// `REPORT_ASSOCIATED_TEXT` answers "what did this press put on the screen",
/// and a keypad glyph key puts its glyph there. The keypad used to arrive from
/// the hosts as `Key::Character` and reported it; since the winit seam resolves
/// the physical keypad it arrives as `Key::Named(Numpad5)`, whose `Named` arm
/// answered `None` — the text vanished from the report while the dedicated
/// `57404` code stayed. The twin supplies the text; the CODE is still the
/// keypad's own.
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
    // A keypad key whose twin is NAMED carries no text, exactly as its twin
    // does — and KP_Begin, which has no twin, carries none either.
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadEnd), Modifiers::empty(), full),
        b"\x1b[57424u"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::NumpadBegin), Modifiers::empty(), full),
        b"\x1b[E",
        "KP_Begin keeps its legacy letter form in every kitty mode"
    );
    // The main block is unmoved: `5` still reports itself.
    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::empty(), full),
        b"\x1b[53;1;53u"
    );
}

/// A KEYPAD KEY DOES NOT COMPOSE, and a kitty REPORTING flag cannot make it.
/// The legacy fold above names the key for the CSI-u report; handing that name
/// to the legacy encoder too ran the MAIN ROW's tables over it, so under a bare
/// `REPORT_EVENT_TYPES` a Shift+KP_5 typed `%` and a Ctrl+KP_5 typed 0x1D —
/// glyphs no keypad emits, and a strict regression for the NumLock-OFF shifted
/// press (xkb's KEYPAD type is `map[Shift] = Level2`, the level holding the
/// digit, so that press IS KP_5 and arrives here as `Numpad5` + SHIFT). xterm's
/// keypad rule is that SHIFT only cancels application keypad mode, and kitty's
/// legacy rule is to send the text the key produced; both say `5`.
#[test]
fn a_modified_keypad_key_never_composes_the_main_rows_glyph() {
    use NamedKey as N;
    let events_only = KeyboardMode::REPORT_EVENT_TYPES;

    // The shifted keypad types its own glyph; the trailing comment is what the
    // fold composed before — the main row's shifted character.
    for (named, want) in [
        (N::Numpad0, &b"0"[..]),    // was ")"
        (N::Numpad1, b"1"),         // was "!"
        (N::Numpad5, b"5"),         // was "%"
        (N::Numpad9, b"9"),         // was "("
        (N::NumpadDecimal, b"."),   // was ">"
        (N::NumpadSeparator, b","), // was "<"
        (N::NumpadDivide, b"/"),    // was "?"
        (N::NumpadSubtract, b"-"),  // was "_"
    ] {
        assert_eq!(
            encode_key(&Key::Named(named), Modifiers::SHIFT, events_only),
            want,
            "{named:?} under SHIFT"
        );
    }

    // …and no kitty reporting flag changes what the keypad types, for any
    // chord: the same physical key must answer the same bytes with the flag on
    // and off. CTRL is the other table the fold used to reach.
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

    // The NEGATIVE CONTROL: the main row still composes. The fold is what must
    // not compose, not the shift table.
    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::SHIFT, events_only),
        b"%"
    );
    assert_eq!(
        encode_key(&Key::Character('5'), Modifiers::CTRL, events_only),
        b"\x1d"
    );

    // The unshifted fold is exactly as it was: the digit is still the digit.
    assert_eq!(
        encode_key(&Key::Named(N::Numpad5), Modifiers::empty(), events_only),
        b"5"
    );
    assert_eq!(
        encode_key(&Key::Named(N::NumpadAdd), Modifiers::empty(), events_only),
        b"+"
    );

    // CTRL on the keypad is the engine's documented keypad rule (#7480: the
    // keypad encoder ignores it), now the SAME rule in every mode rather than
    // the control byte with a kitty flag and the glyph without.
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

    // The KITTY side of the split is untouched: where the fold reaches the
    // CSI-u encoder it still reports the twin's code AND the full modifier
    // set, SHIFT bit included (`5`=53, Ctrl+Shift=6, release=`:3`).
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

    // …and DECKPAM still outranks both halves.
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
