// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Legacy/xterm-compatible keyboard encoding helpers.

use super::{KeyboardMode, Modifiers, NamedKey};

pub(super) fn encode_character_legacy(
    c: char,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Vec<u8> {
    // DEC private mode 1039 (xterm `altSendsEscape`): set (the default) prefixes
    // an Alt-modified key with ESC. Reset (`ALT_NO_ESC`) suppresses the prefix.
    let alt_sends_escape =
        modifiers.contains(Modifiers::ALT) && !mode.contains(KeyboardMode::ALT_NO_ESC);
    // DEC private mode 1036 (xterm `metaSendsEscape`): when set (`META_SENDS_ESC`)
    // a Meta-modified key is ESC-prefixed, mirroring Alt. Off by default.
    let meta_sends_escape =
        modifiers.contains(Modifiers::META) && mode.contains(KeyboardMode::META_SENDS_ESC);

    // Trust gate note: `[..].to_vec()` instead of `vec![..]` throughout this
    // file's byte-sequence constructors. The `vec!` list macro lowers to
    // `into_vec(box [..])`, whose inlined allocation/pointer writes the Trust
    // full verifier refutes spuriously (the known workspace-wide
    // `trust-bitmask-refute` alignment artifact, ledger item T10);
    // `<[u8]>::to_vec` is a modeled std callee, so the artifact never fires.
    // Behavior-identical: both build a fresh heap `Vec<u8>` with the same
    // length, capacity, and bytes — the legacy encode tests pin the bytes.
    if modifiers.contains(Modifiers::CTRL)
        && let Some(ctrl_char) = ctrl_character(c)
    {
        if alt_sends_escape || meta_sends_escape {
            return [0x1b, ctrl_char].to_vec();
        }
        return [ctrl_char].to_vec();
    }

    if alt_sends_escape || meta_sends_escape {
        let mut buf = [0x1b].to_vec();
        // Meta sends ESC + the glyph that would be typed. Under SHIFT that is the
        // SHIFTED glyph, not merely an uppercased letter — `to_ascii_uppercase`
        // no-ops on digits/symbols, so Alt+Shift+2 wrongly emitted ESC '2'
        // instead of ESC '@'. Reuse the engine's US-QWERTY shift table.
        let glyph = if modifiers.contains(Modifiers::SHIFT) {
            super::shifted_character(c, modifiers).unwrap_or(c)
        } else {
            c.to_ascii_lowercase()
        };
        let mut char_buf = [0u8; 4];
        let encoded = glyph.encode_utf8(&mut char_buf);
        buf.extend_from_slice(encoded.as_bytes());
        return buf;
    }

    // SHIFT must yield the SHIFTED glyph, not just an uppercased letter. The old
    // `to_ascii_uppercase` silently no-ops on every non-letter, so Shift+2 stayed
    // '2' instead of '@' and EVERY shifted symbol was lost in legacy mode — the
    // "Shift doesn't work" regression. Reuse `shifted_character`, the same
    // US-QWERTY table the Kitty `REPORT_ALTERNATE_KEYS` path already uses, so the
    // legacy and Kitty shift mappings agree. (Layout-aware shifting for non-US
    // keyboards is a separate, larger concern: the engine has no live layout.)
    let output = super::shifted_character(c, modifiers).unwrap_or(c);

    let mut buf = [0u8; 4];
    let encoded = output.encode_utf8(&mut buf);
    encoded.as_bytes().to_vec()
}

pub(super) fn encode_named_legacy(
    key: NamedKey,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Vec<u8> {
    let app_cursor = mode.contains(KeyboardMode::APP_CURSOR);
    let app_keypad = mode.contains(KeyboardMode::APP_KEYPAD);
    // Lock bits (CapsLock/NumLock) are folded into the modifier set upstream but
    // must NOT turn an unmodified navigation/function key into its modified CSI
    // form (e.g. Up -> ESC[1;1A). Mask to the real chord modifiers — mirroring
    // encode_xterm_other_keys — so lock state is ignored on the legacy path.
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

    if let Some(encoded) = encode_function_named_legacy(key, modifiers, has_modifiers) {
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
            // aterm INPUT POLICY — a terminal that forces programs to be better rather
            // than faking its own identity to coax them. Legacy VT100/xterm has no
            // escape for Shift+Enter, so every other terminal sends a plain CR for it,
            // which is why "Shift+Enter for a newline" silently fails in TUIs (e.g.
            // Claude Code) that haven't enabled the Kitty protocol (handled EARLIER in
            // `encode_key_with_layout`; reaching this legacy arm means NO protocol is
            // active). Instead of masquerading as a recognized terminal, aterm IMPOSES
            // the useful behaviour directly: a bare Shift+Enter emits LF (0x0a), which
            // every app using the universal CR=submit / LF=insert-newline convention
            // (Claude Code, readline, vim, …) honours as a newline — no negotiation, no
            // fake identity. Plain Enter stays CR; Alt keeps Meta-Enter (ESC CR);
            // Ctrl+Enter stays CR.
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
        NamedKey::Escape => {
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x1b]
            } else {
                vec![0x1b]
            }
        }
        NamedKey::Backspace => {
            // DECBKM (mode 67): when set, Backspace sends BS (0x08) and DEL (0x7f)
            // becomes the Ctrl-modified form; default (reset) is the reverse. The
            // Alt form ESC-prefixes the unmodified byte. (xterm `backarrowKey`.)
            let bksp = if mode.contains(KeyboardMode::BACKARROW_SENDS_BS) {
                0x08
            } else {
                0x7f
            };
            let ctrl_bksp = if bksp == 0x08 { 0x7f } else { 0x08 };
            if modifiers.contains(Modifiers::CTRL) {
                vec![ctrl_bksp]
            } else if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, bksp]
            } else {
                vec![bksp]
            }
        }
        NamedKey::Space => {
            if modifiers.contains(Modifiers::CTRL) {
                vec![0x00]
            } else if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, 0x20]
            } else {
                vec![0x20]
            }
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
            encode_home_end(b'H', app_cursor, modifiers, has_modifiers)
        }
        NamedKey::End | NamedKey::NumpadEnd => {
            encode_home_end(b'F', app_cursor, modifiers, has_modifiers)
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

fn encode_function_named_legacy(
    key: NamedKey,
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::F1 => encode_f1_f4(b'P', modifiers, has_modifiers),
        NamedKey::F2 => encode_f1_f4(b'Q', modifiers, has_modifiers),
        NamedKey::F3 => encode_f1_f4(b'R', modifiers, has_modifiers),
        NamedKey::F4 => encode_f1_f4(b'S', modifiers, has_modifiers),
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
        // NumpadEnter: SS3 M in DECKPAM, CR otherwise. Per VT420 spec,
        // this distinguishes numpad Enter from main Enter (#7558).
        NamedKey::NumpadEnter => encode_numpad(b'M', '\r', app_keypad, vt52, modifiers),
        NamedKey::NumpadEqual => encode_character_legacy('=', modifiers, mode),
        // NumpadSeparator: comma on some international keyboards (SS3 l in DECKPAM).
        NamedKey::NumpadSeparator => encode_numpad(b'l', ',', app_keypad, vt52, modifiers),
        // NumpadBegin (KP_BEGIN / center 5 key): SS3 E in DECKPAM, '5' otherwise.
        // xterm encodes this as ESC O E in app mode, ESC [E in normal mode.
        NamedKey::NumpadBegin => {
            let effective_app = app_keypad && !modifiers.contains(Modifiers::SHIFT);
            if modifiers.contains(Modifiers::ALT) {
                vec![0x1b, b'5']
            } else if vt52 && effective_app {
                // VT52 application keypad: ESC ? 5
                vec![0x1b, b'?', b'5']
            } else if effective_app {
                vec![0x1b, b'O', b'E']
            } else {
                vec![b'5']
            }
        }
        // Numpad navigation (NumpadArrow*, NumpadHome, etc.) is handled by
        // encode_navigation_named_legacy which runs earlier in the call chain.
        _ => return None,
    })
}

fn ctrl_character(c: char) -> Option<u8> {
    let c_upper = c.to_ascii_uppercase();
    if c_upper.is_ascii_uppercase() {
        // `c_upper` is 'A'..='Z' here, so `c_upper as u8` is 65..=90 and the
        // control byte is 1..=26. The saturating ops are identity on that range
        // (they only discharge the Trust gate's underflow/overflow obligations,
        // which the `is_ascii_uppercase` guard already rules out in practice).
        Some((c_upper as u8).saturating_sub(b'A').saturating_add(1))
    } else {
        match c {
            ' ' | '@' | '2' => Some(0x00),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '^' | '6' => Some(0x1e),
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
    modifiers: Modifiers,
    has_modifiers: bool,
) -> Vec<u8> {
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

fn encode_f1_f4(suffix: u8, modifiers: Modifiers, has_modifiers: bool) -> Vec<u8> {
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
    // Per xterm: Shift cancels application keypad mode, forcing numeric output.
    // Ctrl has no effect on numpad digits in legacy encoding. (#7480)
    //
    // Trust gate note: `char_val` is always an ASCII numpad glyph, so the
    // narrowing to `u8` never truncates and `& 0xFF` is identity — masking
    // before the cast is behavior-identical (`char as u8` already takes the
    // low byte for every input) and discharges the cast-overflow obligations
    // (Phase 1.3). The stdlib `vec!` alignment refutations this let the
    // verifier reach (the workspace-wide `trust-bitmask-refute` artifact,
    // ledger item T10) are killed by building the sequences as
    // `[..].to_vec()` instead of `vec![..]` — same fresh `Vec<u8>`, same
    // bytes, but via the modeled `<[u8]>::to_vec` rather than the macro's
    // inlined `into_vec(box [..])` allocation the artifact refutes.
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
