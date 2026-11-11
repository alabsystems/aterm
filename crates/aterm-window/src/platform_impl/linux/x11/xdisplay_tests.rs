// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/xdisplay.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::io::{ErrorKind, Write as _};
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;

use super::{socket_end, ConnectionEnd};

/// [`socket_end`] once a closed peer's close has reached `fd`.
fn once_closed(fd: std::os::raw::c_int) -> std::io::Error {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let end = socket_end(fd, "the witness");
        if end.to_string() != "the witness" {
            return end;
        }
        assert!(std::time::Instant::now() < deadline, "the peer's close never came");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A socket with nothing to say: the witness's word stands.
fn witness(seen: &'static str) -> std::io::Error {
    std::io::Error::other(seen)
}

fn said(end: Option<ConnectionEnd>) -> String {
    match end {
        Some(ConnectionEnd::Transport(why)) => why.to_string(),
        other => panic!("not a failed transport: {other:?}"),
    }
}

/// Only a failed transport is the display's loss: libxcb's stream error,
/// Xlib's IO error, or a socket the poll saw hang up.
#[test]
fn only_a_failed_transport_is_the_end_of_the_server() {
    let transport = aterm_x11::Error::Connection(1);
    let too_long = aterm_x11::Error::Connection(4);
    let ended = |status, xlib, hung_up| ConnectionEnd::of(status, xlib, hung_up, witness);
    for (xlib, hung_up) in [(false, false), (true, false), (false, true)] {
        assert!(matches!(
            ended(Err(transport.clone()), xlib, hung_up),
            Some(ConnectionEnd::Transport(_))
        ));
        assert!(matches!(
            ended(Err(too_long.clone()), xlib, hung_up),
            Some(ConnectionEnd::Closed(aterm_x11::Error::Connection(4)))
        ));
    }
    assert!(said(ended(Err(transport.clone()), true, true)).contains("XCB_CONN_ERROR"));
    assert!(said(ended(Ok(()), true, true)).contains("Xlib"));
    assert!(said(ended(Ok(()), false, true)).contains("poll"));
    assert!(ended(Ok(()), false, false).is_none());
}

/// What the socket says of a dead server: end of file once it closed,
/// `ECONNRESET` when it closed on requests of ours it never read.
#[test]
fn the_socket_says_how_the_server_went() {
    let (ours, server) = UnixStream::pair().expect("socketpair");
    let quiet = socket_end(ours.as_raw_fd(), "the witness");
    assert_eq!(quiet.kind(), ErrorKind::Other, "{quiet}");
    assert_eq!(quiet.to_string(), "the witness");
    drop(server);
    let closed = once_closed(ours.as_raw_fd());
    assert_eq!(closed.kind(), ErrorKind::UnexpectedEof, "{closed}");

    let (mut ours, server) = UnixStream::pair().expect("socketpair");
    ours.write_all(b"a request").expect("write");
    drop(server);
    let reset = once_closed(ours.as_raw_fd());
    assert_eq!(reset.raw_os_error(), Some(libc::ECONNRESET), "{reset}");

    let (ours, mut server) = UnixStream::pair().expect("socketpair");
    server.write_all(b"an event").expect("write");
    drop(server);
    let unread = socket_end(ours.as_raw_fd(), "the witness");
    assert_eq!(unread.to_string(), "the witness");
}
