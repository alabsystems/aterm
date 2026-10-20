// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The named keys' win32-input-mode records, pinned byte for byte against the records measured
//! on 2026-10-17 (see `encode_win32_keys.rs`), and the keys left on their legacy bytes.

use super::super::{encode_key, encode_key_with_event};
use crate::keyboard::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey};

const WIN32: KeyboardMode = KeyboardMode::WIN32_INPUT;

/// The key-down/key-up pair `CSI vk;sc;uc;Kd;cs;1 _`, written out from the spec.
fn pair(vk: u32, sc: u32, uc: u32, cs: u32) -> Vec<u8> {
    format!("\x1b[{vk};{sc};{uc};1;{cs};1_\x1b[{vk};{sc};{uc};0;{cs};1_").into_bytes()
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

/// The audit's `ctrl-break-lost`: Ctrl+Pause wrote nothing. The VK_CANCEL record is the one that
/// made `ping -t` print its statistics (`ESC[3;70;0;1;264;1_…`).
#[test]
fn ctrl_break_is_the_vk_cancel_record() {
    let pause = named(NamedKey::Pause);
    assert_eq!(
        encode_key(&pause, Modifiers::CTRL, WIN32),
        b"\x1b[3;70;0;1;264;1_\x1b[3;70;0;0;264;1_"
    );
    assert_eq!(
        encode_key(&pause, Modifiers::CTRL | Modifiers::SHIFT, WIN32),
        pair(3, 70, 0, 0x118)
    );
    // Plain Pause is conhost's pause key: it stays the nothing it was.
    assert!(encode_key(&pause, Modifiers::empty(), WIN32).is_empty());
    // The pair carries its key-up; a release adds nothing.
    assert!(
        encode_key_with_event(&pause, Modifiers::CTRL, WIN32, KeyEventType::Release).is_empty()
    );
    // Off ConPTY nothing changed.
    assert!(encode_key(&pause, Modifiers::CTRL, KeyboardMode::empty()).is_empty());
}

/// The audit's `keypad-identity-lost`: KP_5 reached `ReadKey` as `D5`, KP_* as `D8 MODS=Shift`
/// and Ctrl+KP_+ as `OemPlus MODS=Shift`. The records read as `NumPad5`, `Multiply`, `Add
/// MODS=Control`; Git Bash still got `5`, `*` and `+`.
#[test]
fn keypad_keys_are_records_of_the_keypad() {
    let none = Modifiers::empty();
    for (key, vk, sc, glyph) in [
        (NamedKey::Numpad0, 96, 82, b'0'),
        (NamedKey::Numpad5, 101, 76, b'5'),
        (NamedKey::Numpad9, 105, 73, b'9'),
        (NamedKey::NumpadDecimal, 110, 83, b'.'),
    ] {
        assert_eq!(
            encode_key(&named(key), none, WIN32),
            pair(vk, sc, u32::from(glyph), 0x20),
            "{key:?}: NUMLOCK_ON, the key implies it"
        );
    }
    // The operators carry NUMLOCK_ON when the lock bit says so.
    assert_eq!(
        encode_key(&named(NamedKey::NumpadMultiply), Modifiers::NUM_LOCK, WIN32),
        pair(106, 55, 42, 0x20)
    );
    assert_eq!(
        encode_key(&named(NamedKey::NumpadAdd), none, WIN32),
        pair(107, 78, 43, 0)
    );
    assert_eq!(
        encode_key(&named(NamedKey::NumpadSubtract), none, WIN32),
        pair(109, 74, 45, 0)
    );
    // `/` is the E0 key: ENHANCED_KEY.
    assert_eq!(
        encode_key(&named(NamedKey::NumpadDivide), Modifiers::NUM_LOCK, WIN32),
        b"\x1b[111;53;47;1;288;1_\x1b[111;53;47;0;288;1_"
    );
    // Ctrl and Shift ride along, and the char stays the glyph: a char-0 Ctrl+KP_5 reached Git
    // Bash as ^U.
    assert_eq!(
        encode_key(&named(NamedKey::Numpad5), Modifiers::CTRL, WIN32),
        pair(101, 76, 53, 0x28)
    );
    assert_eq!(
        encode_key(&named(NamedKey::Numpad5), Modifiers::SHIFT, WIN32),
        pair(101, 76, 53, 0x30)
    );
    assert_eq!(
        encode_key(
            &named(NamedKey::NumpadAdd),
            Modifiers::CTRL | Modifiers::NUM_LOCK,
            WIN32
        ),
        pair(107, 78, 43, 0x28)
    );
}

/// The keypad keys a record would cost a VT reader under conhost: with Alt (Windows' Alt-code
/// entry), and KP_Begin (Git Bash got `CSI G` for xterm's `CSI E`).
#[test]
fn keypad_keys_a_record_would_change_for_a_vt_reader_keep_their_bytes() {
    assert_eq!(
        encode_key(&named(NamedKey::Numpad5), Modifiers::ALT, WIN32),
        b"\x1b5"
    );
    assert_eq!(
        encode_key(&named(NamedKey::NumpadBegin), Modifiers::empty(), WIN32),
        b"\x1b[E"
    );
    // F13-F24 too: the console readers got their records, Git Bash got nothing for them.
    assert_eq!(
        encode_key(&named(NamedKey::F13), Modifiers::empty(), WIN32),
        b"\x1b[25~"
    );
    // Off ConPTY the keypad is what it was.
    assert_eq!(
        encode_key(
            &named(NamedKey::Numpad5),
            Modifiers::empty(),
            KeyboardMode::empty()
        ),
        b"5"
    );
}

/// The session's host is OpenConsole, the default Windows host.
const OPENCONSOLE: KeyboardMode = WIN32.union(KeyboardMode::OPENCONSOLE_HOST);

/// The judge's `kp-begin-erases-line-openconsole`: under OpenConsole the legacy `CSI E` reached
/// `ReadKey` as ESC, `[`, `E`, so PSReadLine reverted the line and typed `[E` (Shift, Ctrl and
/// Alt alike). The `VK_CLEAR` record reads as `Clear`, which types nothing, and Git Bash gets
/// `CSI E`, `CSI 1;2E`, `CSI 1;5E`, `CSI 1;3E`, and `SS3 E` under DECCKM (measured 2026-10-18).
#[test]
fn under_openconsole_kp_begin_is_the_vk_clear_record() {
    let begin = named(NamedKey::NumpadBegin);
    for (mods, cs) in [
        (Modifiers::empty(), 0),
        (Modifiers::SHIFT, 0x10),
        (Modifiers::CTRL, 0x08),
        (Modifiers::ALT, 0x02),
        (Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT, 0x1a),
        // A lock is not a chord bit, and the key exists only with NumLock off.
        (Modifiers::CAPS_LOCK, 0),
    ] {
        assert_eq!(
            encode_key(&begin, mods, OPENCONSOLE),
            pair(12, 76, 0, cs),
            "{mods:?}+KP_Begin"
        );
        // The record names the key; DECKPAM does not change it (OpenConsole hands Git Bash the
        // same `CSI E`, and `SS3 E` only under DECCKM).
        assert_eq!(
            encode_key(&begin, mods, OPENCONSOLE | KeyboardMode::APP_KEYPAD),
            pair(12, 76, 0, cs),
            "{mods:?}+KP_Begin under DECKPAM"
        );
    }
    assert!(
        encode_key_with_event(
            &begin,
            Modifiers::empty(),
            OPENCONSOLE,
            KeyEventType::Release
        )
        .is_empty(),
        "the pair carries its key-up"
    );
    // conhost keeps xterm's bytes, which it reads as nothing and hands Git Bash intact.
    assert_eq!(encode_key(&begin, Modifiers::empty(), WIN32), b"\x1b[E");
    assert_eq!(encode_key(&begin, Modifiers::ALT, WIN32), b"\x1b\x1b[E");
    // The host bit means nothing without win32-input-mode.
    assert_eq!(
        encode_key(&begin, Modifiers::empty(), KeyboardMode::OPENCONSOLE_HOST),
        b"\x1b[E"
    );
}

/// The judge's `f13-f20-records-fixable-under-openconsole`: the legacy `CSI 25~`.. reached no
/// `ReadKey` reader on either host. Under OpenConsole the records read as `F13`..`F20` with
/// their modifiers and Git Bash gets `CSI 25~`..`CSI 34~` (`CSI 25;5~` with Ctrl). F21-F24's
/// records gave Git Bash nothing there, and conhost gave it nothing for any of them.
#[test]
fn under_openconsole_f13_to_f20_are_records() {
    for (key, vk, sc) in [
        (NamedKey::F13, 0x7c, 0x64),
        (NamedKey::F14, 0x7d, 0x65),
        (NamedKey::F15, 0x7e, 0x66),
        (NamedKey::F16, 0x7f, 0x67),
        (NamedKey::F17, 0x80, 0x68),
        (NamedKey::F18, 0x81, 0x69),
        (NamedKey::F19, 0x82, 0x6a),
        (NamedKey::F20, 0x83, 0x6b),
    ] {
        assert_eq!(
            encode_key(&named(key), Modifiers::empty(), OPENCONSOLE),
            pair(vk, sc, 0, 0),
            "{key:?}"
        );
        assert_eq!(
            encode_key(&named(key), Modifiers::empty(), WIN32),
            encode_key(&named(key), Modifiers::empty(), KeyboardMode::empty()),
            "{key:?} under conhost keeps its bytes"
        );
    }
    assert_eq!(
        encode_key(&named(NamedKey::F13), Modifiers::SHIFT, OPENCONSOLE),
        b"\x1b[124;100;0;1;16;1_\x1b[124;100;0;0;16;1_"
    );
    assert_eq!(
        encode_key(
            &named(NamedKey::F13),
            Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT,
            OPENCONSOLE
        ),
        pair(0x7c, 0x64, 0, 0x1a)
    );
    for (key, legacy) in [
        (NamedKey::F21, &b"\x1b[35~"[..]),
        (NamedKey::F24, &b"\x1b[38~"[..]),
        (NamedKey::F12, &b"\x1b[24~"[..]),
    ] {
        assert_eq!(
            encode_key(&named(key), Modifiers::empty(), OPENCONSOLE),
            legacy,
            "{key:?} keeps its bytes"
        );
    }
}

/// The judges' `keypad-dead-under-deckpam-openconsole` and `rvfy-gitbash-deckpam-keypad-plain`:
/// on both hosts the keypad stays records under DECKPAM, its Enter included, so a `ReadKey`
/// reader under OpenConsole gets `NumPad5`, `Multiply` and `Enter` (the `SS3` bytes reached it
/// as nothing) and a VT reader on either host gets `5`, `*` and CR, as under Windows Terminal.
/// No record reaches a VT reader as `SS3` on either host, and the `SS3` bytes made Git's vim
/// page on keypad 3 and 9 and nano type no keypad key under conhost (measured 2026-10-18). Alt
/// keeps its bytes (Alt-code entry, and `ESC CR` for Alt+KP_Enter).
#[test]
fn the_keypad_stays_records_under_deckpam_on_both_hosts() {
    let none = Modifiers::empty();
    for host in [WIN32, OPENCONSOLE] {
        let app = host | KeyboardMode::APP_KEYPAD;
        assert_eq!(
            encode_key(&named(NamedKey::Numpad5), none, app),
            pair(101, 76, 53, 0x20),
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::Numpad3), none, app),
            pair(99, 81, 51, 0x20),
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::Numpad0), none, app),
            pair(96, 82, 48, 0x20),
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::NumpadMultiply), none, app),
            pair(106, 55, 42, 0),
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::NumpadDecimal), none, app),
            pair(110, 83, 46, 0x20),
            "{host:?}"
        );
        // KP_Enter: the ENHANCED_KEY VK_RETURN record with CR.
        assert_eq!(
            encode_key(&named(NamedKey::NumpadEnter), none, app),
            b"\x1b[13;28;13;1;256;1_\x1b[13;28;13;0;256;1_",
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::Numpad5), Modifiers::ALT, app),
            b"\x1b5",
            "{host:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::NumpadEnter), Modifiers::ALT, app),
            b"\x1b\r",
            "{host:?}"
        );
        // Outside DECKPAM the keypad's Enter is the CR every reader takes as Enter.
        assert_eq!(
            encode_key(&named(NamedKey::NumpadEnter), none, host),
            b"\r",
            "{host:?}"
        );
    }
    // Without a console host DECKPAM keeps xterm's `SS3` forms.
    assert_eq!(
        encode_key(&named(NamedKey::Numpad5), none, KeyboardMode::APP_KEYPAD),
        b"\x1bOu"
    );
}

/// Under OpenConsole exactly KP_Begin and F13-F20 change: every other named key, under every
/// Shift/Ctrl/Alt combination, in and out of DECKPAM and DECCKM, encodes as it does under
/// conhost.
#[test]
fn openconsole_changes_only_kp_begin_and_f13_to_f20() {
    let mods_list = [
        Modifiers::empty(),
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::SHIFT | Modifiers::ALT,
        Modifiers::CTRL | Modifiers::ALT,
        Modifiers::CTRL | Modifiers::SHIFT,
        Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
    ];
    let keys = [
        NamedKey::Escape,
        NamedKey::Enter,
        NamedKey::Tab,
        NamedKey::Backspace,
        NamedKey::Space,
        NamedKey::Pause,
        NamedKey::ArrowUp,
        NamedKey::Home,
        NamedKey::PageDown,
        NamedKey::Insert,
        NamedKey::Delete,
        NamedKey::F1,
        NamedKey::F5,
        NamedKey::F12,
        NamedKey::F13,
        NamedKey::F16,
        NamedKey::F20,
        NamedKey::F21,
        NamedKey::F24,
        NamedKey::Numpad0,
        NamedKey::Numpad5,
        NamedKey::NumpadDecimal,
        NamedKey::NumpadAdd,
        NamedKey::NumpadDivide,
        NamedKey::NumpadEnter,
        NamedKey::NumpadBegin,
        NamedKey::NumpadHome,
        NamedKey::NumpadArrowLeft,
    ];
    let mut changed = 0;
    for extra in [
        KeyboardMode::empty(),
        KeyboardMode::APP_KEYPAD,
        KeyboardMode::APP_CURSOR,
    ] {
        for key in keys {
            for mods in mods_list {
                let conhost = encode_key(&named(key), mods, WIN32 | extra);
                let openconsole = encode_key(&named(key), mods, OPENCONSOLE | extra);
                // Named here from the keys, not from the encoder's predicates.
                let moved = matches!(
                    key,
                    NamedKey::NumpadBegin | NamedKey::F13 | NamedKey::F16 | NamedKey::F20
                );
                if !moved {
                    assert_eq!(openconsole, conhost, "{key:?}+{mods:?} under {extra:?}");
                    continue;
                }
                changed += 1;
                assert_ne!(openconsole, conhost, "{key:?}+{mods:?} under {extra:?}");
                assert!(
                    openconsole.starts_with(b"\x1b[") && openconsole.ends_with(b";1_"),
                    "{key:?}+{mods:?} under {extra:?}: {openconsole:?} is a record pair"
                );
            }
        }
    }
    // KP_Begin and three F-keys in every mode and chord (4 * 8 * 3).
    assert_eq!(changed, 96);
}

/// The audit's `ctrl-space-identity` and half of `lost-modifiers-legacy`: Shift+Space reached
/// `ReadKey` as a bare Spacebar, Ctrl+Space's NUL as `D2 MODS=Shift+Control`, and Ctrl+Alt+Space
/// reached Git Bash as `ESC` with its NUL lost. The records read as `Spacebar MODS=Shift` and
/// `Spacebar MODS=Control` (PSReadLine still menu-completes), and Git Bash got `ESC NUL`.
#[test]
fn space_chords_are_records_of_the_space_key() {
    let space = named(NamedKey::Space);
    assert_eq!(
        encode_key(&space, Modifiers::SHIFT, WIN32),
        b"\x1b[32;57;32;1;16;1_\x1b[32;57;32;0;16;1_"
    );
    assert_eq!(
        encode_key(&space, Modifiers::CTRL, WIN32),
        pair(32, 57, 0, 0x08)
    );
    assert_eq!(
        encode_key(
            &space,
            Modifiers::CTRL | Modifiers::SHIFT | Modifiers::ALT,
            WIN32
        ),
        pair(32, 57, 0, 0x1a)
    );
    // Ctrl+Shift+Space keeps its NUL: read as Ctrl+@ it menu-completes in PSReadLine, which
    // binds nothing to the record's Ctrl+Shift+Space.
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::SHIFT, WIN32),
        b"\x00"
    );
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::ALT, WIN32),
        b"\x1b[32;57;0;1;10;1_\x1b[32;57;0;0;10;1_"
    );
    // Lock state is not a chord modifier.
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::CAPS_LOCK, WIN32),
        pair(32, 57, 0, 0x08)
    );
    // What conhost already reads keeps its bytes: Space, Alt+Space, Alt+Shift+Space.
    for mods in [
        Modifiers::empty(),
        Modifiers::ALT,
        Modifiers::ALT | Modifiers::SHIFT,
    ] {
        assert_eq!(
            encode_key(&space, mods, WIN32),
            encode_key(&space, mods, KeyboardMode::empty()),
            "{mods:?}+Space"
        );
    }
    // Off ConPTY: Ctrl+Space is NUL, Ctrl+Alt+Space is ESC NUL.
    assert_eq!(
        encode_key(&space, Modifiers::CTRL, KeyboardMode::empty()),
        b"\x00"
    );
    assert_eq!(
        encode_key(
            &space,
            Modifiers::CTRL | Modifiers::ALT,
            KeyboardMode::empty()
        ),
        b"\x1b\x00"
    );
}

/// Shift+Backspace reached `ReadKey` as a bare Backspace; its record reads as `Backspace
/// MODS=Shift`, and PSReadLine and Git Bash do what they did. Ctrl+Shift+Backspace stays BS:
/// PSReadLine inserted the record's DEL, where the BS kills a word.
#[test]
fn shift_backspace_is_a_record_and_ctrl_shift_backspace_is_not() {
    let bksp = named(NamedKey::Backspace);
    assert_eq!(
        encode_key(&bksp, Modifiers::SHIFT, WIN32),
        b"\x1b[8;14;8;1;16;1_\x1b[8;14;8;0;16;1_"
    );
    assert_eq!(
        encode_key(
            &bksp,
            Modifiers::SHIFT,
            WIN32 | KeyboardMode::BACKARROW_SENDS_BS
        ),
        pair(8, 14, 8, 0x10),
        "under DECBKM the legacy BS read as Ctrl+Backspace"
    );
    assert_eq!(
        encode_key(&bksp, Modifiers::CTRL | Modifiers::SHIFT, WIN32),
        b"\x08"
    );
    assert_eq!(encode_key(&bksp, Modifiers::empty(), WIN32), b"\x7f");
    assert_eq!(
        encode_key(&bksp, Modifiers::SHIFT, KeyboardMode::empty()),
        b"\x7f"
    );
}

/// Every record here outranks a negotiated kitty mode and modifyOtherKeys, as the other records
/// do: conhost is the reader and drops both protocols' spellings.
#[test]
fn named_records_outrank_kitty_and_modify_other_keys() {
    for mode in [
        KeyboardMode::DISAMBIGUATE_ESC_CODES,
        KeyboardMode::REPORT_ALL_KEYS_AS_ESC,
        KeyboardMode::XTERM_MODIFY_OTHER_KEYS_LEVEL2,
    ] {
        assert_eq!(
            encode_key(&named(NamedKey::Space), Modifiers::CTRL, WIN32 | mode),
            pair(32, 57, 0, 0x08),
            "{mode:?}"
        );
        assert_eq!(
            encode_key(&named(NamedKey::Numpad5), Modifiers::empty(), WIN32 | mode),
            pair(101, 76, 53, 0x20),
            "{mode:?}"
        );
    }
}
