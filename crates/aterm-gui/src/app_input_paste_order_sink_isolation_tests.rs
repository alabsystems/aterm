// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::paste_order;
use aterm_models_session::paste_order_sink_isolation_model;
#[cfg(unix)]
use aterm_models_session::queued_key_kernel_delivery_model;
use aterm_session::sink::SinkWriter;

#[cfg(unix)]
fn fill_socket_to_backpressure(writer: &mut std::os::unix::net::UnixStream) -> usize {
    use std::io::Write;

    writer.set_nonblocking(true).unwrap();
    let mut wedged = 0usize;
    loop {
        match writer.write(&[b'.'; 4096]) {
            Ok(n) if n > 0 => wedged += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected socket fill result: {other:?}"),
        }
    }
    writer.set_nonblocking(false).unwrap();
    assert!(wedged > 0);
    wedged
}

#[test]
fn a_sink_reads_only_its_own_ordered_input_jobs() {
    let model = paste_order_sink_isolation_model();
    for (a_jobs, b_jobs) in [
        (0usize, 0usize),
        (1, 0),
        (0, 1),
        (1, 1),
        (2, 0),
        (0, 2),
        (2, 1),
        (1, 2),
        (2, 2),
    ] {
        let a = SinkWriter::new(-1);
        let b = SinkWriter::new(-2);
        let mut state = model.init_state();
        for _ in 0..a_jobs {
            a.claim_ordered_egress();
            assert!(model.fire("BeginA", &mut state));
        }
        for _ in 0..b_jobs {
            b.claim_ordered_egress();
            assert!(model.fire("BeginB", &mut state));
        }
        assert_eq!(a.ordered_egress_count() as i64, state["pending_a"]);
        assert_eq!(b.ordered_egress_count() as i64, state["pending_b"]);

        for (action, sink) in [("ReadA", &a), ("ReadB", &b)] {
            let mut read = state.clone();
            assert!(model.fire(action, &mut read));
            assert_eq!(
                i64::from(paste_order::is_ordering(sink)),
                read["decision"],
                "{action} after A={a_jobs}, B={b_jobs}"
            );
        }
        if a_jobs > 0 && b_jobs == 0 {
            assert!(a.ordered_egress_count() + b.ordered_egress_count() > 0);
            assert!(
                !paste_order::is_ordering(&b),
                "global ACTIVE is the caught cross-session mutant"
            );
        }
        for _ in 0..a_jobs {
            a.retire_ordered_egress();
            assert!(model.fire("CompleteA", &mut state));
            let mut read = state.clone();
            assert!(model.fire("ReadA", &mut read));
            assert_eq!(
                i64::from(paste_order::is_ordering(&a)),
                read["decision"],
                "A after a partial drain"
            );
        }
        for _ in 0..b_jobs {
            b.retire_ordered_egress();
            assert!(model.fire("CompleteB", &mut state));
            let mut read = state.clone();
            assert!(model.fire("ReadB", &mut read));
            assert_eq!(
                i64::from(paste_order::is_ordering(&b)),
                read["decision"],
                "B after a partial drain"
            );
        }
        assert_eq!(a.ordered_egress_count() as i64, state["pending_a"]);
        assert_eq!(b.ordered_egress_count() as i64, state["pending_b"]);
    }
}

#[cfg(unix)]
#[test]
fn an_unrelated_key_decides_while_the_cold_registry_is_locked() {
    use std::sync::mpsc;

    let (app_a, a, pipe_a) = super::typed_kitty_summon_tests::app_with_private_pty();
    let (app_b, b, pipe_b) = super::typed_kitty_summon_tests::app_with_private_pty();
    let pin_a = paste_order::pin_ordering_for_test(&a);
    let held = paste_order::hold_registry_for_test();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || tx.send(paste_order::is_ordering(&b)).unwrap());
    let decision = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(held);
    worker.join().expect("ordering decision thread");
    assert!(!decision.expect("an unrelated key never waits for REG"));
    drop(app_a);
    drop(a);
    assert!(
        paste_order::is_master_ordering_for_handoff(pipe_a[1]),
        "the ordered slot still holds the sink after its pool entry is gone"
    );
    drop(pin_a);
    assert!(!paste_order::is_master_ordering_for_handoff(pipe_a[1]));
    drop(app_b);
    unsafe {
        libc::close(pipe_a[0]);
        libc::close(pipe_a[1]);
        libc::close(pipe_b[0]);
        libc::close(pipe_b[1]);
    }
}

/// A sink that outlives its borrowed fd leaves its writer slot under a number the next fd can
/// take: the new sink gets a slot of its own, so the handoff gate reads the new sink's queue.
#[cfg(unix)]
#[test]
fn a_reused_master_number_gates_on_the_new_sink() {
    use std::sync::Arc;

    const MASTER: i32 = -3;
    let stale = Arc::new(SinkWriter::new(MASTER));
    drop(paste_order::pin_ordering_for_test(&stale));
    let fresh = Arc::new(SinkWriter::new(MASTER));
    let pin = paste_order::pin_ordering_for_test(&fresh);
    assert!(
        paste_order::is_master_ordering_for_handoff(MASTER),
        "the gate reads the sink now on the number"
    );
    drop(pin);
    assert!(!paste_order::is_master_ordering_for_handoff(MASTER));
    drop(stale);
}

/// Two real, independent input sinks: A submits a paste and a key to its
/// writer while B submits many keys to its own PTY. A's held ordering pin fixes
/// the interleaving; every B receipt is an inline acceptance, and A's bytes keep
/// their FIFO order.
#[cfg(unix)]
#[test]
fn a_paste_in_one_session_does_not_queue_another_sessions_keys() {
    use super::{InputEvent, Source, WindowId};
    use crate::input::PasteFraming;
    use std::time::Duration;

    fn read_exact_pipe(fd: i32, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count);
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while std::time::Instant::now() < deadline {
            if bytes.len() == count {
                break;
            }
            let mut chunk = [0u8; 256];
            let len = (count - bytes.len()).min(chunk.len());
            let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), len) };
            if n > 0 {
                bytes.extend_from_slice(&chunk[..n as usize]);
            } else if n == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN)
            {
                std::thread::sleep(Duration::from_millis(1));
            } else {
                panic!("pipe read returned {n} after {} bytes", bytes.len());
            }
        }
        assert_eq!(bytes.len(), count, "the writer delivered every byte");
        bytes
    }

    let (mut app_a, sink_a, pipe_a) = super::typed_kitty_summon_tests::app_with_private_pty();
    let (app_b, sink_b, pipe_b) = super::typed_kitty_summon_tests::app_with_private_pty();
    let wid = WindowId(0);
    let pin_a = paste_order::pin_ordering_for_test(&sink_a);
    assert!(paste_order::is_ordering(&sink_a));
    assert!(!paste_order::is_ordering(&sink_b));
    assert!(paste_order::is_master_ordering_for_handoff(pipe_a[1]));
    assert!(!paste_order::is_master_ordering_for_handoff(pipe_b[1]));

    assert_eq!(
        app_a.input(
            wid,
            InputEvent::Paste("P".into(), PasteFraming::AtDrain),
            Source::Human
        ),
        crate::input::InputOutcome::Ok
    );
    let a = app_a.front_terminal_mirror(wid).expect("A terminal");
    let a_session = app_a.pool.get(a.session).expect("A session");
    let (receipt_a, inline_a) = paste_order::ordered_or_inline(
        &a.term,
        &a_session.ctx.modes,
        &a.sink,
        &a_session.ctx.output_echo,
        &InputEvent::Text("a".into()),
        None,
        None,
    );
    assert!(!inline_a, "A's later key must queue behind its paste");
    assert!(matches!(
        receipt_a.egress,
        crate::input::Egress::Reported(_)
    ));

    let b = app_b.front_terminal_mirror(wid).expect("B terminal");
    let b_session = app_b.pool.get(b.session).expect("B session");
    for _ in 0..128 {
        let (receipt, wrote_inline) = paste_order::ordered_or_inline(
            &b.term,
            &b_session.ctx.modes,
            &b.sink,
            &b_session.ctx.output_echo,
            &InputEvent::Text("b".into()),
            None,
            None,
        );
        assert!(wrote_inline, "B's key must never join A's FIFO");
        assert!(
            receipt.accepted_order().is_some(),
            "B has a real PTY receipt"
        );
        assert_eq!(sink_b.ordered_egress_count(), 0);
    }
    assert_eq!(read_exact_pipe(pipe_b[0], 128), vec![b'b'; 128]);
    assert_eq!(read_exact_pipe(pipe_a[0], 2), b"Pa");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        if sink_a.ordered_egress_count() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sink_a.ordered_egress_count(), 1, "only the pin remains");
    drop(pin_a);
    assert_eq!(sink_a.ordered_egress_count(), 0);
    assert!(!paste_order::is_master_ordering_for_handoff(pipe_a[1]));
    drop(a);
    drop(b);
    drop(app_a);
    drop(app_b);
    drop(sink_a);
    drop(sink_b);
    unsafe {
        libc::close(pipe_a[0]);
        libc::close(pipe_a[1]);
        libc::close(pipe_b[0]);
        libc::close(pipe_b[1]);
    }
}

/// Tier-1 conformance: a real wedged sink accepts the whole key into its
/// spill, but the GUI's production decision returns without a wake or a
/// wait. Draining later cannot relabel that old receipt as direct.
#[cfg(unix)]
#[test]
fn spilled_queued_key_never_grants_a_direct_delivery_wake() {
    use crate::input::{Delivery, EgressReceipt};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    let wedged = fill_socket_to_backpressure(&mut writer);

    let write = sink.write_frame_nonparking_with_receipt(b"k").unwrap();
    assert_eq!(write.accepted(), 1);
    assert!(!write.is_direct(), "the wedged key entered the spill");
    let receipt = EgressReceipt::from_reported_delivery(Delivery::Full, write.order());
    let model = queued_key_kernel_delivery_model();
    let mut state = model.init_state();
    assert!(model.fire("AcceptSpill", &mut state));
    assert!(!model.action_enabled("PostWake", &state));

    let failed_prefix = EgressReceipt::from_reported_delivery(Delivery::Failed, write.order());
    assert!(
        paste_order::completed_queued_key_wake(failed_prefix, Some(7)).is_none(),
        "a failed event's earlier accepted prefix cannot mint a key wake"
    );
    assert!(
        paste_order::completed_queued_key_wake(receipt, None).is_none(),
        "a paste receipt cannot mint a key wake"
    );

    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(paste_order::completed_queued_key_wake(receipt, Some(7)))
            .unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(60))
            .expect("spill decision never waits for whole-sink drain")
            .is_none()
    );
    worker.join().unwrap();

    let mut received = Vec::with_capacity(wedged + 1);
    let mut chunk = [0u8; 65536];
    while received.len() < wedged + 1 {
        let n = reader.read(&mut chunk).expect("drain accepted bytes");
        assert!(n > 0, "peer closed before the queued key");
        received.extend_from_slice(&chunk[..n]);
    }
    assert_eq!(received.len(), wedged + 1);
    assert_eq!(received[wedged], b'k');
    assert!(model.fire("DrainSuccess", &mut state));
    assert!(!model.action_enabled("PostWake", &state));
    assert!(paste_order::completed_queued_key_wake(receipt, Some(7)).is_none());
    assert!(model.check_invariant("WakeRequiresDirectReceipt", &state));
}

/// A hot key admitted behind an existing spill must retire its FIFO job
/// immediately. Holding the sole writer on a whole-sink drain would leave
/// both keys outstanding while the peer is wedged.
#[cfg(unix)]
#[test]
fn spilled_key_does_not_hold_later_queued_keys() {
    use super::{InputEvent, WindowId};
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let (app, _app_sink, app_pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
    fill_socket_to_backpressure(&mut writer);
    let seed = sink.write_frame_nonparking_with_receipt(b"s").unwrap();
    assert!(!seed.is_direct(), "seed makes a real undrained spill");

    for key in ["a", "b"] {
        paste_order::enqueue(
            &mirror.term,
            &session.ctx.modes,
            &sink,
            &session.ctx.output_echo,
            InputEvent::Text(key.into()),
            None,
            Some((mirror.session, None)),
        )
        .expect("enqueue key behind the spill");
    }
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while sink.ordered_egress_count() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        sink.ordered_egress_count(),
        0,
        "a spilled hot key cannot hold the sole FIFO writer"
    );
    assert!(
        !sink.egress_drained_to_kernel(),
        "the backlog is still wedged: retirement did not wait for drain"
    );

    drop(reader);
    drop(writer);
    drop(sink);
    drop(mirror);
    drop(app);
    unsafe {
        libc::close(app_pipe[0]);
        libc::close(app_pipe[1]);
    }
}

#[cfg(unix)]
#[test]
fn direct_queued_key_receipt_can_post_delivery_wake() {
    use super::{InputEvent, WindowId};
    use crate::input::EgressMode;

    let (app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    let receipt = crate::input::seam_egress_receipt(
        &mirror.term,
        &session.ctx.modes,
        &sink,
        &InputEvent::Text("d".into()),
        EgressMode::Interactive,
    );
    assert!(receipt.fully_accepted_nonempty());
    assert!(receipt.is_direct(), "the unwedged write reached the kernel");

    let model = queued_key_kernel_delivery_model();
    let mut state = model.init_state();
    assert!(model.fire("AcceptDirect", &mut state));
    assert!(model.action_enabled("PostWake", &state));
    assert!(matches!(
        paste_order::completed_queued_key_wake(receipt, Some(mirror.session)),
        Some(super::Wake::QueuedKeyDelivered { session, .. }) if session == mirror.session
    ));
    assert!(model.fire("PostWake", &mut state));
    assert!(model.check_invariant("WakeRequiresDirectReceipt", &state));

    let mut delivered = [0u8; 1];
    assert_eq!(
        unsafe { libc::read(pipe[0], delivered.as_mut_ptr().cast(), 1) },
        1
    );
    assert_eq!(&delivered, b"d");
    drop(mirror);
    drop(app);
    drop(sink);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}
