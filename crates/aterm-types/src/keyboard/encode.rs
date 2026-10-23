// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard input encoding: legacy VT100/xterm sequences and the Kitty keyboard protocol.

#[path = "encode_legacy.rs"]
mod encode_legacy;
#[path = "encode_win32_keys.rs"]
mod encode_win32_keys;

use super::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey};
use encode_legacy::{
    chord_ctrl_byte, ctrl_character, encode_character_legacy, encode_named_legacy,
};

/// Encode a key press into terminal bytes, legacy or Kitty protocol as `mode` says.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn encode_key(key: &Key, modifiers: Modifiers, mode: KeyboardMode) -> Vec<u8> {
    encode_key_with_event(key, modifiers, mode, KeyEventType::Press)
}

/// Encode a key event with its event type (repeat and release report under the Kitty protocol).
#[inline]
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

/// Encode a key event with an optional `base_layout_key`.
#[inline]
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

/// [`encode_key_with_layout`] plus `layout_text`.
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
    let layout_text = layout_glyph(key, layout_text);
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
    let ctrl_twin = ctrl_layout_twin(key, modifiers, base_layout_key, layout_text);
    let legacy_key = ctrl_twin.as_ref().unwrap_or(key);
    let legacy_text = if ctrl_twin.is_some() {
        None
    } else {
        layout_text
    };

    if mode.contains(KeyboardMode::WIN32_INPUT)
        && let Some(record) =
            win32_key_record(legacy_key, modifiers, mode, base_layout_key, legacy_text)
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

    if !mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
        && let Some(bytes) = encode_xterm_other_keys(legacy_key, modifiers, mode, legacy_text)
    {
        return bytes;
    }

    encode_legacy(legacy_key, modifiers, mode, legacy_text)
}

/// The chord modifiers that make a press something other than TEXT: with any
/// of them held the key writes its chord form (a control byte, an ESC prefix,
/// a CSI form) where one exists, and otherwise the character the layout types
/// at that level ([`encode_key_with_text`]).
const NON_TEXT_MODIFIERS: Modifiers = Modifiers::CTRL
    .union(Modifiers::ALT)
    .union(Modifiers::META)
    .union(Modifiers::HYPER)
    .union(Modifiers::SUPER);

/// `layout_text` when it may stand for the layout's character on this key: a character key
/// and a printable glyph.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn layout_glyph(key: &Key, layout_text: Option<char>) -> Option<char> {
    if !matches!(key, Key::Character(_)) {
        return None;
    }
    layout_text.filter(|glyph| !glyph.is_control())
}

/// The US key a Ctrl chord on a non-Latin layout stands for: `Some('c')` for Ctrl + Russian
/// 'с' (physical C) and `Some('[')` for Ctrl + Russian 'х' or German 'ü' (physical [), the C0
/// byte kitty (its `alternate_key` for a non-ASCII key) and ghostty (its physical key) send;
/// Terminal.app and iTerm2 send Ctrl + Russian 'с' as ETX too.
#[cfg_attr(trust_verify, trust::skip)]
fn ctrl_layout_twin(
    key: &Key,
    modifiers: Modifiers,
    base_layout_key: Option<char>,
    layout_glyph: Option<char>,
) -> Option<Key> {
    let Key::Character(c) = key else {
        return None;
    };
    if !modifiers.contains(Modifiers::CTRL)
        || c.is_ascii()
        || chord_ctrl_byte(*c, modifiers, layout_glyph).is_some()
    {
        return None;
    }
    let base = base_layout_key?;
    chord_ctrl_byte(base, modifiers, None).map(|_| Key::Character(base))
}

/// The US-QWERTY letter that a non-Latin layout's letter stands for in a chord.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn layout_letter_twin(layout_char: char, base_layout_key: Option<char>) -> Option<char> {
    if layout_char.is_ascii() || !layout_char.is_alphabetic() {
        return None;
    }
    base_layout_key.filter(char::is_ascii_alphabetic)
}

/// Modifier and lock keys, mirroring kitty's `is_modifier_key` (kitty also gates ISO_LEVEL3/5_SHIFT, which `NamedKey` lacks).
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub const fn is_modifier_or_lock_key(key: &Key) -> bool {
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

#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn should_encode_kitty_event(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    event_type: KeyEventType,
) -> bool {
    // Releases report only under REPORT_EVENT_TYPES. Without it `encode_kitty` drops the `:3`
    if event_type == KeyEventType::Release && !mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
        return false;
    }

    if is_modifier_or_lock_key(key) {
        return mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC);
    }

    if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }

    if event_type == KeyEventType::Release {
        if is_reset_escape_hatch_key(key) {
            return false;
        }
        return key_event_needs_csi_u(key, modifiers);
    }

    if mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES) {
        return key_event_needs_csi_u(key, modifiers);
    }

    if event_type == KeyEventType::Press || !mode.contains(KeyboardMode::REPORT_EVENT_TYPES) {
        return false;
    }

    key_event_needs_csi_u(key, modifiers)
}

/// Chord modifiers — the bits that make a key combination "ambiguous" in
/// legacy encodings.
const CHORD_MODIFIERS: Modifiers = Modifiers::SHIFT
    .union(Modifiers::ALT)
    .union(Modifiers::CTRL)
    .union(Modifiers::SUPER);

/// Kitty's recovery keys have no release event unless report-all is active,
/// regardless of modifiers.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
const fn is_reset_escape_hatch_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace)
    )
}

/// Whether a key press/repeat needs the CSI-u form under kitty modes that keep text as text
/// (disambiguate, and event-types-only repeats).
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn key_event_needs_csi_u(key: &Key, modifiers: Modifiers) -> bool {
    let effective_mods = modifiers & CHORD_MODIFIERS;
    let text_mods = effective_mods.is_empty() || effective_mods == Modifiers::SHIFT;
    match key {
        Key::Character(_) => !text_mods,
        Key::Named(NamedKey::Space | NamedKey::Backspace) => !text_mods,
        Key::Named(NamedKey::Enter | NamedKey::Tab) => !effective_mods.is_empty(),
        Key::Named(_) => true,
    }
}

/// Encode using the Kitty keyboard protocol.
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
        NamedKey::F1 => letter_final(b'P', mod_value, event),
        NamedKey::F2 => letter_final(b'Q', mod_value, event),
        NamedKey::F3 => tilde_final(13, mod_value, event),
        NamedKey::F4 => letter_final(b'S', mod_value, event),
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
            let alternate = shifted_character(*c, modifiers)
                .map(|us| layout_text.map_or(us, |t| if t == *c { shift_level(t) } else { t }))
                .map(u32::from)
                .filter(|alt| *alt != primary);
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

    if modifiers.intersects(Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER) {
        return None;
    }

    match key {
        Key::Character(c) => {
            let text = layout_text.unwrap_or_else(|| typed_character(*c, modifiers));
            if text.is_control() {
                None
            } else {
                Some(text as u32)
            }
        }
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
/// Tab (`'\t'`) and Backspace (`'\x7f'`).
#[inline]
#[must_use]
const fn named_key_text(named: NamedKey) -> Option<char> {
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
/// cancels Shift on a letter), so the level is its capital.
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

/// The character a TEXT press of base key `c` types when no layout says otherwise.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_character(c: char, modifiers: Modifiers) -> char {
    let shifted = shifted_character(c, modifiers).unwrap_or(c);
    if !modifiers.contains(Modifiers::CAPS_LOCK) {
        return shifted;
    }
    let mut upper = shifted.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(capital), None) => capital,
        _ => shifted,
    }
}

/// The glyph SHIFT composes from base key `c` on the US layout (`'h'`→`'H'`, `'2'`→`'@'`, `'/'`→`'?'`; `c` itself when Shift does not change it), or `None` when SHIFT is not held.
#[inline]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub const fn shifted_character(c: char, modifiers: Modifiers) -> Option<char> {
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
/// keyboard gives each: ESC (0x1B) and BS (0x08).
const WIN32_VK_ESCAPE: u32 = 0x1b;
const WIN32_SC_ESCAPE: u32 = 0x01;
const WIN32_ESCAPE_CHAR: u32 = 0x1b;
const WIN32_VK_BACK: u32 = 0x08;
const WIN32_SC_BACK: u32 = 0x0e;
const WIN32_BACK_CHAR: u32 = 0x08;
/// `dwControlKeyState` bits (wincon.h).
const WIN32_SHIFT_PRESSED: u32 = 0x10;
const WIN32_LEFT_CTRL_PRESSED: u32 = 0x08;
const WIN32_LEFT_ALT_PRESSED: u32 = 0x02;
const WIN32_ENHANCED_KEY: u32 = 0x100;
/// The `UnicodeChar` of an Enter-chord record: what the physical chord carries under Windows
/// Terminal (the OS's `ToUnicode` of it), so every ReadConsoleInput reader (.NET `ReadKey`,
/// libuv, MSYS, cmd.exe) sees the record it sees under WT.
const WIN32_SHIFT_ENTER_CHAR: u32 = 13;
const WIN32_CTRL_ENTER_CHAR: u32 = 10;

/// The four `INPUT_RECORD` fields a routed key decides — `wVirtualKeyCode`,
/// `wVirtualScanCode`, `UnicodeChar` and `dwControlKeyState`.
struct Win32KeyRecord {
    vk: u32,
    sc: u32,
    uc: u32,
    cs: u32,
}

/// The win32-input-mode record for a key conhost cannot read from legacy VT, or reads as a
/// different key, or `None` for a key it reads correctly, which keeps its legacy bytes: the
/// Enter chords legacy VT has no byte for ([`win32_enter_chord`]), the keys whose legacy bytes
/// conhost holds once it has read a record ([`win32_escape_record`]), Ctrl+H and Ctrl+J,
/// whose BS and LF conhost reads as Ctrl+Backspace and Ctrl+Enter
/// ([`win32_ctrl_character_record`]), and the named keys legacy
/// loses or misnames: Ctrl+Break, the keypad, and the Space and Backspace chords, and under
/// OpenConsole ([`KeyboardMode::OPENCONSOLE_HOST`]) KP_Begin and F13-F20
/// ([`encode_win32_keys::win32_named_record`]).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_key_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
    layout_glyph: Option<char>,
) -> Option<Win32KeyRecord> {
    win32_enter_chord(key, modifiers)
        .or_else(|| win32_escape_record(key, modifiers, mode, base_layout_key, layout_glyph))
        .or_else(|| {
            win32_ctrl_character_record(key, modifiers, mode, base_layout_key, layout_glyph)
        })
        .or_else(|| encode_win32_keys::win32_named_record(key, modifiers, mode))
}

/// The win32-input-mode record for an Enter chord: `Some` only for Enter (main block or
/// keypad) with SHIFT and/or CTRL, ALT riding along.
#[cfg_attr(trust_verify, trust::skip)]
const fn win32_enter_chord(key: &Key, modifiers: Modifiers) -> Option<Win32KeyRecord> {
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
#[inline]
const fn conhost_holds_once_switched(legacy: &[u8]) -> bool {
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
/// (Ctrl+[ and Ctrl+3 are a lone ESC, Alt+[ is `ESC [`, Ctrl+Alt+8 is `ESC DEL`).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_escape_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
    layout_glyph: Option<char>,
) -> Option<Win32KeyRecord> {
    let legacy = encode_legacy(key, modifiers, mode, layout_glyph);
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
    if legacy.len() == 2 {
        cs |= WIN32_LEFT_ALT_PRESSED;
    }
    Some(Win32KeyRecord { vk, sc, uc, cs })
}

/// BS, the legacy byte of Ctrl+H: conhost decodes a bare BS as Ctrl+Backspace.
const LEGACY_CTRL_H: u8 = 0x08;

/// LF, the legacy byte of Ctrl+J: conhost decodes a bare LF as Ctrl+Enter.
const LEGACY_CTRL_J: u8 = 0x0a;

/// The record for a Ctrl letter chord whose legacy byte conhost reads as a different key:
/// `Some` only when the legacy bytes are a lone BS or a lone LF, which a letter key writes
/// for Ctrl+H and Ctrl+J and their Shift chords, Caps Lock or not (and Ctrl+р on a Russian
/// layout's H key, through [`ctrl_layout_twin`]).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_ctrl_character_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
    base_layout_key: Option<char>,
    layout_glyph: Option<char>,
) -> Option<Win32KeyRecord> {
    if !modifiers.contains(Modifiers::CTRL) {
        return None;
    }
    let twin = ctrl_layout_twin(key, modifiers, base_layout_key, layout_glyph);
    let key = twin.as_ref().unwrap_or(key);
    let Key::Character(c) = key else {
        return None;
    };
    let legacy = encode_legacy(key, modifiers, mode, layout_glyph);
    let [byte @ (LEGACY_CTRL_H | LEGACY_CTRL_J)] = legacy[..] else {
        return None;
    };
    if !c.is_ascii_alphabetic() {
        return None;
    }
    let (_, sc, _) = base_layout_key
        .and_then(us_physical_key)
        .or_else(|| us_physical_key(*c))?;
    Some(Win32KeyRecord {
        vk: u32::from(c.to_ascii_uppercase()),
        sc,
        uc: u32::from(byte),
        cs: WIN32_LEFT_CTRL_PRESSED,
    })
}

/// The US-QWERTY main-block key that types `c`: its `wVirtualKeyCode`, its set-1 scan code (a
/// physical position, the same on every layout), and whether `c` is its shifted glyph.
#[cfg_attr(trust_verify, trust::skip)]
fn us_physical_key(c: char) -> Option<(u32, u32, bool)> {
    const LETTER_SCAN: [u8; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
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
/// them (microsoft/terminal doc/specs/#4999-win32-input-mode.md).
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
    let caps_lock = modifiers.contains(Modifiers::CAPS_LOCK);
    let modifiers = modifiers & !Modifiers::CAPS_LOCK;
    let modifiers = if mode.contains(KeyboardMode::NO_SPECIAL_MODIFIERS) {
        modifiers & !Modifiers::NUM_LOCK
    } else {
        modifiers
    };
    let text_press = !modifiers.intersects(NON_TEXT_MODIFIERS);
    match key {
        Key::Character(c) => match layout_text.filter(|_| text_press).or_else(|| {
            (caps_lock && text_press).then(|| typed_character(*c, modifiers | Modifiers::CAPS_LOCK))
        }) {
            Some(glyph) => {
                let mut buf = [0u8; 4];
                glyph.encode_utf8(&mut buf).as_bytes().to_vec()
            }
            None => encode_character_legacy(*c, modifiers, mode, layout_text),
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
        Key::Character(c) => {
            u32::from(layout_text.unwrap_or_else(|| shifted_character(c, modifiers).unwrap_or(c)))
        }
        Key::Named(NamedKey::Tab) => 9,
        Key::Named(NamedKey::Enter) => 13,
        Key::Named(NamedKey::Escape) => 27,
        Key::Named(NamedKey::Backspace) => 127,
        Key::Named(NamedKey::Space) => 32,
        _ => return None,
    };

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
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(b"\x1b[");
        write_u32(&mut buf, code);
        buf.push(b';');
        write_u8(&mut buf, mod_value);
        buf.push(b'u');
        Some(buf)
    } else {
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
fn xterm_modify_other_keys_level1_applies(key: &Key, modifiers: Modifiers) -> bool {
    if modifiers.is_empty() {
        return false;
    }
    match key {
        Key::Named(NamedKey::Backspace) => false,
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
            let shifted = shifted_character(*c, modifiers).unwrap_or(*c) as u32;
            (u32::from(b'@')..=u32::from(0x7f_u8)).contains(&shifted)
        }
        _ => true,
    }
}

/// Write a u8 as decimal digits to a buffer.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn write_u8(buf: &mut Vec<u8>, val: u8) {
    write_u32(buf, u32::from(val));
}
/// Write a u32 as decimal digits to a buffer.
#[inline]
#[cfg_attr(trust_verify, trust::skip)]
fn write_u32(buf: &mut Vec<u8>, val: u32) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    let mut digits = [0u8; 16];
    let mut n = val;
    let mut count = 0usize;
    while n > 0 && count < 10 {
        digits[count & 15] = b'0'.wrapping_add((n % 10) as u8);
        n /= 10;
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
