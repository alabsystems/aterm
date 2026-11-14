// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_netplace::facts::Here;
use aterm_netplace::{Answer, Facts, PathFacts, member_key};

const NOW: u64 = 1_800_000_000_000;
const HOME_HW: [u8; 6] = [0xa0, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5];
/// When this Mac's current stay on the network began: half an hour before [`NOW`].
const STAY: u64 = NOW - 30 * 60_000;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn v4(text: &str) -> Ipv4Addr {
    text.parse().unwrap()
}

fn path(expensive: bool, link: Link) -> PathFacts {
    PathFacts {
        usable: Some(true),
        expensive,
        constrained: false,
        link,
    }
}

/// This Mac with its default route on `iface` at `addr`/`prefix` behind the gateway `gw` whose
/// hardware address is `hw` (none: unread), on a Wi-Fi path that is not expensive, holding
/// `addr` and `others`, in a stay that began at [`STAY`] on that network, with `answers`, each
/// booked on that network unless it names one.
fn on(
    (iface, addr, prefix): (&str, &str, u8),
    (gw, hw): (&str, Option<&[u8]>),
    others: &[&str],
    answers: Vec<Answer>,
) -> Now {
    let route = RouteFacts {
        interface: iface.to_owned(),
        address: v4(addr),
        prefix,
        gateway: Some(v4(gw)),
        gateway_hw: hw.map(<[u8]>::to_vec),
    };
    let mut own = vec![ip("127.0.0.1"), ip(addr)];
    own.extend(others.iter().map(|other| ip(other)));
    let answers = answers
        .into_iter()
        .map(|answer| Answer {
            network: answer.network.clone().or_else(|| NetworkId::of(&route)),
            ..answer
        })
        .collect();
    Now {
        stay_since_ms: Some(STAY),
        stay_network: NetworkId::of(&route),
        facts: Facts {
            route: Some(route),
            tunnel: None,
            path: Some(path(false, Link::Wifi)),
        },
        point_to_point: false,
        own,
        answers,
        at_ms: NOW,
    }
}

/// m22 on the home LAN: Wi-Fi at 192.168.1.20/24 behind the home router.
fn home(answers: Vec<Answer>) -> Now {
    on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(&HOME_HW)),
        &[],
        answers,
    )
}

/// `node`'s node-proven fleet answer from `addr`, through `via` on `en0`, its dial begun at
/// `began`, answered half a second later, booked on the network [`on`] puts this Mac on.
fn answer(node: &str, addr: &str, via: &str, began: u64) -> Answer {
    Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, node),
        addr: Some(ip(addr)),
        via: Some(v4(via)),
        iface: Some("en0".to_owned()),
        network: None,
        logged: true,
        began_ms: began,
        at_ms: began + 500,
    }
}

/// Three members that proved their nodes on the home LAN a minute ago.
fn members_at_home() -> Vec<Answer> {
    (0..3u8)
        .map(|n| {
            answer(
                &format!("node{n}"),
                &format!("192.168.1.{}", 30 + n),
                "192.168.1.20",
                NOW - 60_000,
            )
        })
        .collect()
}

/// Whether `now` is home on its fresh evidence alone: the rule before the 7-day condition.
fn on_evidence(now: &Now) -> Result<HomeNet, Away> {
    home_net_at(now, &HomeDays::default(), Rule { days: false })
}

/// A Mac on the home LAN, where three members proved their nodes, is home there, and holds
/// another host on its subnet (IPv4-mapped too) as on the home ALab LAN.
///
/// NEGATIVE CONTROL: `home_at` with its count compared to `MEMBERS + 1` returns
/// `TooFew { heard: 3 }` and this test fails.
#[test]
fn a_network_where_enough_members_proved_themselves_is_home() {
    let home_net = on_evidence(&home(members_at_home()));
    assert_eq!(
        home_net,
        Ok(HomeNet::at(v4("192.168.1.20"), v4("255.255.255.0")))
    );
    let home_net = home_net.unwrap();
    assert!(home_net.holds(ip("192.168.1.40")));
    // An IPv4-mapped peer reads as IPv4.
    assert!(home_net.holds(ip("::ffff:192.168.1.41")));
}

/// A phone hotspot m22 shares is never home, even where three members answered on that hotspot;
/// nor is a network the path calls expensive or cellular, one whose path this process could not
/// read, or one whose path carries no traffic.
///
/// NEGATIVE CONTROLS: with `home_at`'s hotspot check removed the hotspot reads
/// `Ok(<172.20.10.3/28>)` and this test fails; with `path.link != Link::Cellular` removed
/// from the path rule the cellular path reads home and it fails; with the path rule's
/// `is_some_and` turned to `is_none_or` (an unread path let through) it fails at "not read".
#[test]
fn a_hotspot_is_never_home_whatever_answered() {
    let answered: Vec<Answer> = (0..3u8)
        .map(|n| {
            answer(
                &format!("node{n}"),
                &format!("172.20.10.{}", 4 + n),
                "172.20.10.3",
                NOW - 1_000,
            )
        })
        .collect();
    let hotspot = on(
        ("en0", "172.20.10.3", 28),
        ("172.20.10.1", Some(&[0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f])),
        &[],
        answered,
    );
    assert_eq!(on_evidence(&hotspot), Err(Away::Hotspot));
    for (path, why) in [
        (Some(path(true, Link::Wifi)), "expensive"),
        (Some(path(false, Link::Cellular)), "cellular"),
        (None, "not read"),
        (
            Some(PathFacts {
                usable: Some(false),
                ..path(false, Link::Wifi)
            }),
            "no traffic",
        ),
        (
            Some(PathFacts {
                usable: None,
                ..path(false, Link::Wifi)
            }),
            "invalid",
        ),
    ] {
        let mut now = home(members_at_home());
        now.facts.path = path;
        assert_eq!(on_evidence(&now), Err(Away::Expensive), "{why}");
    }
    // CONTROL: the same evidence on a usable Wi-Fi path that is not expensive reads home.
    assert!(on_evidence(&home(members_at_home())).is_ok());
}

/// A café network where m22 sits with one other member (who answered three times) is no home:
/// one member counts once, and two are fewer than three.
///
/// NEGATIVE CONTROL: with `nodes.dedup()` removed in `home_at` the repeated member counts three
/// times, the café reads home, and this test fails.
#[test]
fn a_cafe_beside_too_few_members_is_no_home() {
    let cafe = on(
        ("en0", "10.0.0.5", 24),
        ("10.0.0.1", Some(&[0xc0, 0xff, 0xee, 0xc0, 0xff, 0xee])),
        &[],
        vec![
            answer("m7node", "10.0.0.6", "10.0.0.5", NOW - 1_000),
            answer("m7node", "10.0.0.6", "10.0.0.5", NOW - 2_000),
            answer("m7node", "10.0.0.7", "10.0.0.5", NOW - 3_000),
            answer("m9node", "10.0.0.8", "10.0.0.5", NOW - 4_000),
        ],
    );
    assert_eq!(on_evidence(&cafe), Err(Away::TooFew { heard: 2 }));
}

/// No evidence is no home: none at all, evidence older than `FRESH`, evidence dated ahead of the
/// clock, evidence heard behind another router (a café that numbers its hosts like home, where
/// the stay the record names is still home's), evidence heard through an address this Mac no
/// longer holds, a router not read yet, and no stay at all all fail closed.
///
/// NEGATIVE CONTROLS: with the stay's network comparison removed from `home_at` (any stay
/// counts), the café that numbers its hosts like home reads home and this test fails; with the
/// `FRESH` comparison removed, the stale evidence reads home and it fails.
#[test]
fn no_member_evidence_fails_closed() {
    assert_eq!(
        on_evidence(&home(Vec::new())),
        Err(Away::TooFew { heard: 0 })
    );
    let aged = |at_ms: u64| -> Vec<Answer> {
        members_at_home()
            .into_iter()
            .map(|a| Answer {
                began_ms: at_ms - 500,
                at_ms,
                ..a
            })
            .collect()
    };
    assert_eq!(
        on_evidence(&home(aged(NOW - FRESH.as_millis() as u64))),
        Err(Away::TooFew { heard: 0 })
    );
    assert_eq!(
        on_evidence(&home(aged(NOW + 2 * AHEAD_MS))),
        Err(Away::TooFew { heard: 0 })
    );
    // A café that numbers its hosts 192.168.1.0/24 too, behind another router, read while the
    // stay the record names is still the home network's.
    let mut lookalike = on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(&[0x0b, 0xad, 0xc0, 0xff, 0xee, 0x00])),
        &[],
        members_at_home(),
    );
    lookalike.stay_network = home(Vec::new()).stay_network;
    assert_eq!(on_evidence(&lookalike), Err(Away::TooFew { heard: 0 }));
    // The evidence came through 192.168.1.21, which this Mac no longer holds.
    let elsewhere: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer {
            via: Some(v4("192.168.1.21")),
            ..a
        })
        .collect();
    assert_eq!(
        on_evidence(&home(elsewhere)),
        Err(Away::TooFew { heard: 0 })
    );
    let unread = on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", None),
        &[],
        members_at_home(),
    );
    assert_eq!(on_evidence(&unread), Err(Away::NoGateway));
    let mut no_stay = home(members_at_home());
    no_stay.stay_since_ms = None;
    assert_eq!(on_evidence(&no_stay), Err(Away::TooFew { heard: 0 }));
    // CONTROL: the same evidence, fresh, behind the home router, reads home.
    assert!(on_evidence(&home(members_at_home())).is_ok());
}

/// Only answers in the current stay count: three members that answered on the home LAN eight
/// minutes ago, before this Mac went to a phone hotspot for a few minutes and came back, are
/// fresh by the clock but from an earlier stay; and only node-proven fleet answers count, never
/// three Commons contacts.
///
/// NEGATIVE CONTROLS: with `answer.began_ms >= since` removed from `home_at`, the answers from
/// before the hotspot read home and this test fails; with `answer.lane == Lane::Fleet` removed,
/// the Commons contacts read home and it fails.
#[test]
fn only_this_stays_fleet_answers_count() {
    let before: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer {
            began_ms: NOW - 8 * 60_000,
            at_ms: NOW - 8 * 60_000 + 500,
            ..a
        })
        .collect();
    let mut back = home(before);
    back.stay_since_ms = Some(NOW - 60_000);
    assert_eq!(on_evidence(&back), Err(Away::TooFew { heard: 0 }));
    let contacts: Vec<Answer> = members_at_home()
        .into_iter()
        .enumerate()
        .map(|(n, a)| Answer {
            lane: Lane::Commons,
            member: member_key(Lane::Commons, &format!("peer{n}")),
            ..a
        })
        .collect();
    assert_eq!(on_evidence(&home(contacts)), Err(Away::TooFew { heard: 0 }));
    // CONTROL: the same members heard in this stay over the fleet read home.
    assert!(on_evidence(&home(members_at_home())).is_ok());
}

/// Only another host on the home subnet is on the home ALab LAN: a routed or relayed peer, a
/// public address, a host on this Mac's second network (here wired, `10.1.0.0/24`, while the
/// default route takes Wi-Fi), this Mac itself, loopback and IPv6 are not; nor does an answer
/// that came from this Mac's own address count as a member's.
///
/// NEGATIVE CONTROL: with `HomeNet::holds`'s subnet comparison removed, the routed peer
/// `192.168.2.9` reads as held and this test fails.
#[test]
fn only_another_host_on_the_home_subnet_is_held() {
    let wired_too = on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(&HOME_HW)),
        &["10.1.0.5"],
        members_at_home(),
    );
    let home_net = on_evidence(&wired_too).unwrap();
    for away in [
        "192.168.2.9",
        "93.184.216.34",
        "10.1.0.9",
        "10.1.0.5",
        "192.168.1.20",
        "::ffff:192.168.1.20",
        "127.0.0.1",
        "::1",
        "fe80::2",
    ] {
        assert!(!home_net.holds(ip(away)), "{away}");
    }
    // Three answers, one of them from this Mac's own address: two members.
    let mut own = members_at_home();
    own[2].addr = Some(ip("192.168.1.20"));
    assert_eq!(on_evidence(&home(own)), Err(Away::TooFew { heard: 2 }));
    // CONTROL: a host on the evidenced subnet.
    assert!(home_net.holds(ip("192.168.1.99")));
}

/// A Mac with two interfaces on the home subnet (Wi-Fi at the default route, Ethernet beside it)
/// holds neither of its own addresses as another host: a process on this Mac that dials its own
/// member service leaves by either, and a proof from the second is its own, which no home-LAN
/// rule may count as another Mac's. Its neighbours are held as before, and an address it holds
/// on another subnet changes nothing.
///
/// NEGATIVE CONTROL: `HomeNet::holds` without its `also` clause holds `192.168.1.21`
/// (`this Mac's Ethernet address`); `HomeNet::of` keeping every held address instead of those on
/// the subnet makes the last `assert_eq!` fail.
#[test]
fn a_second_own_address_on_the_home_subnet_is_not_held() {
    let both = on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(&HOME_HW)),
        &["192.168.1.21", "::ffff:192.168.1.22", "10.1.0.5", "fe80::2"],
        members_at_home(),
    );
    let home_net = on_evidence(&both).unwrap();
    for (own, what) in [
        ("192.168.1.20", "this Mac's Wi-Fi address"),
        ("192.168.1.21", "this Mac's Ethernet address"),
        ("::ffff:192.168.1.21", "that address, IPv4-mapped"),
        ("192.168.1.22", "an address this Mac holds IPv4-mapped"),
    ] {
        assert!(!home_net.holds(ip(own)), "{what}");
    }
    // CONTROL: its neighbours on the subnet, the members among them.
    for other in ["192.168.1.23", "192.168.1.31", "192.168.1.99"] {
        assert!(home_net.holds(ip(other)), "{other}");
    }
    // An address on another subnet is no part of the home network's value: this Mac at one
    // address there reads as the same home whatever else it holds.
    let wired_elsewhere = on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(&HOME_HW)),
        &["10.1.0.5", "fe80::2"],
        members_at_home(),
    );
    assert_ne!(on_evidence(&both), Ok(HomeNet::test()));
    assert_eq!(on_evidence(&wired_elsewhere), Ok(HomeNet::test()));
}

/// Only the default route's Ethernet or Wi-Fi network can be home: three member VMs answering on
/// a VM bridge, members heard through a VPN's tunnel or a point-to-point link, a Mac with no
/// default route, a full-tunnel VPN beside the home Wi-Fi, and evidence heard on another
/// interface than the one that holds the address now are no home.
///
/// NEGATIVE CONTROLS: with `home_kind`'s check removed from `home_route`, the VM bridge reads
/// `Ok(<192.168.64.1/24>)` and this test fails; with the interface comparison
/// (`answer.iface …`) removed from `home_at`, the evidence heard on `en0` reads home on `en1` and
/// it fails.
#[test]
fn only_the_default_routes_ethernet_or_wifi_can_be_home() {
    let three = |subnet: &str, via: &str, iface: &str| -> Vec<Answer> {
        (0..3u8)
            .map(|n| Answer {
                iface: Some(iface.to_owned()),
                ..answer(
                    &format!("node{n}"),
                    &format!("{subnet}.{}", 30 + n),
                    via,
                    NOW - 1_000,
                )
            })
            .collect()
    };
    // A Mac whose default route takes its VM bridge, where three member VMs answered.
    let bridge = on(
        ("bridge100", "192.168.64.1", 24),
        ("192.168.64.254", Some(&HOME_HW)),
        &[],
        three("192.168.64", "192.168.64.1", "bridge100"),
    );
    assert_eq!(on_evidence(&bridge), Err(Away::Interface));
    // A tunnel, and an `en` link that is point to point.
    for name in ["utun4", "en5"] {
        let mut tunnel = on(
            (name, "10.8.0.2", 24),
            ("10.8.0.1", Some(&HOME_HW)),
            &[],
            three("10.8.0", "10.8.0.2", name),
        );
        tunnel.point_to_point = true;
        assert_eq!(on_evidence(&tunnel), Err(Away::Interface), "{name}");
    }
    // No default route at all.
    let mut unrouted = home(members_at_home());
    unrouted.facts.route = None;
    assert_eq!(on_evidence(&unrouted), Err(Away::Interface));
    // A full-tunnel VPN beside the home Wi-Fi: a tunnel holds the default route.
    let mut vpn = home(members_at_home());
    vpn.facts.tunnel = Some("utun3".to_owned());
    assert_eq!(on_evidence(&vpn), Err(Away::Interface));
    // The address the evidence came through is on another interface now.
    let rewired = on(
        ("en1", "192.168.1.20", 24),
        ("192.168.1.1", Some(&HOME_HW)),
        &[],
        members_at_home(),
    );
    assert_eq!(on_evidence(&rewired), Err(Away::TooFew { heard: 0 }));
    // CONTROL: the same evidence on the interface it came through reads home.
    assert!(on_evidence(&home(members_at_home())).is_ok());
    assert!(home_kind("en0", false) && home_kind("en12", false));
    assert!(!home_kind("en", false) && !home_kind("enx0", false) && !home_kind("awdl0", false));
    assert!(!home_kind("en0", true));
}

/// The fleet heartbeat's notes reach admission through the one source: three members noted into
/// the network place's evidence (`aterm_netplace::heard::note_answer`, with the route each answer
/// took and the interface holding it) under a scratch state root read back, through the place's
/// own stay, as home; a member reached by a route this Mac took no address on for (a routed or
/// relayed member) is passed over.
///
/// NEGATIVE CONTROL: with `note_answer` writing no interface (`iface: None`), no answer passes
/// the interface rule, admission reads `TooFew { heard: 0 }`, and this test fails.
#[test]
fn the_heartbeats_notes_in_the_network_place_read_back_as_evidence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = aterm_netplace::record::dir(root.path());
    let home_now = home(Vec::new());
    let mut script = aterm_netplace::facts::Script {
        facts: home_now.facts.clone(),
        via: Vec::new(),
    };
    let members: Vec<(String, IpAddr)> = (0..3u8)
        .map(|n| (format!("node{n}"), ip(&format!("192.168.1.{}", 30 + n))))
        .chain([("routed".to_owned(), ip("172.16.5.5"))])
        .collect();
    for (_, addr) in &members[..3] {
        script.via.push((*addr, Some(v4("192.168.1.20"))));
    }
    script.via.push((ip("172.16.5.5"), None));
    let here = Here {
        facts: home_now.facts.clone(),
        point_to_point: false,
        own: home_now.own.clone(),
    };
    // The keeper opened this stay a minute before the round.
    let (record, _) = aterm_netplace::step(
        &aterm_netplace::Record::default(),
        &here.facts,
        &aterm_netplace::Evidence::default(),
        None,
        NOW - 120_000,
        Some(NOW - 120_000),
    );
    aterm_netplace::record::write(
        &dir,
        &aterm_netplace::record::Written {
            revision: 1,
            keeper_pid: Some(std::process::id()),
            keeper_started_ms: Some(NOW - 120_000),
            written_ms: NOW - 120_000,
            record,
        },
    )
    .unwrap();
    aterm_netplace::facts::script_here(script, || {
        for (node, addr) in &members {
            aterm_netplace::heard::note_answer(
                &dir,
                (Lane::Fleet, node),
                Some(*addr),
                (home_now.stay_network.as_ref(), true),
                (NOW - 61_000, NOW - 60_000),
            );
        }
    });
    let now = aterm_netplace::proof::read_with(root.path(), here, NOW);
    assert_eq!(now.answers.len(), 4, "every answer noted");
    assert_eq!(on_evidence(&now), Ok(HomeNet::test()));
    // Without the third member's answer, the routed one does not make up the count.
    let mut two = now.clone();
    two.answers
        .retain(|answer| answer.member != member_key(Lane::Fleet, "node2"));
    assert_eq!(on_evidence(&two), Err(Away::TooFew { heard: 2 }));
}

/// Answers heard on the home LAN count only on the home LAN, whatever the stay says: this Mac on
/// a lookalike network (a café that numbers its hosts like home, behind its own router) roams to
/// the real home LAN for under a minute, where the keeper reads no gateway hardware address (or
/// does not look) and so the lookalike's stay continues, and the heartbeat proves three members
/// there; back on the lookalike, the stay since before the roam is this network's, and those
/// answers, fresh, through this Mac's address and interface, from the subnet, are booked on the
/// home LAN (or on none, where the gateway was unread), never here.
///
/// NEGATIVE CONTROL: with `answer.network.as_ref() == Some(&network)` removed from `home_at`, the
/// answers heard on the home LAN read the lookalike home and "booked on another network" fails.
#[test]
fn answers_heard_on_another_network_in_the_same_stay_are_no_evidence_here() {
    const LOOKALIKE_HW: [u8; 6] = [0x0b, 0xad, 0xc0, 0xff, 0xee, 0x00];
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = aterm_netplace::record::dir(root.path());
    let facts = |hw: Option<&[u8]>| {
        on(
            ("en0", "192.168.1.20", 24),
            ("192.168.1.1", hw),
            &[],
            Vec::new(),
        )
    };
    let (lookalike, home_read, home_unread) = (
        facts(Some(&LOOKALIKE_HW)),
        facts(Some(&HOME_HW)),
        facts(None),
    );
    let mut record = aterm_netplace::Record::default();
    for (now, at) in [(&lookalike, STAY), (&home_unread, NOW - 3 * 60_000)] {
        record = aterm_netplace::step(
            &record,
            &now.facts,
            &aterm_netplace::Evidence::default(),
            None,
            at,
            Some(STAY),
        )
        .0;
    }
    aterm_netplace::record::write(
        &dir,
        &aterm_netplace::record::Written {
            revision: 1,
            keeper_pid: Some(std::process::id()),
            keeper_started_ms: Some(STAY),
            written_ms: NOW - 3 * 60_000,
            record,
        },
    )
    .unwrap();
    // The round on the home LAN: one member noted while its gateway was read (the keeper did not
    // look), two while it was not.
    for (n, now) in [(0u8, &home_read), (1, &home_unread), (2, &home_unread)] {
        let addr = ip(&format!("192.168.1.{}", 30 + n));
        let script = aterm_netplace::facts::Script {
            facts: now.facts.clone(),
            via: vec![(addr, Some(v4("192.168.1.20")))],
        };
        aterm_netplace::facts::script_here(script, || {
            aterm_netplace::heard::note_answer(
                &dir,
                (Lane::Fleet, &format!("node{n}")),
                Some(addr),
                (aterm_netplace::facts::network_now().as_ref(), true),
                (NOW - 2 * 60_000, NOW - 2 * 60_000 + 500),
            );
        });
    }
    let here = Here {
        facts: lookalike.facts.clone(),
        point_to_point: false,
        own: lookalike.own.clone(),
    };
    let back = aterm_netplace::proof::read_with(root.path(), here, NOW);
    assert_eq!(
        (back.stay_since_ms, back.stay_network.clone()),
        (Some(STAY), lookalike.stay_network.clone()),
        "PRECONDITION: the lookalike's stay continued across the roam"
    );
    assert_eq!(back.answers.len(), 3, "PRECONDITION: every answer noted");
    assert_eq!(
        on_evidence(&back),
        Err(Away::TooFew { heard: 0 }),
        "booked on another network"
    );
    // CONTROL: the same answers booked on this network read home.
    let mut booked_here = back.clone();
    for answer in &mut booked_here.answers {
        answer.network.clone_from(&lookalike.stay_network);
    }
    assert!(on_evidence(&booked_here).is_ok());
}

/// Only answers a round under this Mac's vault's membership log heard are evidence: three members
/// an isolated job's heartbeat (a test's, a lab's, CI's, with no vault and no deny list) heard on
/// the home LAN, under the very state root the vault reads, are no home, so a member the log took
/// out, which such a round dials, never counts.
///
/// NEGATIVE CONTROL: with `answer.logged` removed from `home_at`, the isolated round's answers
/// read home and "an isolated round's answers" fails.
#[test]
fn only_answers_a_round_under_the_vaults_log_heard_are_evidence() {
    let isolated: Vec<Answer> = members_at_home()
        .into_iter()
        .map(|a| Answer { logged: false, ..a })
        .collect();
    assert_eq!(
        on_evidence(&home(isolated)),
        Err(Away::TooFew { heard: 0 }),
        "an isolated round's answers"
    );
    // Two under the log and one isolated: two members.
    let mut mixed = members_at_home();
    mixed[2].logged = false;
    assert_eq!(on_evidence(&home(mixed)), Err(Away::TooFew { heard: 2 }));
    // CONTROL: the same answers under the log read home.
    assert!(on_evidence(&home(members_at_home())).is_ok());
}

/// A network the place merely remembers is no proof: back on the home LAN with nothing heard in
/// this stay yet, the place reads `alab-lan` (remembered), and admission still says too few, from
/// the very files the place read.
///
/// NEGATIVE CONTROL: with `answer.began_ms >= since` removed from `home_at`, the answers from the
/// stay before the hotspot (within `FRESH`) read home and this test fails.
#[test]
fn a_network_the_place_remembers_is_not_enough() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (at_home, own) = returned_home_from_a_hotspot(root.path());
    let place = aterm_netplace::read_with(root.path(), at_home.clone(), NOW);
    assert_eq!(
        (place.at, place.recognised),
        (
            aterm_netplace::Where::AlabLan,
            Some(aterm_netplace::Recognised::Remembered)
        ),
        "PRECONDITION: the place reads the remembered home"
    );
    let here = Here {
        facts: at_home,
        point_to_point: false,
        own,
    };
    let now = aterm_netplace::proof::read_with(root.path(), here, NOW);
    assert_eq!(now.stay_since_ms, Some(NOW - 60_000));
    assert_eq!(on_evidence(&now), Err(Away::TooFew { heard: 0 }));
}

/// BACK ON THE HOME LAN, THE FIRST HEARTBEAT ROUND THERE ADMITS, with no wait: the keeper steps
/// home, onto a phone hotspot and home again (the record the keeper writes), the answers from
/// before the hotspot do not count, and the round that runs on the return notes three members
/// through the heartbeat's own writer (`heard::note_answer`, the network it began and ended on
/// booked on each), so the very next read of the proof is home, holding a peer on the subnet. This
/// drives the keeper, the heartbeat's notes and admission together across a return home (the lab
/// lane that drives real instances is design §8's).
///
/// NEGATIVE CONTROL: `note_answer` booking every answer on no network (`network = None`) leaves
/// the round's answers unbooked, and "the first round back home admits" fails with
/// `TooFew { heard: 0 }`.
#[test]
fn back_on_the_home_lan_the_first_round_there_admits() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (at_home, own) = returned_home_from_a_hotspot(root.path());
    let dir = aterm_netplace::record::dir(root.path());
    let read = || {
        aterm_netplace::proof::read_with(
            root.path(),
            Here {
                facts: at_home.clone(),
                point_to_point: false,
                own: own.clone(),
            },
            NOW,
        )
    };
    assert_eq!(
        on_evidence(&read()),
        Err(Away::TooFew { heard: 0 }),
        "PRECONDITION: nothing from before the hotspot counts"
    );
    for n in 0..3u8 {
        let addr = ip(&format!("192.168.1.{}", 30 + n));
        let script = aterm_netplace::facts::Script {
            facts: at_home.clone(),
            via: vec![(addr, Some(v4("192.168.1.20")))],
        };
        aterm_netplace::facts::script_here(script, || {
            let began_on = aterm_netplace::facts::network_now();
            aterm_netplace::heard::note_answer(
                &dir,
                (Lane::Fleet, &format!("node{n}")),
                Some(addr),
                (began_on.as_ref(), true),
                (NOW - 30_000, NOW - 30_000 + 500),
            );
        });
    }
    let now = read();
    assert_eq!(
        now.stay_since_ms,
        Some(NOW - 60_000),
        "the stay since the return"
    );
    assert_eq!(
        on_evidence(&now).map(|_| ()),
        Ok(()),
        "the first round back home admits"
    );
    assert!(
        on_evidence(&now).is_ok_and(|net| net.holds(ip("192.168.1.40"))),
        "and a Mac on the subnet proves itself from home"
    );
}

/// This Mac back on the home LAN a minute ago, under `root`: three members answered there nine
/// minutes ago and the keeper learned it, then three minutes on a phone hotspot (the record the
/// keeper wrote, the answers heard and the fleet's book of five), and the facts and addresses it
/// holds there now.
fn returned_home_from_a_hotspot(root: &std::path::Path) -> (Facts, Vec<IpAddr>) {
    let dir = aterm_netplace::record::dir(root);
    let home_now = home(Vec::new());
    let at_home = home_now.facts.clone();
    let hotspot = Facts {
        route: Some(RouteFacts {
            interface: "en0".to_owned(),
            address: v4("172.20.10.2"),
            prefix: 28,
            gateway: Some(v4("172.20.10.1")),
            gateway_hw: Some(vec![2, 0, 0, 0, 0, 1]),
        }),
        tunnel: None,
        path: Some(path(true, Link::Wifi)),
    };
    // Home: three members answer and the keeper learns the network; then the hotspot for three
    // minutes; then home again a minute ago.
    let evidence = aterm_netplace::Evidence {
        answers: members_at_home()
            .into_iter()
            .map(|a| Answer {
                began_ms: NOW - 9 * 60_000,
                at_ms: NOW - 9 * 60_000 + 500,
                ..a
            })
            .collect(),
        ..aterm_netplace::Evidence::default()
    };
    let book = aterm_netplace::Book {
        members: (0..5)
            .map(|n| aterm_netplace::BookMember {
                member: member_key(Lane::Fleet, &format!("node{n}")),
                addr: None,
                answered_ms: None,
                network: None,
            })
            .collect(),
    };
    let mut record = aterm_netplace::Record::default();
    for (facts, at) in [
        (&at_home, NOW - 20 * 60_000),
        (&at_home, NOW - 8 * 60_000),
        (&hotspot, NOW - 4 * 60_000),
        (&at_home, NOW - 60_000),
    ] {
        record = aterm_netplace::step(&record, facts, &evidence, Some(&book), at, Some(STAY)).0;
    }
    assert_eq!(record.remembered.len(), 1, "PRECONDITION: home was learned");
    aterm_netplace::record::write(
        &dir,
        &aterm_netplace::record::Written {
            revision: 1,
            keeper_pid: Some(std::process::id()),
            keeper_started_ms: Some(STAY),
            written_ms: NOW - 60_000,
            record,
        },
    )
    .unwrap();
    aterm_netplace::heard::add(&dir, evidence).unwrap();
    aterm_netplace::heard::write_book(&dir, (0..5).map(|n| (format!("node{n}"), None, None)), None)
        .unwrap();
    (at_home, home_now.own)
}

/// A member last heard off this network is not here: its newer answer from another network
/// shadows its older one on the home LAN, so two members remain and this Mac is not home there;
/// an answer elsewhere older than its home one shadows nothing.
///
/// NEGATIVE CONTROL: dropping the newer-answer check from `home_at`'s filter counts `node2`'s
/// older home answer, and this test fails at "a member last heard elsewhere".
#[test]
fn a_newer_answer_from_another_network_shadows_an_older_home_answer() {
    let elsewhere = RouteFacts {
        interface: "en0".to_owned(),
        address: v4("10.0.0.20"),
        prefix: 24,
        gateway: Some(v4("10.0.0.1")),
        gateway_hw: Some(vec![0x02, 0x11, 0x22, 0x33, 0x44, 0x55]),
    };
    let away = |began: u64| Answer {
        network: NetworkId::of(&elsewhere),
        ..answer("node2", "10.0.0.32", "10.0.0.20", began)
    };
    let mut answers = members_at_home();
    answers.push(away(NOW - 10_000));
    assert_eq!(
        on_evidence(&home(answers)),
        Err(Away::TooFew { heard: 2 }),
        "a member last heard elsewhere"
    );
    let mut answers = members_at_home();
    answers.push(away(NOW - 120_000));
    assert!(
        on_evidence(&home(answers)).is_ok(),
        "an older answer elsewhere shadows nothing"
    );
}

/// The 7-day condition on, as the mesh turns it on.
const MESH_RULE: Rule = Rule { days: true };

/// The router of a network first seen after the seed, numbered like home.
const NEW_HW: [u8; 6] = [0x02, 0x0e, 0x0e, 0x0e, 0x0e, 0x01];

/// The first start of a build with the 7-day condition, as an hour of the injected clock: day
/// 20,400 at 10:00 UTC, past [`NOW`].
const SWITCH: u64 = 20_400 * DAY_MS + 10 * 60 * 60_000;

/// One hour of the injected clock.
const HOUR: u64 = 60 * 60_000;

/// This Mac on the home LAN's subnet behind the router whose hardware address is `hw`.
fn behind(hw: &[u8]) -> Now {
    on(
        ("en0", "192.168.1.20", 24),
        ("192.168.1.1", Some(hw)),
        &[],
        Vec::new(),
    )
}

/// What this Mac reads of `now`'s network.
fn here_of(now: &Now) -> Here {
    Here {
        facts: now.facts.clone(),
        point_to_point: false,
        own: now.own.clone(),
    }
}

/// The network place under `root` as its keeper wrote it at `since`: a stay on `now`'s network
/// since then, remembering `remembered`'s networks as the ALab LAN.
fn keep_place(root: &Path, now: &Now, since: u64, remembered: &[&Now]) {
    let (mut record, _) = aterm_netplace::step(
        &aterm_netplace::Record::default(),
        &now.facts,
        &aterm_netplace::Evidence::default(),
        None,
        since,
        Some(since),
    );
    record.remembered = remembered
        .iter()
        .filter_map(|on| on.stay_network.clone())
        .map(|network| aterm_netplace::place::Known {
            network,
            learned_ms: since - 30 * DAY_MS,
            confirmed_ms: since,
        })
        .collect();
    aterm_netplace::record::write(
        &aterm_netplace::record::dir(root),
        &aterm_netplace::record::Written {
            revision: 1,
            keeper_pid: Some(std::process::id()),
            keeper_started_ms: Some(since),
            written_ms: since,
            record,
        },
    )
    .unwrap();
}

/// `members` members proving their nodes to this Mac on `now`'s network half a minute before
/// `at_ms`, noted into the place under `root` as the mesh notes them (with the day each came on)
/// or, `days` false, as the heartbeat does (without).
fn members_prove(root: &Path, now: &Now, at_ms: u64, members: u8, days: bool) {
    let dir = aterm_netplace::record::dir(root);
    let answers: Vec<Answer> = (0..members)
        .map(|n| Answer {
            network: now.stay_network.clone(),
            ..answer(
                &format!("node{n}"),
                &format!("192.168.1.{}", 30 + n),
                "192.168.1.20",
                at_ms - 30_500,
            )
        })
        .collect();
    aterm_netplace::heard::add(
        &dir,
        aterm_netplace::Evidence {
            answers: answers.clone(),
            ..aterm_netplace::Evidence::default()
        },
    )
    .unwrap();
    if days {
        aterm_netplace::heard::note_days(&dir, &answers);
    }
}

/// ON AN INJECTED CLOCK, A NETWORK HOME FOR FEWER THAN 7 DISTINCT DAYS ADMITS NOBODY BY ITSELF
/// (§10, "LAN admission without the heartbeat"): three members prove themselves each day on a
/// network this Mac does not remember, and with the 7-day condition it is refused as new until
/// its seventh distinct day: a second look on one day adds no day, and a day only two members
/// were heard on is no home day. The same evidence on the first day admits under the rule before
/// the condition.
///
/// NEGATIVE CONTROL: `home_at` with its `rule.days` branch dropped admits on the first day, and
/// "the first day is no home" fails; `home_days` counting every booked day (`heard_by(1)`) counts
/// the day two members were heard on, and "two members make no home day" fails.
#[test]
fn a_network_home_on_fewer_than_seven_distinct_days_admits_nobody_by_itself() {
    let root = aterm_tempfile::tempdir().unwrap();
    let new = behind(&NEW_HW);
    keep_place(root.path(), &new, SWITCH - HOUR, &[]);
    assert!(matches!(
        aterm_secrets_seed(root.path(), SWITCH - HOUR),
        Seeding::Now(credited) if credited.is_empty()
    ));
    let judge = |at: u64, rule: Rule| home_net_with(root.path(), here_of(&new), at, rule);

    members_prove(root.path(), &new, SWITCH, 3, true);
    assert_eq!(
        judge(SWITCH, MESH_RULE),
        Err(Away::NewNetwork { days: 1 }),
        "the first day is no home"
    );
    assert_eq!(
        judge(SWITCH, Rule { days: false }),
        Ok(HomeNet::test()),
        "CONTROL: the rule before the condition admits on the first day"
    );
    members_prove(root.path(), &new, SWITCH + HOUR, 3, true);
    assert_eq!(
        judge(SWITCH + HOUR, MESH_RULE),
        Err(Away::NewNetwork { days: 1 }),
        "a second look on one day adds no day"
    );
    // Day 2: only two members are heard.
    members_prove(root.path(), &new, SWITCH + DAY_MS, 2, true);
    assert_eq!(
        judge(SWITCH + DAY_MS, MESH_RULE),
        Err(Away::TooFew { heard: 2 })
    );
    // Days 3 to 8: three members each day.
    for day in 2..=7u64 {
        let at = SWITCH + day * DAY_MS;
        members_prove(root.path(), &new, at, 3, true);
        let judged = judge(at, MESH_RULE);
        if day < 7 {
            let days = u32::try_from(day).unwrap();
            assert_eq!(
                judged,
                Err(Away::NewNetwork { days }),
                "two members make no home day (day {day})"
            );
        } else {
            assert_eq!(judged, Ok(HomeNet::test()), "the seventh distinct day");
        }
    }
    assert!(
        Away::NewNetwork { days: 3 }
            .to_string()
            .contains("3 of the 7 days")
    );
}

/// The seed at the first start, as the node service calls it.
fn aterm_secrets_seed(root: &Path, at: u64) -> Seeding {
    seed_at(root, at).expect("the seed")
}

/// THE SEED, ONCE (§10, "LAN admission without the heartbeat"; §11 B6): a network the place
/// remembers as the ALab LAN before the first start of a build with the 7-day condition admits at
/// once after that start, its fresh evidence as before. A second start credits nothing: a network
/// the place came to remember between the two starts, three members heard there, admits nobody
/// until its seventh distinct day.
///
/// NEGATIVE CONTROL: `seed_days` crediting again at a second start (its `seeded_ms` check
/// dropped) credits the network remembered since, and "a second start credits nothing" fails.
#[test]
fn a_lan_remembered_before_the_first_start_admits_at_once_and_the_seed_runs_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let lan = behind(&HOME_HW);
    let new = behind(&NEW_HW);
    keep_place(root.path(), &lan, SWITCH - 3 * DAY_MS, &[&lan]);
    assert_eq!(
        aterm_secrets_seed(root.path(), SWITCH),
        Seeding::Now(vec![lan.stay_network.clone().unwrap()])
    );
    members_prove(root.path(), &lan, SWITCH + 60_000, 3, true);
    assert_eq!(
        home_net_with(root.path(), here_of(&lan), SWITCH + 60_000, MESH_RULE),
        Ok(HomeNet::test()),
        "the remembered LAN admits at once"
    );

    // The Mac goes to the new network, which the place comes to remember; then it starts again.
    let moved = SWITCH + 2 * DAY_MS;
    keep_place(root.path(), &new, moved, &[&lan, &new]);
    assert_eq!(
        aterm_secrets_seed(root.path(), moved + HOUR),
        Seeding::Before(SWITCH),
        "a second start credits nothing"
    );
    for day in 0..7u64 {
        let at = moved + HOUR + day * DAY_MS;
        members_prove(root.path(), &new, at, 3, true);
        let judged = home_net_with(root.path(), here_of(&new), at, MESH_RULE);
        if day < 6 {
            let days = u32::try_from(day + 1).unwrap();
            assert_eq!(judged, Err(Away::NewNetwork { days }), "day {day}");
        } else {
            assert_eq!(judged, Ok(HomeNet::test()), "the seventh distinct day");
        }
    }
}

/// AUTOMATIC LAN ADMISSION NEVER STOPS ACROSS THE R2 SWITCH (§11 B6), on an injected clock: a
/// Mac on its home LAN is looked at every hour from three days before the first start of a build
/// with the 7-day condition to ten days after it. Before, its evidence is the heartbeat's, which
/// books no day, under the rule before the condition; at the start it seeds; after, its
/// evidence is the mesh's, under the condition. Every look admits.
///
/// NEGATIVE CONTROL (in the test): the same timeline with no seed at the start stops admitting
/// at the first look after it, and admits again only on the seventh distinct day the mesh booked.
#[test]
fn automatic_lan_admission_never_stops_across_the_r2_switch() {
    /// The looks of the timeline, and which of them admitted, `seeded` or not.
    fn timeline(seeded: bool) -> Vec<(u64, Result<HomeNet, Away>)> {
        let root = aterm_tempfile::tempdir().unwrap();
        let lan = behind(&HOME_HW);
        keep_place(root.path(), &lan, SWITCH - 4 * DAY_MS, &[&lan]);
        let mut looks = Vec::new();
        let mut at = SWITCH - 3 * DAY_MS;
        while at <= SWITCH + 10 * DAY_MS {
            let after = at >= SWITCH;
            if at == SWITCH && seeded {
                aterm_secrets_seed(root.path(), at);
            }
            members_prove(root.path(), &lan, at, 3, after);
            let rule = Rule { days: after };
            looks.push((at, home_net_with(root.path(), here_of(&lan), at, rule)));
            at += HOUR;
        }
        looks
    }
    let seeded = timeline(true);
    assert_eq!(seeded.len(), 13 * 24 + 1);
    for (at, judged) in &seeded {
        assert_eq!(judged, &Ok(HomeNet::test()), "the look at {at}");
    }

    let unseeded = timeline(false);
    for (at, judged) in &unseeded {
        // The distinct day of the look, the switch's day the first.
        let day = (at / DAY_MS).saturating_sub(SWITCH / DAY_MS) + 1;
        if *at < SWITCH {
            assert_eq!(judged, &Ok(HomeNet::test()), "before the switch, at {at}");
        } else if day < 7 {
            assert!(
                matches!(judged, Err(Away::NewNetwork { .. })),
                "CONTROL: with no seed admission stops after the switch, on day {day}: {judged:?}"
            );
        } else {
            assert_eq!(
                judged,
                &Ok(HomeNet::test()),
                "the seventh distinct day, at {at}"
            );
        }
    }
    assert!(
        matches!(unseeded[3 * 24].1, Err(Away::NewNetwork { days: 1 })),
        "the first look after the switch"
    );
}

/// A home-day ledger with a line this build cannot read counts no day: the seed's credit read
/// from it is not taken, so a damaged ledger admits nothing it did not prove.
///
/// NEGATIVE CONTROL: `home_net_with` taking the ledger whatever `read_days` passed over reads the
/// credit beside the torn line, and this test fails.
#[test]
fn a_torn_home_day_ledger_counts_no_day() {
    let root = aterm_tempfile::tempdir().unwrap();
    let lan = behind(&HOME_HW);
    keep_place(root.path(), &lan, SWITCH - DAY_MS, &[&lan]);
    aterm_secrets_seed(root.path(), SWITCH);
    members_prove(root.path(), &lan, SWITCH + 60_000, 3, false);
    assert!(home_net_with(root.path(), here_of(&lan), SWITCH + 60_000, MESH_RULE).is_ok());
    let dir = aterm_netplace::record::dir(root.path());
    let file = dir.join(aterm_netplace::heard::DAYS_FILE);
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("d n0");
    aterm_uds::private_file::write_private(&dir, aterm_netplace::heard::DAYS_FILE, text.as_bytes())
        .unwrap();
    assert_eq!(
        home_net_with(root.path(), here_of(&lan), SWITCH + 60_000, MESH_RULE),
        Err(Away::NewNetwork { days: 1 })
    );
}

/// THE DESK'S PATH SEEDS BEFORE IT JUDGES: with the 7-day condition, `home_net`'s judgement
/// ([`home_net_seeding`]) of a Mac whose place remembers its LAN, its node service not yet
/// started (no seed ran), seeds first and so admits at once; with the condition off it writes
/// no ledger at all.
///
/// NEGATIVE CONTROL: `home_net_seeding` without its `seed_at` call judges an unseeded ledger,
/// and "the desk's first look admits" fails with `NewNetwork { days: 1 }`.
#[test]
fn the_desks_path_seeds_before_it_judges() {
    let root = aterm_tempfile::tempdir().unwrap();
    let lan = behind(&HOME_HW);
    keep_place(root.path(), &lan, SWITCH - DAY_MS, &[&lan]);
    members_prove(root.path(), &lan, SWITCH, 3, true);
    let dir = aterm_netplace::record::dir(root.path());
    assert_eq!(
        home_net_seeding(root.path(), here_of(&lan), SWITCH, Rule { days: false }),
        Ok(HomeNet::test())
    );
    assert_eq!(
        aterm_netplace::heard::read_days(&dir).0.seeded_ms,
        None,
        "the condition off seeds nothing"
    );
    assert_eq!(
        home_net_with(root.path(), here_of(&lan), SWITCH, MESH_RULE),
        Err(Away::NewNetwork { days: 1 }),
        "CONTROL: unseeded, the remembered LAN is new"
    );
    assert_eq!(
        home_net_seeding(root.path(), here_of(&lan), SWITCH, MESH_RULE),
        Ok(HomeNet::test()),
        "the desk's first look admits"
    );
    assert_eq!(
        aterm_netplace::heard::read_days(&dir).0.seeded_ms,
        Some(SWITCH)
    );
}
