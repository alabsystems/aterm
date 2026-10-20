// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The win32-input-mode records for the NAMED keys whose legacy bytes conhost reads as
//! another key, or not at all: Ctrl+Break, the NumLock-on keypad (in keypad application mode
//! too, its Enter included), Shift+Space, the Ctrl+Space chords and Shift+Backspace, and under
//! OpenConsole KP_Begin and F13-F20. [`super::win32_key_record`] asks [`win32_named_record`]
//! after its own three families.
//!
//! Every record was fed raw into a ConPTY tab on 2026-10-17 (conhost 10.0.26200) and read by
//! `[Console]::ReadKey`, `RawUI.ReadKey`, PSReadLine 2.4.5 (pwsh 7.6.7) and 2.0.0 (Windows
//! PowerShell 5.1), and Git Bash's `dd` behind `stty raw`, the VT reader conhost translates the
//! record back for. A family is routed only where the VT reader kept its legacy bytes (or gained
//! the ones legacy lost) and PSReadLine did what the legacy bytes did. Left on their legacy
//! bytes, by that measurement:
//!
//! - Plain Pause: its record is conhost's pause key, which froze output and ate the next key.
//! - F13-F24: the console readers got them, Git Bash got nothing where legacy gives `CSI 25~`.
//! - KP_Begin (NumLock-off keypad 5): Git Bash got `CSI G` where legacy gives xterm's `CSI E`.
//! - Alt+keypad, which is Windows' Alt-code entry.
//! - Ctrl+Shift+Backspace: PSReadLine inserted the record's DEL; legacy BS kills a word.
//! - Ctrl+Shift+Tab and Alt+Shift+Tab: Git Bash got TAB and `ESC TAB` where legacy gives
//!   `CSI Z`.
//!
//! Under OpenConsole ([`KeyboardMode::OPENCONSOLE_HOST`], re-measured 2026-10-18 with
//! 1.25.2609.30003 by the same readers) two more families go out as records:
//!
//! - KP_Begin: its legacy `CSI E` reached `ReadKey` as ESC, `[`, `E`, so PSReadLine reverted the
//!   line and typed `[E` (Shift, Ctrl and Alt alike). Its `VK_CLEAR` record reads as `Clear`,
//!   which PSReadLine ignores, and Git Bash gets xterm's `CSI E`, `CSI 1;2E`, `CSI 1;3E`, and
//!   `SS3 E` under DECCKM (OpenConsole reads the record as a cursor key, so DECKPAM alone
//!   leaves it `CSI E`).
//! - F13-F20: the records read as `F13`..`F20` with their modifiers, and Git Bash gets `CSI 25~`
//!   .. `CSI 34~`, `CSI 25;5~` with Ctrl. F21-F24 stay legacy: their records gave Git Bash
//!   nothing.
//!
//! Keypad application mode (DECKPAM) changes nothing on either host: the keypad stays records,
//! so a program that turns it on gets the keypad's digits, operators and Enter, as under Windows
//! Terminal, which ships its keypad mode switched off after it broke vim, nano and zsh
//! (microsoft/terminal#16654), and as from xterm while NumLock is on (its `numLock` resource).
//! Measured 2026-10-18 on both hosts:
//!
//! - No record reaches a VT reader as `SS3`: neither host writes keypad mode into the record it
//!   translates. With and without `NUMLOCK_ON` or `ENHANCED_KEY`, scan 0 or `VK_PACKET`, Git
//!   Bash got the record's char (`5`, `*`, CR), and nothing for a char-0 record.
//! - The `SS3` bytes reach only a VT reader: OpenConsole forwards a `ReadKey` program's `ESC =`
//!   too (conhost only a VT-input program's) and then drops the bytes for it, so that reader got
//!   no keypad digit, operator or Enter. The records arrive as `NumPad5`, `Multiply`, `Enter`.
//! - The `SS3` bytes were also wrong for the VT programs that turn the mode on: Git's vim typed
//!   nothing for keypad 3 and 9 (`SS3 s` and `SS3 y`, its PageDown and PageUp) and nano typed no
//!   keypad key at all; with the records both type every digit and operator.
//!
//! Alt+keypad keeps its bytes under DECKPAM as outside it.

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

/// The record for a named key [`super::win32_key_record`]'s own families leave alone, or `None`
/// for a key that keeps its legacy bytes (see the module note for the measured exclusions).
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
/// the legacy `ESC ESC [ E` typed `[E`). No `NUMLOCK_ON`: the key is only KP_Begin with the
/// lock off.
#[cfg_attr(trust_verify, trust::skip)]
fn win32_keypad_begin(modifiers: Modifiers) -> Win32KeyRecord {
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
/// the module note). Shift and Ctrl chords are records already ([`super::win32_enter_chord`]);
/// Alt keeps the main Enter's `ESC CR`, which legacy sends whatever the keypad mode; outside
/// DECKPAM the legacy CR is Enter to every reader.
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
/// every chord bit. The legacy `CSI 25~`.. reached no console-API reader on either host, and
/// OpenConsole hands a VT reader the same bytes from the record, with the xterm modifier
/// parameter. conhost gave a VT reader nothing for the record; F21-F24's records gave nothing
/// under OpenConsole too, so they keep their bytes.
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
fn chord_state(modifiers: Modifiers) -> u32 {
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
/// -t` never printed its statistics and a batch file or msbuild could not be broken into. The
/// `VK_CANCEL` record makes conhost raise `CTRL_BREAK_EVENT` (measured: `ping -t` printed its
/// statistics and kept going). Char 0: a char of 3 would also read as Ctrl+C.
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
/// `ESC NUL` lost its NUL on the way to a VT reader (Git Bash got `ESC`). The records arrive as
/// `Spacebar MODS=Shift`, `Spacebar MODS=Control`, and `ESC NUL` for Git Bash; PSReadLine still
/// runs MenuComplete on Ctrl+Space, and Git Bash still gets nothing for Ctrl+Space, as before.
/// The char is Shift+Space's space, and 0 under Ctrl: a Windows keyboard gives Ctrl+Space a
/// space too, but a reader that types a record's char (libuv's) would then type one where the
/// legacy NUL's record typed nothing; PSReadLine menu-completed on either char. Alt+Space and
/// Alt+Shift+Space keep `ESC SP`, and Ctrl+Shift+Space keeps its NUL: PSReadLine binds no
/// Ctrl+Shift+Space, so its record completed nothing where the NUL, read as Ctrl+@, runs
/// MenuComplete (and Git Bash gets nothing either way).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_space_chord(modifiers: Modifiers) -> Option<Win32KeyRecord> {
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
/// BS reads as Ctrl+Backspace, a word delete. The record reads as `Backspace MODS=Shift`;
/// PSReadLine deletes one character on it, as on the legacy byte, and Git Bash still gets DEL.
/// Ctrl+Shift+Backspace stays legacy: PSReadLine inserted its record's DEL as text.
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
/// with its keypad scan code and the glyph it types. Legacy writes the main row's bytes, which
/// conhost reads as the main-row keys (KP_* as `D8 MODS=Shift`, Ctrl+KP_+ as `OemPlus
/// MODS=Shift` with the Ctrl lost), so no console reader could tell the keypad apart. The records
/// read as `NumPad5`, `Multiply`, `NumPad5 MODS=Control`, …, and Git Bash still gets the digit
/// or operator. The char stays the glyph under Ctrl too, where a Windows keyboard gives 0:
/// conhost turns a char-0 record into a control byte for a VT reader (Ctrl+KP_5 became ^U).
///
/// `NUMLOCK_ON` is set when the press says NumLock is on: always for a digit or the decimal
/// key, which exist only with NumLock on, and for an operator when the lock bit is set. `/` is
/// the E0-prefixed key, flagged `ENHANCED_KEY`. Not with Alt; DECKPAM or not, on either host
/// (see the module note).
#[cfg_attr(trust_verify, trust::skip)]
fn win32_keypad(named: NamedKey, modifiers: Modifiers) -> Option<Win32KeyRecord> {
    if modifiers.contains(Modifiers::ALT) {
        return None;
    }
    // (key, VK, scan, glyph): set-1 scan codes of the keypad.
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
