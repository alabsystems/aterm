// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use aterm_core::terminal::BlockState;

use super::{DeferredReaderGate, UpdatePark, attach_reader_inner, park_reader};
use crate::{App, Session, Wake, WindowId};

const PATIENCE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let seen =
        aterm_spec::wait::within(PATIENCE, std::time::Duration::ZERO, || done().then_some(()));
    assert!(seen.is_some(), "timed out waiting for {what}");
}

fn newest_block(session: &Session) -> Option<BlockState> {
    crate::term_lock(&session.term)
        .all_blocks()
        .last()
        .map(|block| block.state)
}

fn program_writes(slave: i32, bytes: &[u8]) {
    // SAFETY: a write of a live buffer to the test-owned slave.
    let wrote = unsafe { libc::write(slave, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(
        usize::try_from(wrote).ok(),
        Some(bytes.len()),
        "the program wrote"
    );
}

fn parked_at_a_prompt(app: &App, id: u64) -> (Session, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut session = crate::stub_session(id);
    session.master = master;
    attach_reader_inner(
        &mut session,
        WindowId(0),
        Arc::new(|_: Wake| true),
        &app.session_factory,
        None,
    )
    .expect("attach the real reader");
    program_writes(slave, b"\x1b]133;A\x07$ ");
    wait_for("the prompt to be parsed", || {
        newest_block(&session) == Some(BlockState::PromptOnly)
    });
    assert!(!session.ctx.update_parked.is_parked(), "a live reader");
    assert!(park_reader(&mut session, Instant::now() + PATIENCE));
    assert!(session.ctx.update_parked.is_parked());
    (session, slave)
}

fn close(mut session: Session, slave: i32) {
    let _ = park_reader(&mut session, Instant::now() + PATIENCE);
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

#[test]
fn a_resumed_reader_ends_the_park_only_once_its_parser_holds_what_queued() {
    let app = App::headless_for_test();
    let (mut session, slave) = parked_at_a_prompt(&app, 93);
    program_writes(slave, b"\x1b]133;B\x07make\r\n\x1b]133;C\x07building...");
    assert!(aterm_pty::output_queue_len(session.master).is_some_and(|n| n > 0));

    let mut factory = app.session_factory.clone();
    factory.byte_arm = false;
    let gate = DeferredReaderGate::closed();
    attach_reader_inner(
        &mut session,
        WindowId(0),
        Arc::new(|_: Wake| true),
        &factory,
        Some(gate.clone()),
    )
    .expect("reattach the reader");
    assert!(
        session.ctx.update_parked.is_parked(),
        "attached, the reader has read nothing: the park stands"
    );

    session.last_output_ns.store(0, Ordering::Release);
    let stalled = session.term.lock().expect("terminal lock");
    gate.release();
    wait_for("the parse stage to take up what queued", || {
        session.last_output_ns.load(Ordering::Acquire) != 0
    });
    assert_eq!(aterm_pty::output_queue_len(session.master), Some(0));
    assert!(
        session.ctx.update_parked.is_parked(),
        "read but not yet parsed: the parser still shows the prompt"
    );
    assert_eq!(
        stalled.all_blocks().last().map(|block| block.state),
        Some(BlockState::PromptOnly)
    );
    drop(stalled);

    wait_for("the caught-up reader to end the park", || {
        !session.ctx.update_parked.is_parked()
    });
    assert_eq!(
        newest_block(&session),
        Some(BlockState::Executing),
        "the park ended with `make` parsed"
    );
    close(session, slave);
}

#[test]
fn a_resumed_reader_with_nothing_queued_ends_the_park_at_once() {
    let app = App::headless_for_test();
    let (mut session, slave) = parked_at_a_prompt(&app, 94);
    assert_eq!(aterm_pty::output_queue_len(session.master), Some(0));
    let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);
    attach_reader_inner(
        &mut session,
        WindowId(0),
        Arc::new(|_: Wake| true),
        &app.session_factory,
        None,
    )
    .expect("reattach the reader");
    wait_for("the reader to end the park", || {
        !session.ctx.update_parked.is_parked()
    });
    assert!(waits.wait(PATIENCE), "the resume rings the session's waits");
    assert_eq!(newest_block(&session), Some(BlockState::PromptOnly));

    assert!(park_reader(&mut session, Instant::now() + PATIENCE));
    assert!(
        waits.wait(std::time::Duration::ZERO),
        "the park rings the session's waits"
    );
    let gate = DeferredReaderGate::closed();
    let (started, ready) = std::sync::mpsc::channel();
    attach_reader_inner(
        &mut session,
        WindowId(0),
        Arc::new(move |wake: Wake| {
            if matches!(wake, Wake::Ready { .. }) {
                let _ = started.send(());
            }
            true
        }),
        &app.session_factory,
        Some(gate.clone()),
    )
    .expect("reattach the reader");
    session.ctx.update_parked.park_for_test();
    gate.release();
    assert!(
        ready.recv_timeout(PATIENCE).is_ok(),
        "timed out waiting for the reader to start"
    );
    session.reader_stop.store(true, Ordering::Release);
    aterm_pty::wake(session.wake_wr);
    wait_for("the reader to stop", || {
        session
            .reader_join
            .as_ref()
            .is_some_and(super::ReaderJoin::is_finished)
    });
    assert!(
        session.ctx.update_parked.is_parked(),
        "a stale catch-up ends no park"
    );
    assert!(
        !waits.wait(std::time::Duration::ZERO),
        "a stale catch-up rings nobody"
    );
    close(session, slave);
}

#[test]
fn a_late_catch_up_never_ends_a_later_park() {
    let park = UpdatePark::default();
    assert!(!park.is_parked());
    park.park();
    let first = park.mark();
    park.park();
    assert!(!park.caught_up(first), "a stale mark ends nothing");
    assert!(park.is_parked(), "the later park stands");
    assert!(
        park.caught_up(park.mark()),
        "the current mark ends its park"
    );
    assert!(!park.is_parked());
    park.park();
    assert!(!park.caught_up(0));
    assert!(park.is_parked());
}
