// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immediate actuator egress over a non-blocking pipe, which gives deterministic backpressure and,
//! like a raw PTY master, can refuse with EAGAIN or short-write.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use aterm_session::sink::{ImmediateWrite, Seam, SinkWriter};
use aterm_spec::wait::MUST_HAPPEN;

fn pipe_pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    // SAFETY: `fds` points to two initialized c_int slots; on success pipe(2) replaces both with
    // owned descriptors, each wrapped exactly once below.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe: {}", std::io::Error::last_os_error());
    // SAFETY: successful pipe(2) returned two fresh, independently-owned fds.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: same as above, for the distinct write descriptor.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    (read, write)
}

fn fill_nonblocking(fd: i32) -> usize {
    aterm_pty::set_nonblocking(fd, true).expect("nonblocking pipe writer");
    let mut filled = 0_usize;
    loop {
        match aterm_pty::write_some(fd, &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    assert!(filled > 0, "pipe must accept bytes before EAGAIN");
    filled
}

#[test]
fn initial_eagain_is_busy_zero_and_never_appears_later() {
    let (read, write) = pipe_pair();
    let filled = fill_nonblocking(write.as_raw_fd());
    let sink = SinkWriter::new(write.as_raw_fd());
    sink.note_master_nonblocking(true);

    assert_eq!(
        sink.try_write_frame_immediate(b"REJECTED"),
        ImmediateWrite::BusyZero,
        "initial EAGAIN must be an exact zero-byte refusal"
    );

    let mut reader = std::fs::File::from(read);
    let mut old = vec![0_u8; filled];
    reader.read_exact(&mut old).expect("drain original fill");
    assert!(old.iter().all(|byte| *byte == b'.'));

    assert_eq!(
        sink.try_write_frame_immediate(b"accepted"),
        ImmediateWrite::Full
    );
    let mut next = [0_u8; 8];
    reader.read_exact(&mut next).expect("read accepted frame");
    assert_eq!(&next, b"accepted");
}

#[test]
fn short_write_is_partial_in_doubt_and_tail_is_never_injected() {
    let (read, write) = pipe_pair();
    let filled = fill_nonblocking(write.as_raw_fd());
    let sink = SinkWriter::new(write.as_raw_fd());
    sink.note_master_nonblocking(true);
    let mut reader = std::fs::File::from(read);

    let room = filled.min(8 * 1024);
    let mut prefix = vec![0_u8; room];
    reader.read_exact(&mut prefix).expect("make partial room");
    assert!(prefix.iter().all(|byte| *byte == b'.'));

    let frame = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
    let accepted = match sink.try_write_frame_immediate(&frame) {
        ImmediateWrite::PartialInDoubt { accepted } => accepted,
        other => panic!("expected a kernel short write, got {other:?}"),
    };
    assert!(accepted > 0);
    assert!(accepted < frame.len());

    let old_remaining = filled - room;
    let mut landed = vec![0_u8; old_remaining + accepted];
    reader
        .read_exact(&mut landed)
        .expect("drain old bytes and accepted prefix");
    assert!(landed[..old_remaining].iter().all(|byte| *byte == b'.'));
    assert!(landed[old_remaining..].iter().all(|byte| *byte == b'P'));

    aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
    let mut unexpected = [0_u8; 1];
    let error = reader
        .read(&mut unexpected)
        .expect_err("unaccepted tail must never appear later");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

/// THE GUARDED WRITE FINISHES A FRAME IT BEGAN, IN ORDER
/// (`try_write_frame_whole_with_receipt`): over the same partly-full pipe as
/// [`short_write_is_partial_in_doubt_and_tail_is_never_injected`], the kernel
/// takes a prefix and the rest is queued behind it — `Queued`, with the
/// prefix's count and an accepted order — and a frame another writer sends
/// right after lands after ALL of it: the reader drains the old fill, every
/// `P` of the frame, then `after`, with nothing between. A second guarded
/// write while the rest is queued is refused with zero bytes (`BusyZero`), so a
/// guarded Enter can never overtake the text it submits.
#[test]
fn the_guarded_write_queues_the_rest_of_a_short_write_and_delivers_it_whole() {
    let (read, write) = pipe_pair();
    let filled = fill_nonblocking(write.as_raw_fd());
    let sink = SinkWriter::new(write.as_raw_fd());
    sink.note_master_nonblocking(true);
    let mut reader = std::fs::File::from(read);

    assert_eq!(
        sink.try_write_frame_whole_with_receipt(b"REJECTED"),
        (ImmediateWrite::BusyZero, None)
    );
    assert_eq!(sink.spilled_bytes(), 0, "a refusal queues nothing");

    let room = filled.min(8 * 1024);
    let mut prefix = vec![0_u8; room];
    reader.read_exact(&mut prefix).expect("make partial room");

    let frame = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
    let (outcome, order) = sink.try_write_frame_whole_with_receipt(&frame);
    let accepted = match outcome {
        ImmediateWrite::Queued { accepted } => accepted,
        other => panic!("expected the rest queued behind a short write, got {other:?}"),
    };
    assert!(accepted > 0 && accepted < frame.len(), "{accepted}");
    assert!(order.is_some(), "an accepted prefix carries its order");
    assert_eq!(sink.spilled_bytes(), frame.len() - accepted);

    assert_eq!(
        sink.try_write_frame_whole_with_receipt(b"\r"),
        (ImmediateWrite::BusyZero, None)
    );
    assert_eq!(sink.write_frame_nonparking(b"after").expect("queued"), 5);

    let old_remaining = filled - room;
    let want = old_remaining + frame.len() + b"after".len();
    aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
    let mut landed = Vec::with_capacity(want);
    let drained = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || {
            let mut buf = [0_u8; 16 * 1024];
            match reader.read(&mut buf) {
                Ok(n) => landed.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("read: {e}"),
            }
            (landed.len() >= want).then_some(())
        },
    );
    assert!(
        drained.is_some(),
        "only {} of {want} bytes came",
        landed.len()
    );
    assert_eq!(landed.len(), want, "nothing past the later frame");
    assert!(landed[..old_remaining].iter().all(|byte| *byte == b'.'));
    let body = &landed[old_remaining..old_remaining + frame.len()];
    assert!(body.iter().all(|byte| *byte == b'P'), "the frame whole");
    assert_eq!(&landed[old_remaining + frame.len()..], b"after");
}

/// One run of the race the spill's active bit decides: a blocking frame
/// (`write_frame`, a paste or a control verb's) has looked at the spill,
/// found it empty, and is stopped before it waits for the fd lock
/// (`Seam::FrameLock`); meanwhile `write` (an immediate write) takes a short
/// write over a partly-full pipe. Then the blocking frame goes on, and the
/// reader drains everything for at most a minute (a hang detector). The
/// drainer the immediate write deferred to the arranger is spawned only once
/// the blocking frame has returned, so the frame took the fd lock with no
/// drainer to race it for the lock: nothing but the code orders the bytes.
/// Returns what the immediate write answered, the reader's bytes past the
/// old fill, and the frame written.
fn a_blocking_frame_behind(
    write: impl FnOnce(&SinkWriter, &[u8]) -> ImmediateWrite,
) -> (ImmediateWrite, Vec<u8>, Vec<u8>) {
    let (read, pipe) = pipe_pair();
    let filled = fill_nonblocking(pipe.as_raw_fd());
    let sink = Arc::new(SinkWriter::new(pipe.as_raw_fd()));
    sink.note_master_nonblocking(true);
    sink.install_spill_arranger(|| {});
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let stopped = AtomicBool::new(false);
    sink.seam_for_testing(move |seam| {
        if seam != Seam::FrameLock || stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = at_tx.send(());
        go_rx
            .lock()
            .expect("go")
            .recv_timeout(MUST_HAPPEN)
            .expect("the test lets the blocking frame go on");
    });
    let mut reader = std::fs::File::from(read);
    let room = filled.min(8 * 1024);
    let mut made = vec![0_u8; room];
    reader.read_exact(&mut made).expect("make partial room");

    let (done_tx, done_rx) = mpsc::channel::<usize>();
    let blocking = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            let wrote = sink.write_frame(b"after").expect("the blocking frame");
            let _ = done_tx.send(wrote);
        })
    };
    at_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the blocking frame found the spill empty");
    let frame = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
    let outcome = write(&sink, &frame);
    go_tx.send(()).expect("the blocking frame waits");

    aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
    let accepted = match outcome {
        ImmediateWrite::Queued { .. } => frame.len(),
        ImmediateWrite::PartialInDoubt { accepted } => accepted,
        other => panic!("a short write over a partly-full pipe: {other:?}"),
    };
    let want = filled - room + accepted + b"after".len();
    let mut landed = Vec::with_capacity(want);
    let mut wrote = None;
    let drained =
        aterm_spec::wait::within(MUST_HAPPEN, std::time::Duration::from_millis(5), || {
            if wrote.is_none()
                && let Ok(n) = done_rx.try_recv()
            {
                wrote = Some(n);
                sink.arrange_pending_drainer();
            }
            let mut buf = [0_u8; 16 * 1024];
            match reader.read(&mut buf) {
                Ok(n) => landed.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("read: {e}"),
            }
            (wrote.is_some() && landed.len() >= want).then_some(())
        });
    assert!(
        drained.is_some(),
        "only {} of {want} bytes came",
        landed.len()
    );
    blocking.join().expect("the blocking frame");
    assert_eq!(wrote, Some(b"after".len()));
    let mut more = [0_u8; 1];
    let error = reader
        .read(&mut more)
        .expect_err("nothing past the later frame");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(landed[..filled - room].iter().all(|byte| *byte == b'.'));
    (outcome, landed.split_off(filled - room), frame)
}

/// A BLOCKING FRAME THAT SAW THE SPILL EMPTY NEVER OVERTAKES THE GUARDED
/// WRITE'S QUEUED REST. A blocking writer looks at the spill before it waits
/// for the fd lock (so a wedged drainer holding that lock cannot park it
/// behind the cap), so it can find the spill empty, wait while a guarded
/// write holds the lock and queues the rest of a short write, and take the
/// lock after it. The guarded write sets the spill's active bit as it queues
/// (`Shared::queue_rest_held`), so that writer is refused the direct lane
/// (`direct_order_if_spill_empty`) and queues behind the rest: the reader
/// gets the frame whole, then `after`.
#[test]
fn a_blocking_frame_that_saw_the_spill_empty_queues_behind_the_guarded_rest() {
    let (outcome, landed, frame) =
        a_blocking_frame_behind(|sink, frame| sink.try_write_frame_whole_with_receipt(frame).0);
    assert!(
        matches!(outcome, ImmediateWrite::Queued { accepted } if accepted < frame.len()),
        "{outcome:?}"
    );
    assert!(
        landed[..frame.len()].iter().all(|byte| *byte == b'P'),
        "the guarded frame whole, nothing inside it"
    );
    assert_eq!(&landed[frame.len()..], b"after", "then the blocking frame");

    let (outcome, landed, _) =
        a_blocking_frame_behind(|sink, frame| sink.try_write_frame_immediate_with_receipt(frame).0);
    let ImmediateWrite::PartialInDoubt { accepted } = outcome else {
        panic!("the plain write in doubt: {outcome:?}");
    };
    assert!(landed[..accepted].iter().all(|byte| *byte == b'P'));
    assert_eq!(&landed[accepted..], b"after", "right behind the prefix");
}
