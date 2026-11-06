// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::sink::{Landing, SinkWriter};
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(60);

#[test]
fn kernel_prefix_state_conforms_including_later_reply_failure_and_cancel() {
    let model = aterm_models_session::kernel_prefix_model();
    for cancel in [false, true] {
        let sink = SinkWriter::new(-1);
        let mut frame = Some(AcceptedFrame::new(&sink.shared, AcceptedOrder(1), true));
        let prefix = frame.as_ref().unwrap().prefix.clone().unwrap();
        let mut state = model.init_state();
        let mut delivered = false;
        let mut schedule = vec!["Admit", "QueueReply", "Seal"];
        if cancel {
            schedule.push("CancelOld");
        } else {
            schedule.extend(["DrainOld", "FailReply"]);
        }
        for action in schedule {
            assert!(model.fire(action, &mut state));
            match action {
                "Admit" => sink.shared.spill.lock().unwrap().push_back(
                    b"PP",
                    Landing::Driven,
                    Some(&prefix),
                ),
                "QueueReply" => {
                    sink.shared
                        .spill
                        .lock()
                        .unwrap()
                        .push_back(b"RRR", Landing::Answer, None)
                }
                "Seal" => drop(frame.take()),
                "DrainOld" => {
                    let mut spill = sink.shared.spill.lock().unwrap();
                    prefix.delivered(2);
                    spill.pop_front(2);
                    delivered = true;
                }
                "FailReply" => sink
                    .shared
                    .fail_spill(&mut sink.shared.spill.lock().unwrap()),
                "CancelOld" => {
                    let mut spill = sink.shared.spill.lock().unwrap();
                    // Cancel just the owned run; a later reply still has no prefix owner.
                    prefix.cancelled(2);
                    spill.pop_front(2);
                }
                _ => unreachable!(),
            }
            let progress = prefix.progress.state.lock().unwrap();
            let done = progress.settled() && !progress.failed;
            for (key, value) in [
                ("old", progress.pending as i64),
                ("sealed", i64::from(progress.sealed)),
                ("failed", i64::from(progress.failed)),
                ("done", i64::from(done)),
                ("delivered", i64::from(delivered)),
                ("in_flight", progress.in_flight as i64),
                ("settled", i64::from(progress.settled())),
                ("escaped", 0),
            ] {
                assert_eq!(value, state[key], "{key} after {action}");
            }
            drop(progress);
            assert_eq!(
                sink.shared.spill.lock().unwrap().buf.len(),
                state["old"] as usize + state["later"] as usize
            );
            for law in &model.invariants {
                assert!(
                    model.check_invariant(law.name, &state),
                    "{} after {action}",
                    law.name
                );
            }
            if action == "DrainOld" {
                assert!(sink.wait_prefix_to_kernel(&prefix));
                assert_eq!(
                    sink.try_egress_drained_to_kernel(),
                    Some(false),
                    "negative control: the former global-empty condition still refuses"
                );
            }
        }
    }
    for (bug, law, suffix) in [
        (1, "LaterReplyDoesNotProlong", vec!["DrainOld"]),
        (
            2,
            "LaterFailureDoesNotRevoke",
            vec!["DrainOld", "FailReply"],
        ),
        (3, "OnlyCompletedPrefixPasses", vec![]),
        (4, "OnlyCompletedPrefixPasses", vec!["CancelOld"]),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        for action in ["Admit", "QueueReply", "Seal"].into_iter().chain(suffix) {
            assert!(mutant.fire(action, &mut state));
        }
        assert!(
            !model.check_invariant(law, &state),
            "mutant {bug} must break {law}"
        );
    }
}

#[test]
fn kernel_prefix_cancelled_native_write_conforms_and_rejects_early_retirement() {
    let model = aterm_models_session::kernel_prefix_model();
    let sink = SinkWriter::new(-1);
    let mut frame = Some(AcceptedFrame::new(&sink.shared, AcceptedOrder(1), true));
    let prefix = frame.as_ref().unwrap().prefix.clone().unwrap();
    let mut native = None;
    let mut generation = 0;
    let mut escaped = false;
    let mut state = model.init_state();
    for action in [
        "Admit",
        "QueueReply",
        "Seal",
        "BeginWrite",
        "CancelWriting",
        "FinishCancelled",
        "RetryCancelled",
    ] {
        assert!(model.fire(action, &mut state));
        match action {
            "Admit" => {
                sink.shared
                    .spill
                    .lock()
                    .unwrap()
                    .push_back(b"PP", Landing::Driven, Some(&prefix))
            }
            "QueueReply" => {
                sink.shared
                    .spill
                    .lock()
                    .unwrap()
                    .push_back(b"RRR", Landing::Answer, None)
            }
            "Seal" => drop(frame.take()),
            "BeginWrite" => {
                let spill = sink.shared.spill.lock().unwrap();
                generation = spill.generation;
                native = Some(prefix.native_write());
            }
            "CancelWriting" => {
                sink.shared
                    .fail_spill(&mut sink.shared.spill.lock().unwrap());
                let progress = prefix.progress.state.lock().unwrap();
                assert!(
                    progress.sealed && progress.pending == 0,
                    "negative control: the former receipt predicate retires too early"
                );
                assert!(!progress.settled());
            }
            "FinishCancelled" => {
                let spill = sink.shared.spill.lock().unwrap();
                if spill.generation == generation {
                    prefix.delivered(2);
                }
                drop(native.take());
            }
            "RetryCancelled" => {
                let spill = sink.shared.spill.lock().unwrap();
                escaped = spill.generation == generation;
            }
            _ => unreachable!(),
        }
        let progress = prefix.progress.state.lock().unwrap();
        for (key, value) in [
            ("old", progress.pending as i64),
            ("sealed", i64::from(progress.sealed)),
            ("failed", i64::from(progress.failed)),
            ("in_flight", progress.in_flight as i64),
            ("settled", i64::from(progress.settled())),
            ("done", i64::from(progress.settled() && !progress.failed)),
            ("escaped", i64::from(escaped)),
        ] {
            assert_eq!(value, state[key], "{key} after {action}");
        }
        drop(progress);
        assert_eq!(
            sink.shared.spill.lock().unwrap().buf.len(),
            (state["old"] + state["later"]) as usize
        );
        for law in &model.invariants {
            assert!(
                model.check_invariant(law.name, &state),
                "{} after {action}",
                law.name
            );
        }
    }
    assert!(!prefix.wait());
    for (bug, law) in [
        (5, "ActiveNativeWriteRetainsCustody"),
        (6, "CancelledChunkCannotContinue"),
        (7, "CancelledBytesAreNotDeliveredTwice"),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        let mut caught = false;
        for action in [
            "Admit",
            "QueueReply",
            "Seal",
            "BeginWrite",
            "CancelWriting",
            "FinishCancelled",
            "RetryCancelled",
        ] {
            assert!(mutant.fire(action, &mut state));
            caught |= !model.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug} must break {law}");
    }
}

fn pipe() -> (Arc<SinkWriter>, std::fs::File) {
    let (reader, writer) = std::io::pipe().unwrap();
    let (reader, writer): (OwnedFd, OwnedFd) = (reader.into(), writer.into());
    aterm_pty::set_nonblocking(writer.as_raw_fd(), true).unwrap();
    let sink = Arc::new(SinkWriter::new_owned(writer));
    sink.note_master_nonblocking(true);
    (sink, std::fs::File::from(reader))
}

fn fill(sink: &SinkWriter) -> usize {
    let mut filled = 0;
    loop {
        match aterm_pty::write_some(sink.master(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("fill private pipe: {other:?}"),
        }
    }
    assert!(filled > 0);
    filled
}

fn read_exactly(reader: &mut std::fs::File, want: usize) -> Vec<u8> {
    aterm_pty::set_nonblocking(reader.as_raw_fd(), true).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut bytes = vec![0; want];
    let mut off = 0;
    while off < want {
        match reader.read(&mut bytes[off..]) {
            Ok(n) if n > 0 => off += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "hang: private pipe prefix did not arrive"
                );
                aterm_uds::deadline::poll_yield();
            }
            other => panic!("read private pipe: {other:?}"),
        }
    }
    bytes
}

#[test]
fn a_real_partial_pipe_prefix_finishes_while_a_later_terminal_reply_is_still_spilled() {
    let (sink, mut reader) = pipe();
    sink.install_spill_arranger(|| {});
    let filled = fill(&sink);
    let room = filled.min(8192);
    assert!(read_exactly(&mut reader, room).iter().all(|b| *b == b'.'));
    let old = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
    let (outcome, receipt) = sink.try_write_frame_whole_with_prefix(&old);
    assert!(
        matches!(outcome, crate::sink::ImmediateWrite::Queued { .. }),
        "{outcome:?}"
    );
    let prefix = receipt.kernel_prefix().unwrap().clone();
    let later = vec![b'R'; 512 * 1024];
    assert_eq!(sink.write_reply(&later).unwrap(), later.len());
    let (tx, rx) = mpsc::channel();
    let waiter = {
        let sink = Arc::clone(&sink);
        let prefix = prefix.clone();
        std::thread::spawn(move || tx.send(sink.wait_prefix_to_kernel(&prefix)).unwrap())
    };
    let bytes = read_exactly(&mut reader, filled - room + old.len());
    assert!(bytes[..filled - room].iter().all(|b| *b == b'.'));
    assert_eq!(
        &bytes[filled - room..],
        old.as_slice(),
        "every byte of the admitted command arrived in order"
    );
    assert!(
        rx.recv_timeout(PATIENCE)
            .expect("hang: later replies prolonged the old prefix")
    );
    waiter.join().unwrap();
    assert!(
        sink.spilled_bytes() > 0,
        "later reply remains genuinely backpressured"
    );
    assert_ne!(
        sink.try_egress_drained_to_kernel(),
        Some(true),
        "negative control: the global-empty probe must refuse empty, including when busy"
    );
    let (other, _other_reader) = pipe();
    assert!(
        !other.wait_prefix_to_kernel(&prefix),
        "equal receipt shape does not grant another sink's custody"
    );
    assert!(
        !SinkWriter::new(sink.master()).wait_prefix_to_kernel(&prefix),
        "even the same raw descriptor is a different sink identity"
    );
    sink.sever_input();
    assert!(
        sink.wait_prefix_to_kernel(&prefix),
        "later reply cancellation cannot revoke an already delivered prefix"
    );
}

#[test]
fn a_cleared_pipe_prefix_retains_its_started_native_write_and_never_retries_the_old_chunk() {
    use crate::sink::{ImmediateWrite, Seam};
    use std::sync::atomic::{AtomicUsize, Ordering};

    for sever in [false, true] {
        let (sink, mut reader) = pipe();
        sink.install_spill_arranger(|| {});
        let filled = fill(&sink);
        let room = filled.min(8192);
        assert!(
            read_exactly(&mut reader, room)
                .iter()
                .all(|byte| *byte == b'.')
        );
        let old = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
        let (outcome, receipt) = sink.try_write_frame_whole_with_prefix(&old);
        let ImmediateWrite::Queued { accepted: direct } = outcome else {
            panic!("private pipe must admit a partial guarded frame: {outcome:?}");
        };
        let prefix = receipt.kernel_prefix().unwrap().clone();
        aterm_pty::set_nonblocking(sink.master(), false).unwrap();
        sink.note_master_nonblocking(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let (at_tx, at_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        sink.seam_for_testing({
            let calls = Arc::clone(&calls);
            move |seam| {
                if seam == Seam::DrainWrite {
                    assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
                    at_tx.send(()).unwrap();
                    resume_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(PATIENCE)
                        .expect("hang: native write reservation was never released");
                }
            }
        });
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = {
            let sink = Arc::clone(&sink);
            let prefix = prefix.clone();
            std::thread::spawn(move || {
                done_tx.send(sink.wait_prefix_to_kernel(&prefix)).unwrap();
            })
        };
        at_rx
            .recv_timeout(PATIENCE)
            .expect("hang: native operation did not start");
        assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
        if sever {
            sink.sever_input();
        } else {
            // Exercise a clear that changes no public discard counter or sever bit.
            sink.shared
                .fail_spill(&mut sink.shared.spill.lock().unwrap());
        }
        {
            let progress = prefix.progress.state.lock().unwrap();
            assert_eq!((progress.pending, progress.in_flight), (0, 1));
            assert!(progress.failed);
            assert!(!progress.settled());
            assert!(
                progress.sealed && progress.pending == 0,
                "negative control: the former empty-prefix condition releases active I/O"
            );
        }
        assert!(done_rx.try_recv().is_err());
        let before = read_exactly(&mut reader, filled - room + direct);
        assert!(before[..filled - room].iter().all(|byte| *byte == b'.'));
        assert!(before[filled - room..].iter().all(|byte| *byte == b'P'));
        resume_tx.send(()).unwrap();
        let started_bytes = crate::sink::Shared::DRAIN_CHUNK.min(old.len() - direct);
        assert!(
            read_exactly(&mut reader, started_bytes)
                .iter()
                .all(|byte| *byte == b'P'),
            "the registered operation performs real native I/O before retiring"
        );
        assert!(
            !done_rx
                .recv_timeout(PATIENCE)
                .expect("hang: canceled native operation never retired")
        );
        waiter.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let progress = prefix.progress.state.lock().unwrap();
        assert_eq!((progress.pending, progress.in_flight), (0, 0));
        assert!(progress.settled() && progress.failed);
    }
}

#[test]
fn queued_prefix_fails_on_sever_or_peer_close_and_ordinary_receipts_remain_unowned() {
    for close_peer in [false, true] {
        let (sink, reader) = pipe();
        sink.install_spill_arranger(|| {});
        fill(&sink);
        assert_eq!(sink.write_reply(b"seed").unwrap(), 4);
        let receipt = sink.write_frame_with_prefix(b"lost").unwrap();
        let prefix = receipt.kernel_prefix().unwrap().clone();
        assert_eq!(sink.write_reply(b"later").unwrap(), 5);
        if close_peer {
            drop(reader);
        } else {
            sink.sever_input();
        }
        assert!(!sink.wait_prefix_to_kernel(&prefix));
    }
    let (sink, mut reader) = pipe();
    let ordinary = sink.write_frame_with_receipt(b"plain").unwrap();
    assert!(
        ordinary.kernel_prefix().is_none(),
        "the ordinary hot path allocates no prefix owner"
    );
    assert_eq!(read_exactly(&mut reader, 5), b"plain");
}

#[test]
fn a_direct_partial_error_owns_only_its_accepted_kernel_prefix_and_zero_bytes_own_none() {
    let (sink, mut reader) = pipe();
    let (tx, rx) = mpsc::channel();
    let writer = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            tx.send(sink.write_frame_with_prefix(&vec![b'P'; 512 * 1024]))
                .unwrap()
        })
    };
    assert_eq!(
        read_exactly(&mut reader, 1),
        b"P",
        "the real write reached the kernel before closing its peer"
    );
    drop(reader);
    let result = rx
        .recv_timeout(PATIENCE)
        .expect("hang: partial write did not observe peer closure");
    let prefix = match &result {
        Ok(receipt) => {
            assert!(receipt.accepted() > 0 && receipt.accepted() < 512 * 1024);
            receipt.kernel_prefix().unwrap()
        }
        Err(error) => {
            assert!(error.order().is_some());
            error.kernel_prefix().unwrap()
        }
    };
    assert!(
        sink.wait_prefix_to_kernel(prefix),
        "every actually accepted byte was direct, despite the unaccepted tail's error"
    );
    writer.join().unwrap();
    let result = sink.write_frame_with_prefix(b"no-peer");
    match result {
        Ok(receipt) => {
            assert_eq!(receipt.accepted(), 0);
            assert!(receipt.order().is_none());
            assert!(receipt.kernel_prefix().is_none());
        }
        Err(error) => {
            assert!(error.order().is_none());
            assert!(error.kernel_prefix().is_none());
        }
    }
}
