// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::time::Duration;

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

fn deadline() -> Instant {
    Instant::now() + MUST_HAPPEN
}

#[test]
fn a_closed_unix_peer_keeps_its_queued_reply() {
    let (mut client, mut server) = CtlStream::pair().unwrap();
    client.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    server.write_all(b"queued verdict").unwrap();
    drop(server);
    aterm_spec::wait::until_peer_closed(&client);
    #[cfg(target_os = "macos")]
    assert_eq!(
        client.peer_addr().unwrap_err().raw_os_error(),
        Some(libc::EINVAL)
    );
    // NEGATIVE CONTROL: a repeated peer_addr check here discards the queued verdict on macOS.
    client = finish_unix_connect(client, deadline(), None).unwrap();
    let mut bytes = Vec::new();
    client.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"queued verdict");
}

#[test]
fn connected_stream_has_peer_identity_and_is_blocking_and_cloexec() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("connect.sock");
    let listener = crate::CtlListener::bind(&path).unwrap();
    crate::keep_out_of_children(&listener).unwrap();
    let mut client = until(&path, deadline()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (mut server, _) = crate::accept(
        &listener,
        crate::AcceptWait::Ready,
        crate::AcceptedMode::Blocking,
    )
    .unwrap();
    crate::keep_out_of_children(&server).unwrap();
    assert_eq!(crate::peer::peer_uid(&client), Some(crate::peer::our_uid()));
    // SAFETY: both fcntl calls read flags from a live test descriptor.
    let status = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
    let descriptor = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
    assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
    assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
    #[cfg(target_vendor = "apple")]
    {
        let mut enabled: libc::c_int = 0;
        let mut length = std::mem::size_of_val(&enabled) as libc::socklen_t;
        // SAFETY: enabled and length are writable and describe the option buffer.
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    client.as_raw_fd(),
                    libc::SOL_SOCKET,
                    SO_NOSIGPIPE,
                    std::ptr::from_mut(&mut enabled).cast(),
                    &raw mut length,
                )
            },
            0
        );
        assert_eq!(enabled, 1);
    }
    server
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client.write_all(b"x").unwrap();
    let mut byte = [0];
    server.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
}

#[test]
fn expired_missing_and_invalid_paths_refuse() {
    let root = aterm_tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.sock");
    assert_eq!(
        until(&missing, Instant::now()).unwrap_err().kind(),
        ErrorKind::TimedOut
    );
    assert_eq!(
        until(&missing, deadline()).unwrap_err().raw_os_error(),
        Some(libc::ENOENT)
    );
    let stopped = crate::stop::Stop::new();
    assert!(stopped.set());
    assert_eq!(
        until_stopped(&missing, deadline(), &stopped)
            .unwrap_err()
            .kind(),
        ErrorKind::Interrupted
    );
    for path in [
        Path::new("").to_owned(),
        std::ffi::OsString::from_vec(b"bad\0path".to_vec()).into(),
        Path::new(&"x".repeat(256)).to_owned(),
    ] {
        assert_eq!(
            until(&path, deadline()).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
}

#[test]
fn a_tcp_connection_blocks_closes_on_exec_and_is_kept_out_of_children() {
    for place in [
        std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
        std::net::Ipv6Addr::LOCALHOST.into(),
    ] {
        let listener = std::net::TcpListener::bind((place, 0)).unwrap();
        let mut client = tcp_within(&listener.local_addr().unwrap(), MUST_HAPPEN).unwrap();
        let (mut server, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        assert_eq!(client.peer_addr().unwrap(), listener.local_addr().unwrap());
        // SAFETY: both fcntl calls read flags from a live test descriptor.
        let status = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
        // SAFETY: as above.
        let descriptor = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
        assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
        assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
        // `FD_CLOFORK`, which Apple's libc crate does not name.
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            descriptor & 2,
            if crate::spawn_lock::tests::clofork_supported() {
                2
            } else {
                0
            },
            "{place}: the connection agrees with the kernel's supported flags"
        );
        server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        server.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
    }
}

#[test]
fn a_tcp_connect_refused_or_given_no_time_says_so() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    assert_eq!(
        tcp_within(&addr, MUST_HAPPEN).unwrap_err().kind(),
        ErrorKind::ConnectionRefused
    );
    assert_eq!(
        tcp_within(&addr, Duration::ZERO).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn a_dial_the_partition_cuts_reaches_no_listener_until_the_cut_heals() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let cut = format!("{addr}\n");
    let refused = crate::partition::cut_here(&cut, || tcp_within(&addr, MUST_HAPPEN)).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::HostUnreachable, "{refused}");
    assert_eq!(
        crate::accept(
            &listener,
            crate::AcceptWait::Ready,
            crate::AcceptedMode::Blocking,
        )
        .unwrap_err()
        .kind(),
        ErrorKind::WouldBlock,
        "the cut dial made no connection"
    );
    let healed = crate::partition::cut_here("", || tcp_within(&addr, MUST_HAPPEN));
    assert!(healed.is_ok(), "{healed:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn full_linux_backlog_refuses_without_returning_an_unconnected_stream() {
    // Linux also reports a never-connected Unix socket writable with SO_ERROR=0.
    // That is not a pending connect: the real backlog EAGAIN below must still refuse.
    let unconnected = crate::CtlStream::from(socket(libc::AF_UNIX, libc::SOCK_STREAM).unwrap());
    unconnected.set_nonblocking(true).unwrap();
    crate::ready::until_cancelled(
        unconnected.as_fd(),
        crate::ready::Interest::Write,
        deadline(),
        None,
    )
    .unwrap();
    assert!(unconnected.take_error().unwrap().is_none());
    assert_eq!(
        unconnected.peer_addr().unwrap_err().kind(),
        ErrorKind::NotConnected
    );
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("backlog.sock");
    let listener = crate::CtlListener::bind(&path).unwrap();
    crate::keep_out_of_children(&listener).unwrap();
    // SAFETY: listen changes the queue size of this live listening socket.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
    let first = until(&path, deadline()).unwrap();
    let refused = until(&path, deadline()).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN));
    assert!(first.peer_addr().is_ok());
}

const CLOSED_VERDICT: &[u8] = b"verdict sent before the peer closes";

fn read_closed_verdict(mut stream: impl Read + AsRawFd) {
    // SAFETY: both fcntl calls read flags from the live test stream.
    let status = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above.
    let descriptor = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
    assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
    assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, CLOSED_VERDICT);
}

#[test]
fn a_unix_peer_can_finish_sending_before_connect_completion_is_checked() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("closing.sock");
    let listener = crate::CtlListener::bind(&path).unwrap();
    crate::keep_out_of_children(&listener).unwrap();
    let (accepted, was_accepted) = std::sync::mpsc::channel();
    let (release, may_close) = std::sync::mpsc::channel();
    let (closed, was_closed) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        accepted.send(()).unwrap();
        may_close.recv_timeout(MUST_HAPPEN).unwrap();
        stream.write_all(CLOSED_VERDICT).unwrap();
        drop(stream);
        closed.send(()).unwrap();
    });
    let client = until_cancelled_after_connect(&path, deadline(), None, |stream, error| {
        assert!(
            error.is_none(),
            "the fresh Unix listener connected: {error:?}"
        );
        was_accepted.recv_timeout(MUST_HAPPEN).unwrap();
        assert!(
            stream.peer_addr().is_ok(),
            "the established peer is still open"
        );
        stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        release.send(()).unwrap();
        was_closed.recv_timeout(MUST_HAPPEN).unwrap();
        // Closing the server does not require the kernel to have discarded its peer address
        // already. The contract is the queued verdict and EOF checked below, in either state.
    })
    .expect("a successful connect retains the peer's queued verdict after it closes");
    assert_eq!(crate::peer::peer_uid(&client), Some(crate::peer::our_uid()));
    read_closed_verdict(client);
    server.join().unwrap();
}

#[test]
fn a_tcp_peer_can_finish_sending_before_connect_completion_is_checked() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    crate::keep_out_of_children(&listener).unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, was_accepted) = std::sync::mpsc::channel();
    let (release, may_close) = std::sync::mpsc::channel();
    let (closed, was_closed) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        accepted.send(()).unwrap();
        may_close.recv_timeout(MUST_HAPPEN).unwrap();
        stream.write_all(CLOSED_VERDICT).unwrap();
        drop(stream);
        closed.send(()).unwrap();
    });
    let client = tcp_until_cancelled_after_connect(
        &address,
        deadline(),
        |_| Ok(()),
        None,
        |stream, error| {
            assert!(
                error.is_none()
                    || error.is_some_and(|error| {
                        matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EINTR))
                    }),
                "the fresh TCP listener connected or began connecting: {error:?}"
            );
            eprintln!("closed TCP peer initial connect result: {error:?}");
            was_accepted.recv_timeout(MUST_HAPPEN).unwrap();
            assert!(
                stream.peer_addr().is_ok(),
                "the established peer is still open"
            );
            stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            release.send(()).unwrap();
            was_closed.recv_timeout(MUST_HAPPEN).unwrap();
        },
    )
    .expect("the completed TCP connect retains the peer's queued verdict after it closes");
    read_closed_verdict(client);
    server.join().unwrap();
}
