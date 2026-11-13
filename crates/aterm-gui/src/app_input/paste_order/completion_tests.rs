// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_input_ack::{InputMark, testing};

#[test]
fn scoped_writer_drops_stale_queues_and_finishes_the_started_command_before_ack() {
    use crate::session_timeline::{SupervisorInputDelivery, scoped_input_tests};
    use aterm_agent::harness::contract::Scope;
    use std::io::Read;
    let (handle, mut reader) = testing::owned_handle(74);
    {
        let mut timeline = handle.ctx.timeline.lock().unwrap();
        let at = timeline.slog().now();
        timeline.slog_mut().open_session(None, None, at);
    }
    scoped_input_tests::install(&handle.ctx, 71, &[Scope::Upgrade]);
    let delivery = SupervisorInputDelivery::new(
        &handle.ctx,
        scoped_input_tests::ticket(&handle.ctx, true, Scope::Approvals),
    );
    let stale_delivery = SupervisorInputDelivery::new(
        &handle.ctx,
        scoped_input_tests::ticket(&handle.ctx, true, Scope::Approvals),
    );
    let mark = InputMark::at_epoch(
        &handle.ctx,
        74,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .supervised(delivery.batch());
    let stale_mark = InputMark::at_epoch(
        &handle.ctx,
        74,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .supervised(stale_delivery.batch());
    let budget = Arc::new(Budget::new(LIMITS));
    let job = |byte, completion| {
        let event = InputEvent::KeySequence(vec![byte]);
        let bytes = retained_bytes(&event).unwrap();
        assert!(budget.claim(bytes, false));
        handle.ctx.sink.claim_ordered_egress();
        Job {
            term: handle.term.clone(),
            modes: handle.ctx.modes.clone(),
            sink: handle.ctx.sink.clone(),
            echo: Some(handle.ctx.output_echo.clone()),
            control_vim: None,
            ev: event,
            ticket: None,
            key_ns: crate::metrics::KeyWriteStamp::default(),
            hot_key: None,
            meter: None,
            completion: Some(completion),
            written: None,
            epoch: None,
            _reservation: Reservation {
                budget: budget.clone(),
                sink: handle.ctx.sink.clone(),
                bytes,
                meter: None,
            },
        }
    };
    let queued = job(b'X', stale_mark.completion());
    run_one(job(b'A', mark.completion()));
    assert_eq!(testing::byte(&mut reader), b'A');
    let old_revision = handle
        .ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision();
    let (tx, rx) = std::sync::mpsc::channel();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let transition = {
        let ctx = handle.ctx.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            let claim = scoped_input_tests::install(&ctx, 71, &[Scope::Approvals]);
            tx.send(claim.revision).unwrap();
        })
    };
    barrier.wait();
    let since = std::time::Instant::now();
    while handle
        .ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision()
        == old_revision
    {
        assert!(
            since.elapsed() < Duration::from_secs(60),
            "hang: scope renewal never became visible"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert!(
        rx.try_recv().is_err(),
        "first actual write owns the whole logical command"
    );
    run_one(queued);
    let mut byte = [0];
    assert_eq!(
        reader.read(&mut byte).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "stale queue emitted no byte"
    );
    run_one(job(b'B', mark.completion()));
    assert_eq!(
        testing::byte(&mut reader),
        b'B',
        "the admitted command's suffix is never truncated"
    );
    assert_eq!(
        testing::input_count(&handle),
        1,
        "one logical request, no stale acceptance"
    );
    assert!(rx.try_recv().is_err());
    drop(mark);
    drop(delivery);
    assert!(
        rx.recv_timeout(Duration::from_secs(60))
            .expect("hang: batch never drained")
            > old_revision
    );
    transition.join().unwrap();
    // Negative control: removing the carried custody from this otherwise identical
    // job really emits the stale byte. The model law must reject that projection.
    run_one(job(b'X', stale_mark.supervised(None).completion()));
    assert_eq!(testing::byte(&mut reader), b'X');
    let model = aterm_models_harness::scoped_input_drain_model();
    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    let mut state = mutant.init_state();
    for action in ["Queue", "TransitionQueued", "Stale"] {
        assert!(mutant.fire(action, &mut state));
    }
    assert_eq!(state["stale_byte"], 1);
    assert!(!model.check_invariant("StaleQueueWritesNothing", &state));
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
}

#[test]
fn only_completed_nonempty_jobs_mark_the_exact_admitted_log() {
    let (handle, mut reader) = testing::owned_handle(73);
    {
        let mut timeline = handle.ctx.timeline.lock().unwrap();
        let at = timeline.slog().now();
        timeline.slog_mut().open_session(None, None, at);
    }
    let caller = |word: &str| {
        Some(crate::typed_by::Caller::new(crate::typed_by::Who::Word(
            word.to_owned(),
        )))
    };
    let credited = || handle.ctx.typed.last().map(|t| t.by.words());
    let mark = InputMark::at_epoch(
        &handle.ctx,
        73,
        aterm_slog::Actor::Bridge,
        crate::session_log::InputVerb::Feed,
        None,
        false,
    )
    .typed_by(&handle.ctx, caller("first"));
    let budget = Arc::new(Budget::new(LIMITS));
    let job = |event: InputEvent, completion| {
        let bytes = retained_bytes(&event).expect("tiny job");
        assert!(budget.claim(bytes, false));
        handle.ctx.sink.claim_ordered_egress();
        Job {
            term: handle.term.clone(),
            modes: handle.ctx.modes.clone(),
            sink: handle.ctx.sink.clone(),
            echo: Some(handle.ctx.output_echo.clone()),
            control_vim: None,
            ev: event,
            ticket: None,
            key_ns: crate::metrics::KeyWriteStamp::default(),
            hot_key: None,
            meter: None,
            completion: Some(completion),
            written: None,
            epoch: None,
            _reservation: Reservation {
                budget: budget.clone(),
                sink: handle.ctx.sink.clone(),
                bytes,
                meter: None,
            },
        }
    };
    let (tx, rx) = sync_channel(1);
    assert!(
        tx.try_send(job(InputEvent::KeySequence(vec![b'Q']), mark.completion()))
            .is_ok()
    );
    assert_eq!(
        testing::input_count(&handle),
        0,
        "admission is not acceptance"
    );
    assert_eq!(credited(), None, "queue admission gives no credit");
    run_one(rx.try_recv().expect("owned admitted job"));
    assert_eq!(credited().as_deref(), Some("first"));
    assert_eq!(testing::byte(&mut reader), b'Q');
    assert_eq!(
        testing::input_count(&handle),
        1,
        "ordinary actual completion marks"
    );
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    let cancelled = InputMark::at_epoch(
        &handle.ctx,
        73,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .typed_by(&handle.ctx, caller("cancelled"));
    drop(job(
        InputEvent::KeySequence(vec![b'X']),
        cancelled.completion(),
    ));
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
    assert_eq!(
        credited().as_deref(),
        Some("first"),
        "unrun job kept no credit"
    );
    let refused = InputMark::at_epoch(
        &handle.ctx,
        73,
        aterm_slog::Actor::Ctl,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .typed_by(&handle.ctx, caller("next"));
    for receipt in [
        crate::input::EgressReceipt::deferred_full(),
        crate::input::EgressReceipt::queue_full(),
        crate::input::EgressReceipt::unwritten(crate::input::Egress::Reported(
            crate::input::Delivery::Full,
        )),
    ] {
        refused.completion().finish(receipt);
    }
    run_one(job(
        InputEvent::KeySequence(Vec::new()),
        refused.completion(),
    ));
    assert_eq!(
        testing::input_count(&handle),
        1,
        "empty, deferred, refused and cancelled input is unmarked"
    );
    assert_eq!(
        credited().as_deref(),
        Some("first"),
        "empty/deferred/refused kept no credit"
    );
    run_one(job(
        InputEvent::KeySequence(vec![b'R']),
        refused.completion(),
    ));
    assert_eq!(testing::byte(&mut reader), b'R');
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
    assert_eq!(
        testing::input_count(&handle),
        2,
        "CTL_INPUT_MARK_REQUIRES_COMPLETED_NONEMPTY_RECEIPT"
    );
    assert_eq!(credited().as_deref(), Some("next"));
    assert!(testing::paste(&handle, &mark, "Z"));
    assert_eq!(testing::byte(&mut reader), b'Z');
    assert_eq!(
        credited().as_deref(),
        Some("next"),
        "one mark credits only its first acceptance"
    );
    assert_eq!(testing::input_count(&handle), 2);
    let stale = InputMark::at_epoch(
        &handle.ctx,
        73,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Send,
        None,
        false,
    )
    .typed_by(&handle.ctx, caller("stale"));
    {
        let mut timeline = handle.ctx.timeline.lock().unwrap();
        let old = timeline.slog().log().ep();
        let at = timeline.slog().now();
        timeline.slog_mut().adopt(
            crate::session_log::LogAdopt {
                ids: Some(crate::session_log::LogCarry {
                    ep: old,
                    ids: aterm_slog::IdCounters::default(),
                }),
                seq: 0,
                lines: None,
                keeper_ep: None,
                mirror: None,
            },
            false,
            None,
            at,
        );
        timeline.slog_mut().open_session(None, None, at);
        assert_ne!(timeline.slog().log().ep(), old, "ordinary epoch transition");
    }
    run_one(job(InputEvent::KeySequence(vec![b'E']), stale.completion()));
    assert_eq!(
        testing::byte(&mut reader),
        b'E',
        "acceptance itself is unchanged"
    );
    assert_eq!(
        testing::input_count(&handle),
        0,
        "an old request cannot mark a new log epoch"
    );
    assert_eq!(
        credited().as_deref(),
        Some("next"),
        "old epoch cannot credit a newer one"
    );
    let current = InputMark::at_epoch(
        &handle.ctx,
        73,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Send,
        None,
        false,
    )
    .typed_by(&handle.ctx, caller("current"));
    run_one(job(
        InputEvent::KeySequence(vec![b'N']),
        current.completion(),
    ));
    assert_eq!(testing::byte(&mut reader), b'N');
    assert_eq!(credited().as_deref(), Some("current"));
    assert_eq!(
        testing::input_count(&handle),
        1,
        "a current-epoch receipt remains observable"
    );
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
}
