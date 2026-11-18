// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Hang detector for wake tests.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A deadline beyond Winsock's one-call range is a capped slice, not an early expiry.
#[test]
fn accept_poll_timeout_marks_only_a_long_deadline_as_capped() {
    let now = Instant::now();
    let long = now + Duration::from_millis(i32::MAX as u64 + 1_000);
    let (timeout, capped) = accept_poll_timeout(Some(long), now).expect("a future deadline");
    assert_eq!(timeout, i32::MAX);
    assert!(capped, "the caller must wait another slice after this one");

    let short = now + Duration::from_secs(1);
    let (timeout, capped) = accept_poll_timeout(Some(short), now).expect("a future deadline");
    assert_eq!(timeout, 1_000);
    assert!(!capped, "this timeout is the absolute deadline");
    assert_eq!(
        accept_poll_timeout(Some(now), now)
            .expect_err("the deadline is expired")
            .kind(),
        io::ErrorKind::TimedOut
    );
}

/// The sanctioned Windows Unix accept applies the requested logical stream mode: a
/// nonblocking read answers immediately, while a blocking read waits across a planted write
/// gap. Changing either mode application breaks its corresponding assertion/control.
#[test]
fn sanctioned_accept_applies_each_requested_stream_mode() {
    use std::io::{Read as _, Write as _};

    let root = aterm_tempfile::tempdir().expect("a short private directory");
    let path = root.path().join("a.sock");
    let listener = CtlListener::bind(&path).expect("bind");

    let empty = CtlStream::connect(&path).expect("connect the empty peer");
    let (nonblocking, ()) = crate::accept(
        &listener,
        crate::AcceptWait::Until(Instant::now() + HANG_DETECTOR),
        crate::AcceptedMode::Nonblocking,
    )
    .expect("accept the empty peer");
    assert!(
        !nonblocking.0.blocking.load(Ordering::Acquire),
        "the requested nonblocking mode is applied before the read"
    );
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = read_tx.send((&nonblocking).read(&mut [0]));
    });
    let result = match read_rx.recv_timeout(HANG_DETECTOR) {
        Ok(result) => result,
        Err(error) => {
            // Release and join a mutant that blocked instead of returning `WouldBlock`.
            (&empty)
                .write_all(b"x")
                .expect("release the blocked mutant");
            reader.join().expect("blocked reader");
            panic!("the nonblocking read hung: {error}");
        }
    };
    reader.join().expect("nonblocking reader");
    let error = result.expect_err("an empty nonblocking stream cannot wait");
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
    drop(empty);

    let delayed_path = path.clone();
    let writer = std::thread::spawn(move || {
        let stream = CtlStream::connect(&delayed_path).expect("connect the delayed peer");
        std::thread::sleep(Duration::from_millis(300));
        (&stream).write_all(b"x").expect("the delayed write");
    });
    let (blocking, ()) = crate::accept(
        &listener,
        crate::AcceptWait::Until(Instant::now() + HANG_DETECTOR),
        crate::AcceptedMode::Blocking,
    )
    .expect("accept the delayed peer");
    assert!(
        blocking.0.blocking.load(Ordering::Acquire),
        "the requested blocking mode is applied before the read"
    );
    blocking
        .set_read_timeout(Some(HANG_DETECTOR))
        .expect("read hang detector");
    let mut byte = [0];
    (&blocking)
        .read_exact(&mut byte)
        .expect("the blocking stream waits for the delayed write");
    assert_eq!(&byte, b"x");
    writer.join().expect("writer");
}

/// Non-ASCII UTF-8 paths encode directly within the 107-byte `sun_path` budget.
#[test]
fn utf8_non_ascii_paths_encode_directly() {
    let s = "C:\\Users\\Владимир\\AppData\\Local\\aterm\\aterm-1234.sock";
    let (addr, _) = try_encode(s).expect("non-ASCII UTF-8 within budget encodes");
    assert_eq!(addr.sun_family, ffi::AF_UNIX as u16);
    assert_eq!(&addr.sun_path[..s.len()], s.as_bytes());
    assert_eq!(addr.sun_path[s.len()], 0, "NUL-terminated");
    assert!(try_encode(&format!("C:\\{}", "я".repeat(54))).is_none());
    assert!(
        try_encode("C:\\a\0b.sock").is_none(),
        "interior NUL rejected"
    );
}

/// afunix.sys reads `sun_path` as UTF-8: a full bind/connect/accept round
/// trip through a non-ASCII directory.
#[test]
fn non_ascii_dir_binds_and_connects() {
    use std::io::{Read, Write};
    let dir = std::env::temp_dir().join(format!("aterm-uds-профиль-配置-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create non-ASCII test dir");
    let path = dir.join("aterm-1.sock");

    let listener = CtlListener::bind(&path).expect("bind in non-ASCII dir");
    let server = std::thread::spawn(move || {
        let (stream, ()) = crate::accept(
            &listener,
            crate::AcceptWait::Until(Instant::now() + HANG_DETECTOR),
            crate::AcceptedMode::Blocking,
        )
        .expect("accept");
        let mut buf = [0u8; 4];
        (&stream).read_exact(&mut buf).expect("read");
        (&stream).write_all(&buf).expect("echo");
    });
    let client = CtlStream::connect(&path).expect("connect in non-ASCII dir");
    (&client).write_all(b"ping").expect("write");
    let mut back = [0u8; 4];
    (&client).read_exact(&mut back).expect("read echo");
    assert_eq!(&back, b"ping");
    server.join().expect("server thread");

    assert!(std::fs::symlink_metadata(&path).is_ok());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A no-timeout read parks on the readiness/shutdown events and wakes
/// with EOF when a clone is shut down.
#[test]
fn idle_parked_read_wakes_on_shutdown() {
    use std::io::Read;
    let (a, _b) = CtlStream::pair().expect("pair");
    let clone = a.try_clone().expect("clone");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let _ = tx.send((&a).read(&mut buf));
    });
    let deadline = Instant::now() + HANG_DETECTOR;
    while clone.0.parked_reads.load(Ordering::Acquire) == 0 {
        assert!(
            Instant::now() < deadline,
            "reader never reached wait_interruptible"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    clone.shutdown(std::net::Shutdown::Both).expect("shutdown");
    let res = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("shutdown must wake the parked read");
    if let Ok(n) = res {
        assert_eq!(n, 0, "post-shutdown read is EOF");
    }
    reader.join().expect("reader thread");
}

/// A no-timeout read parked with no data wakes on the peer's write via the
/// `FD_READ` readiness event and returns the bytes.
#[test]
fn parked_read_wakes_on_peer_write() {
    use std::io::{Read, Write};
    let (a, b) = CtlStream::pair().expect("pair");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 5];
        let n = (&a).read(&mut buf).expect("read");
        let _ = tx.send((n, buf));
    });
    std::thread::sleep(Duration::from_millis(80));
    (&b).write_all(b"hello").expect("write");
    let (n, buf) = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("peer write must wake the parked read");
    assert_eq!(&buf[..n], b"hello");
    reader.join().expect("reader thread");
}

/// A write to a stream whose peer has closed fails at that write, as a Unix socket's `EPIPE`
/// does, where afunix takes the first send into such a connection and fails only the next: a
/// relay forwarding a byte to a host that has gone sees it at once. A zero-length send writes
/// nothing the peer reads, and a peer that shut only its write half reads what is written.
/// NEGATIVE CONTROL: with the check after a send (`CtlStream::delivered`) answering the send's
/// own count, the first write to each gone peer answers `Ok`.
#[test]
fn a_write_to_a_peer_that_has_closed_fails_at_that_write() {
    use std::io::{IoSlice, Read, Write};
    let gone = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        )
    };
    let (writer, peer) = CtlStream::pair().expect("pair");
    drop(peer);
    let wrote = (&writer).write(b"x");
    assert!(wrote.as_ref().is_err_and(gone), "a closed peer: {wrote:?}");

    let (writer, peer) = CtlStream::pair().expect("pair");
    crate::hangup::hang_up(&peer);
    drop(peer);
    let wrote = (&writer).write_vectored(&[IoSlice::new(b"ab"), IoSlice::new(b"c")]);
    assert!(wrote.as_ref().is_err_and(gone), "a hung-up peer: {wrote:?}");

    let (writer, peer) = CtlStream::pair().expect("pair");
    peer.shutdown(std::net::Shutdown::Write)
        .expect("the peer shuts its write half");
    (&writer)
        .write_all(b"still read")
        .expect("a half-closed peer still takes bytes");
    peer.set_read_timeout(Some(HANG_DETECTOR))
        .expect("a hang detector");
    let mut got = [0u8; 10];
    (&peer)
        .read_exact(&mut got)
        .expect("the half-closed peer reads");
    assert_eq!(&got, b"still read");
    let mut more = [0u8; 1];
    let after = peer.read_nonblocking(&mut more);
    assert!(
        after
            .as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock),
        "nothing past the bytes written: {after:?}"
    );
}

/// A WRITE THE PEER READ BEFORE IT CLOSED STAYS DELIVERED: the check after a send
/// (`CtlStream::delivered`) reads only what a send found, so a peer that reads the bytes and then
/// closes between the send and that check leaves the write answering its count, as a Unix write
/// that reached its peer does, and the next write is the one that fails, as on Unix. That holds
/// for a peer dropped, hung up first, or that wrote an answer this end has not read yet (a host
/// that reads a forwarded request, answers and goes), whose answer this end still reads, then
/// its end. A peer that closes with the bytes still unread discards them, and afunix resets the
/// connection then: that write fails, since nothing will ever read what it sent. Each case runs
/// the send and the check as `send_with` does, with the peer's close between them.
/// NEGATIVE CONTROL: a check that reads the peer's end (a peeking `recv` that reads EOF) in place
/// of the zero-length send fails the delivered write (`Err` where `Ok(5)` is due).
#[test]
fn a_write_the_peer_read_before_it_closed_stays_delivered() {
    use std::io::{Read, Write};
    let gone = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        )
    };
    // (the case, the peer reads the bytes, it answers, it hangs up before it is dropped)
    for (case, reads, answers, hangs_up) in [
        ("read, then dropped", true, false, false),
        ("read, then hung up and dropped", true, false, true),
        ("read, answered, then dropped", true, true, false),
        ("dropped with the bytes unread", false, false, false),
    ] {
        let (writer, peer) = CtlStream::pair().expect("pair");
        // SAFETY: `raw` is `writer`'s live socket; the five bytes outlive the call.
        let sent = unsafe { ffi::send(writer.0.raw, b"reply".as_ptr(), 5, 0) };
        assert_eq!(sent, 5, "{case}: the send");
        if reads {
            peer.set_read_timeout(Some(HANG_DETECTOR))
                .expect("a hang detector");
            let mut got = [0u8; 5];
            (&peer).read_exact(&mut got).expect("the peer reads");
            assert_eq!(&got, b"reply", "{case}");
        }
        if answers {
            (&peer).write_all(b"done").expect("the peer answers");
        }
        if hangs_up {
            crate::hangup::hang_up(&peer);
        }
        drop(peer);
        let answered = writer.delivered(5);
        if reads {
            assert!(
                matches!(answered, Ok(5)),
                "{case}: a write the peer read before it closed: {answered:?}"
            );
        } else {
            assert!(
                answered.as_ref().is_err_and(gone),
                "{case}: a write nothing will read: {answered:?}"
            );
        }
        if answers {
            writer
                .set_read_timeout(Some(HANG_DETECTOR))
                .expect("a hang detector");
            let mut answer = Vec::new();
            (&writer)
                .read_to_end(&mut answer)
                .expect("the answer, then the end");
            assert_eq!(answer, b"done", "{case}");
        }
        let next = (&writer).write(b"x");
        assert!(
            next.as_ref().is_err_and(gone),
            "{case}: the next write: {next:?}"
        );
    }
}
