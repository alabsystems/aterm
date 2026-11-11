// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::place::{Book, Evidence, Facts, Link, PathFacts, RouteFacts, step};
use std::net::Ipv4Addr;

const T0: u64 = 1_800_000_000_000;

fn on(hw: u8, expensive: bool) -> Facts {
    Facts {
        route: Some(RouteFacts {
            interface: "en0".to_owned(),
            address: Ipv4Addr::new(192, 168, 4, 31),
            prefix: 22,
            gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
            gateway_hw: Some(vec![0, 1, 2, 3, 4, hw]),
        }),
        tunnel: None,
        path: Some(PathFacts {
            usable: Some(true),
            expensive,
            constrained: false,
            link: Link::Wifi,
        }),
    }
}

fn offline() -> Facts {
    Facts {
        route: None,
        tunnel: None,
        path: Some(PathFacts {
            usable: Some(false),
            expensive: false,
            constrained: false,
            link: Link::Wifi,
        }),
    }
}

/// THE PLACE SETTLING IS A MOVE: from a remote stay through offline onto the remembered ALab
/// LAN tells one move (when the network is read, not when the link went down), and that move
/// is a lone return; a router reboot (the ALab LAN, offline, the same ALab LAN) tells one too,
/// once the network is back, and that one keeps the herd; a look that finds nothing changed,
/// or the place unsettled, tells nothing.
/// NEGATIVE CONTROLS: telling a change only between two settled places (the last arm's
/// `before.as_ref() != Some(&now)` → `before.is_some_and(|before| before != now)`) misses the
/// arrival after offline, and "home again" fails; `Record::lone_return` without its pass over
/// the stay the move arrived in reads that arrival as the ALab LAN's, and "a lone return"
/// fails.
#[test]
fn the_place_settling_onto_a_network_is_one_move() {
    let book = Book::default();
    let evidence = Evidence::default();
    let home = on(5, false);
    let (seed, _) = step(
        &Record::default(),
        &home,
        &evidence,
        Some(&book),
        T0,
        Some(T0),
    );
    let mut record = seed;
    record.remembered.push(crate::place::Known {
        network: NetworkId::of(home.route.as_ref().expect("a route")).expect("strong"),
        learned_ms: T0,
        confirmed_ms: T0,
    });
    let mut watch = SettledWatch::new();
    let mut walk = |facts: &Facts, now: u64, record: &mut Record| {
        let (next, _) = step(record, facts, &evidence, Some(&book), now, Some(T0));
        *record = next;
        watch.look(Some(record), now)
    };
    assert!(
        !walk(&home, T0 + 1_000, &mut record),
        "the first look learns"
    );
    assert!(!walk(&home, T0 + 2_000, &mut record), "nothing changed");
    assert!(
        walk(&on(9, false), T0 + 60_000, &mut record),
        "another network"
    );
    let moved_at = T0 + 30 * 60_000;
    assert!(
        !walk(&offline(), moved_at, &mut record),
        "offline is no arrival"
    );
    assert!(
        walk(&home, moved_at + 5_000, &mut record),
        "home again, once its network is read"
    );
    assert!(record.lone_return(moved_at + 5_000), "a lone return");
    assert!(!walk(&home, moved_at + 6_000, &mut record));
    let reboot = moved_at + 60 * 60_000;
    assert!(!walk(&offline(), reboot, &mut record));
    assert!(
        walk(&home, reboot + 30_000, &mut record),
        "back after a reboot"
    );
    assert!(
        !record.lone_return(reboot + 30_000),
        "a reboot keeps the herd"
    );
    assert!(!watch.look(None, reboot), "an unread record tells nothing");
}

/// A FLAPPING LINK TELLS AT MOST ONE MOVE A REST: weak Wi-Fi drops to offline and back onto
/// the same network every few seconds, and only the first settle is told at once; the next
/// within [`REST_MS`] is held and told when the rest ends, once, while another network is
/// told at once whatever the rest.
/// NEGATIVE CONTROL: telling every settle (`rested` always true) tells the flap back within
/// the rest, and "a flap within the rest tells nothing" fails.
#[test]
fn a_flapping_link_tells_at_most_one_move_a_rest() {
    let book = Book::default();
    let evidence = Evidence::default();
    let home = on(5, false);
    let mut record = Record::default();
    let mut watch = SettledWatch::new();
    let mut walk = |facts: &Facts, now: u64, record: &mut Record| {
        let (next, _) = step(record, facts, &evidence, Some(&book), now, Some(T0));
        *record = next;
        watch.look(Some(record), now)
    };
    assert!(!walk(&offline(), T0, &mut record), "the first look learns");
    assert!(walk(&home, T0 + 1_000, &mut record), "the arrival, at once");
    assert!(!walk(&offline(), T0 + 4_000, &mut record));
    assert!(
        !walk(&home, T0 + 6_000, &mut record),
        "a flap within the rest tells nothing"
    );
    assert!(!walk(&home, T0 + 20_000, &mut record), "still resting");
    assert!(
        walk(&home, T0 + 1_000 + REST_MS, &mut record),
        "the held settle, once the rest ends"
    );
    assert!(!walk(&home, T0 + 2_000 + REST_MS, &mut record), "once");
    assert!(
        walk(&on(9, false), T0 + 3_000 + REST_MS, &mut record),
        "another network, at once"
    );
}
