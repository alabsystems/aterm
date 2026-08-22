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
    // KP numbers report-all-keys uses. The fold applies ONLY inside kitty
    // semantics: with no kitty flag active the legacy encoder owns keypad
    // keys (DECKPAM SS3 forms), which the fold must not disturb.
    let folded = if mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        && !mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
        && !mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    {
        fold_numpad_key(key)
    } else {
        None
    };
    let key = folded.as_ref().unwrap_or(key);

    if should_encode_kitty_event(key, modifiers, mode, event_type) {
        return encode_kitty(key, modifiers, mode, event_type, base_layout_key);
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

/// The keypad key's non-keypad equivalent (`None` = no fold), per the kitty
/// spec's legacy rule: "All keypad keys are reported as their equivalent
/// non-keypad keys. To distinguish these, use the disambiguate flag." Keys
/// with no non-keypad equivalent (NumpadEqual/Separator/Begin, digits,
/// operators) are unchanged — digits/operators arrive as `Key::Character`
/// from the hosts anyway.
// Skip: the `Key` inspection walks table slices / iterators (absent std
// bodies). Exhaustively unit-tested against the kitty/xterm specs.
#[cfg_attr(trust_verify, trust::skip)]
fn fold_numpad_key(key: &Key) -> Option<Key> {
    let Key::Named(named) = key else { return None };
    Some(Key::Named(match named {
        NamedKey::NumpadEnter => NamedKey::Enter,
        NamedKey::NumpadArrowUp => NamedKey::ArrowUp,
        NamedKey::NumpadArrowDown => NamedKey::ArrowDown,
        NamedKey::NumpadArrowLeft => NamedKey::ArrowLeft,
        NamedKey::NumpadArrowRight => NamedKey::ArrowRight,
        NamedKey::NumpadHome => NamedKey::Home,
        NamedKey::NumpadEnd => NamedKey::End,
        NamedKey::NumpadPageUp => NamedKey::PageUp,
        NamedKey::NumpadPageDown => NamedKey::PageDown,
        NamedKey::NumpadInsert => NamedKey::Insert,
        NamedKey::NumpadDelete => NamedKey::Delete,
        _ => return None,
    }))
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
    let modifier_flag = match key {
        Key::Named(NamedKey::ShiftLeft) | Key::Named(NamedKey::ShiftRight) => Modifiers::SHIFT,
        Key::Named(NamedKey::ControlLeft) | Key::Named(NamedKey::ControlRight) => Modifiers::CTRL,
        Key::Named(NamedKey::AltLeft) | Key::Named(NamedKey::AltRight) => Modifiers::ALT,
        Key::Named(NamedKey::SuperLeft) | Key::Named(NamedKey::SuperRight) => Modifiers::SUPER,
        Key::Named(NamedKey::HyperLeft) | Key::Named(NamedKey::HyperRight) => Modifiers::HYPER,
        Key::Named(NamedKey::MetaLeft) | Key::Named(NamedKey::MetaRight) => Modifiers::META,
        _ => return modifiers,
    };

    // Spec: the bit reflects the state INCLUDING the current event — set on
    // press/repeat, cleared on release. Known limitation: when BOTH the left
    // and right key of one kind are held and one is released, the spec keeps
    // the bit set; the hosts deliver only an aggregated pre-event modifier
    // state (winit canonicalizes to the *Left variants), so we cannot tell
    // that case apart and clear unconditionally.
    let mut adjusted = modifiers;
    match event_type {
        KeyEventType::Release => adjusted.remove(modifier_flag),
        KeyEventType::Press | KeyEventType::Repeat => adjusted.insert(modifier_flag),
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
        Key::Named(_) => None,
    }
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
