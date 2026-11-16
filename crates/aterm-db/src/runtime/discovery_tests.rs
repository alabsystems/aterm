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
        hints.vouch(peer.ip(), [7; 48], now);
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
            // The latest follow-up's turn: the ordinary endpoint before rests until its audit.
            at = [first, second]
                .iter()
                .map(|address| hints.peers[address].next)
                .max()
                .unwrap();
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

/// One established endpoint over sixty virtual minutes of ticks 100 ms apart, with no beacon
/// to time a probe: each dial completes at once as a round that moved nothing and had no table
/// exchange, and where `dials_in` its seat completes a round dialled in every two seconds
/// ([`Hints::vouch`]). When each of this side's own dials began, with whether it was an audit.
fn own_dials_in_an_hour(
    dials_in: bool,
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> Vec<(Duration, bool)> {
    under(flag, || {
        let quiet = Outcome::Repaired { transferred: false };
        let actor = [2; 48];
        let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
        let mut hints = hints();
        let start = Instant::now();
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        // Its beacons are not what this law is of: one heard and then silence would call for
        // the silence's probe ([`Hints::probe_at`]), which a live peer's beacons put off.
        hints.peers.get_mut(&peer).unwrap().beacon = None;
        let mut dialled = Vec::new();
        for tick in 0..36_000_u64 {
            let since = Duration::from_millis(100 * tick);
            let now = start + since;
            if dials_in && tick > 0 && tick % 20 == 0 {
                hints.vouch(peer.ip(), actor, now);
            }
            if let Some((address, repair)) = hints.next(now) {
                dialled.push((since, repair.audited()));
                hints.finish(address, Repair::heard_from(actor), quiet, now);
            }
            assert!(
                hints.established(peer),
                "the endpoint stays verified: {since:?}"
            );
        }
        dialled
    })
}

/// A settled endpoint rests until its audit: after its first round this side dials it once in
/// each audit's rest, [`AUDIT_FLOOR`] to [`REVERIFY`] after the one before, each dial an audit,
/// and nothing else, over an hour. Rounds its seat dials in every two seconds move none of it:
/// the audit runs on this side's own clock, and is never held back past [`REVERIFY`] (the law
/// main's `a_peer_that_always_dials_first_holds_this_side_back_no_longer_than_the_reverify_bound`
/// held for its quiet-round hold, which this rest replaces).
/// MUTANT (`QUIET_REDIALS`): a quiet round's endpoint is due an ordinary interval on, as before
/// the rest: over 700 dials an hour.
#[test]
fn a_settled_endpoint_is_dialled_once_an_audit_whoever_dials_in() {
    // Rounds dialled in draw this side's jitter afresh, so each run's audits fall apart; each
    // keeps the law.
    for dials_in in [false, true] {
        let own = own_dials_in_an_hour(dials_in, None);
        assert_eq!(own[0], (Duration::ZERO, false), "the first round, no audit");
        assert!(
            (8..=11).contains(&own.len()),
            "one round an audit's rest: {own:?}"
        );
        for pair in own.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!(
                (AUDIT_FLOOR..REVERIFY + Duration::from_millis(100)).contains(&gap),
                "{gap:?} between two of its own dials: {own:?}"
            );
            assert!(pair[1].1, "each later dial is an audit: {own:?}");
        }
    }
    let redials = own_dials_in_an_hour(false, Some(&QUIET_REDIALS)).len();
    assert!(redials > 700, "MUTANT: {redials} dials an hour");
}

/// A round its seat dialled in leaves a healthy endpoint as it is, at rest or due, whatever the
/// round moved; it pulls in one whose own dial failed, once per failure streak.
/// MUTANT (`REPULLS`): it pulls in the healthy endpoint at rest, which then redials within the
/// ordinary interval.
#[test]
fn a_round_dialled_in_leaves_a_healthy_endpoint_and_pulls_a_failing_one() {
    let healthy = |flag| {
        under(flag, || {
            let mut hints = hints();
            let start = Instant::now();
            let seat = seats(&mut hints, 1, start)[0];
            let rested = hints.next_at(seat.0);
            let heard = start + Duration::from_secs(10);
            hints.vouch(seat.0.ip(), seat.1, heard);
            (rested, hints.next_at(seat.0), heard)
        })
    };
    let (rested, after, heard) = healthy(None);
    assert!(rested >= heard + AUDIT_FLOOR - Duration::from_secs(10));
    assert_eq!(after, rested, "a resting endpoint keeps its audit");
    let (_, pulled, heard) = healthy(Some(&REPULLS));
    assert!(
        pulled <= heard + Duration::from_secs(5),
        "MUTANT: the healthy endpoint is pulled in"
    );

    let mut hints = hints();
    let start = Instant::now();
    let seat = seats(&mut hints, 1, start)[0];
    let mut now = start + Duration::from_secs(10);
    hints.peers.get_mut(&seat.0).unwrap().next = now;
    let (address, repair) = hints.next(now).unwrap();
    hints.finish(address, nothing_there(repair), Outcome::Refused, now);
    assert!(
        hints.next_at(seat.0) >= now + Duration::from_secs(5),
        "it backs off"
    );
    now += Duration::from_secs(1);
    hints.vouch(seat.0.ip(), seat.1, now);
    assert!(
        hints.next_at(seat.0) <= now + Duration::from_secs(5),
        "a failing endpoint is pulled in"
    );
    let at = hints.next_at(seat.0);
    let (address, repair) = hints.next(at).unwrap();
    hints.finish(address, nothing_there(repair), Outcome::Refused, at);
    let backoff = hints.next_at(seat.0);
    hints.vouch(seat.0.ip(), seat.1, at + Duration::from_secs(1));
    assert_eq!(hints.next_at(seat.0), backoff, "once per failure streak");
}

/// What a round over a table reports where this side asked which tables differ and the scan
/// took its turn: an audit's whole work.
fn audited_round(table: Digest) -> crate::replica::Report {
    crate::replica::Report {
        table: Some(table),
        caught_up: true,
        witnesses_asked: true,
        regular: true,
        ..Default::default()
    }
}

/// The wait after one endpoint's first round, which completed at once as `repair` and moved
/// nothing, and whether it rests.
fn first_round_waits(
    repair: Repair,
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> (Duration, bool) {
    under(flag, || {
        let mut hints = hints();
        let start = Instant::now();
        let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        let (address, _) = hints.next(start).unwrap();
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            start,
        );
        (hints.next_at(peer) - start, hints.peers[&peer].resting())
    })
}

/// A quiet round rests its endpoint only where it left the pair nothing to do. Its audit done
/// and its scan round every table both list, or with no table exchange at all, it rests until
/// its audit. Where its hint still names a table to take (here a lead), the next round follows
/// at once; where the scan has not yet taken its turn at each table both list (a first sweep, or
/// one after an absence), it goes on at the ordinary interval.
/// MUTANT (`RESTS_PENDING`): both of those rest until the audit.
#[test]
fn no_endpoint_rests_while_its_round_left_the_pair_something_to_do() {
    let actor = [2; 48];
    let swept = || {
        Repair::heard_from(actor)
            .swept(4)
            .applied(audited_round([1; 32]))
    };
    let owing = || {
        let mut owing = swept();
        assert!(owing.lead([7; 32]));
        owing
    };
    let sweeping = || Repair::heard_from(actor).applied(audited_round([1; 32]));
    let resting = |(wait, rests): (Duration, bool)| rests && wait >= AUDIT_FLOOR;
    assert!(
        resting(first_round_waits(Repair::heard_from(actor), None)),
        "no tables"
    );
    assert!(
        resting(first_round_waits(swept(), None)),
        "swept and audited"
    );
    let (follows, rests) = first_round_waits(owing(), None);
    assert!(
        !rests && follows < Duration::from_millis(500),
        "owed: {follows:?}"
    );
    let (ordinary, rests) = first_round_waits(sweeping(), None);
    assert!(
        !rests && (Duration::from_secs(3)..Duration::from_secs(5)).contains(&ordinary),
        "sweeping: {ordinary:?}"
    );
    assert!(
        resting(first_round_waits(owing(), Some(&RESTS_PENDING))),
        "MUTANT: owed, and rests"
    );
    assert!(
        resting(first_round_waits(sweeping(), Some(&RESTS_PENDING))),
        "MUTANT: sweeping, and rests"
    );
}

/// One endpoint's audit, its clock and its ask. It rests after a first round that audited.
/// A walk's dial 200 s in, led to a moved table, completes a round that neither asked nor took
/// the scan's turn: whether its audit is still due when it was. That dial is an audit, and its
/// round asks and visits a table whose witness differed, so the audit is not complete and the
/// next round follows at the ordinary interval; that next dial is not asked to audit again, and
/// its round takes the scan's turn, which completes it. Whether each of the two dials was an
/// audit, and the rest after the second round.
fn an_audit(
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> (bool, [bool; 2], Duration) {
    under(flag, || {
        let quiet = Outcome::Repaired { transferred: false };
        let (table, moved) = ([1; 32], [7; 32]);
        let mut hints = hints();
        let start = Instant::now();
        let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
        let actor = [2; 48];
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        hints.peers.get_mut(&peer).unwrap().beacon = None;
        let swept = || Repair::heard_from(actor).swept(4);
        let (address, _) = hints.next(start).unwrap();
        hints.finish(address, swept().applied(audited_round(table)), quiet, start);
        let due = hints.next_at(peer);

        let walked = start + Duration::from_secs(200);
        hints.moved(Subject::Table(moved), None, [], walked);
        let (address, repair) = hints.next(walked).expect("the walk dials it");
        assert!(repair.leads(&moved) && !repair.audited());
        let led = crate::replica::Report {
            table: Some(moved),
            caught_up: true,
            ..Default::default()
        };
        let led = swept().applied(led);
        hints.settle(actor, Some(address), walked, &led);
        hints.finish(address, led, quiet, walked);
        let kept = hints.next_at(peer) == due;

        let now = hints.next_at(peer);
        let (address, first) = hints.next(now).expect("its audit");
        let differed = crate::replica::Report {
            regular: false,
            ..audited_round(moved)
        };
        hints.finish(address, swept().applied(differed), quiet, now);
        let now = hints.next_at(peer);
        let (address, second) = hints.next(now).expect("the audit goes on");
        let turned = crate::replica::Report {
            witnesses_asked: false,
            ..audited_round(table)
        };
        hints.finish(address, swept().applied(turned), quiet, now);
        assert!(hints.peers[&peer].resting());
        (
            kept,
            [first.audited(), second.audited()],
            hints.next_at(peer) - now,
        )
    })
}

/// Only an audit this side dialled moves its endpoint's audit clock: an audit asks which tables
/// differ once, and is complete once its rounds asked and took the scan's turn, whichever
/// rounds did each; its endpoint then rests an audit's rest. A walk's round sets nothing.
/// MUTANT (`ANY_ROUND_AUDITS`): the walk's round completes an audit and puts the next off.
/// MUTANT (`AUDIT_REASKS`): the round that finishes the audit is asked to audit again, and
/// would ask and visit the table that differed first, again.
#[test]
fn only_an_audit_moves_its_endpoints_clock_and_it_asks_once() {
    let (kept, audits, rest) = an_audit(None);
    assert!(kept, "the walk's round leaves the audit when it was");
    assert_eq!(
        audits,
        [true, false],
        "the audit's dial, then the round that ends it"
    );
    assert!((AUDIT_FLOOR..REVERIFY).contains(&rest), "{rest:?}");
    assert!(
        !an_audit(Some(&ANY_ROUND_AUDITS)).0,
        "MUTANT: the walk put it off"
    );
    assert_eq!(
        an_audit(Some(&AUDIT_REASKS)).1,
        [true, true],
        "MUTANT: asked again"
    );
}

/// Whether the dial of a settled endpoint after this Mac woke from a sleep of `slept` finds the
/// table scan gone round ([`Repair::cycled`]).
fn cycled_after_sleeping(
    slept: Duration,
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> bool {
    under(flag, || {
        let quiet = Outcome::Repaired { transferred: false };
        let mut hints = hints();
        let start = Instant::now();
        let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
        let actor = [2; 48];
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        hints.peers.get_mut(&peer).unwrap().beacon = None;
        let (address, _) = hints.next(start).unwrap();
        let swept = Repair::heard_from(actor)
            .swept(4)
            .applied(audited_round([1; 32]));
        hints.finish(address, swept, quiet, start);
        assert!(hints.peers[&peer].resting());
        let woke = start + Duration::from_secs(100);
        hints.unsettle(woke, slept);
        let (_, repair) = hints.next(woke).expect("its audit");
        repair.cycled()
    })
}

/// A round after an absence of [`ABSENT`] by the wall clock sweeps the tables both list again:
/// the steady clock stopped while this Mac slept, so a sleep its owner told
/// ([`Hints::unsettle`]) counts. A wake after a short sleep, or a link that opened, keeps the
/// scan's count.
/// MUTANT (`ABSENCE_KEEPS_CYCLE`): the round after a night's sleep keeps the count, and its
/// endpoint rests at once.
#[test]
fn a_round_after_a_long_absence_sweeps_the_tables_again() {
    let minutes = |m: u64| Duration::from_secs(60 * m);
    assert!(cycled_after_sleeping(Duration::ZERO, None), "a link opened");
    assert!(cycled_after_sleeping(minutes(10), None), "a short sleep");
    assert!(!cycled_after_sleeping(minutes(15), None), "past ABSENT");
    assert!(
        cycled_after_sleeping(minutes(15), Some(&ABSENCE_KEEPS_CYCLE)),
        "MUTANT: the scan's count kept"
    );
}

/// `repair` as a round that ended as `outcome` leaves it, where a completed one moved nothing in
/// its endpoint's first sweep: the scan has not yet taken its turn at every table both list
/// ([`Repair::cycled`]), so its next dial follows at the ordinary interval.
fn sweeping(repair: Repair, outcome: Outcome) -> Repair {
    if matches!(outcome, Outcome::Repaired { .. }) {
        repair.applied(crate::replica::Report::default())
    } else {
        repair
    }
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
        hints.finish(address, sweeping(repair, outcome), outcome, at);
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
    let quiet = Outcome::Repaired { transferred: false };
    hints.finish(address, sweeping(repair, quiet), quiet, at);
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

/// `count` seats at `10.1.1.1..`, each with one endpoint a quiet round established at `now`:
/// every one rests for its quiet interval after it.
fn seats(hints: &mut Hints, count: u8, now: Instant) -> Vec<(SocketAddr, Actor)> {
    (1..=count)
        .map(|n| {
            let address = SocketAddr::from(([10, 1, 1, n], 1234));
            hints.observe(&wire(address.port()), address, now);
            let (chosen, _) = hints.next(now).unwrap();
            assert_eq!(chosen, address);
            let repair = Repair::heard_from([n; 48]);
            let quiet = Outcome::Repaired { transferred: false };
            hints.finish(address, repair, quiet, now);
            assert!(hints.established(address));
            (address, [n; 48])
        })
        .collect()
}

/// What a completed round over `table` reports, having sent the peer `sent` records.
fn over(table: Digest, sent: usize) -> crate::replica::Report {
    crate::replica::Report {
        table: Some(table),
        sent,
        caught_up: true,
        ..Default::default()
    }
}

/// A round this side dialled to `address` at `started` completes at `now` with the seat `actor`
/// and the application exchange `report` tells of, as the transport joins it: the round settles
/// its seat, then its endpoint. The state the round returns holds no request: its lease took
/// them when the peer authenticated.
fn completes(
    hints: &mut Hints,
    (address, actor): (SocketAddr, Actor),
    report: Option<crate::replica::Report>,
    started: Instant,
    now: Instant,
) {
    let repair = Repair::heard_from(actor);
    let repair = match report {
        Some(report) => repair.applied(report),
        None => repair,
    };
    hints.settle(actor, Some(address), started, &repair);
    let outcome = Outcome::of(true, &repair, None);
    hints.finish(address, repair, outcome, now);
}

/// A move seen now of a subject no look found a store of, as [`may_take`] reads it.
fn unheld() -> Hot {
    Hot {
        since: Instant::now(),
        held: None,
        cold: 0,
        tried: Default::default(),
    }
}

/// `body`'s result with the mutant `flag`, if any, on for this thread.
fn under<R>(
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
    body: impl FnOnce() -> R,
) -> R {
    if let Some(flag) = flag {
        flag.with(|flag| flag.set(true));
    }
    let result = body();
    if let Some(flag) = flag {
        flag.with(|flag| flag.set(false));
    }
    result
}

/// The endpoints a table's walk dials, in order, among five resting seats of which the first is
/// the move's source: the second lacked the table (it is sent a record), and each later one
/// already held it. Then whether the fifth seat's own wait stands as it was.
fn table_walk(
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> (Vec<SocketAddr>, bool) {
    under(flag, || {
        let table = [7; 32];
        let mut hints = hints();
        let start = Instant::now();
        let seats = seats(&mut hints, 5, start);
        let rest = hints.next_at(seats[4].0);
        let mut now = start + Duration::from_secs(1);
        assert!(hints.next(now).is_none(), "every seat rests");
        hints.moved(Subject::Table(table), None, [seats[0].1], now);
        let mut walked = Vec::new();
        for _ in 0..10 {
            let Some((address, repair)) = hints.next(now) else {
                break;
            };
            let seat = *seats.iter().find(|(known, _)| *known == address).unwrap();
            // A walk's dial is handed the table to lead with; the follow-up of the round that
            // sent a record is not, its seat being tried.
            let sent = if repair.pending() {
                walked.push(address);
                usize::from(walked.len() == 1)
            } else {
                0
            };
            completes(&mut hints, seat, Some(over(table, sent)), now, now);
            if hints.follow_up_contains(address) {
                // The round that sent a record is followed up before the walk goes on.
                now = hints.next_at(address);
            }
        }
        assert!(
            now < start + Duration::from_secs(3),
            "no seat's own wait ended"
        );
        (walked, hints.next_at(seats[4].0) == rest)
    })
}

/// A table that moved here is offered to the seats that may lack it without waiting for their
/// own next dials: one at a time, in this node's order, each handed the table to lead with, and
/// never to the seat it was received from. The walk ends once two seats in a row already held
/// the table, and the seat it never reached keeps its own wait: a move makes no endpoint due.
/// MUTANT (`NO_WALK`): a move dials nobody. MUTANT (`WALK_UNENDING`): the walk dials every seat.
/// MUTANT (`SOURCE_WALKED`): the table is offered back to the seat it came from.
#[test]
fn a_moved_table_walks_untried_seats_until_two_already_held_it_and_skips_its_source() {
    let at = |n: u8| SocketAddr::from(([10, 1, 1, n], 1234));
    assert_eq!(
        table_walk(None),
        (vec![at(2), at(3), at(4)], true),
        "the seat that lacked it, then two that held it; the fifth rests on"
    );
    assert_eq!(table_walk(Some(&NO_WALK)).0, [], "MUTANT: no dial");
    assert_eq!(
        table_walk(Some(&WALK_UNENDING)).0,
        [at(2), at(3), at(4), at(5)],
        "MUTANT: no cold answer ends the walk"
    );
    assert_eq!(
        table_walk(Some(&SOURCE_WALKED)).0,
        [at(1), at(2), at(3)],
        "MUTANT: the source is dialled first"
    );
}

/// The control journal's walk, and the membership records', reach every established seat
/// however many already held what moved: a revocation is never left to the cutoff a table's
/// walk ends at. Each seat is dialled once, handed no table, and the walk then offers nothing.
/// MUTANT (`CONTROL_CUTOFF`): the walk ends at the second seat.
#[test]
fn a_control_move_reaches_every_seat() {
    for subject in [Subject::Control, Subject::Members] {
        for mutant in [false, true] {
            let mut hints = hints();
            let start = Instant::now();
            let seats = seats(&mut hints, 5, start);
            let now = start + Duration::from_secs(1);
            hints.moved(subject, None, [], now);
            let mut walked = Vec::new();
            while let Some((address, repair)) = hints.next(now) {
                assert!(!repair.pending(), "no table moved");
                walked.push(address);
                let seat = *seats.iter().find(|(known, _)| *known == address).unwrap();
                under(mutant.then_some(&CONTROL_CUTOFF), || {
                    completes(&mut hints, seat, None, now, now);
                });
                assert!(walked.len() <= seats.len(), "a seat was dialled twice");
            }
            let all: Vec<_> = seats.iter().map(|(address, _)| *address).collect();
            if mutant {
                assert_eq!(walked, all[..2], "MUTANT: two cold answers ended it");
            } else {
                assert_eq!(walked, all, "{subject:?} reached every seat once");
            }
            assert_eq!(hints.hot_count(), 0, "the walk is over");
        }
    }
    // More subjects than a full table list, the control journal and the membership records
    // are not kept.
    let mut hints = hints();
    let now = Instant::now();
    for table in 0..=u8::try_from(MAX_HOT).unwrap() {
        hints.moved(Subject::Table([table; 32]), None, [], now);
    }
    assert_eq!(hints.hot_count(), MAX_HOT);
}

/// A round that began before a move was seen settles nothing for it, though it was over the
/// moved table: it may have read its inventory before the write, so its seat is still offered
/// the table. The walk dials it [`WALK_GAP`] after that round ended and not the instant it
/// ended, and the scheduler's next dial says so: writes that keep coming leave each round its
/// wait (the law main's `a_local_append_makes_every_established_endpoint_due_at_once` held for
/// an announced append before the merge of 2026-11-13).
/// MUTANT (`STALE_SETTLES`): the round settles its seat, and the write waits for the seat's
/// own next dial.
/// MUTANT (`WALK_AT_ONCE`): the seat is dialled again the instant its round ended.
#[test]
fn a_round_begun_before_a_move_settles_nothing_for_it() {
    for (mutant, expected) in [
        (None, "walked after the gap"),
        (Some(&STALE_SETTLES), "settled"),
        (Some(&WALK_AT_ONCE), "walked at once"),
    ] {
        let table = [7; 32];
        let mut hints = hints();
        let start = Instant::now();
        let seat = seats(&mut hints, 1, start)[0];
        let began = hints.next_at(seat.0);
        assert_eq!(hints.next(began).unwrap().0, seat.0, "its own turn");
        let seen = began + Duration::from_secs(1);
        hints.moved(Subject::Table(table), None, [], seen);
        assert!(hints.next(seen).is_none(), "its round is in flight");
        let ended = seen + Duration::from_secs(1);
        let offered = under(mutant, || {
            completes(&mut hints, seat, Some(over(table, 0)), began, ended);
            let at_once = hints
                .next(ended)
                .map(|(address, repair)| (address, repair.pending()));
            let timed = hints.next_dial_at(ended, true);
            let after = hints
                .next(ended + WALK_GAP)
                .map(|(address, repair)| (address, repair.pending()));
            (at_once, timed, after)
        });
        let walked = Some((seat.0, true));
        match expected {
            "walked after the gap" => assert_eq!(
                offered,
                (None, Some(ended + WALK_GAP), walked),
                "the seat is dialled, led by the table, once its round's gap is over"
            ),
            "settled" => assert_eq!(
                (offered.0, offered.2),
                (None, None),
                "MUTANT: the seat counts as holding the write"
            ),
            _ => assert_eq!(
                offered.0, walked,
                "MUTANT: the seat is dialled the instant its round ended"
            ),
        }
    }
}

/// A dial handed a moved table settles its seat when its round completes, whichever table the
/// round took: a peer that lacks the table, or a round a follow-up outranked the lead in, is
/// not dialled for it again, and the lead stays with the peer's hints for its next round.
/// MUTANT (`LED_UNSETTLED`): only a round over the table settles its seat, so the walk dials
/// that seat again at every turn, once its gap after the round is over ([`WALK_GAP`]).
#[test]
fn a_seat_handed_a_table_is_settled_by_its_round_whatever_table_it_took() {
    for mutant in [false, true] {
        let table = [7; 32];
        let mut hints = hints();
        let start = Instant::now();
        let seat = seats(&mut hints, 1, start)[0];
        let now = start + Duration::from_secs(1);
        hints.moved(Subject::Table(table), None, [], now);
        let (address, repair) = hints.next(now).unwrap();
        assert!(address == seat.0 && repair.pending());
        under(mutant.then_some(&LED_UNSETTLED), || {
            completes(&mut hints, seat, Some(over([8; 32], 0)), now, now);
        });
        let again = hints.next(now + WALK_GAP).map(|(address, _)| address);
        if mutant {
            assert_eq!(
                again,
                Some(seat.0),
                "MUTANT: dialled again at its gap's end"
            );
        } else {
            assert_eq!(again, None, "the seat was tried");
            assert_eq!(hints.hot_count(), 0);
        }
    }
}

/// A seat that dials in settles itself for a moved table only by a round over that table, begun
/// once the move was seen: such a round that already held it is a cold answer, and the walk
/// goes on to the next seat. A round it dialled in over another table says nothing of this one.
#[test]
fn a_seat_that_dials_in_is_settled_by_a_round_over_the_table() {
    for (taken, next) in [([8; 32], 1), ([7; 32], 2)] {
        let table = [7; 32];
        let mut hints = hints();
        let start = Instant::now();
        let seats = seats(&mut hints, 2, start);
        let now = start + Duration::from_secs(1);
        hints.moved(Subject::Table(table), None, [], now);
        let round = Repair::heard_from(seats[0].1).applied(over(taken, 0));
        hints.settle(seats[0].1, None, now, &round);
        assert_eq!(
            hints.next(now).map(|(address, _)| address),
            Some(SocketAddr::from(([10, 1, 1, next], 1234)))
        );
    }
}

/// The state a round leaves whose table exchange broke off after its control exchange
/// completed, with the seat `actor` ([`Outcome::Unfinished`]).
fn broken_off(actor: Actor) -> Repair {
    let mut repair = Repair::heard_from(actor);
    repair.application_failed(crate::Error::repair(
        "the table exchange broke off",
        aterm_types::network_state::RepairCause::Io,
    ));
    repair
}

/// A round whose table exchange broke off after its control exchange completed is unfinished:
/// its endpoint stays established but neither rests nor completes its audit, so it is dialled
/// again at the ordinary interval for its first few such rounds and then backs off as a busy one
/// does, until a round finishes. The witness ask the broken exchange had answered counts toward
/// the audit, so the round that finishes it need only take the scan's turn.
/// MUTANT (`UNFINISHED_FINISHES`): the round counts as a quiet one, completes the audit and
/// rests until the next.
/// NEGATIVE CONTROL (2026-11-13, by an inverse edit then undone): with `finish` counting no
/// broken round's ask, the round that took the scan's turn left the audit incomplete (`the ask
/// the broken round had answered`).
#[test]
fn an_unfinished_round_neither_rests_nor_completes_its_audit() {
    for mutant in [false, true] {
        let mut hints = hints();
        let start = Instant::now();
        let peer: SocketAddr = "10.1.1.2:1234".parse().unwrap();
        let actor = [2; 48];
        hints.observe(&wire(1234), SocketAddr::from((peer.ip(), 1)), start);
        hints.peers.get_mut(&peer).unwrap().beacon = None;
        let mut now = start;
        let mut waits = Vec::new();
        for round in 0..=QUICK_BUSY_RETRIES {
            let (address, _) = hints.next(now).expect("its retry");
            let asked = crate::replica::Report {
                witnesses_asked: round == 0,
                ..Default::default()
            };
            let broken = broken_off(actor).swept(4).interrupted(asked);
            let outcome = under(mutant.then_some(&UNFINISHED_FINISHES), || {
                Outcome::of(true, &broken, None)
            });
            hints.finish(address, broken, outcome, now);
            assert!(hints.established(peer), "the control exchange completed");
            waits.push(hints.next_at(peer) - now);
            now = hints.next_at(peer);
            if mutant {
                break;
            }
        }
        if mutant {
            assert!(
                waits[0] >= AUDIT_FLOOR && hints.peers[&peer].audit_due.is_some(),
                "MUTANT: the broken round completed the audit and rests: {waits:?}"
            );
            continue;
        }
        assert!(hints.peers[&peer].audit_due.is_none(), "no audit completed");
        let quick = usize::try_from(QUICK_BUSY_RETRIES).unwrap();
        assert!(
            waits[..quick].iter().all(|wait| *wait < BACKOFF_FIRST)
                && waits[quick] >= BACKOFF_FIRST
                && waits[quick] < AUDIT_FLOOR,
            "the ordinary interval, then a backoff: {waits:?}"
        );
        let (address, _) = hints.next(now).unwrap();
        let turned = crate::replica::Report {
            table: Some([1; 32]),
            caught_up: true,
            regular: true,
            ..Default::default()
        };
        let finished = Repair::heard_from(actor).swept(4).applied(turned);
        let outcome = Outcome::of(true, &finished, None);
        hints.finish(address, finished, outcome, now);
        assert!(
            hints.peers[&peer].audit_due.is_some() && hints.next_at(peer) - now >= AUDIT_FLOOR,
            "the ask the broken round had answered and this round's turn complete the audit"
        );
    }
}

/// A round whose table exchange broke off after its control exchange settles its seat for the
/// control journal alone: the membership records and the table its dial was handed may not have
/// reached the seat, so they stay offered to it, and its own retry dial is handed the table
/// again.
/// MUTANT (`UNFINISHED_SETTLES`): the round settles the seat for all three, and its retry is
/// handed nothing.
#[test]
fn an_unfinished_round_settles_its_seat_for_the_control_journal_alone() {
    for mutant in [false, true] {
        let table = [7; 32];
        let mut hints = hints();
        let start = Instant::now();
        let seat = seats(&mut hints, 1, start)[0];
        let now = start + Duration::from_secs(1);
        for subject in [Subject::Control, Subject::Members, Subject::Table(table)] {
            hints.moved(subject, None, [], now);
        }
        let (address, repair) = hints.next(now).unwrap();
        assert!(address == seat.0 && repair.leads(&table));
        let broken = broken_off(seat.1);
        under(mutant.then_some(&UNFINISHED_SETTLES), || {
            hints.settle(seat.1, Some(address), now, &broken);
        });
        let outcome = Outcome::of(true, &broken, None);
        hints.finish(address, broken, outcome, now);
        hints.cool();
        let retry = hints.next_at(address);
        assert!(
            retry - now < BACKOFF_FIRST,
            "it is retried at the ordinary interval"
        );
        let (again, repair) = hints.next(retry).unwrap();
        assert_eq!(again, seat.0);
        if mutant {
            assert_eq!(
                hints.hot_count(),
                0,
                "MUTANT: the seat was tried for all three"
            );
            assert!(!repair.leads(&table), "MUTANT: its retry is handed nothing");
        } else {
            assert_eq!(
                hints.hot_count(),
                2,
                "the records and the table stay offered"
            );
            assert!(repair.leads(&table), "its retry is handed the table");
        }
    }
}

/// A move the walk passed by because the lists of the last round with its one untried seat said
/// the seat does not take it is parked, and offered again once a later round's lists say the seat
/// takes it ([`Hints::reoffer`]): the seat took the table up and listed it in a round of its own
/// that was still in flight when the move was seen, or in the round of a dial of this Mac's to it,
/// in flight then too, whose lists come only as it ends. That round began before the move was
/// seen and settles nothing of it ([`Hints::settle`]).
/// MUTANT (`PARKS_NOTHING`): the move is dropped, and no walk dials the seat for it, in flight or
/// not.
#[test]
fn a_move_passed_by_on_a_seats_older_lists_is_offered_once_its_lists_take_it() {
    let table = [7; 32];
    for (mutant, in_flight) in [(false, false), (false, true), (true, false), (true, true)] {
        under(mutant.then_some(&PARKS_NOTHING), || {
            let mut hints = hints();
            let start = Instant::now();
            let (address, actor) = seats(&mut hints, 1, start)[0];
            let quiet = Outcome::Repaired { transferred: false };
            // The seat's last lists leave out the table this Mac listed.
            assert!(
                hints
                    .peers
                    .get_mut(&address)
                    .unwrap()
                    .remember_lists(&Repair::heard_from(actor).listing_at([table], [table], start))
            );
            let mut now = start + Duration::from_secs(1);
            let dial = if in_flight {
                // A walk for the control journal has a dial of the seat in flight.
                hints.moved(Subject::Control, None, [], now);
                let (dialled, repair) = hints.next(now).expect("the control journal's walk");
                assert_eq!(dialled, address);
                Some(repair)
            } else {
                None
            };
            hints.moved(Subject::Table(table), Some(start), [], now);
            assert!(
                hints.next(now).is_none(),
                "the seat's lists leave the table out"
            );
            assert_eq!(
                hints.hot_count(),
                usize::from(in_flight),
                "the table is not offered, the control journal is while its dial flies"
            );
            let listed = now + Duration::from_millis(100);
            if let Some(repair) = dial {
                // As `Transport::finish` takes a dial's end.
                let ended = repair.listing_at([table], [], listed);
                hints.settle(actor, Some(address), now, &ended);
                hints.finish(address, ended, quiet, now);
                now += WALK_GAP;
            } else {
                // The seat's round, which began before the move was seen, ends.
                let incoming = Repair::heard_from(actor).listing_at([table], [], listed);
                hints.settle(actor, None, start, &incoming);
            }
            let walked = hints.next(now);
            if mutant {
                assert!(
                    walked.is_none(),
                    "MUTANT: never walked (in flight {in_flight})"
                );
                return;
            }
            let (dialled, repair) = walked.expect("the seat is walked for the table");
            assert_eq!(dialled, address);
            assert!(repair.leads(&table), "in flight {in_flight}");
        });
    }
}

/// A later round's lists widen what the walk offers a seat and never narrow what this Mac's own
/// last dial there found both sides listing ([`Peer::own`], [`may_take`]). Here a round the seat
/// began since left out a table both listed in this Mac's own round, as one does while its owner
/// excludes the table; the seat's selection may take the table again with no round between,
/// which nothing would tell this Mac, so a write here is walked to the seat as it was before
/// those lists. At rest, the endpoint's own lists take the move at once. With a dial of this
/// Mac's to the seat in flight and the seat's round, begun after it, ending first, the lists of
/// this Mac's earlier dial stand while the dial flies, so the move stays offered; where no
/// earlier dial traded lists, the move is parked while the dial flies and offered again as the
/// dial ends with lists that found both listing the table ([`Hints::reoffer`]), though they
/// are older than the seat's.
/// MUTANT (`LATER_LISTS_NARROW`, as 2c91654df0d had it): the newest lists alone tell, the move is
/// parked, and no walk dials the seat for it, in flight or not; the write would wait for the next
/// round with the seat, at rest an audit.
#[test]
fn a_later_rounds_lists_never_narrow_what_this_macs_own_round_found_a_seat_taking() {
    let table = [7; 32];
    let quiet = Outcome::Repaired { transferred: false };
    for mutant in [false, true] {
        for (in_flight, earlier) in [(false, true), (true, true), (true, false)] {
            under(mutant.then_some(&LATER_LISTS_NARROW), || {
                let mut hints = hints();
                let start = Instant::now();
                let (address, actor) = seats(&mut hints, 1, start)[0];
                let case =
                    format!("MUTANT {mutant}, in flight {in_flight}, earlier dial {earlier}");
                if earlier {
                    // This Mac's own dial found both listing the table.
                    hints.finish(
                        address,
                        Repair::heard_from(actor)
                            .applied(crate::replica::Report::default())
                            .listing_at([table], [], start),
                        quiet,
                        start,
                    );
                }
                let mut now = start + Duration::from_secs(1);
                let dial = if in_flight {
                    // A walk for the control journal has a dial of the seat in flight.
                    hints.moved(Subject::Control, None, [], now);
                    let (dialled, repair) = hints.next(now).expect("the control journal's walk");
                    assert_eq!(dialled, address, "{case}");
                    Some(repair)
                } else {
                    None
                };
                // The seat's round, begun after the dial, ends first, its lists leaving the
                // table out.
                let later = now + Duration::from_millis(200);
                let left_out = Repair::heard_from(actor).listing_at([table], [table], later);
                hints.settle(actor, None, now, &left_out);
                hints.moved(Subject::Table(table), Some(start), [], now);
                if let Some(repair) = dial {
                    assert!(hints.next(now).is_none(), "{case}: the seat's dial flies");
                    let offered = earlier && !mutant;
                    let moved = Subject::Table(table);
                    assert_eq!(
                        (
                            hints.hot.contains_key(&moved),
                            hints.parked.contains_key(&moved)
                        ),
                        (offered, !offered),
                        "{case}: the table stays offered where an earlier dial's lists stand, and \
                         is parked where none does"
                    );
                    // As `Transport::finish` takes a dial's end: its lists were read before the
                    // seat's.
                    let ended = repair
                        .applied(crate::replica::Report::default())
                        .listing_at([table], [], now + Duration::from_millis(100));
                    hints.settle(actor, Some(address), now, &ended);
                    hints.finish(address, ended, quiet, now);
                    now += WALK_GAP;
                }
                let walked = hints.next(now);
                if mutant {
                    assert!(walked.is_none(), "{case}: passed over");
                    return;
                }
                let (dialled, repair) = walked.expect(&case);
                assert_eq!(dialled, address, "{case}");
                assert!(repair.leads(&table), "{case}");
            });
        }
    }
}

/// A seat whose walk dial failed (a crossed dial, answered busy) keeps the moved table: the
/// walk does not dial it while its dials fail, and its own retry, at the ordinary interval, is
/// handed the table to lead with.
/// MUTANT (`COOLS_UNESTABLISHED`): only established seats keep a moved table, so the failed
/// seat's retry is handed nothing.
#[test]
fn a_seat_whose_walk_dial_failed_is_handed_the_table_at_its_retry() {
    for mutant in [false, true] {
        under(mutant.then_some(&COOLS_UNESTABLISHED), || {
            let table = [7; 32];
            let mut hints = hints();
            let start = Instant::now();
            let seat = seats(&mut hints, 1, start)[0];
            let now = start + Duration::from_secs(1);
            hints.moved(Subject::Table(table), None, [], now);
            let (address, repair) = hints.next(now).unwrap();
            assert!(address == seat.0 && repair.leads(&table));
            hints.finish(address, Repair::heard_from(seat.1), Outcome::Busy, now);
            assert!(!hints.established(address));
            let retry = hints.next_at(address);
            assert!(retry - now < BACKOFF_FIRST, "a busy round's quick retry");
            let (again, repair) = hints.next(retry).unwrap();
            assert_eq!(again, seat.0);
            assert_eq!(repair.leads(&table), !mutant, "MUTANT {mutant}");
        });
    }
}

/// The walk offers a moved table to each seat as the newest lists with it tell, whichever side
/// began their round (`crate::replica::Listing::takes`). A seat whose lists left out a table this
/// Mac listed holds none of it, or cannot take it: the walk passes it over, and no dial of it is
/// handed the table, so a move of a table few seats hold costs no round with the rest. A seat
/// that listed it beside this Mac is walked. A table this Mac did not list with a seat is offered
/// to it only where this Mac's store of it is younger than those lists (one this Mac took up
/// since, as `transport::tests`'
/// `a_table_taken_up_since_the_last_lists_is_offered_to_the_peer_when_written_here` shows across
/// real rounds); one this Mac held then and did not list (its policy excludes it) costs that seat
/// no round, however often it is written.
/// MUTANT (`WALKS_UNLISTED`): every seat is walked and handed it.
/// MUTANT (`crate::replica::TAKES_ANY_UNLISTED`): seat 2 is walked for a table this Mac held
/// before its lists with seat 2 and did not list.
/// MUTANT (`crate::replica::TAKES_ONLY_LISTED`): seat 2 is passed over for a table this Mac took
/// up since.
#[test]
fn a_walk_offers_a_table_as_each_seats_last_lists_tell() {
    use crate::replica::{TAKES_ANY_UNLISTED, TAKES_ONLY_LISTED};
    let (table, other) = ([7; 32], [8; 32]);
    let mutants: [(
        &str,
        Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
    ); 4] = [
        ("none", None),
        ("WALKS_UNLISTED", Some(&WALKS_UNLISTED)),
        ("TAKES_ANY_UNLISTED", Some(&TAKES_ANY_UNLISTED)),
        ("TAKES_ONLY_LISTED", Some(&TAKES_ONLY_LISTED)),
    ];
    for taken_up in [false, true] {
        for (mutant, cell) in mutants {
            under(cell, || {
                let mut hints = hints();
                let start = Instant::now();
                let seats = seats(&mut hints, 3, start);
                let listed = start + Duration::from_millis(100);
                // Seat 0 left out the table this Mac listed, seat 1 listed it beside this Mac,
                // and this Mac did not list it with seat 2.
                let lists = [
                    (vec![table, other], vec![table]),
                    (vec![table, other], vec![]),
                    (vec![other], vec![]),
                ];
                for ((address, actor), (ours, left_out)) in seats.iter().zip(lists) {
                    hints.finish(
                        *address,
                        Repair::heard_from(*actor).listing_at(ours, left_out, listed),
                        Outcome::Repaired { transferred: false },
                        start,
                    );
                }
                let held = if taken_up {
                    listed + Duration::from_millis(100)
                } else {
                    start
                };
                let now = start + Duration::from_secs(1);
                hints.moved(Subject::Table(table), Some(held), [], now);
                let mut walked = Vec::new();
                while let Some((address, repair)) = hints.next(now) {
                    assert!(repair.leads(&table), "a walk's dial is handed the table");
                    walked.push(address);
                    let seat = *seats.iter().find(|(known, _)| *known == address).unwrap();
                    // A round over the table that neither sent it nor caught the seat up: no cold
                    // answer ends the walk before the third seat, and no follow-up holds it.
                    let short = crate::replica::Report {
                        table: Some(table),
                        ..Default::default()
                    };
                    completes(&mut hints, seat, Some(short), now, now);
                    assert!(walked.len() <= seats.len(), "a seat was dialled twice");
                }
                let expected = match (mutant, taken_up) {
                    ("WALKS_UNLISTED", _) => vec![seats[0].0, seats[1].0, seats[2].0],
                    ("TAKES_ANY_UNLISTED", _) | ("none", true) => vec![seats[1].0, seats[2].0],
                    _ => vec![seats[1].0],
                };
                assert_eq!(walked, expected, "MUTANT {mutant}, taken up {taken_up}");
            });
        }
    }
}

/// A table discovered in a peer's incoming round is offered back after a local write, even
/// while the endpoint's last outgoing state still lists only an older table. Learning lists
/// changes neither the endpoint's rest nor its queued repair requests, and affects only that
/// authenticated seat. NEGATIVE CONTROL: leaving the incoming lists out loses the reverse walk.
#[test]
fn an_incoming_table_discovery_enables_its_reverse_walk_without_replacing_repair_state() {
    for mutant in [false, true] {
        under(mutant.then_some(&INCOMING_LISTS_LEFT), || {
            let (table, other) = ([7; 32], [8; 32]);
            let mut hints = hints();
            let start = Instant::now();
            let seats = seats(&mut hints, 2, start);
            let quiet = Outcome::Repaired { transferred: false };
            for (address, actor) in &seats {
                hints.finish(
                    *address,
                    Repair::heard_from(*actor)
                        .listing_at([table, other], [table], start)
                        .swept(1),
                    quiet,
                    start,
                );
            }
            let (address, actor) = seats[0];
            let peer = hints.peers.get_mut(&address).unwrap();
            peer.repair.lead(other);
            peer.repair.audit();
            let before = (
                peer.seen,
                peer.next,
                peer.audit_due,
                peer.verified,
                peer.failures,
            );
            let incoming = Repair::heard_from(actor).listing_at(
                [table, other],
                [],
                start + Duration::from_secs(1),
            );
            hints.settle(actor, None, start, &incoming);
            let peer = &hints.peers[&address];
            assert_eq!(
                (
                    peer.seen,
                    peer.next,
                    peer.audit_due,
                    peer.verified,
                    peer.failures
                ),
                before,
                "incoming lists do not restart or verify an endpoint"
            );
            assert!(peer.repair.leads(&other) && peer.repair.audited() && peer.repair.cycled());
            assert!(
                !peer.repair.listed().unwrap().takes(&table, None),
                "the outgoing repair snapshot was not replaced"
            );
            assert_eq!(may_take(peer, &Subject::Table(table), &unheld()), !mutant);
            assert!(!may_take(
                &hints.peers[&seats[1].0],
                &Subject::Table(table),
                &unheld()
            ));
            let now = start + Duration::from_secs(2);
            hints.moved(Subject::Table(table), None, [], now);
            let dial = hints.next(now);
            assert_eq!(
                dial.as_ref().map(|(address, _)| *address),
                (!mutant).then_some(address)
            );
            if let Some((_, repair)) = dial {
                assert!(repair.leads(&table) && repair.leads(&other) && repair.audited());
            }
        });
    }
}

/// A busy endpoint keeps its seat while its outgoing worker holds the old repair snapshot.
/// Incoming lists learned then survive that worker's later return. An actual newer exchange
/// may replace them; one with no lists or the same exchange time cannot. NEGATIVE CONTROL:
/// accepting an older returned snapshot loses the reverse walk.
#[test]
fn an_outgoing_completion_cannot_roll_back_incoming_table_lists() {
    for (listed_second, mutant) in [
        (Some(1), false),
        (Some(1), true),
        (Some(2), false),
        (Some(3), false),
        (None, false),
    ] {
        under(mutant.then_some(&LISTS_ROLL_BACK), || {
            let (table, other) = ([7; 32], [8; 32]);
            let mut hints = hints();
            let start = Instant::now();
            let (address, actor) = seats(&mut hints, 1, start)[0];
            let began = hints.next_at(address);
            let (chosen, _) = hints.next(began).unwrap();
            assert_eq!(chosen, address);
            let incoming = Repair::heard_from(actor).listing_at(
                [table, other],
                [],
                began + Duration::from_secs(2),
            );
            hints.settle(actor, None, began, &incoming);
            assert!(hints.peers[&address].busy);
            assert_eq!(hints.peers[&address].repair.actor(), None);
            assert!(may_take(
                &hints.peers[&address],
                &Subject::Table(table),
                &unheld()
            ));
            let returned = Repair::heard_from(actor).applied(crate::replica::Report {
                regular: true,
                witnesses_asked: true,
                ..over(other, 0)
            });
            let returned = match listed_second {
                Some(second) => returned.listing_at(
                    [table, other],
                    [table],
                    began + Duration::from_secs(second),
                ),
                None => returned,
            }
            .swept(1);
            let finished = began + Duration::from_secs(4);
            hints.settle(actor, Some(address), began, &returned);
            hints.finish(
                address,
                returned,
                Outcome::Repaired { transferred: false },
                finished,
            );
            let now = finished + WALK_GAP;
            assert!(
                hints.next_at(address) > now,
                "its ordinary audit is still later"
            );
            hints.moved(Subject::Table(table), None, [], now);
            let expects_walk = !mutant && listed_second != Some(3);
            assert_eq!(
                hints.next(now).map(|(address, repair)| {
                    assert!(repair.leads(&table));
                    address
                }),
                expects_walk.then_some(address),
                "returned lists at {listed_second:?}, MUTANT {mutant}"
            );
        });
    }
}

/// A monotonic clock need not advance between two exchanges. Conflicting tied lists then
/// establish no absence, while their time still excludes older returned negative lists. A
/// strictly newer negative list can exclude the table again. NEGATIVE CONTROL: ignoring the
/// incoming tied positive list strands the reverse write.
#[test]
fn tied_incoming_table_lists_keep_a_reverse_write_walkable() {
    for mutant in [false, true] {
        under(mutant.then_some(&TIED_LISTS_LEFT), || {
            let (table, other) = ([7; 32], [8; 32]);
            let mut hints = hints();
            let start = Instant::now();
            let (address, actor) = seats(&mut hints, 1, start)[0];
            let quiet = Outcome::Repaired { transferred: false };
            let at = start + Duration::from_secs(2);
            hints.finish(
                address,
                Repair::heard_from(actor).listing_at([table, other], [table], at),
                quiet,
                at,
            );
            let incoming = Repair::heard_from(actor).listing_at([table, other], [], at);
            hints.settle(actor, None, at, &incoming);
            let old = Repair::heard_from(actor).listing_at([table, other], [table], start);
            hints.finish(address, old, quiet, at + Duration::from_secs(1));
            let now = at + Duration::from_secs(2);
            hints.moved(Subject::Table(table), None, [], now);
            assert_eq!(
                hints.next(now).map(|(address, repair)| {
                    assert!(repair.leads(&table));
                    address
                }),
                (!mutant).then_some(address),
                "MUTANT {mutant}"
            );
            let newer = Repair::heard_from(actor).listing_at(
                [table, other],
                [table],
                at + Duration::from_secs(3),
            );
            hints.settle(actor, None, now, &newer);
            assert!(!may_take(
                &hints.peers[&address],
                &Subject::Table(table),
                &unheld()
            ));
        });
    }
}

/// An endpoint rebound to another authenticated seat cannot use its former seat's lists, and a
/// later round from the former seat cannot replace the new one's evidence.
#[test]
fn incoming_table_lists_belong_to_the_authenticated_seat() {
    let (table, other) = ([7; 32], [8; 32]);
    let mut hints = hints();
    let start = Instant::now();
    let (address, first) = seats(&mut hints, 1, start)[0];
    let second = [9; 48];
    let quiet = Outcome::Repaired { transferred: false };
    hints.finish(
        address,
        Repair::heard_from(first).listing_at([table, other], [table], start),
        quiet,
        start,
    );
    assert!(!may_take(
        &hints.peers[&address],
        &Subject::Table(table),
        &unheld()
    ));
    hints.finish(address, Repair::heard_from(second), quiet, start);
    assert!(
        may_take(&hints.peers[&address], &Subject::Table(table), &unheld()),
        "new seat has no lists yet"
    );
    let incoming = Repair::heard_from(second).listing_at(
        [table, other],
        [table],
        start + Duration::from_secs(1),
    );
    hints.settle(second, None, start, &incoming);
    let former = Repair::heard_from(first).listing_at(
        [table, other],
        [other],
        start + Duration::from_secs(2),
    );
    hints.settle(first, None, start, &former);
    assert!(!may_take(
        &hints.peers[&address],
        &Subject::Table(table),
        &unheld()
    ));
}

/// A seat with two endpoints (a Mac on Wi-Fi and Ethernet) is walked at one of them: while a
/// round with the seat is in flight the walk does not dial its other endpoint, and the round's
/// end settles the seat for both.
/// MUTANT (`TWIN_WALKED`): the second endpoint is dialled beside the first.
#[test]
fn a_walk_dials_a_seat_at_one_endpoint_at_a_time() {
    for mutant in [false, true] {
        let mut hints = hints();
        let start = Instant::now();
        let seat = [9; 48];
        let ends: [SocketAddr; 2] = ["10.1.1.1:1234", "10.1.2.1:1234"].map(|a| a.parse().unwrap());
        for address in ends {
            hints.observe(&wire(address.port()), address, start);
            assert_eq!(hints.next(start).unwrap().0, address);
            let quiet = Outcome::Repaired { transferred: false };
            hints.finish(address, Repair::heard_from(seat), quiet, start);
        }
        let now = start + Duration::from_secs(1);
        hints.moved(Subject::Control, None, [], now);
        assert_eq!(hints.next(now).unwrap().0, ends[0]);
        let beside = under(mutant.then_some(&TWIN_WALKED), || {
            hints.next(now).map(|(address, _)| address)
        });
        if mutant {
            assert_eq!(beside, Some(ends[1]), "MUTANT: both at once");
            continue;
        }
        assert_eq!(beside, None, "the seat's round is in flight");
        completes(&mut hints, (ends[0], seat), None, now, now);
        assert!(hints.next(now).is_none(), "the seat was tried");
        assert_eq!(hints.hot_count(), 0);
    }
}

/// A dial handed a moved table asks which tables differ once its endpoint's last completed
/// round ended two minutes ago by this transport's steady clock, whatever the wall clock says;
/// a sooner one does not ask.
/// MUTANT (`WALK_UNASKED`): it never asks.
#[test]
fn a_dial_handed_a_table_asks_after_two_minutes_without_a_round() {
    for (gap, mutant, asks) in [(119, false, false), (120, false, true), (120, true, false)] {
        let mut hints = hints();
        let start = Instant::now();
        let seat = seats(&mut hints, 1, start)[0];
        let now = start + Duration::from_secs(gap);
        hints.moved(Subject::Table([7; 32]), None, [], now);
        let (address, repair) = under(mutant.then_some(&WALK_UNASKED), || hints.next(now).unwrap());
        assert!(address == seat.0 && repair.pending());
        assert_eq!(
            repair.asked(),
            asks,
            "{gap} s after its last round, MUTANT {mutant}"
        );
    }
}

/// The serving loop is woken for a walk's dial: while a walk has a seat to dial, the next dial
/// is due now, though every endpoint's own wait is still to run; once the walk is over it is
/// those waits again.
/// MUTANT (`WALK_UNTIMED`): the next dial is due at the earliest of those waits, so a walk's
/// dial comes late.
#[test]
fn the_next_dial_is_due_now_while_a_walk_has_a_seat_to_dial() {
    let mut hints = hints();
    let start = Instant::now();
    let seats = seats(&mut hints, 2, start);
    let now = start + Duration::from_secs(1);
    let rest = hints.next_dial_at(now, false).unwrap();
    assert!(rest > now, "both seats rest");
    hints.moved(Subject::Control, None, [], now);
    for established_only in [true, false] {
        assert!(hints.next_dial_at(now, !established_only).unwrap() <= now);
    }
    let late = under(Some(&WALK_UNTIMED), || hints.next_dial_at(now, false));
    assert_eq!(late, Some(rest), "MUTANT: the walk's dial waits");
    for seat in &seats {
        assert_eq!(hints.next(now).unwrap().0, seat.0);
        completes(&mut hints, *seat, None, now, now);
    }
    assert!(hints.next(now).is_none());
    assert!(
        hints.next_dial_at(now, false).unwrap() > now,
        "the walk is over"
    );
}

/// A seat's dials over ten minutes, read once a second. Its one endpoint rests until second
/// 500, as a settled endpoint does between two audits, and for good after that dial; its
/// beacons are heard at the seconds `beacons` names, or it was never heard by beacon at all
/// (`heard` false: an endpoint this Mac remembered). Every dial completes quiet at once. The
/// second each dial began at with whether it was an audit, and whether one began before
/// [`Hints::next_dial_at`] said a dial was due.
fn dials_at_rest(
    heard: bool,
    beacons: impl Fn(u64) -> bool,
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> (Vec<(u64, bool)>, bool) {
    let mut hints = hints();
    let start = Instant::now();
    let seat = seats(&mut hints, 1, start)[0];
    let rest = |hints: &mut Hints, until: u64| {
        hints.peers.get_mut(&seat.0).unwrap().next = start + Duration::from_secs(until);
    };
    rest(&mut hints, 500);
    if !heard {
        hints.peers.get_mut(&seat.0).unwrap().beacon = None;
    }
    under(flag, || {
        let (mut dials, mut late) = (Vec::new(), false);
        for second in 0..=600 {
            let now = start + Duration::from_secs(second);
            if heard && beacons(second) {
                hints.observe(&wire(seat.0.port()), seat.0, now);
            }
            let due = hints.next_dial_at(now, false);
            if let Some((address, repair)) = hints.next_allowed(now, false) {
                assert_eq!(address, seat.0);
                late |= due.is_none_or(|due| due > now);
                dials.push((second, repair.audited()));
                completes(&mut hints, seat, None, now, now);
                rest(&mut hints, if second < 500 { 500 } else { 86_400 });
            }
        }
        (dials, late)
    })
}

/// A beacon dials nothing and puts off nothing: an endpoint whose beacons are heard every five
/// seconds is dialled when its own wait ends, at second 500, and at no other time; so is one no
/// beacon ever named. An endpoint that beaconed and then went silent is dialled once, 30 s
/// after it was last heard, as an audit; however long the silence then lasts no second probe
/// follows, and its own wait still dials it. A beacon heard again starts over: the next
/// silence is probed once too. The serving loop is woken for each: no dial begins before
/// `next_dial_at` said one was due.
/// MUTANT (`BEACON_DEFERS_AUDIT`): each beacon puts the endpoint's own dial off, and one that
/// keeps beaconing is never dialled. MUTANT (`PROBE_REPEATS`): a silence is probed again 30 s
/// after every round. MUTANT (`PROBE_UNTIMED`): the probe's dial begins with none said due.
#[test]
fn a_beacon_never_dials_or_defers_and_a_silence_is_probed_once() {
    let beaconing = |second: u64| second % 5 == 0;
    // Heard at seconds 0, 5 and 10, and again at 200.
    let silent = |second: u64| second <= 10 && second % 5 == 0 || second == 200;
    assert_eq!(
        dials_at_rest(true, beaconing, None),
        (vec![(500, true)], false),
        "its own wait, its audit, and no beacon's"
    );
    assert_eq!(
        dials_at_rest(false, beaconing, None),
        (vec![(500, true)], false),
        "never heard by beacon: no silence to probe"
    );
    assert_eq!(
        dials_at_rest(true, silent, None),
        (vec![(40, true), (230, true), (500, false)], false),
        "one probe a silence, each an audit, then its own wait"
    );
    assert_eq!(
        dials_at_rest(true, beaconing, Some(&BEACON_DEFERS_AUDIT)).0,
        [],
        "MUTANT: beacons put its own dial off for ever"
    );
    let repeats = dials_at_rest(true, silent, Some(&PROBE_REPEATS)).0;
    assert_eq!(
        repeats[..4],
        [(40, true), (70, true), (100, true), (130, true)],
        "MUTANT: the silence is probed again and again"
    );
    assert_eq!(
        dials_at_rest(true, silent, Some(&PROBE_UNTIMED)),
        (vec![(40, true), (230, true), (500, false)], true),
        "MUTANT: a probe begins with no dial said due"
    );
}

/// Each endpoint's probe waits 30 s and up to 10 s more by this node's private draw, so the
/// Macs that stop hearing one peer do not all dial it in the same instant: under one seed
/// every endpoint's wait is within that range and they are not all one, and another node's
/// seed draws another wait for the same endpoint.
#[test]
fn a_probe_waits_out_the_silence_and_this_nodes_own_jitter() {
    let waits = |seed: u8| -> Vec<Duration> {
        let mut hints = Hints::tagged([1; 32], [2; 32], [seed; 32]);
        let start = Instant::now();
        (1..=20)
            .map(|n| {
                let address = SocketAddr::from(([10, 1, 1, n], 1234));
                hints.observe(&wire(address.port()), address, start);
                let (chosen, _) = hints.next(start).unwrap();
                let quiet = Outcome::Repaired { transferred: false };
                hints.finish(chosen, Repair::heard_from([n; 48]), quiet, start);
                assert_eq!(hints.peers[&chosen].beacon, Some(start));
                hints.probe_at(&chosen, &hints.peers[&chosen]).unwrap() - start
            })
            .collect()
    };
    let (one, other) = (waits(3), waits(4));
    for wait in one.iter().chain(&other) {
        assert!(
            (Duration::from_secs(30)..Duration::from_secs(40)).contains(wait),
            "{wait:?}"
        );
    }
    assert!(one.iter().any(|wait| *wait != one[0]), "{one:?}");
    assert_ne!(one, other, "two nodes draw apart");
}

/// How one dial of an endpoint ends, as the departure rules read it.
#[derive(Clone, Copy, Debug)]
enum Dial {
    /// Its host refused the connection ([`Repair::disowned`]).
    Refuses,
    /// It reached no peer at the address ([`Outcome::Unreached`]).
    NoRoute,
    /// It failed without saying whether a peer is there ([`Outcome::Refused`]).
    Stalls,
    /// This side never asked the peer ([`Outcome::Unasked`]).
    Held,
    /// The peer authenticated and the round did not complete ([`Outcome::Busy`]).
    Busy,
    /// Something took this side's hello and answered nothing ([`Repair::unanswered`]).
    Unanswered,
    /// This side could not start the round ([`Outcome::Busy`], nothing sent).
    Unstarted,
    /// The peer authenticated and the round completed.
    Completes,
}

/// One dial of `seat`'s endpoint at `now`, whatever its backoff and with every other endpoint
/// at rest, that ends as `dial` a second later: the departures the scheduler then tells.
fn dial_ends(
    hints: &mut Hints,
    seat: (SocketAddr, Actor),
    dial: Dial,
    now: Instant,
) -> Vec<(Actor, Instant)> {
    for (address, peer) in &mut hints.peers {
        peer.next = if *address == seat.0 {
            now
        } else {
            now + Duration::from_secs(3600)
        };
    }
    let (address, mut repair) = hints.next(now).unwrap();
    assert_eq!(address, seat.0);
    // As its worker begins the round: what the last round authenticated is not this one's.
    repair.begin_round();
    let (repair, outcome) = match dial {
        Dial::Refuses => (nothing_there(repair), Outcome::Refused),
        Dial::NoRoute => (repair, Outcome::Unreached),
        Dial::Stalls => (repair, Outcome::Refused),
        Dial::Held => (repair, Outcome::Unasked),
        Dial::Busy => (Repair::heard_from(seat.1), Outcome::Busy),
        Dial::Unanswered => (repair.hello_unanswered(), Outcome::Busy),
        Dial::Unstarted => (repair, Outcome::Busy),
        Dial::Completes => (
            Repair::heard_from(seat.1),
            Outcome::Repaired { transferred: false },
        ),
    };
    hints.finish(address, repair, outcome, now + Duration::from_secs(1));
    hints.take_departures()
}

/// Which of `dials`, made ten seconds apart to one established seat's only endpoint, told the
/// seat gone; each that did named the seat and the instant its dial began.
fn tells(
    dials: &[Dial],
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> Vec<bool> {
    under(flag, || {
        let mut hints = hints();
        let start = Instant::now();
        let seat = seats(&mut hints, 1, start)[0];
        (1u64..)
            .zip(dials)
            .map(|(n, dial)| {
                let now = start + Duration::from_secs(10 * n);
                let told = dial_ends(&mut hints, seat, *dial, now);
                assert!(
                    told.is_empty() || told == [(seat.1, now)],
                    "the seat, at the instant the dial began"
                );
                !told.is_empty()
            })
            .collect()
    })
}

/// When a dial that fails tells its seat gone. A dial that reached no peer tells it at once: a
/// refused connection, or no answer or route at the address. One that failed without saying
/// whether a peer is there tells nothing, and a second in a row tells it. A round this side
/// never asked (a held LAN, a stopped round) or could not start tells nothing and counts for
/// nothing; a round the peer authenticated in, completed or not, shows it there and ends a
/// doubt, and a hello that was taken and went unanswered ends a doubt and shows no seat. A seat
/// is told gone once: later failures of the endpoint tell nothing until a round authenticates
/// the seat there again.
/// MUTANT (`ANY_FAILURE_DEPARTS`): every dial that does not complete tells it.
#[test]
fn a_seat_is_told_gone_by_a_decisive_failure_or_the_second_ambiguous_one() {
    use Dial::{Busy, Completes, Held, NoRoute, Refuses, Stalls, Unanswered, Unstarted};
    let cases: [(&[Dial], &[bool], &[bool]); 12] = [
        (&[Refuses], &[true], &[true]),
        (&[NoRoute], &[true], &[true]),
        (
            &[Stalls, Stalls, Stalls, NoRoute],
            &[false, true, false, false],
            &[true, true, true, true],
        ),
        (
            &[Stalls, Completes, Stalls, Stalls],
            &[false, false, false, true],
            &[true, false, true, true],
        ),
        (
            &[Held, Held, NoRoute],
            &[false, false, true],
            &[true, true, true],
        ),
        (
            &[Stalls, Held, Stalls],
            &[false, false, true],
            &[true, true, true],
        ),
        (
            &[Stalls, Busy, Stalls, Stalls],
            &[false, false, false, true],
            &[true, true, true, true],
        ),
        (&[Busy, Refuses], &[false, true], &[true, true]),
        (
            &[Stalls, Unanswered, Stalls, Stalls],
            &[false, false, false, true],
            &[true, true, true, true],
        ),
        (
            &[Refuses, Unanswered, Refuses],
            &[true, false, false],
            &[true, true, true],
        ),
        (
            &[Stalls, Unstarted, Stalls],
            &[false, false, true],
            &[true, true, true],
        ),
        (
            &[Refuses, Refuses, Completes, Refuses],
            &[true, false, false, true],
            &[true, true, false, true],
        ),
    ];
    for (dials, told, mutant) in cases {
        assert_eq!(tells(dials, None), told, "{dials:?}");
        assert_eq!(
            tells(dials, Some(&ANY_FAILURE_DEPARTS)),
            mutant,
            "MUTANT: {dials:?}"
        );
    }
}

/// A seat with two endpoints here (a Mac that came back on a new port, or on two attachments)
/// is not told gone while one of them stands: its dead old port refuses and tells nothing, and
/// the seat is told gone only when its live endpoint fails too. An endpoint no round ever
/// authenticated a seat at tells nothing, whatever its dials come to.
/// MUTANT (`ANY_FAILURE_DEPARTS`): the dead old port tells the live seat gone.
#[test]
fn a_dead_old_port_beside_a_live_endpoint_tells_nothing() {
    for mutant in [false, true] {
        under(mutant.then_some(&ANY_FAILURE_DEPARTS), || {
            let mut hints = hints();
            let start = Instant::now();
            let actor = [1; 48];
            let [old, new, stranger]: [SocketAddr; 3] =
                ["10.1.1.1:1234", "10.1.1.1:1235", "10.1.1.2:1234"].map(|a| a.parse().unwrap());
            for address in [old, new] {
                hints.observe(&wire(address.port()), address, start);
                let (chosen, _) = hints.next(start).unwrap();
                assert_eq!(chosen, address);
                let quiet = Outcome::Repaired { transferred: false };
                hints.finish(address, Repair::heard_from(actor), quiet, start);
            }
            let now = start + Duration::from_secs(10);
            let told = dial_ends(&mut hints, (old, actor), Dial::Refuses, now);
            assert_eq!(
                !told.is_empty(),
                mutant,
                "MUTANT {mutant}: the old port told"
            );
            let later = now + Duration::from_secs(10);
            assert_eq!(
                dial_ends(&mut hints, (new, actor), Dial::Refuses, later),
                [(actor, later)],
                "its last endpoint"
            );
            hints.observe(&wire(stranger.port()), stranger, later);
            for dial in [Dial::Refuses, Dial::NoRoute, Dial::Stalls, Dial::Stalls] {
                assert_eq!(
                    dial_ends(&mut hints, (stranger, [9; 48]), dial, later),
                    [],
                    "{dial:?}: no seat was ever there"
                );
            }
        });
    }
}

/// What a dial came to tells a peer that is not there from a round this side never asked. Of a
/// round that reached nobody's answer: one this side stopped, or never sent because this Mac's
/// LAN is held, asked nothing; one whose dial reached no peer says so; any other failure stays
/// ambiguous. A round the peer answered, or that completed, is what it was.
#[test]
fn a_refused_round_is_told_apart_by_what_its_dial_came_to() {
    use aterm_types::network_state::RepairCause::{Io, LanHeld, Other, Unreachable};
    let quiet = Outcome::Repaired { transferred: false };
    for (outcome, cause, cancelled, told) in [
        (
            Outcome::Refused,
            Some(Unreachable),
            false,
            Outcome::Unreached,
        ),
        (Outcome::Refused, Some(LanHeld), false, Outcome::Unasked),
        (Outcome::Refused, Some(Unreachable), true, Outcome::Unasked),
        (Outcome::Refused, Some(Io), true, Outcome::Unasked),
        (Outcome::Refused, Some(Io), false, Outcome::Refused),
        (Outcome::Refused, Some(Other), false, Outcome::Refused),
        (Outcome::Refused, None, false, Outcome::Refused),
        (Outcome::Busy, Some(Unreachable), false, Outcome::Busy),
        (Outcome::Busy, Some(LanHeld), true, Outcome::Busy),
        (Outcome::Deferred, None, false, Outcome::Deferred),
        (quiet, None, true, quiet),
    ] {
        assert_eq!(
            outcome.dialled(cause, cancelled),
            told,
            "{outcome:?}, {cause:?}, cancelled {cancelled}"
        );
    }
}

/// What four endpoints read once what their rests stood on moved at second 130: the seconds
/// after the start at which the first three are then due, whether the first's next dial and
/// the second's and third's next rounds are audits, and whether the fourth's backoff stands.
/// All four rested until second 500, and no beacon times a probe of any. The first's last
/// round ended at second 0 and the second's at second 100; the third's round is in flight and
/// ends at second 132; the fourth failed its last dial.
fn unsettled(
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
) -> ([u64; 3], [bool; 3], bool) {
    let mut hints = hints();
    let start = Instant::now();
    let after = |second: u64| start + Duration::from_secs(second);
    let seats = seats(&mut hints, 4, start);
    for peer in hints.peers.values_mut() {
        peer.next = after(500);
        peer.beacon = None;
    }
    let dial = |hints: &mut Hints, seat: usize, second: u64| {
        hints.peers.get_mut(&seats[seat].0).unwrap().next = after(second);
        let (address, repair) = hints.next(after(second)).unwrap();
        assert_eq!(address, seats[seat].0);
        repair
    };
    dial(&mut hints, 1, 100);
    completes(&mut hints, seats[1], None, after(100), after(100));
    hints.peers.get_mut(&seats[1].0).unwrap().next = after(500);
    let failed = dial(&mut hints, 3, 128);
    hints.finish(seats[3].0, failed, Outcome::Refused, after(128));
    let backoff = hints.next_at(seats[3].0);
    dial(&mut hints, 2, 129);
    under(flag, || hints.unsettle(after(130), Duration::ZERO));
    let due = |hints: &Hints, seat: usize| (hints.next_at(seats[seat].0) - start).as_secs();
    let held = |hints: &Hints, seat: usize| hints.peers[&seats[seat].0].repair.audited();
    let (first, second) = (due(&hints, 0), due(&hints, 1));
    let first_audits = if first == 130 {
        // The serving loop is woken for it, and its dial is the audit.
        assert!(hints.next_dial_at(after(130), false).unwrap() <= after(130));
        let (address, repair) = hints.next(after(130)).unwrap();
        assert_eq!(address, seats[0].0);
        repair.audited()
    } else {
        held(&hints, 0)
    };
    completes(&mut hints, seats[2], None, after(129), after(132));
    (
        [first, second, due(&hints, 2)],
        [first_audits, held(&hints, 1), held(&hints, 2)],
        hints.next_at(seats[3].0) == backoff,
    )
}

/// When a link of this Mac's opens, or it wakes, every established endpoint is looked at
/// again, whatever it rested on: one whose last round is a minute old or more is due at once,
/// one whose round is younger once that round is a minute old, and one whose round was in
/// flight a minute after it ends at the latest. Each one's next round is an audit. An endpoint
/// that is not established keeps its backoff.
/// MUTANT (`STAYS_SETTLED`): each keeps its rest and no audit is asked.
#[test]
fn a_changed_network_makes_every_established_endpoint_due_an_audit_within_a_minute() {
    let (due, audits, backoff_stands) = unsettled(None);
    assert_eq!(due[..2], [130, 160], "now, and a minute after its round");
    assert!(
        (133..=192).contains(&due[2]),
        "a minute after its round's end at the latest: {}",
        due[2]
    );
    assert_eq!(audits, [true; 3]);
    assert!(backoff_stands);
    let (due, audits, backoff_stands) = unsettled(Some(&STAYS_SETTLED));
    assert_eq!(due[..2], [500, 500], "MUTANT: each rests on");
    assert_eq!(audits, [false; 3], "MUTANT: no audit");
    assert!(backoff_stands);
}

/// Whether each of fourteen dials among three resting seats was handed the lab's catalog to
/// lead with, in order; every instant is named and no beacon times a probe. The catalog is
/// `[5; 32]`.
/// 1. Second 10: an ordinary dial, no round over the catalog ever heard.
/// 2. Second 20: the first seat's audit.
/// 3. Second 21: the second seat's audit, the first's round still in flight. The first's round
///    was over the catalog and left nothing to bring.
/// 4. Second 319: an audit 299 s after that round began.
/// 5. Second 320: an audit 300 s after it. Its round over the catalog brings records.
/// 6. Second 330: the next audit. Its dial fails.
/// 7. Second 340: the next audit. Its round over the catalog leaves nothing. Then three rounds
///    that began earlier end: one over the catalog that left nothing, one over it that brought
///    records, and one over another table that left nothing.
/// 8. Second 350: an audit ten seconds on. Its round, over the catalog on the scan's own turn,
///    brings records.
/// 9. Second 360: the next audit. Its round over the catalog leaves nothing.
/// 10. Second 659 and 11. second 660: audits 299 s and 300 s after that. The second's round
///     over the catalog brings records.
/// 12. Second 670: an audit that is also the dial a moved table is handed to.
/// 13. Second 680: the audit after it.
/// 14. Second 690: an audit on a transport that takes no Commons tables.
fn catalog_leads(flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>) -> Vec<bool> {
    const CATALOG: Digest = [5; 32];
    const MOVED: Digest = [7; 32];
    let mut hints = hints();
    hints.keep_catalog(Some(CATALOG));
    let start = Instant::now();
    let after = |second: u64| start + Duration::from_secs(second);
    let seats = seats(&mut hints, 3, start);
    for peer in hints.peers.values_mut() {
        peer.next = after(86_400);
        peer.beacon = None;
    }
    // The dial of `seat` at `second`, an audit where one was asked of it first.
    let dial = |hints: &mut Hints, seat: usize, second: u64, audit: bool| {
        let peer = hints.peers.get_mut(&seats[seat].0).unwrap();
        peer.next = after(second);
        if audit {
            peer.repair.audit();
        }
        let (address, repair) = hints.next_allowed(after(second), false).unwrap();
        assert_eq!(address, seats[seat].0);
        assert_eq!(repair.audited(), audit);
        repair
    };
    // The round `seat` was dialled for at `began` completes quiet, a second on.
    let quiet = |hints: &mut Hints, seat: usize, began: u64| {
        completes(hints, seats[seat], None, after(began), after(began + 1));
        hints.peers.get_mut(&seats[seat].0).unwrap().next = after(86_400);
    };
    under(flag, || {
        let mut led = Vec::new();
        led.push(dial(&mut hints, 0, 10, false).leads(&CATALOG));
        quiet(&mut hints, 0, 10);
        led.push(dial(&mut hints, 0, 20, true).leads(&CATALOG));
        led.push(dial(&mut hints, 1, 21, true).leads(&CATALOG));
        quiet(&mut hints, 1, 21);
        // As the transport tells a round over a table that said something of it.
        hints.told(CATALOG, after(20), true);
        quiet(&mut hints, 0, 20);
        led.push(dial(&mut hints, 1, 319, true).leads(&CATALOG));
        quiet(&mut hints, 1, 319);
        led.push(dial(&mut hints, 2, 320, true).leads(&CATALOG));
        hints.told(CATALOG, after(320), false);
        quiet(&mut hints, 2, 320);
        let failing = dial(&mut hints, 1, 330, true);
        led.push(failing.leads(&CATALOG));
        hints.finish(seats[1].0, failing, Outcome::Refused, after(331));
        led.push(dial(&mut hints, 2, 340, true).leads(&CATALOG));
        hints.told(CATALOG, after(340), true);
        quiet(&mut hints, 2, 340);
        // Rounds end out of the order they began in, and another table is not the catalog.
        hints.told(CATALOG, after(100), true);
        hints.told(CATALOG, after(100), false);
        hints.told([6; 32], after(600), true);
        if flag.is_none() {
            assert_eq!(hints.catalog_heard(), Some(after(340)));
        }
        led.push(dial(&mut hints, 2, 350, true).leads(&CATALOG));
        hints.told(CATALOG, after(350), false);
        quiet(&mut hints, 2, 350);
        if flag.is_none() {
            assert_eq!(hints.catalog_heard(), None, "a round found more");
        }
        led.push(dial(&mut hints, 2, 360, true).leads(&CATALOG));
        hints.told(CATALOG, after(360), true);
        quiet(&mut hints, 2, 360);
        led.push(dial(&mut hints, 2, 659, true).leads(&CATALOG));
        quiet(&mut hints, 2, 659);
        led.push(dial(&mut hints, 2, 660, true).leads(&CATALOG));
        hints.told(CATALOG, after(660), false);
        quiet(&mut hints, 2, 660);
        hints.moved(Subject::Table(MOVED), None, [], after(670));
        let handed = dial(&mut hints, 2, 670, true);
        assert!(
            handed.leads(&MOVED),
            "the moved table is the dial's one table"
        );
        led.push(handed.leads(&CATALOG));
        quiet(&mut hints, 2, 670);
        led.push(dial(&mut hints, 2, 680, true).leads(&CATALOG));
        quiet(&mut hints, 2, 680);
        hints.keep_catalog(None);
        led.push(dial(&mut hints, 2, 690, true).leads(&CATALOG));
        led
    })
}

/// An audit leads the lab's catalog once no round over it has left nothing to bring, with any
/// seat, for five minutes, or ever, or since a round over it that brought or deferred records:
/// one audit at a time, and again at the next audit while the rounds it led found more or
/// failed. A round that left nothing counts from when it began, whichever side dialled it and
/// whenever it ended, and a round that found more unsettles only one that began no later; a
/// round over another table does neither. An ordinary dial is handed nothing however stale
/// the catalog is, a dial handed a moved table keeps to that one table, and a transport that
/// takes no Commons tables leads none.
/// MUTANT (`CATALOG_UNLED`): no audit leads the catalog. MUTANT (`CATALOG_EVERY_AUDIT`): every
/// audit does, beside one in flight that carries it and within five minutes of a round over it.
///
/// NEGATIVE CONTROLS (2026-11-12, each by an inverse edit then undone, each failing the first
/// list below or the `catalog_heard` reading it names): an ordinary dial handed the catalog;
/// a dial handed a moved table handed the catalog too; `finish` leaving its endpoint's audit
/// carrying the catalog; a round over any table read as one over the catalog, and the newest
/// told round taken whenever it began (both `Some(after(340))`); a round that found more
/// leaving the catalog heard (`a round found more`), and one unsettling a round that began
/// after it (`Some(after(340))`); and an audit at exactly five minutes handed nothing.
#[test]
fn an_audit_leads_the_catalog_once_no_round_over_it_left_nothing_for_five_minutes() {
    const T: bool = true;
    const F: bool = false;
    assert_eq!(
        catalog_leads(None),
        [F, T, F, F, T, T, T, F, T, F, T, F, T, F]
    );
    assert_eq!(
        catalog_leads(Some(&CATALOG_UNLED)),
        [F; 14],
        "MUTANT: the catalog waits for the scan's turn"
    );
    assert_eq!(
        catalog_leads(Some(&CATALOG_EVERY_AUDIT)),
        [F, T, T, T, T, T, T, T, T, T, T, F, T, F],
        "MUTANT: every audit carries the catalog"
    );
}

/// One Mac's own rounds on a virtual clock, with more endpoints than its places serve at once:
/// 64 verified peers, two rounds at a time, each ten seconds long and moving nothing, a dial
/// each 250 ms at most. The target's rounds each complete an audit, so it rests between them;
/// the 63 others never finish their first sweep (their table scan never takes its turn at every
/// table), so each is due again at the ordinary interval and the ordinary scan comes round to an
/// endpoint only every 320 s or so. Before each pick, `dials_in(since, reaches)` says whether
/// the target's seat completes a quiet round dialled in then ([`Hints::vouch`]); `reaches` says
/// that a place is free and the scan's next endpoint is the target. When each of the target's
/// own rounds completed, from the start, or the first tick at which an endpoint was dropped.
fn place_bound_rounds(
    span: Duration,
    mut dials_in: impl FnMut(Duration, bool) -> bool,
) -> Result<Vec<Duration>, String> {
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
    // Their beacons are not what this law is of: one heard and then silence would call for each
    // silence's probe ([`Hints::probe_at`]), which live peers' beacons put off for good.
    for peer in hints.peers.values_mut() {
        peer.beacon = None;
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
            let seat = Repair::heard_from(actor(*address));
            let repair = if *address == target {
                completed.push(since);
                seat.swept(4).applied(audited_round([1; 32]))
            } else {
                seat.applied(crate::replica::Report::default())
            };
            hints.finish(*address, repair, quiet, now);
            false
        });
        let free = running.len() < 2;
        if dials_in(since, free && hints.cursor == Some(before)) {
            hints.vouch(target.ip(), actor(target), now);
        }
        if free && let Some((address, _)) = hints.next(now) {
            running.push((address, now + ROUND));
        }
        if hints.count() != usize::from(ENDPOINTS) {
            return Err(format!(
                "an endpoint was dropped at {since:?}; the target's rounds completed at \
                 {completed:?}"
            ));
        }
    }
    Ok(completed)
}

/// The audit's bound where the places are full, over forty virtual minutes
/// ([`place_bound_rounds`]): the scan takes 320 s to come round the 63 endpoints still sweeping,
/// longer than the 120 s between [`REVERIFY`] and [`VERIFIED_EXPIRES`]. Whether the target's
/// seat dials in just as the scan reaches its endpoint or every two seconds, its own rounds
/// complete an audit's rest apart, no further apart than [`REVERIFY`], a place's wait and the
/// round itself, and no endpoint is ever dropped: an endpoint whose audit is due goes before the
/// ordinary scan ([`Hints::next_allowed`]). Main's law of the same name, kept with its second
/// negative control; its first planted a defect in a quiet round dialled in holding the
/// endpoint back, which the rest replaced, and the rest's own bound is held by
/// `a_settled_endpoint_is_dialled_once_an_audit_whoever_dials_in`.
///
/// NEGATIVE CONTROL (run here): with `AUDIT_UNPICKED`, the target's audit waits for the scan's
/// turn behind the sweeping endpoints, and its rounds come further apart than the bound or it
/// is dropped.
#[test]
fn a_full_macs_endpoint_is_verified_again_before_it_expires_whoever_dials_in() {
    let span = Duration::from_secs(2400);
    let bound = REVERIFY + 2 * Duration::from_secs(10);
    let held = |rounds: &Result<Vec<Duration>, String>| {
        rounds.as_ref().is_ok_and(|rounds| {
            rounds.len() >= 5
                && rounds
                    .windows(2)
                    .map(|pair| pair[1] - pair[0])
                    .all(|gap| gap >= AUDIT_FLOOR && gap <= bound)
        })
    };
    let at_its_turn = place_bound_rounds(span, |_, reaches| reaches);
    let always_first = place_bound_rounds(span, |since, _| since.as_millis() % 2000 == 0);
    for rounds in [at_its_turn, always_first] {
        assert!(
            held(&rounds),
            "its own round at every audit, an audit's rest apart within the bound, and no \
             endpoint dropped: {rounds:?}"
        );
    }
    let unpicked = under(Some(&AUDIT_UNPICKED), || {
        place_bound_rounds(span, |_, reaches| reaches)
    });
    assert!(
        !held(&unpicked),
        "MUTANT: an audit left to the scan's turn: {unpicked:?}"
    );
}
