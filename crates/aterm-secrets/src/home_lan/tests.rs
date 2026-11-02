// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: u64 = 1_800_000_000_000;
const HOME_GW: &str = "a0b1c2d3e4f5";

fn on(name: &str, ip: [u8; 4], mask: [u8; 4]) -> InterfaceAddress {
    InterfaceAddress {
        name: name.into(),
        address: Ipv4Addr::from(ip).into(),
        netmask: Some(Ipv4Addr::from(mask).into()),
        flags: 0,
    }
}

/// `interfaces` with the default route leaving from the first one that is not loopback.
fn facts(interfaces: Vec<InterfaceAddress>, gateway: Option<&str>) -> Facts {
    let primary = interfaces.iter().find_map(|address| match address.address {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    });
    Facts {
        interfaces,
        primary,
        gateway: gateway.map(str::to_owned),
        expensive: false,
    }
}

/// m22 on the home LAN: Wi-Fi at 192.168.1.20/24, loopback beside it.
fn home() -> Facts {
    facts(
        vec![
            on("lo0", [127, 0, 0, 1], [255, 0, 0, 0]),
            on("en0", [192, 168, 1, 20], [255, 255, 255, 0]),
        ],
        Some(HOME_GW),
    )
}

fn answer(node: &str, addr: [u8; 4], via: [u8; 4], gateway: &str, ms: u64) -> Answer {
    Answer {
        node: node.to_owned(),
        addr: Ipv4Addr::from(addr),
        via: Ipv4Addr::from(via),
        iface: "en0".to_owned(),
        gateway: Some(gateway.to_owned()),
        ms,
    }
}

/// Three members that proved their nodes on the home LAN a minute ago.
fn members_at_home() -> Vec<Answer> {
    (0..3u8)
        .map(|n| {
            answer(
                &format!("node{n}"),
                [192, 168, 1, 30 + n],
                [192, 168, 1, 20],
                HOME_GW,
                NOW - 60_000,
            )
        })
        .collect()
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// A Mac on the home LAN, where three members proved their nodes, observes a peer on its subnet
/// at home, and says it is home there (the tag a listing carries is the subnet's, never an
/// address).
///
/// NEGATIVE CONTROL: `home_at` with its count compared to `MEMBERS + 1` returns
/// `TooFew { heard: 3 }` and this test fails.
#[test]
fn a_home_lan_peer_with_member_evidence_is_observed_home() {
    let tag = HomeTag::of(Ipv4Addr::new(192, 168, 1, 0), 24);
    assert_eq!(
        observed(
            &home(),
            &members_at_home(),
            ip("192.168.1.20"),
            ip("192.168.1.40"),
            NOW
        ),
        Ok(tag.clone())
    );
    // An IPv4-mapped arrival and peer read as IPv4.
    assert_eq!(
        observed(
            &home(),
            &members_at_home(),
            ip("::ffff:192.168.1.20"),
            ip("::ffff:192.168.1.41"),
            NOW
        ),
        Ok(tag.clone())
    );
    assert_eq!(here(&home(), &members_at_home(), NOW), vec![tag.clone()]);
    assert_eq!(HomeTag::parse(tag.as_str()), Some(tag.clone()));
    assert!(!tag.as_str().contains("192"));
}

/// An attacker on a phone hotspot shared with m22 is never observed at home, even where three
/// members answered on that hotspot; nor is one on a network the facts call expensive.
///
/// NEGATIVE CONTROL: with `home_at`'s hotspot check removed the hotspot peer reads
/// `Ok(<tag of 172.20.10.0/28>)` and this test fails.
#[test]
fn a_hotspot_peer_is_refused_whatever_answered() {
    let hotspot = facts(
        vec![on("en0", [172, 20, 10, 3], [255, 255, 255, 240])],
        Some("0a0b0c0d0e0f"),
    );
    let answered: Vec<Answer> = (0..3u8)
        .map(|n| {
            answer(
                &format!("node{n}"),
                [172, 20, 10, 4 + n],
                [172, 20, 10, 3],
                "0a0b0c0d0e0f",
                NOW - 1_000,
            )
        })
        .collect();
    assert_eq!(
        observed(
            &hotspot,
            &answered,
            ip("172.20.10.3"),
            ip("172.20.10.9"),
            NOW
        ),
        Err(Away::Hotspot)
    );
    assert!(here(&hotspot, &answered, NOW).is_empty());
    let mut expensive = home();
    expensive.expensive = true;
    assert_eq!(
        observed(
            &expensive,
            &members_at_home(),
            ip("192.168.1.20"),
            ip("192.168.1.40"),
            NOW
        ),
        Err(Away::Expensive)
    );
}

/// A café network where m22 sits with one other member (who answered three times) is no home:
/// one member counts once, and two are fewer than three.
///
/// NEGATIVE CONTROL: with `nodes.dedup()` removed in `home_at` the repeated member counts three
/// times, the café reads home, and this test fails.
#[test]
fn a_cafe_peer_beside_too_few_members_is_refused() {
    let cafe = facts(
        vec![on("en0", [10, 0, 0, 5], [255, 255, 255, 0])],
        Some("c0ffeec0ffee"),
    );
    let answered = vec![
        answer(
            "m7node",
            [10, 0, 0, 6],
            [10, 0, 0, 5],
            "c0ffeec0ffee",
            NOW - 1_000,
        ),
        answer(
            "m7node",
            [10, 0, 0, 6],
            [10, 0, 0, 5],
            "c0ffeec0ffee",
            NOW - 2_000,
        ),
        answer(
            "m7node",
            [10, 0, 0, 7],
            [10, 0, 0, 5],
            "c0ffeec0ffee",
            NOW - 3_000,
        ),
        answer(
            "m9node",
            [10, 0, 0, 8],
            [10, 0, 0, 5],
            "c0ffeec0ffee",
            NOW - 4_000,
        ),
    ];
    assert_eq!(
        observed(&cafe, &answered, ip("10.0.0.5"), ip("10.0.0.66"), NOW),
        Err(Away::TooFew { heard: 2 })
    );
}

/// No evidence is no home: none at all, evidence older than `FRESH`, evidence heard behind
/// another router (the same subnet numbered by a café's), evidence heard through an address this
/// Mac no longer holds, and a router not read yet all fail closed.
///
/// NEGATIVE CONTROLS: with the gateway comparison removed from `home_at`, the café that numbers
/// its hosts like home reads home and this test fails; with the `FRESH` comparison removed, the
/// stale evidence reads home and it fails.
#[test]
fn no_member_evidence_fails_closed() {
    let peer = (ip("192.168.1.20"), ip("192.168.1.40"));
    let judge = |facts: &Facts, answers: &[Answer]| observed(facts, answers, peer.0, peer.1, NOW);
    assert_eq!(judge(&home(), &[]), Err(Away::TooFew { heard: 0 }));
    let stale: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer {
            ms: NOW - FRESH.as_millis() as u64,
            ..a
        })
        .collect();
    assert_eq!(judge(&home(), &stale), Err(Away::TooFew { heard: 0 }));
    let ahead: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer {
            ms: NOW + 2 * AHEAD_MS,
            ..a
        })
        .collect();
    assert_eq!(judge(&home(), &ahead), Err(Away::TooFew { heard: 0 }));
    // A café that numbers its hosts 192.168.1.0/24 too, behind another router.
    let lookalike = facts(
        vec![on("en0", [192, 168, 1, 20], [255, 255, 255, 0])],
        Some("0badc0ffee00"),
    );
    assert_eq!(
        judge(&lookalike, &members_at_home()),
        Err(Away::TooFew { heard: 0 })
    );
    // The evidence came through 192.168.1.21, which this Mac no longer holds.
    let moved = facts(
        vec![on("en0", [192, 168, 1, 20], [255, 255, 255, 0])],
        Some(HOME_GW),
    );
    let elsewhere: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer {
            via: Ipv4Addr::new(192, 168, 1, 21),
            ..a
        })
        .collect();
    assert_eq!(judge(&moved, &elsewhere), Err(Away::TooFew { heard: 0 }));
    assert_eq!(
        judge(&facts(home().interfaces, None), &members_at_home()),
        Err(Away::NoGateway)
    );
    // CONTROL: the same evidence, fresh, behind the home router, reads home.
    assert!(judge(&home(), &members_at_home()).is_ok());
}

/// Only a peer on the subnet of the address it reached counts, and only on the network the
/// default route takes: a routed or relayed peer, this Mac itself, an IPv6 or loopback request,
/// and a request that reached this Mac's second network (here wired, `en7`, while the default
/// route takes Wi-Fi) are no home.
///
/// NEGATIVE CONTROL: with `observed`'s on-link comparison removed, the routed peer
/// `192.168.2.9` reads home and this test fails.
#[test]
fn only_an_on_link_peer_on_the_evidenced_subnet_is_observed_home() {
    let wired_too = facts(
        vec![
            on("en0", [192, 168, 1, 20], [255, 255, 255, 0]),
            on("en7", [10, 1, 0, 5], [255, 255, 255, 0]),
        ],
        Some(HOME_GW),
    );
    let members = members_at_home();
    let judge =
        |arrival: &str, peer: &str| observed(&wired_too, &members, ip(arrival), ip(peer), NOW);
    assert_eq!(judge("192.168.1.20", "192.168.2.9"), Err(Away::NotOnLink));
    assert_eq!(judge("192.168.1.20", "93.184.216.34"), Err(Away::NotOnLink));
    assert_eq!(judge("192.168.1.20", "10.1.0.9"), Err(Away::NotOnLink));
    assert_eq!(judge("192.168.1.20", "10.1.0.5"), Err(Away::ThisMac));
    assert_eq!(judge("127.0.0.1", "127.0.0.1"), Err(Away::NotIpv4));
    assert_eq!(judge("fe80::1", "fe80::2"), Err(Away::NotIpv4));
    assert_eq!(judge("10.1.0.5", "10.1.0.9"), Err(Away::NotOnLink));
    assert_eq!(judge("192.168.9.9", "192.168.9.10"), Err(Away::NotOnLink));
    // CONTROL: a peer on the evidenced subnet.
    assert!(judge("192.168.1.20", "192.168.1.99").is_ok());
}

/// Only the default route's Ethernet or Wi-Fi network can be home: three member VMs answering on
/// a VM bridge, members heard through a VPN's tunnel or a point-to-point link, a Mac with no
/// default route, a full-tunnel VPN beside the home Wi-Fi, and evidence heard on another
/// interface than the one that holds the address now are no home.
///
/// NEGATIVE CONTROLS: with `home_kind`'s check removed from `Facts::home_subnet`, the VM bridge
/// reads `Ok(<tag of 192.168.64.0/24>)` and this test fails; with `answer.iface == iface` removed
/// from `home_at`, the evidence heard on `en1` reads home and it fails.
#[test]
fn only_the_default_routes_ethernet_or_wifi_can_be_home() {
    let three = |subnet: [u8; 3], via: u8, iface: &str| -> Vec<Answer> {
        (0..3u8)
            .map(|n| Answer {
                iface: iface.to_owned(),
                ..answer(
                    &format!("node{n}"),
                    [subnet[0], subnet[1], subnet[2], 30 + n],
                    [subnet[0], subnet[1], subnet[2], via],
                    HOME_GW,
                    NOW - 1_000,
                )
            })
            .collect()
    };
    // A Mac whose default route takes its VM bridge, where three member VMs answered.
    let bridge = facts(
        vec![on("bridge100", [192, 168, 64, 1], [255, 255, 255, 0])],
        Some(HOME_GW),
    );
    let vms = three([192, 168, 64], 1, "bridge100");
    assert_eq!(
        observed(&bridge, &vms, ip("192.168.64.1"), ip("192.168.64.40"), NOW),
        Err(Away::Interface)
    );
    assert!(here(&bridge, &vms, NOW).is_empty());
    // A tunnel, and an `en` link that is point to point.
    for (name, flags) in [("utun4", IFF_POINTOPOINT), ("en5", IFF_POINTOPOINT)] {
        let tunnel = facts(
            vec![InterfaceAddress {
                flags,
                ..on(name, [10, 8, 0, 2], [255, 255, 255, 0])
            }],
            Some(HOME_GW),
        );
        let members = three([10, 8, 0], 2, name);
        assert_eq!(
            observed(&tunnel, &members, ip("10.8.0.2"), ip("10.8.0.9"), NOW),
            Err(Away::Interface),
            "{name}"
        );
    }
    // No default route at all.
    let unrouted = Facts {
        primary: None,
        ..home()
    };
    let peer = (ip("192.168.1.20"), ip("192.168.1.40"));
    assert_eq!(
        observed(&unrouted, &members_at_home(), peer.0, peer.1, NOW),
        Err(Away::Interface)
    );
    // A full-tunnel VPN beside the home Wi-Fi: the default route takes the tunnel.
    let mut vpn = home();
    vpn.interfaces
        .push(on("utun3", [10, 8, 0, 2], [255, 255, 255, 255]));
    vpn.primary = Some(Ipv4Addr::new(10, 8, 0, 2));
    assert_eq!(
        observed(&vpn, &members_at_home(), peer.0, peer.1, NOW),
        Err(Away::Interface)
    );
    // The address the evidence came through is on another interface now.
    let rewired = facts(
        vec![on("en1", [192, 168, 1, 20], [255, 255, 255, 0])],
        Some(HOME_GW),
    );
    assert_eq!(
        observed(&rewired, &members_at_home(), peer.0, peer.1, NOW),
        Err(Away::TooFew { heard: 0 })
    );
    // CONTROL: the same evidence on the interface it came through reads home.
    assert!(observed(&home(), &members_at_home(), peer.0, peer.1, NOW).is_ok());
    assert!(home_kind("en0", 0) && home_kind("en12", 0));
    assert!(!home_kind("en", 0) && !home_kind("enx0", 0) && !home_kind("awdl0", 0));
}

/// An approver honours a listing's `home=` only from a server it reaches at home: itself
/// (loopback, or one of its own addresses, IPv4-mapped too) or a server on its home subnet. A
/// server it reaches by a route (another subnet, the Internet, a VPN's far end) or over IPv6
/// off this Mac observed nothing it can stand behind, nor does any server while it is away.
///
/// NEGATIVE CONTROL: with `serving` returning `here` whatever `server` is, the routed server
/// `10.8.0.1` gives the home tag and this test fails.
#[test]
fn an_approver_honours_home_only_from_a_server_it_reaches_at_home() {
    let tag = HomeTag::of(Ipv4Addr::new(192, 168, 1, 0), 24);
    let members = members_at_home();
    let through = |server: &str| serving(&home(), &members, ip(server), NOW);
    for server in [
        "127.0.0.1",
        "::1",
        "192.168.1.20",
        "192.168.1.50",
        "::ffff:192.168.1.50",
    ] {
        assert_eq!(through(server), vec![tag.clone()], "{server}");
    }
    for server in ["10.8.0.1", "192.168.2.9", "93.184.216.34", "fe80::1"] {
        assert!(through(server).is_empty(), "{server}");
    }
    assert!(serving(&home(), &[], ip("127.0.0.1"), NOW).is_empty());
}

/// The fleet heartbeat's notes keep the newest answer per member, tie each to the address this
/// Mac reached it through and the router then, pass over a member reached from no subnet of
/// this Mac and a node id the file cannot hold, and read back as evidence.
///
/// NEGATIVE CONTROL: with `note_with` keeping every answer (no `retain` of the noted nodes),
/// `node0` reads back twice and this test fails.
#[test]
fn notes_keep_the_newest_answer_per_member_and_read_back_as_evidence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let first: Vec<(String, IpAddr)> = (0..3u8)
        .map(|n| (format!("node{n}"), ip(&format!("192.168.1.{}", 30 + n))))
        .chain([
            ("routed".to_owned(), ip("172.16.5.5")),
            ("Bad Node".to_owned(), ip("192.168.1.50")),
        ])
        .collect();
    note_with(&paths, &first, &home(), NOW - 120_000).unwrap();
    note_with(
        &paths,
        &[("node0".to_owned(), ip("192.168.1.30"))],
        &home(),
        NOW - 60_000,
    )
    .unwrap();
    let read = read(&paths);
    let mut nodes: Vec<&str> = read.iter().map(|a| a.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, ["node0", "node1", "node2"]);
    let node0 = read.iter().find(|a| a.node == "node0").unwrap();
    assert_eq!(node0.ms, NOW - 60_000);
    assert_eq!(node0.via, Ipv4Addr::new(192, 168, 1, 20));
    assert_eq!(node0.iface, "en0");
    assert_eq!(node0.gateway.as_deref(), Some(HOME_GW));
    assert!(observed(&home(), &read, ip("192.168.1.20"), ip("192.168.1.40"), NOW).is_ok());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(paths.dir.join(FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// The test place says every IPv4 request is home under the test tag; the empty place says no
/// request is.
#[test]
fn the_test_places_say_what_they_name() {
    let peer = ip("127.0.0.1");
    assert_eq!(AtHome.observed(peer, peer), Ok(HomeTag::test()));
    assert_eq!(AtHome.here(), vec![HomeTag::test()]);
    assert_eq!(AtHome.serving(peer), vec![HomeTag::test()]);
    assert!(Nowhere.observed(peer, peer).is_err());
    assert!(Nowhere.here().is_empty());
    assert!(Nowhere.serving(peer).is_empty());
}
