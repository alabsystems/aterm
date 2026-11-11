// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener};

use aterm_spec::wait::MUST_HAPPEN;

use super::*;

#[test]
fn a_listening_port_connects_and_the_stream_blocks() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let addr = listener.local_addr().expect("addr");
    let mut stream = connect_timeout(&addr, MUST_HAPPEN).expect("connects");
    let (mut peer, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accepts");
    peer.write_all(b"x").expect("writes");
    let mut byte = [0u8; 1];
    stream
        .read_exact(&mut byte)
        .expect("a blocking read waits for it");
    assert_eq!(&byte, b"x");
}

/// A select that returned is a connection only for a writable socket outside the failed set;
/// a failed one is an error even where winsock left no pending one. NEGATIVE CONTROL: writable
/// and not failed connects without reading the pending error.
#[test]
fn only_a_writable_unfailed_select_is_a_connection() {
    let none = || Ok(None);
    let refused = || Ok(Some(io::Error::from(io::ErrorKind::ConnectionRefused)));
    assert!(selected(1, 0, || panic!("a connection reads no pending error")).is_ok());
    for (writable, failed) in [(0, 1), (1, 1), (0, 0)] {
        let err = selected(writable, failed, none).expect_err("no connection");
        assert_eq!(
            err.kind(),
            io::ErrorKind::Other,
            "{writable} {failed}: {err}"
        );
        let err = selected(writable, failed, refused).expect_err("no connection");
        assert_eq!(
            err.kind(),
            io::ErrorKind::ConnectionRefused,
            "{writable} {failed}: {err}"
        );
    }
}

#[test]
fn a_closed_port_is_refused() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let err = connect_timeout(&addr, MUST_HAPPEN).expect_err("nothing listens");
    assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused, "{err}");
}

#[test]
fn a_connect_the_partition_cuts_reaches_no_listener_until_the_cut_heals() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    let addr = listener.local_addr().expect("addr");
    let cut = format!("{addr}\n");
    let err = aterm_uds::partition::cut_here(&cut, || connect_timeout(&addr, MUST_HAPPEN))
        .expect_err("cut");
    assert_eq!(err.kind(), io::ErrorKind::HostUnreachable, "{err}");
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect_err("nothing came")
        .kind(),
        io::ErrorKind::WouldBlock,
        "the cut connect made no connection"
    );
    let healed = aterm_uds::partition::cut_here("", || connect_timeout(&addr, MUST_HAPPEN));
    assert!(healed.is_ok(), "{healed:?}");
}

#[test]
fn a_zero_budget_is_invalid_input() {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
    let err = connect_timeout(&addr, Duration::ZERO).expect_err("zero");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
}
