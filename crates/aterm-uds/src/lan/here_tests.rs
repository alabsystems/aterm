// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read as _, Write as _};

const MUST_HAPPEN: Duration = Duration::from_secs(60);

#[test]
fn a_dial_with_options_reaches_a_loopback_listener() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let options = ConnectOptions {
        bound_if: None,
        source: Some(std::net::Ipv4Addr::LOCALHOST.into()),
        hop_limit: Some(8),
    };
    let (mut client, reached) = dial_any(&[addr], Instant::now() + MUST_HAPPEN, options).unwrap();
    assert_eq!(reached, addr);
    let (mut server, _) = crate::accept(
        &listener,
        crate::AcceptWait::Forever,
        crate::AcceptedMode::Blocking,
    )
    .unwrap();
    server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    client.write_all(b"x").unwrap();
    let mut byte = [0];
    server.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
    let wrong = ConnectOptions {
        source: Some(std::net::Ipv6Addr::LOCALHOST.into()),
        ..ConnectOptions::default()
    };
    assert_eq!(
        dial(&addr, Instant::now() + MUST_HAPPEN, wrong)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_udp_socket_binds_joins_and_refuses_a_named_address_another_port_or_a_unicast_group() {
    let options = UdpOptions {
        bind: "0.0.0.0:0".parse().unwrap(),
        reuse: true,
        groups: Vec::new(),
        multicast_if: None,
        ttl: Some(1),
    };
    let socket = udp(&options).unwrap();
    assert!(socket.local_addr().unwrap().port() != 0);
    for refused in ["0.0.0.0:53", "0.0.0.0:7484", "127.0.0.1:0", "[::1]:5353"] {
        let refused = UdpOptions {
            bind: refused.parse().unwrap(),
            ..options.clone()
        };
        assert_eq!(
            check_udp(&refused).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
            "{refused:?}"
        );
    }
    for allowed in [
        "0.0.0.0:5353",
        "224.0.0.251:5353",
        "0.0.0.0:41384",
        "239.255.31.84:41384",
        "0.0.0.0:41385",
        "239.255.31.85:41385",
        "[::]:0",
        "[ff02::fb]:5353",
    ] {
        let allowed = UdpOptions {
            bind: allowed.parse().unwrap(),
            ..options.clone()
        };
        check_udp(&allowed).unwrap();
    }
    let unicast = UdpOptions {
        groups: vec![("10.0.0.1".parse().unwrap(), None)],
        ..options
    };
    assert_eq!(
        udp(&unicast).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn resolutions_past_the_bound_are_refused_until_one_ends() {
    static SLOTS: Slots = Slots::new(2);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Arc::new(std::sync::Mutex::new(held));
    let hang = || {
        let held = std::sync::Arc::clone(&held);
        move |_: &str, _: u16| {
            let _ = held.lock().unwrap().recv();
            Ok(Vec::new())
        }
    };
    for _ in 0..2 {
        assert_eq!(
            resolve_in(&SLOTS, "hangs", 7, Duration::from_millis(1), hang())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
    let refused = resolve_in(&SLOTS, "third", 7, MUST_HAPPEN, hang()).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN), "{refused}");
    release.send(()).unwrap();
    release.send(()).unwrap();
    let answered = |_: &str, _: u16| Ok(vec![SocketAddr::from(([127, 0, 0, 1], 7))]);
    let start = Instant::now();
    let resolved = loop {
        match resolve_in(&SLOTS, "fourth", 7, MUST_HAPPEN, answered) {
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {
                assert!(
                    start.elapsed() < MUST_HAPPEN,
                    "the resolving slots never returned"
                );
                crate::deadline::poll_yield();
            }
            other => break other.unwrap(),
        }
    };
    assert_eq!(resolved, vec![SocketAddr::from(([127, 0, 0, 1], 7))]);
}

/// A literal address answers at once with every slot held by a lookup that never ends, as the
/// system resolver would answer it, and is never looked up; a name still meets the bound. A
/// vault publish to 33 loopback mirrors (`aterm-secrets`' approver queue tests) lost the
/// 33rd PUT to `EAGAIN` this way, and the mirror it missed answered a join `404`.
///
/// NEGATIVE CONTROL: drop the literal answer from `resolve_in`: each literal is refused
/// `EAGAIN` behind the held slot, and the unwrap fails.
#[test]
fn a_literal_address_answers_with_every_slot_taken() {
    static SLOTS: Slots = Slots::new(1);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let hang = move |_: &str, _: u16| {
        let _ = held.recv();
        Ok(Vec::new())
    };
    assert_eq!(
        resolve_in(&SLOTS, "hangs", 7, Duration::from_millis(1), hang)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    let unasked =
        |name: &str, _: u16| -> io::Result<Vec<SocketAddr>> { panic!("{name} was looked up") };
    for literal in ["127.0.0.1", "::1", "192.0.2.7", "fe80::1"] {
        let expected: Vec<SocketAddr> = (literal, 7).to_socket_addrs().unwrap().collect();
        assert_eq!(
            resolve_in(&SLOTS, literal, 7, MUST_HAPPEN, unasked).unwrap(),
            expected,
            "{literal}"
        );
    }
    let refused = resolve_in(&SLOTS, "localhost", 7, MUST_HAPPEN, unasked).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN), "{refused}");
    release.send(()).unwrap();
}

#[test]
fn localhost_resolves_here_and_a_zero_bound_is_refused() {
    let addrs = resolve("localhost", 7, MUST_HAPPEN).unwrap();
    assert!(
        addrs
            .iter()
            .all(|addr| addr.ip().is_loopback() && addr.port() == 7)
    );
    assert!(!addrs.is_empty());
    assert_eq!(
        resolve("localhost", 7, Duration::ZERO).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}
