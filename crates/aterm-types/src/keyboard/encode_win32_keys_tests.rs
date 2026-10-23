// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for win32-input-mode named-key encoding records.

use super::super::{encode_key, encode_key_with_event};
use crate::keyboard::{Key, KeyEventType, KeyboardMode, Modifiers, NamedKey};

const WIN32: KeyboardMode = KeyboardMode::WIN32_INPUT;

fn pair(vk: u32, sc: u32, uc: u32, cs: u32) -> Vec<u8> {
    format!("\x1b[{vk};{sc};{uc};1;{cs};1_\x1b[{vk};{sc};{uc};0;{cs};1_").into_bytes()
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

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
    assert!(encode_key(&pause, Modifiers::empty(), WIN32).is_empty());
    assert!(
        encode_key_with_event(&pause, Modifiers::CTRL, WIN32, KeyEventType::Release).is_empty()
    );
    assert!(encode_key(&pause, Modifiers::CTRL, KeyboardMode::empty()).is_empty());
}

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
    assert_eq!(
        encode_key(&named(NamedKey::NumpadDivide), Modifiers::NUM_LOCK, WIN32),
        b"\x1b[111;53;47;1;288;1_\x1b[111;53;47;0;288;1_"
    );
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
    assert_eq!(
        encode_key(&named(NamedKey::F13), Modifiers::empty(), WIN32),
        b"\x1b[25~"
    );
    assert_eq!(
        encode_key(
            &named(NamedKey::Numpad5),
            Modifiers::empty(),
            KeyboardMode::empty()
        ),
        b"5"
    );
}

const OPENCONSOLE: KeyboardMode = WIN32.union(KeyboardMode::OPENCONSOLE_HOST);

#[test]
fn under_openconsole_kp_begin_is_the_vk_clear_record() {
    let begin = named(NamedKey::NumpadBegin);
    for (mods, cs) in [
        (Modifiers::empty(), 0),
        (Modifiers::SHIFT, 0x10),
        (Modifiers::CTRL, 0x08),
        (Modifiers::ALT, 0x02),
        (Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT, 0x1a),
        (Modifiers::CAPS_LOCK, 0),
    ] {
        assert_eq!(
            encode_key(&begin, mods, OPENCONSOLE),
            pair(12, 76, 0, cs),
            "{mods:?}+KP_Begin"
        );
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
    assert_eq!(encode_key(&begin, Modifiers::empty(), WIN32), b"\x1b[E");
    assert_eq!(encode_key(&begin, Modifiers::ALT, WIN32), b"\x1b\x1b[E");
    assert_eq!(
        encode_key(&begin, Modifiers::empty(), KeyboardMode::OPENCONSOLE_HOST),
        b"\x1b[E"
    );
}

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
        assert_eq!(
            encode_key(&named(NamedKey::NumpadEnter), none, host),
            b"\r",
            "{host:?}"
        );
    }
    assert_eq!(
        encode_key(&named(NamedKey::Numpad5), none, KeyboardMode::APP_KEYPAD),
        b"\x1bOu"
    );
}

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
    assert_eq!(changed, 96);
}

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
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::SHIFT, WIN32),
        b"\x00"
    );
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::ALT, WIN32),
        b"\x1b[32;57;0;1;10;1_\x1b[32;57;0;0;10;1_"
    );
    assert_eq!(
        encode_key(&space, Modifiers::CTRL | Modifiers::CAPS_LOCK, WIN32),
        pair(32, 57, 0, 0x08)
    );
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
