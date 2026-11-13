// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::collections::HashMap;

use super::{option_only_ascii, LayoutKeys, UC_OPTION, UC_SHIFT};

/// A layout as a table of (key code, modifier byte) to character.
struct Table(HashMap<(u16, u32), char>);

impl LayoutKeys for Table {
    fn typed(&self, scancode: u16, mods: u32) -> Option<char> {
        self.0.get(&(scancode, mods)).copied()
    }
}

const L: u16 = 37;
const N: u16 = 45;
const SIX: u16 = 22;
const GRAVE: u16 = 50;
const CIRCUMFLEX: u16 = 10;

/// The keys these rows press, as the U.S. layout has them (`UCKeyTranslate`, keyboard
/// type 91): no Option character is an ASCII one only Option reaches.
fn us() -> Table {
    Table(HashMap::from([
        ((L, 0), 'l'),
        ((L, UC_SHIFT), 'L'),
        ((L, UC_OPTION), '¬'),
        ((N, 0), 'n'),
        ((N, UC_OPTION), '˜'),
        ((SIX, 0), '6'),
        ((SIX, UC_SHIFT), '^'),
        ((SIX, UC_OPTION), '§'),
        ((GRAVE, 0), '`'),
        ((GRAVE, UC_SHIFT), '~'),
        ((GRAVE, UC_OPTION), '`'),
    ]))
}

/// The same keys as the German layout has them.
fn german() -> Table {
    Table(HashMap::from([
        ((L, 0), 'l'),
        ((L, UC_OPTION), '@'),
        ((N, 0), 'n'),
        ((N, UC_OPTION), '~'),
        ((SIX, 0), '6'),
        ((SIX, UC_SHIFT), '&'),
        ((SIX, UC_OPTION), ']'),
        ((SIX, UC_OPTION | UC_SHIFT), '^'),
        ((CIRCUMFLEX, 0), '^'),
        ((GRAVE, 0), '<'),
        ((GRAVE, UC_SHIFT), '>'),
        ((GRAVE, UC_OPTION), '≤'),
    ]))
}

#[test]
fn on_the_us_layout_no_option_chord_is_the_only_way_to_a_character() {
    let us = us();
    for (code, shift) in [(L, false), (N, false), (SIX, false), (GRAVE, false), (GRAVE, true)] {
        assert_eq!(option_only_ascii(&us, code, shift), None, "key {code} shift {shift}");
    }
}

#[test]
fn a_german_ascii_character_only_option_reaches_is_that_chords() {
    let de = german();
    assert_eq!(option_only_ascii(&de, L, false), Some('@'));
    assert_eq!(option_only_ascii(&de, SIX, false), Some(']'));
    assert_eq!(option_only_ascii(&de, N, false), Some('~'), "a dead key, by its accent");
}

#[test]
fn a_character_reached_without_option_or_not_ascii_is_none() {
    let de = german();
    assert_eq!(option_only_ascii(&de, SIX, true), None, "^ is the ^ key's, bare");
    assert_eq!(option_only_ascii(&de, GRAVE, false), None, "≤ is not ASCII");
    assert_eq!(option_only_ascii(&de, CIRCUMFLEX, false), None, "no Option character");
}
