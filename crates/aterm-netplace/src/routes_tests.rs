// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::place::{Answer, InternetState, member_key};

#[cfg(unix)]
#[test]
fn pacing_cache_separates_roots_and_expires_at_the_existing_boundary() {
    let mut cache = PacingCache::default();
    let first = Path::new("fixture/first");
    let other = Path::new("fixture/other");
    let at = std::time::Instant::now();
    let due = at + crate::keeper::CACHE_FOR;
    assert_eq!(cache.get(first, at), None);
    cache.remember(first, at, true);
    assert_eq!(cache.get(first, at), Some(true));
    assert_eq!(cache.get(other, at), None, "another root never borrows it");
    assert_eq!(
        cache.get(first, due - std::time::Duration::from_nanos(1)),
        Some(true)
    );
    assert_eq!(cache.get(first, due), None, "the boundary is exclusive");
    assert_eq!(
        cache.get(first, due + std::time::Duration::from_secs(1)),
        None
    );
    cache.remember(first, due, false);
    assert_eq!(
        cache.get(first, due),
        Some(false),
        "refresh replaces the decision"
    );
    cache.remember(other, due, true);
    assert_eq!(cache.get(other, due), Some(true));
    assert_eq!(cache.get(first, due), None, "only the last root is cached");
}

#[cfg(unix)]
#[test]
fn pacing_cache_hits_and_same_root_refreshes_allocate_nothing() {
    let mut cache = PacingCache::default();
    let root = Path::new("fixture/pacing-state");
    let at = std::time::Instant::now();
    let (_, control) =
        aterm_heap_count::allocations_in(|| std::hint::black_box(root.to_path_buf()));
    assert!(control > 0, "the allocation counter sees an owned root");
    cache.remember(root, at, true);
    let (_, allocations) = aterm_heap_count::allocations_in(|| {
        for step in 0..64 {
            let now = at + std::time::Duration::from_nanos(step);
            cache.remember(root, now, step % 2 == 0);
            assert_eq!(
                std::hint::black_box(cache.get(root, now)),
                Some(step % 2 == 0)
            );
        }
    });
    assert_eq!(
        allocations, 0,
        "the cache retains only its root and a boolean"
    );
}

const HOTSPOT_ADDR: Ipv4Addr = Ipv4Addr::new(172, 20, 10, 2);

fn hotspot() -> Place {
    crate::sample::hotspot(1_000_000, 900_000)
}

/// The kernel's routes on the hotspot: the home LAN's addresses leave through the hotspot's
/// own interface, `10.8.0.0/16` through a VPN's tunnel.
fn via(addr: IpAddr) -> Option<Ipv4Addr> {
    match addr {
        IpAddr::V4(v4) if v4.octets()[0] == 10 => Some(Ipv4Addr::new(10, 8, 0, 2)),
        IpAddr::V4(v4) if v4.octets()[..3] == [198, 51, 100] => None,
        _ => Some(HOTSPOT_ADDR),
    }
}

fn ip(text: &str) -> Option<IpAddr> {
    Some(text.parse().expect("an address"))
}

/// Away on a hotspot, an automatic lane dials a member on the hotspot itself, one a VPN
/// routes and one that answered in this stay, and none of the home LAN's addresses (they
/// leave through the hotspot toward the internet) or members with no address; at home it
/// dials every one.
/// NEGATIVE CONTROL: answering `true` for every address once paced (dropping the route test,
/// `(self.via)(addr).is_some_and(..)` → `true`) dials the home LAN's address from the hotspot,
/// and "behind the ALab LAN" fails; dropping the loopback rule (`is_loopback()` →
/// `is_loopback() && false`) passes over a member on this Mac, and "this Mac itself" fails.
#[test]
fn away_a_lane_dials_only_members_this_network_can_reach() {
    let mut evidence = Evidence::default();
    evidence.answers.push(Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, "heard-here"),
        addr: ip("192.168.4.9"),
        via: Some(HOTSPOT_ADDR),
        iface: Some("en0".to_owned()),
        network: None,
        logged: true,
        began_ms: 1_000_500,
        at_ms: 1_001_000,
    });
    evidence.answers.push(Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, "heard-at-home"),
        addr: ip("192.168.4.10"),
        via: Some(Ipv4Addr::new(192, 168, 4, 31)),
        iface: Some("en0".to_owned()),
        network: None,
        logged: true,
        began_ms: 500_000,
        at_ms: 500_100,
    });
    let away = Routes::of(hotspot(), &evidence, via);
    assert!(away.paced());
    assert_eq!(away.alab_path(), AlabPath::None);
    assert!(away.dials(None, ip("172.20.10.5")), "on the hotspot itself");
    assert!(away.dials(None, ip("10.8.3.4")), "a VPN routes it");
    assert!(
        away.dials(Some("heard-here"), ip("192.168.4.9")),
        "it answered in this stay"
    );
    assert!(
        !away.dials(Some("heard-at-home"), ip("192.168.4.10")),
        "behind the ALab LAN"
    );
    assert!(!away.dials(None, ip("192.168.4.77")), "behind the ALab LAN");
    assert!(!away.dials(None, None), "no address to judge");
    assert!(!away.dials(None, ip("198.51.100.7")), "no route named");
    assert!(away.dials(None, ip("127.0.0.1")), "this Mac itself");
    let home = Routes::of(crate::sample::home(1_000_000, 3), &evidence, via);
    assert!(!home.paced());
    assert_eq!(home.alab_path(), AlabPath::Lan);
    assert!(home.dials(None, ip("192.168.4.77")));
    assert!(home.dials(None, None));
}

/// Away, a round that dials every member is due once on each network this Mac comes to and
/// once every half hour after, never in between; at home every round is.
/// NEGATIVE CONTROL: dropping the network test (`network != self.network()`) makes the round
/// right after a move wait out the half hour, and "the first round after a move" fails.
#[test]
fn away_every_member_is_dialled_after_each_move_and_each_half_hour() {
    let away = Routes::of(hotspot(), &Evidence::default(), via);
    let here = away.network().cloned();
    let home = Routes::of(crate::sample::home(1_000_000, 3), &Evidence::default(), via);
    let before = home.network().cloned();
    assert!(away.full_round_due(None, 1_000_000), "the first round");
    assert!(
        away.full_round_due(Some((before.as_ref(), 999_000)), 1_000_000),
        "the first round after a move"
    );
    assert!(!away.full_round_due(Some((here.as_ref(), 1_000_000)), 1_000_000 + 60_000));
    assert!(away.full_round_due(
        Some((here.as_ref(), 1_000_000)),
        1_000_000 + AWAY_FULL_EVERY_MS
    ));
    assert!(home.full_round_due(Some((before.as_ref(), 1_000_000)), 1_000_001));
}

/// A lane's rounds away: the first on a network dials every member, the next passes over the
/// ones behind the ALab LAN (and counts them), the first on another network dials every one
/// again; a lane under another state root, or another lane, paces from its own memory.
/// NEGATIVE CONTROL: never remembering a full round (dropping the `rounds[at] = entry` /
/// `rounds.push(entry)` write) makes every round full, and "the next round on it is paced"
/// fails.
#[test]
fn a_lane_dials_every_member_once_per_network_then_paces() {
    let root = Path::new("/nonexistent/aterm-round-test");
    let away = || Routes::of(hotspot(), &Evidence::default(), via);
    let behind = ip("192.168.4.77");
    let mut first = Round::begin_at(root, "fleet", away(), 1_000_000);
    assert!(first.full(), "the first round on a network");
    assert!(first.dials(None, behind));
    first.end(root);
    let mut next = Round::begin_at(root, "fleet", away(), 1_060_000);
    assert!(!next.full(), "the next round on it is paced");
    assert!(!next.dials(None, behind));
    assert!(next.dials(None, ip("172.20.10.5")));
    assert_eq!(next.passed(), 1);
    next.end(root);
    assert!(
        Round::begin_at(root, "vault", away(), 1_060_000).full(),
        "another lane's own memory"
    );
    assert!(
        Round::begin_at(Path::new("/nonexistent/other"), "fleet", away(), 1_060_000).full(),
        "another state root's own memory"
    );
    let home = Routes::of(crate::sample::home(1_100_000, 3), &Evidence::default(), via);
    assert!(
        Round::begin_at(root, "fleet", home, 1_100_000).full(),
        "at home"
    );
    assert!(
        Round::begin_at(root, "fleet", away(), 1_120_000).full(),
        "back on the hotspot after home: another network"
    );
}

/// The internet-path seam engages only away with the internet answering.
#[test]
fn the_internet_path_is_wanted_away_with_the_internet_answering() {
    let mut place = hotspot();
    assert!(!Routes::of(place.clone(), &Evidence::default(), via).wants_internet_path());
    place.internet.state = InternetState::Ok;
    assert!(Routes::of(place, &Evidence::default(), via).wants_internet_path());
}
