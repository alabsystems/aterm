// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    OutputEchoInput, PetInputKind, PredictKey, classify_press, classify_sent, is_plain_enter,
};
use crate::input::InputEvent;
use crate::predict::EditKey;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn press(key: NamedKey, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(key),
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

#[test]
fn keypad_enter_is_a_plain_enter_and_a_turn_boundary() {
    let kp_enter = press(NamedKey::NumpadEnter, Modifiers::empty());
    assert!(is_plain_enter(&kp_enter));
    assert_eq!(
        OutputEchoInput::of(&kp_enter),
        OutputEchoInput::TurnBoundary
    );
    let class = classify_press(&kp_enter);
    assert!(class.enter_like && class.keyed_enter);
    assert!(matches!(class.console_input, Some(PetInputKind::Submit)));
    assert_eq!(class.typed_forward, Some(true));
    let shifted = press(NamedKey::NumpadEnter, Modifiers::SHIFT);
    assert!(!is_plain_enter(&shifted));
    assert!(classify_press(&shifted).enter_like);
}

#[test]
fn a_key_sequence_that_types_only_text_is_classified_as_that_text() {
    let seq = |bytes: &[u8]| InputEvent::KeySequence(bytes.to_vec());
    for text in ["#", "ab", "日", "e\u{301}"] {
        let ev = seq(text.as_bytes());
        let sent = classify_sent(&ev, true);
        let typed_ev = InputEvent::Text(text.to_owned());
        let typed = classify_press(&typed_ev);
        assert_eq!(sent.typed_forward, Some(true), "{text:?}");
        assert_eq!(sent.ime, Some(text), "{text:?}");
        assert_eq!(
            (
                sent.predict_candidate,
                sent.brk,
                sent.keyed_enter,
                sent.line_terminator,
                sent.typed
            ),
            (
                typed.predict_candidate,
                typed.brk,
                typed.keyed_enter,
                typed.line_terminator,
                typed.typed
            ),
            "{text:?}: exactly the Text of that string"
        );
        assert!(matches!(sent.console_input, Some(PetInputKind::Text)));
        let bytes = classify_sent(&ev, false);
        assert_eq!(
            (bytes.typed_forward, bytes.ime, bytes.brk),
            (None, None, true),
            "{text:?}: a controller's `send` stays bytes"
        );
    }
    for bytes in [
        &b"\x1b#"[..],
        b"#\r",
        b"\t",
        b"",
        b"\xff#",
        "\u{301}".as_bytes(),
        b"\x7f",
        b"\x03",
    ] {
        let ev = seq(bytes);
        let class = classify_sent(&ev, true);
        assert_eq!(
            (class.typed_forward, class.ime, class.brk),
            (None, None, true),
            "{bytes:?}: not text alone"
        );
    }
}

#[test]
fn keypad_digit_is_a_typed_glyph() {
    let five_ev = press(NamedKey::Numpad5, Modifiers::empty());
    let class = classify_press(&five_ev);
    assert_eq!(class.typed, Some('5'));
    assert_eq!(class.predict_candidate, Some(PredictKey::Glyph('5')));
    assert_eq!(class.typed_forward, Some(true));
    assert!(matches!(class.console_input, Some(PetInputKind::Text)));
    assert!(!class.brk);
    let decimal_ev = press(NamedKey::NumpadDecimal, Modifiers::empty());
    let decimal = classify_press(&decimal_ev);
    assert_eq!(decimal.typed, Some('.'));
    let shifted_ev = press(NamedKey::Numpad5, Modifiers::SHIFT);
    let shifted = classify_press(&shifted_ev);
    assert_eq!(shifted.typed, Some('5'));
    assert_eq!(shifted.predict_candidate, Some(PredictKey::Glyph('5')));
    let equal_ev = press(NamedKey::NumpadEqual, Modifiers::SHIFT);
    assert_eq!(classify_press(&equal_ev).typed, Some('='));
}

#[test]
fn only_bare_arrows_and_backspace_are_edit_keys() {
    let candidate = |key, mods| classify_press(&press(key, mods)).predict_candidate;
    for locks in [
        Modifiers::CAPS_LOCK,
        Modifiers::NUM_LOCK,
        Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
    ] {
        assert_eq!(
            candidate(NamedKey::ArrowRight, locks),
            Some(PredictKey::Edit(EditKey::Right)),
            "{locks:?}"
        );
        assert_eq!(
            candidate(NamedKey::ArrowLeft, locks | Modifiers::ALT),
            None,
            "{locks:?} with Alt is a word motion"
        );
    }
    for mods in [Modifiers::HYPER, Modifiers::META] {
        assert_eq!(candidate(NamedKey::ArrowLeft, mods), None, "{mods:?}");
    }
    assert_eq!(
        candidate(NamedKey::ArrowLeft, Modifiers::empty()),
        Some(PredictKey::Edit(EditKey::Left))
    );
    assert_eq!(
        candidate(NamedKey::ArrowRight, Modifiers::empty()),
        Some(PredictKey::Edit(EditKey::Right))
    );
    assert_eq!(
        candidate(NamedKey::Backspace, Modifiers::empty()),
        Some(PredictKey::Edit(EditKey::Backspace))
    );
    for mods in [
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::SUPER,
    ] {
        assert_eq!(candidate(NamedKey::ArrowLeft, mods), None, "{mods:?}");
    }
    for locks in [Modifiers::empty(), Modifiers::NUM_LOCK] {
        assert_eq!(
            candidate(NamedKey::NumpadArrowLeft, locks),
            Some(PredictKey::Edit(EditKey::Left)),
            "the keypad's ←, {locks:?}"
        );
        assert_eq!(
            candidate(NamedKey::NumpadArrowRight, locks),
            Some(PredictKey::Edit(EditKey::Right)),
            "the keypad's →, {locks:?}"
        );
    }
    assert_eq!(
        candidate(NamedKey::NumpadArrowLeft, Modifiers::SHIFT),
        None,
        "a chord on the keypad is a chord"
    );
    assert_eq!(candidate(NamedKey::ArrowUp, Modifiers::empty()), None);
}

#[test]
fn home_end_delete_and_ctrl_a_e_are_learned_edit_keys() {
    let candidate = |key, mods| classify_press(&press(key, mods)).predict_candidate;
    for (key, edit) in [
        (NamedKey::Home, EditKey::Home),
        (NamedKey::End, EditKey::End),
        (NamedKey::Delete, EditKey::Delete),
    ] {
        for locks in [
            Modifiers::empty(),
            Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
        ] {
            assert_eq!(
                candidate(key, locks),
                Some(PredictKey::Edit(edit)),
                "{key:?} {locks:?}"
            );
        }
        for mods in [
            Modifiers::SHIFT,
            Modifiers::ALT,
            Modifiers::SUPER,
            Modifiers::CTRL,
        ] {
            assert_eq!(candidate(key, mods), None, "{key:?} {mods:?}");
        }
    }
    for key in [
        NamedKey::NumpadHome,
        NamedKey::NumpadEnd,
        NamedKey::NumpadDelete,
    ] {
        assert_eq!(candidate(key, Modifiers::empty()), None, "{key:?}");
    }
    let chord = |c: char, mods| {
        classify_press(&InputEvent::Key {
            key: Key::Character(c),
            mods,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        })
        .predict_candidate
    };
    for (c, edit) in [('a', EditKey::CtrlA), ('e', EditKey::CtrlE)] {
        assert_eq!(
            chord(c, Modifiers::CTRL),
            Some(PredictKey::Edit(edit)),
            "Ctrl-{c}"
        );
        assert_eq!(
            chord(c, Modifiers::CTRL | Modifiers::NUM_LOCK),
            Some(PredictKey::Edit(edit)),
            "Ctrl-{c} with a lock"
        );
        for mods in [
            Modifiers::CTRL | Modifiers::SHIFT,
            Modifiers::CTRL | Modifiers::ALT,
            Modifiers::CTRL | Modifiers::SUPER,
        ] {
            assert_eq!(chord(c, mods), None, "{c} {mods:?}");
        }
        assert_eq!(chord(c, Modifiers::empty()), Some(PredictKey::Glyph(c)));
    }
    assert_eq!(chord('b', Modifiers::CTRL), None, "Ctrl-B is not modelled");
}

#[test]
fn keypad_navigation_and_delete_classify_as_their_twins() {
    let left_ev = press(NamedKey::NumpadArrowLeft, Modifiers::empty());
    let left = classify_press(&left_ev);
    assert!(left.navigation_key && left.brk);
    assert!(matches!(left.console_input, Some(PetInputKind::Navigate)));
    let end_ev = press(NamedKey::NumpadEnd, Modifiers::empty());
    assert!(classify_press(&end_ev).navigation_key);
    let del_ev = press(NamedKey::NumpadDelete, Modifiers::empty());
    let del = classify_press(&del_ev);
    assert!(del.kill_key && !del.kill_moves && del.kill_word);
    assert!(matches!(del.console_input, Some(PetInputKind::Delete)));
}

#[test]
fn keypad_begin_and_the_main_block_are_untouched() {
    let begin_ev = press(NamedKey::NumpadBegin, Modifiers::empty());
    let begin = classify_press(&begin_ev);
    assert_eq!(begin.typed, None);
    assert!(!begin.navigation_key && !begin.enter_like && !begin.kill_key);
    assert!(begin.console_input.is_none());
    let five = InputEvent::Key {
        key: Key::Character('5'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert_eq!(classify_press(&five).typed, Some('5'));
    assert!(is_plain_enter(&press(NamedKey::Enter, Modifiers::empty())));
}
