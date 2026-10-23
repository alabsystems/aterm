// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy/xterm-compatible keyboard encoding helpers.

use super::{KeyboardMode, Modifiers, NamedKey};

/// A character key's legacy bytes for a chord (or a text press with no layout character).
pub(super) fn encode_character_legacy(
    c: char,
    modifiers: Modifiers,
    mode: KeyboardMode,
    layout_glyph: Option<char>,
) -> Vec<u8> {
    let alt_sends_escape =
        modifiers.contains(Modifiers::ALT) && !mode.contains(KeyboardMode::ALT_NO_ESC);
    let meta_sends_escape =
        modifiers.contains(Modifiers::META) && mode.contains(KeyboardMode::META_SENDS_ESC);

    if modifiers.contains(Modifiers::CTRL)
        && let Some(ctrl_char) = chord_ctrl_byte(c, modifiers, layout_glyph)
    {
        if alt_sends_escape || meta_sends_escape {
            return [0x1b, ctrl_char].to_vec();
        }
        return [ctrl_char].to_vec();
    }

    let typed = layout_glyph.unwrap_or_else(|| super::shifted_character(c, modifiers).unwrap_or(c));

    if alt_sends_escape || meta_sends_escape {
        let mut buf = [0x1b].to_vec();
        let glyph = if layout_glyph.is_some() || modifiers.contains(Modifiers::SHIFT) {
            typed
        } else {
            c.to_ascii_lowercase()
        };
        let mut char_buf = [0u8; 4];
        let encoded = glyph.encode_utf8(&mut char_buf);
        buf.extend_from_slice(encoded.as_bytes());
        return buf;
    }

    let mut buf = [0u8; 4];
    let encoded = typed.encode_utf8(&mut buf);
    encoded.as_bytes().to_vec()
}

/// The control byte a Ctrl chord on character key `c` sends.
#[inline]
pub(super) fn chord_ctrl_byte(
    c: char,
    modifiers: Modifiers,
    layout_glyph: Option<char>,
) -> Option<u8> {
    ctrl_character(c).or_else(|| {
        layout_glyph
            .or_else(|| super::shifted_character(c, modifiers))
            .and_then(ctrl_character)
    })
}

pub(super) fn encode_named_legacy(
    key: NamedKey,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Vec<u8> {
    let app_cursor = mode.contains(KeyboardMode::APP_CURSOR);
    let app_keypad = mode.contains(KeyboardMode::APP_KEYPAD);
    let modifiers =
        modifiers & (Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER);
    let has_modifiers = !modifiers.is_empty();

    if let Some(encoded) = encode_control_named_legacy(key, modifiers, mode) {
        return encoded;
    }

    let vt52 = mode.contains(KeyboardMode::VT52_MODE);
    if let Some(encoded) =
        encode_navigation_named_legacy(key, app_cursor, vt52, modifiers, has_modifiers)
    {
        return encoded;
    }

    if let Some(encoded) = encode_function_named_legacy(key, vt52, modifiers, has_modifiers) {
        return encoded;
    }

    if let Some(encoded) = encode_numpad_named_legacy(key, app_keypad, vt52, modifiers, mode) {
        return encoded;
    }

    Vec::new()
}

fn encode_control_named_legacy(
    key: NamedKey,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::Enter => {
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x0d]
            } else if modifiers.contains(Modifiers::SHIFT) && !modifiers.contains(Modifiers::CTRL) {
                vec![0x0a]
            } else {
                vec![0x0d]
            }
        }
        NamedKey::Tab => {
            if modifiers.contains(Modifiers::SHIFT) {
                vec![0x1b, b'[', b'Z']
            } else if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x09]
            } else {
                vec![0x09]
            }
        }
        NamedKey::Escape => {
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x1b]
            } else {
                vec![0x1b]
            }
        }
        NamedKey::Backspace => {
            let bksp = if mode.contains(KeyboardMode::BACKARROW_SENDS_BS) {
                0x08
            } else {
                0x7f
            };
            let ctrl_bksp = if bksp == 0x08 { 0x7f } else { 0x08 };
            let byte = if modifiers.contains(Modifiers::CTRL) {
                ctrl_bksp
            } else {
                bksp
            };
            alt_prefixed(modifiers, byte)
        }
        NamedKey::Space => {
            let byte = if modifiers.contains(Modifiers::CTRL) {
                0x00
            } else {
                0x20
            };
            alt_prefixed(modifiers, byte)
        }
        _ => return None,
    })
}

fn encode_navigation_named_legacy(
    key: NamedKey,
    app_cursor: bool,
    vt52: bool,
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::ArrowUp | NamedKey::NumpadArrowUp => {
            encode_arrow(b'A', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::ArrowDown | NamedKey::NumpadArrowDown => {
            encode_arrow(b'B', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::ArrowRight | NamedKey::NumpadArrowRight => {
            encode_arrow(b'C', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::ArrowLeft | NamedKey::NumpadArrowLeft => {
            encode_arrow(b'D', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::Home | NamedKey::NumpadHome => {
            encode_home_end(b'H', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::End | NamedKey::NumpadEnd => {
            encode_home_end(b'F', app_cursor, vt52, modifiers, has_modifiers)
        }
        NamedKey::PageUp | NamedKey::NumpadPageUp => encode_tilde_key(5, modifiers, has_modifiers),
        NamedKey::PageDown | NamedKey::NumpadPageDown => {
            encode_tilde_key(6, modifiers, has_modifiers)
        }
        NamedKey::Insert | NamedKey::NumpadInsert => encode_tilde_key(2, modifiers, has_modifiers),
        NamedKey::Delete | NamedKey::NumpadDelete => encode_tilde_key(3, modifiers, has_modifiers),
        _ => return None,
    })
}

/// `byte`, ESC-prefixed when Alt is held.
#[inline]
fn alt_prefixed(modifiers: Modifiers, byte: u8) -> Vec<u8> {
    if modifiers.contains(Modifiers::ALT) {
        vec![0x1b, byte]
    } else {
        vec![byte]
    }
}

fn encode_function_named_legacy(
    key: NamedKey,
    vt52: bool,
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::F1 => encode_f1_f4(b'P', vt52, modifiers, has_modifiers),
        NamedKey::F2 => encode_f1_f4(b'Q', vt52, modifiers, has_modifiers),
        NamedKey::F3 => encode_f1_f4(b'R', vt52, modifiers, has_modifiers),
        NamedKey::F4 => encode_f1_f4(b'S', vt52, modifiers, has_modifiers),
        NamedKey::F5 => encode_tilde_key(15, modifiers, has_modifiers),
        NamedKey::F6 => encode_tilde_key(17, modifiers, has_modifiers),
        NamedKey::F7 => encode_tilde_key(18, modifiers, has_modifiers),
        NamedKey::F8 => encode_tilde_key(19, modifiers, has_modifiers),
        NamedKey::F9 => encode_tilde_key(20, modifiers, has_modifiers),
        NamedKey::F10 => encode_tilde_key(21, modifiers, has_modifiers),
        NamedKey::F11 => encode_tilde_key(23, modifiers, has_modifiers),
        NamedKey::F12 => encode_tilde_key(24, modifiers, has_modifiers),
        NamedKey::F13 => encode_tilde_key(25, modifiers, has_modifiers),
        NamedKey::F14 => encode_tilde_key(26, modifiers, has_modifiers),
        NamedKey::F15 => encode_tilde_key(28, modifiers, has_modifiers),
        NamedKey::F16 => encode_tilde_key(29, modifiers, has_modifiers),
        NamedKey::F17 => encode_tilde_key(31, modifiers, has_modifiers),
        NamedKey::F18 => encode_tilde_key(32, modifiers, has_modifiers),
        NamedKey::F19 => encode_tilde_key(33, modifiers, has_modifiers),
        NamedKey::F20 => encode_tilde_key(34, modifiers, has_modifiers),
        NamedKey::F21 => encode_tilde_key(35, modifiers, has_modifiers),
        NamedKey::F22 => encode_tilde_key(36, modifiers, has_modifiers),
        NamedKey::F23 => encode_tilde_key(37, modifiers, has_modifiers),
        NamedKey::F24 => encode_tilde_key(38, modifiers, has_modifiers),
        NamedKey::F25
        | NamedKey::F26
        | NamedKey::F27
        | NamedKey::F28
        | NamedKey::F29
        | NamedKey::F30
        | NamedKey::F31
        | NamedKey::F32
        | NamedKey::F33
        | NamedKey::F34
        | NamedKey::F35 => Vec::new(),
        _ => return None,
    })
}

fn encode_numpad_named_legacy(
    key: NamedKey,
    app_keypad: bool,
    vt52: bool,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::Numpad0 => encode_numpad(b'p', '0', app_keypad, vt52, modifiers),
        NamedKey::Numpad1 => encode_numpad(b'q', '1', app_keypad, vt52, modifiers),
        NamedKey::Numpad2 => encode_numpad(b'r', '2', app_keypad, vt52, modifiers),
        NamedKey::Numpad3 => encode_numpad(b's', '3', app_keypad, vt52, modifiers),
        NamedKey::Numpad4 => encode_numpad(b't', '4', app_keypad, vt52, modifiers),
        NamedKey::Numpad5 => encode_numpad(b'u', '5', app_keypad, vt52, modifiers),
        NamedKey::Numpad6 => encode_numpad(b'v', '6', app_keypad, vt52, modifiers),
        NamedKey::Numpad7 => encode_numpad(b'w', '7', app_keypad, vt52, modifiers),
        NamedKey::Numpad8 => encode_numpad(b'x', '8', app_keypad, vt52, modifiers),
        NamedKey::Numpad9 => encode_numpad(b'y', '9', app_keypad, vt52, modifiers),
        NamedKey::NumpadDecimal => encode_numpad(b'n', '.', app_keypad, vt52, modifiers),
        NamedKey::NumpadDivide => encode_numpad(b'o', '/', app_keypad, vt52, modifiers),
        NamedKey::NumpadMultiply => encode_numpad(b'j', '*', app_keypad, vt52, modifiers),
        NamedKey::NumpadSubtract => encode_numpad(b'm', '-', app_keypad, vt52, modifiers),
        NamedKey::NumpadAdd => encode_numpad(b'k', '+', app_keypad, vt52, modifiers),
        NamedKey::NumpadEnter => {
            let effective_app = app_keypad && !modifiers.contains(Modifiers::SHIFT);
            if !effective_app || modifiers.contains(Modifiers::ALT) {
                return encode_control_named_legacy(NamedKey::Enter, modifiers, mode);
            }
            if vt52 {
                [0x1b, b'?', b'M'].to_vec()
            } else {
                [0x1b, b'O', b'M'].to_vec()
            }
        }
        NamedKey::NumpadEqual => encode_character_legacy('=', modifiers, mode, None),
        NamedKey::NumpadSeparator => encode_numpad(b'l', ',', app_keypad, vt52, modifiers),
        NamedKey::NumpadBegin => {
            let effective_app = app_keypad && !modifiers.contains(Modifiers::SHIFT);
            if modifiers.contains(Modifiers::ALT) {
                [0x1b, 0x1b, b'[', b'E'].to_vec()
            } else if vt52 && effective_app {
                [0x1b, b'?', b'5'].to_vec()
            } else if effective_app {
                [0x1b, b'O', b'E'].to_vec()
            } else {
                [0x1b, b'[', b'E'].to_vec()
            }
        }
        _ => return None,
    })
}

#[inline]
pub(super) const fn ctrl_character(c: char) -> Option<u8> {
    let c_upper = c.to_ascii_uppercase();
    if c_upper.is_ascii_uppercase() {
        Some((c_upper as u8).saturating_sub(b'A').saturating_add(1))
    } else {
        match c {
            ' ' | '@' | '2' => Some(0x00),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '^' | '6' | '~' => Some(0x1e),
            '_' | '/' | '7' => Some(0x1f),
            '?' | '8' => Some(0x7f),
            _ => None,
        }
    }
}

fn encode_arrow(
    suffix: u8,
    app_cursor: bool,
    vt52: bool,
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Vec<u8> {
    if vt52 {
        return vec![0x1b, suffix];
    }
    if has_modifiers {
        let mut buf = vec![0x1b, b'[', b'1', b';'];
        super::write_u8(&mut buf, modifiers.xterm_encoded());
        buf.push(suffix);
        buf
    } else if app_cursor {
        vec![0x1b, b'O', suffix]
    } else {
        vec![0x1b, b'[', suffix]
    }
}

fn encode_home_end(
    suffix: u8,
    app_cursor: bool,
    vt52: bool,
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Vec<u8> {
    if vt52 {
        return vec![0x1b, suffix];
    }
    if has_modifiers {
        let mut buf = vec![0x1b, b'[', b'1', b';'];
        super::write_u8(&mut buf, modifiers.xterm_encoded());
        buf.push(suffix);
        buf
    } else if app_cursor {
        vec![0x1b, b'O', suffix]
    } else {
        vec![0x1b, b'[', suffix]
    }
}

fn encode_tilde_key(num: u8, modifiers: Modifiers, has_modifiers: bool) -> Vec<u8> {
    let mut buf = vec![0x1b, b'['];
    super::write_u8(&mut buf, num);

    if has_modifiers {
        buf.push(b';');
        super::write_u8(&mut buf, modifiers.xterm_encoded());
    }

    buf.push(b'~');
    buf
}

fn encode_f1_f4(suffix: u8, vt52: bool, modifiers: Modifiers, has_modifiers: bool) -> Vec<u8> {
    if vt52 {
        return vec![0x1b, suffix];
    }
    if has_modifiers {
        let mut buf = vec![0x1b, b'[', b'1', b';'];
        super::write_u8(&mut buf, modifiers.xterm_encoded());
        buf.push(suffix);
        buf
    } else {
        vec![0x1b, b'O', suffix]
    }
}

fn encode_numpad(
    ss3_suffix: u8,
    char_val: char,
    app_keypad: bool,
    vt52: bool,
    modifiers: Modifiers,
) -> Vec<u8> {
    let effective_app = app_keypad && !modifiers.contains(Modifiers::SHIFT);
    if modifiers.contains(Modifiers::ALT) {
        let mut buf = [0x1b].to_vec();
        buf.push((char_val as u32 & 0xFF) as u8);
        buf
    } else if vt52 && effective_app {
        [0x1b, b'?', (char_val as u32 & 0xFF) as u8].to_vec()
    } else if effective_app {
        [0x1b, b'O', ss3_suffix].to_vec()
    } else {
        [(char_val as u32 & 0xFF) as u8].to_vec()
    }
}
