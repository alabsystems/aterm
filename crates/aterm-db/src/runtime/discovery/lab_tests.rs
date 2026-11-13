// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CR2: a Mac that remembers nothing finds its own lab's endpoints first on a crowded LAN, by the
//! tag its beacons draw with each nonce ([`LabKey`]), each node in its own order
//! ([`Hints::spread`]).

use super::*;

/// This lab's key, and another's: any two labs whose discriminators differ.
const LAB: LabKey = [0x1a; 32];
const OTHER: LabKey = [0x2b; 32];

/// A fresh node of [`LAB`], drawn from `nonce`, its own order drawn from it too.
fn fresh(nonce: u8) -> Hints {
    Hints::tagged(LAB, [nonce; 32], [nonce; 32])
}

/// A beacon of a node of the lab `key` (none: an older build's whole random token) at `port`.
fn beacon(key: Option<LabKey>, nonce: u8, port: u16) -> Vec<u8> {
    Hints::of(key, [nonce; 32]).wire(port).to_vec()
}

/// What a round leaves when nothing accepted its connection ([`Repair::disowned`]).
fn nothing_there(mut repair: Repair) -> Repair {
    repair.disown();
    repair
}

/// Run `body` with the mutant `cell` set to `on` for this thread, cleared after.
fn with<R>(
    cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    on: bool,
    body: impl FnOnce() -> R,
) -> R {
    cell.with(|mutant| mutant.set(on));
    let out = body();
    cell.with(|mutant| mutant.set(false));
    out
}

/// A lab's beacon key is its discriminator's: each of the three trust pins moves both, so equal
/// keys are what equal TLS discriminators are, and unequal ones name a lab no round reaches.
#[test]
fn beacon_keys_follow_the_discriminator_pin_by_pin() {
    use crate::control::{beacon_key, discriminator};
    use aterm_secrets::commons::journal::Trust;
    let trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    assert_eq!(beacon_key(&trust), beacon_key(&trust));
    assert_ne!(beacon_key(&trust), discriminator(&trust), "its own label");
    for changed in [
        Trust {
            network: [9; 16],
            ..trust
        },
        Trust {
            authority: [9; 32],
            ..trust
        },
        Trust {
            genesis: [9; 32],
            ..trust
        },
    ] {
        assert_ne!(discriminator(&changed), discriminator(&trust));
        assert_ne!(beacon_key(&changed), beacon_key(&trust));
    }
}

/// A tagged beacon is the old wire: `ACH2`, 38 bytes, its tag inside the token, so a build that
/// predates CR2 reads it as an opaque token and dials it in its crowd, and a tagged node reads an
/// older build's beacon as naming no lab and dials it in the crowd too. A rotation draws a new
/// tag, so nobody without the lab's key links one beacon to the next, while the node still knows
/// its last token as its own and its lab still reads the new one as its own.
#[test]
fn a_tagged_beacon_keeps_the_old_wire_and_draws_a_new_tag_each_rotation() {
    let now = Instant::now();
    let mut node = fresh(1);
    let wire = node.wire(4000);
    assert_eq!((wire.len(), &wire[..4]), (WIRE_LEN, b"ACH2".as_slice()));
    assert_eq!(&wire[4..8], LAB_MARK);
    let mut older = Hints::new([7; 32]);
    let address: SocketAddr = "10.1.1.2:4000".parse().unwrap();
    older.observe(&wire, address, now);
    assert_eq!(
        older.next(now).map(|(a, _)| a),
        Some(address),
        "an older build dials it"
    );
    let old: SocketAddr = "10.1.1.3:4000".parse().unwrap();
    node.observe(&older.wire(4000), old, now);
    assert_eq!(
        node.peers[&old].lead(),
        Lead::None,
        "an older build's beacon names no lab"
    );

    let before = node.wire(4000);
    node.rotate([2; 32]);
    let after = node.wire(4000);
    assert_eq!(&after[4..8], LAB_MARK);
    assert_ne!(&after[8..16], &before[8..16], "a new nonce draws a new tag");
    node.observe(&before, "10.1.1.4:1".parse().unwrap(), now);
    node.observe(&after, "10.1.1.4:1".parse().unwrap(), now);
    assert_eq!(
        node.count(),
        1,
        "its own beacons, last token and new, are not peers"
    );
    let mut kin = fresh(3);
    let mut stranger = Hints::tagged(OTHER, [4; 32], [4; 32]);
    for (n, wire) in [before, after].iter().enumerate() {
        let from = SocketAddr::from(([10, 1, 1, 5 + n as u8], 4000));
        kin.observe(wire, from, now);
        assert_eq!(
            kin.peers[&from].lead(),
            Lead::Tagged,
            "its lab reads each tag"
        );
        stranger.observe(wire, from, now);
    }
    assert_eq!(stranger.count(), 0, "another lab dials neither");
}

/// CR2's first law: a fresh node, which remembers nothing, dials its own lab's endpoint before
/// 70 of another lab's (the lab LAN case, a lab node among the ALab once the ALab tags) and
/// before 20 that name no lab (older builds, some slow): another lab's are never candidates, and
/// another lab's enrollee's presence counts for nothing.
///
/// NEGATIVE CONTROL (run here, [`UNTAGGED`]): read as naming no lab, as before CR2, another lab's
/// endpoints are candidates, and the crowd's order puts one of them first.
#[test]
fn a_fresh_node_dials_its_own_labs_endpoint_before_another_labs_crowd() {
    let kin: SocketAddr = "10.1.9.9:4000".parse().unwrap();
    for untagged in [false, true] {
        let now = Instant::now();
        let mut node = fresh(1);
        let (candidates, unverified, first, second) = with(&UNTAGGED, untagged, || {
            for n in 1..=70 {
                let from = SocketAddr::from(([10, 1, 1, n], 1));
                node.observe(&beacon(Some(OTHER), n, 4000), from, now);
            }
            for n in 1..=20 {
                let from = SocketAddr::from(([10, 1, 2, n], 1));
                node.observe(&beacon(None, n, 4000), from, now);
            }
            node.observe_presence(
                &Hints::tagged(OTHER, [99; 32], [99; 32]).presence_wire(),
                "10.1.3.1:1".parse().unwrap(),
                now,
            );
            node.observe(&beacon(Some(LAB), 100, kin.port()), kin, now);
            let counts = (node.count(), node.unverified_count());
            let first = node.next(now).unwrap().0;
            let second = node.next(now).unwrap().0;
            (counts.0, counts.1, first, second)
        });
        if untagged {
            assert_eq!((candidates, unverified), (91, 92));
            assert_ne!(first, kin, "MUTANT: the crowd goes first");
            continue;
        }
        assert_eq!(first, kin, "its own lab's endpoint goes first");
        assert_eq!(candidates, 21, "another lab's endpoints are no candidates");
        assert_eq!(unverified, 21, "nor is another lab's enrollee");
        // An older build's endpoints keep their turns: they may be this lab's.
        assert_eq!(
            second.ip().to_string().rsplit_once('.').unwrap().0,
            "10.1.2"
        );
    }
}

/// A host running two trusts beacons both; a beacon tagged for the other from an address a peer
/// of this lab authenticated from neither leads nor spends that address.
///
/// NEGATIVE CONTROL (run here, [`UNTAGGED`]): read as naming no lab, it leads as kin.
#[test]
fn another_labs_beacon_from_a_peers_address_is_no_kin() {
    for untagged in [false, true] {
        let now = Instant::now();
        let mut node = fresh(1);
        let host: SocketAddr = "10.1.6.6:5000".parse().unwrap();
        node.vouch(host.ip(), [7; 48], false, now);
        with(&UNTAGGED, untagged, || {
            node.observe(&beacon(Some(OTHER), 5, host.port()), host, now);
        });
        if untagged {
            assert_eq!(
                node.peers[&host].lead(),
                Lead::Kin,
                "MUTANT: it leads as kin"
            );
            continue;
        }
        assert!(!node.peers.contains_key(&host));
        assert!(
            node.kin(host.ip(), now),
            "the address still vouches for this lab's beacons"
        );
    }
}

/// Anyone can replay a lab's tagged beacon from another address. A dial a tagged beacon led that
/// finds nothing listening spends its address: a new port tagged from there waits its turn with
/// the crowd until a peer authenticates from that address. A tagged dial that stalls (a busy peer
/// of this lab looks so) keeps its endpoint's lead and backs off, and spends its address too: once
/// that endpoint expires, neither it heard again nor a new port from there leads. A forged beacon
/// cannot hide a real peer: another lab's tag forged at its endpoint is dropped, and its own
/// tagged beacon raises an endpoint first heard naming no lab.
///
/// NEGATIVE CONTROLS (run here): [`UNSPENT`], every forged tagged port leads, and a stalled one
/// leads again each time it is heard after it expired; [`STALL_DISOWNS`], a stalled tagged dial
/// loses its endpoint's lead, as CR1 did.
#[test]
fn forged_tags_lead_one_dial_per_address_and_a_stall_keeps_its_lead() {
    let forged = |port: u16| SocketAddr::from(([10, 1, 9, 9], port));
    for unspent in [false, true] {
        let now = Instant::now();
        let mut node = fresh(1);
        for n in 1..=10 {
            let from = SocketAddr::from(([10, 1, 1, n], 1));
            node.observe(&beacon(None, n, 4000), from, now);
        }
        node.observe(&beacon(Some(LAB), 50, 2000), forged(2000), now);
        let (first, repair) = node.next(now).unwrap();
        assert_eq!(first, forged(2000), "a tagged beacon leads");
        with(&UNSPENT, unspent, || {
            node.finish(first, nothing_there(repair), Outcome::Refused, now);
        });
        node.observe(&beacon(Some(LAB), 51, 2001), forged(2001), now);
        if unspent {
            assert_eq!(
                node.peers[&forged(2001)].lead(),
                Lead::Tagged,
                "MUTANT: every forged tagged port leads"
            );
            continue;
        }
        assert_eq!(
            node.peers[&forged(2001)].lead(),
            Lead::None,
            "it waits its turn"
        );
        // A peer authenticates from there: its tagged ports lead again, as kin.
        node.vouch(forged(2001).ip(), [7; 48], false, now);
        assert_eq!(node.peers[&forged(2001)].lead(), Lead::Kin);

        // Forged beacons at a real peer's endpoint: another lab's tag is dropped, and one naming
        // no lab is raised by the peer's own tagged beacon.
        let real: SocketAddr = "10.1.8.8:3000".parse().unwrap();
        node.observe(&beacon(Some(OTHER), 60, real.port()), real, now);
        assert!(
            !node.peers.contains_key(&real),
            "another lab's tag admits nothing"
        );
        node.observe(&beacon(None, 61, real.port()), real, now);
        assert_eq!(node.peers[&real].lead(), Lead::None);
        node.observe(&beacon(Some(OTHER), 62, real.port()), real, now);
        node.observe(&beacon(Some(LAB), 63, real.port()), real, now);
        assert_eq!(
            node.peers[&real].lead(),
            Lead::Tagged,
            "its own beacon raises it"
        );
    }
    for (stall_disowns, unspent) in [(false, false), (true, false), (false, true)] {
        let now = Instant::now();
        let mut node = fresh(2);
        let busy: SocketAddr = "10.1.7.7:3000".parse().unwrap();
        node.observe(&beacon(Some(LAB), 70, busy.port()), busy, now);
        let (first, repair) = node.next(now).unwrap();
        assert_eq!(first, busy);
        // Its accept queue held the dial to the handshake limit: no peer authenticated, and
        // nothing says no peer of this lab is there.
        let later = now + Duration::from_secs(30);
        with(&STALL_DISOWNS, stall_disowns, || {
            with(&UNSPENT, unspent, || {
                node.finish(busy, repair, Outcome::Refused, later);
            });
        });
        let lead = node.peers[&busy].lead();
        if stall_disowns {
            assert_eq!(lead, Lead::None, "MUTANT: a stall drops the lead");
            continue;
        }
        assert_eq!(lead, Lead::Tagged, "a busy peer keeps its lead");
        assert!(node.peers[&busy].next > later, "and backs off");
        // Its endpoint expires, and is heard again; another port is heard from there.
        let again = now + EXPIRES + Duration::from_secs(1);
        let other = SocketAddr::from((busy.ip(), 3001));
        node.observe(&beacon(Some(LAB), 71, busy.port()), busy, again);
        node.observe(&beacon(Some(LAB), 72, other.port()), other, again);
        let leads = (node.peers[&busy].lead(), node.peers[&other].lead());
        if unspent {
            assert_eq!(
                leads,
                (Lead::Tagged, Lead::Tagged),
                "MUTANT: a stalled address leads each time it is heard"
            );
            continue;
        }
        assert_eq!(leads, (Lead::None, Lead::None), "they wait their turn");
    }
}

/// Seventy Macs that start together, each remembering nothing, do not all dial the same one of
/// their lab's endpoints first, nor the same one of an untagged crowd (the live ALab, still on
/// older builds): each draws its own order, so no Mac's accept queue takes every first dial while
/// the rest wait behind it.
///
/// NEGATIVE CONTROL (run here, [`UNSPREAD`]): in address order, every Mac dials the lowest first.
#[test]
fn seventy_fresh_macs_spread_their_first_dials() {
    for key in [Some(LAB), None] {
        for unspread in [false, true] {
            let now = Instant::now();
            let endpoints: Vec<SocketAddr> = (1..=70)
                .map(|n| SocketAddr::from(([10, 1, 4, n], 4000)))
                .collect();
            let firsts = with(&UNSPREAD, unspread, || {
                let mut firsts = BTreeMap::<SocketAddr, usize>::new();
                for mac in 1..=70u8 {
                    let mut node = fresh(mac);
                    for (n, address) in endpoints.iter().enumerate() {
                        let wire = beacon(key, n as u8 + 100, address.port());
                        node.observe(&wire, *address, now);
                    }
                    let mut order = Vec::new();
                    while let Some((address, _)) = node.next(now) {
                        order.push(address);
                    }
                    assert_eq!(order.len(), endpoints.len(), "every endpoint gets its turn");
                    *firsts.entry(order[0]).or_default() += 1;
                }
                firsts
            });
            let most = firsts.values().copied().max().unwrap();
            if unspread {
                assert_eq!(most, 70, "MUTANT: every Mac dials the same endpoint first");
                continue;
            }
            // Seventy draws over seventy endpoints: a handful at most land on one.
            assert!(
                most <= 6,
                "{most} of 70 Macs dial one endpoint first: {firsts:?}"
            );
            assert!(
                firsts.len() >= 30,
                "first dials land on only {} endpoints",
                firsts.len()
            );
        }
    }
}
