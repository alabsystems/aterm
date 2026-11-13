// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{feed_bytes, parse_key, send_bytes};
use crate::input::InputEvent;
use crate::{VideoInputSample, record_controller_video_attempts, video_sample_for_key};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

#[test]
fn video_keys_ledger_logs_the_send_and_feed_verbs() {
    let bytes = send_bytes("ls\\n");
    assert_eq!(bytes, b"ls\r".to_vec(), "the verb's bytes, not a stand-in");
    let mut log = Vec::new();
    let _ = record_controller_video_attempts(&mut log, &InputEvent::KeySequence(bytes), 100, 64);
    assert_eq!(
        log,
        vec![
            (100, VideoInputSample::Char('l')),
            (100, VideoInputSample::Char('s')),
            (100, VideoInputSample::Char('\r')),
        ],
        "`send ls\\n` must land three timestamped attempts, not silence"
    );

    let bytes = feed_bytes("1b5b41").expect("valid hex");
    let mut fed = Vec::new();
    let _ = record_controller_video_attempts(&mut fed, &InputEvent::KeySequence(bytes), 200, 64);
    assert_eq!(
        fed,
        vec![
            (200, VideoInputSample::Char('\u{1b}')),
            (200, VideoInputSample::Char('[')),
            (200, VideoInputSample::Char('A')),
        ],
        "`feed` bytes are attempts on the frame clock too"
    );

    let raw = [b'a', 0xff, 0xfe, b'b'];
    let mut lossy = Vec::new();
    let _ = record_controller_video_attempts(
        &mut lossy,
        &InputEvent::KeySequence(raw.to_vec()),
        300,
        64,
    );
    assert_eq!(
        lossy,
        vec![
            (300, VideoInputSample::Char('a')),
            (300, VideoInputSample::Char('\u{fffd}')),
            (300, VideoInputSample::Char('\u{fffd}')),
            (300, VideoInputSample::Char('b')),
        ]
    );
    assert_eq!(
        lossy.len(),
        String::from_utf8_lossy(&raw).chars().count(),
        "the in-place decoder agrees with from_utf8_lossy's char count"
    );

    let mut capped = Vec::new();
    let dropped = record_controller_video_attempts(
        &mut capped,
        &InputEvent::KeySequence(b"abcdefgh".to_vec()),
        400,
        3,
    );
    assert_eq!(capped.len(), 3, "the bounded ledger is still bounded");
    assert_eq!(dropped, 5, "and counts the five attempts it could not take");
}

#[test]
fn video_keys_ledger_names_keys_that_have_no_character() {
    for (verb, expect) in [
        ("up", VideoInputSample::Named(NamedKey::ArrowUp)),
        ("esc", VideoInputSample::Named(NamedKey::Escape)),
        ("backspace", VideoInputSample::Named(NamedKey::Backspace)),
        ("f1", VideoInputSample::Named(NamedKey::F1)),
        ("pagedown", VideoInputSample::Named(NamedKey::PageDown)),
        ("enter", VideoInputSample::Char('\n')),
        ("tab", VideoInputSample::Char('\t')),
        ("space", VideoInputSample::Char(' ')),
    ] {
        let ev = parse_key(verb).unwrap_or_else(|| panic!("`key {verb}` parses"));
        let mut log = Vec::new();
        let _ = record_controller_video_attempts(&mut log, &ev, 7, 64);
        assert_eq!(log, vec![(7, expect)], "`key {verb}` must be an attempt");
    }

    assert_eq!(
        video_sample_for_key(&Key::Named(NamedKey::ShiftLeft)),
        None,
        "a bare Shift expresses no input intent"
    );
    assert_eq!(video_sample_for_key(&Key::Named(NamedKey::CapsLock)), None);

    assert_eq!(
        VideoInputSample::Named(NamedKey::ArrowUp).json_field(),
        "\"key\":\"ArrowUp\""
    );
    assert_eq!(VideoInputSample::Char('a').json_field(), "\"ch\":\"a\"");
    assert_eq!(
        VideoInputSample::Char('\r').json_field(),
        "\"ch\":\"\\u000d\""
    );
    assert_eq!(
        VideoInputSample::Char('"').json_field(),
        "\"ch\":\"\\\"\"",
        "a typed quote cannot break the index"
    );
}

#[test]
fn unloggable_counter_ignores_events_the_ledger_never_records() {
    for quiet in [
        InputEvent::Focus(true),
        InputEvent::Focus(false),
        InputEvent::Key {
            key: Key::Named(NamedKey::ShiftLeft),
            mods: Modifiers::default(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
    ] {
        assert_eq!(
            crate::video_attempt_count(&quiet),
            0,
            "{quiet:?} is not an input attempt and must not be reported as an unlogged one"
        );
    }

    assert_eq!(
        crate::video_attempt_count(&InputEvent::Text("ab".into())),
        2,
        "a cross-session paste of two chars IS two attempts this ledger missed"
    );

    assert_eq!(
        crate::video_attempt_count(&InputEvent::KeySequence(Vec::new())),
        0
    );
    assert_eq!(
        crate::video_attempt_count(&InputEvent::KeySequence(send_bytes("hey"))),
        3
    );
}
