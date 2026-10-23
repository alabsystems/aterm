// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The win32-input-mode records for the NAMED keys whose legacy bytes conhost reads as another key, or not at all: Ctrl+Break, the NumLock-on keypad (in keypad application mode too, its Enter included), Shift+Space, the Ctrl+Space chords and Shift+Backspace, and under OpenConsole KP_Begin and F13-F20.

use super::{
    Key, KeyboardMode, Modifiers, NamedKey, WIN32_BACK_CHAR, WIN32_ENHANCED_KEY,
    WIN32_LEFT_ALT_PRESSED, WIN32_LEFT_CTRL_PRESSED, WIN32_SC_BACK, WIN32_SC_RETURN,
    WIN32_SHIFT_PRESSED, WIN32_VK_BACK, WIN32_VK_RETURN, Win32KeyRecord,
};

/// `VK_CANCEL` / scan 0x46: the key Windows reports for Ctrl+Pause (Ctrl+Break), flagged
/// `ENHANCED_KEY` as the E0-prefixed scan arrives.
const WIN32_VK_CANCEL: u32 = 0x03;
const WIN32_SC_CANCEL: u32 = 0x46;
/// `VK_SPACE` / scan 0x39, with the char a Windows keyboard gives Shift+Space.
const WIN32_VK_SPACE: u32 = 0x20;
const WIN32_SC_SPACE: u32 = 0x39;
const WIN32_SPACE_CHAR: u32 = 0x20;
/// `NUMLOCK_ON` in `dwControlKeyState`.
const WIN32_NUMLOCK_ON: u32 = 0x20;
/// `VK_CLEAR` / scan 0x4C: the key Windows reports for the keypad's 5 with NumLock off.
const WIN32_VK_CLEAR: u32 = 0x0c;
const WIN32_SC_KEYPAD_5: u32 = 0x4c;
/// The Enter `UnicodeChar`, CR.
const WIN32_RETURN_CHAR: u32 = 0x0d;

/// The record for a named key [`super::win32_key_record`]'s own families leave alone, or `None` for a key that keeps its legacy bytes (see the module note for the measured exclusions).
#[cfg_attr(trust_verify, trust::skip)]
pub(super) fn win32_named_record(
    key: &Key,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Option<Win32KeyRecord> {
    let Key::Named(named) = key else {
        return None;
    };
    let openconsole = mode.contains(KeyboardMode::OPENCONSOLE_HOST);
    match named {
        NamedKey::Pause => win32_ctrl_break(modifiers),
        NamedKey::Space => win32_space_chord(modifiers),
        NamedKey::Backspace => win32_shift_backspace(modifiers),
        NamedKey::NumpadBegin => openconsole.then(|| win32_keypad_begin(modifiers)),
        NamedKey::NumpadEnter => win32_keypad_enter(modifiers, mode),
        _ => win32_f13_f20(*named, modifiers, mode).or_else(|| win32_keypad(*named, modifiers)),
    }
}

/// KP_Begin (the keypad's 5 with NumLock off) under OpenConsole: the `VK_CLEAR` record, char 0,
/// with every chord bit, Alt included (`CSI 1;3E` for Git Bash; nothing for `ReadKey`, where
/// the legacy `ESC ESC [ E` typed `[E`).
#[cfg_attr(trust_verify, trust::skip)]
const fn win32_keypad_begin(modifiers: Modifiers) -> Win32KeyRecord {
    Win32KeyRecord {
        vk: WIN32_VK_CLEAR,
        sc: WIN32_SC_KEYPAD_5,
        uc: 0,
        cs: chord_state(modifiers),
    }
}

/// The keypad's Enter while a program has DECKPAM set: its `ENHANCED_KEY` `VK_RETURN` record
/// with CR, which reaches a `ReadKey` reader under OpenConsole as Enter where the legacy `SS3 M`
/// reached it as nothing, and a VT reader on either host as CR, as under Windows Terminal (see
/// the module note).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_keypad_enter(modifiers: Modifiers, mode: KeyboardMode) -> Option<Win32KeyRecord> {
    let routed = mode.contains(KeyboardMode::APP_KEYPAD) && !modifiers.contains(Modifiers::ALT);
    routed.then(|| Win32KeyRecord {
        vk: WIN32_VK_RETURN,
        sc: WIN32_SC_RETURN,
        uc: WIN32_RETURN_CHAR,
        cs: chord_state(modifiers) | WIN32_ENHANCED_KEY,
    })
}

/// F13-F20 under OpenConsole: `VK_F13`.. with their set-1 scan codes (0x64..0x6B), char 0, and
/// every chord bit.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_f13_f20(
    named: NamedKey,
    modifiers: Modifiers,
    mode: KeyboardMode,
) -> Option<Win32KeyRecord> {
    if !mode.contains(KeyboardMode::OPENCONSOLE_HOST) {
        return None;
    }
    const KEYS: [NamedKey; 8] = [
        NamedKey::F13,
        NamedKey::F14,
        NamedKey::F15,
        NamedKey::F16,
        NamedKey::F17,
        NamedKey::F18,
        NamedKey::F19,
        NamedKey::F20,
    ];
    let index = KEYS.iter().position(|&key| key == named)?;
    let index = u32::try_from(index).ok()?;
    Some(Win32KeyRecord {
        vk: 0x7c + index,
        sc: 0x64 + index,
        uc: 0,
        cs: chord_state(modifiers),
    })
}

/// The chord bits of `dwControlKeyState`, LEFT standing for either side as elsewhere.
#[cfg_attr(trust_verify, trust::skip)]
const fn chord_state(modifiers: Modifiers) -> u32 {
    let mut cs = 0;
    if modifiers.contains(Modifiers::SHIFT) {
        cs |= WIN32_SHIFT_PRESSED;
    }
    if modifiers.contains(Modifiers::CTRL) {
        cs |= WIN32_LEFT_CTRL_PRESSED;
    }
    if modifiers.contains(Modifiers::ALT) {
        cs |= WIN32_LEFT_ALT_PRESSED;
    }
    cs
}

/// Ctrl+Pause is Ctrl+Break, which legacy VT cannot spell: the encoder wrote nothing, so `ping
/// -t` never printed its statistics and a batch file or msbuild could not be broken into.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_ctrl_break(modifiers: Modifiers) -> Option<Win32KeyRecord> {
    modifiers.contains(Modifiers::CTRL).then(|| Win32KeyRecord {
        vk: WIN32_VK_CANCEL,
        sc: WIN32_SC_CANCEL,
        uc: 0,
        cs: chord_state(modifiers) | WIN32_ENHANCED_KEY,
    })
}

/// Shift+Space and the Ctrl+Space chords. Legacy has no Shift+Space (a bare space, read as
/// Space), and Ctrl+Space's NUL reads as Ctrl+Shift+2 (`D2 MODS=Shift+Control`); Ctrl+Alt+Space's
/// `ESC NUL` lost its NUL on the way to a VT reader (Git Bash got `ESC`).
#[cfg_attr(trust_verify, trust::skip)]
const fn win32_space_chord(modifiers: Modifiers) -> Option<Win32KeyRecord> {
    let shift = modifiers.contains(Modifiers::SHIFT);
    let ctrl = modifiers.contains(Modifiers::CTRL);
    let alt = modifiers.contains(Modifiers::ALT);
    let routed = if ctrl { alt || !shift } else { shift && !alt };
    if !routed {
        return None;
    }
    let uc = if ctrl { 0 } else { WIN32_SPACE_CHAR };
    Some(Win32KeyRecord {
        vk: WIN32_VK_SPACE,
        sc: WIN32_SC_SPACE,
        uc,
        cs: chord_state(modifiers),
    })
}

/// Shift+Backspace: legacy DEL reads as a bare Backspace (the Shift lost), and under DECBKM the
/// BS reads as Ctrl+Backspace, a word delete.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_shift_backspace(modifiers: Modifiers) -> Option<Win32KeyRecord> {
    let chord = modifiers & (Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT);
    (chord == Modifiers::SHIFT).then_some(Win32KeyRecord {
        vk: WIN32_VK_BACK,
        sc: WIN32_SC_BACK,
        uc: WIN32_BACK_CHAR,
        cs: WIN32_SHIFT_PRESSED,
    })
}

/// The NumLock-on keypad: `VK_NUMPAD0`..`VK_NUMPAD9`, `VK_DECIMAL` and the four operators, each
/// with its keypad scan code and the glyph it types.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_keypad(named: NamedKey, modifiers: Modifiers) -> Option<Win32KeyRecord> {
    if modifiers.contains(Modifiers::ALT) {
        return None;
    }
    const KEYPAD: [(NamedKey, u8, u8, u8); 15] = [
        (NamedKey::Numpad0, 0x60, 0x52, b'0'),
        (NamedKey::Numpad1, 0x61, 0x4f, b'1'),
        (NamedKey::Numpad2, 0x62, 0x50, b'2'),
        (NamedKey::Numpad3, 0x63, 0x51, b'3'),
        (NamedKey::Numpad4, 0x64, 0x4b, b'4'),
        (NamedKey::Numpad5, 0x65, 0x4c, b'5'),
        (NamedKey::Numpad6, 0x66, 0x4d, b'6'),
        (NamedKey::Numpad7, 0x67, 0x47, b'7'),
        (NamedKey::Numpad8, 0x68, 0x48, b'8'),
        (NamedKey::Numpad9, 0x69, 0x49, b'9'),
        (NamedKey::NumpadDecimal, 0x6e, 0x53, b'.'),
        (NamedKey::NumpadMultiply, 0x6a, 0x37, b'*'),
        (NamedKey::NumpadAdd, 0x6b, 0x4e, b'+'),
        (NamedKey::NumpadSubtract, 0x6d, 0x4a, b'-'),
        (NamedKey::NumpadDivide, 0x6f, 0x35, b'/'),
    ];
    let &(_, vk, sc, glyph) = KEYPAD.iter().find(|&&(key, ..)| key == named)?;
    let numlock =
        glyph.is_ascii_digit() || glyph == b'.' || modifiers.contains(Modifiers::NUM_LOCK);
    let mut cs = chord_state(modifiers);
    if numlock {
        cs |= WIN32_NUMLOCK_ON;
    }
    if named == NamedKey::NumpadDivide {
        cs |= WIN32_ENHANCED_KEY;
    }
    Some(Win32KeyRecord {
        vk: u32::from(vk),
        sc: u32::from(sc),
        uc: u32::from(glyph),
        cs,
    })
}

#[cfg(test)]
#[path = "encode_win32_keys_tests.rs"]
mod tests;
