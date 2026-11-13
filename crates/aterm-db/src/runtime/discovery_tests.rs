// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// What a round leaves when nothing accepted its connection ([`Repair::disowned`]).
fn nothing_there(mut repair: Repair) -> Repair {
    repair.disown();
    repair
}
#[test]
fn needs_update_waits_for_an_advertised_version_change() {
    use aterm_types::network_state::RepairCause;
    let mut hints = hints();
    let address = SocketAddr::from(([10, 1, 1, 1], 1234));
    let now = Instant::now();
    let old = BTreeMap::from([([7; 48], (address.ip(), "0.153.0".to_owned()))]);
    hints.advertised_versions(old.clone(), now);
    hints.observe(&wire(address.port()), address, now);
    let (_, mut repair) = hints.next(now).unwrap();
    repair.application_failed(crate::Error::repair(
        "old protocol",
        RepairCause::NeedsUpdate,
    ));
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        now,
    );
    // A day passing, pushes and repeated advertisements cannot redial an incompatible build.
    let later = now + Duration::from_secs(86_400);
    hints.hurry(later);
    hints.observe(&wire(address.port()), address, later);
    hints.advertised_versions(old, later);
    assert!(hints.next(later).is_none());
    hints.observe(&wire(address.port() + 1), address, later);
    assert!(
        hints.next(later).is_none(),
        "a listener port change is no build change"
    );
    hints.advertised_versions(BTreeMap::new(), later);
    hints.observe(&wire(address.port()), address, later);
    assert!(hints.next(later).is_none(), "missing metadata is no update");
    hints.advertised_versions(
        BTreeMap::from([([7; 48], (address.ip(), "0.155.0".to_owned()))]),
        later,
    );
    hints.observe(&wire(address.port()), address, later);
    assert_eq!(hints.next(later).unwrap().0, address);
}
#[test]
fn an_incompatible_seats_hold_does_not_follow_an_ip_to_another_seat() {
    use aterm_types::network_state::RepairCause;
    let mut hints = hints();
    let address = SocketAddr::from(([10, 1, 1, 1], 1234));
    let now = Instant::now();
    hints.advertised_versions(
        BTreeMap::from([([7; 48], (address.ip(), "0.153.0".into()))]),
        now,
    );
    hints.observe(&wire(address.port()), address, now);
    let (_, mut repair) = hints.next(now).unwrap();
    repair.application_failed(crate::Error::repair(
        "old protocol",
        RepairCause::NeedsUpdate,
    ));
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        now,
    );
    assert!(hints.next(now).is_none());
    hints.advertised_versions(
        BTreeMap::from([([8; 48], (address.ip(), "0.153.0".into()))]),
        now,
    );
    hints.observe(&wire(address.port()), address, now);
    assert_eq!(hints.next(now).unwrap().0, address);
}
/// A seat that speaks `Bus` is the mesh's: discovery never dials it and a push neither
/// hurries nor counts it, while it stays known, and it is dialled again once no longer named.
/// Another seat is dialled and hurried as before. NEGATIVE CONTROL: with no `Bus` speaker the
/// same peer is dialled and hurried.
#[test]
fn a_bus_speaker_is_never_dialled_or_hurried_but_stays_known() {
    for speaks in [true, false] {
        let mut hints = hints();
        let now = Instant::now();
        let [mesh, old]: [SocketAddr; 2] =
            ["10.1.1.1:1234", "10.1.1.2:1234"].map(|address| address.parse().unwrap());
        hints.advertised_versions(
            BTreeMap::from([
                ([7; 48], (mesh.ip(), "0.160.0".to_owned())),
                ([8; 48], (old.ip(), "0.153.0".to_owned())),
            ]),
            now,
        );
        for address in [mesh, old] {
            hints.observe(&wire(address.port()), address, now);
            hints.peers.get_mut(&address).unwrap().verified = true;
        }
        if speaks {
            hints.bus_speakers(BTreeSet::from([[7; 48]]));
        }
        let mut dialled = BTreeSet::new();
        while let Some((address, _)) = hints.next(now) {
            dialled.insert(address);
            hints.peers.get_mut(&address).unwrap().busy = false;
            hints.peers.get_mut(&address).unwrap().next = now + Duration::from_secs(30);
        }
        let hurried = hints.hurry(now);
        if speaks {
            assert_eq!(dialled, BTreeSet::from([old]), "a Bus speaker was dialled");
            assert_eq!(hurried, 1, "a push counted a Bus speaker");
            assert_eq!(hints.next(now).map(|(address, _)| address), Some(old));
            assert!(hints.next(now).is_none(), "a push dialled a Bus speaker");
            assert_eq!(hints.count(), 2, "a Bus speaker was forgotten");
            // Named no longer, it is dialled again.
            hints.bus_speakers(BTreeSet::new());
            assert_eq!(hints.next(now).map(|(address, _)| address), Some(mesh));
        } else {
            assert_eq!(dialled, BTreeSet::from([mesh, old]));
            assert_eq!(hurried, 2);
        }
    }
}
fn hints() -> Hints {
    Hints::new([4; 32])
}

/// A crowd of unverified LAN beacons, as every aterm on the ALab LAN sends them, each sorting
/// ahead of `10.1.9.0/24` ("B held 70 to 74", E1b).
fn crowd(hints: &mut Hints, now: Instant) -> SocketAddr {
    for n in 1..=70 {
        hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1)), now);
    }
    SocketAddr::from(([10, 1, 1, 1], 1234))
}

/// CR1: a peer that restarted beacons a new port from the address it was verified at, and the
/// peer that stayed up dials that port before a crowd of unverified hints that sort ahead of
/// it, at once. A stranger's new port gets no such lead, nor does an endpoint whose dial failed
/// without authenticating anyone, and the ordinary scan keeps its place.
///
/// NEGATIVE CONTROL (run here): with no candidate leading ([`UNLED`]), the old address order
/// dials the crowd first.
#[test]
fn a_verified_peers_new_port_is_dialled_before_the_crowd() {
    for unled in [false, true] {
        UNLED.with(|cell| cell.set(unled));
        let mut hints = hints();
        let now = Instant::now();
        let old: SocketAddr = "10.1.9.9:1000".parse().unwrap();
        hints.observe(&wire(old.port()), old, now);
        // It was verified; its listener went with the restart, and the dials since failed.
        let peer = hints.peers.get_mut(&old).unwrap();
        peer.verified = true;
        peer.failed = true;
        peer.failures = 4;
        peer.next = now + BACKOFF_MAX;
        let first_of_crowd = crowd(&mut hints, now);
        let stranger: SocketAddr = "10.1.9.7:2000".parse().unwrap();
        hints.observe(&wire(stranger.port()), stranger, now);
        let new: SocketAddr = "10.1.9.9:2000".parse().unwrap();
        hints.observe(&wire(new.port()), new, now);
        let (first, repair) = hints.next(now).unwrap();
        if unled {
            assert_eq!(first, first_of_crowd, "the old order dials the crowd first");
            continue;
        }
        assert_eq!(first, new, "the verified peer's new port goes first");
        assert_eq!(hints.peers[&stranger].lead(), Lead::None);
        // The ordinary scan did not move: the crowd's first is next.
        let (second, _) = hints.next(now).unwrap();
        assert_eq!(second, first_of_crowd);
        // Nothing listening there loses the lead.
        hints.finish(first, nothing_there(repair), Outcome::Refused, now);
        assert_eq!(hints.peers[&new].lead(), Lead::None);
        let again = hints.peers[&new].next;
        assert_ne!(
            hints.next(again).map(|(address, _)| address),
            Some(new),
            "a failed lead waits its turn behind the crowd"
        );
    }
    UNLED.with(|cell| cell.set(false));
}

/// CR1: what a Mac verified before it restarted is dialled first after: a remembered endpoint
/// that beacons again leads as kin, then one not yet heard, both before the crowd; one older
/// than [`REMEMBER_MS`] is forgotten, and only the most recent [`MAX_REMEMBERED`] seats are
/// kept. Their file round-trips in one spelling, and any other spelling reads as none.
///
/// NEGATIVE CONTROL (run here): with no candidate leading ([`UNLED`]), the old address order
/// dials the crowd first after a restart.
#[test]
fn a_remembered_endpoint_is_dialled_first_after_a_restart() {
    let (heard, unheard, stale): (SocketAddr, SocketAddr, SocketAddr) = (
        "10.1.9.9:1000".parse().unwrap(),
        "10.1.9.8:1000".parse().unwrap(),
        "10.1.9.6:1000".parse().unwrap(),
    );
    let day_ms = 24 * 60 * 60 * 1000;
    let wall_ms = 30 * day_ms;
    let mut before = hints();
    let t0 = Instant::now() + Duration::from_millis(REMEMBER_MS + 2 * day_ms);
    before.keep([7; 48], heard, t0);
    before.keep([8; 48], unheard, t0 - Duration::from_secs(60));
    before.keep(
        [9; 48],
        stale,
        t0 - Duration::from_millis(REMEMBER_MS + day_ms),
    );
    let kept = before
        .remembered(t0, wall_ms)
        .expect("new seats are a change");
    assert!(
        before.remembered(t0, wall_ms).is_none(),
        "nothing changed since the last read"
    );
    before.keep([7; 48], heard, t0 + Duration::from_secs(5));
    assert!(
        before
            .remembered(t0 + Duration::from_secs(5), wall_ms)
            .is_none(),
        "a later round with the same endpoint is no change"
    );
    assert!(
        before.remembered(t0 + REMEMBER_REFRESH, wall_ms).is_some(),
        "kept times are refreshed once an hour"
    );
    let scope: MemoryScope = [3; std::mem::size_of::<MemoryScope>()];
    let bytes = Remembered::encode(&scope, &kept);
    assert!(bytes.len() <= REMEMBERED_MAX);
    assert_eq!(
        Remembered::decode(&scope, &bytes).as_deref(),
        Some(&kept[..])
    );
    let mut another = scope;
    another[0] = 4;
    assert_eq!(
        Remembered::decode(&another, &bytes),
        None,
        "another trust or seat reads none"
    );
    for damaged in [
        &bytes[..bytes.len() - 1],
        &[bytes.as_slice(), &[0]].concat()[..],
        &[b"ATPE0".as_slice(), &bytes[5..]].concat()[..],
    ] {
        assert_eq!(Remembered::decode(&scope, damaged), None);
    }
    let mut unsorted = bytes.clone();
    let entries = REMEMBERED_MAGIC.len() + scope.len() + 1;
    let (first, rest) = unsorted[entries..].split_at_mut(REMEMBERED_ENTRY);
    first.swap_with_slice(&mut rest[..REMEMBERED_ENTRY]);
    assert_eq!(
        Remembered::decode(&scope, &unsorted),
        None,
        "one spelling: sorted seats"
    );

    for unled in [false, true] {
        UNLED.with(|cell| cell.set(unled));
        let mut after = Hints::new([9; 32]);
        let now = Instant::now();
        let first_of_crowd = crowd(&mut after, now);
        after.remember(
            Remembered::decode(&scope, &bytes).unwrap(),
            now,
            wall_ms + 60_000,
        );
        assert!(
            !after.peers.contains_key(&stale),
            "a week-old endpoint is forgotten"
        );
        // One remembered peer is up again and beacons the port it was verified on.
        after.observe(&wire(heard.port()), heard, now);
        let (first, _) = after.next(now).unwrap();
        if unled {
            assert_eq!(first, first_of_crowd, "the old order dials the crowd first");
            continue;
        }
        let (second, repair) = after.next(now).unwrap();
        assert_eq!((first, second), (heard, unheard));
        assert_eq!(after.next(now).unwrap().0, first_of_crowd);
        after.finish(unheard, nothing_there(repair), Outcome::Refused, now);
        assert_eq!(after.peers[&unheard].lead(), Lead::None);
        // What it remembers stays to be kept again, though nothing was verified since.
        let again = after.remembered(now, wall_ms + 60_000).unwrap();
        assert_eq!(again.len(), 2);
    }
    UNLED.with(|cell| cell.set(false));

    let mut many = hints();
    let now = Instant::now() + Duration::from_secs(3600);
    for n in 0..=MAX_REMEMBERED as u8 {
        let address = SocketAddr::from(([10, 1, 3, n + 1], 1000));
        many.keep([n; 48], address, now - Duration::from_secs(u64::from(n)));
    }
    let kept = many.remembered(now, wall_ms).unwrap();
    assert_eq!(kept.len(), MAX_REMEMBERED);
    assert!(
        kept.iter()
            .all(|entry| entry.actor != [MAX_REMEMBERED as u8; 48]),
        "the oldest seat is the one forgotten"
    );
}
/// CR1: beacons forged from a peer's address, a new port every few hundred ms, lead one
/// dial per round the peer makes from there: a led dial that authenticates nobody spends the
/// address's lead until a peer authenticates from it again, and the crowd keeps its turns.
///
/// NEGATIVE CONTROL (run here, [`UNSPENT`]): every forged port leads, ahead of the crowd.
#[test]
fn forged_ports_from_a_peers_address_lead_one_dial_per_round_the_peer_makes() {
    for unspent in [false, true] {
        let mut hints = hints();
        let now = Instant::now();
        let peer: SocketAddr = "10.1.9.9:1000".parse().unwrap();
        hints.observe(&wire(peer.port()), peer, now);
        let established = hints.peers.get_mut(&peer).unwrap();
        established.verified = true;
        established.next = now + BACKOFF_MAX;
        let first_of_crowd = crowd(&mut hints, now);
        let forged = |port: u16| SocketAddr::from(([10, 1, 9, 9], port));
        hints.observe(&wire(2000), forged(2000), now);
        let (first, repair) = hints.next(now).unwrap();
        assert_eq!(
            first,
            forged(2000),
            "a new port from a peer's address leads"
        );
        UNSPENT.with(|cell| cell.set(unspent));
        hints.finish(first, nothing_there(repair), Outcome::Refused, now);
        UNSPENT.with(|cell| cell.set(false));
        hints.observe(&wire(2001), forged(2001), now);
        let (next, _) = hints.next(now).unwrap();
        if unspent {
            assert_eq!(next, forged(2001), "MUTANT: every forged port leads");
            continue;
        }
        assert_eq!(
            next, first_of_crowd,
            "a spent address's new port waits its turn"
        );
        // The peer dials in from its address: one new port there leads again.
        hints.vouch(peer.ip(), [7; 48], false, now);
        assert_eq!(hints.peers[&forged(2001)].lead(), Lead::Kin);
    }
}

/// CR1: a seat on two attachments (Wi-Fi and Ethernet) completes rounds on both endpoints in
/// turn; the one remembered stays while it answers, so the file is not rewritten each round,
/// and the other takes its place once it stops answering.
///
/// NEGATIVE CONTROL (run here, [`FLAPS`]): each round's endpoint replaces the last.
#[test]
fn a_seat_on_two_attachments_keeps_one_remembered_endpoint_while_it_answers() {
    for flaps in [false, true] {
        FLAPS.with(|cell| cell.set(flaps));
        let mut hints = hints();
        let now = Instant::now();
        let wifi: SocketAddr = "10.1.9.9:1000".parse().unwrap();
        let wired: SocketAddr = "10.1.8.9:1000".parse().unwrap();
        for address in [wifi, wired] {
            hints.observe(&wire(address.port()), address, now);
            let peer = hints.peers.get_mut(&address).unwrap();
            peer.verified = true;
        }
        hints.keep([7; 48], wifi, now);
        assert!(hints.remembered(now, 0).is_some(), "a new seat is a change");
        let mut writes = 0;
        for round in 1..=4 {
            let address = if round % 2 == 0 { wifi } else { wired };
            hints.keep([7; 48], address, now + Duration::from_secs(round));
            writes += usize::from(
                hints
                    .remembered(now + Duration::from_secs(round), 0)
                    .is_some(),
            );
        }
        FLAPS.with(|cell| cell.set(false));
        if flaps {
            assert_eq!(writes, 4, "MUTANT: every round rewrites the file");
            continue;
        }
        assert_eq!(writes, 0, "rounds on either attachment change nothing kept");
        hints.peers.get_mut(&wifi).unwrap().failed = true;
        hints.keep([7; 48], wired, now + Duration::from_secs(5));
        let kept = hints.remembered(now + Duration::from_secs(5), 0).unwrap();
        assert_eq!(
            kept[0].address, wired,
            "the endpoint that answers replaces one that stopped"
        );
    }
}
fn wire(port: u16) -> Vec<u8> {
    Hints::new([5; 32]).wire(port).to_vec()
}
#[test]
fn untrusted_hints_have_size_source_and_capacity_bounds() {
    let mut hints = hints();
    let now = Instant::now();
    let mut bad = wire(1234);
    bad.push(0);
    hints.observe(&bad, "10.1.1.2:1".parse().unwrap(), now);
    hints.observe(&hints.wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    assert_eq!(hints.count(), 0);
    for n in 0..MAX_PEERS + 36 {
        let ip = [10, 1, 2 + (n / 200) as u8, 1 + (n % 200) as u8];
        hints.observe(&wire(1234), SocketAddr::from((ip, 999)), now);
    }
    assert_eq!(hints.count(), MAX_PEERS);
    const { assert!(MAX_PEERS > aterm_secrets::commons::roster::MAX_MEMBERS) };
    // Every queued candidate gets a turn even when a slow handshake would
    // put it past the failed-candidate expiry before it was tried.
    hints.expire(now + EXPIRES);
    assert_eq!(hints.count(), MAX_PEERS);
    while let Some((address, repair)) = hints.next(now) {
        hints.finish(address, repair, Outcome::Refused, now);
    }
    hints.expire(now + EXPIRES);
    assert_eq!(hints.count(), 0);
}

#[test]
fn producer_hint_capacity_remains_readable_over_the_local_observer_relay() {
    let mut hints = hints();
    let now = Instant::now();
    for index in 0..MAX_PEERS {
        let index = u32::try_from(index).unwrap();
        let mut token = [5; 32];
        token[..4].copy_from_slice(&index.to_be_bytes());
        let remote = Hints::new(token);
        hints.observe(
            &remote.wire(1234),
            SocketAddr::from((std::net::Ipv4Addr::from(0x0a01_0001 + index), 1)),
            now,
        );
        hints.observe_presence(
            &remote.presence_wire(),
            SocketAddr::from((std::net::Ipv4Addr::from(0x0a02_0001 + index), 1)),
            now,
        );
    }
    let count = hints.unverified_count();
    assert_eq!(count, MAX_UNVERIFIED_ENDPOINTS);
    let root = aterm_tempfile::Builder::new()
        .prefix("hint-relay-")
        .tempdir_in("/tmp")
        .unwrap();
    let config = root.path().join("aterm.toml");
    let handle = crate::network_state::ObservationHandle::default();
    let mut owner = crate::network_state::Observer::open(&config, handle.clone()).unwrap();
    owner.observe(
        aterm_types::network_state::Visibility::Active,
        0,
        "discovery hints",
        aterm_time::unix_millis().unwrap(),
        Instant::now(),
    );
    let control = crate::network_state::read_live_observation(&config)
        .expect("the same owner must serve a zero-count control");
    assert_eq!(control.via_local_process, Some(std::process::id()));
    assert_eq!(control.unverified_endpoints, 0);
    owner.observe(
        aterm_types::network_state::Visibility::Active,
        count,
        "discovery hints",
        aterm_time::unix_millis().unwrap(),
        Instant::now(),
    );
    let relayed = crate::network_state::read_live_observation(&config)
        .expect("a producer's full hint count must remain readable");
    assert_eq!(relayed.via_local_process, Some(std::process::id()));
    assert_eq!(relayed.unverified_endpoints, count);
    assert!(
        relayed.peers.is_empty(),
        "hints manufactured trusted machines"
    );
    drop(owner);
}
#[test]
fn reserved_turns_preserve_the_ordinary_discovery_position() {
    let mut hints = hints();
    let now = Instant::now();
    let addresses: Vec<_> = (1..=4)
        .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
        .collect();
    for address in &addresses {
        hints.observe(&wire(address.port()), *address, now);
    }
    for address in &addresses[2..] {
        hints.peers.get_mut(address).unwrap().verified = true;
    }
    let (first, repair) = hints.next(now).unwrap();
    assert_eq!(first, addresses[0]);
    let (third, _) = hints.next_allowed(now, false).unwrap();
    let (fourth, _) = hints.next_allowed(now, false).unwrap();
    assert_eq!((third, fourth), (addresses[2], addresses[3]));
    hints.finish(first, repair, Outcome::Refused, now);
    let (next, _) = hints.next(now + Duration::from_secs(10)).unwrap();
    assert_eq!(
        next, addresses[1],
        "reserved established turns must not keep returning discovery to its first candidate"
    );
}

#[test]
fn failed_established_peers_release_their_reserved_capacity() {
    let mut hints = hints();
    let now = Instant::now();
    let address = "10.1.1.2:1234".parse().unwrap();
    hints.observe(&wire(1234), address, now);
    assert!(!hints.has_established());
    assert!(hints.next_allowed(now, false).is_none());
    hints.peers.get_mut(&address).unwrap().verified = true;
    assert!(hints.has_established());
    let (selected, repair) = hints.next_allowed(now, false).unwrap();
    assert_eq!(selected, address);
    hints.finish(selected, repair, Outcome::Refused, now);
    hints.observe(&wire(1234), address, now);
    assert!(!hints.has_established());
    assert!(
        hints
            .next_allowed(now + Duration::from_secs(10), false)
            .is_none()
    );
    assert_eq!(
        hints.next(now + Duration::from_secs(10)).unwrap().0,
        address
    );
}

#[test]
fn beacons_cannot_reset_backoff_or_duplicate_an_active_dial() {
    let mut hints = hints();
    let now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    let (address, repair) = hints.next(now).unwrap();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    assert!(hints.next(now + Duration::from_secs(10)).is_none());
    hints.finish(address, repair, Outcome::Refused, now);
    assert!(hints.next(now).is_none());
    assert!(hints.next(now + Duration::from_secs(10)).is_some());
    for port in [2000_u16, 2001, 2002] {
        hints.observe(&wire(port), "10.1.1.2:1".parse().unwrap(), now);
    }
    assert_eq!(hints.count(), PER_IP);
}

/// A push dials every verified peer at once, and one whose round runs as it begins again as
/// that round ends; an unverified candidate keeps its own turn. A first quiet success gets
/// an immediate confirmation while the push runs; once calm, rounds keep their quiet
/// interval again.
///
/// NEGATIVE CONTROL: make `Hints::hurry` count the verified peers without moving their next
/// dial, and nothing is due at once.
#[test]
fn a_push_dials_every_verified_peer_at_once_and_a_busy_one_as_its_round_ends() {
    let mut hints = hints();
    let now = Instant::now();
    for source in ["10.1.1.2:1", "10.1.1.3:1", "10.1.1.4:1"] {
        hints.observe(&wire(1234), source.parse().unwrap(), now);
    }
    let [idle, busy, cold]: [SocketAddr; 3] =
        ["10.1.1.2:1234", "10.1.1.3:1234", "10.1.1.4:1234"].map(|address| address.parse().unwrap());
    for (address, verified, running) in [
        (idle, true, false),
        (busy, true, true),
        (cold, false, false),
    ] {
        let peer = hints.peers.get_mut(&address).unwrap();
        peer.verified = verified;
        peer.busy = running;
        peer.next = now + Duration::from_secs(30);
    }
    assert!(hints.next(now).is_none());

    assert_eq!(hints.hurry(now), 2, "the verified peers, busy or not");
    let (first, repair) = hints
        .next(now)
        .expect("the idle verified peer is due at once");
    assert_eq!(first, idle);
    assert!(
        hints.next(now).is_none(),
        "the busy one waits for its round; the cold one for its turn"
    );

    // The round that ran as the push began ends, failed: its peer is dialled again at once.
    let ended = now + Duration::from_secs(1);
    hints.finish(busy, Repair::default(), Outcome::Busy, ended);
    assert_eq!(hints.next(ended).map(|(address, _)| address), Some(busy));

    // While the push runs, a round that succeeded is followed at once.
    hints.finish(
        first,
        repair,
        Outcome::Repaired { transferred: false },
        ended,
    );
    let (again, repair) = hints.next(ended).expect("followed at once");
    assert_eq!(again, idle);

    // Calm, a success keeps its quiet interval.
    hints.calm();
    hints.finish(
        again,
        repair,
        Outcome::Repaired { transferred: false },
        ended,
    );
    assert!(hints.next(ended).is_none());
}

/// C3: a local append makes every established endpoint due at once, with the clock not
/// advanced. One whose round runs is dialled again within the follow-up interval of that
/// round's end (200 to 500 ms, as after a transfer) if it repairs, and not at once: appends
/// that keep coming leave each round its wait. A failed round keeps its backoff and a table
/// that stays deferred its ordinary interval. An unverified endpoint and a failing one keep
/// their own schedule. Only `next` moves, so the dials still wait for the transport's places
/// and spacing.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::announce`] leaving each endpoint's next dial
/// where it was, none was due (`the three established endpoints at rest are due at once`:
/// `left: {}`, `right: {10.1.1.2:1234, 10.1.1.3:1234, 10.1.1.4:1234}`).
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::finish`] making a told endpoint due at its
/// round's end whenever the round completed, as it did, the endpoint was due with no wait
/// (`dialled again within the follow-up interval of its round's end: 0ns after it`).
#[test]
fn a_local_append_makes_every_established_endpoint_due_at_once() {
    let mut hints = hints();
    let now = Instant::now();
    let at = |n: u8| SocketAddr::from(([10, 1, 1, n], 1234));
    for (n, verified, busy, failed) in [
        (2, true, false, false),
        (3, true, false, false),
        (4, true, false, false),
        (5, true, true, false),
        (6, true, false, true),
        (7, false, false, false),
    ] {
        hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1)), now);
        let peer = hints.peers.get_mut(&at(n)).unwrap();
        (peer.verified, peer.busy, peer.failed) = (verified, busy, failed);
        peer.next = now + Duration::from_secs(30);
    }
    assert!(hints.next(now).is_none(), "none is due before the append");

    assert_eq!(
        hints.announce(now),
        4,
        "the established endpoints, in a round or at rest"
    );
    let due: BTreeSet<_> = std::iter::from_fn(|| hints.next(now))
        .map(|(address, _)| address)
        .collect();
    assert_eq!(
        due,
        BTreeSet::from([at(2), at(3), at(4)]),
        "the three established endpoints at rest are due at once"
    );

    // The round that ran as the append came repairs: its endpoint is dialled again within the
    // follow-up interval of its end, and not at once.
    let quiet = Outcome::Repaired { transferred: false };
    let follows = |ended: Instant, next: Instant| {
        (ended + Duration::from_millis(200)..ended + Duration::from_millis(500)).contains(&next)
    };
    let ended = now + Duration::from_secs(1);
    hints.finish(at(5), Repair::default(), quiet, ended);
    let again = hints.next_at(at(5));
    assert!(
        follows(ended, again),
        "dialled again within the follow-up interval of its round's end: {:?} after it",
        again.saturating_duration_since(ended)
    );
    assert!(hints.next(ended).is_none(), "and not at once");
    assert_eq!(hints.next(again).map(|(address, _)| address), Some(at(5)));

    // A second append while the four rounds run. One of them fails and keeps its backoff; one
    // defers its table again and keeps the ordinary interval; one repairs and is dialled again
    // within the follow-up interval, once.
    assert_eq!(hints.announce(again), 4);
    let ordinary = again + Duration::from_secs(3);
    hints.finish(at(2), Repair::default(), Outcome::Busy, again);
    assert!(
        hints.next_at(at(2)) >= ordinary,
        "a failed round keeps its wait"
    );
    hints.peers.get_mut(&at(4)).unwrap().deferring = true;
    hints.finish(at(4), Repair::default(), Outcome::Deferred, again);
    assert!(
        hints.next_at(at(4)) >= ordinary,
        "a table that stays deferred keeps the ordinary interval"
    );
    hints.finish(at(3), Repair::default(), quiet, again);
    let third = hints.next_at(at(3));
    assert!(follows(again, third), "the round that repaired");
    assert_eq!(hints.next(third).map(|(address, _)| address), Some(at(3)));
    hints.finish(at(3), Repair::default(), quiet, third);
    assert!(
        hints.next_at(at(3)) >= third + Duration::from_secs(3),
        "one append, one redial"
    );
}

/// A real round whose table was deferred and moved nothing is [`Outcome::Deferred`], not a
/// repair: its endpoint stays established with no backoff, is dialled again after the short
/// follow-up delay, then at the quiet interval while it keeps deferring, and never joins the
/// follow-up queue that productive rounds share. MUTANT (`DEFERRAL_SUCCEEDS`, as before
/// CR1): the same round reads as a quiet repair.
#[test]
fn a_deferred_round_is_no_success_and_is_retried_soon_once() {
    let repair = crate::replica::tests::witness::deferred_repair();
    assert_eq!(Outcome::of(true, &repair, None), Outcome::Deferred);
    DEFERRAL_SUCCEEDS.with(|flag| flag.set(true));
    let mutant = Outcome::of(true, &repair, None);
    DEFERRAL_SUCCEEDS.with(|flag| flag.set(false));
    assert_eq!(mutant, Outcome::Repaired { transferred: false });

    let mut hints = hints();
    let now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    let (address, _) = hints.next(now).unwrap();
    hints.finish(address, repair.clone(), Outcome::Deferred, now);
    let peer = &hints.peers[&address];
    assert!(
        peer.established(),
        "the peer authenticated and completed control"
    );
    assert_eq!(peer.failures, 0);
    assert!(
        peer.next <= now + Duration::from_millis(500),
        "soon after a first deferral"
    );
    assert!(!hints.follow_up.contains(&address));
    let again = hints.peers[&address].next;
    let (next, _) = hints.next(again).unwrap();
    assert_eq!(next, address);
    hints.finish(address, repair.clone(), Outcome::Deferred, again);
    assert!(
        hints.peers[&address].next >= again + Duration::from_secs(3),
        "a table that stays deferred waits the quiet interval"
    );
    let later = hints.peers[&address].next;
    let _ = hints.next(later).unwrap();
    hints.finish(
        address,
        repair.clone(),
        Outcome::Repaired { transferred: false },
        later,
    );
    hints.peers.get_mut(&address).unwrap().next = later;
    let _ = hints.next(later).unwrap();
    hints.finish(address, repair, Outcome::Deferred, later);
    assert!(
        hints.peers[&address].next <= later + Duration::from_millis(500),
        "a repair in between makes the next deferral a first one"
    );
}

#[test]
fn a_quiet_push_bounds_followups_but_a_transferring_push_stays_fast() {
    let mut counts = Vec::new();
    for transferred in [false, true] {
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        hints
            .peers
            .values_mut()
            .for_each(|peer| peer.verified = true);
        assert_eq!(hints.hurry(now), 1);
        let mut dials = 0;
        // Ten virtual seconds of ticks 100 ms apart, the closest the runtime's TICK floor
        // lets two ticks come (its worst case). The peer keeps
        // answering but, in the quiet case, cannot take the pushed table.
        for tick in 0..100 {
            let at = now + Duration::from_millis(100 * tick);
            if let Some((address, repair)) = hints.next(at) {
                hints.finish(address, repair, Outcome::Repaired { transferred }, at);
                dials += 1;
            }
        }
        counts.push(dials);
    }
    println!(
        "ten virtual seconds: quiet={}, transferring={}",
        counts[0], counts[1]
    );
    assert!(counts[0] <= 5, "a quiet push kept redialing: {counts:?}");
    assert_eq!(counts[1], 100, "productive pushes keep making progress");
}

#[test]
fn quiet_push_confirmation_starts_after_the_old_round_and_rearms_on_progress() {
    let mut hints = hints();
    let mut now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    hints
        .peers
        .values_mut()
        .for_each(|peer| peer.verified = true);
    let (address, old_round) = hints.next(now).unwrap();
    hints.hurry(now);
    hints.finish(
        address,
        old_round,
        Outcome::Repaired { transferred: false },
        now,
    );
    for _ in 0..2 {
        let (next, repair) = hints
            .next(now)
            .expect("the post-push round and confirmation");
        assert_eq!(next, address);
        hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
    }
    assert!(hints.next(now).is_none(), "quiet confirmation is bounded");
    now = hints.peers[&address].next;
    let (next, repair) = hints.next(now).unwrap();
    hints.finish(next, repair, Outcome::Repaired { transferred: true }, now);
    for _ in 0..2 {
        let (next, repair) = hints.next(now).expect("progress re-arms confirmation");
        hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
    }
    assert!(hints.next(now).is_none());
    hints.hurry(now);
    for _ in 0..2 {
        let (next, repair) = hints.next(now).expect("a new push has a new confirmation");
        hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
    }
    assert!(hints.next(now).is_none());
}

#[test]
fn a_slow_round_still_has_a_quiet_interval_after_completion() {
    let mut hints = hints();
    let now = Instant::now();
    let source = "10.1.1.2:1".parse().unwrap();
    hints.observe(&wire(1234), source, now);
    let (address, repair) = hints.next(now).unwrap();
    let finished = now + Duration::from_secs(30);
    hints.finish(address, repair, Outcome::Refused, finished);
    hints.observe(&wire(1234), source, finished);
    assert!(hints.next(finished).is_none());
    assert!(hints.next(finished + Duration::from_secs(2)).is_none());
    assert!(hints.next(finished + Duration::from_secs(10)).is_some());
}

#[test]
fn a_productive_peer_keeps_capacity_during_its_short_wait() {
    let mut hints = hints();
    let now = Instant::now();
    let addresses: Vec<SocketAddr> = (1..=3)
        .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
        .collect();
    for address in &addresses {
        hints.observe(&wire(address.port()), *address, now);
    }
    // One slow dial remains busy while the other outgoing slot completes
    // a productive round. The third peer must not take that last slot.
    let (slow, _) = hints.next(now).unwrap();
    assert_eq!(slow, addresses[0]);
    let (productive, repair) = hints.next(now).unwrap();
    assert_eq!(productive, addresses[1]);
    hints.finish(
        productive,
        repair,
        Outcome::Repaired { transferred: true },
        now,
    );
    let due = hints.peers[&productive].next;
    assert!(hints.next(now).is_none());
    assert!(hints.next(now + (due - now) / 2).is_none());
    let (address, repair) = hints.next(due).unwrap();
    assert_eq!(address, productive);
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        due,
    );
    assert_eq!(hints.next(due).unwrap().0, addresses[2]);
}

#[test]
fn a_ready_follow_up_can_pass_a_waiting_peer_without_losing_either_turn() {
    // This reproducible token makes a fast repeated exchange ready before
    // a peer that finished earlier. Use the production retry jitter.
    let mut hints = Hints::new([17; 32]);
    let now = Instant::now();
    let addresses: Vec<SocketAddr> = (1..=3)
        .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
        .collect();
    for address in &addresses {
        hints.observe(&wire(address.port()), *address, now);
    }
    let (first, first_repair) = hints.next(now).unwrap();
    let (second, second_repair) = hints.next(now).unwrap();
    hints.finish(
        first,
        first_repair,
        Outcome::Repaired { transferred: true },
        now,
    );
    assert!(hints.next(now).is_none());
    let resume = now + Duration::from_millis(500);
    hints.finish(
        second,
        second_repair,
        Outcome::Repaired { transferred: true },
        resume,
    );
    let (address, repair) = hints.next(resume).unwrap();
    assert_eq!(address, first);
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: true },
        resume + Duration::from_millis(100),
    );
    // Both writers completed through the normal path. On ticks 100 ms apart (the
    // TICK floor), the later queued retry becomes eligible one tick earlier.
    assert!(hints.next(resume + Duration::from_millis(300)).is_none());
    let earlier = resume + Duration::from_millis(400);
    let later = resume + Duration::from_millis(500);
    let (address, repair) = hints
        .next(earlier)
        .expect("a ready queued peer must not wait for another peer's deadline");
    assert_eq!(address, first);
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        earlier,
    );
    assert!(
        hints.next(earlier).is_none(),
        "ordinary discovery must still reserve the waiting peer's capacity"
    );
    let (address, repair) = hints.next(later).unwrap();
    assert_eq!(address, second, "the waiting peer kept its queued turn");
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        later,
    );
    assert_eq!(hints.next(later).unwrap().0, addresses[2]);
}

#[test]
fn progress_gets_a_bounded_burst_without_starving_the_ordinary_scan() {
    let mut hints = hints();
    let mut at = Instant::now();
    let addresses: Vec<SocketAddr> = (1..=4)
        .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
        .collect();
    for address in &addresses {
        hints.observe(&wire(address.port()), *address, at);
    }
    let (first, repair) = hints.next(at).unwrap();
    assert_eq!(first, addresses[0]);
    hints.finish(first, repair, Outcome::Repaired { transferred: true }, at);
    for expected in &addresses[1..] {
        for _ in 0..FOLLOW_UP_BURST {
            at = hints.peers[&first].next;
            let (address, repair) = hints.next(at).unwrap();
            assert_eq!(address, first, "completed data gets its follow-up");
            hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
        }
        let (address, repair) = hints.next(at).unwrap();
        assert_eq!(
            address, *expected,
            "every ordinary candidate still gets its turn"
        );
        hints.finish(address, repair, Outcome::Busy, at);
    }
}

#[test]
fn completed_peers_keep_their_follow_up_turns() {
    let mut hints = hints();
    let mut at = Instant::now();
    let addresses: Vec<SocketAddr> = (1..=4)
        .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
        .collect();
    for address in &addresses {
        hints.observe(&wire(address.port()), *address, at);
    }
    let (first, first_repair) = hints.next(at).unwrap();
    let (second, second_repair) = hints.next(at).unwrap();
    assert_eq!((first, second), (addresses[0], addresses[1]));
    for (address, repair) in [(first, first_repair), (second, second_repair)] {
        hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
    }
    for (preferred, ordinary) in [
        ([first, second, first], addresses[2]),
        ([second, first, second], addresses[3]),
    ] {
        for expected in preferred {
            at = hints.peers.values().map(|peer| peer.next).max().unwrap();
            let (address, repair) = hints.next(at).unwrap();
            assert_eq!(
                address, expected,
                "a later completion must not erase an earlier queued turn"
            );
            hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
        }
        let (address, repair) = hints.next(at).unwrap();
        assert_eq!(
            address, ordinary,
            "queued work must still yield to discovery"
        );
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            at,
        );
    }
}

#[test]
fn a_failed_queued_peer_preserves_another_peers_follow_up() {
    let mut hints = hints();
    let mut at = Instant::now();
    for n in 1..=2 {
        hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1234)), at);
    }
    let (first, first_repair) = hints.next(at).unwrap();
    let (second, second_repair) = hints.next(at).unwrap();
    for (address, repair) in [(first, first_repair), (second, second_repair)] {
        hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
    }
    at = hints.peers.values().map(|peer| peer.next).max().unwrap();
    let (address, repair) = hints.next(at).unwrap();
    assert_eq!(address, first);
    hints.finish(address, repair, Outcome::Refused, at);
    at = hints.peers[&second].next.max(at);
    let (address, repair) = hints.next(at).unwrap();
    assert_eq!(
        address, second,
        "one failure must not discard another peer's work"
    );
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        at,
    );
    assert!(hints.next(at).is_none());
}

#[test]
fn a_failed_follow_up_returns_to_ordinary_discovery() {
    let mut hints = hints();
    let mut at = Instant::now();
    for n in 1..=2 {
        hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1234)), at);
    }
    let (first, repair) = hints.next(at).unwrap();
    hints.finish(first, repair, Outcome::Repaired { transferred: true }, at);
    at = hints.peers[&first].next;
    let (address, repair) = hints.next(at).unwrap();
    assert_eq!(address, first);
    hints.finish(address, repair, Outcome::Refused, at);
    assert_ne!(hints.next(at).unwrap().0, first);
}

#[test]
fn failed_retries_do_not_keep_a_vanished_unverified_endpoint_alive() {
    let mut hints = hints();
    let now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    let mut tries = 0;
    for seconds in 0..45 {
        let tried = now + Duration::from_secs(seconds);
        if let Some((address, repair)) = hints.next(tried) {
            tries += 1;
            hints.finish(address, repair, Outcome::Refused, tried);
        }
    }
    // 0 s, then 5, 10 and 20 s (each plus up to 2 s) after each failure.
    assert!(tries >= 3, "the candidate kept {tries} turns");
    assert!(
        hints.next(now + Duration::from_secs(46)).is_none(),
        "connection failures kept an endpoint with no new announcement alive"
    );
    assert_eq!(hints.count(), 0);
}

#[test]
fn an_untried_candidate_gets_one_round_even_after_its_announcement_expires() {
    let mut hints = hints();
    let now = Instant::now();
    let source = "10.1.1.2:1".parse().unwrap();
    hints.observe(&wire(1234), source, now);
    let tried = now + Duration::from_secs(60);
    let (address, repair) = hints.next(tried).expect("untried candidate kept its turn");
    hints.expire(tried);
    assert_eq!(hints.count(), 1, "an active round must remain tracked");
    hints.finish(address, repair, Outcome::Refused, tried);
    assert!(hints.next(tried + Duration::from_secs(10)).is_none());

    // A new announcement grants a fresh attempt after the stale slot has expired.
    hints.observe(&wire(1234), source, tried + Duration::from_secs(11));
    assert!(hints.next(tried + Duration::from_secs(11)).is_some());
}

/// Dials of one verified endpoint in ten virtual minutes of ticks 100 ms apart (the closest
/// the runtime's TICK floor lets them come), each ending as `outcome` says for the dial's
/// index.
fn dials_in_ten_minutes(mut outcome: impl FnMut(usize) -> Outcome) -> usize {
    let mut hints = hints();
    let now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    hints
        .peers
        .values_mut()
        .for_each(|peer| peer.verified = true);
    let mut dials = 0;
    for tick in 0..6000 {
        let at = now + Duration::from_millis(100 * tick);
        if let Some((address, repair)) = hints.next(at) {
            hints.finish(address, repair, outcome(dials), at);
            dials += 1;
        }
    }
    dials
}

/// C4: two members with nothing to exchange run one round an interval where each ran its own.
/// Ten virtual minutes of ticks 100 ms apart; each holds the other's verified endpoint; a dial
/// completes in its tick as a round that moved nothing, and the dialled side hears a quiet round
/// dialled in ([`Hints::vouch`]). One round per 3 to 5 s is between 120 and 200 in all, where
/// two members each dialling alone make twice [`dials_in_ten_minutes`]. Both sides go on
/// dialling: neither becomes the pair's only dialler.
///
/// NEGATIVE CONTROL (2026-11-12): with a quiet round dialled in holding nothing back
/// (`Hints::vouch` not moving a resting endpoint's next dial), the pair made 301 rounds (`one
/// round an interval between the two: 301 rounds, [150, 151] by each side, 150 by a member
/// alone`).
#[test]
fn a_quiet_pair_runs_one_round_an_interval_between_the_two() {
    let quiet = Outcome::Repaired { transferred: false };
    let alone = dials_in_ten_minutes(|_| quiet);
    assert!(
        (120..=200).contains(&alone),
        "{alone} dials by one member alone"
    );
    let actors: [astream_wire::sync::Actor; 2] = [[1; 48], [2; 48]];
    let at: [SocketAddr; 2] = ["10.1.1.1:1234", "10.1.1.2:1234"].map(|a| a.parse().unwrap());
    let mut members = [Hints::new([4; 32]), Hints::new([6; 32])];
    let start = Instant::now();
    for (n, member) in members.iter_mut().enumerate() {
        member.observe(&wire(1234), SocketAddr::from((at[1 - n].ip(), 1)), start);
    }
    let mut dials = [0_usize; 2];
    for tick in 0..6000 {
        let now = start + Duration::from_millis(100 * tick);
        for n in 0..2 {
            if let Some((address, _)) = members[n].next(now) {
                assert_eq!(address, at[1 - n]);
                members[n].finish(address, Repair::heard_from(actors[1 - n]), quiet, now);
                members[1 - n].vouch(at[n].ip(), actors[n], true, now);
                dials[n] += 1;
            }
        }
    }
    let rounds = dials[0] + dials[1];
    assert!(
        (120..=200).contains(&rounds),
        "one round an interval between the two: {rounds} rounds, {dials:?} by each side, \
         {alone} by a member alone"
    );
    assert!(
        dials.iter().all(|dialled| *dialled >= 30),
        "each side goes on dialling: {dials:?}"
    );
}

/// C4's bound: a peer whose quiet rounds always come before this side's wait ends (it dials in
/// every 2 s, for twenty virtual minutes) holds this side's own dial back no longer than
/// [`REVERIFY`] after its last. This side's rounds come at the start and every [`REVERIFY`]
/// after, so its endpoint is verified again before [`VERIFIED_EXPIRES`] and is never dropped.
///
/// NEGATIVE CONTROL (2026-11-12): with the wait not capped at the endpoint's last round plus
/// [`REVERIFY`], this side never dialled again and its endpoint was dropped ten minutes in (`the
/// endpoint stays verified: 600s`).
#[test]
fn a_peer_that_always_dials_first_holds_this_side_back_no_longer_than_the_reverify_bound() {
    let quiet = Outcome::Repaired { transferred: false };
    let actor = [2; 48];
    let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
    let mut hints = hints();
    let start = Instant::now();
    hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
    let mut dialled = Vec::new();
    for tick in 0..=12_000_u64 {
        let since = Duration::from_millis(100 * tick);
        let now = start + since;
        if tick > 0 && tick % 20 == 0 {
            hints.vouch(peer.ip(), actor, true, now);
        }
        if let Some((address, _)) = hints.next(now) {
            hints.finish(address, Repair::heard_from(actor), quiet, now);
            dialled.push(since);
        }
        assert!(
            hints.established(peer),
            "the endpoint stays verified: {since:?}"
        );
    }
    assert_eq!(
        dialled,
        [Duration::ZERO, REVERIFY, 2 * REVERIFY],
        "this side's own rounds: the first, then one every REVERIFY"
    );
}

/// C4 holds back only an endpoint at rest. A quiet round dialled in moves a resting endpoint's
/// next dial an ordinary interval on from that round; it leaves alone one whose own last round
/// moved records (its follow-up is due), one whose own last round moved nothing and left its
/// hint a table still to take (announced, and not that round's), one a local append made due,
/// every one while a push runs, and a resting one where the round dialled in moved records.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::vouch`] holding back an endpoint whether or not
/// it rests, the endpoint whose round moved records waited an interval for its follow-up (`a
/// follow-up is not held back`), and a failing endpoint kept its backoff where a round dialled
/// in ends it (`an_inbound_round_from_a_peer_ends_its_endpoints_backoff` in the transport's
/// tests: `MUTANT false: the backoff ended`, `left: false`, `right: true`).
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::finish`] resting an endpoint whatever its hint
/// still names, the endpoint whose hint named an announced table waited out the quiet round
/// dialled in (`an endpoint whose hint names a table still to take is not held back`).
#[test]
fn a_quiet_round_dialled_in_holds_back_only_an_endpoint_at_rest() {
    let quiet = Outcome::Repaired { transferred: false };
    let actor = [2; 48];
    let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
    let start = Instant::now();
    let heard = start + Duration::from_secs(2);
    // An endpoint whose first round ended as `outcome` at the start.
    let after = |outcome| {
        let mut hints = hints();
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        let (address, _) = hints.next(start).unwrap();
        hints.finish(address, Repair::heard_from(actor), outcome, start);
        hints
    };

    let mut hints = after(quiet);
    let rested = hints.next_at(peer);
    assert!(rested < start + Duration::from_secs(5));
    hints.vouch(peer.ip(), actor, false, heard);
    assert_eq!(
        hints.next_at(peer),
        rested,
        "a round that moved records holds nothing back"
    );
    hints.vouch(peer.ip(), actor, true, heard);
    assert!(
        hints.next_at(peer) >= heard + Duration::from_secs(3),
        "a quiet round dialled in: an ordinary interval on from it"
    );

    // A quiet round dialled in before the follow-up is due: an endpoint already due is held
    // back by none, at rest or not.
    let mut hints = after(Outcome::Repaired { transferred: true });
    let follow_up = hints.next_at(peer);
    assert!(follow_up >= start + Duration::from_millis(200));
    assert!(follow_up < start + Duration::from_secs(1));
    hints.vouch(peer.ip(), actor, true, start + Duration::from_millis(100));
    assert_eq!(
        hints.next_at(peer),
        follow_up,
        "a follow-up is not held back"
    );

    // Its own round took one announced table and moved nothing; its hint names another.
    let mut hints = Hints::new([4; 32]);
    hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
    let (address, _) = hints.next(start).unwrap();
    let mut owing = Repair::heard_from(actor);
    owing.announce(&BTreeSet::from([[7; 32]]), false);
    hints.finish(address, owing, quiet, start);
    let owed = hints.next_at(peer);
    hints.vouch(peer.ip(), actor, true, heard);
    assert_eq!(
        hints.next_at(peer),
        owed,
        "an endpoint whose hint names a table still to take is not held back"
    );

    let mut hints = after(quiet);
    assert_eq!(hints.announce(start), 1);
    hints.vouch(peer.ip(), actor, true, heard);
    assert_eq!(
        hints.next_at(peer),
        start,
        "a local append's dial is not held back"
    );

    let mut hints = after(quiet);
    let rested = hints.next_at(peer);
    hints.hurrying = true;
    hints.vouch(peer.ip(), actor, true, heard);
    assert_eq!(
        hints.next_at(peer),
        rested,
        "nothing is held back in a push"
    );
}

/// A verified endpoint that stopped answering (or whose proof this Mac refuses) is dialed at a
/// doubling interval, up to two minutes, so a few dead peers cannot hold both outgoing
/// workers every few seconds; brief contention keeps the ordinary interval, and one repaired round
/// starts the doubling again. NEGATIVE CONTROL: with `Hints::finish` giving a refusal
/// `retry_delay` as it did, the refused endpoint is dialed more than 100 times.
#[test]
fn refused_dials_back_off_to_two_minutes_and_a_repaired_round_resets_them() {
    let refused = dials_in_ten_minutes(|_| Outcome::Refused);
    assert!(refused <= 12, "{refused} refused dials in ten minutes");
    let mut hints = hints();
    let now = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
    hints
        .peers
        .values_mut()
        .for_each(|peer| peer.verified = true);
    let mut at = now;
    let mut waits = Vec::new();
    for outcome in [
        Outcome::Refused,
        Outcome::Refused,
        Outcome::Refused,
        Outcome::Repaired { transferred: false },
        Outcome::Refused,
    ] {
        let (address, repair) = hints.next(at).unwrap();
        hints.finish(address, repair, outcome, at);
        let next = hints.peers[&address].next;
        waits.push(next - at);
        at = next;
    }
    // 5, 10 and 20 s, each plus under 2 s of jitter.
    let fives: Vec<_> = waits[..3].iter().map(|wait| wait.as_secs() / 5).collect();
    assert_eq!(fives, [1, 2, 4], "{waits:?}");
    assert!(waits[3] < Duration::from_secs(5), "{waits:?}");
    assert!(
        (BACKOFF_FIRST..BACKOFF_FIRST * 2).contains(&waits[4]),
        "a repaired round left the doubling at {waits:?}"
    );
}

#[test]
fn persistent_incomplete_rounds_back_off_after_brief_contention() {
    let busy = dials_in_ten_minutes(|_| Outcome::Busy);
    assert!(busy <= 16, "{busy} incomplete dials in ten minutes");
    let mixed = dials_in_ten_minutes(|attempt| {
        if attempt % 2 == 0 {
            Outcome::Busy
        } else {
            Outcome::Refused
        }
    });
    assert!(mixed <= 16, "{mixed} mixed failed dials in ten minutes");
    println!("ten virtual minutes: incomplete={busy}, mixed={mixed}");

    let mut hints = hints();
    let mut at = Instant::now();
    hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), at);
    hints
        .peers
        .values_mut()
        .for_each(|peer| peer.verified = true);
    for attempt in 0..10 {
        let (address, repair) = hints.next(at).unwrap();
        hints.finish(address, repair, Outcome::Busy, at);
        let next = hints.peers[&address].next;
        let delay = next - at;
        if attempt < 3 {
            assert!(
                (Duration::from_secs(3)..Duration::from_secs(5)).contains(&delay),
                "{attempt}: {delay:?}"
            );
        } else {
            let seconds = (5_u64 << (attempt - 3)).min(120);
            assert!(
                (Duration::from_secs(seconds)..Duration::from_secs(seconds + 2)).contains(&delay),
                "{attempt}: {delay:?}"
            );
        }
        at = next;
    }
    let (address, repair) = hints.next(at).unwrap();
    hints.finish(
        address,
        repair,
        Outcome::Repaired { transferred: false },
        at,
    );
    let next = hints.peers[&address].next;
    assert!(next - at < Duration::from_secs(5));
    at = next;
    let (address, repair) = hints.next(at).unwrap();
    hints.finish(address, repair, Outcome::Busy, at);
    assert!(
        hints.peers[&address].next - at < Duration::from_secs(5),
        "a completed round must restore fast recovery"
    );
}

#[test]
fn verified_and_busy_entries_survive_untrusted_floods() {
    let mut hints = hints();
    let now = Instant::now();
    let source = "10.1.1.2:1".parse().unwrap();
    hints.observe(&wire(1234), source, now);
    let (address, _) = hints.next(now).unwrap();
    for n in 1..=100 {
        hints.observe(&wire(1234), SocketAddr::from(([10, 1, 2, n], 1)), now);
    }
    assert!(hints.peers.contains_key(&address));
    let peer = hints.peers.get_mut(&address).unwrap();
    peer.busy = false;
    peer.verified = true;
    let later = now + Duration::from_secs(30);
    hints.observe(&wire(1234), source, later);
    assert_eq!(hints.peers[&address].seen, now);
    hints.expire(now + VERIFIED_EXPIRES);
    assert!(!hints.peers.contains_key(&address));
}

#[test]
fn rotating_tokens_preserves_repair_progress_and_rejects_own_previous_beacon() {
    let mut hints = hints();
    let now = Instant::now();
    let old = hints.wire(1234);
    hints.observe(&wire(2000), "10.1.1.2:1".parse().unwrap(), now);
    let (address, repair) = hints.next(now).unwrap();
    hints.rotate([6; 32]);
    assert_ne!(old, hints.wire(1234));
    hints.observe(&old, "10.1.1.3:1".parse().unwrap(), now);
    hints.observe(&hints.wire(1234), "10.1.1.3:1".parse().unwrap(), now);
    assert_eq!(hints.count(), 1);
    hints.finish(address, repair, Outcome::Refused, now);
    assert_eq!(
        hints.next(now + Duration::from_secs(10)).unwrap().0,
        address
    );
}

#[test]
fn repeated_failed_ports_cannot_pin_a_source_and_untried_hints_keep_their_turn() {
    let mut hints = hints();
    let now = Instant::now();
    let source = "10.1.1.2:1".parse().unwrap();
    hints.observe(&wire(1), source, now);
    hints.observe(&wire(2), source, now);
    let (address, repair) = hints.next(now).unwrap();
    hints.finish(address, repair, Outcome::Refused, now);
    hints.observe(&wire(3), source, now);
    assert!(hints.peers.contains_key(&"10.1.1.2:3".parse().unwrap()));
    for n in 1..=100 {
        hints.observe(&wire(1234), SocketAddr::from(([10, 2, 3, n], 1)), now);
    }
    assert!(hints.peers.contains_key(&"10.1.1.2:3".parse().unwrap()));
    hints.observe(&wire(3), source, now + Duration::from_secs(40));
    assert_eq!(hints.peers[&"10.1.1.2:3".parse().unwrap()].seen, now);
}

/// One Mac's own rounds on a virtual clock, with more endpoints than its places serve at once:
/// 64 verified peers, two rounds at a time, each ten seconds long and moving nothing, a dial
/// each 250 ms at most, so the ordinary scan comes round to an endpoint every 320 s or so.
/// Before each pick, `dials_in(since, reaches)` says whether the seat of one endpoint, the
/// target, completes a quiet round dialled in then ([`Hints::vouch`]); `reaches` says that a
/// place is free and the scan's next endpoint is the target. No endpoint may be dropped at any
/// tick. When each of the target's own rounds completed, from the start.
fn place_bound_rounds(
    span: Duration,
    mut dials_in: impl FnMut(Duration, bool) -> bool,
) -> Vec<Duration> {
    const ENDPOINTS: u8 = 64;
    const ROUND: Duration = Duration::from_secs(10);
    let quiet = Outcome::Repaired { transferred: false };
    let at = |n: u8| SocketAddr::from(([10, 1, 1, n], 1234));
    let actor = |address: SocketAddr| -> astream_wire::sync::Actor {
        let IpAddr::V4(ip) = address.ip() else {
            unreachable!("an IPv4 endpoint")
        };
        [ip.octets()[3]; 48]
    };
    let (target, before) = (at(34), at(33));
    let mut hints = hints();
    let start = Instant::now();
    for n in 2..2 + ENDPOINTS {
        hints.observe(&wire(1234), SocketAddr::from((at(n).ip(), 1)), start);
    }
    let mut running: Vec<(SocketAddr, Instant)> = Vec::new();
    let mut completed = Vec::new();
    for tick in 0..=span.as_millis() / 250 {
        let since = Duration::from_millis(250 * u64::try_from(tick).unwrap());
        let now = start + since;
        running.retain(|(address, ends)| {
            if *ends > now {
                return true;
            }
            hints.finish(*address, Repair::heard_from(actor(*address)), quiet, now);
            if *address == target {
                completed.push(since);
            }
            false
        });
        let free = running.len() < 2;
        if dials_in(since, free && hints.cursor == Some(before)) {
            hints.vouch(target.ip(), actor(target), true, now);
        }
        if free && let Some((address, _)) = hints.next(now) {
            running.push((address, now + ROUND));
        }
        assert_eq!(
            hints.count(),
            usize::from(ENDPOINTS),
            "no endpoint is dropped: {since:?}, the target's rounds completed at {completed:?}"
        );
    }
    completed
}

/// C4's bound where the places are full, over forty virtual minutes ([`place_bound_rounds`]):
/// the scan takes 320 s to come round, longer than the 120 s between [`REVERIFY`] and
/// [`VERIFIED_EXPIRES`]. A quiet round dialled in just as the scan reaches its endpoint holds
/// nothing back, since that endpoint is already due: its own rounds come at the scan's every
/// turn. And an endpoint whose seat dials in every two seconds, held back to [`REVERIFY`], is
/// then picked before the scan comes round: its own rounds complete no further apart than
/// [`REVERIFY`], a place's wait and the round itself. Neither is ever dropped.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::vouch`] holding back an endpoint already due,
/// as it did, the scan passed the endpoint by at each turn and its own rounds waited for the
/// bound, 490 s apart (`its own round at the scan's every turn: [170s, 660s, 1150s, 1640s,
/// 2130s]`).
/// NEGATIVE CONTROL (2026-11-12): with [`Hints::next_allowed`] leaving a held endpoint whose
/// last round is [`REVERIFY`] old to the scan's turn, the endpoint whose seat always dials in
/// was dropped ten minutes after its only round (`no endpoint is dropped: 610s, the target's
/// rounds completed at [10s]`: `left: 63`, `right: 64`).
/// NEGATIVE CONTROL (2026-11-12): with both, as before either, the endpoint the scan was about
/// to reach was dropped the same way (`no endpoint is dropped: 770s, the target's rounds
/// completed at [170s]`: `left: 63`, `right: 64`).
#[test]
fn a_full_macs_endpoint_is_verified_again_before_it_expires_whoever_dials_in() {
    let span = Duration::from_secs(2400);
    let bound = REVERIFY + 2 * Duration::from_secs(10);
    let gaps = |completed: &[Duration]| -> Vec<Duration> {
        completed.windows(2).map(|pair| pair[1] - pair[0]).collect()
    };

    let at_its_turn = place_bound_rounds(span, |_, reaches| reaches);
    assert!(
        at_its_turn.len() >= 7,
        "its own round at the scan's every turn: {at_its_turn:?}"
    );
    assert!(
        gaps(&at_its_turn).iter().all(|gap| *gap <= bound),
        "an endpoint already due is not held back: {at_its_turn:?}"
    );

    let always_first = place_bound_rounds(span, |since, _| since.as_millis() % 2000 == 0);
    assert_eq!(
        always_first.len(),
        5,
        "held back to the bound, its own rounds a REVERIFY apart: {always_first:?}"
    );
    assert!(
        gaps(&always_first)
            .iter()
            .all(|gap| *gap >= REVERIFY && *gap <= bound),
        "and picked before the scan comes round: {always_first:?}"
    );
}
