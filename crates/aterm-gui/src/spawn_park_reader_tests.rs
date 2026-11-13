// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::park_reader;

#[test]
fn panicked_reader_fails_closed_instead_of_authorizing_handoff() {
    let mut session = crate::stub_session(77);
    session.reader_join = Some(super::ReaderJoin::for_test(|| {
        panic!("injected PTY reader failure");
    }));
    let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);

    assert!(!park_reader(
        &mut session,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    ));
    assert!(session.reader_join.is_none());
    assert!(session.ctx.update_parked.is_parked());
    assert!(
        waits.wait(std::time::Duration::ZERO),
        "the park rings the session's waits"
    );
    assert!(park_reader(
        &mut session,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    ));
    assert!(!waits.wait(std::time::Duration::ZERO));
}

#[test]
fn a_reattached_reader_never_inherits_a_dead_wake_arm() {
    let session = crate::stub_session(78);
    session
        .output_wake_pending
        .store(1_000, std::sync::atomic::Ordering::Relaxed);
    session
        .reader_stop
        .store(true, std::sync::atomic::Ordering::Release);
    session.ctx.update_parked.park();
    let waits = crate::session_watch::Subscription::single(&session.ctx.watchers);

    super::reset_reader_latches(&session);

    assert!(
        session.ctx.update_parked.is_parked(),
        "the attach ends no park: the new reader has parsed nothing yet \
         (`update_park_catch_up_tests`)"
    );
    assert!(
        !waits.wait(std::time::Duration::ZERO),
        "the attach resumes nothing, so it rings nobody: the caught-up \
         reader rings as it ends the park"
    );

    assert!(
        !session
            .reader_stop
            .load(std::sync::atomic::Ordering::Acquire),
        "a fresh reader must not start already stopped"
    );
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    super::gated_output_wake(
        &session.output_wake_pending,
        1_001,
        0,
        false,
        || tx.send(()).is_ok(),
        || 1_002,
    );
    assert_eq!(
        rx.try_iter().count(),
        1,
        "the first burst after a re-attach must post its wake"
    );
}

#[test]
fn a_park_past_its_deadline_keeps_the_unfinished_handle() {
    let mut session = crate::stub_session(79);
    let (finish_reader, reader_held) = std::sync::mpsc::channel::<()>();
    session.reader_join = Some(super::ReaderJoin::for_test(move || {
        let _ = reader_held.recv();
    }));
    assert!(!park_reader(&mut session, std::time::Instant::now()));
    assert!(session.reader_join.is_some(), "the reader's handle is kept");
    assert!(!super::reader_attach_ready(&session));
    assert!(session.ctx.update_parked.is_parked());
    drop(finish_reader);
    assert!(park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    ));
    assert!(session.reader_join.is_none());
    assert!(super::reader_attach_ready(&session));

    let reader = super::ReaderJoin::for_test(|| {});
    let stopped = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::ZERO,
        || reader.is_finished().then_some(()),
    );
    assert!(stopped.is_some(), "timed out waiting for the reader to end");
    session.reader_join = Some(reader);
    let (drain, finish_writer) = crate::temporal::TemporalDrain::held_for_test();
    let observed_drain = drain.clone();
    session.temporal_drain = Some(drain);
    assert!(!park_reader(&mut session, std::time::Instant::now()));
    assert!(
        session.reader_join.is_none(),
        "the stopped reader is joined"
    );
    assert!(
        session.temporal_drain.is_some(),
        "the unwritten queue's drain is kept"
    );
    assert!(!super::reader_attach_ready(&session));
    assert_eq!(observed_drain.demands_for_test(), [1, 0, 0]);
    drop(finish_writer);
    let joined = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::ZERO,
        || super::reader_attach_ready(&session).then_some(()),
    );
    assert!(joined.is_some(), "timed out waiting for the writer to end");
    assert_eq!(observed_drain.demands_for_test(), [0, 0, 0]);
}

#[test]
fn an_abandoned_writer_can_finish_parking_without_claiming_capture() {
    let mut session = crate::stub_session(80);
    let reader = super::ReaderJoin::for_test(|| {});
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || reader.is_finished().then_some(()),
        )
        .is_some()
    );
    session.reader_join = Some(reader);
    let (drain, finish_writer) = crate::temporal::TemporalDrain::held_for_test();
    let observed_drain = drain.clone();
    session.temporal_drain = Some(drain);
    assert!(!park_reader(&mut session, std::time::Instant::now()));
    assert_eq!(observed_drain.demands_for_test(), [1, 0, 0]);
    finish_writer.abandon();
    assert!(super::reader_attach_ready(&session));
    assert!(!observed_drain.capture_complete_for_test());
    assert_eq!(observed_drain.demands_for_test(), [0, 0, 0]);
    assert!(park_reader(&mut session, std::time::Instant::now()));
}

/// The park sleeps on the reader's end, which the reader's return wakes: it looks at the
/// reader once, however long the reader takes to stop.
#[test]
fn a_park_sleeps_until_its_reader_ends_and_never_polls_it() {
    let mut session = crate::stub_session(83);
    let (reader, looks) = super::ReaderJoin::watched_for_test(|looks| {
        // This reader stops only once the park has looked at it, so a park that polls the
        // thread finds it still running and looks again.
        let looked = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || (looks.load(std::sync::atomic::Ordering::Acquire) > 0).then_some(()),
        );
        assert!(looked.is_some(), "timed out waiting for the park to look");
    });
    session.reader_join = Some(reader);

    assert!(park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    ));
    assert!(session.reader_join.is_none(), "the ended reader is joined");
    assert_eq!(
        looks.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the park looked at its reader once, and the reader's end woke it"
    );
}

/// A reader ends only once its body has returned: the park joins a reader it saw end
/// without a deadline, so one still in its body is not ended, and the park keeps its handle.
#[test]
fn a_reader_still_in_its_body_has_not_ended() {
    let mut session = crate::stub_session(84);
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let (finish_reader, reader_held) = std::sync::mpsc::channel::<()>();
    let reader = super::ReaderJoin::for_test(move || {
        entered_tx.send(()).expect("the test waits");
        let _ = reader_held.recv();
    });
    entered
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the reader runs its body");
    assert!(
        !reader.ended_by(std::time::Instant::now()),
        "a reader in its body has not ended"
    );
    session.reader_join = Some(reader);
    assert!(!park_reader(&mut session, std::time::Instant::now()));
    assert!(session.reader_join.is_some(), "the running reader is kept");
    drop(finish_reader);
    assert!(park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    ));
    assert!(session.reader_join.is_none(), "the ended reader is joined");
}

/// A park past its deadline still lets go of what has finished: a reader that has ended and
/// a recording queue that is written.
#[test]
fn a_late_park_lets_go_of_an_ended_reader_and_a_written_queue() {
    let mut session = crate::stub_session(85);
    let reader = super::ReaderJoin::for_test(|| {});
    let ended = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::ZERO,
        || reader.is_finished().then_some(()),
    );
    assert!(ended.is_some(), "timed out waiting for the reader to end");
    session.reader_join = Some(reader);
    let (drain, finish_writer) = crate::temporal::TemporalDrain::held_for_test();
    drop(finish_writer);
    session.temporal_drain = Some(drain);
    assert!(
        park_reader(&mut session, std::time::Instant::now()),
        "nothing unfinished is left to wait for"
    );
    assert!(session.reader_join.is_none(), "the ended reader is joined");
    assert!(
        session.temporal_drain.is_none(),
        "the written queue's drain is let go"
    );
}

/// A park waiting out what its reader's recording queued sleeps until the writer's last
/// write wakes it.
#[test]
fn a_park_waiting_out_a_recording_is_woken_by_its_last_write() {
    let mut session = crate::stub_session(82);
    session.reader_join = Some(super::ReaderJoin::for_test(|| {}));
    let (drain, finish_writer) = crate::temporal::TemporalDrain::held_for_test();
    let observed_drain = drain.clone();
    session.temporal_drain = Some(drain);
    let writer = std::thread::spawn(move || {
        // The writer finishes only once the park sleeps on its queue: a park that polls the
        // queue never sleeps, and one that sleeps is woken only by the write's ring, or else
        // by its deadline.
        let slept = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::ZERO,
            || (finish_writer.sleeps() > 0).then_some(()),
        );
        drop(finish_writer);
        slept.is_some()
    });

    let parked = park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    assert!(
        writer.join().expect("the writer"),
        "timed out waiting for the park to sleep on the queue"
    );
    assert!(parked, "the park waited out the queue");
    assert_eq!(
        observed_drain.slept_out_for_test(),
        0,
        "the writer's last write woke the park, not its deadline"
    );
    assert!(
        session.temporal_drain.is_none(),
        "the written queue's drain is let go"
    );
}

/// An idle reader sits in its poll of the master: the park's wake ends it, and its end wakes
/// the park.
#[cfg(unix)]
#[test]
fn an_idle_reader_is_woken_by_the_park_and_its_end_wakes_the_park() {
    let app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut session = crate::stub_session(81);
    session.master = master;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let post = std::sync::Arc::new(move |wake: crate::Wake| {
        if matches!(wake, crate::Wake::Ready { .. }) {
            let _ = ready_tx.send(());
        }
        true
    });
    super::attach_reader_inner(
        &mut session,
        crate::WindowId(0),
        post,
        &app.session_factory,
        None,
    )
    .expect("attach the real reader");
    ready_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the reader's gather started");
    let looks = session
        .reader_join
        .as_ref()
        .expect("the attached reader")
        .looks_for_test();

    let parked = park_reader(
        &mut session,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    let master = std::mem::replace(&mut session.master, -1);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
    assert!(parked, "the woken reader stopped before the deadline");
    assert!(session.reader_join.is_none(), "the ended reader is joined");
    assert_eq!(
        looks.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the park slept once, until the reader's end woke it"
    );
}
