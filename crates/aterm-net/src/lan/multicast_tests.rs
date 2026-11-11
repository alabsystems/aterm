// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);

#[test]
fn multicast_bounds_and_permanent_withdrawal() {
    let socket = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    assert!(socket.port() > 0);
    socket.recheck().unwrap();
    assert!(socket.send(&[0; MAX_PACKET + 1]).is_err());
    socket.socket.set_multicast_ttl_v4(2).unwrap();
    assert!(socket.recheck().is_err());
    socket.socket.set_multicast_ttl_v4(1).unwrap();
    assert!(socket.send(b"withdrawn").is_err());
    assert!(socket.receive().is_err());
}

/// The hint `socket` sends to its own group comes back to it (its sends loop back).
fn hears_its_own_hint(socket: &UdpSocket, policy: impl Fn(&UdpSocket) -> io::Result<()>) {
    let port = socket.local_addr().unwrap().port();
    socket.send_to(b"hint", (GROUP, port)).unwrap();
    let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut bytes = [0; MAX_PACKET];
    let (length, from) = loop {
        match socket.recv_from(&mut bytes) {
            Ok(heard) => break heard,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{error}"),
        }
        assert!(
            std::time::Instant::now() < until,
            "the hint never came back"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(&bytes[..length], b"hint");
    assert!(from.ip().is_loopback(), "{from}");
    policy(socket).unwrap();
}

/// A loopback discovery socket reaches no LAN, so it is made here: with no LAN broker at all
/// (this thread's seam names a socket nothing serves) it binds and hears its own hint.
#[test]
fn a_loopback_socket_needs_no_broker() {
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = aterm_uds::lan::Seam {
        socket: Some(root.path().join("absent.sock")),
        ..aterm_uds::lan::Seam::default()
    };
    aterm_uds::lan::seam_here(absent, || {
        let socket = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        hears_its_own_hint(&socket.socket, |socket| {
            native::multicast::policy(socket, Ipv4Addr::LOCALHOST, None)
        });
    });
}

/// An attachment's discovery socket is the LAN broker's (on macOS; elsewhere made here):
/// bound to its interface (loopback standing in for one), configured here, it carries a hint
/// to its group and back and keeps its policy.
#[test]
fn an_attachments_socket_is_the_brokers_and_carries_a_hint() {
    let index = loopback_index().unwrap();
    let name = if cfg!(target_os = "linux") {
        "lo"
    } else {
        "lo0"
    };
    aterm_uds::lan::seam_here(crate::lan::tests::broker(), || {
        let socket =
            native::multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, index, Some(name)).unwrap();
        hears_its_own_hint(&socket, |socket| {
            native::multicast::policy(socket, Ipv4Addr::LOCALHOST, Some(index))
        });
    });
}

/// Commons' discovery group binds the fixed port its peers share
/// (`aterm_uds::lan::here::COMMONS_PORT`, group 239.255.31.84), not the kernel's choice: the
/// LAN broker must make that socket for an attachment, or Commons has no LAN on macOS.
#[test]
fn an_attachment_binds_the_commons_port_through_the_broker() {
    let index = loopback_index().unwrap();
    let name = if cfg!(target_os = "linux") {
        "lo"
    } else {
        "lo0"
    };
    let port = aterm_uds::lan::here::COMMONS_PORT;
    aterm_uds::lan::seam_here(crate::lan::tests::broker(), || {
        let socket = native::multicast::bind(
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(239, 255, 31, 84),
            port,
            index,
            Some(name),
        )
        .unwrap();
        assert_eq!(socket.local_addr().unwrap().port(), port);
        native::multicast::policy(&socket, Ipv4Addr::LOCALHOST, Some(index)).unwrap();
    });
}

#[test]
fn discovery_never_infers_loopback_or_accepts_other_scopes() {
    assert!(Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Lan).is_err());
    assert!(Multicast::bind(Ipv4Addr::UNSPECIFIED, GROUP, 0, Scope::Loopback).is_err());
    assert!(
        Multicast::bind(
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(224, 0, 0, 251),
            0,
            Scope::Loopback
        )
        .is_err()
    );
}

#[test]
fn discovery_sources_exclude_self_and_subnet_broadcast_but_allow_point_to_point_pairs() {
    let ip = |s: &str| s.parse().unwrap();
    for source in ["10.1.2.0", "10.1.2.255", "10.1.2.1", "10.2.2.3"] {
        assert!(!source_allowed(
            ip(source),
            ip("10.1.2.1"),
            ip("255.255.255.0")
        ));
    }
    assert!(source_allowed(
        ip("10.1.2.3"),
        ip("10.1.2.1"),
        ip("255.255.255.0")
    ));
    assert!(source_allowed(
        ip("10.1.2.0"),
        ip("10.1.2.1"),
        ip("255.255.255.254")
    ));
}
