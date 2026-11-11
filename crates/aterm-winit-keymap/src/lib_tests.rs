// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_window::keyboard::SmolStr;

#[test]
fn named_key_table_maps_every_category_and_rejects_unmapped() {
    for (winit, want) in [
        (WinitNamed::ArrowUp, NamedKey::ArrowUp),
        (WinitNamed::PageDown, NamedKey::PageDown),
        (WinitNamed::Backspace, NamedKey::Backspace),
        (WinitNamed::Escape, NamedKey::Escape),
        (WinitNamed::Space, NamedKey::Space),
        (WinitNamed::CapsLock, NamedKey::CapsLock),
        (WinitNamed::ContextMenu, NamedKey::ContextMenu),
        (WinitNamed::F1, NamedKey::F1),
        (WinitNamed::F12, NamedKey::F12),
        (WinitNamed::F13, NamedKey::F13),
        (WinitNamed::F24, NamedKey::F24),
        (WinitNamed::F35, NamedKey::F35),
        (WinitNamed::MediaPlayPause, NamedKey::MediaPlayPause),
        (WinitNamed::AudioVolumeMute, NamedKey::AudioVolumeMute),
        (WinitNamed::Shift, NamedKey::ShiftLeft),
        (WinitNamed::Control, NamedKey::ControlLeft),
        (WinitNamed::Alt, NamedKey::AltLeft),
        (WinitNamed::Super, NamedKey::SuperLeft),
        (WinitNamed::Hyper, NamedKey::HyperLeft),
        (WinitNamed::Meta, NamedKey::MetaLeft),
    ] {
        assert_eq!(map_named_key(winit), Some(want), "{winit:?}");
    }
    assert_eq!(map_named_key(WinitNamed::Fn), None);
    assert_eq!(map_named_key(WinitNamed::Copy), None);
}

fn ch(c: &str) -> WinitKey {
    WinitKey::Character(SmolStr::new(c))
}

fn unidentified() -> WinitKey {
    WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified)
}

#[test]
fn numpad_location_promotes_digit_to_numpad_key() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Numpad5),
            &ch("5"),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::Numpad5))
    );
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::NumpadAdd),
            &ch("+"),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadAdd))
    );
}

#[test]
fn numpad_enter_is_not_main_enter() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::NumpadEnter),
            &WinitKey::Named(WinitNamed::Enter),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadEnter))
    );
}

#[test]
fn numlock_off_navigation_gets_numpad_twin() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Numpad4),
            &WinitKey::Named(WinitNamed::ArrowLeft),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadArrowLeft))
    );
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Numpad1),
            &WinitKey::Named(WinitNamed::End),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadEnd))
    );
    for centre in [
        unidentified(),
        WinitKey::Dead(None),
        WinitKey::Named(WinitNamed::Clear),
    ] {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &centre,
                KeyLocation::Numpad
            ),
            Some(Key::Named(NamedKey::NumpadBegin)),
            "{centre:?} on the physical keypad 5 is KP_Begin"
        );
    }
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Numpad4),
            &unidentified(),
            KeyLocation::Numpad
        ),
        None,
        "an unnamed key elsewhere on the keypad stays unmapped"
    );
}

#[test]
fn keypad_identity_is_the_glyph_not_the_scancode() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::NumpadDecimal),
            &ch("."),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadDecimal))
    );
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::NumpadDecimal),
            &ch(","),
            KeyLocation::Numpad
        ),
        Some(Key::Named(NamedKey::NumpadSeparator))
    );
}

#[test]
fn keypad_key_without_keypad_identity_takes_the_logical_route() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Unidentified(aterm_window::keyboard::NativeKeyCode::Unidentified),
            &WinitKey::Named(WinitNamed::Tab),
            KeyLocation::Numpad
        ),
        None
    );
    for non_begin in [ch(""), ch("55"), ch("a"), WinitKey::Named(WinitNamed::Tab)] {
        assert_eq!(
            map_numpad_key(
                PhysicalKey::Code(KeyCode::Numpad5),
                &non_begin,
                KeyLocation::Numpad
            ),
            None,
            "{non_begin:?} on physical Numpad5 must not become NumpadBegin"
        );
    }
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Numpad1),
            &ch("a"),
            KeyLocation::Numpad
        ),
        None
    );
}

#[test]
fn standard_location_is_untouched() {
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Digit5),
            &ch("5"),
            KeyLocation::Standard
        ),
        None
    );
    assert_eq!(
        map_numpad_key(
            PhysicalKey::Code(KeyCode::Enter),
            &WinitKey::Named(WinitNamed::Enter),
            KeyLocation::Standard
        ),
        None
    );
    for loc in [KeyLocation::Standard, KeyLocation::Left, KeyLocation::Right] {
        assert_eq!(
            map_numpad_key(PhysicalKey::Code(KeyCode::Numpad5), &ch("5"), loc),
            None
        );
        assert_eq!(
            map_numpad_key(PhysicalKey::Code(KeyCode::Numpad5), &unidentified(), loc),
            None
        );
    }
}

#[test]
fn character_logical_key_maps_through() {
    assert_eq!(map_logical_key(&ch("a")), Some(Key::Character('a')));
    for rejected in [
        ch(""),
        ch("ab"),
        ch("e\u{0301}"),
        WinitKey::Dead(None),
        unidentified(),
    ] {
        assert_eq!(map_logical_key(&rejected), None, "{rejected:?}");
    }
}

#[test]
fn base_layout_key_is_us_qwerty() {
    assert_eq!(
        base_layout_key_for(PhysicalKey::Code(KeyCode::KeyA)),
        Some('a')
    );
    assert_eq!(
        base_layout_key_for(PhysicalKey::Code(KeyCode::Digit1)),
        Some('1')
    );
    assert_eq!(
        base_layout_key_for(PhysicalKey::Code(KeyCode::Slash)),
        Some('/')
    );
    assert_eq!(base_layout_key_for(PhysicalKey::Code(KeyCode::F1)), None);
}

#[test]
fn layout_letter_twin_names_the_us_letter_under_a_non_latin_letter() {
    let ch = |s: &str| WinitKey::Character(SmolStr::new(s));
    let key = |code| PhysicalKey::Code(code);
    assert_eq!(
        layout_letter_twin_for(&ch("с"), key(KeyCode::KeyC)),
        Some('c')
    );
    assert_eq!(
        layout_letter_twin_for(&ch("ψ"), key(KeyCode::KeyC)),
        Some('c')
    );
    assert_eq!(
        layout_letter_twin_for(&ch("ב"), key(KeyCode::KeyC)),
        Some('c')
    );
    assert_eq!(layout_letter_twin_for(&ch("a"), key(KeyCode::KeyQ)), None);
    assert_eq!(layout_letter_twin_for(&ch("é"), key(KeyCode::Digit2)), None);
    assert_eq!(layout_letter_twin_for(&ch("б"), key(KeyCode::Comma)), None);
    assert_eq!(layout_letter_twin_for(&ch("№"), key(KeyCode::Digit3)), None);
    assert_eq!(layout_letter_twin_for(&ch("сб"), key(KeyCode::KeyC)), None);
    assert_eq!(layout_letter_twin_for(&ch(""), key(KeyCode::KeyC)), None);
    assert_eq!(
        layout_letter_twin_for(&WinitKey::Named(WinitNamed::Enter), key(KeyCode::Enter)),
        None
    );
    assert_eq!(
        layout_letter_twin_for(
            &ch("с"),
            PhysicalKey::Unidentified(aterm_window::keyboard::NativeKeyCode::Unidentified)
        ),
        None
    );
}

#[test]
fn a_right_side_modifier_press_maps_to_its_right_variant() {
    for (winit, left, right) in [
        (WinitNamed::Shift, NamedKey::ShiftLeft, NamedKey::ShiftRight),
        (
            WinitNamed::Control,
            NamedKey::ControlLeft,
            NamedKey::ControlRight,
        ),
        (WinitNamed::Alt, NamedKey::AltLeft, NamedKey::AltRight),
        (WinitNamed::Super, NamedKey::SuperLeft, NamedKey::SuperRight),
        (WinitNamed::Hyper, NamedKey::HyperLeft, NamedKey::HyperRight),
        (WinitNamed::Meta, NamedKey::MetaLeft, NamedKey::MetaRight),
    ] {
        let key = map_logical_key(&WinitKey::Named(winit)).expect("a modifier maps");
        assert_eq!(key, Key::Named(left));
        assert_eq!(
            sided_modifier(key.clone(), KeyLocation::Right),
            Key::Named(right)
        );
        for loc in [KeyLocation::Left, KeyLocation::Standard] {
            assert_eq!(sided_modifier(key.clone(), loc), Key::Named(left));
        }
    }
    let a = Key::Character('a');
    assert_eq!(sided_modifier(a.clone(), KeyLocation::Right), a);
}

#[test]
fn a_sided_modifier_has_its_other_side_as_twin() {
    for (l, r) in [
        (KeyCode::ShiftLeft, KeyCode::ShiftRight),
        (KeyCode::ControlLeft, KeyCode::ControlRight),
        (KeyCode::AltLeft, KeyCode::AltRight),
        (KeyCode::SuperLeft, KeyCode::SuperRight),
    ] {
        let (l, r) = (PhysicalKey::Code(l), PhysicalKey::Code(r));
        assert_eq!(physical_modifier_twin(l), Some(r));
        assert_eq!(physical_modifier_twin(r), Some(l));
    }
    for other in [KeyCode::KeyA, KeyCode::CapsLock, KeyCode::Meta, KeyCode::Fn] {
        assert_eq!(physical_modifier_twin(PhysicalKey::Code(other)), None);
    }
    assert_eq!(
        physical_modifier_twin(PhysicalKey::Unidentified(
            aterm_window::keyboard::NativeKeyCode::Unidentified
        )),
        None
    );
}
