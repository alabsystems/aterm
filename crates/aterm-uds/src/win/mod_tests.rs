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
