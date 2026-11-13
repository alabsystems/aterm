// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, NodeIdentity};

const HANG: Duration = Duration::from_secs(60);

#[test]
fn ineligible_addresses_do_not_consume_the_physical_link_budget() {
    let mut addresses: Vec<IpAddr> = (1..=MAX_LINKS as u8)
        .map(|last| Ipv4Addr::new(10, 0, 0, last).into())
        .collect();
    addresses.push(Ipv4Addr::LOCALHOST.into());
    let selected = source_addresses(addresses);
    let bound = links(&selected, 0, Scope::Loopback);
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].0, Ipv4Addr::LOCALHOST);
}

#[test]
fn reply_budget_allows_concurrent_joiners_and_bounded_packet_loss_recovery() {
    let now = Instant::now();
    let source: IpAddr = Ipv4Addr::LOCALHOST.into();
    let first = request(&[1; NONCE]);
    let second = request(&[2; NONCE]);
    let mut window = ReplyWindow::new(now);
    assert!(window.admit(source, &first, now));
    assert!(
        window.admit(source, &second, now),
        "init and subscriber can ask together"
    );
    assert!(!window.admit(source, &first, now), "duplicate flood");
    assert!(
        window.admit(source, &first, now + RETRANSMIT),
        "lost response"
    );
    assert!(window.admit(source, &second, now + RETRANSMIT));
    assert!(
        !window.admit(source, &first, now + RETRANSMIT * 2),
        "per-source ceiling"
    );
    for last in 1..=MAX_PEERS - 4 {
        assert!(window.admit(Ipv4Addr::new(10, 0, 0, last as u8).into(), &first, now));
    }
    assert!(
        !window.admit(Ipv4Addr::new(10, 0, 1, 1).into(), &first, now),
        "global ceiling"
    );
    assert!(window.admit(source, &first, now + REFRESH));
}

#[test]
fn packets_are_bounded_versioned_and_bound_to_this_solicitation() {
    let nonce = [7; NONCE];
    let source: SocketAddr = "127.0.0.1:41385".parse().unwrap();
    let key = NodeKey::from_bytes([9; 32]);
    let ask = request(&nonce);
    let reply = response(&ask, 7100, key).unwrap();
    assert_eq!(
        hint(&reply, source, &nonce),
        Some(Hint {
            address: "127.0.0.1:7100".parse().unwrap(),
            key,
        })
    );
    assert!(
        hint(&reply, source, &[8; NONCE]).is_none(),
        "old or unsolicited reply"
    );
    assert!(hint(&reply[..RESPONSE - 1], source, &nonce).is_none());
    let mut extended = reply.to_vec();
    extended.push(0);
    assert!(hint(&extended, source, &nonce).is_none());
    assert!(response(&ask, 0, key).is_none());
    assert!(
        response(&reply, 7100, key).is_none(),
        "responses never elicit responses"
    );
    let mut unknown = ask;
    unknown[0] ^= 1;
    assert!(response(&unknown, 7100, key).is_none());

    let ready = announcement(&nonce, 1);
    assert!(ready_token(&ready).is_some());
    assert!(ready_token(&ready[..READY - 1]).is_none());
    let mut extended = ready.to_vec();
    extended.push(0);
    assert!(ready_token(&extended).is_none());
    assert!(ready_token(&ask).is_none());
    assert!(ready_token(&reply).is_none());
    assert!(hint(&ready, source, &nonce).is_none());
    assert!(response(&ready, 7100, key).is_none());
}

/// A later reply carries the original nonce. Starting another solicitation cannot
/// recover it, and no DNS or membership authority is involved in receiving it.
#[test]
fn a_native_solicitation_keeps_later_replies_after_its_first_batch() {
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let client = links(&[Ipv4Addr::LOCALHOST], member.port(), Scope::Loopback);
    let mut probe = Probe::on(client, HANG).unwrap();
    let until = Instant::now() + HANG;
    let ask = loop {
        aterm_uds::ready::any_until(&[member.as_fd()], until).unwrap();
        if let Some((wire, _)) = member.receive().unwrap()
            && wire.len() == REQUEST
        {
            break wire;
        }
        assert!(Instant::now() < until, "a native request must arrive");
    };
    member
        .send(&response(&ask, 7101, NodeKey::from_bytes([1; 32])).unwrap())
        .unwrap();
    let first = probe.next(until).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7101]
    );
    // Publish only after the consumer took its first batch. This models a second
    // member's reply while the caller authenticates the first, without a timing race.
    member
        .send(&response(&ask, 7102, NodeKey::from_bytes([2; 32])).unwrap())
        .unwrap();
    let next = probe.next(until).unwrap();
    assert_eq!(
        next.iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7102]
    );
}

#[test]
fn native_reply_continuation_rejects_duplicates_foreign_nonces_and_expired_proof() {
    let (mut probe, reply) = queued_test_probe();
    let until = Instant::now() + HANG;
    reply(7101, NodeKey::from_bytes([1; 32]));
    assert_eq!(probe.next(until).unwrap().len(), 1);
    reply(7101, NodeKey::from_bytes([9; 32]));
    let sender = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        GROUP,
        probe.active[0].1.port(),
        Scope::Loopback,
    )
    .unwrap();
    let mut foreign = probe.nonce;
    foreign[0] ^= 1;
    sender
        .send(&response(&request(&foreign), 7199, NodeKey::from_bytes([9; 32])).unwrap())
        .unwrap();
    sender.send(b"malformed").unwrap();
    reply(7102, NodeKey::from_bytes([2; 32]));
    let later = probe.next(until).unwrap();
    assert_eq!(
        later
            .iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7102]
    );
    reply(7103, NodeKey::from_bytes([3; 32]));
    assert!(
        probe.next(Instant::now()).unwrap().is_empty(),
        "a queued reply cannot renew the proof deadline"
    );
}

#[test]
fn native_reply_cardinality_is_global_across_batches() {
    let (mut probe, reply) = queued_test_probe();
    let until = Instant::now() + HANG;
    reply(7200, NodeKey::from_bytes([1; 32]));
    assert_eq!(probe.next(until).unwrap().len(), 1);
    for offset in 1..=MAX_PEERS {
        reply(7200 + offset as u16, NodeKey::from_bytes([2; 32]));
    }
    let rest = probe.complete(until).unwrap();
    assert_eq!(rest.len(), MAX_PEERS - 1);
    assert_eq!(probe.seen.len(), MAX_PEERS);
    assert!(probe.next(until).unwrap().is_empty());
}

#[test]
fn native_solicitation_reaches_the_member_handshake_without_dns() {
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let discovery_port = reservation.port();
    let mut arrivals = Arrivals::on(
        Network {
            scope: Scope::Loopback,
            port: discovery_port,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: None,
        },
        None,
    )
    .unwrap();
    // A real bounded solicitation first misses: no responder exists yet. The receiver
    // remains owned after that probe, and a later bind must wake it through the wire.
    let mut missed = Probe::on(
        links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
        RETRANSMIT,
    )
    .unwrap();
    assert!(missed.first().unwrap().is_empty());
    drop(missed);
    assert!(!arrivals.poll().unwrap());
    let identity = |seed| {
        let provisional =
            NodeIdentity::from_seed("00000000000000000000000000000000", &[seed; 32]).unwrap();
        NodeIdentity::from_seed(
            &aterm_net::fleet_member::node_principal(provisional.key()),
            &[seed; 32],
        )
        .unwrap()
    };
    let server = identity(11);
    let requester = identity(12);
    let secret = FleetSecret::new(&[7; 32]).unwrap();
    let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_on(
        address.port(),
        server.key(),
        || true,
        Network {
            scope: Scope::Loopback,
            port: discovery_port,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: Some(ready),
        },
    )
    .unwrap();
    assert_eq!(bound.recv_timeout(HANG).unwrap(), 1);
    let until = Instant::now() + HANG;
    while !arrivals.poll().unwrap() {
        assert!(
            Instant::now() < until,
            "the late member announced no arrival"
        );
        aterm_uds::ready::any_until(&arrivals.fds(), until).unwrap();
    }
    assert!(!arrivals.poll().unwrap());
    // Admission becoming usable is a distinct hint on the same socket cohort. It
    // neither restarts discovery nor supplies an identity or replaces the handshake.
    let live = Arc::new(AtomicBool::new(true));
    responder.announce_ready(Arc::downgrade(&live));
    while !arrivals.poll().unwrap() {
        assert!(
            Instant::now() < until,
            "the admitted member announced no capability edge"
        );
        aterm_uds::ready::any_until(&arrivals.fds(), until).unwrap();
    }
    assert!(!arrivals.poll().unwrap());
    let hints = probe_on(
        links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
        HANG,
    )
    .unwrap();
    assert_eq!(
        hints,
        [Hint {
            address,
            key: server.key()
        }]
    );
    assert!(!responder.finished());
    let server_secret = secret.clone();
    let (done, finished) = std::sync::mpsc::channel();
    let server_task = thread::spawn(move || {
        // Prove the actual key twice: a copied or forged discovery claim
        // must not pass the ordinary membership-bound handshake.
        for _ in 0..2 {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            aterm_net::fleet::server_handshake_as(
                tcp,
                &server_secret,
                Some(&server),
                FleetScope::Facts,
                &[FleetOp::Survey],
                HANG,
            )
            .unwrap();
        }
        done.send(()).unwrap();
    });
    let keys = [(hints[0].key, aterm_net::fleet_member::Standing::Member)]
        .into_iter()
        .collect();
    for (claim, expected) in [(hints[0].key, true), (requester.key(), false)] {
        let advert = aterm_secrets::discover::Advert {
            instance: "direct-lan".into(),
            host: address.ip().to_string(),
            port: address.port(),
            txt: vec![
                ("v".into(), "2".into()),
                ("id".into(), aterm_net::fleet_member::node_principal(claim)),
                ("port".into(), address.port().to_string()),
            ],
        };
        assert_eq!(
            crate::node_service::prove_discovery_address(&advert, &secret, Some(&requester), &keys)
                .is_some(),
            expected
        );
    }
    finished.recv_timeout(HANG).unwrap();
    server_task.join().unwrap();
    drop(responder);
}

#[test]
fn sharing_hold_opens_no_discovery_socket_and_owner_drop_wakes_the_worker() {
    let mut inert = Arrivals::on(
        Network {
            scope: Scope::Loopback,
            port: 0,
            sources: Box::new(Vec::new),
            ready: None,
        },
        None,
    )
    .unwrap();
    assert!(inert.fds().is_empty());
    assert!(!inert.poll().unwrap());
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_on(
        7100,
        NodeKey::from_bytes([3; 32]),
        || false,
        Network {
            scope: Scope::Loopback,
            port: 0,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: Some(ready),
        },
    )
    .unwrap();
    assert_eq!(bound.recv_timeout(HANG).unwrap(), 0);
    assert!(!responder.finished());
    drop(responder);
}
