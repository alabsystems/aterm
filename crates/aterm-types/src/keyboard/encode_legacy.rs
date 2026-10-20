// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy/xterm-compatible keyboard encoding helpers.
//!
//! Some constructors use `[..].to_vec()` rather than `vec![..]`: `vec!` lowers to
//! `into_vec(box [..])`, which the Trust full verifier refutes spuriously, while
//! `<[u8]>::to_vec` is a modeled std callee. The bytes are the same.

use super::{KeyboardMode, Modifiers, NamedKey};

/// A character key's legacy bytes for a chord (or a text press with no layout character).
/// `layout_glyph` is the layout's character on this key at the press's Shift level, Ctrl and
/// Alt aside, when the host knows it (`encode_key_with_text`); without it the US Shift table
/// names the glyph.
pub(super) fn encode_character_legacy(
    c: char,
    modifiers: Modifiers,
    mode: KeyboardMode,
    layout_glyph: Option<char>,
) -> Vec<u8> {
    // DEC private mode 1039 (xterm `altSendsEscape`): set (the default) prefixes
    // an Alt-modified key with ESC. Reset (`ALT_NO_ESC`) suppresses the prefix.
    let alt_sends_escape =
        modifiers.contains(Modifiers::ALT) && !mode.contains(KeyboardMode::ALT_NO_ESC);
    // DEC private mode 1036 (xterm `metaSendsEscape`): when set (`META_SENDS_ESC`)
    // a Meta-modified key is ESC-prefixed, mirroring Alt. Off by default.
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

    // The glyph the press types: the layout's when given (German Shift+2 is '"'), else the
    // US Shift table's (Shift+2 is '@').
    let typed = layout_glyph.unwrap_or_else(|| super::shifted_character(c, modifiers).unwrap_or(c));

    if alt_sends_escape || meta_sends_escape {
        let mut buf = [0x1b].to_vec();
        // Meta sends ESC + the glyph that would be typed, as xterm prefixes its
        // `XLookupString` text: German Alt+Shift+2 is ESC '"', US Alt+Shift+2 ESC '@'.
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

    // The US Shift table is the one the Kitty `REPORT_ALTERNATE_KEYS` path falls back to, so
    // the two agree.
    let mut buf = [0u8; 4];
    let encoded = typed.encode_utf8(&mut buf);
    encoded.as_bytes().to_vec()
}

/// The control byte a Ctrl chord on character key `c` sends, or `None` when neither the key
/// nor the character it types has one.
///
/// `Key::Character` carries the unshifted base, so a chord named after a shifted glyph
/// arrives as base + SHIFT (Ctrl-`_` is Ctrl+Shift+'-'). The typed character's control byte
/// is only a fallback: a chord whose base is in the table keeps its byte (Ctrl+Shift+'/'
/// stays 0x1F, not Ctrl-? = 0x7F). The typed character is the layout's (`layout_glyph`, German
/// Shift+ß is '?') when given, else the US Shift table's.
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
    // Lock bits must not turn an unmodified navigation or function key into its modified CSI
    // form (Up -> ESC[1;1A): keep only the chord modifiers, as encode_xterm_other_keys does.
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
            // Legacy VT has no Shift+Enter, so other terminals send CR and "Shift+Enter for a
            // newline" fails in TUIs without the Kitty protocol. Rather than fake another
            // terminal's identity, aterm sends LF (0x0a) for a bare Shift+Enter, which apps using
            // CR=submit / LF=newline (Claude Code, readline, vim) take as a newline. Plain and
            // Ctrl+Enter stay CR; Alt sends ESC CR. Under ConPTY win32-input-mode the Shift/Ctrl
            // chords go out earlier as win32 records (`win32_enter_chord`), since conhost reads a
            // bare LF as Ctrl+Enter.
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x0d]
            } else if modifiers.contains(Modifiers::SHIFT) && !modifiers.contains(Modifiers::CTRL) {
                vec![0x0a]
            } else {
                vec![0x0d]
            }
        }
        // NumpadEnter is handled by encode_numpad_named_legacy (DECKPAM → SS3 M, #7558).
        NamedKey::Tab => {
            if modifiers.contains(Modifiers::SHIFT) {
                vec![0x1b, b'[', b'Z']
            } else if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x09]
            } else {
                vec![0x09]
            }
        }
        // Under ConPTY win32-input-mode these bytes are only what `win32_escape_record`
        // inspects: Escape goes out as a record there.
        NamedKey::Escape => {
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x1b]
            } else {
                vec![0x1b]
            }
        }
        NamedKey::Backspace => {
            // DECBKM (mode 67, xterm `backarrowKey`): set, Backspace sends BS (0x08) and Ctrl
            // sends DEL (0x7f); reset (the default) is the reverse.
            let bksp = if mode.contains(KeyboardMode::BACKARROW_SENDS_BS) {
                0x08
            } else {
                0x7f
            };
            let ctrl_bksp = if bksp == 0x08 { 0x7f } else { 0x08 };
            // Ctrl picks the byte; Alt ESC-prefixes it, so Ctrl+Alt+Backspace is `ESC ^H`
            // (readline's M-C-h), as kitty's legacy C0 table and xterm send it.
            let byte = if modifiers.contains(Modifiers::CTRL) {
                ctrl_bksp
            } else {
                bksp
            };
            alt_prefixed(modifiers, byte)
        }
        NamedKey::Space => {
            // Ctrl+Alt+Space is `ESC NUL` (emacs C-M-SPC), as above.
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
        // NumpadEnter: SS3 M in DECKPAM (VT52: ESC ? M), which tells it from the main Return
        // (#7558); Shift cancels application keypad mode. Outside application mode, and under
        // Alt, it is the main Enter byte for byte (CR, Shift's LF, Alt's ESC CR). Under ConPTY
        // win32-input-mode a Shift/Ctrl+KP_Enter goes out earlier as an ENHANCED_KEY record, and
        // so does a DECKPAM KP_Enter, on either console host (`encode_win32_keys`).
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
        // NumpadSeparator: comma on some international keyboards (SS3 l in DECKPAM).
        NamedKey::NumpadSeparator => encode_numpad(b'l', ',', app_keypad, vt52, modifiers),
        // NumpadBegin (KP_BEGIN, the centre key with NumLock off): SS3 E in DECKPAM, CSI E
        // otherwise (xterm's `kb2`; the kitty encoder sends `E` too). The key has no glyph, so
        // it never sends a digit. Alt sends ESC + CSI E; VT52 application keypad `ESC ? 5`.
        // Under OpenConsole, which types an unknown CSI as text, it is the `VK_CLEAR` record
        // instead (`encode_win32_keys`).
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
        // Numpad navigation (NumpadArrow*, NumpadHome, etc.) is handled by
        // encode_navigation_named_legacy which runs earlier in the call chain.
        _ => return None,
    })
}

pub(super) fn ctrl_character(c: char) -> Option<u8> {
    let c_upper = c.to_ascii_uppercase();
    if c_upper.is_ascii_uppercase() {
        // `c_upper` is 'A'..='Z', so the byte is 1..=26; the saturating ops are identity
        // there and only discharge the Trust gate's overflow obligations.
        Some((c_upper as u8).saturating_sub(b'A').saturating_add(1))
    } else {
        match c {
            ' ' | '@' | '2' => Some(0x00),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            // `~` is Shift+` on US keyboards: Ctrl+~ is RS, kitty's legacy
            // ctrl table ("~ -> 30") and xterm's control alias.
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
    // VT52 mode: cursor keys are ESC A/B/C/D (no CSI/SS3), ignoring DECCKM.
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
    // VT52 (DECANM reset): `ESC H` / `ESC F`, modifiers ignored, as the
    // arrows' VT52 arm sends them (xterm's VT52_CURSOR_KEYS).
    if vt52 {
        return vec![0x1b, suffix];
    }
    if has_modifiers {
        let mut buf = vec![0x1b, b'[', b'1', b';'];
        super::write_u8(&mut buf, modifiers.xterm_encoded());
        buf.push(suffix);
        buf
    } else if app_cursor {
        // DECCKM: Home → SS3 H, End → SS3 F (matches xterm/kitty/alacritty)
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
    // VT52: PF1-PF4 are `ESC P`..`ESC S` (xterm applies VT52_CURSOR_KEYS to
    // the SS3 reply), modifiers ignored.
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
    // Shift cancels application keypad mode (xterm); Ctrl does nothing to a keypad digit
    // (#7480). `char_val` is ASCII, so `& 0xFF` before the cast is identity; it discharges the
    // cast-overflow obligation.
    let effective_app = app_keypad && !modifiers.contains(Modifiers::SHIFT);
    if modifiers.contains(Modifiers::ALT) {
        let mut buf = [0x1b].to_vec();
        buf.push((char_val as u32 & 0xFF) as u8);
        buf
    } else if vt52 && effective_app {
        // VT52 application keypad: ESC ? followed by the digit/symbol character.
        [0x1b, b'?', (char_val as u32 & 0xFF) as u8].to_vec()
    } else if effective_app {
        [0x1b, b'O', ss3_suffix].to_vec()
    } else {
        [(char_val as u32 & 0xFF) as u8].to_vec()
    }
}
