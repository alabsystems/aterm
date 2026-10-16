// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard input encoding: legacy VT100/xterm sequences and the Kitty keyboard protocol.
//!
//! The encoders are `trust::skip`: they build bytes with Vec push/extend and table lookups,
//! std bodies the prover does not model; `encode_tests.rs` pins the bytes to the specs.

#[path = "encode_legacy.rs"]
mod encode_legacy;

use super::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey};
use encode_legacy::{ctrl_character, encode_character_legacy, encode_named_legacy};

/// Encode a key press into terminal bytes, legacy or Kitty protocol as `mode` says.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key(key: &Key, modifiers: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    encode_key_with_event(key, modifiers, mode, KeyEventType::Press)
}

/// Encode a key event with its event type (repeat and release report under the Kitty
/// protocol).
///
/// Progressive Kitty enhancements:
/// - `REPORT_ALL_KEYS_AS_ESC` sends text keys and Enter/Tab/Backspace as CSI u too
///   (functional keys keep their legacy CSI forms).
/// - `REPORT_ALTERNATE_KEYS` emits `unicode:alternate` in the first CSI parameter when a
///   shifted alternate codepoint is known.
/// - `REPORT_ASSOCIATED_TEXT` appends the text's codepoint as the third CSI parameter when
///   paired with `REPORT_ALL_KEYS_AS_ESC`.
///
/// A modifier key's own bit follows kitty, which reports the state the event leaves: a press
/// or repeat sets it, and on a release `modifiers` is that state (set only while the key's
/// other side is still held, [`NamedKey::modifier_twin`]).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key_with_event(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> Vec<u8> {
    encode_key_with_layout(key, modifiers, mode, event_type, None)
}

/// Encode a key event with an optional `base_layout_key`: the character the physical key types
/// on US QWERTY, whatever the active layout. Under `REPORT_ALTERNATE_KEYS` it is the third
/// colon-delimited value of the first CSI parameter (`key[:shifted[:base_layout]]`), sent only
/// when it differs from the key. `None` when the platform cannot tell.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key_with_layout(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    base_layout_key: Option<char>,
) -> Vec<u8> {
    encode_key_with_text(key, modifiers, mode, event_type, base_layout_key, None)
}

/// [`encode_key_with_layout`] plus `layout_text`: the character the user's active layout
/// typed for this press, when the host knows it.
///
/// `key` is the layout's unshifted key (winit's `key_without_modifiers`). Without layout text
/// the engine derives Shift and Caps Lock from the US table ([`typed_character`]), which is
/// wrong elsewhere (German Shift+2 types `"`, Russian Shift+ф types `Ф`). Given, a TEXT press
/// writes it: as UTF-8 in the legacy and text-preserving kitty modes, as the shifted key under
/// `REPORT_ALTERNATE_KEYS` while Shift is held, and as `REPORT_ASSOCIATED_TEXT`'s text. The
/// kitty key code stays `key`'s.
///
/// Honoured only for a `Key::Character` press with no Ctrl, Alt, Meta, Hyper or Super: a chord
/// keeps its base-key encoding (Ctrl+c is 0x03 on every layout), and a control character is
/// never text.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key_with_text(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    base_layout_key: Option<char>,
    layout_text: Option<char>,
) -> Vec<u8> {
    let layout_text = text_press_glyph(key, modifiers, layout_text);
    // Kitty folds keypad keys onto their main-block twins ([`Key::main_block_twin`]) unless
    // the app asked to tell them apart: disambiguate, report-all, or DECKPAM (the legacy
    // spelling of that request). With no kitty flag the legacy encoder owns keypad keys.
    //
    // The fold reaches only the kitty encoder: the twin is the key's kitty name, and the
    // legacy encoder's main-row tables would make Shift+KP_5 `%`. A keypad key does not
    // compose (Shift picks a level and cancels DECKPAM), so the legacy fallback keeps the
    // original key, and `modifiers` passes through untouched on both paths.
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

    // ConPTY win32-input-mode (DEC 9001): a key conhost cannot read from legacy VT goes out
    // as its INPUT_RECORD pair ([`win32_key_record`]), ahead of the kitty gate and xterm
    // modifyOtherKeys, because conhost is the only reader: an application's kitty push and
    // modifyOtherKeys reach this terminal, but conhost drops the `CSI 13;2 u`, `CSI 27;2;13 ~`
    // and `CSI 27 u` written back. A routed key's release encodes to nothing: the pair carries
    // its key-up. 9001 is never set on Unix.
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
        return encode_kitty(
            kitty_key,
            modifiers,
            mode,
            event_type,
            base_layout_key,
            layout_text,
        );
    }

    if event_type == KeyEventType::Release {
        return Vec::new();
    }

    // Ctrl on a non-Latin layout: the layout's letter (Russian 'с') has no control byte, so
    // the physical key's US letter (`base_layout_key`, 'c') names it, as macOS terminals do.
    // Kitty, above, reports the layout's key with the US key in its base-layout field.
    let ctrl_twin = ctrl_layout_twin(key, modifiers, base_layout_key);
    let key = ctrl_twin.as_ref().unwrap_or(key);

    // The kitty protocol supersedes xterm modifyOtherKeys (an app may set both): a key the
    // kitty gate left as text (Shift+a -> 'A') is not re-escaped in the xterm dialect.
    if !mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        && let Some(bytes) = encode_xterm_other_keys(key, modifiers, mode, layout_text)
    {
        return bytes;
    }

    encode_legacy(key, modifiers, mode, layout_text)
}

/// The chord modifiers that make a press something other than TEXT: with any
/// of them held the key encodes from its base (a control byte, an ESC
/// prefix, a CSI form), never from the layout's composed character.
const NON_TEXT_MODIFIERS: Modifiers = Modifiers::CTRL
    .union(Modifiers::ALT)
    .union(Modifiers::META)
    .union(Modifiers::HYPER)
    .union(Modifiers::SUPER);

/// `layout_text` when it may stand for this press's text: a character key,
/// no [`NON_TEXT_MODIFIERS`], and a printable glyph. See
/// [`encode_key_with_text`].
#[cfg_attr(trust_verify, trust::skip)]
fn text_press_glyph(key: &Key, modifiers: Modifiers, layout_text: Option<char>) -> Option<char> {
    if !matches!(key, Key::Character(_)) || modifiers.intersects(NON_TEXT_MODIFIERS) {
        return None;
    }
    layout_text.filter(|glyph| !glyph.is_control())
}

/// The US letter a Ctrl chord on a non-Latin layout stands for: `Some('c')` for Ctrl + Russian
/// 'с' (physical C), the C0 byte Terminal.app and iTerm2 send. Only for a letter the control
/// table does not know, on a key whose US twin is a letter: a Latin layout keeps its bytes,
/// and a letter on a US digit or symbol key (AZERTY 'é' on 2) stays as it is.
#[cfg_attr(trust_verify, trust::skip)]
fn ctrl_layout_twin(key: &Key, modifiers: Modifiers, base_layout_key: Option<char>) -> Option<Key> {
    let Key::Character(c) = key else {
        return None;
    };
    if !modifiers.contains(Modifiers::CTRL)
        || c.is_ascii()
        || !c.is_alphabetic()
        || ctrl_character(*c).is_some()
    {
        return None;
    }
    base_layout_key
        .filter(char::is_ascii_alphabetic)
        .map(Key::Character)
}

/// Modifier and lock keys, mirroring kitty's `is_modifier_key` (kitty also gates
/// ISO_LEVEL3/5_SHIFT, which `NamedKey` lacks). Kitty reports these only under
/// REPORT_ALL_KEYS_AS_ESC, and the GUI's press path runs no viewport snap or selection clear
/// for them (holding Command or Shift is not typing): one list keeps the two gates together.
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

#[cfg_attr(trust_verify, trust::skip)]
fn should_encode_kitty_event(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> bool {
    // Releases report only under REPORT_EVENT_TYPES. Without it `encode_kitty` drops the `:3`
    // subfield, so a release would encode identical to a press and a DISAMBIGUATE-only app
    // would see every key twice; the legacy release path encodes nothing.
    if event_type == KeyEventType::Release && !mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
        return false;
    }

    // Modifier and lock keys report only under REPORT_ALL_KEYS_AS_ESC; legacy encodes them to
    // nothing, so standing down is exact parity.
    if is_modifier_or_lock_key(key) {
        return mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC);
    }

    // REPORT_ALL_KEYS_AS_ESC sends every key through `encode_kitty`.
    if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }

    // Without REPORT_ALL_KEYS_AS_ESC, a text-producing event (a bare or Shift-only Character
    // or Space) stays UTF-8 and has no release. Enter/Tab/Backspace, the reset escape hatch,
    // have no release in any modifier form. Other non-text events carry the `:3`.
    if event_type == KeyEventType::Release {
        if is_reset_escape_hatch_key(key) {
            return false;
        }
        return key_event_needs_csi_u(key, modifiers);
    }

    // Press/Repeat under DISAMBIGUATE: only ambiguous chords escape; kitty sends presses and
    // repeats of plain and shifted text keys as plain UTF-8.
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

/// Whether a key press/repeat needs the CSI-u form under kitty modes that keep text as text
/// (disambiguate, and event-types-only repeats). Kitty disambiguates "Esc, alt+key, ctrl+key,
/// ctrl+alt+key, shift+alt+key"; a bare or Shift-only text key composes text and is not
/// ambiguous. Enter/Tab/Backspace keep their bytes only unmodified: Shift+Enter is
/// `ESC[13;2u`.
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

/// Encode using the Kitty keyboard protocol. Functional keys with a legacy CSI form (arrows,
/// Home/End, Insert/Delete, Page Up/Down, F1-F12, KP_BEGIN) keep it in every mode, report-all
/// included (kitty's encode_function_key, #7474: an arrow release is `ESC[1;1:3A`); the rest
/// use `CSI unicode [; modifiers [: event-type]] u`.
#[cfg_attr(trust_verify, trust::skip)]
fn encode_kitty(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    base_layout_key: Option<char>,
    layout_text: Option<char>,
) -> Vec<u8> {
    let kitty_modifiers = kitty_modifiers_for_event(key, modifiers, event_type);

    if let Some(legacy) = encode_kitty_legacy_functional(key, kitty_modifiers, mode, event_type) {
        return legacy;
    }

    let (primary_code, alternate_code, base_layout_code) =
        kitty_key_codes(key, kitty_modifiers, mode, base_layout_key, layout_text);
    let associated_text =
        associated_text_codepoints(key, kitty_modifiers, mode, event_type, layout_text);

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

    if let Some(codepoint) = associated_text {
        buf.push(b';');
        write_u32(&mut buf, codepoint);
    }

    buf.push(b'u');
    buf
}

/// For a named key with a legacy CSI form, that form with kitty's modifier value (1+mods);
/// `None` for keys that use CSI u.
///
/// - Arrows, Home/End, KP_BEGIN: CSI [1;{mod}] A/B/C/D, H/F, E
/// - F1, F2, F4: CSI [1;{mod}] P/Q/S
/// - Insert/Delete/PageUp/PageDown, F3, F5-F12: CSI {num} [;{mod}] ~
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
    let event = if report_events && event_type != KeyEventType::Press {
        Some(event_type)
    } else {
        None
    };

    Some(match named {
        NamedKey::ArrowUp => letter_final(b'A', mod_value, event),
        NamedKey::ArrowDown => letter_final(b'B', mod_value, event),
        NamedKey::ArrowRight => letter_final(b'C', mod_value, event),
        NamedKey::ArrowLeft => letter_final(b'D', mod_value, event),
        NamedKey::Home => letter_final(b'H', mod_value, event),
        NamedKey::End => letter_final(b'F', mod_value, event),
        NamedKey::NumpadBegin => letter_final(b'E', mod_value, event),
        NamedKey::Insert => tilde_final(2, mod_value, event),
        NamedKey::Delete => tilde_final(3, mod_value, event),
        NamedKey::PageUp => tilde_final(5, mod_value, event),
        NamedKey::PageDown => tilde_final(6, mod_value, event),
        // F3 is tilde-only: the spec dropped `CSI R`, which collides with the Cursor Position
        // Report.
        NamedKey::F1 => letter_final(b'P', mod_value, event),
        NamedKey::F2 => letter_final(b'Q', mod_value, event),
        NamedKey::F3 => tilde_final(13, mod_value, event),
        NamedKey::F4 => letter_final(b'S', mod_value, event),
        // F13-F24 have no legacy form in kitty's table: they take their CSI-u numbers
        // (57376+) in every mode, not xterm's 25~..38~.
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

/// Build the modifier suffix ";{mod}[:event]" portion.
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
#[cfg_attr(trust_verify, trust::skip)]
fn tilde_final(num: u8, mod_value: u8, event: Option<KeyEventType>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    buf.extend_from_slice(b"\x1b[");
    write_u8(&mut buf, num);
    append_mod_event(&mut buf, mod_value, event);
    buf.push(b'~');
    buf
}

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

    // The bit reflects the state including this event (spec). A press or repeat sets it; on a
    // release `modifiers` already is the post-event state, which only the caller knows (kitty
    // keeps the bit while the key's other side is still held).
    let mut adjusted = modifiers;
    if event_type != KeyEventType::Release {
        adjusted.insert(modifier_flag);
    }
    adjusted
}

#[cfg_attr(trust_verify, trust::skip)]
fn kitty_key_codes(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
    layout_text: Option<char>,
) -> (u32, Option<u32>, Option<u32>) {
    match key {
        Key::Named(named) => (named.kitty_code(), None, None),
        Key::Character(c) => {
            let primary = *c as u32;
            if !mode.contains(KeyboardMode::REPORT_ALTERNATE_KEYS) {
                return (primary, None, None);
            }
            // The shifted key is "the shifted version, in the currently active
            // keyboard layout" (spec), present only while Shift is held: the
            // layout's own glyph when the host gave it (German Shift+2 is `"`,
            // 34), the US table's otherwise.
            let alternate = shifted_character(*c, modifiers)
                .map(|us| layout_text.map_or(us, |t| if t == *c { shift_level(t) } else { t }))
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

#[cfg_attr(trust_verify, trust::skip)]
fn associated_text_codepoints(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
    layout_text: Option<char>,
) -> Option<u32> {
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
            // The text the OS produced: the layout's when given, else the derivation, in
            // which Caps Lock uppercases a letter like Shift does.
            let text = layout_text.unwrap_or_else(|| typed_character(*c, modifiers));
            if text.is_control() {
                None
            } else {
                Some(text as u32)
            }
        }
        // A named key reports the text it types: its own character ([`named_key_text`];
        // Space reports 32), else its keypad twin's glyph (KP_5 → `5`) while the key code
        // stays the keypad's. The twin's glyph goes out as is, with no shift/caps table: Shift
        // on the keypad picks a level, never a glyph (NumLock on, Shift+KP_5 is KP_Begin, which
        // has no twin; NumLock off it is KP_5). The control-code filter drops Enter, Tab and
        // Backspace.
        Key::Named(named) => {
            let text = named_key_text(*named).or(match key.main_block_twin() {
                Some(Key::Character(glyph)) => Some(glyph),
                _ => None,
            });
            match text {
                Some(glyph) if !glyph.is_control() => Some(glyph as u32),
                _ => None,
            }
        }
    }
}

/// The character a named key types, before any control-code filter: Space, Enter (`'\r'`),
/// Tab (`'\t'`) and Backspace (`'\x7f'`). `None` for the keys that type nothing and for the
/// keypad keys, whose glyph comes from [`Key::main_block_twin`]. The control codes are listed
/// so `associated_text_codepoints` drops them by the spec's one exclusion (code points below
/// U+0020), which keeps U+0020 in. Backspace's DECBKM byte (0x7f/0x08) does not matter here:
/// both are control codes.
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

/// The Shift level a layout's typed glyph stands for, for kitty's shifted key: the glyph,
/// except that a lowercase letter typed with Shift held is Caps Lock's doing (xkb's Lock
/// cancels Shift on a letter), so the level is its capital. The caller asks only when the
/// glyph is the unshifted key itself: a layout's Shift level may hold another lowercase
/// letter (Swiss German Shift+ö is é), which is the level as it is. A capital that is not one
/// character ('ß') leaves the glyph as it is.
#[cfg_attr(trust_verify, trust::skip)]
fn shift_level(glyph: char) -> char {
    if !glyph.is_lowercase() {
        return glyph;
    }
    let mut upper = glyph.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(capital), None) => capital,
        _ => glyph,
    }
}

/// The character a TEXT press of base key `c` types when no layout says otherwise: Shift
/// through the US table ([`shifted_character`]), and Caps Lock upper-casing a letter (caps OR
/// shift gives the capital, as macOS composes it and kitty's associated text reports it).
/// The fallback for a key with no layout text (a controller's `key a mods=caps`); the window
/// asks it whether the layout's own character differs.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_character(c: char, modifiers: Modifiers) -> char {
    let shifted = shifted_character(c, modifiers).unwrap_or(c);
    if !modifiers.contains(Modifiers::CAPS_LOCK) {
        return shifted;
    }
    // A capital that is one character: 'ß' (→ "SS") stays as it is.
    let mut upper = shifted.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(capital), None) => capital,
        _ => shifted,
    }
}

/// The glyph SHIFT composes from base key `c` on the US layout (`'h'`→`'H'`, `'2'`→`'@'`,
/// `'/'`→`'?'`; `c` itself when Shift does not change it), or `None` when SHIFT is not held.
/// The legacy encoder picks its byte from this, and the GUI's predicted echo reaches it
/// through [`typed_character`], so a predicted glyph matches the shell's echo.
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

/// ConPTY win32-input-mode: `VK_RETURN` (13) and scan code 0x1C (28), shared by both Enter
/// keys; the keypad's is flagged `ENHANCED_KEY` in `dwControlKeyState`.
const WIN32_VK_RETURN: u32 = 13;
const WIN32_SC_RETURN: u32 = 28;
/// `VK_ESCAPE` / scan 0x01 and `VK_BACK` / scan 0x0E, with the `UnicodeChar` a Windows
/// keyboard gives each: ESC (0x1B) and BS (0x08). BS, not the DEL aterm's legacy Backspace
/// writes: conhost puts BS in the Alt+Backspace record it decodes from `ESC DEL`.
const WIN32_VK_ESCAPE: u32 = 0x1b;
const WIN32_SC_ESCAPE: u32 = 0x01;
const WIN32_ESCAPE_CHAR: u32 = 0x1b;
const WIN32_VK_BACK: u32 = 0x08;
const WIN32_SC_BACK: u32 = 0x0e;
const WIN32_BACK_CHAR: u32 = 0x08;
/// `dwControlKeyState` bits (wincon.h). The LEFT modifier bits stand for either side: once
/// winit has canonicalized the chord the host cannot tell them apart, and conhost reports the
/// same for a modifier it cannot side-attribute.
const WIN32_SHIFT_PRESSED: u32 = 0x10;
const WIN32_LEFT_CTRL_PRESSED: u32 = 0x08;
const WIN32_LEFT_ALT_PRESSED: u32 = 0x02;
const WIN32_ENHANCED_KEY: u32 = 0x100;
/// The `UnicodeChar` of an Enter-chord record: what the physical chord carries under Windows
/// Terminal (the OS's `ToUnicode` of it), so every ReadConsoleInput reader (.NET `ReadKey`,
/// libuv, MSYS, cmd.exe) sees the record it sees under WT. Shift+Enter is CR; Enter with CTRL
/// held, with or without Shift, is LF (conhost reads a bare 0x0A as Ctrl+Enter).
///
/// Not aterm's Unix Shift+Enter LF (`encode_legacy.rs`, the `Enter` arm): no Windows keyboard
/// makes `Shift+Enter, UnicodeChar 10`, and cmd.exe's cooked reader ignores that record where
/// it runs the line on the CR one.
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

/// The win32-input-mode record for a key conhost cannot read from legacy VT, or `None` for a
/// key it reads correctly, which keeps its legacy bytes: the Enter chords legacy VT has no
/// byte for ([`win32_enter_chord`]) and the keys whose legacy bytes conhost holds once it has
/// read a record ([`win32_escape_record`]).
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

/// The win32-input-mode record for an Enter chord: `Some` only for Enter (main block or
/// keypad) with SHIFT and/or CTRL, ALT riding along. Plain Enter (CR) and Alt+Enter (`ESC CR`)
/// keep their legacy bytes, which conhost reads correctly even after
/// [`conhost_holds_once_switched`]'s switch. The keypad Enter is the same VK/scan pair flagged
/// `ENHANCED_KEY`; it is routed too because the legacy keypad arm writes the main Enter's
/// Shift+Enter LF, which conhost reads as Ctrl+Enter. DECKPAM is not consulted: the record
/// names the physical key, and conhost applies keypad mode itself.
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

/// Whether conhost's input parser, once it has read a win32-input-mode record, holds these
/// legacy key bytes instead of delivering the key they spell (conhost 10.0.26200).
///
/// Before the first record conhost settles an ambiguous tail at the end of a read: a lone ESC
/// is Escape, `ESC [` is Alt+[. The first record turns that flush off for the session (a
/// win32-input-mode terminal never sends a bare ESC), and aterm's first Shift+Enter is a
/// record. After it:
///
/// - `ESC` alone stays pending and the next key reads as Alt+key: Escape and Ctrl+[.
/// - `ESC ESC`, `ESC DEL` lose the second byte and the ESC stays pending: Alt+Escape,
///   Ctrl+Alt+[, Alt+Backspace.
/// - `ESC [`, `ESC O`, `ESC P`, `ESC ]`, `ESC X`, `ESC ^`, `ESC _` open a CSI / SS3 / DCS /
///   OSC / SOS / PM / APC that swallows what follows: Alt+[, Alt+Shift+O, ….
///
/// Every other ESC pair and every complete CSI/SS3 sequence arrives intact, so it keeps its
/// bytes. (`ESC ESC [ E`, Alt+keypad-5 with NumLock off, is dropped before and after the
/// switch alike, so it is outside this rule.)
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

/// The record for a key whose legacy bytes [`conhost_holds_once_switched`]: Escape (any
/// modifiers), Alt+Backspace, and a character chord legacy spells as one of those forms
/// (Ctrl+[ and Ctrl+3 are a lone ESC, Alt+[ is `ESC [`, Ctrl+Alt+8 is `ESC DEL`). `None` for
/// every other key, and for a character no US-QWERTY key types: every held form is ASCII, so
/// the legacy bytes are its only spelling.
///
/// The record re-spells what the legacy bytes said. `Uc` is the byte after the Alt prefix (BS
/// for Backspace). `LEFT_ALT_PRESSED` is set exactly when the legacy form carried the ESC
/// prefix. `Vk`/`Sc` name the physical key: `base_layout_key`'s US identity, else the
/// character's own US key, which adds SHIFT for a shifted glyph (`_` is Shift+-).
///
/// Ctrl+[ is the one key whose identity moves: it goes out as the record a Windows keyboard
/// makes for it (`VK_OEM_4`, char 0x1B), not the Escape key conhost read its lone ESC as
/// before the switch, which would name a key nobody pressed. vim, which reads the char, still
/// leaves insert mode; PSReadLine, which binds by key, inserts `^[`. Ctrl+Escape and
/// Alt+Escape go out as records too and conhost drops them (its rule for the Start-menu and
/// window-cycling keys); the legacy bytes would swallow the next key instead.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_escape_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
) -> Option<Win32KeyRecord> {
    let legacy = encode_legacy(key, modifiers, mode, None);
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

/// The US-QWERTY main-block key that types `c`: its `wVirtualKeyCode`, its set-1 scan code (a
/// physical position, the same on every layout), and whether `c` is its shifted glyph. The VK
/// is the US one, as conhost's own `VkKeyScan` answers for these characters. `None` for
/// anything the US main block cannot type.
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

/// The win32-input-mode `INPUT_RECORD` pair (key-down, then key-up) for a routed key:
/// `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _` with all six fields spelled, as Windows Terminal writes
/// them (microsoft/terminal doc/specs/#4999-win32-input-mode.md). `Uc` and `Cs` match on both
/// halves, `Kd` is 1 then 0, `Rc` is 1. Both halves come from the press: the host keeps no
/// per-key state, so the release path emits nothing for a routed key.
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
#[cfg_attr(trust_verify, trust::skip)]
fn encode_legacy(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    layout_text: Option<char>,
) -> Vec<u8> {
    // Caps Lock matters only for TEXT (Caps Lock + a types 'A'): a window press brings the
    // layout's character, a controller key derives it ([`typed_character`]).
    let caps_lock = modifiers.contains(Modifiers::CAPS_LOCK);
    // Caps Lock is a lock, not a chord modifier, in xterm's encoding; left in, arrows and
    // F-keys would take the modified `ESC[1;1A` form and lose SS3 `ESC OA`. Only the kitty
    // modifier byte reports it.
    let modifiers = modifiers & !Modifiers::CAPS_LOCK;
    // DEC private mode 1035 (xterm `numLock`) reset sets `NO_SPECIAL_MODIFIERS`: NumLock
    // is then no modifier either.
    let modifiers = if mode.contains(KeyboardMode::NO_SPECIAL_MODIFIERS) {
        modifiers & !Modifiers::NUM_LOCK
    } else {
        modifiers
    };
    match key {
        // A TEXT press writes the character it types: the layout's when given, else Caps
        // Lock's capital. Everything else is the legacy encoder's (Shift table, control and
        // ESC forms).
        Key::Character(c) => match layout_text.or_else(|| {
            (caps_lock && !modifiers.intersects(NON_TEXT_MODIFIERS))
                .then(|| typed_character(*c, modifiers | Modifiers::CAPS_LOCK))
        }) {
            Some(glyph) => {
                let mut buf = [0u8; 4];
                glyph.encode_utf8(&mut buf).as_bytes().to_vec()
            }
            None => encode_character_legacy(*c, modifiers, mode),
        },
        Key::Named(named) => encode_named_legacy(*named, modifiers, mode),
    }
}

#[cfg_attr(trust_verify, trust::skip)]
fn encode_xterm_other_keys(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    layout_text: Option<char>,
) -> Option<Vec<u8>> {
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

    // xterm's modifyOtherKeys decision is key-sensitive: level 1 keeps the unambiguous
    // Shift/Ctrl encodings and Backspace's DECBKM/Ctrl toggle, so Alt+Backspace and
    // Ctrl+Backspace stay on the legacy path. Keypad keys never get here: xterm files them
    // under modifyKeypadKeys.
    //
    // Level 2 escapes a Shift-only character whose shifted keysym is in '@'..DEL, and that
    // keysym is the layout's when the host gave it: German Shift+2 is `"`, plain text, where
    // the US table's '@' is escaped.
    let apply = match (level, key, layout_text) {
        (1, ..) => xterm_modify_other_keys_level1_applies(key, effective_mods),
        (2, Key::Character(_), Some(glyph)) if effective_mods == Modifiers::SHIFT => {
            (u32::from(b'@')..=u32::from(0x7f_u8)).contains(&u32::from(glyph))
        }
        (2, ..) => xterm_modify_other_keys_level2_applies(key, effective_mods),
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
            // xterm checks the shifted keysym: Shift-only characters below '@' stay text
            // (`1` -> `!`, `,` -> `<`); the control-input range '@'..DEL is escaped. Space,
            // the one exception, is NamedKey::Space in the native/DOM mappings.
            let shifted = shifted_character(*c, modifiers).unwrap_or(*c) as u32;
            (u32::from(b'@')..=u32::from(0x7f_u8)).contains(&shifted)
        }
        _ => true,
    }
}

/// Write a u8 as decimal digits to a buffer.
#[cfg_attr(trust_verify, trust::skip)]
fn write_u8(buf: &mut Vec<u8>, val: u8) {
    write_u32(buf, u32::from(val));
}
/// Write a u32 as decimal digits to a buffer.
#[cfg_attr(trust_verify, trust::skip)]
fn write_u32(buf: &mut Vec<u8>, val: u32) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    // Digits go least-significant-first into a 16-slot scratch (u32::MAX has 10), then out
    // in reverse. A literal divisor, masked indices and `wrapping_add` on a 0..=9 digit leave
    // the Trust gate no div-by-zero, bounds or overflow obligation.
    let mut digits = [0u8; 16];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 10 {
        digits[count & 15] = b'0'.wrapping_add((n % 10) as u8);
        n /= 10;
        // Saturating is exact (`count < 10`, `count > 0`); the verifier cannot carry the
        // loop conditions into the arithmetic.
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
#[cfg(test)]
#[path = "layout_text_tests.rs"]
mod layout_text_tests;
