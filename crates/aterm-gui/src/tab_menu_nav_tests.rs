// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MenuNav, nav_for_key};
use aterm_types::keyboard::{Key, NamedKey as Nk};

#[test]
fn the_win32_menu_bindings_are_the_table() {
    for (k, want) in [
        (Key::Named(Nk::Enter), MenuNav::Activate),
        (Key::Named(Nk::NumpadEnter), MenuNav::Activate),
        (Key::Named(Nk::Space), MenuNav::Activate),
        (Key::Character(' '), MenuNav::Activate),
        (Key::Named(Nk::Escape), MenuNav::Dismiss),
        (Key::Named(Nk::ArrowDown), MenuNav::Down),
        (Key::Named(Nk::NumpadArrowDown), MenuNav::Down),
        (Key::Named(Nk::ArrowUp), MenuNav::Up),
        (Key::Named(Nk::Home), MenuNav::First),
        (Key::Named(Nk::End), MenuNav::Last),
        (Key::Character('a'), MenuNav::Dismiss),
        (Key::Named(Nk::Tab), MenuNav::Dismiss),
        (Key::Named(Nk::F5), MenuNav::Dismiss),
    ] {
        assert_eq!(nav_for_key(&k), want, "{k:?}");
    }
}

/// A bare modifier or lock press is inert: swallowed, card untouched, so the Shift
/// half of ⇧F10 does not dismiss the card.
#[test]
fn bare_modifiers_and_locks_are_inert_not_a_dismiss() {
    for k in [
        Nk::ShiftLeft,
        Nk::ShiftRight,
        Nk::ControlLeft,
        Nk::AltLeft,
        Nk::SuperLeft,
        Nk::MetaLeft,
        Nk::HyperLeft,
        Nk::CapsLock,
        Nk::NumLock,
        Nk::ScrollLock,
    ] {
        assert_eq!(
            nav_for_key(&Key::Named(k)),
            MenuNav::Inert,
            "{k:?} must not dismiss the card"
        );
    }
}
