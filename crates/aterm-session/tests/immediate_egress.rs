// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immediate actuator-egress regression tests.
//!
//! A non-blocking pipe gives deterministic kernel backpressure without a shell
//! or live aterm session.  Like a raw PTY master, it can return initial EAGAIN or
//! a short write; unlike the ordinary sink path, the immediate API must never
//! spill the refused/tail bytes for a detached thread to inject later.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use aterm_session::sink::{ImmediateWrite, SinkWriter};

fn pipe_pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    // SAFETY: `fds` points to two initialized c_int slots; on success pipe(2)
    // replaces both with owned descriptors, each wrapped exactly once below.
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

    // There is room now.  A distinct accepted frame must be the very next
    // sequence; this catches any rejected frame that was secretly spilled.
    assert_eq!(
        sink.try_write_frame_immediate(b"accepted"),
        ImmediateWrite::Full
    );
    let mut next = [0_u8; 8];
    reader.read_exact(&mut next).expect("read accepted frame");
    assert_eq!(&next, b"accepted");
}

#[test]
fn conditional_submit_rejects_intervening_foreign_input_without_enter() {
    let (read, write) = pipe_pair();
    aterm_pty::set_nonblocking(write.as_raw_fd(), true).expect("nonblocking pipe writer");
    let sink = SinkWriter::new(write.as_raw_fd());
    sink.note_master_nonblocking(true);

    let baseline = sink.input_epoch();
    let (paste, after_paste) = sink.try_write_frame_immediate_if_epoch(baseline, b"operator-paste");
    assert_eq!(paste, ImmediateWrite::Full);

    // This is the ordinary human/raw-controller path. It reserves the shared
    // attempted-input epoch before taking the sink lock, so the operator may not
    // carry its pre-interjection token into Enter.
    assert_eq!(sink.write_frame(b"human").expect("foreign write"), 5);
    let (submit, _) = sink.try_write_frame_immediate_if_epoch(after_paste, b"\r");
    assert_eq!(submit, ImmediateWrite::ConflictZero);

    let mut reader = std::fs::File::from(read);
    let mut landed = vec![0_u8; b"operator-pastehuman".len()];
    reader
        .read_exact(&mut landed)
        .expect("drain accepted input");
    assert_eq!(&landed, b"operator-pastehuman");
    aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
    let mut unexpected = [0_u8; 1];
    let error = reader
        .read(&mut unexpected)
        .expect_err("guarded Enter must contribute zero bytes");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn short_write_is_partial_in_doubt_and_tail_is_never_injected() {
    let (read, write) = pipe_pair();
    let filled = fill_nonblocking(write.as_raw_fd());
    let sink = SinkWriter::new(write.as_raw_fd());
    sink.note_master_nonblocking(true);
    let mut reader = std::fs::File::from(read);

    // Make SOME room, but strictly less than the admitted frame.  For an
    // O_NONBLOCK pipe write larger than PIPE_BUF this deterministically exercises
    // the same short-write outcome a PTY is permitted to return.
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

    // Drain the untouched original suffix, followed by exactly the accepted P
    // prefix.  Then switch the reader non-blocking: EAGAIN proves the unaccepted
    // tail was not queued for a later drainer write.
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
