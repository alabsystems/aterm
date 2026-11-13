// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::tests::parks;
use super::*;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn wedged() -> (Arc<SinkWriter>, UnixStream, usize) {
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    writer.set_nonblocking(true).expect("nonblocking master");
    let mut filled = 0usize;
    loop {
        match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            _ => break,
        }
    }
    assert!(filled > 0, "the socket took bytes before it filled");
    let owned: OwnedFd = writer.into();
    let sink = Arc::new(SinkWriter::new_owned(owned));
    sink.note_master_nonblocking(true);
    (sink, reader, filled)
}

fn bracketed(body: &[u8]) -> Vec<u8> {
    [PASTE_OPEN, body, PASTE_CLOSE].concat()
}

fn parked_after(parks: &mpsc::Receiver<()>) {
    while parks.try_recv().is_ok() {}
    parks
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the blocking writer parked on the full queue");
}

fn fill_spill_to_the_cap(sink: &SinkWriter) -> Vec<Vec<u8>> {
    let mut accepted = Vec::new();
    for i in 0usize.. {
        let frame = vec![b'a' + (i % 26) as u8; 4097];
        match sink.write_frame_interactive_with_receipt(&frame) {
            Ok(receipt) => {
                assert_eq!(receipt.accepted(), frame.len());
                accepted.push(frame);
            }
            Err(e) => {
                assert!(e.is_refused(), "the only refusal is the cap: {e}");
                break;
            }
        }
        assert!(i < 1024, "the cap never refused");
    }
    assert!(sink.shared.spill_len() > Shared::SPILL_CAP);
    accepted
}

#[test]
fn a_stop_reaches_a_paste_parked_on_a_tty_that_is_not_draining() {
    let (sink, mut reader, filled) = wedged();
    let parks = parks(&sink, Seam::WriterPark);
    let frame = bracketed(&vec![b'p'; 256 * 1024]);
    let meter = Arc::new(BulkMeter::new());
    let (tx, rx) = mpsc::channel();
    let writer = {
        let (sink, meter, frame) = (sink.clone(), meter.clone(), frame.clone());
        std::thread::spawn(move || {
            let receipt = sink.write_frame_metered_with_receipt(&frame, &meter);
            tx.send(receipt.map(|r| r.accepted()).map_err(|e| e.to_string()))
                .expect("receiver");
        })
    };
    let mut head = vec![0u8; filled + 64 * 1024];
    reader.read_exact(&mut head).expect("the fill and a head");
    parked_after(&parks);
    let before = meter.progress();
    assert!(before.sent > 0, "the paste began: {before:?}");
    assert_eq!(before.state, BulkState::Writing, "{before:?}");
    assert!(before.sent < frame.len() as u64, "the writer is parked");

    meter.request_stop();
    let accepted = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the stop reached the parked writer")
        .expect("a stop is not an error");
    writer.join().expect("writer");
    let end = meter.progress();
    assert_eq!(end.state, BulkState::Stopped);
    assert_eq!(end.sent, accepted as u64, "the meter is the bytes accepted");
    assert!(accepted > PASTE_OPEN.len() && accepted < frame.len());

    let (tx, rx) = mpsc::channel();
    {
        let sink = sink.clone();
        std::thread::spawn(move || {
            tx.send(sink.write_frame(b"K").map_err(|e| e.to_string()))
                .expect("receiver");
        });
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(60)).expect("K queued"),
        Ok(1)
    );

    let mut delivered = head.split_off(filled);
    let mut chunk = [0u8; 65536];
    while delivered.last() != Some(&b'K') {
        let n = reader
            .read(&mut chunk)
            .expect("the key reaches the program");
        assert!(n > 0, "peer closed early");
        delivered.extend_from_slice(&chunk[..n]);
    }
    delivered.pop();
    assert_eq!(delivered.len(), accepted, "exactly the accepted bytes");
    assert!(delivered.starts_with(PASTE_OPEN));
    assert!(
        delivered.ends_with(PASTE_CLOSE),
        "the owed close reaches the program ahead of the key"
    );
    let middle = &delivered[PASTE_OPEN.len()..delivered.len() - PASTE_CLOSE.len()];
    assert!(middle.iter().all(|b| *b == b'p'), "only the paste's text");
}

#[test]
fn a_stop_ends_a_paste_waiting_for_spill_room() {
    let (sink, _reader, _filled) = wedged();
    fill_spill_to_the_cap(&sink);
    let spilled = sink.shared.spill_len();
    let meter = Arc::new(BulkMeter::new());
    let (tx, rx) = mpsc::channel();
    let writer = {
        let (sink, meter) = (sink.clone(), meter.clone());
        std::thread::spawn(move || {
            let receipt = sink.write_frame_metered_with_receipt(&bracketed(b"late"), &meter);
            tx.send(receipt.map(|r| r.accepted()).map_err(|e| e.to_string()))
                .expect("receiver");
        })
    };
    meter.request_stop();
    let accepted = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the stop ended the room wait")
        .expect("a stop is not an error");
    writer.join().expect("writer");
    assert_eq!(accepted, 0);
    assert_eq!(meter.progress().state, BulkState::Stopped);
    assert_eq!(meter.progress().sent, 0);
    assert_eq!(sink.shared.spill_len(), spilled, "nothing was appended");
    sink.sever_input();
}

#[test]
fn the_interactive_egress_refuses_at_the_spill_cap_and_never_parks() {
    let (sink, mut reader, filled) = wedged();
    let accepted = fill_spill_to_the_cap(&sink);

    let (tx, rx) = mpsc::channel();
    {
        let sink = sink.clone();
        std::thread::spawn(move || {
            let refused = sink
                .write_frame_interactive_with_receipt(b"K")
                .expect_err("a key into a full input queue is refused");
            let is_refused = refused.is_refused();
            let order = refused.order();
            let kind = refused.into_error().kind();
            tx.send((is_refused, order, kind)).expect("receiver");
        });
    }
    let (is_refused, order, kind) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the interactive egress did not park");
    assert!(is_refused);
    assert_eq!(order, None, "no byte of a refused key is accepted");
    assert_eq!(kind, io::ErrorKind::WouldBlock);
    let dead = SinkWriter::new(-1)
        .write_frame_interactive_with_receipt(b"x")
        .expect_err("a dead fd fails");
    assert!(
        !dead.is_refused(),
        "a dead session is a failure, not a refusal"
    );

    let (tx, rx) = mpsc::channel();
    let parked = {
        let sink = sink.clone();
        std::thread::spawn(move || {
            tx.send(sink.write_frame_nonparking(b"P").map_err(|e| e.to_string()))
                .expect("receiver");
        })
    };

    let expect = filled + accepted.iter().map(Vec::len).sum::<usize>() + 1;
    let mut got = vec![0u8; expect];
    reader
        .read_exact(&mut got)
        .expect("everything accepted arrives");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(60)).expect("unparked"),
        Ok(1)
    );
    parked.join().expect("parked writer");
    assert!(got[..filled].iter().all(|b| *b == b'.'));
    let mut at = filled;
    for frame in &accepted {
        assert_eq!(&got[at..at + frame.len()], frame.as_slice(), "in order");
        at += frame.len();
    }
    assert_eq!(&got[at..], b"P");
    assert!(!got.contains(&b'K'), "the refused key was never delivered");

    let deadline = Instant::now() + Duration::from_secs(60);
    while !sink.egress_drained_to_kernel() {
        assert!(Instant::now() < deadline, "the spill drained");
        std::thread::sleep(Duration::from_millis(1));
    }
    let receipt = sink
        .write_frame_interactive_with_receipt(b"K")
        .expect("the retyped key goes through");
    assert_eq!(receipt.accepted(), 1);
    let mut k = [0u8; 1];
    reader.read_exact(&mut k).expect("read K");
    assert_eq!(&k, b"K");
}

#[test]
fn a_large_interactive_frame_spills_instead_of_parking() {
    let (sink, mut reader, filled) = wedged();
    let frame = vec![b'L'; 3 * SinkWriter::NONPARK_MAX];
    let (tx, rx) = mpsc::channel();
    {
        let (sink, frame) = (sink.clone(), frame.clone());
        std::thread::spawn(move || {
            let receipt = sink
                .write_frame_interactive_with_receipt(&frame)
                .expect("spilled");
            tx.send((receipt.accepted(), receipt.is_direct()))
                .expect("receiver");
        });
    }
    let (accepted, direct) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the large interactive frame did not park");
    assert_eq!(accepted, frame.len());
    assert!(!direct);
    let mut got = vec![0u8; filled + frame.len()];
    reader.read_exact(&mut got).expect("delivered once read");
    assert_eq!(&got[filled..], frame.as_slice());
}

#[test]
fn a_sever_ends_a_drainer_parked_on_a_full_queue() {
    let (sink, _reader, _filled) = wedged();
    let parks = parks(&sink, Seam::DrainPark);
    assert_eq!(sink.write_frame_nonparking(b"spilled").expect("spill"), 7);
    parks
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the drainer parked on the full queue");
    sink.sever_input();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the parked drainer ended at the sever",
        || {
            let s = sink.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
            (!s.draining).then_some(())
        },
    );
}

#[test]
fn a_sever_unblocks_every_waiter() {
    let (sink, _reader, _filled) = wedged();
    let parks = parks(&sink, Seam::WriterPark);
    let (tx, rx) = mpsc::channel::<(&str, Result<usize, String>)>();
    let spawn = |name: &'static str, bytes: Vec<u8>| {
        let (sink, tx) = (sink.clone(), tx.clone());
        std::thread::spawn(move || {
            let r = sink.write_frame(&bytes).map_err(|e| e.to_string());
            tx.send((name, r)).expect("receiver");
        })
    };
    let paste = spawn("paste", vec![b'p'; 64 * 1024]);
    parked_after(&parks);
    let mut keys = 0usize;
    while sink.shared.spill_len() <= Shared::SPILL_CAP {
        let receipt = sink
            .write_frame_interactive_with_receipt(&[b'k'; 4097])
            .expect("spilled");
        assert!(!receipt.is_direct());
        keys += 1;
        assert!(keys < 1024);
    }
    let waiter = spawn("room", b"w".to_vec());

    sink.sever_input();
    let mut returned = Vec::new();
    for _ in 0..2 {
        let (name, _) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the sever unblocked a waiter");
        returned.push(name);
    }
    returned.sort_unstable();
    assert_eq!(returned, ["paste", "room"]);
    paste.join().expect("paste");
    waiter.join().expect("waiter");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !sink.egress_drained_to_kernel() || sink.shared.spill.lock().unwrap().draining {
        assert!(Instant::now() < deadline, "the drainer exited");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(sink.is_severed());
    assert!(
        !sink.wait_egress_drained_to_kernel(),
        "the dropped spill is a loss"
    );
    for write in [
        sink.write_frame_with_receipt(b"x").map(|r| r.accepted()),
        sink.write_frame_interactive_with_receipt(b"x")
            .map(|r| r.accepted()),
        sink.write_frame_nonparking_with_receipt(b"x")
            .map(|r| r.accepted()),
    ] {
        let error = write
            .expect_err("a severed sink takes nothing")
            .into_error();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
    assert_eq!(
        sink.try_write_frame_immediate(b"x"),
        ImmediateWrite::BusyZero
    );
    assert!(sink.reply_budget().try_reserve(1).is_none());
    sink.sever_input();
}

#[test]
fn a_waiter_for_spill_room_arranges_a_pending_drainer_itself() {
    let (sink, mut reader, filled) = wedged();
    sink.install_spill_arranger(|| {});
    let accepted = fill_spill_to_the_cap(&sink);
    {
        let s = sink.shared.spill.lock().unwrap();
        assert!(s.draining && s.arrange_pending, "only a pending mark");
    }
    let (tx, rx) = mpsc::channel();
    let waiter = {
        let sink = sink.clone();
        std::thread::spawn(move || {
            tx.send(sink.write_frame(b"W").map_err(|e| e.to_string()))
                .expect("receiver");
        })
    };
    let expect = filled + accepted.iter().map(Vec::len).sum::<usize>() + 1;
    let mut got = vec![0u8; expect];
    reader
        .read_exact(&mut got)
        .expect("the spill drained through a drainer the waiter arranged");
    assert_eq!(got.last(), Some(&b'W'));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the waiter returned"),
        Ok(1)
    );
    waiter.join().expect("waiter");

    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the room waiter's drainer finished",
        || (!sink.shared.spill.lock().unwrap().draining).then_some(()),
    );
    let held = sink.shared.lock.lock().unwrap();
    assert_eq!(sink.write_frame_nonparking(b"C").expect("concede"), 1);
    assert!(sink.shared.spill.lock().unwrap().arrange_pending);
    drop(held);
    assert!(
        sink.wait_egress_drained_to_kernel(),
        "completion fence arranges a pending drainer itself"
    );
    let mut c = [0u8; 1];
    reader.read_exact(&mut c).expect("read C");
    assert_eq!(&c, b"C");

    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the completion waiter's drainer finished",
        || (!sink.shared.spill.lock().unwrap().draining).then_some(()),
    );
    let held = sink.shared.lock.lock().unwrap();
    assert_eq!(sink.write_frame_nonparking(b"T").expect("concede"), 1);
    assert!(sink.shared.spill.lock().unwrap().arrange_pending);
    drop(held);
    assert_eq!(
        sink.wait_egress_drained_to_kernel_for(aterm_spec::wait::MUST_HAPPEN),
        Some(true),
        "bounded completion fence arranges a pending drainer itself"
    );
    reader.read_exact(&mut c).expect("read T");
    assert_eq!(&c, b"T");
}

#[test]
fn reply_permits_bound_the_budget_and_release_on_drop() {
    let sink = SinkWriter::new(-1);
    let budget = sink.reply_budget();
    let first = budget.try_reserve(REPLY_BUDGET_BYTES / 2).expect("half");
    let second = budget
        .try_reserve(REPLY_BUDGET_BYTES / 2)
        .expect("the other half");
    assert_eq!(sink.reply_reserved(), REPLY_BUDGET_BYTES);
    assert!(budget.try_reserve(1).is_none(), "a full budget refuses");
    drop(first);
    assert_eq!(sink.reply_reserved(), REPLY_BUDGET_BYTES / 2);
    assert!(
        budget.try_reserve(REPLY_BUDGET_BYTES).is_none(),
        "an oversized reply waits for the queue to empty"
    );
    drop(second);
    let lone = budget
        .try_reserve(REPLY_BUDGET_BYTES + 1)
        .expect("a lone oversized reply is admitted into an empty queue");
    assert!(budget.try_reserve(1).is_none());
    drop(lone);
    assert_eq!(sink.reply_reserved(), 0, "released");
}
