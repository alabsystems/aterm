// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAN admission without the heartbeat (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §10, §11 B6):
//! a member's node service with the mesh on notes each peer its links prove into the heard
//! ledger, and the vault's automatic LAN admission (`aterm_secrets::home_lan`) reads this Mac
//! home on that evidence alone, under the 7-day condition its first start seeded.

use aterm_net::fleet::Denied;
use aterm_net::fleet::link::LinkConfig;
use aterm_netplace::facts::Here;
use aterm_netplace::{NetworkId, RouteFacts};
use aterm_secrets::home_lan::{Away, Rule};
use std::net::IpAddr;

use super::*;

/// The home LAN's router, and the router of a network the place comes to remember later.
const LAN_HW: [u8; 6] = [0xa0, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5];
const LATER_HW: [u8; 6] = [0x02, 0x0e, 0x0e, 0x0e, 0x0e, 0x01];

const HOUR: u64 = 60 * 60_000;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// The member Mac's default route: Wi-Fi at 192.168.1.20/24 behind the router whose hardware
/// address is `hw`.
fn route(hw: [u8; 6]) -> RouteFacts {
    RouteFacts {
        interface: "en0".to_owned(),
        address: "192.168.1.20".parse().unwrap(),
        prefix: 24,
        gateway: Some("192.168.1.1".parse().unwrap()),
        gateway_hw: Some(hw.to_vec()),
    }
}

/// What the member Mac reads of the home LAN: the route, on a Wi-Fi path that is not expensive.
fn here() -> Here {
    Here {
        facts: aterm_netplace::Facts {
            route: Some(route(LAN_HW)),
            tunnel: None,
            path: Some(aterm_netplace::PathFacts {
                usable: Some(true),
                expensive: false,
                constrained: false,
                link: aterm_netplace::Link::Wifi,
            }),
        },
        point_to_point: false,
        own: vec![ip("127.0.0.1"), ip("192.168.1.20")],
    }
}

/// The network place under `state` as its keeper wrote it at `since`: a stay on the home LAN
/// since then, remembering `remembered` as the ALab LAN.
fn keep_place(state: &Path, since: u64, remembered: &[NetworkId]) {
    let (mut record, _) = aterm_netplace::step(
        &aterm_netplace::Record::default(),
        &here().facts,
        &aterm_netplace::Evidence::default(),
        None,
        since,
        Some(since),
    );
    record.remembered = remembered
        .iter()
        .map(|network| aterm_netplace::place::Known {
            network: network.clone(),
            learned_ms: since - 30 * 24 * HOUR,
            confirmed_ms: since,
        })
        .collect();
    aterm_netplace::record::write(
        &aterm_netplace::record::dir(state),
        &aterm_netplace::record::Written {
            revision: 1,
            keeper_pid: Some(std::process::id()),
            keeper_started_ms: Some(since),
            written_ms: since,
            record,
        },
    )
    .expect("the place's record");
}

/// A member's neighbour table, as another Mac holds it: `me` dialing the mesh, judging listeners
/// by a log that seats `seats`, serving no stream, on a clock of its own that never moves.
fn peer_table(me: &NodeIdentity, seats: &[&NodeIdentity]) -> Arc<Neighbours> {
    let denied = Arc::new(Denied {
        members: seats.iter().map(|seat| seat.key()).collect(),
        ..Denied::default()
    });
    let verdict: aterm_net::fleet::DenyFn = Arc::new(move || Arc::clone(&denied));
    Neighbours::new(
        me.clone().dialing_mesh(Some(Arc::clone(&verdict))),
        LinkConfig {
            verdict,
            serve: Arc::new(|incoming: aterm_net::fleet::link::Incoming| incoming.refuse("op")),
            note: Arc::new(|_| {}),
            clock: ManualClock::new(),
        },
    )
}

/// The member's link clock as wall milliseconds past [`T0`].
fn past(by: Duration) -> u64 {
    T0 + u64::try_from(by.as_millis()).unwrap()
}

/// A FRESH INSTANCE IS ADMITTED ON A HOME NETWORK WHOSE EVIDENCE COMES ONLY FROM `atm/1`
/// CONNECTIONS (§10, "LAN admission without the heartbeat"), on the injected clock: member M's
/// node service, the mesh on, starts on the home LAN its place remembers, and its first start
/// seeds that LAN's seven days. Three members' nodes dial M's one listener; no heartbeat runs. M
/// then reads itself home, under the 7-day condition, and holds a new Mac on the LAN as on the
/// home ALab LAN. Thirty minutes on, M's links have re-proved each peer every five minutes, and M
/// is home still. A second start credits nothing: a network the place came to remember between
/// the starts gets no days.
///
/// The seam injects the route facts M's sight reads (and the LAN addresses its loopback links
/// stand for), never the answers: each answer is a peer's real proof.
///
/// NEGATIVE CONTROL (in the test): once the peers answer no re-proof, M's evidence ages out, and
/// M reads `TooFew { heard: 0 }`. With the listener's noting hook in `serve_inner` removed, M
/// hears nobody and "home on its mesh links alone" fails; with `Heard::noting_then` noting no
/// re-proof, the look at `FRESH` after the handshakes fails ("round 2").
#[test]
fn a_member_reads_itself_home_on_its_mesh_links_alone_and_the_seed_runs_once() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay = crate::test_scratch::short_dir("node-mesh-heard");
    let state = root.path().join("m-state");
    let (_, id_m) = seed_fleet_node(&state);
    let peers: Vec<NodeIdentity> = (0..3)
        .map(|n| seed_id(&root.path().join(format!("p{n}-state"))))
        .collect();
    let out = seed_id(&root.path().join("out-state"));
    let founders = [
        (1, &id_m),
        (2, &peers[0]),
        (3, &peers[1]),
        (4, &peers[2]),
        (5, &out),
    ];
    let log = lab_log(&root.path().join("records"), &founders, 5);
    let lan = NetworkId::of(&route(LAN_HW)).expect("the LAN's id");
    keep_place(&state, T0 - HOUR, std::slice::from_ref(&lan));
    let start = || {
        MeshNode::start_seeing(
            root.path(),
            relay.path(),
            &state,
            (
                Arc::clone(&log),
                atpkg::store::Layout {
                    prefix: root.path().join("m-pkg"),
                },
            ),
            |clock| {
                Arc::new(ScriptedSight {
                    now: wall(clock),
                    route: Some(route(LAN_HW)),
                    ends: Some((ip("192.168.1.30"), ip("192.168.1.20"))),
                })
            },
        )
    };
    let days_now = || {
        let (days, passed) = aterm_netplace::heard::read_days(&aterm_netplace::record::dir(&state));
        assert_eq!(passed, 0, "the home-day ledger reads whole");
        days
    };
    let judge =
        |at: u64| aterm_secrets::home_lan::home_net_with(&state, here(), at, Rule { days: true });

    let mac_m = start();
    let days = days_now();
    assert_eq!(
        (days.seeded_ms, days.of(&lan).map(|entry| entry.credited)),
        (Some(T0), Some(aterm_secrets::home_lan::HOME_DAYS)),
        "the first start credits the remembered LAN its seven days"
    );
    assert_eq!(
        judge(T0),
        Err(Away::TooFew { heard: 0 }),
        "nobody heard yet"
    );

    // Three members' nodes dial M, one after another; M's end of each link reports its
    // handshake once its listener has noted the proof, so the notes never contend.
    let mut shook = Vec::new();
    let tables: Vec<Arc<Neighbours>> = peers
        .iter()
        .map(|peer| {
            let table = peer_table(peer, &[&id_m]);
            table
                .dial(mac_m.addr, DIAL_WITHIN)
                .expect("a member dials M's one listener");
            let proven = mac_m.note();
            assert_eq!(proven.by, ProvenBy::Handshake);
            shook.push(proven.peer.key);
            table
        })
        .collect();
    let mut want: Vec<_> = peers.iter().map(NodeIdentity::key).collect();
    shook.sort_unstable_by_key(|key| key.hex());
    want.sort_unstable_by_key(|key| key.hex());
    assert_eq!(shook, want);

    let home = judge(T0).expect("home on its mesh links alone");
    assert!(
        home.holds(ip("192.168.1.77")),
        "a new Mac on the LAN is on the home ALab LAN"
    );
    assert!(!home.holds(ip("10.0.0.77")));
    assert_eq!(
        days_now()
            .of(&lan)
            .and_then(|entry| entry.days.first())
            .map(|day| day.members.len()),
        Some(3),
        "today booked with the three members"
    );

    // Thirty minutes: M's timer re-proves each link every five minutes, and M stays home.
    for round in 1..=6u32 {
        mac_m.clock.advance(REPROVE_EVERY);
        for _ in 0..3 {
            assert_eq!(mac_m.note().by, ProvenBy::Reproof, "round {round}");
        }
        assert!(
            judge(past(REPROVE_EVERY * round)).is_ok(),
            "round {round}: home on its re-proofs"
        );
    }

    // NEGATIVE CONTROL: the peers answer no more re-proofs, and M's evidence ages out.
    for table in &tables {
        for link in table.links() {
            link.mute_reproofs();
        }
    }
    mac_m.clock.advance(REPROVE_EVERY);
    mac_m.clock.advance(REPROVE_EVERY);
    assert_eq!(
        judge(past(REPROVE_EVERY * 8) + 60_000),
        Err(Away::TooFew { heard: 0 }),
        "CONTROL: with no re-proof answered, the evidence ages out"
    );
    mac_m.end();

    // The place has come to remember a second network; a second start credits nothing.
    let later = NetworkId::of(&route(LATER_HW)).expect("the later network's id");
    keep_place(&state, T0 + HOUR, &[lan.clone(), later.clone()]);
    let again = start();
    let days = days_now();
    assert_eq!(
        (
            days.seeded_ms,
            days.of(&lan).map(|entry| entry.credited),
            days.of(&later).map(|entry| entry.credited),
        ),
        (Some(T0), Some(aterm_secrets::home_lan::HOME_DAYS), None),
        "a second start credits nothing"
    );
    again.end();
    for table in tables {
        table.stop();
    }
}
