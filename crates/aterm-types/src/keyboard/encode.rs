// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard input encoding for terminal emulators.
//!
//! Encodes key presses into terminal escape sequences, supporting both legacy
//! VT100/xterm encoding and the Kitty keyboard protocol.

#[path = "encode_legacy.rs"]
mod encode_legacy;

use super::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey};
use encode_legacy::{ctrl_character, encode_character_legacy, encode_named_legacy};

/// Encode a key press into terminal escape sequence bytes.
///
/// Automatically selects between legacy encoding and Kitty keyboard protocol
/// based on the terminal mode flags.
#[must_use]
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key(key: &Key, modifiers: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    encode_key_with_event(key, modifiers, mode, KeyEventType::Press)
}

/// Encode a key event with event type information.
///
/// Extends `encode_key` to support key repeat and release events
/// when using the Kitty keyboard protocol.
///
/// Progressive Kitty enhancements:
/// - `REPORT_ALL_KEYS_AS_ESC` forces CSI-u encoding for keys that would
///   otherwise use legacy escapes.
/// - `REPORT_ALTERNATE_KEYS` emits `unicode:alternate` in the first CSI
///   parameter when a shifted alternate codepoint is known.
/// - `REPORT_ASSOCIATED_TEXT` appends text-as-codepoints as the third CSI
///   parameter when paired with `REPORT_ALL_KEYS_AS_ESC`.
///
/// A MODIFIER key's own bit follows kitty, which reports the state the event
/// leaves: a press or repeat sets it whatever `modifiers` says, and on a
/// release `modifiers` IS that state — the bit set only while the key's other
/// side is still held ([`NamedKey::modifier_twin`]; both Shifts down, one let
/// go, `shift` is still in force). The window states it from its
/// physical-press record; a controller's `key … type=release` names it with
/// `mods=`.
#[must_use]
// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key_with_event(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> Vec<u8> {
    encode_key_with_layout(key, modifiers, mode, event_type, None)
}

/// Encode a key event with optional `base_layout_key` for Kitty protocol.
///
/// The `base_layout_key` is the character that the physical key would produce
/// on a US QWERTY layout, regardless of the user's active keyboard layout.
/// When `REPORT_ALTERNATE_KEYS` mode is active, this is emitted as the third
/// colon-delimited value in the first CSI parameter: `key[:shifted[:base_layout]]`.
///
/// Pass `None` when the platform cannot determine the base layout key or when
/// the base layout key is the same as the primary key.
#[must_use]
// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key_with_layout(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    base_layout_key: Option<char>,
) -> Vec<u8> {
    // Kitty folds keypad keys onto their non-keypad equivalents unless the
    // app opted into telling them apart (disambiguate) or into the dedicated
    // KP numbers report-all-keys uses — the spec's legacy rule: "All keypad
    // keys are reported as their equivalent non-keypad keys. To distinguish
    // these, use the disambiguate flag." `Key::main_block_twin` is that fold,
    // digits and operators included: a physical KP_5 reaches this encoder as
    // `Numpad5` now, and under a kitty mode without disambiguate it must be
    // the `5` kitty sends (a text press, a text repeat), not a dedicated
    // `CSI 57404 u`. The fold applies ONLY inside kitty semantics: with no
    // kitty flag active the legacy encoder owns keypad keys (DECKPAM SS3
    // forms), which the fold must not disturb.
    //
    // …AND NOT UNDER DECKPAM, whichever flags are set. The fold answers "the
    // app has not asked to tell the keypad from the main block" — but setting
    // application keypad mode IS that request, spelled in the legacy encoding
    // the fold is folding INTO. Folding there threw the mode away and sent the
    // main row's `5` for a KP_5 an SS3-reading application had explicitly
    // asked to hear as `ESC O u`; the legacy encoder, not the fold, owns a
    // keypad key while DECKPAM is set. (SHIFT still cancels application keypad
    // mode inside that encoder, so a Shift+KP_5 is the same `5` either way.)
    //
    // …AND THE FOLD REACHES THE KITTY ENCODER ONLY. A KEYPAD KEY DOES NOT
    // COMPOSE. The twin is the kitty NAME for the key — the code and the text
    // the CSI-u report carries — not a re-spelling of the press for the legacy
    // encoder. Handed to `encode_legacy`, the folded `Key::Character('5')` runs
    // the MAIN ROW's shift/ctrl tables (`shifted_character`, `ctrl_character`)
    // and Shift+KP_5 came out `%`, Ctrl+KP_5 came out 0x1D — glyphs no keypad
    // has ever typed, and only because an unrelated kitty REPORTING flag was
    // on. On the keypad SHIFT selects the other level (xkb's KEYPAD type is
    // `map[Shift] = Level2`, so a NumLock-OFF Shift+KP_5 IS KP_5 and arrives
    // here as `Numpad5` + SHIFT), and xterm's rule is that SHIFT only cancels
    // application keypad mode — it never composes a keypad glyph. So the
    // legacy fallback keeps the ORIGINAL keypad key and `encode_numpad` owns
    // its bytes, which is the same rule `associated_text_codepoints` states
    // for the twin's glyph one screen down. `modifiers` is passed through
    // UNTOUCHED on both paths: the kitty modifier field still carries the
    // SHIFT bit (`CSI 53;6u` for Ctrl+Shift+KP_5), and the legacy keypad
    // encoder wants SHIFT to cancel DECKPAM.
    let folded = if mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        && !mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
        && !mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        && !mode.contains(KeyboardMode::APP_KEYPAD)
    {
        key.main_block_twin()
    } else {
        None
    };
    let kitty_key = folded.as_ref().unwrap_or(key);

    // ConPTY win32-input-mode (DEC 9001): a key conhost cannot read from legacy
    // VT goes out as the INPUT_RECORD pair conhost asked for — the Enter chords
    // and the ESC keys [`win32_key_record`] names — and it goes out FIRST, ahead
    // of the kitty gate and of xterm modifyOtherKeys, because under ConPTY
    // conhost is the only reader there is. Measured 2026-09-22 (scratch
    // CreatePseudoConsole, conhost 10.0.26200): an application's `CSI > 1 u`
    // push and its `CSI > 4;2 m` reach this terminal verbatim through the
    // pipe, so the kitty flags and the xterm level really do get set here — but
    // conhost's input parser then DROPS the `CSI 13;2 u` and the
    // `CSI 27;2;13 ~` the terminal writes back (no INPUT_RECORD at all reached
    // a `ReadKey` loop; a byte fed after them arrived fine), while the win32
    // pair arrives as `Enter+Shift`; re-measured 2026-09-27 for Escape's own
    // `CSI 27 u` and Ctrl+['s `CSI 27;5;91 ~`, both dropped the same way.
    // Letting the application's push outrank the record would therefore kill
    // the key for exactly the applications that asked for it (Claude Code
    // pushes disambiguate). Unix is untouched: 9001 is never set there. The
    // RELEASE of a routed key encodes to nothing whatever the kitty flags say
    // — the pair already carries its key-up, and a kitty release report would
    // be dropped by conhost anyway.
    if mode.contains(KeyboardMode::WIN32_INPUT)
        && let Some(record) = win32_key_record(key, modifiers, mode, base_layout_key)
    {
        return if event_type == KeyEventType::Release {
            Vec::new()
        } else {
            encode_win32_record_pair(&record)
        };
    }

    if should_encode_kitty_event(kitty_key, modifiers, mode, event_type) {
        return encode_kitty(kitty_key, modifiers, mode, event_type, base_layout_key);
    }

    // For release events without Kitty protocol, return nothing
    if event_type == KeyEventType::Release {
        return Vec::new();
    }

    // The kitty protocol supersedes xterm modifyOtherKeys: an app that pushed
    // kitty flags (agent TUIs can push CSI > ... u AND set modifyOtherKeys=2)
    // negotiated kitty semantics, so a key the kitty gate deliberately left
    // as text (Shift+a -> 'A') must not be re-escaped in the xterm dialect.
    if !mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        && let Some(bytes) = encode_xterm_other_keys(key, modifiers, mode)
    {
        return bytes;
    }

    encode_legacy(key, modifiers, mode)
}

/// Modifier and lock keys, mirroring kitty's `is_modifier_key`: the spec
/// reports events for these ONLY under REPORT_ALL_KEYS_AS_ESC ("Additionally,
/// with this mode, events for pressing modifier keys are reported"). kitty
/// also gates ISO_LEVEL3/5_SHIFT here; add them if NamedKey ever grows those.
///
/// SELECTION CUSTODY (R1) shares this ONE list. A key in it expresses no
/// typing intent — holding Command to reach ⌘-C, or Shift to extend a click,
/// is not "the user asked to be taken to the prompt" — so the GUI's press path
/// runs no viewport snap and no selection clear for it. Keeping the Kitty
/// report gate and the inert-press gate on the same predicate means "keys only
/// Kitty reports" and "keys that do not disturb reading" cannot drift apart.
/// The encoding is UNAFFECTED: a modifier still reports under
/// REPORT_ALL_KEYS_AS_ESC exactly as before.
// Skip: slice `contains`/iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn is_modifier_or_lock_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::ShiftLeft
                | NamedKey::ShiftRight
                | NamedKey::ControlLeft
                | NamedKey::ControlRight
                | NamedKey::AltLeft
                | NamedKey::AltRight
                | NamedKey::SuperLeft
                | NamedKey::SuperRight
                | NamedKey::HyperLeft
                | NamedKey::HyperRight
                | NamedKey::MetaLeft
                | NamedKey::MetaRight
                | NamedKey::CapsLock
                | NamedKey::ScrollLock
                | NamedKey::NumLock
        )
    )
}

// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
fn should_encode_kitty_event(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> bool {
    // Kitty spec: RELEASE events are reported ONLY when the app negotiated
    // REPORT_EVENT_TYPES — no other enhancement flag opts into them. Encoding
    // one anyway is worse than a fidelity leak: `encode_kitty` gates the `:3`
    // event-type subfield on REPORT_EVENT_TYPES, so the release bytes come out
    // IDENTICAL to a press (release of 'a' → `ESC[97u`), and an app that
    // pushed only DISAMBIGUATE (`CSI > 1 u`) sees every key
    // twice. Standing down here falls through to the legacy release path,
    // which encodes nothing.
    if event_type == KeyEventType::Release && !mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
        return false;
    }

    // Modifier and lock keys are reported ONLY under REPORT_ALL_KEYS_AS_ESC
    // (spec: "Additionally, with this mode, events for pressing modifier keys
    // are reported"). Under 0b1/0b10/0b11 kitty emits nothing for a bare
    // Shift/Ctrl/CapsLock press OR release; legacy encodes them to nothing
    // already, so standing down here is exact parity.
    if is_modifier_or_lock_key(key) {
        return mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC);
    }

    // REPORT_ALL_KEYS_AS_ESC forces every key through the CSI-u encoder.
    if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }

    // Without REPORT_ALL_KEYS_AS_ESC, an event that produces text stays plain
    // UTF-8 and therefore has NO representable release event. This includes a
    // bare/Shift-only Character or Space. Enter/Tab/Backspace are an explicit
    // reset escape hatch and have no release events in ANY modifier form until
    // report-all is set. Other non-text events (Esc, arrows, function keys,
    // Ctrl/Alt chords) still carry the requested `:3`.
    if event_type == KeyEventType::Release {
        if is_reset_escape_hatch_key(key) {
            return false;
        }
        return key_event_needs_csi_u(key, modifiers);
    }

    // Press/Repeat under DISAMBIGUATE (with or without event types): only
    // genuinely ambiguous chords escape; text-producing events stay text.
    // kitty delivers presses AND repeats of plain/shifted text keys as plain
    // UTF-8 in these modes — the event-type subfield only ever decorates
    // events that already needed an escape form.
    if mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES) {
        return key_event_needs_csi_u(key, modifiers);
    }

    if event_type == KeyEventType::Press || !mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
        return false;
    }

    // Repeat under REPORT_EVENT_TYPES without disambiguate: a text-producing
    // repeat is delivered as text (indistinguishable from a press, as the
    // spec's legacy note says); only no-text chords need the CSI-u `:2` form.
    key_event_needs_csi_u(key, modifiers)
}

/// Chord modifiers — the bits that make a key combination "ambiguous" in
/// legacy encodings. Lock bits (CapsLock/NumLock) and the exotic Hyper/Meta
/// bits never force an escape form, mirroring `encode_xterm_other_keys`.
const CHORD_MODIFIERS: Modifiers = Modifiers::SHIFT
    .union(Modifiers::ALT)
    .union(Modifiers::CTRL)
    .union(Modifiers::SUPER);

/// Kitty's recovery keys have no release event unless report-all is active,
/// regardless of modifiers. Keeping this test separate from
/// [`key_event_needs_csi_u`] matters: modified presses/repeats still need their
/// unambiguous CSI-u form.
#[cfg_attr(trust_verify, trust::skip)]
fn is_reset_escape_hatch_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace)
    )
}

/// Whether a key PRESS/REPEAT needs the CSI-u escape form under kitty modes
/// that keep text as text (disambiguate, and event-types-only repeats).
///
/// Kitty's disambiguation list is exactly "the Esc, alt+key, ctrl+key,
/// ctrl+alt+key, shift+alt+key keys" — a bare or SHIFT-only chord on a
/// text-producing key composes text ('A' for Shift+a, '@' for Shift+2,
/// ' ' for Shift+Space) and is NOT ambiguous. The legacy text keys
/// (Enter/Tab/Backspace) keep their bytes only UNMODIFIED: Shift+Enter →
/// `ESC[13;2u` is precisely how kitty-protocol apps detect that chord.
// Skip: the `Key` inspection walks table slices / iterators (absent std
// bodies). Exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn key_event_needs_csi_u(key: &Key, modifiers: Modifiers) -> bool {
    let effective_mods = modifiers & CHORD_MODIFIERS;
    let text_mods = effective_mods.is_empty() || effective_mods == Modifiers::SHIFT;
    match key {
        Key::Character(_) => !text_mods,
        Key::Named(NamedKey::Space) => !text_mods,
        Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace) => {
            !effective_mods.is_empty()
        }
        // Everything else — Esc, arrows, nav, F-keys, dedicated numpad keys
        // (incl. NumpadEnter when the fold kept it) — has no text form.
        Key::Named(_) => true,
    }
}

/// Encode using the Kitty keyboard protocol.
///
/// Per the Kitty spec, functional keys with legacy CSI forms in the
/// functional-key table (arrows, Home/End, Insert/Delete, Page Up/Down,
/// F1-F12, KP_BEGIN) retain that format in EVERY kitty mode — including
/// `REPORT_ALL_KEYS_AS_ESC`. Keys without legacy representations (Escape,
/// Enter, Tab, Backspace, Space, modifier keys, media keys, dedicated numpad
/// keys, F13+) use the CSI u format:
/// `CSI unicode [; modifiers [: event-type]] u`.
// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
fn encode_kitty(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    base_layout_key: Option<char>,
) -> Vec<u8> {
    let kitty_modifiers = kitty_modifiers_for_event(key, modifiers, event_type);

    // Functional keys with legacy CSI representations retain their legacy
    // format UNCONDITIONALLY — kitty's encode_function_key rewrite table is
    // not gated on REPORT_ALL_KEYS_AS_ESC (#7474): even full-mode kitty sends
    // `ESC[A` for an arrow press and `ESC[1;1:3A` for its release. The PUA
    // numbers (57344+) are wire codes only for keys with no legacy form.
    if let Some(legacy) = encode_kitty_legacy_functional(key, kitty_modifiers, mode, event_type) {
        return legacy;
    }

    let (primary_code, alternate_code, base_layout_code) =
        kitty_key_codes(key, kitty_modifiers, mode, base_layout_key);
    let associated_text = associated_text_codepoints(key, kitty_modifiers, mode, event_type);

    let mod_value = kitty_modifiers.kitty_encoded();
    let report_events = mode.contains(KeyboardMode::REPORT_EVENT_TYPES);
    let include_event_type = report_events && event_type != KeyEventType::Press;
    let include_modifiers = mod_value > 1 || include_event_type || associated_text.is_some();

    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(b"\x1b[");

    write_u32(&mut buf, primary_code);
    if alternate_code.is_some() || base_layout_code.is_some() {
        buf.push(b':');
        if let Some(alt) = alternate_code {
            write_u32(&mut buf, alt);
        }
        if let Some(base) = base_layout_code {
            buf.push(b':');
            write_u32(&mut buf, base);
        }
    }

    if include_modifiers {
        buf.push(b';');
        write_u8(&mut buf, mod_value);

        if include_event_type {
            buf.push(b':');
            write_u8(&mut buf, event_type.kitty_value());
        }
    }

    if let Some(associated_text) = associated_text {
        buf.push(b';');
        let mut codepoints = associated_text.into_iter();
        if let Some(first) = codepoints.next() {
            write_u32(&mut buf, first);
            for codepoint in codepoints {
                buf.push(b':');
                write_u32(&mut buf, codepoint);
            }
        }
    }

    buf.push(b'u');
    buf
}

/// For named keys with legacy CSI representations, encode using the legacy
/// format with Kitty modifier encoding (1+mods). Returns `None` for keys
/// that have no legacy representation and should use CSI u.
///
/// Legacy formats preserved under Kitty protocol:
/// - Arrows: CSI [1;{mod}] A/B/C/D
/// - Home/End: CSI [1;{mod}] H/F
/// - Insert/Delete/PageUp/PageDown: CSI {num} [;{mod}] ~
/// - F1-F4: CSI [1;{mod}] P/Q/R/S (or SS3 P/Q/R/S without mods)
/// - F5-F24: CSI {num} [;{mod}] ~
// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
fn encode_kitty_legacy_functional(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> Option<Vec<u8>> {
    let named = match key {
        Key::Named(n) => *n,
        Key::Character(_) => return None,
    };

    let report_events = mode.contains(KeyboardMode::REPORT_EVENT_TYPES);
    let mod_value = modifiers.kitty_encoded();
    // `Some(event)` exactly when the former `include_event_type` bool was
    // true (REPORT_EVENT_TYPES negotiated and the event is not a plain
    // press); the hoisted helpers below recover `has_modifiers_or_event` as
    // `mod_value > 1 || event.is_some()`, the same expression it was
    // computed from here.
    let event = if report_events && event_type != KeyEventType::Press {
        Some(event_type)
    } else {
        None
    };

    Some(match named {
        // Arrows
        NamedKey::ArrowUp => letter_final(b'A', mod_value, event),
        NamedKey::ArrowDown => letter_final(b'B', mod_value, event),
        NamedKey::ArrowRight => letter_final(b'C', mod_value, event),
        NamedKey::ArrowLeft => letter_final(b'D', mod_value, event),
        // Home/End, and KP_BEGIN's dedicated letter form ("KP_BEGIN | 1 E").
        NamedKey::Home => letter_final(b'H', mod_value, event),
        NamedKey::End => letter_final(b'F', mod_value, event),
        NamedKey::NumpadBegin => letter_final(b'E', mod_value, event),
        // Insert/Delete/PageUp/PageDown
        NamedKey::Insert => tilde_final(2, mod_value, event),
        NamedKey::Delete => tilde_final(3, mod_value, event),
        NamedKey::PageUp => tilde_final(5, mod_value, event),
        NamedKey::PageDown => tilde_final(6, mod_value, event),
        // F1/F2/F4 (letter finals). F3 is tilde-only: the spec REMOVED its
        // original `CSI R` letter form because it collides with the Cursor
        // Position Report ("F3 | 13 ~", spec note) — kitty emits 13;m~.
        NamedKey::F1 => letter_final(b'P', mod_value, event),
        NamedKey::F2 => letter_final(b'Q', mod_value, event),
        NamedKey::F3 => tilde_final(13, mod_value, event),
        NamedKey::F4 => letter_final(b'S', mod_value, event),
        // F5-F12 (tilde finals). F13-F24 have NO legacy alternative in the
        // spec's functional table (unlike F1 "1 P or 11 ~") — kitty sends
        // their dedicated CSI-u numbers (57376+) in every mode, so they must
        // fall through to the CSI-u path, not borrow xterm's 25~..38~ forms.
        NamedKey::F5 => tilde_final(15, mod_value, event),
        NamedKey::F6 => tilde_final(17, mod_value, event),
        NamedKey::F7 => tilde_final(18, mod_value, event),
        NamedKey::F8 => tilde_final(19, mod_value, event),
        NamedKey::F9 => tilde_final(20, mod_value, event),
        NamedKey::F10 => tilde_final(21, mod_value, event),
        NamedKey::F11 => tilde_final(23, mod_value, event),
        NamedKey::F12 => tilde_final(24, mod_value, event),
        _ => return None,
    })
}

// The three helpers below were capturing closures inside
// `encode_kitty_legacy_functional` (`append_mod_event`, `letter_final`,
// `tilde_final`). Hoisted to named fns with the captures passed explicitly:
// the Trust gate verifies fn items directly, whereas a capturing closure
// lowers to an opaque environment it cannot model. `event` is `Some`
// exactly when the old `include_event_type` bool was true, and
// `mod_value > 1 || event.is_some()` is the old `has_modifiers_or_event`,
// so every byte written is unchanged.

/// Build the modifier suffix ";{mod}[:event]" portion.
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn append_mod_event(buf: &mut Vec<u8>, mod_value: u8, event: Option<KeyEventType>) {
    if mod_value > 1 || event.is_some() {
        buf.push(b';');
        write_u8(buf, mod_value);
        if let Some(event_type) = event {
            buf.push(b':');
            write_u8(buf, event_type.kitty_value());
        }
    }
}

/// Letter-final keys: CSI [1;{mod}[:event]] {letter}
/// Without modifiers/event: CSI {letter}
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn letter_final(letter: u8, mod_value: u8, event: Option<KeyEventType>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    buf.extend_from_slice(b"\x1b[");
    if mod_value > 1 || event.is_some() {
        buf.push(b'1');
        append_mod_event(&mut buf, mod_value, event);
    }
    buf.push(letter);
    buf
}

/// Tilde-final keys: CSI {num} [;{mod}[:event]] ~
// Skip: the key-table lookup drives absent std iterator/slice bodies; an
// unknown key returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
fn tilde_final(num: u8, mod_value: u8, event: Option<KeyEventType>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    buf.extend_from_slice(b"\x1b[");
    write_u8(&mut buf, num);
    append_mod_event(&mut buf, mod_value, event);
    buf.push(b'~');
    buf
}

// Skip: iterator/slice absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
fn kitty_modifiers_for_event(
    key: &Key,
    modifiers: Modifiers,
    event_type: KeyEventType,
) -> Modifiers {
    let Some((modifier_flag, _)) = (match key {
        Key::Named(named) => named.modifier_twin(),
        Key::Character(_) => None,
    }) else {
        return modifiers;
    };

    // Spec: the bit reflects the state INCLUDING the current event. A press or
    // repeat sets it. A release leaves it as the caller states it, because only
    // the caller can know the answer: kitty keeps the bit while the key's other
    // side is still held (both Shifts down, one released) and clears it
    // otherwise, and `modifiers` on a release is that post-event state — the
    // window reads it from its physical-press record (`release_physical_press`),
    // a controller names it. The SIDE is not lost either: the window maps a
    // right-side press to the *Right key (`aterm_winit_keymap::sided_modifier`),
    // whose code kitty reports.
    let mut adjusted = modifiers;
    if event_type != KeyEventType::Release {
        adjusted.insert(modifier_flag);
    }
    adjusted
}

// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
fn kitty_key_codes(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
) -> (u32, Option<u32>, Option<u32>) {
    match key {
        Key::Named(named) => (named.kitty_code(), None, None),
        Key::Character(c) => {
            let primary = *c as u32;
            if !mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS) {
                return (primary, None, None);
            }
            let alternate = shifted_character(*c, modifiers)
                .map(u32::from)
                .filter(|alt| *alt != primary);
            // base_layout_key: the US QWERTY equivalent of this physical key.
            // Only emit when it differs from the primary key (#7678).
            let base_layout = base_layout_key
                .map(u32::from)
                .filter(|base| *base != primary);
            (primary, alternate, base_layout)
        }
    }
}

// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
fn associated_text_codepoints(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> Option<Vec<u32>> {
    if !mode.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT)
        || !mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        || event_type == KeyEventType::Release
    {
        return None;
    }

    // Match Kitty/Terminal behavior: modified control/meta paths do not carry text payloads.
    if modifiers.intersects(Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER) {
        return None;
    }

    match key {
        Key::Character(c) => {
            // CapsLock composes uppercase letters exactly like Shift on macOS
            // (caps OR shift → uppercase; caps+shift stays uppercase). The
            // shift table alone missed caps: `caps+a` reported text 'a' while
            // the modifier field correctly said caps — kitty embeds the text
            // the OS produced ('A').
            let text = if c.is_ascii_lowercase()
                && modifiers.intersects(Modifiers::SHIFT | Modifiers::CAPS_LOCK)
            {
                c.to_ascii_uppercase()
            } else {
                shifted_character(*c, modifiers).unwrap_or(*c)
            };
            if text.is_control() {
                None
            } else {
                Some(vec![text as u32])
            }
        }
        // A KEYPAD GLYPH KEY REPORTS THE TEXT IT TYPES. `REPORT_ASSOCIATED_TEXT`
        // answers "what did this press put on the screen", and for KP_5, KP_.
        // and KP_+ that is `5`, `.` and `+` — the main-block twin's character.
        // They used to arrive from the hosts as `Key::Character` and reported
        // their glyph; since the winit seam resolves the physical keypad they
        // arrive as `Key::Named(Numpad5)` and fell to this arm's `None`, so the
        // keypad's text silently vanished from the report while its dedicated
        // `CSI 57404 u` number stayed. Only the TEXT field consults the twin —
        // the key code stays the keypad's own, which is the whole point of the
        // mode that turned this on.
        //
        // The twin's glyph is reported AS IS, with no shift/caps table: those
        // compose a MAIN-BLOCK character key (`'5'` under Shift is `'%'`), and
        // no keypad key composes. SHIFT on the keypad picks a LEVEL, not a
        // glyph: with NumLock ON a shifted keypad 5 is KP_Begin, a different
        // key, which this arm then answers `None` for because `NumpadBegin`
        // has no twin; with NumLock OFF the same press is KP_5 itself (xkb's
        // KEYPAD type is `map[Shift] = Level2`, the level that holds the
        // digit) and reports `5`. Either way the shift table never runs — the
        // rule `encode_key_with_layout` keeps by handing the legacy encoder
        // the keypad key rather than this twin. The keypad's named keys
        // (`NumpadEnter`, `NumpadEnd`) fold to named twins and carry no text,
        // exactly as `Enter` and `End` do.
        //
        // …AND A NAMED KEY THAT TYPES A CHARACTER ITSELF ANSWERS FIRST. The
        // twin is the KEYPAD fold, and asking it alone silenced the SPACEBAR:
        // `Space` is a main-block key, so it has no twin, and the one key
        // whose glyph IS its own key code reported no text at all (`ESC[32u`
        // where kitty sends `ESC[32;1;32u`). A client that inserts strictly
        // from the associated-text field — the field's whole purpose, since
        // that is how a non-US layout or an IME delivers the character
        // actually typed — typed every letter, digit, symbol and keypad glyph
        // but never a space. `named_key_text` is that "what does this key
        // type" question, and the SAME control-code filter the `Character`
        // arm applies decides the rest, exactly as the spec's single
        // exclusion does ("code points below U+0020"): Enter's `\r`, Tab's
        // `\t` and Backspace's `\x7f` are dropped BY THE RULE rather than by
        // never being asked, and U+0020, which is not below U+0020, survives.
        Key::Named(named) => {
            let text = named_key_text(*named).or(match key.main_block_twin() {
                Some(Key::Character(glyph)) => Some(glyph),
                _ => None,
            });
            match text {
                Some(glyph) if !glyph.is_control() => Some(vec![glyph as u32]),
                _ => None,
            }
        }
    }
}

/// The character a NAMED key types — the text its press puts on the screen,
/// before any control-code filter. `None` for the keys that type nothing
/// (arrows, nav, F-keys, modifiers) and for the dedicated keypad keys, whose
/// glyph comes from [`Key::main_block_twin`] instead.
///
/// The list is the named keys with an ASCII code point rather than a kitty PUA
/// functional number: Space (`' '`), and the legacy text keys Enter (`'\r'`),
/// Tab (`'\t'`) and Backspace (`'\x7f'`). The three control codes are listed
/// HONESTLY, not omitted — `associated_text_codepoints` drops them with the
/// same `is_control` test it applies to a character key, which is the kitty
/// spec's one exclusion ("the associated text must not contain control
/// codes … code points below U+0020"). Spelling them as "types nothing" would
/// hide the rule that must keep U+0020 in while keeping U+000D out.
///
/// Backspace's WIRE byte swaps with DECBKM (`0x7f`/`0x08`); both are control
/// codes, so the filter answers the same for either and this table need not
/// know the mode.
#[must_use]
fn named_key_text(named: NamedKey) -> Option<char> {
    Some(match named {
        NamedKey::Space => ' ',
        NamedKey::Enter => '\r',
        NamedKey::Tab => '\t',
        NamedKey::Backspace => '\x7f',
        _ => return None,
    })
}

/// The glyph SHIFT composes from base key `c` on the US layout (`'h'`→`'H'`, `'2'`→`'@'`,
/// `'/'`→`'?'`), or `None` when SHIFT is not held / does not change `c`. The single source
/// of truth the legacy encoder uses to pick the byte it sends — reused by predictive echo
/// so a predicted glyph matches what the shell will echo.
// Skip: the keyboard encoder family — byte-sequence building (Vec
// push/extend) and key-table lookups over absent std bodies. The
// encoded bytes are exhaustively unit-tested against the kitty/xterm
// specs (encode_tests.rs).
#[cfg_attr(trust_verify, trust::skip)]
pub fn shifted_character(c: char, modifiers: Modifiers) -> Option<char> {
    if !modifiers.contains(Modifiers::SHIFT) {
        return None;
    }

    match c {
        'a'..='z' => Some(c.to_ascii_uppercase()),
        '1' => Some('!'),
        '2' => Some('@'),
        '3' => Some('#'),
        '4' => Some('$'),
        '5' => Some('%'),
        '6' => Some('^'),
        '7' => Some('&'),
        '8' => Some('*'),
        '9' => Some('('),
        '0' => Some(')'),
        '`' => Some('~'),
        '-' => Some('_'),
        '=' => Some('+'),
        '[' => Some('{'),
        ']' => Some('}'),
        '\\' => Some('|'),
        ';' => Some(':'),
        '\'' => Some('"'),
        ',' => Some('<'),
        '.' => Some('>'),
        '/' => Some('?'),
        _ => Some(c),
    }
}

/// ConPTY win32-input-mode: `VK_RETURN` (13) and its PS/2 scan code (0x1C = 28)
/// — the `wVirtualKeyCode` / `wVirtualScanCode` of BOTH Enter keys on a Windows
/// keyboard. The keypad's Enter is the same pair behind the E0 prefix, which a
/// real `INPUT_RECORD` reports as `ENHANCED_KEY` in `dwControlKeyState`.
const WIN32_VK_RETURN: u32 = 13;
const WIN32_SC_RETURN: u32 = 28;
/// `VK_ESCAPE` / scan 0x01 and `VK_BACK` / scan 0x0E, with the `UnicodeChar`
/// a Windows keyboard gives each: ESC (0x1B) and BS (0x08). BS, not the DEL
/// aterm's legacy Backspace writes: it is the char conhost itself put in the
/// Alt+Backspace record it decoded from `ESC DEL` (measured 2026-09-27,
/// `Backspace MODS=Alt CHAR=0x8`), so the record keeps what applications saw.
const WIN32_VK_ESCAPE: u32 = 0x1b;
const WIN32_SC_ESCAPE: u32 = 0x01;
const WIN32_ESCAPE_CHAR: u32 = 0x1b;
const WIN32_VK_BACK: u32 = 0x08;
const WIN32_SC_BACK: u32 = 0x0e;
const WIN32_BACK_CHAR: u32 = 0x08;
/// `dwControlKeyState` bits (wincon.h): `SHIFT_PRESSED`, `LEFT_CTRL_PRESSED`,
/// `LEFT_ALT_PRESSED`, `ENHANCED_KEY`. The host cannot tell a left modifier
/// from a right one once winit has canonicalized the chord, so the LEFT bits
/// are reported — what conhost itself synthesizes for a modifier it cannot
/// side-attribute.
const WIN32_SHIFT_PRESSED: u32 = 0x10;
const WIN32_LEFT_CTRL_PRESSED: u32 = 0x08;
const WIN32_LEFT_ALT_PRESSED: u32 = 0x02;
const WIN32_ENHANCED_KEY: u32 = 0x100;
/// The `UnicodeChar` of an Enter-chord record: what a PHYSICAL chord carries
/// under Windows Terminal, so every ReadConsoleInput reader (.NET `ReadKey`,
/// libuv's `tty.c`, the MSYS runtime, cmd.exe's cooked reader) sees under aterm
/// exactly the record it sees under WT, and every WT-tuned workaround an
/// application ships keeps working. WT copies the field from the OS's own
/// `ToUnicode` translation of the chord: Shift+Enter is CR, and Enter with
/// CTRL held — with or without Shift — is LF (the same translation conhost
/// applies in reverse when it reads a bare 0x0A as Ctrl+Enter). Measured
/// 2026-09-22 (conhost 10.0.26200, records fed into a ConPTY tab):
/// `[Console]::ReadKey` reports `Enter MODS=Shift CHAR=13` for the shift
/// record and `Enter MODS=Control CHAR=10` / `MODS=Shift, Control CHAR=10` for
/// the ctrl ones; PSReadLine, which binds by key and modifiers, runs AddLine on
/// the shift record (`>>` continuation, both lines executed on the next
/// Enter); cmd.exe's cooked reader runs the pending line on it, as it does
/// under WT.
///
/// Deliberately NOT aterm's Unix Shift+Enter policy (`encode_legacy.rs`, the
/// `Enter` arm: LF, so a LF-honouring reader gets a newline unnegotiated).
/// Carrying that LF inside the shift record was tried and rejected: no Windows
/// keyboard produces `Shift+Enter, UnicodeChar 10`, so a reader that keys on
/// the char sees a chord that exists nowhere else — cmd.exe's cooked reader
/// stayed inert on the LF record where it runs the line on the CR one (both
/// measured), and a reader that emits the char gets a '\n' WT would never
/// have sent it. The Unix policy is untouched, since 9001 is never set there.
const WIN32_SHIFT_ENTER_CHAR: u32 = 13;
const WIN32_CTRL_ENTER_CHAR: u32 = 10;

/// The four `INPUT_RECORD` fields a routed key decides — `wVirtualKeyCode`,
/// `wVirtualScanCode`, `UnicodeChar` and `dwControlKeyState`. `bKeyDown` and
/// `wRepeatCount` belong to the pair ([`encode_win32_record_pair`]).
struct Win32KeyRecord {
    vk: u32,
    sc: u32,
    uc: u32,
    cs: u32,
}

/// The win32-input-mode record for a key conhost cannot read from legacy VT,
/// or `None` for every key it reads correctly, which keeps its legacy bytes.
/// Two families, both measured against conhost 10.0.26200: the Enter chords
/// legacy VT has no byte for ([`win32_enter_chord`]), and the keys whose
/// legacy bytes conhost stops reading once it has seen one record
/// ([`win32_escape_record`]).
// Skip: `Option::or_else` over two table lookups — absent std bodies; both
// halves are exhaustively unit-tested against the measured records.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_key_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
) -> Option<Win32KeyRecord> {
    win32_enter_chord(key, modifiers)
        .or_else(|| win32_escape_record(key, modifiers, mode, base_layout_key))
}

/// The win32-input-mode record for an Enter chord — `Some` only for an
/// Enter (main block or keypad) with SHIFT and/or CTRL (ALT may ride along),
/// `None` for everything else so the caller falls through to the legacy bytes:
/// plain Enter stays CR and Alt+Enter stays `ESC CR`, both of which conhost
/// already translates correctly (measured 2026-09-22, and again 2026-09-27
/// after a record had switched its parser — see
/// [`conhost_holds_once_switched`]). The keypad Enter is the
/// same VK/scan pair flagged `ENHANCED_KEY`, as a real record for the
/// E0-prefixed key is; it must be routed here too, because the legacy keypad
/// arm falls back to the main Enter's bare LF, which is precisely the byte
/// conhost reads as Ctrl+Enter. DECKPAM is not consulted: the record names the
/// physical key, and conhost applies the application's keypad mode itself when
/// the application reads in VT mode.
// Skip: bitflags `contains` and a `match` over a table enum — absent std
// bodies; the mapping is exhaustively unit-tested against the spec.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_enter_chord(key: &Key, modifiers: Modifiers) -> Option<Win32KeyRecord> {
    let mut cs = match key {
        Key::Named(NamedKey::Enter) => 0u32,
        Key::Named(NamedKey::NumpadEnter) => WIN32_ENHANCED_KEY,
        _ => return None,
    };
    let shift = modifiers.contains(Modifiers::SHIFT);
    let ctrl = modifiers.contains(Modifiers::CTRL);
    if !shift && !ctrl {
        return None;
    }
    if shift {
        cs |= WIN32_SHIFT_PRESSED;
    }
    if ctrl {
        cs |= WIN32_LEFT_CTRL_PRESSED;
    }
    if modifiers.contains(Modifiers::ALT) {
        cs |= WIN32_LEFT_ALT_PRESSED;
    }
    // CTRL decides the char, whatever else is held: Windows translates the
    // chord's character before it looks at Shift.
    let uc = if ctrl {
        WIN32_CTRL_ENTER_CHAR
    } else {
        WIN32_SHIFT_ENTER_CHAR
    };
    Some(Win32KeyRecord {
        vk: WIN32_VK_RETURN,
        sc: WIN32_SC_RETURN,
        uc,
        cs,
    })
}

/// Whether conhost's input parser, once it has read a win32-input-mode record,
/// HOLDS these legacy key bytes instead of delivering the key they spell.
///
/// Before the first record, conhost settles an ambiguous tail by the end of
/// the read that carried it: a read ending in a lone ESC is the Escape key, and
/// a read ending in `ESC [` is Alt+[. The first record switches that flush off
/// for the rest of the session — a terminal speaking win32-input-mode never
/// sends a bare ESC, so a trailing ESC must be a sequence split across reads —
/// and aterm's first Shift+Enter IS a record. Measured 2026-09-27 (conhost
/// 10.0.26200; every ESC pair fed raw into a ConPTY tab running a `ReadKey`
/// loop, each followed by `z`, before and after one Shift+Enter record):
///
/// - `ESC` alone: nothing, even after 6 s idle, and the next `z` read as
///   Alt+Z. This is Escape — PSReadLine's RevertLine, vim's leave-insert,
///   Claude Code's Esc-to-interrupt — and Ctrl+[.
/// - `ESC ESC`, `ESC DEL`: the second byte vanished and the ESC stayed pending
///   (`z` → Alt+Z): Alt+Escape, Ctrl+Alt+[, Alt+Backspace.
/// - `ESC [`, `ESC O`, `ESC P`, `ESC ]`, `ESC X`, `ESC ^`, `ESC _`: each opens a
///   CSI / SS3 / DCS / OSC / SOS / PM / APC that swallowed the `z` (the string
///   forms swallow everything up to BEL or ST): Alt+[, Alt+Shift+O, ….
///
/// Everything else arrived intact after the switch, so it keeps its bytes:
/// ESC + every other printable, ESC + every other C0 (Alt+Enter, Alt+Tab,
/// Ctrl+Alt+letter), ESC + non-ASCII, and every complete CSI/SS3 sequence
/// (arrows, Home/End, F-keys, Shift+Tab and their modified forms). Before the
/// switch the held forms arrived intact too, which is why this is a win32-mode
/// rule and not a legacy one. (`ESC ESC [ E`, Alt+keypad-5 with NumLock off, is
/// dropped by conhost before the switch and after it alike and poisons
/// nothing, so it is outside this rule.)
fn conhost_holds_once_switched(legacy: &[u8]) -> bool {
    match legacy {
        [0x1b] => true,
        [0x1b, second] => matches!(
            second,
            0x1b | 0x7f | b'[' | b'O' | b'P' | b']' | b'X' | b'^' | b'_'
        ),
        _ => false,
    }
}

/// The record for a key whose legacy bytes [`conhost_holds_once_switched`]:
/// Escape (any modifiers), Alt+Backspace, and a character chord that legacy
/// spells as one of those forms (Ctrl+[ and Ctrl+3 are a lone ESC, Alt+[ is
/// `ESC [`, Ctrl+Alt+8 is `ESC DEL`, …). `None` for every other key, and for a
/// character no US-QWERTY key types — every held form is ASCII, so that is out
/// of a keyboard's reach and the legacy bytes are the only spelling left.
///
/// The record states what the legacy bytes stated, re-spelled so conhost
/// cannot mis-split it. `Uc` is the byte after the Alt prefix (ESC for Escape
/// and Ctrl+[, `[` for Alt+[, DEL for Ctrl+Alt+8), except Backspace, whose
/// Windows char is BS. `LEFT_ALT_PRESSED` is set exactly when the legacy form
/// carried the ESC prefix, which is what conhost decoded that prefix to (`ESC
/// a` → `A MODS=Alt`). `Vk`/`Sc` name the PHYSICAL key: the US identity the
/// host resolved from the scan code (`base_layout_key`), else the character's
/// own US key, which also contributes SHIFT for a shifted glyph (`_` is
/// Shift+-). Measured after the switch, each record reads back as the key it
/// names: Escape `27;1;27` → `Escape CHAR=0x1B`, Alt+Backspace `8;14;8;…;2` →
/// `Backspace MODS=Alt CHAR=0x8` and Alt+[ `219;26;91;…;2` → `Oem4 MODS=Alt`
/// (both exactly what conhost decoded from the legacy pair before it), and
/// Ctrl+[ `219;26;27;…;8` → `Oem4 MODS=Control CHAR=0x1B`.
///
/// Ctrl+[ is the one key whose identity moves. Before the switch conhost read
/// its lone ESC as the Escape key; the record is the one a Windows keyboard
/// makes for that key (the US layout gives `VK_OEM_4` with Ctrl the char
/// 0x1B), the record a console window hands its reader when Ctrl+[ is pressed
/// on it directly. So vim, which reads the char, leaves insert mode on it as
/// before (measured, Git's vim), while PSReadLine's Windows mode, which binds
/// by key and has nothing on Ctrl+[, now inserts `^[` where it used to revert
/// the line (measured). Sending the Escape key instead would keep that revert
/// but name a key nobody pressed; Escape itself reverts the line.
///
/// Ctrl+Escape and Alt+Escape go out as their records too, and conhost drops
/// them (measured: nothing reached `ReadKey`, and the next key arrived plain) —
/// its own rule for the shell's Start-menu and window-cycling keys, which it
/// applies to a record whichever terminal sent it. The legacy alternative is
/// worse: after the switch it swallows the key typed next.
// Skip: slice pattern matching and a table lookup — absent std bodies; the
// mapping is exhaustively unit-tested against the measured records.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_escape_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
) -> Option<Win32KeyRecord> {
    let legacy = encode_legacy(key, modifiers, mode);
    if !conhost_holds_once_switched(&legacy) {
        return None;
    }
    let (vk, sc, uc, shifted_glyph) = match key {
        Key::Named(NamedKey::Escape) => {
            (WIN32_VK_ESCAPE, WIN32_SC_ESCAPE, WIN32_ESCAPE_CHAR, false)
        }
        Key::Named(NamedKey::Backspace) => (WIN32_VK_BACK, WIN32_SC_BACK, WIN32_BACK_CHAR, false),
        Key::Character(c) => {
            let (vk, sc, shifted_glyph) = base_layout_key
                .and_then(us_physical_key)
                .or_else(|| us_physical_key(*c))?;
            (vk, sc, u32::from(*legacy.last()?), shifted_glyph)
        }
        Key::Named(_) => return None,
    };
    let mut cs = 0;
    if shifted_glyph || modifiers.contains(Modifiers::SHIFT) {
        cs |= WIN32_SHIFT_PRESSED;
    }
    if modifiers.contains(Modifiers::CTRL) {
        cs |= WIN32_LEFT_CTRL_PRESSED;
    }
    // Every two-byte held form is the ESC prefix plus the key's own byte.
    if legacy.len() == 2 {
        cs |= WIN32_LEFT_ALT_PRESSED;
    }
    Some(Win32KeyRecord { vk, sc, uc, cs })
}

/// The US-QWERTY main-block key that types `c`: its `wVirtualKeyCode`, its
/// set-1 scan code, and whether `c` is that key's SHIFTED glyph. Scan codes
/// are physical positions, the same on every layout; the VK is the US one,
/// which is what conhost's own `VkKeyScan` answered for these characters
/// before the switch (`ESC _` → `OemMinus MODS=Alt, Shift`, `ESC ^` → `D6
/// MODS=Alt, Shift`). `None` for anything the US main block cannot type.
// Skip: a table scan (slice iterator) — absent std bodies; unit-tested.
#[cfg_attr(trust_verify, trust::skip)]
fn us_physical_key(c: char) -> Option<(u32, u32, bool)> {
    // Set-1 scan codes of A..Z.
    const LETTER_SCAN: [u8; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    // (unshifted, shifted, VK, scan) for the digit row and the OEM keys.
    const SYMBOL_KEYS: [(char, char, u8, u8); 21] = [
        ('1', '!', 0x31, 0x02),
        ('2', '@', 0x32, 0x03),
        ('3', '#', 0x33, 0x04),
        ('4', '$', 0x34, 0x05),
        ('5', '%', 0x35, 0x06),
        ('6', '^', 0x36, 0x07),
        ('7', '&', 0x37, 0x08),
        ('8', '*', 0x38, 0x09),
        ('9', '(', 0x39, 0x0a),
        ('0', ')', 0x30, 0x0b),
        ('-', '_', 0xbd, 0x0c),
        ('=', '+', 0xbb, 0x0d),
        ('[', '{', 0xdb, 0x1a),
        (']', '}', 0xdd, 0x1b),
        ('\\', '|', 0xdc, 0x2b),
        (';', ':', 0xba, 0x27),
        ('\'', '"', 0xde, 0x28),
        ('`', '~', 0xc0, 0x29),
        (',', '<', 0xbc, 0x33),
        ('.', '>', 0xbe, 0x34),
        ('/', '?', 0xbf, 0x35),
    ];
    if c.is_ascii_alphabetic() {
        let upper = c.to_ascii_uppercase();
        let sc = *LETTER_SCAN.get(usize::from((upper as u8).saturating_sub(b'A')))?;
        return Some((u32::from(upper), u32::from(sc), c.is_ascii_uppercase()));
    }
    SYMBOL_KEYS
        .iter()
        .find(|&&(plain, shifted, _, _)| c == plain || c == shifted)
        .map(|&(plain, _, vk, sc)| (u32::from(vk), u32::from(sc), c != plain))
}

/// The win32-input-mode `INPUT_RECORD` pair (key-down, then key-up) for a
/// routed key.
///
/// Format (microsoft/terminal doc/specs/#4999-win32-input-mode.md):
/// `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, every field spelled (Windows Terminal's
/// own `_GenerateWin32KeySequence` writes all six). `Uc` and `Cs` are the SAME
/// on both halves (the modifier is still held when the key comes back up);
/// `Kd` is 1 then 0; `Rc` is 1.
///
/// Both halves are emitted from the PRESS. Windows applications key on the
/// key-down and the host keeps no per-key state, so a self-contained pair per
/// press is what makes the key atomic; the caller's release path emits
/// nothing for this key, so no second key-up ever follows.
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the win32-input-mode spec.
#[cfg_attr(trust_verify, trust::skip)]
fn encode_win32_record_pair(record: &Win32KeyRecord) -> Vec<u8> {
    let mut buf = Vec::with_capacity(48);
    for key_down in [1u32, 0u32] {
        buf.extend_from_slice(b"\x1b[");
        write_u32(&mut buf, record.vk);
        buf.push(b';');
        write_u32(&mut buf, record.sc);
        buf.push(b';');
        write_u32(&mut buf, record.uc);
        buf.push(b';');
        write_u32(&mut buf, key_down);
        buf.push(b';');
        write_u32(&mut buf, record.cs);
        buf.extend_from_slice(b";1_");
    }
    buf
}

/// Encode using legacy terminal sequences.
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn encode_legacy(key: &Key, modifiers: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    // Caps Lock is a LOCK state, never a chord modifier in legacy xterm encoding —
    // it belongs only to the Kitty modifier byte (which still reports it). Left in,
    // it makes `has_modifiers` true with Caps Lock engaged, so arrows / Home-End /
    // PageUp-Down / F-keys would emit the modified `ESC[1;1A` form (and lose the
    // SS3 app-cursor `ESC OA`) instead of the plain `ESC[A`, breaking readline / vim
    // / less / fzf navigation. xterm itself ignores Caps Lock here, so drop it
    // unconditionally before any encoding decision.
    let modifiers = modifiers & !Modifiers::CAPS_LOCK;
    // DEC private mode 1035 (xterm `numLock`): when reset the terminal advertises
    // `NO_SPECIAL_MODIFIERS`, so NumLock is no longer treated as a real modifier
    // and is dropped before any encoding decision is made (both the character and
    // named paths see the normalized set).
    let modifiers = if mode.contains(KeyboardMode::NO_SPECIAL_MODIFIERS) {
        modifiers & !Modifiers::NUM_LOCK
    } else {
        modifiers
    };
    match key {
        Key::Character(c) => encode_character_legacy(*c, modifiers, mode),
        Key::Named(named) => encode_named_legacy(*named, modifiers, mode),
    }
}

// Skip: the `Key` inspection walks table slices / iterators (absent std
// bodies). Exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn encode_xterm_other_keys(key: &Key, modifiers: Modifiers, mode: KeyboardMode) -> Option<Vec<u8>> {
    let level = mode.xterm_modify_other_keys_level();
    if level == 0 {
        return None;
    }

    let effective_mods =
        modifiers & (Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER);
    let code: u32 = match *key {
        Key::Character(c) => c as u32,
        Key::Named(NamedKey::Tab) => 9,
        Key::Named(NamedKey::Enter) => 13,
        Key::Named(NamedKey::Escape) => 27,
        Key::Named(NamedKey::Backspace) => 127,
        Key::Named(NamedKey::Space) => 32,
        _ => return None,
    };

    // xterm's modifyOtherKeys decision is key-sensitive. In particular, level
    // 1 preserves the established Shift/Ctrl encodings where they are
    // unambiguous, and Backspace keeps its DECBKM/Ctrl toggle. Treating the
    // levels as simply "Alt" and "any modifier" fabricates CSI packets for
    // keys xterm sends through its legacy path (notably Alt+Backspace and
    // Ctrl+Backspace). Keypad keys are excluded above because xterm classifies
    // them under modifyKeypadKeys, never modifyOtherKeys.
    let apply = match level {
        1 => xterm_modify_other_keys_level1_applies(key, effective_mods),
        2 => xterm_modify_other_keys_level2_applies(key, effective_mods),
        _ => false,
    };
    if !apply {
        return None;
    }

    let mod_value = effective_mods.xterm_encoded();
    if mode.xterm_format_other_keys() {
        // formatOtherKeys=1: CSI code ; modifier u
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(b"\x1b[");
        write_u32(&mut buf, code);
        buf.push(b';');
        write_u8(&mut buf, mod_value);
        buf.push(b'u');
        Some(buf)
    } else {
        // Default format: CSI 27 ; modifier ; code ~
        let mut buf = Vec::with_capacity(20);
        buf.extend_from_slice(b"\x1b[27;");
        write_u8(&mut buf, mod_value);
        buf.push(b';');
        write_u32(&mut buf, code);
        buf.push(b'~');
        Some(buf)
    }
}

/// xterm modifyOtherKeys level 1 (`mokUser`) preserves ordinary Shift-only and
/// established Ctrl mappings, but reports otherwise ambiguous combinations.
/// This is the projection of xterm `allowedCharModifiers` + `ModifyOtherKeys`
/// onto aterm's layout-neutral [`Key`] representation.
fn xterm_modify_other_keys_level1_applies(key: &Key, modifiers: Modifiers) -> bool {
    if modifiers.is_empty() {
        return false;
    }
    match key {
        // xterm's mokUser switch explicitly excludes Backspace.
        Key::Named(NamedKey::Backspace) => false,
        // These predefined ordinary keys have no printable Shift/Ctrl fallback.
        Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Escape) => true,
        Key::Named(NamedKey::Space) => xterm_level1_character_applies(' ', modifiers),
        Key::Character(c) => xterm_level1_character_applies(*c, modifiers),
        Key::Named(_) => false,
    }
}

fn xterm_level1_character_applies(c: char, modifiers: Modifiers) -> bool {
    if modifiers.intersects(Modifiers::ALT | Modifiers::SUPER) {
        return true;
    }
    if modifiers.contains(Modifiers::CTRL | Modifiers::SHIFT) {
        return true;
    }
    if modifiers == Modifiers::CTRL {
        // A traditional Ctrl mapping already has a unique legacy byte. If the
        // character has no such mapping, CSI is needed to retain the modifier.
        return ctrl_character(c).is_none();
    }
    false
}

fn xterm_modify_other_keys_level2_applies(key: &Key, modifiers: Modifiers) -> bool {
    if modifiers.is_empty() {
        return false;
    }
    match key {
        Key::Named(NamedKey::Backspace) => !(modifiers & !Modifiers::CTRL).is_empty(),
        Key::Named(NamedKey::Space) => true,
        Key::Character(c) if modifiers == Modifiers::SHIFT => {
            // xterm checks the shifted keysym. Shift-only characters below '@'
            // remain ordinary text (`1` -> `!`, `=` -> `+`, `,` -> `<`, etc.);
            // the ASCII control-input range '@'..DEL is escaped. Space is the
            // one explicit exception and is represented by NamedKey::Space in
            // the native/DOM mappings.
            let shifted = shifted_character(*c, modifiers).unwrap_or(*c) as u32;
            (u32::from(b'@')..=u32::from(0x7f_u8)).contains(&shifted)
        }
        _ => true,
    }
}

/// Write a u8 as decimal digits to a buffer.
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn write_u8(buf: &mut Vec<u8>, val: u8) {
    if val >= 100 {
        buf.push(b'0' + val / 100);
    }
    if val >= 10 {
        buf.push(b'0' + (val / 10) % 10);
    }
    buf.push(b'0' + val % 10);
}
/// Write a u32 as decimal digits to a buffer.
// Skip: the key encoders build byte sequences via Vec push/extend and
// table lookups — absent std bodies (alloc + iterator class). The encoded
// bytes are exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn write_u32(buf: &mut Vec<u8>, val: u32) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    // Extract decimal digits least-significant-first into a fixed 16-slot
    // scratch (u32::MAX is 10 digits), then append most-significant-first. This
    // uses only the literal divisor 10 (no runtime-divisor division for the
    // Trust gate to guard against a zero divisor), masks every scratch index
    // into the array's 0..=15 range, and forms each byte with `wrapping_add`
    // (the digit is 0..=9, so it never actually wraps) — so there is no
    // div-by-zero, index-out-of-bounds, or `b'0' + digit` overflow obligation.
    let mut digits = [0u8; 16];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 10 {
        digits[count & 15] = b'0'.wrapping_add((n % 10) as u8);
        n /= 10;
        // saturating: `count < 10` guards the increment and `count > 0` the
        // decrement — exact on every path; the verifier cannot chain either
        // loop condition into the arithmetic.
        count = count.saturating_add(1);
    }
    while count > 0 {
        count = count.saturating_sub(1);
        buf.push(digits[count & 15]);
    }
}
#[cfg(test)]
#[path = "encode_tests.rs"]
mod encode_tests;
