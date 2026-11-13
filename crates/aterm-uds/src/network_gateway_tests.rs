// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A sockaddr_in carrying `ip`, as a `PF_ROUTE` message stores it.
fn sockaddr_in(ip: Ipv4Addr) -> Vec<u8> {
    let mut sockaddr = vec![0u8; 16];
    sockaddr[0] = 16;
    sockaddr[1] = libc::AF_INET as u8;
    sockaddr[4..8].copy_from_slice(&ip.octets());
    sockaddr
}

/// A link-layer sockaddr_dl carrying `mac` and no interface name.
fn sockaddr_dl(mac: &[u8]) -> Vec<u8> {
    let mut sockaddr = vec![0u8; 8 + mac.len()];
    sockaddr[0] = sockaddr.len() as u8;
    sockaddr[1] = libc::AF_LINK as u8;
    sockaddr[6] = mac.len() as u8; // sdl_alen; sdl_nlen stays zero
    sockaddr[8..].copy_from_slice(mac);
    sockaddr
}

/// One route message: `flags`, the present-sockaddr bitmask, and the sockaddrs each padded
/// up to four bytes the way the kernel lays them out after the `rt_msghdr`.
fn route_message(flags: i32, addrs: i32, sockaddrs: &[&[u8]]) -> Vec<u8> {
    let header = std::mem::size_of::<RtMsghdr>();
    let mut body = Vec::new();
    for sockaddr in sockaddrs {
        let sa_len = sockaddr.first().copied().unwrap_or(0);
        let advance = if sa_len == 0 {
            4
        } else {
            (usize::from(sa_len) + 3) & !3
        };
        let mut chunk = sockaddr.to_vec();
        chunk.resize(advance, 0);
        body.extend_from_slice(&chunk);
    }
    let mut message = vec![0u8; header];
    let msglen = (header + body.len()) as u16;
    message[0..2].copy_from_slice(&msglen.to_ne_bytes());
    let flags_at = std::mem::offset_of!(RtMsghdr, rtm_flags);
    let addrs_at = std::mem::offset_of!(RtMsghdr, rtm_addrs);
    message[flags_at..flags_at + 4].copy_from_slice(&flags.to_ne_bytes());
    message[addrs_at..addrs_at + 4].copy_from_slice(&addrs.to_ne_bytes());
    message.extend_from_slice(&body);
    message
}

#[test]
fn the_default_route_names_the_gateway_and_a_host_route_does_not() {
    let gw = Ipv4Addr::new(192, 168, 1, 1);
    let default = route_message(
        RTF_UP | RTF_GATEWAY,
        RTA_DST | RTA_GATEWAY,
        &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_in(gw)],
    );
    let read = |table: &[u8]| {
        default_routes_of(table, &[], |_| Some("en0".into()), |_, _| None)
            .primary
            .and_then(|route| route.gateway)
    };
    assert_eq!(read(&default), Some(gw));
    // Negative control: a host route to a non-default destination is not the default route,
    // so its gateway is never taken for the default gateway.
    let host = route_message(
        RTF_UP | RTF_GATEWAY,
        RTA_DST | RTA_GATEWAY,
        &[
            &sockaddr_in(Ipv4Addr::new(10, 0, 0, 5)),
            &sockaddr_in(Ipv4Addr::new(10, 0, 0, 1)),
        ],
    );
    assert_eq!(read(&host), None);
    // A table that leads with the host route still finds the default behind it.
    let mut both = host.clone();
    both.extend_from_slice(&default);
    assert_eq!(read(&both), Some(gw));
}

/// `message` with its interface index (`rtm_index`) set to `index`.
fn on_interface(mut message: Vec<u8>, index: u16) -> Vec<u8> {
    let at = std::mem::offset_of!(RtMsghdr, rtm_index);
    message[at..at + 2].copy_from_slice(&index.to_ne_bytes());
    message
}

/// With Wi-Fi and Ethernet both attached, the kernel keeps an interface-scoped default on
/// each and one primary: the primary alone names where traffic goes, whichever comes first
/// in the table, and each route's gateway is paired with its own interface and with that
/// interface's ARP entry, even where both gateways share an address.
/// NEGATIVE CONTROL: dropping the `RTF_IFSCOPE` test makes the scoped Ethernet default,
/// first in the table, the primary (`interface` reads `en7`), and this test fails.
#[test]
fn the_primary_default_is_the_unscoped_one_with_its_own_gateway_and_arp_entry() {
    let gw = Ipv4Addr::new(192, 168, 1, 1);
    let wifi_mac = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
    let wired_mac = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x07];
    let default_on = |index: u16, flags: i32| {
        on_interface(
            route_message(
                flags,
                RTA_DST | RTA_GATEWAY,
                &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_in(gw)],
            ),
            index,
        )
    };
    let mut table = default_on(7, RTF_UP | RTF_GATEWAY | RTF_IFSCOPE);
    table.extend_from_slice(&default_on(4, RTF_UP | RTF_GATEWAY));
    table.extend_from_slice(&default_on(4, RTF_UP | RTF_GATEWAY | RTF_IFSCOPE));
    let arp_on = |index: u16, mac: &[u8]| {
        on_interface(
            route_message(
                0,
                RTA_DST | RTA_GATEWAY,
                &[&sockaddr_in(gw), &sockaddr_dl(mac)],
            ),
            index,
        )
    };
    let mut arp = arp_on(7, &wired_mac);
    arp.extend_from_slice(&arp_on(4, &wifi_mac));
    let name = |index: u16| match index {
        4 => Some("en0".to_owned()),
        7 => Some("en7".to_owned()),
        _ => None,
    };
    let routes = default_routes_of(&table, &arp, name, |_, _| None);
    // The home-LAN evidence reader must use this same primary/interface pairing.
    // The former reader chose the first scoped route and the first matching ARP entry.
    assert_eq!(
        primary_gateway_hardware(&table, &arp, name, |_, _| None).as_deref(),
        Some(wifi_mac.as_slice())
    );
    assert_eq!(
        primary_gateway_hardware(&table, &arp_on(7, &wired_mac), name, |_, _| None),
        None
    );
    let primary = routes.primary.expect("the unscoped default");
    assert_eq!(primary.interface, "en0");
    assert_eq!(primary.gateway, Some(gw));
    assert_eq!(primary.gateway_hw.as_deref(), Some(wifi_mac.as_slice()));
    let scoped: Vec<(&str, Option<&[u8]>)> = routes
        .scoped
        .iter()
        .map(|route| (route.interface.as_str(), route.gateway_hw.as_deref()))
        .collect();
    assert_eq!(
        scoped,
        [
            ("en7", Some(wired_mac.as_slice())),
            ("en0", Some(wifi_mac.as_slice()))
        ]
    );
    // A default straight onto a link (a VPN's utun) has no IPv4 next hop.
    let utun = on_interface(
        route_message(
            RTF_UP,
            RTA_DST | RTA_GATEWAY,
            &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_dl(&[])],
        ),
        9,
    );
    let routes = default_routes_of(&utun, &arp, |_| Some("utun4".to_owned()), |_, _| None);
    let primary = routes.primary.expect("the tunnel's default");
    assert_eq!(
        (primary.interface.as_str(), primary.gateway),
        ("utun4", None)
    );
    assert_eq!(
        default_routes_of(&[], &arp, name, |_, _| None),
        DefaultRoutes::default()
    );
}

#[test]
fn the_arp_table_gives_the_gateways_hardware_address_and_nothing_for_a_stranger() {
    let gw = Ipv4Addr::new(192, 168, 1, 1);
    let mac = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
    let entry = route_message(
        0,
        RTA_DST | RTA_GATEWAY,
        &[&sockaddr_in(gw), &sockaddr_dl(&mac)],
    );
    assert_eq!(
        gateway_hardware_on(&entry, gw, 0).as_deref(),
        Some(mac.as_slice())
    );
    // Negative control: no entry names this gateway yet, so the key cannot be formed.
    assert_eq!(
        gateway_hardware_on(&entry, Ipv4Addr::new(192, 168, 1, 254), 0),
        None
    );
    // An incomplete entry (no hardware address) is no entry.
    let incomplete = route_message(
        0,
        RTA_DST | RTA_GATEWAY,
        &[&sockaddr_in(gw), &sockaddr_dl(&[])],
    );
    assert_eq!(gateway_hardware_on(&incomplete, gw, 0), None);
}

/// A gateway the ARP table's dump leaves out (macOS 27.2 dumps a third-party process no
/// neighbour entry) is looked up on its own route's interface, and only there: the primary's
/// gateway takes the hardware address the lookup on its interface's index answers, never the
/// one another interface's entry would, and an entry the dump holds is taken without a lookup.
/// NEGATIVE CONTROL: dropping the `.or_else(|| ask(gateway, index))` fallback in
/// `default_routes_of` leaves the primary's `gateway_hw` `None`, and this test fails at
/// "the lookup on the primary's own interface" (`left: None`, `right: Some([170, 187, 204,
/// 17, 34, 51])`).
#[test]
fn a_gateway_the_arp_dump_leaves_out_is_looked_up_on_its_own_interface() {
    let gw = Ipv4Addr::new(192, 168, 1, 1);
    let wifi_mac = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
    let wired_mac = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x07];
    let default_on = |index: u16, flags: i32| {
        on_interface(
            route_message(
                flags,
                RTA_DST | RTA_GATEWAY,
                &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_in(gw)],
            ),
            index,
        )
    };
    let mut table = default_on(4, RTF_UP | RTF_GATEWAY);
    table.extend_from_slice(&default_on(7, RTF_UP | RTF_GATEWAY | RTF_IFSCOPE));
    let name = |index: u16| match index {
        4 => Some("en0".to_owned()),
        7 => Some("en7".to_owned()),
        _ => None,
    };
    let asked = std::cell::RefCell::new(Vec::new());
    let ask = |at: Ipv4Addr, index: u16| {
        asked.borrow_mut().push((at, index));
        match index {
            4 => Some(Box::from(wifi_mac.as_slice())),
            7 => Some(Box::from(wired_mac.as_slice())),
            _ => None,
        }
    };
    let routes = default_routes_of(&table, &[], name, ask);
    assert_eq!(
        routes
            .primary
            .as_ref()
            .and_then(|r| r.gateway_hw.as_deref()),
        Some(wifi_mac.as_slice()),
        "the lookup on the primary's own interface"
    );
    assert_eq!(
        routes.scoped.first().and_then(|r| r.gateway_hw.as_deref()),
        Some(wired_mac.as_slice()),
        "never another interface's entry"
    );
    assert_eq!(*asked.borrow(), [(gw, 4), (gw, 7)]);
    // A lookup that finds no entry on the primary's interface leaves it unknown.
    let only_wired =
        |_: Ipv4Addr, index: u16| (index == 7).then(|| Box::from(wired_mac.as_slice()));
    assert_eq!(
        primary_gateway_hardware(&table, &[], name, only_wired),
        None
    );
    // An entry the dump holds stands: nothing is asked for it.
    let arp = on_interface(
        route_message(
            0,
            RTA_DST | RTA_GATEWAY,
            &[&sockaddr_in(gw), &sockaddr_dl(&wifi_mac)],
        ),
        4,
    );
    let never = |_: Ipv4Addr, index: u16| -> Option<Box<[u8]>> {
        assert_ne!(index, 4, "the dump's own entry is asked again");
        None
    };
    assert_eq!(
        primary_gateway_hardware(&table, &arp, name, never).as_deref(),
        Some(wifi_mac.as_slice())
    );
}

/// The gateway's lookup is scoped to the route's interface (`RTF_IFSCOPE` and its index, as
/// `route -n get -ifscope` asks), and reads a hardware address only from the answer to its
/// own lookup that is that interface's ARP entry for one host: the interface's own route
/// (what the kernel answers before ARP resolves the gateway), an entry on another interface,
/// an incomplete entry, a refused lookup, and another lookup's answer give none.
/// NEGATIVE CONTROL: dropping `&& on == index` from `route_get_link` reads the other
/// interface's entry, and this test fails at "another interface's entry" (`left:
/// Some(Some([144, 65, 178, 120, 90, 158]))`, `right: Some(None)`).
#[test]
fn a_neighbour_lookup_reads_only_its_own_interfaces_entry() {
    let gw = Ipv4Addr::new(192, 168, 1, 1);
    let mac = [0x90, 0x41, 0xb2, 0x78, 0x5a, 0x9e];
    let request = route_get_request(gw.into(), 9, Some(8), RTM_GET_SILENT);
    let field = |message: &[u8], offset: usize| {
        i32::from_ne_bytes(message[offset..offset + 4].try_into().unwrap())
    };
    let flags = field(&request, std::mem::offset_of!(RtMsghdr, rtm_flags));
    assert_eq!(flags & RTF_IFSCOPE, RTF_IFSCOPE, "scoped to the interface");
    let at = std::mem::offset_of!(RtMsghdr, rtm_index);
    assert_eq!(u16::from_ne_bytes([request[at], request[at + 1]]), 8);
    assert_eq!(
        field(
            &route_get_request(gw.into(), 9, None, RTM_GET_SILENT),
            std::mem::offset_of!(RtMsghdr, rtm_flags)
        ) & RTF_IFSCOPE,
        0,
        "an unscoped lookup names no interface"
    );

    let answer = |pid: i32, sequence: i32, errno: i32, flags: i32, index: u16, hw: &[u8]| {
        let sockaddrs: Vec<Vec<u8>> = vec![sockaddr_in(gw), sockaddr_dl(hw)];
        let sockaddrs: Vec<&[u8]> = sockaddrs.iter().map(Vec::as_slice).collect();
        let mut message = on_interface(
            route_message(flags, RTA_DST | RTA_GATEWAY, &sockaddrs),
            index,
        );
        message[std::mem::offset_of!(RtMsghdr, rtm_type)] = RTM_GET;
        for (offset, value) in [
            (std::mem::offset_of!(RtMsghdr, rtm_pid), pid),
            (std::mem::offset_of!(RtMsghdr, rtm_seq), sequence),
            (std::mem::offset_of!(RtMsghdr, rtm_errno), errno),
        ] {
            message[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        }
        message
    };
    let neighbour = RTF_UP | RTF_HOST | RTF_LLINFO | RTF_IFSCOPE;
    assert_eq!(
        route_get_link(&answer(41, 9, 0, neighbour, 8, &mac), 41, 9, 8)
            .and_then(Result::ok)
            .flatten()
            .as_deref(),
        Some(mac.as_slice()),
        "its interface's entry for the gateway"
    );
    assert_eq!(
        route_get_link(&answer(41, 9, 0, neighbour, 15, &mac), 41, 9, 8),
        Some(Ok(None)),
        "another interface's entry"
    );
    assert_eq!(
        route_get_link(&answer(41, 9, 0, RTF_UP | RTF_IFSCOPE, 8, &[]), 41, 9, 8),
        Some(Ok(None)),
        "the interface's own route, before ARP resolves the gateway"
    );
    assert_eq!(
        route_get_link(&answer(41, 9, 0, neighbour, 8, &[]), 41, 9, 8),
        Some(Ok(None)),
        "an incomplete entry"
    );
    assert_eq!(
        route_get_link(&answer(41, 9, libc::ESRCH, neighbour, 8, &mac), 41, 9, 8),
        Some(Err(libc::ESRCH)),
        "a refused lookup"
    );
    assert_eq!(
        route_get_link(&answer(42, 9, 0, neighbour, 8, &mac), 41, 9, 8),
        None
    );
    assert_eq!(
        route_get_link(&answer(41, 10, 0, neighbour, 8, &mac), 41, 9, 8),
        None
    );
}

/// `route_source_ipv4`'s lookup asks the kernel for the route to its destination with the
/// interface's slot set, as `route -n get` does, and reads the route's address only from the
/// answer to its own lookup: another process's, another sequence's, or a route change is
/// passed over, and a lookup the kernel refused or a loopback address is no source.
#[test]
fn a_route_lookup_asks_for_the_interface_and_reads_only_its_own_answer() {
    let to = Ipv4Addr::new(224, 0, 0, 251);
    let request = route_get_request(to.into(), 7, None, RTM_GET_SILENT);
    let field = |message: &[u8], offset: usize| {
        i32::from_ne_bytes(message[offset..offset + 4].try_into().unwrap())
    };
    let header = std::mem::size_of::<RtMsghdr>();
    assert_eq!(
        usize::from(u16::from_ne_bytes([request[0], request[1]])),
        request.len()
    );
    assert_eq!(request[std::mem::offset_of!(RtMsghdr, rtm_version)], 5);
    assert_eq!(
        request[std::mem::offset_of!(RtMsghdr, rtm_type)],
        RTM_GET_SILENT
    );
    let plain = route_get_request(to.into(), 7, None, RTM_GET);
    assert_eq!(plain[std::mem::offset_of!(RtMsghdr, rtm_type)], RTM_GET);
    let kind = std::mem::offset_of!(RtMsghdr, rtm_type);
    assert_eq!(
        (&plain[..kind], &plain[kind + 1..]),
        (&request[..kind], &request[kind + 1..]),
        "the two lookups differ in their type alone"
    );
    let addrs = field(&request, std::mem::offset_of!(RtMsghdr, rtm_addrs));
    assert_eq!(
        addrs,
        RTA_DST | RTA_IFP,
        "the interface's slot asks for its address"
    );
    assert_eq!(field(&request, std::mem::offset_of!(RtMsghdr, rtm_seq)), 7);
    let block = &request[header..];
    assert_eq!(
        sockaddr_ipv4(route_sockaddr(block, addrs, RTA_DST)),
        Some(to)
    );
    let ifp = route_sockaddr(block, addrs, RTA_IFP).unwrap();
    assert_eq!(
        (usize::from(ifp[0]), i32::from(ifp[1])),
        (std::mem::size_of::<libc::sockaddr_dl>(), libc::AF_LINK)
    );

    let address = Ipv4Addr::new(192, 168, 1, 20);
    let answer = |kind: u8, pid: i32, sequence: i32, errno: i32, ifa: Option<Ipv4Addr>| {
        let netmask = [0u8; 0];
        let mut sockaddrs: Vec<Vec<u8>> = vec![
            sockaddr_in(to),
            sockaddr_dl(&[1, 0, 0x5e, 0, 0, 0xfb]),
            netmask.to_vec(),
            sockaddr_dl(&[]),
        ];
        let mut addrs = RTA_DST | RTA_GATEWAY | 0x4 | RTA_IFP;
        if let Some(ifa) = ifa {
            sockaddrs.push(sockaddr_in(ifa));
            addrs |= RTA_IFA;
        }
        let sockaddrs: Vec<&[u8]> = sockaddrs.iter().map(Vec::as_slice).collect();
        let mut message = route_message(RTF_UP | RTF_HOST, addrs, &sockaddrs);
        message[std::mem::offset_of!(RtMsghdr, rtm_type)] = kind;
        for (offset, value) in [
            (std::mem::offset_of!(RtMsghdr, rtm_pid), pid),
            (std::mem::offset_of!(RtMsghdr, rtm_seq), sequence),
            (std::mem::offset_of!(RtMsghdr, rtm_errno), errno),
        ] {
            message[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        }
        message
    };
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 41, 7, 0, Some(address)), 41, 7),
        Some(Ok(Some(address))),
        "its own answer names the route's address"
    );
    // Negative controls: another process's lookup, another of its own, and a route added
    // meanwhile are not its answer.
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 42, 7, 0, Some(address)), 41, 7),
        None
    );
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 41, 8, 0, Some(address)), 41, 7),
        None
    );
    assert_eq!(
        route_get_answer(&answer(0x1, 41, 7, 0, Some(address)), 41, 7),
        None
    );
    assert_eq!(
        route_get_answer(&request[..header - 1], 41, 7),
        None,
        "too short"
    );
    // Its own answer with no source in it: the kernel's refusal keeps its errno, which tells
    // "no route" from a lookup that could not be made.
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 41, 7, libc::ESRCH, Some(address)), 41, 7),
        Some(Err(libc::ESRCH)),
        "no route"
    );
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 41, 7, 0, None), 41, 7),
        Some(Ok(None))
    );
    assert_eq!(
        route_get_answer(&answer(RTM_GET, 41, 7, 0, Some(Ipv4Addr::LOCALHOST)), 41, 7),
        Some(Ok(None))
    );
    // The silent type is a request's alone: the kernel answers either lookup as RTM_GET.
    assert_eq!(
        route_get_answer(&answer(RTM_GET_SILENT, 41, 7, 0, Some(address)), 41, 7),
        None
    );
}

/// The kernel's own answer: the routing table gives the address a UDP socket connected to the
/// same destination takes, asked as either type of lookup. The destination is TEST-NET-1
/// (`192.0.2.1`), which the default route carries off this Mac's network, so the connect asks
/// macOS nothing; both read `None` on a Mac with no default route, where the table's none is
/// still its word (`Some(None)`), never a failed ask. Loopback's route is the exception: its
/// connected socket takes `127.0.0.1`, and the lookup reads that address as none.
#[test]
fn the_route_lookup_gives_the_address_a_connected_socket_takes() {
    let connected = |to: Ipv4Addr| {
        std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .ok()
            .filter(|socket| socket.connect((to, 9)).is_ok())
            .and_then(|socket| socket.local_addr().ok())
            .and_then(|local| match local.ip() {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            })
    };
    let to = Ipv4Addr::new(192, 0, 2, 1);
    let usable = connected(to).filter(|ip| !ip.is_unspecified() && !ip.is_loopback());
    assert_eq!(route_source_ipv4(to), usable);
    assert_eq!(
        route_source_ipv4_asked(to),
        Some(usable),
        "the table was read, so its none is the connected socket's none"
    );
    for kind in [RTM_GET_SILENT, RTM_GET] {
        let asked = route_get_as(
            to.into(),
            None,
            route_get_sequence(),
            |write| write(kind),
            route_get_answer,
        );
        assert_eq!(
            table_word(asked).map(Option::flatten),
            Some(usable),
            "asked as {kind:#x}"
        );
    }
    assert_eq!(connected(Ipv4Addr::LOCALHOST), Some(Ipv4Addr::LOCALHOST));
    assert_eq!(
        route_source_ipv4(Ipv4Addr::LOCALHOST),
        None,
        "loopback's route has no address but loopback's"
    );
    assert_eq!(
        route_source_ipv4_asked(Ipv4Addr::LOCALHOST),
        Some(None),
        "which the table said, read"
    );
}
