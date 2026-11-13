// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Awake, PeerState, hang_up, peer_closed, peer_state};
use std::io::{ErrorKind, Read, Write};

#[test]
fn deadline_reads_drain_a_closed_peer_and_keep_the_absolute_bound() {
    let (near, mut far) = crate::CtlStream::pair().unwrap();
    far.write_all(b"queued").unwrap();
    drop(far);
    aterm_spec::wait::until_peer_closed(&near);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut bytes = [0; 6];
    assert_eq!(super::read_until(&near, &mut bytes, deadline).unwrap(), 6);
    assert_eq!(&bytes, b"queued");
    assert_eq!(super::read_until(&near, &mut bytes, deadline).unwrap(), 0);
    assert_eq!(
        super::read_until(&near, &mut bytes, std::time::Instant::now())
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

/// What one non-blocking read of `stream` finds: `Ok(0)` is its
/// end-of-file, `Err(WouldBlock)` an open socket with nothing to read.
fn read_now(stream: &crate::CtlStream) -> Result<usize, ErrorKind> {
    stream.set_nonblocking(true).expect("non-blocking");
    (&*stream).read(&mut [0u8; 16]).map_err(|e| e.kind())
}

/// A read of a nonblocking stream says `WouldBlock` at once, so the byte here was waited for.
#[test]
fn an_awake_read_waits_for_what_a_plain_read_would_not() {
    let (near, mut far) = crate::CtlStream::pair().expect("pair");
    near.set_nonblocking(true).expect("non-blocking");
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        let read = Awake(&near).read(&mut byte).map(|n| (n, byte));
        let _ = done.send(read);
    });
    std::thread::sleep(std::time::Duration::from_millis(20));
    far.write_all(b"k").expect("the byte");
    let hang = std::time::Duration::from_secs(60);
    let read = ended.recv_timeout(hang).expect("the read ends");
    assert_eq!(read.expect("the byte is read"), (1, *b"k"));
}

/// The `try_clone` stands in for the copy a forked child holds until it execs. (Darwin
/// refuses a both-ways `shutdown` once the peer half-closed, and shuts no half.)
#[test]
fn a_hang_up_reaches_a_peer_that_half_closed_though_a_copy_survives() {
    let (near, far) = crate::CtlStream::pair().expect("pair");
    let forked_copy = near.try_clone().expect("a surviving copy");
    far.shutdown(std::net::Shutdown::Write)
        .expect("the peer half-closes toward `near`");
    assert!(hang_up(&near), "the write half was still open to shut");
    assert_eq!(read_now(&far), Ok(0), "the peer reads its end-of-file");
    assert_eq!(
        read_now(&forked_copy),
        Ok(0),
        "a reader of a surviving copy reads its own"
    );
    #[cfg(target_os = "macos")]
    assert!(!hang_up(&near), "nothing is left to shut");
}

/// Between two open ends a hang-up shuts both halves: the peer and a surviving copy both
/// read end-of-file, and the peer's later writes are not delivered.
#[test]
fn a_hang_up_shuts_both_halves_of_an_open_socket() {
    let (near, far) = crate::CtlStream::pair().expect("pair");
    let forked_copy = near.try_clone().expect("a surviving copy");
    assert!(hang_up(&near), "both halves were open");
    assert_eq!(read_now(&far), Ok(0), "the peer reads its end-of-file");
    let _ = (&far).write_all(b"late\n");
    assert_eq!(
        read_now(&forked_copy),
        Ok(0),
        "a surviving copy reads end-of-file, and nothing written after"
    );
}

/// A HANG-UP WAKES A WRITER BLOCKED ON A TCP PEER THAT SENT ITS FIN AND
/// READS NOTHING: the write half's `shutdown` fails the blocked write
/// (`EPIPE`).
#[test]
fn a_hang_up_wakes_a_writer_blocked_on_a_tcp_peer_that_sent_its_fin() {
    use std::os::fd::AsRawFd as _;
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let peer = std::net::TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (near, _) = crate::accept(
        &listener,
        crate::AcceptWait::Forever,
        crate::AcceptedMode::Blocking,
    )
    .expect("accept");
    peer.shutdown(std::net::Shutdown::Write)
        .expect("the peer's FIN");
    near.set_read_timeout(Some(HANG)).expect("read timeout");
    assert_eq!(
        (&near).read(&mut [0u8; 1]).map_err(|e| e.kind()),
        Ok(0),
        "the FIN is in"
    );
    let writing = near.try_clone().expect("the writer's copy");
    let (wrote_tx, wrote) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let chunk = vec![b'a'; 64 * 1024];
        let failed = loop {
            if let Err(e) = (&writing).write_all(&chunk) {
                break e.kind();
            }
        };
        let _ = wrote_tx.send(failed);
    });
    let blocked = std::time::Instant::now() + HANG;
    while super::poll_now(near.as_raw_fd(), super::POLLOUT) & super::POLLOUT != 0 {
        assert!(
            std::time::Instant::now() < blocked,
            "the writer never filled the sockets"
        );
        crate::deadline::poll_yield();
    }
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            near.shutdown(std::net::Shutdown::Both)
                .map_err(|e| e.kind()),
            Err(ErrorKind::NotConnected),
            "Darwin refuses a both-ways shutdown of a socket whose peer sent its FIN"
        );
        assert!(
            wrote.try_recv().is_err(),
            "and that refusal shut no half: the writer is still blocked"
        );
    }
    assert!(hang_up(&near), "the write half was still open to shut");
    let failed = wrote.recv_timeout(HANG);
    assert!(failed.is_ok(), "the blocked write fails: {failed:?}");
    writer.join().expect("the writer");
    drop(peer);
}

/// Waits for `server`'s peer to read as gone, as a hang detector and never a budget: a
/// socket closes with its LAST descriptor, and a sibling test's fork that lands between the
/// pair and the drop hands its child a copy that keeps the peer open until that child execs.
fn until_gone(server: &crate::CtlStream, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while peer_state(server) != PeerState::Gone {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} must read as gone"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(peer_closed(server), "{what} is hung up to a request");
}

/// The four answers, with the request policy beside each: only Gone is a hangup.
#[test]
fn the_probe_tells_idle_readable_half_closed_and_gone_apart() {
    let (server, client) = crate::CtlStream::pair().expect("pair");
    assert_eq!(peer_state(&server), PeerState::Idle, "nothing sent yet");
    assert!(!peer_closed(&server), "an open peer is not hung up");

    (&client).write_all(b"await idle 50\n").expect("request");
    assert_eq!(peer_state(&server), PeerState::Readable, "a queued request");
    assert!(!peer_closed(&server), "nor is one with a request queued");
    let mut request = [0_u8; 14];
    (&server)
        .read_exact(&mut request)
        .expect("the request is still queued");
    assert_eq!(&request, b"await idle 50\n");

    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    assert_eq!(
        peer_state(&server),
        PeerState::HalfClosed,
        "its request read, its end-of-file is next"
    );
    assert!(
        !peer_closed(&server),
        "a half-closed peer still reads its answer: NOT a hangup"
    );
    (&server).write_all(b"OK\n").expect("answer");
    let mut answer = [0_u8; 3];
    (&client)
        .read_exact(&mut answer)
        .expect("the peer reads the answer");
    assert_eq!(&answer, b"OK\n", "and the probe wrote nothing before it");
    drop(client);
    until_gone(&server, "a closed peer");

    let (server, client) = crate::CtlStream::pair().expect("pair");
    let queued: &[u8] = b"await inbox since=4\n";
    (&client).write_all(queued).expect("request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    assert_eq!(
        peer_state(&server),
        PeerState::Readable,
        "a request queued ahead of the end-of-file"
    );
    drop(client);
    until_gone(&server, "a peer that queued a request and closed");
    let mut request = vec![0_u8; queued.len()];
    (&server)
        .read_exact(&mut request)
        .expect("the queued request is still read");
    assert_eq!(request, queued);

    let (server, client) = crate::CtlStream::pair().expect("pair");
    drop(client);
    until_gone(&server, "a peer closed without a half-close");
}
