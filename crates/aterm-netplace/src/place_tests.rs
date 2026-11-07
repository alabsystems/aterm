// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[path = "place_temporal_tests.rs"]
mod temporal;

const HOME_HW: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
const CAFE_HW: [u8; 6] = [0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb];
const PHONE_HW: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const T0: u64 = 1_800_000_000_000;
const MIN: u64 = 60_000;

fn ip(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

fn route(address: Ipv4Addr, prefix: u8, gateway: Ipv4Addr, hw: Option<&[u8]>) -> RouteFacts {
    RouteFacts {
        interface: "en0".to_owned(),
        address,
        prefix,
        gateway: Some(gateway),
        gateway_hw: hw.map(<[u8]>::to_vec),
    }
}

fn home() -> RouteFacts {
    route(ip(192, 168, 4, 31), 22, ip(192, 168, 4, 1), Some(&HOME_HW))
}

fn cafe() -> RouteFacts {
    route(ip(10, 0, 0, 7), 24, ip(10, 0, 0, 1), Some(&CAFE_HW))
}

fn phone() -> RouteFacts {
    route(ip(172, 20, 10, 2), 28, ip(172, 20, 10, 1), Some(&PHONE_HW))
}

fn path(expensive: bool) -> PathFacts {
    PathFacts {
        usable: Some(true),
        expensive,
        constrained: false,
        link: Link::Wifi,
    }
}

fn on(route: RouteFacts) -> Facts {
    Facts {
        route: Some(route),
        tunnel: None,
        path: Some(path(false)),
    }
}

fn hotspot() -> Facts {
    Facts {
        route: Some(phone()),
        tunnel: None,
        path: Some(path(true)),
    }
}

fn offline() -> Facts {
    Facts {
        route: None,
        tunnel: None,
        path: Some(PathFacts {
            usable: Some(false),
            ..path(false)
        }),
    }
}

/// A proven fleet answer from `member` over `route`'s interface, its dial begun at `began`.
fn answer_on(route: &RouteFacts, member: &str, began: u64) -> Answer {
    Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, member),
        addr: Some(IpAddr::V4(route.address)),
        via: Some(route.address),
        iface: Some(route.interface.clone()),
        network: NetworkId::of(route),
        logged: true,
        began_ms: began,
        at_ms: began + 500,
    }
}

fn book(n: usize) -> Book {
    Book {
        members: (0..n)
            .map(|i| BookMember {
                member: member_key(Lane::Fleet, &format!("m{i}")),
                addr: None,
                answered_ms: None,
                network: None,
            })
            .collect(),
    }
}

/// The keeper's steps over `facts` at each time, from `record`, with `evidence` and `book`.
fn keep(
    mut record: Record,
    steps: &[(&Facts, u64)],
    evidence: &Evidence,
    book: Option<&Book>,
) -> (Record, Place) {
    let mut place = None;
    for (facts, now) in steps {
        let (next, p) = step(&record, facts, evidence, book, *now, Some(T0));
        record = next;
        place = Some(p);
    }
    (record, place.expect("a step"))
}

/// A record that learned `route`'s network as the ALab LAN.
fn learned(route: &RouteFacts) -> Record {
    Record {
        remembered: vec![Known {
            network: NetworkId::of(route).expect("strong"),
            learned_ms: T0 - 60 * MIN,
            confirmed_ms: T0 - 30 * MIN,
        }],
        ..Record::default()
    }
}

/// A network no member answered on, on a Mac that learned the ALab LAN elsewhere, is remote,
/// and the ALab axis reads away since the home stay ended.
/// NEGATIVE CONTROL: deciding `Where::AlabLan` for an unremembered strong id (the last arm of
/// `step`'s match) reads alab-lan on the café LAN, and this test fails.
#[test]
fn a_new_network_no_member_answered_on_is_remote() {
    let (_, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&on(cafe()), T0 + 10 * MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Remote(Kind::Lan));
    assert_eq!(
        place.alab,
        Alab::Away {
            since_ms: Some(T0 + 10 * MIN)
        }
    );
    assert_eq!((place.heard, place.known), (0, Some(41)));
}

/// An expensive path is a phone hotspot: remote whoever answers on it, and never remembered
/// as the ALab LAN even with members answering in the stay.
/// NEGATIVE CONTROL: dropping the `hotspot_or_cellular` arm makes three members on the hotspot
/// read alab-lan and learn it, and this test fails.
#[test]
fn an_expensive_path_names_a_phone_hotspot_and_is_never_learned() {
    let mut evidence = Evidence::default();
    for member in ["m1", "m2", "m3"] {
        evidence.answers.push(answer_on(&phone(), member, T0 + MIN));
    }
    let (record, place) = keep(
        learned(&home()),
        &[(&hotspot(), T0), (&hotspot(), T0 + 2 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Remote(Kind::Hotspot { by_range: false }));
    assert_eq!(place.heard, 3, "the members on the hotspot are heard");
    assert_eq!(record.remembered.len(), 1, "the hotspot is never learned");
}

/// With no path monitor, only the iPhone hotspot's address range says hotspot, and says so.
#[test]
fn without_a_path_monitor_the_hotspot_range_is_a_hint() {
    let facts = Facts {
        path: None,
        ..hotspot()
    };
    let (_, place) = keep(
        learned(&home()),
        &[(&facts, T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(place.at, Where::Remote(Kind::Hotspot { by_range: true }));
    let elsewhere = Facts {
        path: None,
        ..on(cafe())
    };
    let (_, place) = keep(
        learned(&home()),
        &[(&elsewhere, T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(place.at, Where::Remote(Kind::Lan));
}

/// Three distinct members answering within one stay make the network the ALab LAN, and it is
/// remembered: a later stay on it reads alab-lan before any member answers.
#[test]
fn enough_authenticated_members_in_one_stay_make_the_alab_lan_and_it_is_remembered() {
    let mut evidence = Evidence::default();
    for member in ["m1", "m2", "m3"] {
        evidence.answers.push(answer_on(&home(), member, T0 + MIN));
    }
    let (record, place) = keep(
        Record::default(),
        &[(&on(home()), T0), (&on(home()), T0 + 2 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(place.recognised, Some(Recognised::Members));
    assert_eq!(place.heard, 3);
    assert_eq!(record.remembered.len(), 1);
    let (_, back) = keep(
        record,
        &[(&on(cafe()), T0 + 10 * MIN), (&on(home()), T0 + 60 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(back.at, Where::AlabLan);
    assert_eq!(back.recognised, Some(Recognised::Remembered));
    assert_eq!(back.heard, 0, "members not heard yet in this stay");
}

/// A fleet of two Macs never learns a network, however often they answer: three members, and a
/// book that knows three, are needed, whatever the fleet's size. With nothing it could ever
/// learn, the place says the network with no ALab axis rather than `checking` for ever; a Mac in
/// no fleet (no book) the same.
/// NEGATIVE CONTROLS: learning at `min(3, ceil(known/2))` (one member for a fleet of two) makes
/// the café learned, and this test fails; dropping the `Where::Network` arm leaves it checking
/// for ever, and "checking always ends" fails.
#[test]
fn a_small_fleet_learns_nothing() {
    let mut evidence = Evidence::default();
    for member in ["m1", "m2"] {
        evidence.answers.push(answer_on(&cafe(), member, T0 + MIN));
    }
    let (record, place) = keep(
        Record::default(),
        &[(&on(cafe()), T0), (&on(cafe()), T0 + 2 * MIN)],
        &evidence,
        Some(&book(2)),
    );
    assert_eq!(place.at, Where::Network(Kind::Lan), "checking always ends");
    assert_eq!(
        place.alab,
        Alab::NoFleet,
        "no fleet to learn from is not away"
    );
    assert!(record.remembered.is_empty());
    let (_, place) = keep(
        Record::default(),
        &[(&on(cafe()), T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(place.at, Where::Network(Kind::Lan), "a Mac in no fleet");
    let (_, place) = keep(
        Record::default(),
        &[(&hotspot(), T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(
        place.at,
        Where::Remote(Kind::Hotspot { by_range: false }),
        "a phone hotspot is never the ALab LAN, fleet or none"
    );
}

/// A remembered network where fleet rounds dial members and hear none stops being home: after
/// two silent rounds it reads remote, "a network members were once on", and the ALab axis away.
/// NEGATIVE CONTROL: ignoring silent rounds keeps the hotel reading alab-lan forever, and this
/// test fails.
#[test]
fn a_remembered_network_that_falls_silent_reads_remote() {
    let hotel = cafe();
    let mut evidence = Evidence::default();
    let (record, place) = keep(
        learned(&hotel),
        &[(&on(home()), T0), (&on(hotel.clone()), T0 + MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(
        place.at,
        Where::AlabLan,
        "remembered, members not heard yet"
    );
    for k in 0..SILENT_ROUNDS as u64 {
        evidence.rounds.push(Round {
            lane: Lane::Fleet,
            answered: 0,
            blocked: 0,
            began_ms: T0 + (2 + 3 * k) * MIN,
            at_ms: T0 + (2 + 3 * k) * MIN + 1_000,
        });
    }
    let (_, place) = keep(
        record,
        &[(&on(hotel), T0 + 10 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Remote(Kind::Lan));
    assert!(place.once_alab);
    assert!(matches!(place.alab, Alab::Away { .. }));
}

/// Two routers both numbering `192.168.1.0/24` are two networks: the gateway's hardware address
/// decides, and the subnet alone does not.
#[test]
fn the_gateway_hardware_address_decides_the_network_not_the_subnet() {
    let a = route(ip(192, 168, 1, 20), 24, ip(192, 168, 1, 1), Some(&HOME_HW));
    let b = route(ip(192, 168, 1, 20), 24, ip(192, 168, 1, 1), Some(&CAFE_HW));
    assert_ne!(NetworkId::of(&a), NetworkId::of(&b));
    let renumbered = route(ip(192, 168, 2, 20), 24, ip(192, 168, 2, 1), Some(&HOME_HW));
    assert_ne!(
        NetworkId::of(&a),
        NetworkId::of(&renumbered),
        "one id rule: hardware address and subnet together"
    );
    let (_, place) = keep(
        learned(&a),
        &[(&on(b), T0)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Remote(Kind::Lan));
}

/// After a move the place reads `checking (moved)` while the gateway's hardware address is
/// unread; past the settle ladder the stay takes a weak id, which never matches the remembered
/// home and is never learned, so the place reads `checking (unidentified)`, not away and not
/// home; once ARP resolves the same network, the stay is the home stay again.
#[test]
fn a_gateway_not_yet_in_arp_reads_checking_then_a_weak_id_that_is_never_learned() {
    let unresolved = route(ip(192, 168, 4, 31), 22, ip(192, 168, 4, 1), None);
    let mut evidence = Evidence::default();
    let (record, place) = keep(
        learned(&home()),
        &[(&on(cafe()), T0), (&on(unresolved.clone()), T0 + MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Checking(Checking::Moved));
    assert_eq!(place.alab, Alab::Unknown);
    let (record, place) = keep(
        record,
        &[(&on(unresolved.clone()), T0 + MIN + SETTLE_MS + 1)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Checking(Checking::Unidentified));
    assert!(place.network.as_ref().is_some_and(|id| !id.is_strong()));
    for member in ["m1", "m2", "m3"] {
        evidence
            .answers
            .push(answer_on(&unresolved, member, T0 + 2 * MIN));
    }
    let (record, place) = keep(
        record,
        &[(&on(unresolved), T0 + 3 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan, "members answering are proof");
    assert_eq!(record.remembered.len(), 1, "the weak id is never learned");
    let (_, place) = keep(
        record,
        &[(&on(home()), T0 + 4 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan);
    assert!(place.network.as_ref().is_some_and(NetworkId::is_strong));
    assert_eq!(place.heard, 3, "the stay and its evidence carried on");
}

/// No default route and a path that cannot carry traffic is offline.
#[test]
fn no_default_route_and_an_unsatisfied_path_is_offline() {
    let (_, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&offline(), T0 + MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Offline);
    assert_eq!(place.internet.state, InternetState::Down);
    assert_eq!(place.internet.source, InternetSource::Path);
}

/// With a tunnel holding the default route the network is the physical one under it: the
/// remembered home with a VPN up is still the ALab LAN, and elsewhere the kind is VPN.
#[test]
fn a_full_tunnel_vpn_keys_the_network_by_the_physical_route() {
    let vpn_home = Facts {
        tunnel: Some("utun4".to_owned()),
        ..on(home())
    };
    let (_, place) = keep(
        learned(&home()),
        &[(&vpn_home, T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(place.kind, Some(Kind::Vpn));
    let vpn_cafe = Facts {
        tunnel: Some("utun4".to_owned()),
        ..on(cafe())
    };
    let (_, place) = keep(
        learned(&home()),
        &[(&vpn_cafe, T0)],
        &Evidence::default(),
        None,
    );
    assert_eq!(place.at, Where::Remote(Kind::Vpn));
}

/// An answer counts for this stay only where its dial began in the stay, its time is not ahead
/// of the clock, and its route took this network's interface: one over a tunnel, one whose dial
/// began before the move, and one stamped in the future count for no network.
#[test]
fn an_answer_off_this_networks_interface_or_stay_counts_for_no_network() {
    let start = T0 + 10 * MIN;
    let mut evidence = Evidence::default();
    let mut tunnel = answer_on(&cafe(), "m1", start + MIN);
    tunnel.via = Some(ip(100, 64, 0, 7));
    let early = answer_on(&cafe(), "m2", start - 1);
    let mut ahead = answer_on(&cafe(), "m3", start + MIN);
    ahead.at_ms = start + 2 * MIN + AHEAD_MAX_MS + 1;
    evidence.answers.extend([tunnel, early, ahead]);
    let (_, place) = keep(
        learned(&home()),
        &[
            (&on(home()), T0),
            (&on(cafe()), start),
            (&on(cafe()), start + 2 * MIN),
        ],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Remote(Kind::Lan));
    assert_eq!(place.heard, 0);
}

/// A home LAN split across routed segments (a VLAN, double NAT, a VM or Linux network): members
/// whose addresses lie off this Mac's subnet, reached through this network's interface, count,
/// so such a home is learned and recognised.
/// NEGATIVE CONTROL: counting only on-link addresses (`route.on_link(addr)` in place of the
/// `via` test) leaves this home unlearned (`checking`), and this test fails.
#[test]
fn a_routed_home_lan_is_recognised_from_members_through_this_gateway() {
    let mut evidence = Evidence::default();
    for (member, far) in [("m1", 7), ("m2", 8), ("m3", 9)] {
        let mut answer = answer_on(&home(), member, T0 + MIN);
        answer.addr = Some(IpAddr::V4(ip(10, 20, far, 5)));
        evidence.answers.push(answer);
    }
    let (record, place) = keep(
        Record::default(),
        &[(&on(home()), T0), (&on(home()), T0 + 2 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(record.remembered.len(), 1);
}

/// The internet reads `ok` only on an answer measured in this stay: an `ok` measured at home
/// reads `path-up` on the hotspot until something measures there, and a stale `ok` in the stay
/// falls back to `path-up` too.
/// NEGATIVE CONTROL: reading internet notes from any stay (dropping `note.at_ms >=
/// stay.start_ms`) keeps the home `ok` on the hotspot, and this test fails.
#[test]
fn internet_reads_ok_only_on_a_measured_answer_in_this_stay() {
    let mut evidence = Evidence::default();
    evidence.internet.push(InternetNote {
        ok: true,
        source: InternetSource::ApiProbe,
        at_ms: T0 + MIN,
        endpoint: "api.anthropic.com".to_owned(),
    });
    let (record, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&on(home()), T0 + 2 * MIN)],
        &evidence,
        None,
    );
    assert_eq!(place.internet.state, InternetState::Ok);
    assert_eq!(place.internet.source, InternetSource::ApiProbe);
    let (record, place) = keep(record, &[(&hotspot(), T0 + 3 * MIN)], &evidence, None);
    assert_eq!(place.internet.state, InternetState::PathUp);
    assert_eq!(place.internet.source, InternetSource::Path);
    evidence.internet.push(InternetNote {
        ok: true,
        source: InternetSource::TurnReply,
        at_ms: T0 + 4 * MIN,
        endpoint: "api.anthropic.com".to_owned(),
    });
    let (record, place) = keep(record, &[(&hotspot(), T0 + 5 * MIN)], &evidence, None);
    assert_eq!(place.internet.state, InternetState::Ok);
    assert_eq!(place.internet.source, InternetSource::TurnReply);
    let (_, place) = keep(
        record,
        &[(&hotspot(), T0 + 4 * MIN + INTERNET_FRESH_MS + 1)],
        &evidence,
        None,
    );
    assert_eq!(place.internet.state, InternetState::PathUp);
}

/// The first run after the update has nothing remembered: the place reads `checking`, the ALab
/// axis `learning`, never away; where the fleet book says three members answered from this
/// subnet in the last day while this Mac was on this network, the first stay reads the ALab LAN,
/// seeded, and remembers nothing. A hotel numbered like home, behind another router, seeds
/// nothing.
/// NEGATIVE CONTROLS: seeding from every book member whatever its address makes the café read
/// alab-lan on the first run, and this test fails; seeding by the subnet alone (dropping the
/// `member.network` filter in `seeded`) seeds the hotel, and "a hotel numbered like home" fails.
#[test]
fn the_first_run_reads_learning_and_the_book_seeds_home_without_remembering() {
    let (_, place) = keep(
        Record::default(),
        &[(&on(cafe()), T0)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::Checking(Checking::Learning));
    assert_eq!(place.alab, Alab::Learning);
    let mut seen = book(41);
    for (i, member) in seen.members.iter_mut().enumerate().take(LEARN_AT) {
        member.addr = Some(IpAddr::V4(ip(192, 168, 5, 10 + i as u8)));
        member.answered_ms = Some(T0 - 2 * 60 * MIN);
        member.network = NetworkId::of(&home());
    }
    let hotel = route(ip(192, 168, 4, 77), 22, ip(192, 168, 4, 1), Some(&CAFE_HW));
    let (_, place) = keep(
        Record::default(),
        &[(&on(hotel), T0)],
        &Evidence::default(),
        Some(&seen),
    );
    assert_eq!(
        place.at,
        Where::Checking(Checking::Learning),
        "a hotel numbered like home"
    );
    let (_, place) = keep(
        Record::default(),
        &[(&on(cafe()), T0)],
        &Evidence::default(),
        Some(&seen),
    );
    assert_eq!(
        place.at,
        Where::Checking(Checking::Learning),
        "not this subnet"
    );
    let (record, place) = keep(
        Record::default(),
        &[(&on(home()), T0)],
        &Evidence::default(),
        Some(&seen),
    );
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(place.recognised, Some(Recognised::Seeded));
    assert!(record.remembered.is_empty(), "a seed remembers nothing");
}

/// A move heard after a remote stay is this Mac returning alone; a router or access point
/// reboot (the ALab LAN, offline, the same ALab LAN) and a move away from home are not.
/// NEGATIVE CONTROL: the first design's rule, a lone return whenever the newest stay is not the
/// ALab LAN (offline counted as away), makes the reboot a lone return: "a reboot keeps the herd"
/// fails.
#[test]
fn only_a_return_from_a_remote_stay_skips_the_herd_and_a_reboot_does_not() {
    let (record, _) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&offline(), T0 + 10 * MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(
        !record.lone_return(T0 + 10 * MIN + 30_000),
        "a reboot keeps the herd"
    );
    let (record, _) = keep(
        record,
        &[(&on(home()), T0 + 11 * MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(
        !record.lone_return(T0 + 11 * MIN),
        "back home after the reboot"
    );
    let (record, _) = keep(
        record,
        &[(&hotspot(), T0 + 20 * MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(
        record.lone_return(T0 + 60 * MIN),
        "leaving a long remote stay"
    );
    assert!(
        !record.lone_return(T0 + 20 * MIN + 1_000),
        "a remote stay of a second is no return"
    );
}

/// Away-since survives a keeper restart on the same network; a restart on a network the record
/// does not continue opens a stay that says it began before the keeper started.
#[test]
fn away_since_survives_a_restart_and_a_new_network_says_since_before() {
    let (record, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&hotspot(), T0 + 10 * MIN)],
        &Evidence::default(),
        Some(&book(41)),
    );
    let away = place.alab;
    let restart = T0 + 30 * MIN;
    let (next, place) = step(
        &record,
        &hotspot(),
        &Evidence::default(),
        Some(&book(41)),
        restart,
        Some(restart),
    );
    assert_eq!(place.alab, away);
    assert_eq!(place.since_ms, Some(T0 + 10 * MIN));
    assert!(!place.since_before);
    let (_, place) = step(
        &next,
        &on(cafe()),
        &Evidence::default(),
        Some(&book(41)),
        restart + 1,
        Some(restart),
    );
    assert!(place.since_before);
    assert_eq!(place.since_ms, Some(restart + 1));
}

/// The wanted-internet-path seam engages only away from the ALab LAN with the internet `ok`.
#[test]
fn the_internet_path_is_wanted_only_away_with_the_internet_answering() {
    let mut evidence = Evidence::default();
    let (record, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&hotspot(), T0 + MIN)],
        &evidence,
        None,
    );
    assert!(!place.wants_internet_path(), "path up is not measured");
    evidence.internet.push(InternetNote {
        ok: true,
        source: InternetSource::ApiProbe,
        at_ms: T0 + 2 * MIN,
        endpoint: "api.anthropic.com".to_owned(),
    });
    let (_, place) = keep(record, &[(&hotspot(), T0 + 3 * MIN)], &evidence, None);
    assert!(place.wants_internet_path());
}

/// A router reboot during which the Macs fell back to another network they know (a guest or
/// extender network, an auto-joined hotspot) for a few minutes keeps the herd on the way back:
/// every Mac took that fallback, so every Mac returns at once. Only a remote stay past
/// [`LONE_RETURN_AFTER_MS`] (a person carried the Mac away) returns alone.
/// NEGATIVE CONTROL: the first threshold, the route source's 30 s rest
/// (`LONE_RETURN_AFTER_MS = 30_000`), makes the five-minute fallback a lone return, and "a
/// fallback during a reboot keeps the herd" fails.
#[test]
fn a_fallback_network_during_a_reboot_keeps_the_herd() {
    let (record, _) = keep(
        learned(&home()),
        &[
            (&on(home()), T0),
            (&offline(), T0 + 10 * MIN),
            (&on(cafe()), T0 + 10 * MIN + 20_000),
            (&offline(), T0 + 15 * MIN),
            (&on(home()), T0 + 16 * MIN),
        ],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(
        !record.lone_return(T0 + 16 * MIN + 5_000),
        "a fallback during a reboot keeps the herd"
    );
    let (record, _) = keep(
        record,
        &[
            (&on(cafe()), T0 + 60 * MIN),
            (&on(home()), T0 + 60 * MIN + LONE_RETURN_AFTER_MS + MIN),
        ],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(
        record.lone_return(T0 + 60 * MIN + LONE_RETURN_AFTER_MS + MIN + 5_000),
        "a stay past the threshold returns alone"
    );
}

/// Away for days, each wake and move a stay of its own, pushes the last ALab LAN stay out of the
/// [`STAYS_MAX`] the record keeps: away-since then reads unknown, never the oldest kept stay's
/// start passed off as when this Mac left.
/// NEGATIVE CONTROL: the first rule, the oldest stay's start where no ALab LAN stay is kept
/// (`None => record.stays.first().map(|stay| stay.start_ms)`), reads an invented time, and "the
/// record lost when it left" fails.
#[test]
fn away_since_reads_unknown_once_the_last_home_stay_is_gone() {
    let mut steps: Vec<(Facts, u64)> = vec![(on(home()), T0), (hotspot(), T0 + MIN)];
    for k in 0..STAYS_MAX as u64 {
        let at = T0 + (2 + 2 * k) * 60 * MIN;
        steps.push((offline(), at));
        steps.push((hotspot(), at + MIN));
    }
    let steps: Vec<(&Facts, u64)> = steps.iter().map(|(facts, at)| (facts, *at)).collect();
    let (record, place) = keep(
        learned(&home()),
        &steps[..4],
        &Evidence::default(),
        Some(&book(41)),
    );
    assert!(record.stays.iter().any(|stay| stay.at == StayAt::AlabLan));
    assert_eq!(
        place.alab,
        Alab::Away {
            since_ms: Some(T0 + MIN)
        }
    );
    let (record, place) = keep(record, &steps[4..], &Evidence::default(), Some(&book(41)));
    assert!(
        !record.stays.iter().any(|stay| stay.at == StayAt::AlabLan),
        "PRECONDITION: the home stay was pushed out"
    );
    assert_eq!(
        place.alab,
        Alab::Away { since_ms: None },
        "the record lost when it left"
    );
}

/// At home with members answering, the keeper's looks leave the record as it was (it writes
/// only on a change): a remembered network's confirmation moves on once an hour, not at every
/// look.
/// NEGATIVE CONTROL: confirming at every look (`confirm` setting `confirmed_ms = now` whatever
/// its age) changes the record between two looks ten seconds apart, and "the next look changes
/// nothing" fails.
#[test]
fn looks_at_home_leave_the_record_unchanged_until_the_confirmation_is_an_hour_old() {
    let mut evidence = Evidence::default();
    for member in ["m1", "m2", "m3"] {
        evidence.answers.push(answer_on(&home(), member, T0 + MIN));
    }
    let (record, place) = keep(
        Record::default(),
        &[(&on(home()), T0), (&on(home()), T0 + 2 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(record.remembered.len(), 1, "learned");
    let (next, _) = keep(
        record.clone(),
        &[(&on(home()), T0 + 2 * MIN + 10_000)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(next, record, "the next look changes nothing");
    let later = T0 + 2 * MIN + CONFIRM_EVERY_MS;
    let (next, _) = keep(record, &[(&on(home()), later)], &evidence, Some(&book(41)));
    assert_eq!(
        next.remembered[0].confirmed_ms, later,
        "an hour on, confirmed again"
    );
}

/// At home behind a firewall here (or with members refusing this Mac's key), the heartbeat's
/// dials to the members on this network are refused by this Mac itself or by the members: those rounds are not silence, and the remembered network stays the ALab LAN however
/// many of them come.
/// NEGATIVE CONTROL: counting every round that heard no member as silent (dropping
/// `round.blocked == 0` from `silent_rounds`) reads home as "a network members were once on",
/// and "refused rounds are no silence" fails.
#[test]
fn rounds_this_mac_or_a_member_refused_are_no_silence() {
    let mut evidence = Evidence::default();
    for k in 0..(SILENT_ROUNDS as u64 + 2) {
        evidence.rounds.push(Round {
            lane: Lane::Fleet,
            answered: 0,
            blocked: 3,
            began_ms: T0 + (2 + 3 * k) * MIN,
            at_ms: T0 + (2 + 3 * k) * MIN + 1_000,
        });
    }
    let (_, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&on(home()), T0 + 20 * MIN)],
        &evidence,
        Some(&book(41)),
    );
    assert_eq!(place.at, Where::AlabLan, "refused rounds are no silence");
    assert!(!place.once_alab);
    assert_eq!(place.alab, Alab::Here);
}

/// A measured `down` goes stale as an `ok` does: a router whose Wi-Fi came back before its WAN
/// measured down once, and an idle Mac with nothing measuring since reads what the path says once
/// that note is [`INTERNET_FRESH_MS`] old, not `down` for the whole stay.
/// NEGATIVE CONTROL: the first rule, a `down` that never goes stale (`!note.ok ||` in
/// `internet_in`'s guard), reads `down` an hour on, and "a stale down reads the path" fails.
#[test]
fn a_measured_down_goes_stale_too() {
    let mut evidence = Evidence::default();
    evidence.internet.push(InternetNote {
        ok: false,
        source: InternetSource::ApiProbe,
        at_ms: T0 + MIN,
        endpoint: "api.anthropic.com".to_owned(),
    });
    let (record, place) = keep(
        learned(&home()),
        &[(&on(home()), T0), (&on(home()), T0 + 2 * MIN)],
        &evidence,
        None,
    );
    assert_eq!(place.internet.state, InternetState::Down);
    assert_eq!(place.internet.source, InternetSource::ApiProbe);
    let (_, place) = keep(
        record,
        &[(&on(home()), T0 + MIN + INTERNET_FRESH_MS + 1)],
        &evidence,
        None,
    );
    assert_eq!(
        (place.internet.state, place.internet.source),
        (InternetState::PathUp, InternetSource::Path),
        "a stale down reads the path"
    );
}

/// The ids and member keys an evidence file holds: what a Mac wrote before this crate's fold
/// became `aterm_hash`'s still reads as the same network and the same member.
#[test]
fn the_ids_and_member_keys_an_evidence_file_holds_do_not_move() {
    let home = route(ip(192, 168, 1, 20), 24, ip(192, 168, 1, 1), Some(&HOME_HW));
    assert_eq!(
        NetworkId::of(&home).expect("a strong id").as_str(),
        "na35ff2c33ad2762c"
    );
    assert_eq!(NetworkId::weak(&home).as_str(), "wfd8f58dc213c8a62");
    assert_eq!(member_key(Lane::Fleet, "m1"), "0d66671f19b65cf3");
    assert_eq!(member_key(Lane::Commons, "m1"), "94a7181e890f14c5");
}
