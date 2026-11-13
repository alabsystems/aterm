// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A doorbell polls ready from its first ring until its clear, and rings again after it: many
/// rings take one clear, and a clear of an unrung doorbell takes nothing. Nothing is timed:
/// each poll asks once.
///
/// NEGATIVE CONTROL (2026-11-09): with `clear` reading one byte instead of every ring, the
/// second poll after three rings read ready (`left: [0], right: []`).
#[test]
fn a_doorbell_reads_rung_until_cleared_and_rings_again() {
    use super::*;
    let bell = Doorbell::new().unwrap();
    assert!(any_now(&[bell.fd()]).unwrap().is_empty());
    assert!(!bell.clear());
    for _ in 0..3 {
        bell.ring();
    }
    assert_eq!(any_now(&[bell.fd()]).unwrap(), [0]);
    assert!(bell.clear());
    assert_eq!(any_now(&[bell.fd()]).unwrap(), Vec::<usize>::new());
    bell.ring();
    assert_eq!(any(&[bell.fd()], None).unwrap(), [0]);
    assert!(bell.clear());
    assert!(
        any(&[bell.fd()], Some(std::time::Instant::now()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mixed_readiness_reports_writable_readable_and_hangup_without_consuming_bytes() {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        os::fd::AsFd as _,
        time::Instant,
    };
    let (quiet, _keep) = crate::pipe().unwrap();
    let (mut sink, mut writable) = crate::pipe().unwrap();
    let (mut readable, mut source) = crate::pipe().unwrap();
    let (mut ended, close) = crate::pipe().unwrap();
    source.write_all(b"r").unwrap();
    drop(close);
    ended_pipe(ended.as_fd());
    let interests = [
        Watch::new(quiet.as_fd(), Interest::Read),
        Watch::new(writable.as_fd(), Interest::Write),
        Watch::new(readable.as_fd(), Interest::Read),
        Watch::new(ended.as_fd(), Interest::Read),
    ];
    assert_eq!(any_interests_now(&interests).unwrap(), [1, 2, 3]);
    assert_eq!(
        any_interests_until(&interests, Instant::now() + HANG).unwrap(),
        [1, 2, 3]
    );
    assert!(
        any_interests_until(&interests, Instant::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        any_interests_until(&[], Instant::now() + HANG)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    let mut byte = [0];
    readable.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"r");
    writable.write_all(b"w").unwrap();
    sink.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"w");
    assert_eq!(ended.read(&mut byte).unwrap(), 0);
    assert!(
        any_interests_now(&[Watch::new(quiet.as_fd(), Interest::Read)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        any_interests_now(&[]).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_blocked_writer_is_not_reported_as_readiness_work() {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        os::fd::AsFd as _,
        time::Instant,
    };
    let (mut drain, mut writer) = crate::pipe().unwrap();
    let flags = writer.try_clone().unwrap();
    with_nonblocking(flags.as_fd(), || {
        // Fill the actual pipe completely, including any partial final page.
        let mut written = 0;
        loop {
            match writer.write(b"x") {
                Ok(1) => {
                    written += 1;
                    assert!(written <= 8 * 1024 * 1024);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("filling private pipe: {other:?}"),
            }
        }
        let (mut hint, mut sender) = crate::pipe().unwrap();
        sender.write_all(b"h")?;
        assert_eq!(
            any_interests_until(
                &[
                    Watch::new(hint.as_fd(), Interest::Read),
                    Watch::new(writer.as_fd(), Interest::Write),
                ],
                Instant::now() + HANG
            )?,
            [0]
        );
        let mut byte = [0];
        hint.read_exact(&mut byte)?;
        assert_eq!(byte, *b"h");
        let mut bytes = vec![0; written];
        drain.read_exact(&mut bytes)?;
        assert!(bytes.iter().all(|byte| *byte == b'x'));
        assert_eq!(
            any_interests_until(
                &[Watch::new(writer.as_fd(), Interest::Write)],
                Instant::now() + HANG
            )?,
            [0]
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn many_readiness_preserves_bytes_and_reports_all_ready_indices() {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        os::fd::AsFd as _,
    };
    let (mut first, mut writer) = crate::pipe().unwrap();
    let (second, _quiet) = crate::pipe().unwrap();
    let (third, closed) = crate::pipe().unwrap();
    writer.write_all(b"x").unwrap();
    drop(closed);
    ended_pipe(third.as_fd());
    let found = any_until(
        &[first.as_fd(), second.as_fd(), third.as_fd()],
        std::time::Instant::now() + HANG,
    )
    .unwrap();
    assert_eq!(found, [0, 2]);
    let mut byte = [0];
    first.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
    assert!(
        any_until(&[second.as_fd()], std::time::Instant::now())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        any_until(&[], std::time::Instant::now() + HANG)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn readiness_stack_and_spill_preserve_indices_bytes_and_hangup() {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::time::Instant;

    for size in [1, 8, 9, 32] {
        let (mut readers, mut writers): (Vec<_>, Vec<_>) =
            (0..size).map(|_| crate::pipe().unwrap()).unzip();
        for writer in &mut writers {
            writer.write_all(b"x").unwrap();
        }
        {
            let fds: Vec<_> = readers.iter().map(|reader| reader.as_fd()).collect();
            let interests: Vec<_> = readers
                .iter()
                .map(|reader| Watch::new(reader.as_fd(), Interest::Read))
                .collect();
            let expected: Vec<_> = (0..size).collect();
            assert_eq!(any_until(&fds, Instant::now() + HANG).unwrap(), expected);
            assert_eq!(any_interests_now(&interests).unwrap(), expected);
            assert!(any_until(&fds, Instant::now()).unwrap().is_empty());
            assert!(
                any_interests_until(&interests, Instant::now())
                    .unwrap()
                    .is_empty()
            );
        }
        for reader in &mut readers {
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte, *b"x");
        }
        let interests: Vec<_> = readers
            .iter()
            .map(|reader| Watch::new(reader.as_fd(), Interest::Read))
            .collect();
        assert!(any_interests_now(&interests).unwrap().is_empty());
        drop(writers);
        for reader in &readers {
            ended_pipe(reader.as_fd());
        }
        assert_eq!(
            any_interests_now(&interests).unwrap(),
            (0..size).collect::<Vec<_>>()
        );
    }
}

#[test]
fn pair_deadline_preserves_bytes_and_hangup_still_wakes() {
    use super::*;
    use std::{
        io::{Read as _, Write as _},
        os::fd::AsFd as _,
        time::Instant,
    };
    let (mut first, mut writer) = crate::pipe().unwrap();
    let (second, other) = crate::pipe().unwrap();
    writer.write_all(b"x").unwrap();
    assert!(
        either_until(first.as_fd(), second.as_fd(), Instant::now())
            .unwrap()
            .is_none()
    );
    let found = either_until(first.as_fd(), second.as_fd(), Instant::now() + HANG)
        .unwrap()
        .unwrap();
    assert!(found.first);
    let mut byte = [0u8; 1];
    first.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
    drop(other);
    assert!(
        either_until(first.as_fd(), second.as_fd(), Instant::now() + HANG)
            .unwrap()
            .unwrap()
            .second
    );
}
#[test]
fn scoped_nonblocking_restores_flags_after_an_operation_error() {
    use std::io::Read as _;
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let (mut reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let observer = reader.try_clone().unwrap();
    // SAFETY: the stream retains its live descriptor throughout the test.
    let before = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
    assert!(before >= 0);
    let error = super::with_nonblocking(observer.as_fd(), || reader.read(&mut [0]).map(|_| ()))
        .expect_err("an empty open stream would block");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    // SAFETY: the same observer still retains the open descriptor.
    let after = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
    assert_eq!(after, before);
}

#[test]
fn scoped_nonblocking_restores_once_after_success_or_operation_error() {
    use std::cell::Cell;
    for fails in [false, true] {
        let flags = Cell::new(0);
        let calls = Cell::new(0);
        let result = super::with_nonblocking_by(
            0,
            |value| {
                calls.set(calls.get() + 1);
                flags.set(value);
                Ok(())
            },
            || {
                assert_ne!(flags.get() & libc::O_NONBLOCK, 0);
                if fails {
                    Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.is_err(), fails);
        assert_eq!(flags.get(), 0);
        assert_eq!(calls.get(), 2, "set nonblocking, then restore once");
    }
}

#[test]
fn scoped_nonblocking_unwind_restores_and_failed_restoration_still_retries() {
    use std::cell::Cell;
    let flags = Cell::new(0);
    let calls = Cell::new(0);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: std::io::Result<()> = super::with_nonblocking_by(
            0,
            |value| {
                calls.set(calls.get() + 1);
                flags.set(value);
                Ok(())
            },
            || panic!("operation unwinds"),
        );
    }));
    assert!(panic.is_err());
    assert_eq!(flags.get(), 0);
    assert_eq!(calls.get(), 2);

    calls.set(0);
    let error = super::with_nonblocking_by(
        0,
        |value| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            }
            flags.set(value);
            Ok(())
        },
        || Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        flags.get(),
        0,
        "Drop retries the failed explicit restoration"
    );
    assert_eq!(calls.get(), 3);

    calls.set(0);
    let error = super::with_nonblocking_by(
        0,
        |value| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
            }
            flags.set(value);
            Ok(())
        },
        || -> std::io::Result<()> { panic!("failed setup cannot run the operation") },
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(flags.get(), 0);
    assert_eq!(calls.get(), 2, "failed setup still attempts restoration");
}

#[test]
fn scoped_nonblocking_restores_native_flags_during_unwind() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
    // SAFETY: reader retains its live descriptor throughout this test.
    let before = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    assert!(before >= 0);
    let panic = std::panic::catch_unwind(|| {
        let _: std::io::Result<()> = super::with_nonblocking(reader.as_fd(), || {
            // SAFETY: the scope retains the same live descriptor.
            let during = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
            assert!(during >= 0);
            assert_ne!(during & libc::O_NONBLOCK, 0);
            panic!("operation unwinds");
        });
    });
    assert!(panic.is_err());
    // SAFETY: reader still retains that descriptor after unwinding.
    assert_eq!(
        unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
        before
    );
}

use super::{Ready, either, one};
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// The one-minute hang detector on a wait that must end.
const HANG: Duration = Duration::from_secs(60);

/// Waits under the hang detector for the pipe `reader` reads, with nothing left in it, to be
/// at its end. Once a test drops its writer the end must come, but not always at once where
/// the kernel keeps no `FD_CLOFORK` (off Apple, or an Apple kernel that ignores the mark): there
/// a sibling test's child parked between fork and exec (`fork_window::Parked`, or a `pre_exec`
/// spawn) holds a copy of every descriptor this process had open, the writer of a kept-out pipe
/// ([`crate::pipe`]) included, until it execs. A check that reads a set of ends at once comes
/// after this, so it reads the set, not a race with a sibling's fork.
fn ended_pipe(reader: std::os::fd::BorrowedFd<'_>) {
    assert_eq!(
        super::any_until(&[reader], std::time::Instant::now() + HANG).unwrap(),
        [0],
        "the pipe's end"
    );
}

#[test]
fn deadline_readiness_keeps_buffered_bytes_and_eof_after_disconnect() {
    let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
    near.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + HANG;
    super::until(near.as_fd(), super::Interest::Write, deadline).unwrap();
    far.write_all(b"kept").unwrap();
    drop(far);
    super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
    let mut bytes = [0; 4];
    near.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"kept");
    super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
    assert_eq!(near.read(&mut bytes).unwrap(), 0);
    for interest in [super::Interest::Read, super::Interest::Write] {
        assert_eq!(
            super::until(near.as_fd(), interest, std::time::Instant::now())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}

/// Bytes and then the connection's end each end the wait for a read, and neither is taken.
#[test]
fn a_wait_for_a_read_ends_at_bytes_and_at_the_end_and_takes_neither() {
    let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
    near.set_read_timeout(Some(HANG)).unwrap();
    far.write_all(b"k").unwrap();
    super::for_read(&near).unwrap();
    let mut byte = [0];
    near.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"k");
    far.shutdown(std::net::Shutdown::Write).unwrap();
    super::for_read(&near).unwrap();
    assert_eq!(near.read(&mut byte).unwrap(), 0);
}

#[test]
fn a_wait_for_a_read_past_its_timeout_says_what_the_read_would() {
    let (near, far) = crate::kept_out(UnixStream::pair).unwrap();
    near.set_read_timeout(Some(Duration::from_millis(1)))
        .unwrap();
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(super::for_read(&near));
    });
    let waited = ended.recv_timeout(HANG).expect("the timeout ends the wait");
    assert_eq!(waited.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    drop(far);
}

/// A byte already queued is the read's to return however short the timeout: `WouldBlock` for
/// it would tell the caller nothing came.
#[test]
fn a_queued_byte_comes_before_a_wait_for_a_reads_timeout() {
    let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
    near.set_read_timeout(Some(Duration::from_nanos(1)))
        .unwrap();
    far.write_all(b"k").unwrap();
    super::for_read(&near).expect("a byte waits to be read");
    near.read_exact(&mut [0]).unwrap();
}

/// `poll(2)` is asked even past the deadline, and only there does a past deadline not end
/// the wait first: [`super::until`] says expired on the same stream.
#[test]
fn a_wait_already_past_its_deadline_still_asks() {
    use std::os::fd::AsFd as _;
    let (near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
    let past = std::time::Instant::now();
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let asked = super::asked_until(near.as_fd(), super::Interest::Read, past);
        let _ = done.send((asked, near));
    });
    let (asked, near) = ended
        .recv_timeout(HANG)
        .expect("the deadline ends the wait");
    assert_eq!(asked.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    far.write_all(b"k").unwrap();
    super::asked_until(near.as_fd(), super::Interest::Read, past).expect("a byte waits");
    let expired = super::until(near.as_fd(), super::Interest::Read, past);
    assert_eq!(expired.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
}

/// With no timeout the wait lasts until a byte comes: on a nonblocking stream a wait that
/// gave up early would show as the read's `WouldBlock`.
#[test]
fn a_wait_for_a_read_with_no_timeout_lasts_until_a_byte_comes() {
    let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
    near.set_nonblocking(true).unwrap();
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = super::for_read(&near).and_then(|()| near.read_exact(&mut [0]));
        let _ = done.send(read);
    });
    std::thread::sleep(Duration::from_millis(20));
    far.write_all(b"k").unwrap();
    ended.recv_timeout(HANG).expect("the wait ends").unwrap();
}

/// EACH SIDE WAKES THE WAIT, AND NEITHER IS READ: a byte on the first is
/// still there after the wait said so; the second's writer closing (a
/// pipe at its end) wakes it as the second; and a wait woken while the
/// other side is still silent names only the side that woke it.
#[test]
fn either_side_ends_the_wait_and_nothing_is_read() {
    let (mut a_near, a_far) = UnixStream::pair().expect("a pair");
    let (b_read, b_write) = crate::pipe().expect("a pipe");
    let (said, heard) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let found = either(a_far.as_fd(), b_read.as_fd()).expect("the first wait");
        said.send(found).expect("the test listens");
        let mut byte = [0_u8; 1];
        (&a_far)
            .read_exact(&mut byte)
            .expect("the byte is still there");
        assert_eq!(&byte, b"x");
        let found = either(a_far.as_fd(), b_read.as_fd()).expect("the second wait");
        said.send(found).expect("the test listens");
    });
    a_near.write_all(b"x").expect("write");
    assert_eq!(
        heard.recv_timeout(HANG).expect("the first wait ended"),
        Ready {
            first: true,
            second: false
        }
    );
    drop(b_write);
    assert_eq!(
        heard.recv_timeout(HANG).expect("the second wait ended"),
        Ready {
            first: false,
            second: true
        }
    );
    waiter.join().expect("the waiter");
}

/// A LISTENER IS READY WITH A CONNECTION WAITING: [`one`] ends once a connect is queued, and
/// a `Ready` accept then finds it.
#[test]
fn one_ends_when_a_listener_has_a_connection_to_accept() {
    let root = aterm_tempfile::tempdir().expect("a folder");
    let path = root.path().join("ready.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let (said, heard) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        one(listener.as_fd()).expect("the wait");
        said.send(
            crate::accept(
                &listener,
                crate::AcceptWait::Ready,
                crate::AcceptedMode::Blocking,
            )
            .is_ok(),
        )
        .expect("the test listens");
    });
    let _client = UnixStream::connect(&path).expect("connect");
    assert!(heard.recv_timeout(HANG).expect("the wait ended"));
    waiter.join().expect("the waiter");
}

/// A PEER'S CLOSE IS READY: a socket whose peer is gone ends the wait
/// as its side, with nothing to read but its end.
#[test]
fn a_closed_peer_ends_the_wait() {
    let (near, far) = UnixStream::pair().expect("a pair");
    let (b_read, _b_write) = crate::pipe().expect("a pipe");
    drop(far);
    let found = either(b_read.as_fd(), near.as_fd()).expect("the wait");
    assert_eq!(
        found,
        Ready {
            first: false,
            second: true
        }
    );
    assert_eq!((&near).read(&mut [0_u8; 1]).expect("its end"), 0);
}
