// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Verify that a DOM `Ctrl` chord on a non-ASCII glyph without a control byte uses `base_layout_key`.
#[test]
fn a_dom_ctrl_chord_without_a_control_byte_takes_the_physical_keys() {
    let legacy = KeyboardMode::empty();
    let ctrl = Modifiers::CTRL.bits();
    let ctrl_alt = (Modifiers::CTRL | Modifiers::ALT).bits();
    let enc = |key, mods, base| encode_dom_key(key, mods, 0, base, legacy);
    for (key, mods, base, want, why) in [
        ("х", ctrl, Some('['), &b"\x1b"[..], "ru Ctrl+х, the [ key"),
        ("ü", ctrl, Some('['), b"\x1b", "de Ctrl+ü, the [ key"),
        ("с", ctrl, Some('c'), b"\x03", "ru Ctrl+с, the C key"),
        ("é", ctrl, Some('2'), b"\x00", "fr Ctrl+é, the 2 key"),
        ("€", ctrl, Some('e'), b"\x05", "Ctrl+€ on the E key"),
        ("€", ctrl_alt, Some('e'), b"\x1b\x05", "Ctrl+Alt is a chord"),
        ("@", ctrl_alt, Some('q'), b"\x1b\x00", "Ctrl+Alt is a chord"),
        (
            "€",
            0,
            Some('e'),
            "€".as_bytes(),
            "AltGr+E, Ctrl+Alt cleared",
        ),
        ("@", 0, Some('q'), b"@", "AltGr+Q, Ctrl+Alt cleared"),
        (
            "ö",
            ctrl,
            Some(';'),
            "ö".as_bytes(),
            "de Ctrl+ö: ; has no byte",
        ),
        ("€", ctrl, None, "€".as_bytes(), "no physical key"),
    ] {
        assert_eq!(enc(key, mods, base).as_deref(), Some(want), "{why}");
    }
}

#[test]
fn map_dom_key_reads_the_dom_key_names() {
    let named = |k| Some(Key::Named(k));
    for (dom, want) in [
        ("a", Some(Key::Character('a'))),
        ("A", Some(Key::Character('A'))),
        ("@", Some(Key::Character('@'))),
        ("é", Some(Key::Character('é'))),
        (" ", named(NamedKey::Space)),
        ("ArrowUp", named(NamedKey::ArrowUp)),
        ("Enter", named(NamedKey::Enter)),
        ("Tab", named(NamedKey::Tab)),
        ("Backspace", named(NamedKey::Backspace)),
        ("Delete", named(NamedKey::Delete)),
        ("Home", named(NamedKey::Home)),
        ("End", named(NamedKey::End)),
        ("PageUp", named(NamedKey::PageUp)),
        ("PageDown", named(NamedKey::PageDown)),
        ("Insert", named(NamedKey::Insert)),
        ("Escape", named(NamedKey::Escape)),
        ("ContextMenu", named(NamedKey::ContextMenu)),
        ("Pause", named(NamedKey::Pause)),
        ("PrintScreen", named(NamedKey::PrintScreen)),
        ("ScrollLock", named(NamedKey::ScrollLock)),
        ("CapsLock", named(NamedKey::CapsLock)),
        ("NumLock", named(NamedKey::NumLock)),
        ("F1", named(NamedKey::F1)),
        ("F12", named(NamedKey::F12)),
        ("F24", named(NamedKey::F24)),
        ("F35", named(NamedKey::F35)),
        ("F36", None),
        ("F0", None),
        ("MediaPlayPause", named(NamedKey::MediaPlayPause)),
        ("AudioVolumeMute", named(NamedKey::AudioVolumeMute)),
        ("Shift", named(NamedKey::ShiftLeft)),
        ("Control", named(NamedKey::ControlLeft)),
        ("Alt", named(NamedKey::AltLeft)),
        ("Meta", named(NamedKey::SuperLeft)),
        ("AltGraph", None),
        ("Fn", None),
        ("Dead", None),
        ("Process", None),
        ("Compose", None),
        ("Unidentified", None),
        ("ab", None),
        ("こんにちは", None),
        ("", None),
    ] {
        assert_eq!(map_dom_key(dom), want, "{dom:?}");
    }
}

#[test]
fn modifier_keys_encode_only_under_report_all() {
    for mode in [
        KeyboardMode::empty(),
        KeyboardMode::DISAMBIGUATE_ESC_CODES,
        KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES,
    ] {
        assert!(encode_dom_key("Shift", 1, 0, None, mode).is_none());
    }
    assert_eq!(
        encode_dom_key("Shift", 1, 0, None, KeyboardMode::REPORT_ALL_KEYS_AS_ESC).as_deref(),
        Some(&b"\x1b[57441;2u"[..])
    );
}

#[test]
fn encode_dom_key_honours_the_explicit_mode() {
    assert_eq!(
        encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::empty()).as_deref(),
        Some(&b"\x1b[A"[..])
    );
    assert_eq!(
        encode_dom_key("ArrowUp", 0, 0, None, KeyboardMode::APP_CURSOR).as_deref(),
        Some(&b"\x1bOA"[..]),
        "APP_CURSOR mode bits must yield SS3"
    );
    assert_eq!(
        encode_dom_key("Enter", 1, 0, None, KeyboardMode::DISAMBIGUATE_ESC_CODES).as_deref(),
        Some(&b"\x1b[13;2u"[..])
    );
    assert!(encode_dom_key("ArrowUp", 0, 2, None, KeyboardMode::empty()).is_none());
    assert!(encode_dom_key("Shift", 0, 0, None, KeyboardMode::empty()).is_none());
    assert!(encode_dom_key("ArrowUp", 0, 3, None, KeyboardMode::empty()).is_none());
}

#[test]
fn a_shifted_dom_key_reports_its_unshifted_kitty_code() {
    let kitty = KeyboardMode::DISAMBIGUATE_ESC_CODES;
    let ctrl_shift = (Modifiers::CTRL | Modifiers::SHIFT).bits();
    assert_eq!(
        encode_dom_key("A", ctrl_shift, 0, Some('a'), kitty).as_deref(),
        Some(&b"\x1b[97;6u"[..])
    );
    assert_eq!(
        encode_dom_key("@", ctrl_shift, 0, Some('2'), kitty).as_deref(),
        Some(&b"\x1b[50;6u"[..])
    );
    let alternates = kitty | KeyboardMode::REPORT_ALTERNATE_KEYS;
    assert_eq!(
        encode_dom_key("A", ctrl_shift, 0, Some('a'), alternates).as_deref(),
        Some(&b"\x1b[97:65;6u"[..])
    );
}

#[test]
fn encode_dom_key_text_release_requires_report_all() {
    let events = KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_EVENT_TYPES;
    assert!(encode_dom_key("a", 0, 2, None, events).is_none());
    assert!(encode_dom_key("Enter", 0, 2, None, events).is_none());

    let report_all = events | KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
    assert_eq!(
        encode_dom_key("a", 0, 2, None, report_all).as_deref(),
        Some(&b"\x1b[97;1:3u"[..])
    );
}
