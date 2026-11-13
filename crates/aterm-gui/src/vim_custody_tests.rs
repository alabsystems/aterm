// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn raw_navigation_sequences_do_not_become_standalone_escape_keys() {
    assert!(!has_escape(b"\x1b[A\x1b[1;5D\x1bOF"));
    assert!(has_escape(b"\x1b"));
    assert!(has_escape(b"\x1bp"));
    assert!(has_escape(b"\x1b[A\x1b"));
}

fn frame(terminal: u64, generation: u64, mode: Mode) -> Frame {
    let mut timeline = crate::session_timeline::SessionTimeline::default();
    timeline.note_foreground_group(123);
    timeline.set_program(123, Some("claude".into()));
    Frame {
        owner: Owner {
            terminal,
            foreground: timeline.foreground_stamp().unwrap(),
        },
        generation,
        epoch: 0,
        mode,
    }
}

fn inserting() -> State {
    let mut custody = State::default();
    let insert = frame(1, 1, Mode::Insert);
    assert!(!custody.key(insert, Key::Text));
    assert!(custody.frame(frame(1, 2, Mode::Insert)));
    custody
}

#[test]
fn first_queued_text_waits_for_insert_after_a_confirmed_normal_frame() {
    let mut custody = inserting();
    assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
    assert!(!custody.frame(frame(1, 3, Mode::Normal)));
    assert!(!custody.key(frame(1, 3, Mode::Normal), Key::Text)); // i
    assert!(!custody.key(frame(1, 3, Mode::Normal), Key::Text)); // p
    assert!(custody.frame(frame(1, 4, Mode::Insert)));
}

#[test]
fn entry_intent_keeps_the_first_press_across_stale_normal_repeats() {
    let mut custody = inserting();
    custody.key(frame(1, 2, Mode::Insert), Key::Escape);
    custody.frame(frame(1, 3, Mode::Normal));
    let first = Instant::now();
    custody.key_at(frame(1, 3, Mode::Normal), Key::Entry('i'), first);
    custody.key_at(
        frame(1, 3, Mode::Normal),
        Key::Entry('i'),
        first + std::time::Duration::from_millis(1),
    );
    custody.frame(frame(1, 4, Mode::Unknown));
    custody.key(frame(1, 4, Mode::Unknown), Key::Entry('a'));
    assert_eq!(custody.entry, Some((first, 'i')));
    assert!(custody.frame(frame(1, 5, Mode::Insert)));
    custody.key(frame(1, 5, Mode::Insert), Key::Boundary);
    assert_eq!(custody.entry, None);
    custody.frame(frame(2, 6, Mode::Insert));
    assert_eq!(custody.entry, None);
}

#[test]
fn failed_entry_revokes_only_its_own_intent() {
    let custody = Custody::default();
    let at = Instant::now();
    custody.state.lock().unwrap().entry = Some((at, 'i'));
    custody.reject_entry(at + std::time::Duration::from_millis(1));
    assert_eq!(custody.state.lock().unwrap().entry, Some((at, 'i')));
    custody.reject_entry(at);
    assert_eq!(custody.state.lock().unwrap().entry, None);
    assert_eq!(custody.epoch(), 1);

    let f = |generation: u64, mode: Mode, epoch: u64| Frame {
        epoch,
        ..frame(1, generation, mode)
    };
    custody.key(f(1, Mode::Insert, custody.epoch()), Key::Text);
    assert!(custody.frame(f(2, Mode::Insert, custody.epoch())));
    custody.key(f(2, Mode::Insert, custody.epoch()), Key::Escape);
    assert!(!custody.frame(f(3, Mode::Normal, custody.epoch())));
    custody.key_at(f(3, Mode::Normal, custody.epoch()), Key::Entry('i'), at);
    custody.key(f(3, Mode::Normal, custody.epoch()), Key::Boundary);
    custody.reject_entry(at + std::time::Duration::from_millis(1));
    assert!(custody.state.lock().unwrap().hint_editable);
    custody.reject_entry(at);
    assert!(!custody.state.lock().unwrap().hint_editable);
    assert!(!custody.frame(f(4, Mode::ExitHint, custody.epoch())));
    assert!(!custody.key(f(4, Mode::ExitHint, custody.epoch()), Key::Text));
}

#[test]
fn intermediate_normal_or_unknown_repaints_do_not_invent_an_exit_boundary() {
    let mut custody = inserting();
    custody.key(frame(1, 2, Mode::Insert), Key::Escape);
    custody.frame(frame(1, 3, Mode::Normal));
    custody.key(frame(1, 3, Mode::Normal), Key::Text);
    assert!(!custody.frame(frame(1, 4, Mode::Normal)));
    custody.key(frame(1, 4, Mode::Normal), Key::Text);
    assert!(!custody.frame(frame(1, 5, Mode::Unknown)));
    custody.key(frame(1, 5, Mode::Unknown), Key::Text);
    assert!(custody.frame(frame(1, 6, Mode::Insert)));
}

#[test]
fn old_insert_and_new_streaming_repaint_do_not_license_normal_motion() {
    let mut custody = inserting();
    assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
    assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Text));
    assert!(!custody.frame(frame(1, 3, Mode::Insert)));
    assert!(!custody.key(frame(1, 3, Mode::Insert), Key::Text));
    assert!(!custody.frame(frame(1, 4, Mode::Unknown)));
    assert!(!custody.frame(frame(1, 5, Mode::Insert)));
}

#[test]
fn confirmed_mode_is_separate_from_the_engines_echo_proof() {
    let mut custody = inserting();
    custody.key(frame(1, 2, Mode::Insert), Key::Escape);
    custody.frame(frame(1, 3, Mode::Normal));
    custody.key(frame(1, 3, Mode::Normal), Key::Text);
    assert!(custody.frame(frame(1, 4, Mode::Insert)));
    assert!(custody.key(frame(1, 4, Mode::Insert), Key::Text));
}

#[test]
fn ctrl_c_hint_preserves_known_insert_but_never_settles_an_escape() {
    for boundary in [Key::Boundary, Key::Escape] {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), boundary);
        assert_eq!(
            custody.frame(frame(1, 3, Mode::ExitHint)),
            matches!(boundary, Key::Boundary)
        );
        let allowed = custody.key(frame(1, 3, Mode::ExitHint), Key::Text);
        assert_eq!(allowed, matches!(boundary, Key::Boundary));
    }
}

#[test]
fn ctrl_c_after_an_entry_the_agent_has_not_drawn_keeps_the_hint_editable() {
    for (entry, editable) in [(Some('i'), true), (None, false)] {
        let mut custody = inserting();
        custody.key(frame(1, 2, Mode::Insert), Key::Escape);
        custody.frame(frame(1, 3, Mode::Normal));
        if let Some(glyph) = entry {
            custody.key(frame(1, 3, Mode::Normal), Key::Entry(glyph));
        }
        custody.key(frame(1, 3, Mode::Normal), Key::Boundary);
        assert_eq!(
            custody.frame(frame(1, 4, Mode::ExitHint)),
            editable,
            "{entry:?}, then Ctrl-C before the repaint: the hint frame"
        );
        assert_eq!(
            custody.key(frame(1, 4, Mode::ExitHint), Key::Text),
            editable
        );
    }
}

#[test]
fn pending_text_does_not_cross_terminal_owners() {
    let mut custody = inserting();
    custody.key(frame(1, 2, Mode::Insert), Key::Text);
    assert!(!custody.frame(frame(2, 3, Mode::Insert)));
    assert!(!custody.frame(frame(1, 4, Mode::Insert)));
}

#[test]
fn contended_event_is_not_lost_as_permission_to_keep_old_insert_evidence() {
    let custody = Custody::default();
    custody.key(frame(1, 1, Mode::Insert), Key::Text);
    let held = custody.state.lock().unwrap();
    assert!(!custody.key(frame(1, 2, Mode::Insert), Key::Escape));
    drop(held);
    assert!(!custody.key(frame(1, 3, Mode::Insert), Key::Text));
    let mut current = frame(1, 4, Mode::Insert);
    current.epoch = custody.epoch();
    assert!(!custody.frame(current));
}

#[test]
fn mode_requires_a_complete_composer_and_distinguishes_exit_hint() {
    let rule = "─".repeat(30);
    for (footer, expected) in [
        ("-- INSERT -- ⏵⏵ auto mode on", Mode::Insert),
        ("⏵⏵ auto mode on", Mode::Normal),
        ("Press Ctrl-C again to exit", Mode::ExitHint),
        ("", Mode::Unknown),
        ("some app text", Mode::Unknown),
    ] {
        let rows = vec![rule.clone(), "❯ prompt".into(), rule.clone(), footer.into()];
        assert_eq!(mode(&rows, 1), expected);
        assert_eq!(mode(&rows, 3), Mode::Unknown);
    }
}
