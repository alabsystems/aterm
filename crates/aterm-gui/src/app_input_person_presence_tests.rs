// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::pet_console_input_tests::observing_app as app;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use crate::input::{InputEvent, InputOutcome, PasteFraming, Source};
use crate::{App, SessionCtx, WindowId};

const CTL: Source = Source::Controller;

fn ctx(app: &App) -> Arc<SessionCtx> {
    let session = app.front_terminal(WindowId(0)).unwrap().session;
    app.pool.get(session).unwrap().ctx.clone()
}

fn human_events(ctx: &SessionCtx) -> usize {
    ctx.timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "human")
        .count()
}

fn typing() -> Vec<InputEvent> {
    vec![
        InputEvent::Key {
            key: Key::Character('x'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::Escape),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Text("typed".into()),
        InputEvent::KeySequence(b"\x1b[D".to_vec()),
        InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
    ]
}

fn click(pressed: bool) -> InputEvent {
    InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed,
        row: 1,
        col: 1,
        mods: 0,
        click_count: 1,
        side: aterm_core::selection::SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    }
}

fn pointer_move(buttons: u8) -> InputEvent {
    InputEvent::MouseMove {
        buttons,
        row: 2,
        col: 3,
        mods: 0,
        side: aterm_core::selection::SelectionSide::Left,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    }
}

fn pointing() -> Vec<InputEvent> {
    vec![
        click(true),
        pointer_move(0),
        InputEvent::Wheel {
            dir: aterm_types::mouse::WheelDir::Up,
            lines: 3,
            detent: true,
            row: 1,
            col: 1,
            mods: 0,
            px_off: crate::input::PixelOffset::CELL_ORIGIN,
        },
        InputEvent::ScrollView(crate::input::ScrollIntent::Up),
    ]
}

#[test]
fn a_window_keystroke_stamps_human_ms_and_a_control_write_never_does() {
    for event in typing().into_iter().chain(pointing()) {
        let (mut app, _reader) = app();
        let ctx = ctx(&app);
        assert_eq!(app.input(WindowId(0), event.clone(), CTL), InputOutcome::Ok);
        assert_eq!(ctx.human_input.ms_since(crate::metrics::now_us()), None);
        assert_eq!(
            ctx.human_input.wire(crate::metrics::now_us()),
            "-",
            "{event:?}"
        );
        assert_eq!(human_events(&ctx), 0, "{event:?}");
        assert_eq!(
            app.input(WindowId(0), event.clone(), Source::Human),
            InputOutcome::Ok
        );
        let since = ctx.human_input.ms_since(crate::metrics::now_us());
        assert!(since.is_some_and(|ms| ms < 60_000), "{event:?}: {since:?}");
        assert!(
            ctx.human_input.wire(crate::metrics::now_us()) != "-",
            "{event:?}"
        );
        assert_eq!(human_events(&ctx), 1, "{event:?}");
    }
}

/// THE PERSON'S TYPING IS STAMPED APART (`human_typed_ms=`, 2026-10-30 review 8): a key, text,
/// a key sequence or a paste that reaches the program stamps it, while a click, a drag, a
/// wheel or a scroll of the view stamps `human_ms` alone, so the harness's hour after the
/// person's key never starts at a gesture that says nothing.
#[test]
fn typing_alone_stamps_human_typed_ms() {
    for (event, types) in typing()
        .into_iter()
        .map(|e| (e, true))
        .chain(pointing().into_iter().map(|e| (e, false)))
    {
        let (mut app, _reader) = app();
        let ctx = ctx(&app);
        assert_eq!(
            app.input(WindowId(0), event.clone(), Source::Human),
            InputOutcome::Ok
        );
        let now = crate::metrics::now_us();
        assert!(ctx.human_input.ms_since(now).is_some(), "{event:?}");
        assert_eq!(
            ctx.human_input.typed_ms_since(now).is_some(),
            types,
            "{event:?}"
        );
        assert_eq!(ctx.human_input.typed_wire(now) != "-", types, "{event:?}");
    }
}

#[test]
fn a_burst_is_one_human_event_and_what_is_not_a_hand_stamps_nothing() {
    let (mut app, _reader) = app();
    let ctx = ctx(&app);
    for event in [
        InputEvent::Key {
            key: Key::Character('x'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Release,
        },
        pointer_move(3),
        InputEvent::Focus(true),
    ] {
        let _ = app.input(WindowId(0), event, Source::Human);
    }
    assert_eq!(ctx.human_input.ms_since(crate::metrics::now_us()), None);
    for event in typing().into_iter().chain(pointing()).chain([click(false)]) {
        let _ = app.input(WindowId(0), event, Source::Human);
    }
    assert_eq!(human_events(&ctx), 1);
}
