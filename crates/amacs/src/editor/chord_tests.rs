// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn from_key_spells_keys_as_the_gui_host_did() {
    let named = |key| Key::Named(key);
    let rows: &[(Key, Modifiers, Option<&str>)] = &[
        (Key::Character('x'), Modifiers::CTRL, Some("C-x")),
        (Key::Character('X'), Modifiers::CTRL, Some("C-x")),
        (Key::Character('x'), Modifiers::ALT, Some("M-x")),
        (Key::Character('x'), Modifiers::META, Some("M-x")),
        (
            Key::Character('a'),
            Modifiers::CTRL | Modifiers::SHIFT,
            Some("C-S-a"),
        ),
        (
            Key::Character('f'),
            Modifiers::CTRL | Modifiers::ALT,
            Some("C-M-f"),
        ),
        (Key::Character('a'), Modifiers::SUPER, Some("a")),
        (Key::Character('a'), Modifiers::HYPER, Some("a")),
        (Key::Character('/'), Modifiers::CTRL, Some("C-/")),
        (Key::Character('-'), Modifiers::empty(), Some("-")),
        (Key::Character('-'), Modifiers::CTRL, Some("C--")),
        (
            Key::Character('-'),
            Modifiers::CTRL | Modifiers::ALT,
            Some("C-M--"),
        ),
        (named(NamedKey::Space), Modifiers::CTRL, Some("C-space")),
        (
            named(NamedKey::Backspace),
            Modifiers::CTRL,
            Some("C-backspace"),
        ),
        (named(NamedKey::Delete), Modifiers::ALT, Some("M-delete")),
        (named(NamedKey::Enter), Modifiers::ALT, Some("M-enter")),
        (named(NamedKey::Escape), Modifiers::empty(), Some("escape")),
        (named(NamedKey::Tab), Modifiers::SHIFT, Some("S-tab")),
        (named(NamedKey::ArrowLeft), Modifiers::ALT, Some("M-left")),
        (
            named(NamedKey::ArrowRight),
            Modifiers::CTRL,
            Some("C-right"),
        ),
        (named(NamedKey::ArrowUp), Modifiers::CTRL, None),
        (named(NamedKey::ArrowDown), Modifiers::ALT, None),
        (named(NamedKey::Home), Modifiers::CTRL, None),
        (named(NamedKey::End), Modifiers::empty(), None),
        (named(NamedKey::PageDown), Modifiers::CTRL, None),
        (named(NamedKey::F5), Modifiers::CTRL, None),
        (named(NamedKey::F6), Modifiers::empty(), None),
        (named(NamedKey::Insert), Modifiers::empty(), None),
    ];
    for (key, mods, spelled) in rows {
        let chord = KeyChord::from_key(key, *mods);
        assert_eq!(
            chord.as_ref().map(KeyChord::display).as_deref(),
            *spelled,
            "{key:?} {mods:?}"
        );
        if let Some(chord) = chord {
            assert_eq!(KeyChord::parse(&chord.display()), Some(chord));
        }
    }
    assert!(KeyChord::parse("escape").unwrap().is_escape());
    assert!(!KeyChord::parse("M-escape").unwrap().is_escape());
    for bad in ["", "C-", "--", "C---", "C-C-x", "M-M-f", "S-S-a", "C-x-y"] {
        assert_eq!(KeyChord::parse(bad), None, "{bad:?}");
    }
}
