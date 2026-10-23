// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::keyboard::{Key as TKey, KeyEventType, Modifiers as TMods, NamedKey as TNamed};

#[cfg(unix)]
fn model_state(
    tracker: &OutputEchoTracker,
    older: AcceptedOrder,
    newer: AcceptedOrder,
    accepted: i64,
    older_done: bool,
    newer_done: bool,
    sample: Option<OutputEchoSample>,
) -> aterm_spec::interp::State {
    let published = tracker
        .published
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let published_order = match published.order {
        None => 0,
        Some(order) if order == older => 1,
        Some(order) if order == newer => 2,
        Some(order) => panic!("unexpected receipt order in tracker projection: {order:?}"),
    };
    let (sampled, sample_order, sample_accepted, sample_boundary) =
        sample.map_or((0, 0, 0, 0), |sample| {
            (
                1,
                published_order,
                i64::from(sample.last_accepted_at.is_some()),
                i64::from(sample.last_boundary_at.is_some()),
            )
        });
    [
        ("accepted", accepted),
        ("older_done", i64::from(older_done)),
        ("newer_done", i64::from(newer_done)),
        ("published_order", published_order),
        (
            "accepted_shadow",
            i64::from(published.last_accepted_us != 0),
        ),
        (
            "boundary_shadow",
            i64::from(published.last_boundary_us != 0),
        ),
        ("sampled", sampled),
        ("sample_order", sample_order),
        ("sample_accepted", sample_accepted),
        ("sample_boundary", sample_boundary),
    ]
    .into_iter()
    .collect()
}

#[cfg(unix)]
fn require_model_transition(
    action: &str,
    before: &aterm_spec::interp::State,
    after: &aterm_spec::interp::State,
) {
    let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
        &aterm_models_effects::output_echo_receipt_publication_model(),
        &[],
        before,
        after,
        Some(action),
        "OutputEchoTracker acceptance-order conformance",
    );
    assert!(ok, "real {action} transition rejected:\n{evidence}");
}

fn enter(mods: TMods, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: TKey::Named(TNamed::Enter),
        mods,
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

#[cfg(unix)]
#[test]
fn a_receipt_is_never_reported_before_the_dispatch_it_receipts() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let now = std::time::Instant::now();
    let dispatched_at = now + std::time::Duration::from_millis(50);
    let ev = InputEvent::KeySequence(b"x".to_vec());
    let write = tracker.begin_ticketed(
        OutputEchoInput::Echoable,
        Some(DeliveryTicket {
            insert: Some(InsertWidth::Cells(1)),
            window: None,
            key: None,
            dispatched_at,
        }),
    );
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    write.finish(receipt, &sink);

    let batch = tracker.deliveries_after(&sink, Some(0), now);
    let (at, ticket) = batch
        .items
        .iter()
        .flatten()
        .copied()
        .next()
        .expect("the ticketed write published a receipt");
    assert_eq!(ticket.dispatched_at, dispatched_at);
    assert!(
        at >= dispatched_at,
        "the receipt was reported {:?} BEFORE the dispatch it receipts",
        dispatched_at.saturating_duration_since(at)
    );

    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn a_ticketed_write_publishes_a_delivery_receipt_the_prelude_reads_once() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let now = std::time::Instant::now();
    let ev = InputEvent::KeySequence(b"x".to_vec());
    let ticket = |cells: u16| DeliveryTicket {
        insert: Some(InsertWidth::Cells(cells)),
        window: None,
        key: None,
        dispatched_at: now,
    };

    let fresh = tracker.deliveries_after(&sink, None, now);
    assert_eq!(fresh.latest, 0);
    assert!(fresh.items.iter().all(Option::is_none));

    let first = tracker.begin_ticketed(OutputEchoInput::Echoable, Some(ticket(63)));
    let first_receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    let second = tracker.begin_ticketed(OutputEchoInput::Echoable, Some(ticket(41)));
    let second_receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    second.finish(second_receipt, &sink);
    first.finish(first_receipt, &sink);
    let batch = tracker.deliveries_after(&sink, Some(0), now);
    assert_eq!(batch.latest, 2, "two receipts, two serials");
    let items: Vec<_> = batch.items.iter().flatten().collect();
    assert_eq!(
        items.iter().map(|(_, t)| t.insert).collect::<Vec<_>>(),
        vec![Some(InsertWidth::Cells(41)), Some(InsertWidth::Cells(63))],
        "both receipts, completion order — neither overwrites the other"
    );
    assert!(
        items
            .iter()
            .all(|(at, _)| now.saturating_duration_since(*at).as_millis() < 1_000),
        "the delivery instant is placed on the frame's clock"
    );
    let again = tracker.deliveries_after(&sink, Some(batch.latest), now);
    assert_eq!(again.latest, 2);
    assert!(again.items.iter().all(Option::is_none));

    let typed = tracker.begin_ticketed(
        OutputEchoInput::Echoable,
        Some(DeliveryTicket {
            insert: None,
            window: Some(WindowId(0)),
            key: Some(DeliveredClass::Typed),
            dispatched_at: now,
        }),
    );
    let typed_receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    typed.finish(typed_receipt, &sink);
    let batch = tracker.deliveries_after(&sink, Some(2), now);
    assert_eq!(batch.latest, 3);
    assert_eq!(
        batch.items[0].map(|(_, t)| t),
        Some(DeliveryTicket {
            insert: None,
            window: Some(WindowId(0)),
            key: Some(DeliveredClass::Typed),
            dispatched_at: now,
        })
    );

    let empty = tracker.begin_ticketed(
        OutputEchoInput::Echoable,
        Some(DeliveryTicket {
            insert: None,
            window: None,
            key: None,
            dispatched_at: now,
        }),
    );
    let r = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    empty.finish(r, &sink);
    let plain = tracker.begin_event(&ev);
    let r = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    plain.finish(r, &sink);
    assert_eq!(tracker.deliveries_after(&sink, Some(3), now).latest, 3);

    let dead = SinkWriter::new(-1);
    let failed = tracker.begin_ticketed(OutputEchoInput::Echoable, Some(ticket(8)));
    let r = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &dead,
        &ev,
        input::EgressMode::Interactive,
    );
    failed.finish(r, &dead);
    let batch = tracker.deliveries_after(&sink, Some(3), now);
    assert!(
        batch.items.iter().all(Option::is_none),
        "a write that did not land is not a delivery"
    );
    assert_eq!(
        batch.failed.iter().flatten().copied().collect::<Vec<_>>(),
        vec![ticket(8)],
        "…it is the one failed receipt"
    );
    assert_eq!(batch.latest, 4, "read once, like any receipt");
    let again = tracker.deliveries_after(&sink, Some(batch.latest), now);
    assert!(again.failed.iter().all(Option::is_none), "and only once");
    let plain = tracker.begin_event(&ev);
    let r = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &dead,
        &ev,
        input::EgressMode::Interactive,
    );
    plain.finish(r, &dead);
    assert_eq!(tracker.deliveries_after(&sink, Some(4), now).latest, 4);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn the_delivery_ring_holds_a_frames_worth_of_receipts() {
    use aterm_effects::cursor_glow::InsertWidth;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let now = std::time::Instant::now();
    let ev = InputEvent::KeySequence(b"x".to_vec());
    let publish = |cells: u16| {
        let write = tracker.begin_ticketed(
            OutputEchoInput::Echoable,
            Some(DeliveryTicket {
                insert: Some(InsertWidth::Cells(cells)),
                window: None,
                key: None,
                dispatched_at: now,
            }),
        );
        let receipt = input::seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &ev,
            input::EgressMode::Interactive,
        );
        write.finish(receipt, &sink);
    };
    for cells in 1..=9u16 {
        publish(cells);
    }
    let batch = tracker.deliveries_after(&sink, Some(0), now);
    assert_eq!((batch.latest, batch.evicted), (9, 0));
    let cells: Vec<InsertWidth> = batch
        .items
        .iter()
        .flatten()
        .filter_map(|(_, t)| t.insert)
        .collect();
    assert_eq!(
        cells,
        (1..=9).map(InsertWidth::Cells).collect::<Vec<_>>(),
        "every receipt of the frame is handed over, oldest first"
    );

    let depth = DELIVERY_RING as u16;
    for cells in 10..=(9 + depth + 1) {
        publish(cells);
    }
    let batch = tracker.deliveries_after(&sink, Some(9), now);
    assert_eq!(batch.latest, u64::from(9 + depth + 1));
    assert_eq!(
        batch.evicted, 1,
        "one receipt past the depth is lost, loudly"
    );
    assert_eq!(
        batch.items[0].map(|(_, t)| t.insert),
        Some(Some(InsertWidth::Cells(11))),
        "the oldest survivor is the second"
    );
    assert_eq!(batch.items.iter().flatten().count(), DELIVERY_RING);
    assert_eq!(
        tracker
            .deliveries_after(&sink, Some(batch.latest), now)
            .evicted,
        0,
        "a reader that kept up has lost nothing"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn an_eviction_is_reported_once_on_the_read_that_advances_past_it() {
    use aterm_effects::cursor_glow::InsertWidth;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking pipe writer");
    aterm_pty::set_nonblocking(pipe[0], true).expect("nonblocking pipe reader");
    let mut filled = 0_usize;
    loop {
        match aterm_pty::write_some(pipe[1], &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    assert!(filled > 0, "pipe must accept bytes before EAGAIN");
    let sink = SinkWriter::new(pipe[1]);
    sink.note_master_nonblocking(true);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let now = std::time::Instant::now();
    let ev = InputEvent::KeySequence(b"x".to_vec());

    let seed = tracker.begin_event(&ev);
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    seed.finish(receipt, &sink);
    assert!(!sink.egress_drained_to_kernel(), "the sink is wedged");

    let depth = DELIVERY_RING as u16;
    for cells in 1..=(depth + 1) {
        let write = tracker.begin_ticketed(
            OutputEchoInput::Echoable,
            Some(DeliveryTicket {
                insert: Some(InsertWidth::Cells(cells)),
                window: None,
                key: None,
                dispatched_at: now,
            }),
        );
        let receipt = input::seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &ev,
            input::EgressMode::Backpressured,
        );
        write.finish(receipt, &sink);
    }
    for _ in 0..2 {
        let held = tracker.deliveries_after(&sink, Some(0), now);
        assert!(held.items.iter().all(Option::is_none), "nothing resolved");
        assert_eq!(
            (held.latest, held.evicted),
            (0, 0),
            "a read that cannot advance reports no loss — it will be \
             reported by the read that does"
        );
    }

    // SAFETY: `pipe[0]` is ours and open; the `File` owns and closes it.
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let mut buf = vec![0_u8; 64 * 1024];
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        if sink.egress_drained_to_kernel() {
            break;
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("pipe read: {error}"),
        }
    }
    assert!(sink.egress_drained_to_kernel(), "the spill drained");

    let mut resolved = tracker.deliveries_after(&sink, Some(0), now);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        if resolved.latest > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        resolved = tracker.deliveries_after(&sink, Some(0), now);
    }
    assert_eq!(
        (resolved.latest, resolved.evicted),
        (u64::from(depth + 1), 1),
        "the loss is reported on the read that advances past it"
    );
    assert_eq!(
        resolved.items[0].map(|(_, t)| t.insert),
        Some(Some(InsertWidth::Cells(2))),
        "the oldest survivor is the second"
    );
    assert_eq!(resolved.items.iter().flatten().count(), DELIVERY_RING);
    let after = tracker.deliveries_after(&sink, Some(resolved.latest), now);
    assert_eq!(
        (after.latest, after.evicted),
        (u64::from(depth + 1), 0),
        "a reader that kept up has lost nothing"
    );
    drop(sink);
    unsafe {
        libc::close(pipe[1]);
    }
}

#[test]
fn only_unmodified_enter_press_or_repeat_is_a_turn_boundary() {
    assert_eq!(
        OutputEchoInput::of(&enter(TMods::empty(), KeyEventType::Press)),
        OutputEchoInput::TurnBoundary
    );
    assert_eq!(
        OutputEchoInput::of(&enter(TMods::empty(), KeyEventType::Repeat)),
        OutputEchoInput::TurnBoundary
    );
    assert_eq!(
        OutputEchoInput::of(&enter(TMods::empty(), KeyEventType::Release)),
        OutputEchoInput::Echoable,
        "a Kitty key-up cannot clear accepted-input evidence"
    );
    for mods in [
        TMods::SHIFT,
        TMods::CTRL,
        TMods::ALT,
        TMods::SUPER,
        TMods::SHIFT | TMods::CTRL,
    ] {
        assert_eq!(
            OutputEchoInput::of(&enter(mods, KeyEventType::Press)),
            OutputEchoInput::Echoable,
            "modified Enter {mods:?} stays an application key: it belongs to the \
             foreground editor, never an agent turn"
        );
    }
    assert_eq!(
        OutputEchoInput::of(&InputEvent::Focus(true)),
        OutputEchoInput::Ignored,
        "terminal protocol reports are not editor echo"
    );
    assert_eq!(
        OutputEchoInput::of(&InputEvent::KeySequence(b"echo hi\r".to_vec())),
        OutputEchoInput::TurnBoundary
    );
    assert_eq!(
        OutputEchoInput::of(&InputEvent::KeySequence(b"echo hi".to_vec())),
        OutputEchoInput::Echoable
    );
}

#[test]
fn active_write_is_raii_hot_and_a_failed_write_stamps_nothing() {
    let tracker = Arc::new(OutputEchoTracker::default());
    let sink = SinkWriter::new(-1);
    let now = std::time::Instant::now();
    let write = tracker.begin(OutputEchoInput::Echoable);
    assert!(tracker.sample(&sink, now).input_hot);
    drop(write);
    assert!(!tracker.sample(&sink, now).input_hot);

    let term = Mutex::new(Terminal::new(24, 80));
    let ev = InputEvent::KeySequence(b"x".to_vec());
    let write = tracker.begin(OutputEchoInput::of(&ev));
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &ev,
        input::EgressMode::Interactive,
    );
    write.finish(receipt, &sink);
    assert!(tracker.sample(&sink, now).last_accepted_at.is_none());
}

#[cfg(unix)]
#[test]
fn accepted_order_rejects_an_older_completion_after_enter() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();

    let echo_event = InputEvent::KeySequence(b"x".to_vec());
    let echo_write = tracker.begin_event(&echo_event);
    let echo_receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &echo_event,
        input::EgressMode::Interactive,
    );
    let boundary_event = enter(TMods::empty(), KeyEventType::Press);
    let boundary_write = tracker.begin_event(&boundary_event);
    let boundary_receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &boundary_event,
        input::EgressMode::Interactive,
    );
    let echo_order = echo_receipt.accepted_order().expect("echo accepted order");
    let boundary_order = boundary_receipt
        .accepted_order()
        .expect("boundary accepted order");
    assert!(echo_order < boundary_order);

    let initial = model_state(&tracker, echo_order, boundary_order, 0, false, false, None);
    assert_eq!(
        initial,
        aterm_models_effects::output_echo_receipt_publication_model().init_state()
    );
    let echo_accepted = model_state(&tracker, echo_order, boundary_order, 1, false, false, None);
    require_model_transition("AcceptOlderEcho", &initial, &echo_accepted);
    let both_accepted = model_state(&tracker, echo_order, boundary_order, 2, false, false, None);
    require_model_transition("AcceptNewerBoundary", &echo_accepted, &both_accepted);

    boundary_write.finish(boundary_receipt, &sink);
    let boundary_published =
        model_state(&tracker, echo_order, boundary_order, 2, false, true, None);
    require_model_transition("PublishNewerBoundary", &both_accepted, &boundary_published);
    echo_write.finish(echo_receipt, &sink);
    let stale_discarded = model_state(&tracker, echo_order, boundary_order, 2, true, true, None);
    require_model_transition("PublishOlderEcho", &boundary_published, &stale_discarded);
    let sample = tracker.sample(&sink, std::time::Instant::now());
    assert!(sample.last_boundary_at.is_some());
    assert!(sample.last_accepted_at.is_none());
    assert!(!sample.input_hot);
    let sampled = model_state(
        &tracker,
        echo_order,
        boundary_order,
        2,
        true,
        true,
        Some(sample),
    );
    require_model_transition("Sample", &stale_discarded, &sampled);

    let mut overwritten = boundary_published.clone();
    overwritten.insert("older_done", 1);
    overwritten.insert("published_order", 1);
    overwritten.insert("accepted_shadow", 1);
    let model = aterm_models_effects::output_echo_receipt_publication_model();
    let (healthy_admits, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &boundary_published,
        &overwritten,
        Some("PublishOlderEcho"),
        "OutputEchoTracker stale-publication negative control",
    );
    assert!(!healthy_admits, "healthy model admitted stale overwrite");
    let (buggy_admits, evidence) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &boundary_published,
        &overwritten,
        Some("PublishOlderEcho"),
        "OutputEchoTracker stale-publication mutant",
    );
    assert!(
        buggy_admits,
        "mutant did not admit stale overwrite: {evidence}"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(!buggy.check_invariant("BoundaryRetiresOlderEcho", &overwritten));

    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[test]
fn contended_publication_sampling_is_nonparking_and_conservative() {
    let tracker = OutputEchoTracker::default();
    let sink = SinkWriter::new(-1);
    let _publication = tracker
        .published
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let sample = tracker.sample(&sink, std::time::Instant::now());
    assert!(sample.input_hot);
    assert!(sample.last_accepted_at.is_none());
    assert!(sample.last_boundary_at.is_none());
}

#[cfg(unix)]
#[test]
fn accepted_editor_input_discounts_but_enter_opens_the_turn_immediately() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = Arc::new(OutputEchoTracker::default());
    let now = std::time::Instant::now();
    let mut streak = aterm_effects::output_streak::OutputStreak::new(1);
    assert!(!streak.note_output(1, &[(2, 0, 0)], now, false));

    term_lock(&term).process(b"\x1b[?1004h");
    let focus = InputEvent::Focus(true);
    tracked_egress(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &tracker,
        &focus,
        input::EgressMode::Backpressured,
    );
    let protocol = tracker.sample(&sink, now);
    assert!(protocol.last_accepted_at.is_none());
    assert!(protocol.last_boundary_at.is_none());
    assert!(!protocol.input_hot, "focus reports cannot shadow output");

    let typed = InputEvent::KeySequence(b"x".to_vec());
    let write = tracker.begin(OutputEchoInput::of(&typed));
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &typed,
        input::EgressMode::Interactive,
    );
    write.finish(receipt, &sink);
    let now = std::time::Instant::now();
    let echo = tracker.sample(&sink, now);
    streak.note_keystroke(echo.last_accepted_at.expect("accepted editor input"));
    assert!(!streak.note_output(2, &[(2, 0, 0)], now, echo.input_hot));

    let paste = InputEvent::Paste("paste".into(), input::PasteFraming::AtDrain);
    let write = tracker.begin(OutputEchoInput::of(&paste));
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &paste,
        input::EgressMode::Backpressured,
    );
    write.finish(receipt, &sink);
    let now = std::time::Instant::now();
    let paste_echo = tracker.sample(&sink, now);
    streak.note_keystroke(paste_echo.last_accepted_at.expect("accepted paste receipt"));
    assert!(!streak.note_output(3, &[(2, 0, 0)], now, paste_echo.input_hot));

    let boundary = enter(TMods::empty(), KeyEventType::Press);
    let write = tracker.begin(OutputEchoInput::of(&boundary));
    let receipt = input::seam_egress_receipt(
        &term,
        &crate::mode_mirror_of(&term),
        &sink,
        &boundary,
        input::EgressMode::Interactive,
    );
    write.finish(receipt, &sink);
    let now = std::time::Instant::now();
    let turn = tracker.sample(&sink, now);
    assert!(turn.last_accepted_at.is_none());
    assert!(!turn.input_hot);
    streak.note_turn_boundary(turn.last_boundary_at.expect("accepted Enter"));
    assert!(
        streak.note_output(4, &[(3, 0, 0)], now, turn.input_hot),
        "the command response immediately after Enter is authored output"
    );

    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn a_turn_boundary_sampled_late_in_its_frame_still_retires_the_paste_before_it() {
    use std::time::{Duration, Instant};
    /// The frame's render work between reading its clock and sampling.
    const PRELUDE: Duration = Duration::from_millis(20);
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let write = |ev: &InputEvent, mode: input::EgressMode| {
        let write = tracker.begin_event(ev);
        let receipt =
            input::seam_egress_receipt(&term, &crate::mode_mirror_of(&term), &sink, ev, mode);
        write.finish(receipt, &sink);
    };
    let mut streak = aterm_effects::output_streak::OutputStreak::new(1);
    assert!(!streak.note_output(1, &[(2, 0, 0)], Instant::now(), false));

    write(
        &InputEvent::Paste("paste".into(), input::PasteFraming::AtDrain),
        input::EgressMode::Backpressured,
    );
    let frame_n = Instant::now();
    let paste = tracker.sample(&sink, frame_n);
    let paste_at = paste.last_accepted_at.expect("accepted paste receipt");
    streak.note_keystroke(paste_at);
    assert!(!streak.note_output(2, &[(2, 0, 0)], frame_n, paste.input_hot));

    write(
        &enter(TMods::empty(), KeyEventType::Press),
        input::EgressMode::Interactive,
    );
    let frame_n1 = Instant::now();
    std::thread::sleep(PRELUDE);
    let turn = tracker.sample(&sink, frame_n1);
    let boundary_at = turn.last_boundary_at.expect("accepted Enter");
    assert!(
        boundary_at >= paste_at,
        "the Enter was placed {:?} BEFORE the paste it followed",
        paste_at.saturating_duration_since(boundary_at)
    );
    assert!(
        boundary_at <= frame_n1,
        "never later than the frame's clock"
    );
    streak.note_turn_boundary(boundary_at);
    assert!(
        streak.note_output(3, &[(3, 0, 0)], frame_n1, turn.input_hot),
        "the command response after Enter is authored output"
    );

    let mut spent = None;
    assert!(crate::app_render::verdict_armed(
        &mut spent,
        1,
        Some(boundary_at)
    ));
    let frame_n2 = Instant::now();
    std::thread::sleep(PRELUDE / 2);
    let replay = tracker.sample(&sink, frame_n2);
    assert_eq!(
        replay.last_boundary_at,
        Some(boundary_at),
        "a re-sampled boundary keeps its instant"
    );
    assert!(
        !crate::app_render::verdict_armed(&mut spent, 1, replay.last_boundary_at),
        "a later `D` with no new Enter behind it is silent"
    );

    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// A pipe whose write end is wedged.
#[cfg(unix)]
fn wedged_pipe() -> [i32; 2] {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking pipe writer");
    aterm_pty::set_nonblocking(pipe[0], true).expect("nonblocking pipe reader");
    fill_pipe(pipe[1]);
    pipe
}

#[cfg(unix)]
fn fill_pipe(writer: i32) {
    let mut filled = 0_usize;
    loop {
        match aterm_pty::write_some(writer, &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    assert!(filled > 0);
}

/// Read the pipe out until the sink reports its spill drained.
#[cfg(unix)]
fn drain_pipe(reader: &mut std::fs::File, sink: &SinkWriter) {
    use std::io::Read;
    let mut buf = vec![0_u8; 64 * 1024];
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        if sink.egress_drained_to_kernel() {
            break;
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("pipe read: {error}"),
        }
    }
    assert!(sink.egress_drained_to_kernel(), "the spill drained");
}

#[cfg(unix)]
#[test]
fn a_drained_publish_settles_every_receipt_the_spill_had_held() {
    use std::os::fd::FromRawFd;
    for rewedge in [false, true] {
        let pipe = wedged_pipe();
        let sink = SinkWriter::new(pipe[1]);
        sink.note_master_nonblocking(true);
        let term = Mutex::new(Terminal::new(24, 80));
        let tracker = OutputEchoTracker::default();
        let ev = InputEvent::KeySequence(b"x".to_vec());
        let publish = |cells: u16| {
            let write = tracker.begin_ticketed(
                OutputEchoInput::Echoable,
                Some(DeliveryTicket {
                    insert: Some(InsertWidth::Cells(cells)),
                    window: None,
                    key: None,
                    dispatched_at: std::time::Instant::now(),
                }),
            );
            let receipt = input::seam_egress_receipt(
                &term,
                &crate::mode_mirror_of(&term),
                &sink,
                &ev,
                input::EgressMode::Backpressured,
            );
            write.finish(receipt, &sink);
        };
        let seed = tracker.begin_event(&ev);
        let receipt = input::seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &ev,
            input::EgressMode::Interactive,
        );
        seed.finish(receipt, &sink);
        assert!(!sink.egress_drained_to_kernel(), "wedged");
        publish(1);
        {
            let p = tracker.published.lock().unwrap();
            assert_eq!(p.deliveries[0].unwrap().delivered_us, 0, "A is spill-held");
        }
        let mut reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
        drain_pipe(&mut reader, &sink);
        std::thread::sleep(std::time::Duration::from_millis(30));
        publish(2);
        {
            let p = tracker.published.lock().unwrap();
            let a = p.deliveries[0].unwrap();
            let b = p.deliveries[1].unwrap();
            assert_ne!(b.delivered_us, 0, "B is stamped at publish");
            assert_eq!(
                a.delivered_us, b.delivered_us,
                "rewedge={rewedge}: A reached the kernel no later than B: settled at B's stamp"
            );
        }
        if rewedge {
            fill_pipe(pipe[1]);
            let seed = tracker.begin_event(&ev);
            let receipt = input::seam_egress_receipt(
                &term,
                &crate::mode_mirror_of(&term),
                &sink,
                &ev,
                input::EgressMode::Interactive,
            );
            seed.finish(receipt, &sink);
            assert!(!sink.egress_drained_to_kernel(), "re-wedged");
        }
        let now = std::time::Instant::now();
        let batch = tracker.deliveries_after(&sink, Some(0), now);
        assert_eq!(
            batch.latest, 2,
            "rewedge={rewedge}: both receipts handed over"
        );
        let items: Vec<_> = batch.items.iter().flatten().copied().collect();
        assert_eq!(items.len(), 2);
        let (a_at, a_t) = items[0];
        let (b_at, b_t) = items[1];
        assert_eq!(a_t.insert, Some(InsertWidth::Cells(1)));
        assert_eq!(b_t.insert, Some(InsertWidth::Cells(2)));
        assert!(
            a_at <= b_at,
            "rewedge={rewedge}: instants monotone in serial order (A {:?} before B {:?})",
            now.saturating_duration_since(a_at),
            now.saturating_duration_since(b_at)
        );
        drop(reader);
        drop(sink);
        unsafe {
            libc::close(pipe[1]);
        }
    }
}

#[cfg(unix)]
#[test]
fn a_contended_spill_probe_never_holds_a_receipt_on_a_drained_sink() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let tracker = OutputEchoTracker::default();
    let ev = InputEvent::KeySequence(b"x".to_vec());
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let contender = &sink;
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                let _ = contender.egress_drained_to_kernel();
            }
        });
        let mut held = 0_u32;
        for _ in 0..400 {
            let write = tracker.begin_ticketed(
                OutputEchoInput::Echoable,
                Some(DeliveryTicket {
                    insert: None,
                    window: Some(WindowId(0)),
                    key: Some(DeliveredClass::Typed),
                    dispatched_at: std::time::Instant::now(),
                }),
            );
            let receipt = input::seam_egress_receipt(
                &term,
                &crate::mode_mirror_of(&term),
                &sink,
                &ev,
                input::EgressMode::Interactive,
            );
            write.finish(receipt, &sink);
            let p = tracker.published.lock().unwrap();
            let head = (p.delivery_head + DELIVERY_RING - 1) % DELIVERY_RING;
            if p.deliveries[head].is_some_and(|r| r.delivered_us == 0) {
                held += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        assert_eq!(
            held, 0,
            "receipts held on a drained sink by a contended probe"
        );
    });
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
    }
}

#[test]
fn fifo_admission_receipt_is_not_pty_acceptance() {
    let receipt = input::EgressReceipt::deferred_full();
    assert_eq!(
        receipt.egress,
        input::Egress::Reported(input::Delivery::Full),
        "FIFO admission retains the caller's ordered-delivery contract"
    );
    assert!(
        !receipt.accepted_nonempty(),
        "only the writer thread's real seam may stamp echo evidence"
    );
}

#[cfg(unix)]
#[test]
fn an_accepted_enter_over_composed_input_counts_one_submission() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    let sink = SinkWriter::new(pipe[1]);
    let term = Mutex::new(Terminal::new(24, 80));
    let modes = crate::mode_mirror_of(&term);
    let tracker = OutputEchoTracker::default();
    let write = |ev: &InputEvent| {
        tracked_egress(
            &term,
            &modes,
            &sink,
            &tracker,
            ev,
            input::EgressMode::Interactive,
        )
    };
    let plain = enter(TMods::empty(), KeyEventType::Press);
    write(&plain);
    assert_eq!(tracker.submits(), 0);
    write(&InputEvent::KeySequence(b"keep going".to_vec()));
    assert_eq!(tracker.submits(), 0, "a draft is no submission");
    tracker
        .begin_event(&plain)
        .finish_delivery(input::Delivery::BusyZero, None, &sink);
    assert_eq!(tracker.submits(), 0, "a refused write moved nothing");
    let fenced = tracker.begin_event(&plain);
    let receipt =
        input::seam_egress_receipt(&term, &modes, &sink, &plain, input::EgressMode::Interactive);
    fenced.finish_delivery(input::Delivery::Full, receipt.accepted_order(), &sink);
    assert_eq!(tracker.submits(), 1);
    write(&plain);
    assert_eq!(tracker.submits(), 1);
    write(&InputEvent::Key {
        key: TKey::Character('y'),
        mods: TMods::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    });
    write(&plain);
    assert_eq!(tracker.submits(), 2);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
    }
}
