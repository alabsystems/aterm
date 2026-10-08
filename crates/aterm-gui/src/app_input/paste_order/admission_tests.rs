// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::derive::ordered_input_admission_model;
use aterm_spec::interp::State;

fn project(budget: &Budget, expected: &State) -> State {
    let mut state = expected.clone();
    let used = budget.used.load(Ordering::Acquire);
    state.insert("bytes", (used & u64::from(u32::MAX)) as i64);
    state.insert("jobs", (used >> 32) as i64);
    state
}

#[test]
fn admission_bounds_and_reserved_key_room_follow_the_model() {
    let model = ordered_input_admission_model();
    let limits = Limits {
        bytes: 5,
        jobs: 3,
        paste_bytes: 4,
        paste_jobs: 2,
    };
    for script in [
        &[
            "AdmitPaste",
            "AdmitPaste",
            "AdmitPaste",
            "AdmitInput",
            "AdmitInput",
            "ReleasePaste",
            "AdmitPaste",
            "ReleaseInput",
            "AdmitPaste",
        ][..],
        &[
            "AdmitTiny",
            "AdmitTiny",
            "AdmitPaste",
            "AdmitTiny",
            "AdmitTiny",
            "ReleaseTiny",
            "AdmitInput",
            "ReleaseTiny",
            "AdmitPaste",
        ][..],
        &[
            "AdmitInput",
            "AdmitInput",
            "AdmitInput",
            "AdmitTiny",
            "ReleaseInput",
            "ReleaseInput",
            "AdmitPaste",
            "ReleasePaste",
            "ReleaseInput",
        ][..],
    ] {
        let budget = Budget::new(limits);
        let mut state = model.init_state();
        for action in script {
            let enabled = model.action_enabled(action, &state);
            match *action {
                "AdmitPaste" | "AdmitInput" | "AdmitTiny" => {
                    let (bytes, paste) = match *action {
                        "AdmitPaste" => (2, true),
                        "AdmitInput" => (1, false),
                        _ => (0, false),
                    };
                    assert_eq!(budget.claim(bytes, paste), enabled, "{action}: {state:?}");
                }
                _ => {
                    assert!(enabled);
                    budget.release(match *action {
                        "ReleasePaste" => 2,
                        "ReleaseInput" => 1,
                        _ => 0,
                    });
                }
            }
            if enabled {
                assert!(model.fire(action, &mut state));
                if *action == "AdmitPaste" {
                    let used = budget.used.load(Ordering::Acquire);
                    assert_eq!(state["paste_bytes"], (used & u64::from(u32::MAX)) as i64);
                    assert_eq!(state["paste_jobs"], (used >> 32) as i64);
                }
            }
            assert_eq!(project(&budget, &state), state, "{action}");
            assert!(model.check_invariant("RetainedWithinBothBounds", &state));
            assert!(model.check_invariant("PasteLeavesInputRoom", &state));
        }
    }
    // Historical unbounded admission accepts a third paste: the real
    // guard declines it while the model's mutant violates both claims.
    let budget = Budget::new(limits);
    assert!(budget.claim(2, true));
    assert!(budget.claim(2, true));
    assert!(!budget.claim(2, true));
    assert!(budget.claim(1, false), "reserved key room remains usable");
    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    let mut state = mutant.init_state();
    for _ in 0..3 {
        assert!(mutant.fire("AdmitPaste", &mut state));
    }
    assert!(!mutant.check_invariant("RetainedWithinBothBounds", &state));
    assert!(!mutant.check_invariant("PasteLeavesInputRoom", &state));
}

#[test]
fn admission_charges_capacity_and_releases_on_unwind() {
    let mut text = String::with_capacity(4096);
    text.push('x');
    let capacity = text.capacity();
    assert_eq!(
        retained_bytes(&InputEvent::Text(text)),
        Some((capacity + std::mem::size_of::<Job>()) as u32)
    );
    let mut bytes = Vec::with_capacity(4096);
    bytes.push(b'x');
    let capacity = bytes.capacity();
    assert_eq!(
        retained_bytes(&InputEvent::KeySequence(bytes)),
        Some((capacity + std::mem::size_of::<Job>()) as u32)
    );
    let budget = Arc::new(Budget::new(LIMITS));
    let sink = Arc::new(SinkWriter::new(-1));
    assert!(budget.claim(123, true));
    sink.claim_ordered_egress();
    let meter = Arc::new(BulkMeter::new());
    let reservation = Reservation {
        budget: Arc::clone(&budget),
        sink: Arc::clone(&sink),
        bytes: 123,
        meter: Some(Arc::clone(&meter)),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _reservation = reservation;
        panic!("owned job unwound");
    }));
    assert!(result.is_err());
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(sink.ordered_egress_count(), 0);
    assert_eq!(
        meter.progress().state,
        aterm_session::sink::BulkState::Failed
    );
}

#[cfg(unix)]
#[test]
fn losing_a_receiver_releases_accepted_payloads_and_finishes_their_meters() {
    use crate::App;
    use crate::input::PasteFraming;
    use std::os::unix::net::UnixStream;

    let (_reader, writer) = UnixStream::pair().unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    let app = App::headless_for_test_with_sink(Arc::clone(&sink));
    let session = app.pool.get(0).unwrap();
    let budget = Arc::new(Budget::new(LIMITS));
    let (tx, rx) = sync_channel(4);
    REG.lock().unwrap_or_else(|p| p.into_inner()).insert(
        sink.master(),
        Serializer {
            tx,
            sink: Arc::downgrade(&sink),
            budget: Arc::clone(&budget),
        },
    );
    let mut meters = Vec::new();
    for end in [None, Some(true), Some(false)] {
        let meter = Arc::new(BulkMeter::new());
        enqueue_metered(
            &session.term,
            &session.ctx.modes,
            &sink,
            &session.ctx.output_echo,
            InputEvent::Paste("owned payload".into(), PasteFraming::AtDrain),
            None,
            None,
            Some(Arc::clone(&meter)),
        )
        .unwrap();
        if let Some(delivered) = end {
            meter.settle_unmetered(delivered);
        }
        meters.push(meter);
    }
    let stopped = Arc::new(BulkMeter::new());
    enqueue_metered(
        &session.term,
        &session.ctx.modes,
        &sink,
        &session.ctx.output_echo,
        InputEvent::Paste("stopped".into(), PasteFraming::AtDrain),
        None,
        None,
        Some(Arc::clone(&stopped)),
    )
    .unwrap();
    stopped.abandon();
    assert_eq!(sink.ordered_egress_count(), 4);
    assert_eq!(
        meters[0].progress().state,
        aterm_session::sink::BulkState::Queued
    );
    drop(rx); // The real channel drops every accepted, unconsumed Job.
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(sink.ordered_egress_count(), 0);
    use aterm_session::sink::BulkState;
    assert_eq!(
        meters
            .iter()
            .map(|m| m.progress().state)
            .collect::<Vec<_>>(),
        vec![BulkState::Failed, BulkState::Delivered, BulkState::Failed]
    );
    assert_eq!(stopped.progress().state, BulkState::Stopped);
    assert!(meters.iter().all(|m| Arc::strong_count(m) == 1));
    assert_eq!(Arc::strong_count(&stopped), 1);
    REG.lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&sink.master());
}

#[cfg(unix)]
#[test]
fn stalled_sink_admission_is_bounded_and_keeps_accepted_fifo_order() {
    stalled_sink_case(false);
}

#[cfg(unix)]
#[test]
fn hidden_paste_admission_returns_without_waiting_for_the_child() {
    stalled_sink_case(true);
}

#[cfg(unix)]
fn stalled_sink_case(hidden: bool) {
    use crate::input::{InputOutcome, PasteFraming, Source};
    use crate::{App, WindowId};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    writer.set_nonblocking(true).unwrap();
    let mut seeded = 0;
    loop {
        match writer.write(&[b'.'; 4096]) {
            Ok(n) if n > 0 => seeded += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("socket fill: {other:?}"),
        }
    }
    writer.set_nonblocking(false).unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    let mut app = App::headless_for_test_with_sink(Arc::clone(&sink));
    let wid = WindowId(0);
    let term = app.pool.get(0).unwrap().term.clone();
    if hidden {
        let id = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(id));
        assert_ne!(app.front_terminal(wid).map(|t| t.session), Some(0));
    }
    let submit = |app: &mut App, ev| {
        if hidden {
            app.input_to_hidden_session(0, ev, super::super::PressPhase::Initial)
        } else {
            app.input(wid, ev, Source::Human)
        }
    };
    let (_, budget) = {
        let mut registry = REG.lock().unwrap_or_else(|p| p.into_inner());
        registry.retain(|_, entry| entry.sink.strong_count() > 0);
        writer_for_limits(
            &mut registry,
            sink.master(),
            &sink,
            Limits {
                bytes: 8 * 1024 * 1024,
                jobs: 3,
                paste_bytes: 7 * 1024 * 1024,
                paste_jobs: 2,
            },
        )
        .unwrap()
    };
    let body = 3 * 1024 * 1024; // Larger than SinkWriter's entire spill cap.
    let mut expected = vec![b'.'; seeded];
    expected.extend_from_slice(b"\x1b[200~");
    expected.resize(expected.len() + body, b'p');
    expected.extend_from_slice(b"\x1b[201~\x1b[200~second\x1b[201~K");
    // A broad watchdog releases the owned socket even if historical
    // synchronous fallback regresses. It is not a latency benchmark.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        let returned_without_drain = ready_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok();
        let mut actual = vec![0; expected.len()];
        reader.read_exact(&mut actual).unwrap();
        (returned_without_drain, actual, expected, reader)
    });
    let paste = |text| InputEvent::Paste(text, PasteFraming::Gesture { bracketed: true });
    assert_eq!(submit(&mut app, paste("p".repeat(body))), InputOutcome::Ok);
    assert_eq!(submit(&mut app, paste("second".into())), InputOutcome::Ok);
    let old_offset = {
        let mut term = term.lock().unwrap();
        term.process("history\r\n".repeat(64).as_bytes());
        term.scroll_to_top();
        let selection = term.text_selection_mut();
        selection.start_selection(
            0,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        selection.update_selection(0, 3, aterm_core::selection::SelectionSide::Right);
        selection.complete_selection();
        term.grid().display_offset()
    };
    assert!(old_offset > 0);
    assert_eq!(
        submit(&mut app, paste("REJECTED".into())),
        InputOutcome::WriteFailed
    );
    {
        let term = term.lock().unwrap();
        assert_eq!(
            term.grid().display_offset(),
            old_offset,
            "rejected paste preserves reading position"
        );
        assert!(term.text_selection().has_selection());
    }
    assert_eq!(
        submit(&mut app, InputEvent::KeySequence(b"K".to_vec())),
        InputOutcome::Ok
    );
    assert_eq!(
        submit(&mut app, InputEvent::KeySequence(b"REJECTED-KEY".to_vec())),
        InputOutcome::WriteFailed
    );
    assert_eq!(budget.used.load(Ordering::Acquire) >> 32, 3);
    assert_eq!(
        app.messages
            .live_rows()
            .filter(|m| m.msg.key.as_deref() == Some("session.input-full.0"))
            .map(|m| m.msg.detail.clone())
            .collect::<Vec<_>>(),
        [vec![
            "earlier input is still being sent \u{2014} try again".to_string()
        ]],
        "one row, naming the cause the queue reported: a full queue, not a size"
    );
    // A paste whose len equals the paste cap still cannot fit an empty
    // queue once its Job is charged: too large, not busy, and the one
    // row's line follows the new cause.
    assert_eq!(
        submit(&mut app, paste("p".repeat(7 * 1024 * 1024))),
        InputOutcome::WriteFailed
    );
    assert_eq!(budget.used.load(Ordering::Acquire) >> 32, 3);
    assert_eq!(
        app.messages
            .live_rows()
            .filter(|m| m.msg.key.as_deref() == Some("session.input-full.0"))
            .map(|m| m.msg.detail.clone())
            .collect::<Vec<_>>(),
        [vec![
            "the paste is too large \u{2014} paste it in smaller pieces".to_string()
        ]],
        "one row, its line replaced by the new cause"
    );
    ready_tx.send(()).unwrap();
    let (responsive, actual, expected, mut reader) = drain.join().unwrap();
    assert!(
        responsive,
        "admission must return while the child is stalled"
    );
    assert_eq!(
        actual, expected,
        "accepted work is FIFO, rejected work writes nothing"
    );
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while sink.ordered_egress_count() != 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(sink.ordered_egress_count(), 0);
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    reader.set_nonblocking(true).unwrap();
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(unix)]
#[test]
fn disconnected_admission_rejects_without_bypass_then_recovers() {
    use crate::input::{InputOutcome, PasteFraming, Source};
    use crate::{App, WindowId};
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    let (mut reader, writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer.into()));
    let mut app = App::headless_for_test_with_sink(Arc::clone(&sink));
    let budget = Arc::new(Budget::new(LIMITS));
    let (tx, rx) = sync_channel(1);
    drop(rx);
    REG.lock().unwrap_or_else(|p| p.into_inner()).insert(
        sink.master(),
        Serializer {
            tx,
            sink: Arc::downgrade(&sink),
            budget: Arc::clone(&budget),
        },
    );
    let paste =
        |text: &str| InputEvent::Paste(text.into(), PasteFraming::Gesture { bracketed: false });
    assert_eq!(
        app.input(WindowId(0), paste("rejected"), Source::Human),
        InputOutcome::WriteFailed
    );
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
    assert_eq!(sink.ordered_egress_count(), 0);
    assert!(
        !REG.lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&sink.master())
    );
    reader.set_nonblocking(true).unwrap();
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    reader.set_nonblocking(false).unwrap();
    assert_eq!(
        app.input(WindowId(0), paste("retry"), Source::Human),
        InputOutcome::Ok
    );
    let mut actual = [0; 5];
    reader.read_exact(&mut actual).unwrap();
    assert_eq!(&actual, b"retry");
}
