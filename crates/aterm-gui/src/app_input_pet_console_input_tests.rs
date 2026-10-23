// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use aterm_effects::kitty_pet::PetInputKind;
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use crate::input::{InputEvent, InputOutcome, PasteFraming, Source};
use crate::{App, WindowId, term_lock};

const CTL: Source = Source::Controller;

/// Returns a headless `App` whose session writes into a pipe, and the pipe's read end.
pub(super) fn observing_app() -> (App, OwnedFd) {
    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let (reader, writer) =
        unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    let app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer)));
    (app, reader)
}

fn press(key: Key) -> InputEvent {
    InputEvent::Key {
        key,
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn observed(app: &App) -> (u64, Option<PetInputKind>) {
    let pet = &app.windows[&WindowId(0)].fx.companion();
    (pet.console_input_seq(), pet.console_input_kind())
}

type WakeCase = (&'static str, fn() -> InputEvent, bool);

#[test]
fn the_typed_wake_is_armed_by_a_controller_key_and_a_paste_but_not_a_bare_modifier() {
    use std::time::Duration;

    let cases: [WakeCase; 3] = [
        ("a controller key", || press(Key::Character('n')), true),
        (
            "a paste",
            || InputEvent::Paste("echo hi".into(), PasteFraming::AtDrain),
            true,
        ),
        (
            "a bare modifier",
            || InputEvent::Key {
                key: Key::Named(NamedKey::SuperLeft),
                mods: Modifiers::SUPER,
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            false,
        ),
    ];
    for (what, build, arms) in cases {
        let (mut app, _reader) = observing_app();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).expect("window").focused = false;
        let before = std::time::Instant::now();
        assert!(
            !app.cursor_fx_focus(wid, false, before),
            "{what}: precondition — an unfocused, untyped window is demoted"
        );

        assert_eq!(app.input(wid, build(), CTL), InputOutcome::Ok, "{what}");

        let after = std::time::Instant::now();
        assert_eq!(
            app.cursor_fx_focus(wid, false, after),
            arms,
            "{what}: the typed wake"
        );
        if arms {
            assert!(
                !app.cursor_fx_focus(
                    wid,
                    false,
                    after + crate::app_render::CURSOR_FX_TYPED_WAKE + Duration::from_secs(1)
                ),
                "{what}: the wake never expired"
            );
        }
    }
}

#[test]
fn human_and_control_input_note_once_without_caret_travel_or_echo_credit() {
    for source in [Source::Human, CTL] {
        let (mut app, _reader) = observing_app();
        let wid = WindowId(0);
        let term = app.front_terminal(wid).unwrap().term.clone();
        term_lock(&term).process(b"old\r");
        let before = observed(&app).0;
        assert_eq!(
            app.input(wid, press(Key::Character('n')), source),
            InputOutcome::Ok
        );
        assert_eq!(observed(&app), (before + 1, Some(PetInputKind::Text)));
        term_lock(&term).process(b"n\r");
        term_lock(&term).process(b"\x1b[2Kprogram repaint\r");
        assert_eq!(observed(&app), (before + 1, Some(PetInputKind::Text)));
    }
}

#[test]
fn ime_preview_is_silent_and_each_commit_bundle_notes_once() {
    let (mut app, _reader) = observing_app();
    let wid = WindowId(0);
    let before = observed(&app).0;
    app.on_ime_preedit(wid, "中文".into(), Some((0, 0)));
    assert_eq!(observed(&app).0, before);
    app.on_ime_commit(wid, "中文🙂".into());
    assert_eq!(observed(&app), (before + 1, Some(PetInputKind::Text)));
    app.on_ime_commit(wid, "\u{0301}".into());
    assert_eq!(observed(&app), (before + 2, Some(PetInputKind::Text)));
    app.on_ime_commit(wid, String::new());
    assert_eq!(observed(&app).0, before + 2);
}

#[test]
fn an_ime_composition_is_a_person_typing() {
    let (mut app, _reader) = observing_app();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("a focused session");
    let human = |app: &App| {
        app.pool
            .get(session)
            .expect("pooled")
            .ctx
            .human_input
            .ms_since(crate::metrics::now_us())
    };
    assert_eq!(human(&app), None, "nobody yet");
    app.on_ime_preedit(wid, String::new(), None);
    assert_eq!(human(&app), None, "an empty preedit is nobody");
    app.on_ime_preedit(wid, "にほ".into(), Some((0, 0)));
    assert!(
        human(&app).is_some_and(|ms| ms < 60_000),
        "{:?}",
        human(&app)
    );
}

#[test]
fn navigation_and_delete_keep_distinct_intents_at_the_real_seam() {
    let (mut app, _reader) = observing_app();
    let wid = WindowId(0);
    for (key, kind) in [
        (NamedKey::ArrowLeft, PetInputKind::Navigate),
        (NamedKey::Home, PetInputKind::Navigate),
        (NamedKey::Tab, PetInputKind::Navigate),
        (NamedKey::Backspace, PetInputKind::Delete),
        (NamedKey::Delete, PetInputKind::Delete),
        (NamedKey::Enter, PetInputKind::Submit),
    ] {
        let before = observed(&app).0;
        assert_eq!(
            app.input(wid, press(Key::Named(key)), CTL),
            InputOutcome::Ok
        );
        assert_eq!(observed(&app), (before + 1, Some(kind)));
    }
}

#[test]
fn releases_modifiers_raw_sequences_and_failed_writes_cannot_claim_an_edit() {
    let (mut app, _reader) = observing_app();
    let wid = WindowId(0);
    let before = observed(&app).0;
    let release = InputEvent::Key {
        key: Key::Character('x'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    for event in [
        release,
        press(Key::Named(NamedKey::ShiftLeft)),
        InputEvent::Text(String::new()),
        InputEvent::KeySequence(b"\x1b[D".to_vec()),
    ] {
        assert_eq!(app.input(wid, event, CTL), InputOutcome::Ok);
        assert_eq!(observed(&app).0, before);
    }
    let mut failed = App::headless_for_test_with_sink(Arc::new(SinkWriter::new(-1)));
    let before = observed(&failed).0;
    assert_eq!(
        failed.input(wid, press(Key::Character('x')), Source::Human),
        InputOutcome::WriteFailed
    );
    assert_eq!(observed(&failed).0, before);
}

#[test]
fn a_paste_and_queued_key_each_commit_one_intent_without_drain_replay() {
    let (mut app, _reader) = observing_app();
    let wid = WindowId(0);
    let sink = app.front_terminal(wid).unwrap().sink.clone();
    let pin = super::paste_order::pin_ordering_for_test(&sink);
    let before = observed(&app).0;
    for text in ["", "\x1b\x03\u{009b}\x7f"] {
        assert_eq!(
            app.input(
                wid,
                InputEvent::Paste(text.into(), PasteFraming::AtDrain),
                Source::Human
            ),
            InputOutcome::Ok
        );
        assert_eq!(observed(&app).0, before);
    }
    assert_eq!(
        app.input(
            wid,
            InputEvent::Paste("a whole pasted sentence".into(), PasteFraming::AtDrain),
            Source::Human
        ),
        InputOutcome::Ok
    );
    assert_eq!(observed(&app), (before + 1, Some(PetInputKind::Paste)));
    assert_eq!(
        app.input(wid, press(Key::Character('x')), CTL),
        InputOutcome::Ok
    );
    assert_eq!(observed(&app), (before + 2, Some(PetInputKind::Text)));
    drop(pin);
    super::paste_order::await_drain_for_test(&sink);
    assert!(!super::paste_order::is_ordering(&sink));
    assert_eq!(observed(&app), (before + 2, Some(PetInputKind::Text)));
}
