// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn link(index: u16, kind: u8) -> Vec<u8> {
    let mut link = vec![0; 20];
    link[0] = 20;
    link[1] = AF_LINK;
    link[2..4].copy_from_slice(&index.to_ne_bytes());
    link[4] = kind;
    link
}
fn inet(ip: [u8; 4]) -> Vec<u8> {
    let mut address = vec![0; 16];
    address[0] = 16;
    address[1] = AF_INET;
    address[4..8].copy_from_slice(&ip);
    address
}
fn reply() -> Vec<u8> {
    let mut bytes = vec![0; HEADER];
    bytes[2] = 5;
    bytes[3] = 4;
    bytes[4..6].copy_from_slice(&3_u16.to_ne_bytes());
    bytes[8..12].copy_from_slice(&5_u32.to_ne_bytes());
    bytes[12..16].copy_from_slice(&0x37_u32.to_ne_bytes());
    bytes[16..20].copy_from_slice(&77_u32.to_ne_bytes());
    bytes[20..24].copy_from_slice(&88_u32.to_ne_bytes());
    bytes.extend_from_slice(&inet([192, 0, 2, 2]));
    bytes.extend_from_slice(&link(3, IFT_ETHER));
    // A short sockaddr netmask is valid, with four-byte alignment padding.
    bytes.extend_from_slice(&[7, 0, 0, 0, 255, 255, 255, 0]);
    bytes.extend_from_slice(&link(3, IFT_ETHER));
    bytes.extend_from_slice(&inet([192, 0, 2, 1]));
    let len = bytes.len() as u16;
    bytes[..2].copy_from_slice(&len.to_ne_bytes());
    bytes
}

#[test]
fn direct_route_decoding_and_exact_reply_identity() {
    let bytes = reply();
    let route = parse(&bytes, 88, 77).unwrap().unwrap();
    assert!(route.direct);
    assert_eq!(route.index, 3);
    assert_eq!(route.source, Some("192.0.2.1".parse().unwrap()));
    assert_eq!(parse(&bytes, 89, 77).unwrap(), None);
    assert_eq!(parse(&bytes, 88, 78).unwrap(), None);
    let mut router = bytes.clone();
    router[8..12].copy_from_slice(&(5_u32 | 0x1000_0000).to_ne_bytes());
    assert!(parse(&router, 88, 77).unwrap().unwrap().direct);
    for denied in [
        0x2_u32, 0x8, 0x10, 0x20, 0x1000, 0x200000, 0x400000, 0x800000, 0x2000000, 0x8000000,
        0x20000000, 0x40000000,
    ] {
        let mut changed = bytes.clone();
        changed[8..12].copy_from_slice(&(5 | denied).to_ne_bytes());
        assert!(!parse(&changed, 88, 77).unwrap().unwrap().direct);
    }
}

#[test]
fn unrelated_short_kernel_notifications_do_not_abort_a_query() {
    for (kind, length) in [(0x0c, 68usize), (0x0f, 52)] {
        let mut bytes = vec![0; length];
        bytes[..2].copy_from_slice(&(length as u16).to_ne_bytes());
        bytes[2] = 5;
        bytes[3] = kind;
        assert_eq!(parse(&bytes, 88, 77).unwrap(), None);
    }
}

#[test]
fn an_interface_address_cannot_name_another_ipv6_zone() {
    let mut bytes = reply();
    bytes.truncate(bytes.len() - 16);
    let mut address = vec![0; 28];
    address[0] = 28;
    address[1] = AF_INET6;
    address[8..24].copy_from_slice(&"fe80::1".parse::<Ipv6Addr>().unwrap().octets());
    address[24..28].copy_from_slice(&9_u32.to_ne_bytes());
    bytes.extend_from_slice(&address);
    let length = bytes.len() as u16;
    bytes[..2].copy_from_slice(&length.to_ne_bytes());
    assert!(parse(&bytes, 88, 77).is_err());
    let start = bytes.len() - 4;
    bytes[start..].copy_from_slice(&3_u32.to_ne_bytes());
    assert!(parse(&bytes, 88, 77).unwrap().unwrap().direct);
}

#[test]
fn malformed_route_frames_and_conflicting_interfaces_fail_closed() {
    let bytes = reply();
    for end in 0..bytes.len() {
        assert!(parse(&bytes[..end], 88, 77).is_err());
    }
    let mut wrong = bytes.clone();
    wrong[4..6].copy_from_slice(&9_u16.to_ne_bytes());
    assert!(parse(&wrong, 88, 77).is_err());
    // A lookup the kernel refused keeps the kernel's errno, as the failed write of a plain
    // RTM_GET does: "no route" is ESRCH either way.
    let mut wrong = bytes.clone();
    wrong[24..28].copy_from_slice(&libc::ESRCH.to_ne_bytes());
    assert_eq!(
        parse(&wrong, 88, 77).unwrap_err().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut wrong = bytes.clone();
    wrong[2] = 6;
    assert!(parse(&wrong, 88, 77).is_err());
    let mut wrong = bytes.clone();
    wrong.push(0);
    let len = wrong.len() as u16;
    wrong[..2].copy_from_slice(&len.to_ne_bytes());
    assert!(parse(&wrong, 88, 77).is_err());
    // Correct header, but the next sockaddr extends beyond the record.
    let mut wrong = bytes;
    wrong[HEADER] = 255;
    assert!(parse(&wrong, 88, 77).is_err());
}

#[test]
fn scoped_requests_are_native_bounded_and_read_only() {
    let peer: SocketAddr = "[fe80::2%3]:99".parse().unwrap();
    let bytes = request(peer, 88, 77, None, RTM_GET_SILENT).unwrap();
    assert_eq!(bytes[3], 0x11);
    let plain = request(peer, 88, 77, None, RTM_GET).unwrap();
    assert_eq!(plain[3], 0x4);
    assert_eq!(
        (&plain[..3], &plain[4..]),
        (&bytes[..3], &bytes[4..]),
        "the two lookups differ in their type alone"
    );
    // RTM_ADD through RTM_LOCK, and whatever else is no lookup, is never written.
    for kind in (0..=u8::MAX).filter(|kind| ![0x4, 0x11].contains(kind)) {
        assert!(request(peer, 88, 77, None, kind).is_err(), "{kind:#x}");
    }
    assert_eq!(u16_at(&bytes, 4).unwrap(), 3);
    assert_eq!(u32_at(&bytes, 8).unwrap(), 0x0100_0000);
    assert_eq!(u32_at(&bytes, 12).unwrap(), 0x11);
    assert_eq!(&bytes[HEADER + 10..HEADER + 12], &3_u16.to_be_bytes());
    assert_eq!(u32_at(&bytes, HEADER + 24).unwrap(), 0);
    assert_eq!(usize::from(u16_at(&bytes, 0).unwrap()), bytes.len());
    assert!(
        request(
            "[fe80::2%65536]:99".parse().unwrap(),
            1,
            1,
            None,
            RTM_GET_SILENT
        )
        .is_err()
    );
    let bound = request(
        "192.0.2.1:99".parse().unwrap(),
        88,
        77,
        Some(3),
        RTM_GET_SILENT,
    )
    .unwrap();
    assert_eq!(u16_at(&bound, 4).unwrap(), 3);
    assert_eq!(u32_at(&bound, 8).unwrap(), 0x0100_0000);
    assert!(request(peer, 88, 77, Some(4), RTM_GET_SILENT).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn live_kernel_loopback_route_is_readable_without_connecting() {
    let route = lookup(
        None,
        "127.0.0.1:9".parse().unwrap(),
        None,
        super::super::deadline(),
    )
    .unwrap();
    assert_ne!(route.index, 0);
    assert!(!route.direct);
    assert!(!physical("lo0").unwrap());
    assert!(!physical("utun0").unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn live_link_local_route_uses_the_kernel_embedded_zone() {
    let index = super::super::index("lo0").unwrap();
    let peer = SocketAddr::V6(std::net::SocketAddrV6::new(
        "fe80::1".parse().unwrap(),
        9,
        0,
        index,
    ));
    for bound in [None, Some(index)] {
        let route = lookup(None, peer, bound, super::super::deadline()).unwrap();
        assert_eq!(route.index, index);
        assert!(!route.direct);
    }
}

/// Either type of lookup reads the same route through [`lookup_as`], or is refused with the same
/// errno: loopback, loopback's link-local address, a destination the default route takes, this
/// Mac's own default gateway where it has one (a LAN peer, asked of the routing table with no
/// packet) and an IPv6 destination, which a Mac with no unscoped IPv6 default refuses with
/// `ESRCH` in the silent lookup's answer and in the plain one's write.
///
/// NEGATIVE CONTROL (2026-11-12, on a Mac with no unscoped IPv6 default): with `parse` reading a
/// refused answer as `refuse("Darwin rejected the route query")`, the silent lookup's "no route"
/// loses its errno and this fails for the IPv6 destination (`left: Err((None,
/// PermissionDenied, ..))`, `right: Err((Some(3), ..))`).
#[cfg(target_os = "macos")]
#[test]
fn either_type_of_lookup_reads_the_same_route_or_the_same_refusal() {
    let lo0 = super::super::index("lo0").unwrap();
    let mut peers: Vec<SocketAddr> = vec![
        "127.0.0.1:9".parse().unwrap(),
        SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::1".parse().unwrap(),
            9,
            0,
            lo0,
        )),
        "192.0.2.1:9".parse().unwrap(),
        "[2001:db8::1]:9".parse().unwrap(),
    ];
    let primary = aterm_uds::network::default_routes().primary;
    if let Some(gateway) = primary.and_then(|primary| primary.gateway) {
        peers.push(SocketAddr::new(gateway.into(), 9));
    }
    let outcome = |route: io::Result<Route>| {
        route.map_err(|error| (error.raw_os_error(), error.kind(), error.to_string()))
    };
    for peer in peers {
        let asked = |kind: u8| {
            outcome(lookup_as(peer, None, super::super::deadline(), |write| {
                write(kind)
            }))
        };
        let plain = asked(RTM_GET);
        assert_eq!(asked(RTM_GET_SILENT), plain, "{peer}");
        // The lookup the process makes is one of the two.
        let made = lookup(None, peer, None, super::super::deadline());
        assert_eq!(outcome(made), plain, "{peer}");
    }
}

/// The routing messages that can change an attachment move the interface generation, and the
/// route-table ones, which an idle Mac's ARP and route lookups send all the time, do not.
#[test]
fn interface_and_address_messages_move_the_interface_generation() {
    // RTM_NEWADDR, RTM_DELADDR, RTM_IFINFO, RTM_NEWMADDR, RTM_DELMADDR, RTM_IFINFO2, and a type
    // this build does not know.
    for kind in [0xc, 0xd, 0xe, 0xf, 0x10, 0x12, 0x13, 0x40] {
        assert!(moves_interfaces(kind), "{kind:#x}");
    }
    // RTM_ADD through RTM_RESOLVE, and RTM_GET2.
    for kind in (0x1..=0xb).chain([0x14]) {
        assert!(!moves_interfaces(kind), "{kind:#x}");
    }
    for kind in (0x1..=0x14).chain([0x40]) {
        assert_eq!(
            route_changed(&routing_message(kind)).unwrap(),
            match kind {
                0x4 | 0x7 | 0x14 => Heard::Nothing,
                0x1..=0xb => Heard::Routes,
                _ => Heard::Attachments,
            },
            "listener validity must include route changes but no lookup, answered or failed, \
             and an attachment is read again only for what moves the generation: {kind:#x}"
        );
        assert_eq!(
            route_changed(&routing_message(kind)).unwrap() == Heard::Attachments,
            moves_interfaces(kind),
            "{kind:#x}"
        );
    }
    for end in 0..HEADER {
        assert!(route_changed(&routing_message(0xe)[..end]).is_err());
    }
}

/// The routing socket opens with no privilege, so a Mac's rechecks get a generation.
#[cfg(target_os = "macos")]
#[test]
fn the_interface_generation_is_read_from_a_routing_socket() {
    assert!(interface_generation().is_some());
}

/// A routing message header of type `kind`: an `rt_msghdr` with its length, version and type,
/// the rest zero.
fn routing_message(kind: u8) -> Vec<u8> {
    let mut bytes = vec![0; HEADER];
    bytes[0..2].copy_from_slice(&u16::try_from(HEADER).unwrap().to_ne_bytes());
    bytes[2] = 5;
    bytes[3] = kind;
    bytes
}

/// Whether delivering `messages` on `kernel` and then draining `watch` (at most `most` messages)
/// moved its generation.
fn moves(
    kernel: &std::os::unix::net::UnixDatagram,
    watch: &mut Watch,
    messages: &[Vec<u8>],
    most: usize,
) -> bool {
    for message in messages {
        assert_eq!(kernel.send(message).unwrap(), message.len());
    }
    let before = watch.generation;
    watch.drain(most) != before
}

/// A drain moves the interface generation for each message that can change an attachment and for
/// whatever the socket cannot vouch for, and for nothing else: crafted `rt_msghdr` headers on a
/// datagram socket pair. `RTM_NEWADDR`, `RTM_DELADDR`, `RTM_IFINFO` and `RTM_IFINFO2` move it, the
/// route-table types (`RTM_ADD` through `RTM_RESOLVE`, `RTM_GET2`) do not, alone or together; a
/// read too short to name its type moves it, as does a read that fails, a drain that reaches its
/// bound and one that finds half the receive room queued.
///
/// NEGATIVE CONTROLS (2026-11-06), each planted in [`Watch::drain`]: with the type left unread
/// (`moved |= received < 4`) `RTM_NEWADDR` moves nothing; with a failed read ending the drain
/// unmoved, the read on `/dev/null` moves nothing (`left: 0, right: 0`); with the queued bytes
/// unweighed, half the room queued moves nothing.
#[test]
fn a_drain_moves_the_generation_for_interface_messages_and_what_it_cannot_vouch_for() {
    let (kernel, socket) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let mut watch = Watch::over(socket.into(), 64 * 1024);
    assert!(!moves(&kernel, &mut watch, &[], DRAIN), "nothing heard");
    let table: Vec<Vec<u8>> = (0x1..=0xb).chain([0x14]).map(routing_message).collect();
    for message in &table {
        assert!(
            !moves(&kernel, &mut watch, std::slice::from_ref(message), DRAIN),
            "{:#x}",
            message[3]
        );
    }
    assert!(
        !moves(&kernel, &mut watch, &table, DRAIN),
        "the route table"
    );
    for kind in [0xc, 0xd, 0xe, 0x12] {
        assert!(
            moves(&kernel, &mut watch, &[routing_message(kind)], DRAIN),
            "{kind:#x}"
        );
    }
    assert!(
        moves(
            &kernel,
            &mut watch,
            &[routing_message(0xe)[..3].to_vec()],
            DRAIN
        ),
        "a short read"
    );
    assert!(
        !moves(&kernel, &mut watch, &table[..2], 3),
        "within its bound"
    );
    assert!(moves(&kernel, &mut watch, &table[..4], 3), "past its bound");
    assert!(!moves(&kernel, &mut watch, &[], DRAIN), "the rest was read");

    let (kernel, socket) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let mut watch = Watch::over(socket.into(), 2 * HEADER + 1);
    assert!(
        !moves(&kernel, &mut watch, &table[..1], DRAIN),
        "under half"
    );
    assert!(
        moves(&kernel, &mut watch, &table[..2], DRAIN),
        "half the room"
    );

    let not_a_socket = std::fs::File::open("/dev/null").unwrap();
    let mut watch = Watch::over(not_a_socket.into(), 64 * 1024);
    let before = watch.generation;
    assert_ne!(watch.drain(DRAIN), before, "a failed read");

    #[cfg(target_os = "macos")]
    {
        let (kernel, socket) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let changes = super::super::RouteChanges {
            socket: super::super::Socket(socket.into()),
            room: 64 * 1024,
        };
        assert_eq!(changes.drain().unwrap(), Heard::Nothing);
        kernel.send(&routing_message(0x4)).unwrap();
        assert_eq!(
            changes.drain().unwrap(),
            Heard::Nothing,
            "a route lookup cannot wake itself"
        );
        kernel.send(&routing_message(0x7)).unwrap();
        assert_eq!(
            changes.drain().unwrap(),
            Heard::Nothing,
            "a lookup that found no route changed none"
        );
        for (kind, heard) in [
            (0x1, Heard::Routes),
            (0x2, Heard::Routes),
            (0x3, Heard::Routes),
            (0xc, Heard::Attachments),
            (0xe, Heard::Attachments),
        ] {
            kernel.send(&routing_message(kind)).unwrap();
            let mut ready = libc::pollfd {
                fd: changes.fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: ready is one live initialized pollfd, and changes owns the descriptor.
            assert_eq!(unsafe { libc::poll(&raw mut ready, 1, 0) }, 1);
            assert_eq!(changes.drain().unwrap(), heard, "{kind:#x}");
            assert_eq!(
                changes.drain().unwrap(),
                Heard::Nothing,
                "the message was consumed"
            );
        }
        // The most any message of one drain tells stands, in either order.
        for kinds in [[0x1, 0xc], [0xc, 0x1]] {
            for kind in kinds {
                kernel.send(&routing_message(kind)).unwrap();
            }
            assert_eq!(changes.drain().unwrap(), Heard::Attachments);
        }
        kernel.send(&[0]).unwrap();
        assert!(
            changes.drain().is_err(),
            "an unreadable notification is not quiet"
        );
        let not_a_socket = std::fs::File::open("/dev/null").unwrap();
        let changes = super::super::RouteChanges {
            socket: super::super::Socket(not_a_socket.into()),
            room: 64 * 1024,
        };
        assert!(
            changes.drain().is_err(),
            "a failed event source cannot stay asleep"
        );
    }
}

/// The attachments a recheck reads (`crate::lan::local_attachments`, which every listener and
/// multicast recheck reads through) are kept while the routing socket hears nothing that moves an
/// interface, and read again once it does: its generation drained from a stand-in socket this
/// test delivers the kernel's messages to, on a clock the test holds still. 50 rechecks read once;
/// a route-table message reads nothing; `RTM_IFINFO` and then `RTM_NEWADDR` read once each.
///
/// NEGATIVE CONTROL (2026-11-06): with `local_attachments` reading under a constant generation
/// (`Some(0)` for `native::interface_generation()`), the `RTM_IFINFO` leaves the kept read
/// standing (`left: 1, right: 2`).
#[cfg(target_os = "macos")]
#[test]
fn local_attachments_are_read_again_once_the_routing_socket_moves() {
    let (kernel, socket) = std::os::unix::net::UnixDatagram::pair().unwrap();
    STAND_IN.set(Some(Watch::over(socket.into(), 64 * 1024)));
    crate::lan::FIXED_NOW.set(Some(std::time::Instant::now()));
    let lo0 = Some(crate::lan::native::index("lo0").unwrap());
    let enumerations = crate::lan::ENUMERATIONS.get();
    let recheck = |message: Option<u8>| {
        if let Some(kind) = message {
            kernel.send(&routing_message(kind)).unwrap();
        }
        crate::lan::local_attachments(None, lo0).unwrap();
        crate::lan::ENUMERATIONS.get() - enumerations
    };
    for _ in 0..50 {
        assert_eq!(recheck(None), 1, "one generation");
    }
    assert_eq!(recheck(Some(0x4)), 1, "RTM_GET");
    assert_eq!(recheck(Some(0xe)), 2, "RTM_IFINFO");
    assert_eq!(recheck(None), 2, "after RTM_IFINFO");
    assert_eq!(recheck(Some(0xc)), 3, "RTM_NEWADDR");
    STAND_IN.set(None);
    crate::lan::FIXED_NOW.set(None);
}

/// A listener that hears a change of an interface or an address on its own routing socket
/// judges by a fresh read of the attachments at once, and once more
/// [`crate::slots::ACCEPT_POLL`] later, though the socket that moves the generation (another
/// socket, here a stand-in that hears nothing) stood still: with no timer, the kept read would
/// otherwise stand until its expiry. A lookup asks for neither and does not turn the listener's
/// server: its answer (`RTM_GET`), or the `RTM_MISS` of one that found no route, which a Mac
/// with no IPv6 default route hears several times a second from any program that keeps
/// connecting toward one. A change of the route table (`RTM_ADD`, `RTM_DELETE`, `RTM_CHANGE`)
/// turns the server, as it did before, and asks for neither either: it moves no interface, the
/// kept read stands and no look is owed. Half the socket's room found queued, of whatever types,
/// is read afresh: the kernel may have dropped a message there. The listener's socket is a
/// stand-in this test delivers the messages to, on a clock it holds.
///
/// NEGATIVE CONTROLS (2026-11-12): without `forget_attachments()` in `Listener::drain_changes`,
/// the read after `RTM_NEWADDR` is the kept one (`left: 1, right: 2`); with a route-table
/// message forgetting and owing a look as an address one does (`Heard::Routes` taken with
/// `Heard::Attachments` there), `RTM_ADD` owes a look (`left: Some(..), right: None`); with
/// `RouteChanges::drain` not weighing what it found queued, half the room of `RTM_MISS` owes
/// none (`left: None, right: Some(..)`); with `RTM_MISS` read as a route change
/// (`route_changed` without its `0x7`), the miss turns the server ("a lookup changes no
/// route: 0x7").
#[cfg(target_os = "macos")]
#[test]
fn a_change_a_listener_hears_reads_the_attachments_afresh_now_and_once_more() {
    use crate::slots::ACCEPT_POLL;

    let (_silent, socket) = std::os::unix::net::UnixDatagram::pair().unwrap();
    STAND_IN.set(Some(Watch::over(socket.into(), 64 * 1024)));
    let start = std::time::Instant::now();
    crate::lan::FIXED_NOW.set(Some(start));
    let (kernel, heard) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let local = "127.0.0.1:0".parse().unwrap();
    let listener = crate::lan::Listener::bind(local, crate::lan::Scope::Loopback)
        .unwrap()
        .hearing(heard.into());
    let enumerations = crate::lan::ENUMERATIONS.get();
    let read = || {
        crate::lan::local_attachments(Some(local), None).unwrap();
        crate::lan::ENUMERATIONS.get() - enumerations
    };
    assert_eq!(read(), 1);
    assert_eq!(read(), 1, "kept");

    // RTM_GET, RTM_MISS.
    for kind in [0x4, 0x7] {
        kernel.send(&routing_message(kind)).unwrap();
        assert!(
            !listener.drain_changes().unwrap(),
            "a lookup changes no route: {kind:#x}"
        );
        assert_eq!(listener.validity_deadline(), None, "{kind:#x} owes no look");
        assert_eq!(read(), 1, "{kind:#x} leaves the kept read");
    }

    // RTM_ADD, RTM_DELETE, RTM_CHANGE.
    for kind in [0x1, 0x2, 0x3] {
        kernel.send(&routing_message(kind)).unwrap();
        assert!(
            listener.drain_changes().unwrap(),
            "a route change turns the server: {kind:#x}"
        );
        assert_eq!(listener.validity_deadline(), None, "{kind:#x} owes no look");
        listener.recheck().unwrap();
        assert_eq!(read(), 1, "{kind:#x} leaves the kept read");
    }

    kernel.send(&routing_message(0xc)).unwrap();
    assert!(listener.drain_changes().unwrap(), "RTM_NEWADDR");
    assert_eq!(read(), 2, "RTM_NEWADDR on the listener's socket alone");
    assert_eq!(listener.validity_deadline(), Some(start + ACCEPT_POLL));
    listener.recheck().unwrap();
    assert_eq!(
        listener.validity_deadline(),
        Some(start + ACCEPT_POLL),
        "a recheck before the look is due leaves it owed"
    );
    assert_eq!(read(), 2, "and reads nothing");

    crate::lan::FIXED_NOW.set(Some(start + ACCEPT_POLL));
    listener.recheck().unwrap();
    assert_eq!(listener.validity_deadline(), None, "the look was given");
    assert_eq!(read(), 3, "by a fresh read");
    assert_eq!(read(), 3, "which is kept again");

    // Route-table messages alone, half the room of them: one may have been dropped unseen.
    let (kernel, heard) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let listener = listener.hearing_in(heard.into(), 2 * HEADER);
    kernel.send(&routing_message(0x7)).unwrap();
    assert!(listener.drain_changes().unwrap());
    assert_eq!(
        listener.validity_deadline(),
        Some(start + 2 * ACCEPT_POLL),
        "a look is owed"
    );
    assert_eq!(read(), 4, "and the read is fresh");
    STAND_IN.set(None);
    crate::lan::FIXED_NOW.set(None);
}
