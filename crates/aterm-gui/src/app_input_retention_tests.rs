// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

fn fresh_app() -> (App, UnixStream) {
    let (writer, reader) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    (App::headless_for_test_with_sink(sink), reader)
}

fn posted(app: &mut App, text: &str) {
    app.dispatch_ime_event(
        WindowId(0),
        aterm_window::event::Ime::Commit(text.into()),
        aterm_window::event::InputOrigin::Posted,
    );
}

#[test]
fn posted_input_retains_its_session_without_claiming_a_person_or_typist() {
    let (mut app, mut reader) = fresh_app();
    let ctx = Arc::clone(&app.pool.get(0).unwrap().ctx);
    crate::feed_session_for_test(&app, 0, b"prompt> ");
    assert!(!ctx.output_echo.retains_input());
    posted(&mut app, "x");
    let mut byte = [0];
    reader.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
    assert!(ctx.output_echo.retains_input());
    assert_eq!(ctx.human_input.seq(), 0);
    assert!(!ctx.typed.driver_admitted());
    let _ = ctx.output_echo.sample(&ctx.sink, std::time::Instant::now());
    assert!(
        ctx.output_echo.retains_input(),
        "transient echo reads do not erase acceptance"
    );
}

#[test]
fn refused_posted_input_never_retains_its_session() {
    for unavailable in [false, true] {
        let (mut app, _reader) = fresh_app();
        let ctx = Arc::clone(&app.pool.get(0).unwrap().ctx);
        let _queue = paste_order::force_conpty_route_for_test();
        let _refusal = paste_order::reject_admission_for_test(&ctx.sink, unavailable);
        posted(&mut app, "refused");
        assert!(!ctx.output_echo.retains_input());
        assert_eq!(ctx.output_echo.pending_input.load(Ordering::Acquire), 0);
        assert_eq!(ctx.human_input.seq(), 0);
        assert!(!ctx.typed.driver_admitted());
    }
}

#[test]
fn queued_posted_input_retains_only_its_original_session_until_the_receipt_settles() {
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    for sever in [false, true] {
        let (mut original, mut reader) = fresh_app();
        let ctx = Arc::clone(&original.pool.get(0).unwrap().ctx);
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release = Release(Some(release));
        let _queue = paste_order::force_conpty_route_for_test();
        paste_order::with_write_gate_for_test(
            move || {
                entered.send(()).unwrap();
                release_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
            },
            || {
                posted(&mut original, "queued");
                entered_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
                assert!(ctx.output_echo.retains_input());
                assert!(!ctx.output_echo.published.lock().unwrap().retained_input);
                assert_eq!(ctx.human_input.seq(), 0);
                assert!(!ctx.typed.driver_admitted());
                // Numeric session 0 now denotes another App's independent session.
                let (replacement, _replacement_reader) = fresh_app();
                let old_app = std::mem::replace(&mut original, replacement);
                let current = Arc::clone(&original.pool.get(0).unwrap().ctx);
                assert!(!Arc::ptr_eq(&ctx.output_echo, &current.output_echo));
                assert!(!current.output_echo.retains_input());
                if sever {
                    ctx.sink.sever_input();
                }
                drop(release);
                if !sever {
                    let mut bytes = [0; 6];
                    reader.read_exact(&mut bytes).unwrap();
                    assert_eq!(&bytes, b"queued");
                }
                paste_order::await_drain_for_test(&ctx.sink);
                assert_eq!(ctx.output_echo.pending_input.load(Ordering::Acquire), 0);
                assert_eq!(ctx.output_echo.retains_input(), !sever);
                assert!(!current.output_echo.retains_input());
                drop(old_app);
            },
        );
    }
}

#[test]
fn retention_excludes_protocol_only_frames_and_keeps_an_accepted_prefix() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let ignored = [
        InputEvent::Text(String::new()),
        InputEvent::KeySequence(Vec::new()),
        InputEvent::Paste(String::new(), input::PasteFraming::AtDrain),
        InputEvent::Paste("\x1b\x07".into(), input::PasteFraming::AtDrain),
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Release,
        },
        InputEvent::Key {
            key: Key::Named(NamedKey::ShiftLeft),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        },
        InputEvent::Focus(true),
    ];
    for event in ignored {
        let (app, _reader) = fresh_app();
        let session = app.pool.get(0).unwrap();
        crate::feed_session_for_test(&app, 0, b"\x1b[?2004h\x1b[>3u");
        paste_order::enqueue(
            &session.term,
            &session.ctx.modes,
            &session.ctx.sink,
            &session.ctx.output_echo,
            event,
            None,
            None,
        )
        .unwrap();
        paste_order::await_drain_for_test(&session.ctx.sink);
        assert!(!session.ctx.output_echo.retains_input());
    }
    let (app, mut reader) = fresh_app();
    let session = app.pool.get(0).unwrap();
    let text = format!("{}x", "\x1b".repeat(5000));
    paste_order::enqueue(
        &session.term,
        &session.ctx.modes,
        &session.ctx.sink,
        &session.ctx.output_echo,
        InputEvent::Paste(text, input::PasteFraming::AtDrain),
        None,
        None,
    )
    .unwrap();
    let mut byte = [0];
    reader.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
    paste_order::await_drain_for_test(&session.ctx.sink);
    assert!(
        session.ctx.output_echo.retains_input(),
        "kept data after a long stripped prefix still counts"
    );
    let (app, mut reader) = fresh_app();
    let session = app.pool.get(0).unwrap();
    let event = InputEvent::Text("prefix".into());
    let write = session.ctx.output_echo.begin_event(&event);
    let receipt = input::seam_egress_receipt(
        &session.term,
        &session.ctx.modes,
        &session.ctx.sink,
        &event,
        input::EgressMode::Interactive,
    );
    let mut bytes = [0; 6];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"prefix");
    assert!(receipt.accepted_order().is_some());
    write.finish_delivery(
        input::Delivery::Failed,
        receipt.accepted_order(),
        &session.ctx.sink,
    );
    assert!(
        session.ctx.output_echo.retains_input(),
        "a failed suffix cannot discard already accepted data"
    );
}

#[test]
fn an_explicit_launch_command_retains_its_input_but_a_prompt_does_not() {
    let (app, mut reader) = fresh_app();
    let session = app.pool.get(0).unwrap();
    crate::feed_session_for_test(&app, 0, b"automatic prompt> ");
    assert!(!session.ctx.output_echo.retains_input());
    crate::app_tabs::inject_launch_line(&session.term, &session.ctx, "sign in");
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"sign in\r");
    assert!(session.ctx.output_echo.retains_input());
}
