// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::place::{ComputedBy, Internet, Link, NetworkId, PathFacts, RouteFacts};
use std::net::Ipv4Addr;

fn place(at: Where, alab: Alab) -> Place {
    let route = RouteFacts {
        interface: "en0".to_owned(),
        address: Ipv4Addr::new(172, 20, 10, 2),
        prefix: 28,
        gateway: Some(Ipv4Addr::new(172, 20, 10, 1)),
        gateway_hw: Some(vec![2, 0, 0, 0, 0, 1]),
    };
    Place {
        at,
        kind: Some(Kind::Hotspot { by_range: false }),
        recognised: None,
        once_alab: false,
        network: NetworkId::of(&route),
        route: Some(route),
        tunnel: None,
        path: Some(PathFacts {
            usable: Some(true),
            expensive: true,
            constrained: false,
            link: Link::Wifi,
        }),
        since_ms: Some(1_800_000_000_000),
        since_before: false,
        internet: Internet {
            state: InternetState::PathUp,
            source: InternetSource::Path,
            at_ms: None,
            endpoint: None,
        },
        alab,
        heard: 0,
        known: Some(41),
        last_answer: None,
        computed_by: ComputedBy::Keeper,
    }
}

/// The `status` fields are `key=value` words in one order, each value from the place, but
/// `members=` from the seats presence read: a script reads the place, its kind, its stay, the
/// internet, the seats up of the members and away-since. A reporter that read no presence
/// prints `members=-`, whatever the place heard.
/// NEGATIVE CONTROL: printing `members=` as the up count alone (no `/<members>`) fails the
/// members field.
#[test]
fn the_status_fields_carry_the_place_in_order() {
    let away = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away {
            since_ms: Some(1_799_999_000_000),
        },
    );
    let seats = Seats {
        members: 52,
        up: Some(47),
        ..Seats::default()
    };
    let fields: Vec<(String, String)> = status_fields(&away, Some(&seats))
        .split(' ')
        .map(|kv| {
            let (k, v) = kv.split_once('=').expect("key=value");
            (k.to_owned(), v.to_owned())
        })
        .collect();
    let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "net",
            "net_kind",
            "net_since_ms",
            "internet",
            "internet_ms",
            "members",
            "away_since_ms"
        ]
    );
    let value = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(value("net"), Some("remote"));
    assert_eq!(value("net_kind"), Some("hotspot"));
    assert_eq!(value("net_since_ms"), Some("1800000000000"));
    assert_eq!(value("internet"), Some("path-up"));
    assert_eq!(value("internet_ms"), Some("-"));
    assert_eq!(value("members"), Some("47/52"));
    assert_eq!(value("away_since_ms"), Some("1799999000000"));
    let unknown_keys: Vec<&str> = STATUS_UNKNOWN
        .split(' ')
        .map(|kv| kv.split_once('=').expect("key=value").0)
        .collect();
    assert_eq!(unknown_keys, keys, "the unknown line has the same keys");
    let home = Place {
        heard: 12,
        ..place(Where::AlabLan, Alab::Here)
    };
    assert!(status_fields(&home, Some(&seats)).contains("away_since_ms=-"));
    assert!(
        status_fields(&home, None).contains(" members=- "),
        "no presence read, no count"
    );
    assert!(
        status_fields(
            &home,
            Some(&Seats {
                members: 52,
                ..Seats::default()
            })
        )
        .contains(" members=- "),
        "seats with no presence read count no one up"
    );
}

/// THE STATUS LINE against a presence fixture with old builds (one-mesh §11 B9): members,
/// up, each member that needs an update with its release, the tables in step with the
/// slowest's lag and those apart, and the log's fill, in that order; with no presence read it
/// says so and counts no one up or in step.
#[test]
fn the_status_line_words_a_presence_fixture_with_old_builds() {
    let seats = Seats {
        members: 52,
        up: Some(47),
        needs_update: vec![
            ("m11".to_owned(), "0.147.0+g1a2b3c4d".to_owned()),
            ("m12".to_owned(), "0.146.0".to_owned()),
            ("m106".to_owned(), "0.148.2+gdeadbeef".to_owned()),
        ],
        in_step: Some(InStep {
            with: 46,
            of: 47,
            slowest_ms: Some(40_000),
            apart: 0,
        }),
        log: Some((4_169, 16_384)),
    };
    assert_eq!(
        status_line(&seats),
        "52 members · 47 up · 3 need update (m11 0.147, m12 0.146, m106 0.148) · tables in \
         step with 46 of 47, slowest 40 s behind · log 4,169 of 16,384"
    );
    assert_eq!(seats_short(&seats), "47 up of 52 members");
    let one_old = Seats {
        members: 2,
        up: Some(1),
        needs_update: vec![("m3".to_owned(), "odd-build".to_owned())],
        in_step: Some(InStep {
            with: 0,
            of: 0,
            slowest_ms: None,
            apart: 0,
        }),
        log: None,
    };
    assert_eq!(
        status_line(&one_old),
        "2 members · 1 up · 1 needs update (m3 odd-build) · no other member up"
    );
    let apart = Seats {
        in_step: Some(InStep {
            with: 3,
            of: 5,
            slowest_ms: Some(2 * 3_600_000),
            apart: 1,
        }),
        needs_update: Vec::new(),
        ..seats.clone()
    };
    assert!(
        status_line(&apart).contains("tables in step with 3 of 5, slowest 2 h behind, 1 apart"),
        "{}",
        status_line(&apart)
    );
    let unread = Seats {
        members: 1,
        up: None,
        needs_update: Vec::new(),
        in_step: None,
        log: Some((12, 16_384)),
    };
    assert_eq!(
        status_line(&unread),
        "1 member · up not read · log 12 of 16,384"
    );
    assert_eq!(seats_short(&unread), "1 member");
}

/// The JSON object carries the axes apart: where, kind, the internet with its source, and
/// the ALab LAN with its counts.
#[test]
fn the_json_fields_keep_the_two_axes_apart() {
    let away = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away { since_ms: Some(7) },
    );
    let fields = json_fields(&away);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("state"), Some(Field::Text("remote".to_owned())));
    assert_eq!(get("kind"), Some(Field::Text("hotspot".to_owned())));
    assert_eq!(get("expensive"), Some(Field::Flag(true)));
    let Some(Field::Object(internet)) = get("internet") else {
        panic!("internet object");
    };
    assert!(internet.contains(&("state", Field::Text("path-up".to_owned()))));
    assert!(internet.contains(&("source", Field::Text("path".to_owned()))));
    let Some(Field::Object(alab)) = get("alab") else {
        panic!("alab object");
    };
    assert!(alab.contains(&("state", Field::Text("away".to_owned()))));
    assert!(alab.contains(&("since_ms", Field::Count(7))));
    assert!(
        !alab
            .iter()
            .any(|(key, _)| matches!(*key, "known" | "reachable")),
        "the place counts no members for a reader: presence does"
    );
}

/// The away entry stands only while this Mac is on a remote network with the ALab LAN
/// reading away, and has been there for a while: never at home, on a first run that learned
/// nothing, while checking, offline (a router reboot past the dwell, at home), or in the
/// first two minutes of a stay (a Wi-Fi blip); and its title and step pass the exceptions
/// vocabulary's title rule (six words, no clause, no full stop).
/// NEGATIVE CONTROLS: dropping the dwell (`if !settled { return None; }`) raises it for a stay
/// a second old, and "a blip raises nothing" fails; taking any place whose ALab axis reads
/// away (the first rule, which let `Where::Offline` through) raises it offline, and the
/// `Offline` row fails.
#[test]
fn the_away_entry_stands_only_away_and_past_a_blip() {
    let since = 1_800_000_000_000;
    let at = |now_ms| Words {
        now_ms,
        zone: &|_| Some(0),
    };
    let away = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away {
            since_ms: Some(since - 1_000),
        },
    );
    let entry = away_entry(&away, &at(since + AWAY_ENTRY_AFTER_MS)).expect("away");
    assert!(entry.what.split_whitespace().count() <= 6 && !entry.what.ends_with('.'));
    assert!(!entry.more.is_empty());
    assert!(
        away_entry(&away, &at(since + 1_000)).is_none(),
        "a blip raises nothing"
    );
    let words = at(since + 60 * AWAY_ENTRY_AFTER_MS);
    for (where_, alab) in [
        (Where::AlabLan, Alab::Here),
        (
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Learning,
        ),
        (Where::Checking(Checking::Moved), Alab::Unknown),
        (
            Where::Offline,
            Alab::Away {
                since_ms: Some(since - 1_000),
            },
        ),
    ] {
        assert!(
            away_entry(&place(where_, alab), &words).is_none(),
            "{where_:?} {alab:?}"
        );
    }
}

/// A remembered network that fell silent ("a network members were once on") may be home
/// with every member asleep or off: no why, no survey note and no away entry say this Mac is
/// elsewhere; a member on the network this Mac is on is one being away never explains.
/// NEGATIVE CONTROL: `why_away` without its `once_alab` return fails "no why on a network
/// members were once on".
#[test]
fn a_network_members_were_once_on_blames_nothing_away() {
    let words = Words {
        now_ms: 1_800_000_000_000 + 60 * AWAY_ENTRY_AFTER_MS,
        zone: &|_| Some(0),
    };
    let once = Place {
        once_alab: true,
        kind: Some(Kind::Lan),
        ..place(Where::Remote(Kind::Lan), Alab::Away { since_ms: Some(7) })
    };
    assert!(
        why_away(&once, &words).is_none(),
        "no why on a network members were once on"
    );
    assert!(survey_note(&once, &words).is_none());
    assert!(away_entry(&once, &words).is_none());
    let hotspot = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away { since_ms: Some(7) },
    );
    assert!(on_this_network(
        &hotspot,
        std::net::IpAddr::V4(Ipv4Addr::new(172, 20, 10, 5))
    ));
    assert!(!on_this_network(
        &hotspot,
        std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))
    ));
}

/// THE ON-LINK RULE judges a no-route dial by its address against the network this Mac is on
/// now: on this network, that member's own (off the link now), a network members were once
/// on alike; off this network, or with no route read, elsewhere; with no address, unplaced.
/// NEGATIVE CONTROL: `on_link` counting every addressed no-route as gone fails "off this
/// network".
#[test]
fn a_no_route_dial_is_judged_by_its_address_on_the_network_this_mac_is_on() {
    use std::net::IpAddr;
    let ip = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
    let (here, home) = (ip(172, 20, 10, 5), ip(192, 168, 4, 9));
    let hotspot = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away { since_ms: Some(7) },
    );
    let gone = on_link(&hotspot, [Dial::NoRoute(Some(here)), Dial::Other]);
    assert_eq!(
        gone,
        OnLink {
            gone: 1,
            ..OnLink::default()
        },
        "a member on this hotspot with no route: its own"
    );
    assert!(gone.gone_at(&hotspot, here));
    assert!(
        !gone.gone_at(&hotspot, home),
        "off this network is not gone"
    );
    assert_eq!(
        on_link(&hotspot, [Dial::NoRoute(Some(home))]),
        OnLink {
            elsewhere: 1,
            ..OnLink::default()
        },
        "off this network"
    );
    assert_eq!(
        on_link(&hotspot, [Dial::NoRoute(None)]),
        OnLink {
            unplaced: 1,
            ..OnLink::default()
        }
    );
    let once = Place {
        once_alab: true,
        kind: Some(Kind::Lan),
        ..place(Where::Remote(Kind::Lan), Alab::Away { since_ms: Some(7) })
    };
    assert_eq!(
        on_link(&once, [Dial::NoRoute(Some(here))]).gone,
        1,
        "a network members were once on changes nothing"
    );
    let unrouted = Place {
        route: None,
        ..hotspot.clone()
    };
    assert_eq!(
        on_link(&unrouted, [Dial::NoRoute(Some(here))]).elsewhere,
        1,
        "with no route read nothing is on this network"
    );
}

/// A why and a survey note come only while this Mac is on a remote network: never on the
/// ALab LAN, offline or while checking, where they would blame the wrong thing.
#[test]
fn the_why_and_the_survey_note_come_only_on_a_remote_network() {
    let words = Words {
        now_ms: 1_800_000_000_000,
        zone: &|_| Some(0),
    };
    let away = place(
        Where::Remote(Kind::Hotspot { by_range: false }),
        Alab::Away { since_ms: None },
    );
    assert!(why_away(&away, &words).is_some());
    assert!(survey_note(&away, &words).is_some());
    for at in [
        Where::AlabLan,
        Where::Offline,
        Where::Checking(Checking::Learning),
    ] {
        let p = place(at, Alab::Here);
        assert!(why_away(&p, &words).is_none(), "{at:?}");
        assert!(survey_note(&p, &words).is_none(), "{at:?}");
    }
}
