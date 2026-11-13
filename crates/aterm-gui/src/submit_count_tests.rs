// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SubmitCount, SubmitInput};
use crate::input::{InputEvent, PasteFraming};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn key(key: Key, mods: Modifiers, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

fn press(k: Key) -> InputEvent {
    key(k, Modifiers::empty(), KeyEventType::Press)
}

fn enter() -> InputEvent {
    press(Key::Named(NamedKey::Enter))
}

fn written(count: &SubmitCount, events: &[InputEvent]) -> u64 {
    for ev in events {
        count.accepted(SubmitInput::of(ev));
    }
    count.submits()
}

#[test]
fn an_enter_over_something_typed_is_a_submission_whoever_typed_it() {
    let count = SubmitCount::default();
    let typed = [
        press(Key::Character('h')),
        press(Key::Character('i')),
        enter(),
    ];
    assert_eq!(written(&count, &typed), 1);
    let sent = [InputEvent::KeySequence(b"keep going".to_vec()), enter()];
    assert_eq!(written(&count, &sent), 2);
    let turn = [
        InputEvent::Paste("land the task".to_string(), PasteFraming::AtDrain),
        enter(),
    ];
    assert_eq!(written(&count, &turn), 3);
    let raw = [InputEvent::KeySequence(b"one\rtwo\r".to_vec())];
    assert_eq!(written(&count, &raw), 5);
    let recalled = [press(Key::Named(NamedKey::ArrowUp)), enter()];
    assert_eq!(written(&count, &recalled), 6);
    let keypad = [
        press(Key::Character('x')),
        press(Key::Named(NamedKey::NumpadEnter)),
    ];
    assert_eq!(written(&count, &keypad), 7);
}

#[test]
fn an_enter_over_nothing_typed_and_a_newline_are_no_submission() {
    let count = SubmitCount::default();
    let empty = [enter(), press(Key::Character('a')), enter(), enter()];
    assert_eq!(
        written(&count, &empty),
        1,
        "the second Enter submits nothing"
    );
    assert_eq!(
        written(&count, &[InputEvent::KeySequence(b"\r".to_vec())]),
        1
    );
    let shifted = key(
        Key::Named(NamedKey::Enter),
        Modifiers::SHIFT,
        KeyEventType::Press,
    );
    assert_eq!(written(&count, &[shifted]), 1);
    assert_eq!(written(&count, &[enter()]), 2, "the newline was composed");
    let inert = [
        key(
            Key::Character('a'),
            Modifiers::empty(),
            KeyEventType::Release,
        ),
        press(Key::Named(NamedKey::ShiftLeft)),
        press(Key::Named(NamedKey::CapsLock)),
        enter(),
    ];
    assert_eq!(written(&count, &inert), 2);
    let reports = [InputEvent::Focus(true), enter()];
    assert_eq!(written(&count, &reports), 2);
    let release = key(
        Key::Named(NamedKey::Enter),
        Modifiers::empty(),
        KeyEventType::Release,
    );
    assert_eq!(written(&count, &[release, enter()]), 2);
}

#[test]
fn raw_bytes_carry_their_composing_across_writes() {
    let count = SubmitCount::default();
    assert_eq!(
        written(&count, &[InputEvent::KeySequence(b"draft".to_vec())]),
        0
    );
    assert_eq!(
        written(&count, &[InputEvent::KeySequence(b"\r".to_vec())]),
        1
    );
    assert_eq!(
        written(&count, &[InputEvent::KeySequence(b"a\rb".to_vec())]),
        2
    );
    assert_eq!(written(&count, &[enter()]), 3);
    assert_eq!(
        written(&count, &[InputEvent::KeySequence(b"\r\r\r".to_vec())]),
        3
    );
}
