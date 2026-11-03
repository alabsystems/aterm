// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local descriptor versions survive clock steps, restarts and concurrent publishers.

use super::*;
use crate::fleet_member::self_entry::{SELF_ENTRY_FILE, self_entry_at};

fn seq(peer: &Peer) -> u64 {
    peer.signed.unwrap().seq
}

/// The old listener passed its early current check, but has not acquired the descriptor
/// writer's lock. Ending it and publishing the new listener must leave that newer descriptor
/// intact when the old worker resumes. Removing only the under-lock current check overwrites
/// the cache with the old endpoint, even though later cancellation checks prevent its dial.
#[test]
fn an_ended_heartbeat_cannot_replace_the_new_generations_descriptor() {
    let state = State::new("self-ended-heartbeat");
    let receiver = State::new("self-current-heartbeat-receiver");
    let secret_bytes = [0x63; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    for dir in [state.dir(), receiver.dir()] {
        write_key(dir, &secret_bytes).unwrap();
    }
    let me = node_identity(state.dir()).unwrap();
    let other = identity(&node_of(0x64), 0x64);
    let log: MemberKeys = [&me, &other]
        .into_iter()
        .map(|who| (who.key(), Standing::Member))
        .collect();
    let server = Issuing::member(secret.clone(), receiver.dir(), other.clone(), log.clone());
    write_book(
        state.dir(),
        &[Peer::own(&other, ("127.0.0.1", server.port, "0.146.0"), 1)],
    );
    let old = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7100,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let new = Registration {
        port: 7200,
        ..old.clone()
    };
    let current = AtomicBool::new(true);
    let (waiting, waited) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let held = BookLock::take(state.dir(), HANG).unwrap();
        let worker = scope.spawn(|| {
            ON_LOCK_WAIT.with_borrow_mut(|before| {
                *before = Some(Box::new(move || {
                    waiting.send(()).unwrap();
                    released.recv_timeout(HANG).unwrap();
                }));
            });
            lock_waiting(HANG, || {
                heartbeat_members_while(
                    state.dir(),
                    (&secret, &me),
                    &old,
                    HANG,
                    (Some(&log), &[], &Vec::new),
                    &current,
                )
                .unwrap()
            })
        });
        waited.recv_timeout(HANG).unwrap();
        current.store(false, Ordering::Release);
        drop(held);
        let answered = heartbeat_members_while(
            state.dir(),
            (&secret, &me),
            &new,
            HANG,
            (Some(&log), &[], &Vec::new),
            &AtomicBool::new(true),
        )
        .unwrap();
        assert_eq!(answered.len(), 1);
        let committed = read_private(state.dir(), SELF_ENTRY_FILE, RECORD_MAX).unwrap();
        let received = read_peers(receiver.dir()).unwrap();
        let received = received
            .iter()
            .find(|peer| peer.node.as_deref() == Some(me.node()))
            .unwrap();
        assert_eq!(received.port, new.port);
        assert!(received.signature_holds());
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_empty());
        assert_eq!(
            read_private(state.dir(), SELF_ENTRY_FILE, RECORD_MAX).unwrap(),
            committed,
            "the old generation allocated no newer version for its stale endpoint"
        );
        assert_eq!(
            read_peers(receiver.dir())
                .unwrap()
                .iter()
                .find(|peer| peer.node.as_deref() == Some(me.node())),
            Some(received)
        );
    });
}

#[test]
fn a_heartbeat_ended_during_its_book_read_keeps_no_observations() {
    let state = State::new("self-ended-book");
    let receiver = State::new("self-ended-book-receiver");
    let secret_bytes = [0x65; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    for dir in [state.dir(), receiver.dir()] {
        write_key(dir, &secret_bytes).unwrap();
    }
    let me = node_identity(state.dir()).unwrap();
    let other = identity(&node_of(0x66), 0x66);
    let current = Arc::new(AtomicBool::new(true));
    let books = Arc::new(AtomicUsize::new(0));
    let log: MemberKeys = [&me, &other]
        .into_iter()
        .map(|who| (who.key(), Standing::Member))
        .collect();
    let server = Issuing::listen(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        (secret.clone(), Some(other.clone())),
        (
            receiver.dir(),
            Issuer {
                node: other.node().to_owned(),
                ..issuer(0x66)
            },
        ),
        EVERY_OP,
        Arc::new({
            let current = Arc::clone(&current);
            let books = Arc::clone(&books);
            move |op| {
                if op == FleetOp::Book {
                    books.fetch_add(1, Ordering::AcqRel);
                    current.store(false, Ordering::Release);
                }
                None
            }
        }),
        Some(log),
    );
    let initial = vec![Peer {
        via: Via::Issuer,
        ..Peer::own(&other, ("127.0.0.1", server.port, "0.146.0"), 1)
    }];
    write_book(state.dir(), &initial);
    write_book(
        receiver.dir(),
        &[Peer::own(&me, ("old.local", 7100, "0.148.0"), u64::MAX - 1)],
    );
    let reg = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7200,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let committed = reg.clone().signed_in(state.dir(), &me).unwrap();
    let hints = || panic!("an ended generation must not resolve more targets");
    let answered = heartbeat_members_while(
        state.dir(),
        (&secret, &me),
        &reg,
        HANG,
        (None, &[], &hints),
        &current,
    )
    .unwrap();
    assert_eq!(books.load(Ordering::Acquire), 1);
    assert!(answered.is_empty());
    assert_eq!(read_peers(state.dir()).unwrap(), initial);
    assert!(read_answered(state.dir()).is_empty());
    assert!(read_issuer_book(state.dir(), other.node()).is_empty());
    assert_eq!(reg.signed_in(state.dir(), &me).unwrap(), committed);
}

#[test]
fn rollback_and_reopen_publish_a_new_endpoint_above_the_received_version() {
    let state = State::new("self-rollback");
    let me = node_identity(state.dir()).unwrap();
    let first = self_entry_at(state.dir(), &me, ("10.0.0.7", 7100, "0.149.0"), 1_000).unwrap();
    let mut receiver = vec![first.clone()];
    let reopened = read_node_identity(state.dir()).unwrap().unwrap();
    let moved = self_entry_at(state.dir(), &reopened, ("10.0.0.8", 7200, "0.149.0"), 10).unwrap();
    assert_eq!(seq(&moved), 1_001);
    assert_eq!(moved.seen_ms, Some(10));
    assert_eq!(record(&mut receiver, moved.clone()), Ok(Recorded::Updated));
    assert_eq!(receiver[0].host, "10.0.0.8");
    assert_eq!(receiver[0].port, 7200);
    assert_eq!(record(&mut receiver, first), Ok(Recorded::Already));
    assert_eq!(receiver[0].signed, moved.signed);

    let versioned =
        self_entry_at(state.dir(), &reopened, ("10.0.0.8", 7200, "0.150.0"), 1).unwrap();
    assert_eq!(seq(&versioned), 1_002);
    assert_eq!(record(&mut receiver, versioned), Ok(Recorded::Updated));
    assert_eq!(receiver[0].version.as_deref(), Some("0.150.0"));
}

#[test]
fn unchanged_descriptor_keeps_its_signature_and_refreshes_observed_address_and_time() {
    let state = State::new("self-unchanged");
    let me = node_identity(state.dir()).unwrap();
    let mut first = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 1_000).unwrap();
    let stored = read_private(state.dir(), SELF_ENTRY_FILE, RECORD_MAX).unwrap();
    first.addr = Some(IpAddr::from([10, 0, 0, 7]));
    first.seen_ms = Some(2_000);
    let mut receiver = vec![first.clone()];
    let mut again = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 30).unwrap();
    assert_eq!(again.signed, first.signed);
    assert_eq!(again.seen_ms, Some(30));
    assert_eq!(
        read_private(state.dir(), SELF_ENTRY_FILE, RECORD_MAX).unwrap(),
        stored
    );
    again.addr = Some(IpAddr::from([10, 0, 0, 8]));
    again.seen_ms = Some(3_000);
    assert_eq!(record(&mut receiver, again), Ok(Recorded::Updated));
    assert_eq!(receiver[0].addr, Some(IpAddr::from([10, 0, 0, 8])));
    assert_eq!(receiver[0].seen_ms, Some(3_000));
    assert_eq!(receiver[0].host, "m7.local");
    assert_eq!(receiver[0].signed, first.signed);
}

#[test]
fn concurrent_publishers_allocate_distinct_changed_versions() {
    let state = State::new("self-concurrent");
    let me = node_identity(state.dir()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut versions = std::thread::scope(|scope| {
        (0..8)
            .map(|offset| {
                let barrier = Arc::clone(&barrier);
                let state = state.dir();
                let me = &me;
                scope.spawn(move || {
                    barrier.wait();
                    let reopened = read_node_identity(state).unwrap().unwrap();
                    assert_eq!(reopened.key(), me.key());
                    lock_waiting(HANG, || {
                        self_entry_at(state, &reopened, ("m7.local", 7100 + offset, "0.149.0"), 1)
                            .unwrap()
                    })
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| seq(&worker.join().unwrap()))
            .collect::<Vec<_>>()
    });
    versions.sort_unstable();
    assert_eq!(versions, (1..=8).collect::<Vec<_>>());
    let cached = Peer::parse(
        std::str::from_utf8(&read_private(state.dir(), SELF_ENTRY_FILE, RECORD_MAX).unwrap())
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(seq(&cached), 8);
}

#[test]
fn optional_names_survive_registration_signing_and_cannot_block_membership() {
    let state = State::new("self-name");
    let me = node_identity(state.dir()).unwrap();
    write_private(state.dir(), NAME_FILE, b"m7\n").unwrap();
    let reg = Registration {
        node: me.node().to_owned(),
        host: "m7.local".into(),
        port: 7100,
        version: "0.149.0".into(),
        key: None,
        signed: None,
    }
    .signed_in(state.dir(), &me)
    .unwrap();
    let initial_seq = reg.signed.unwrap().seq;
    let named = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 1).unwrap();
    assert_eq!(named.primary_name(), Some("m7"));
    assert_eq!(named.signed, reg.signed);
    write_private(state.dir(), NAME_FILE, b"studio\n").unwrap();
    let renamed = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 1).unwrap();
    assert_eq!(renamed.primary_name(), Some("studio"));
    assert_eq!(seq(&renamed), initial_seq + 1);
    write_private(state.dir(), NAME_FILE, b"not a name\n").unwrap();
    let unnamed = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 1).unwrap();
    assert_eq!(unnamed.primary_name(), None);
    assert!(unnamed.signature_holds());
}

#[test]
fn replaced_or_missing_identity_refuses_stale_publishers_and_new_keys_start_their_own_clock() {
    let state = State::new("self-rekey");
    let old = identity(&node_of(7), 7);
    keep_identity(state.dir(), &old, 7);
    self_entry_at(state.dir(), &old, ("m7.local", 7100, "0.149.0"), u64::MAX).unwrap();
    let new = identity(old.node(), 8);
    keep_identity(state.dir(), &new, 8);
    assert!(self_entry_at(state.dir(), &old, ("m7.local", 7200, "0.149.0"), 1).is_err());
    let current = self_entry_at(state.dir(), &new, ("m7.local", 7200, "0.149.0"), 1).unwrap();
    assert_eq!(seq(&current), 1);
    assert_eq!(current.key, Some(new.key()));
    std::fs::remove_file(state.dir().join(NODE_KEY_FILE)).unwrap();
    assert!(self_entry_at(state.dir(), &new, ("m7.local", 7200, "0.149.0"), 2).is_err());
    assert!(!state.dir().join(NODE_KEY_FILE).exists());
}

#[test]
fn malformed_or_forged_cache_and_exhaustion_refuse_publication() {
    let state = State::new("self-corrupt");
    let me = node_identity(state.dir()).unwrap();
    let endpoint = ("m7.local", 7100, "0.149.0");
    write_private(state.dir(), SELF_ENTRY_FILE, b"not a descriptor\n").unwrap();
    assert!(self_entry_at(state.dir(), &me, endpoint, 1).is_err());
    let forged = Peer::own(&me, endpoint, 1)
        .line()
        .replace("host=m7.local", "host=m8.local");
    write_private(state.dir(), SELF_ENTRY_FILE, forged.as_bytes()).unwrap();
    assert!(self_entry_at(state.dir(), &me, endpoint, 1).is_err());
    let line = Peer::own(&me, endpoint, 1).line();
    write_private(
        state.dir(),
        SELF_ENTRY_FILE,
        format!("{line}\n{line}\n").as_bytes(),
    )
    .unwrap();
    assert!(self_entry_at(state.dir(), &me, endpoint, 1).is_err());
    std::fs::remove_file(state.dir().join(SELF_ENTRY_FILE)).unwrap();
    let last = self_entry_at(state.dir(), &me, endpoint, u64::MAX).unwrap();
    assert_eq!(
        self_entry_at(state.dir(), &me, endpoint, 1).unwrap().signed,
        last.signed
    );
    assert!(self_entry_at(state.dir(), &me, ("m8.local", 7100, "0.149.0"), 1).is_err());
}

#[test]
fn a_failed_commit_leaves_the_published_version_intact() {
    let state = State::new("self-write-fails");
    let me = node_identity(state.dir()).unwrap();
    let first = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 100).unwrap();
    mode(state.dir(), 0o500);
    let changed = self_entry_at(state.dir(), &me, ("m8.local", 7100, "0.149.0"), 1);
    mode(state.dir(), 0o700);
    assert!(
        changed.is_err(),
        "a descriptor must not escape a failed commit"
    );
    let reopened = self_entry_at(state.dir(), &me, ("m7.local", 7100, "0.149.0"), 1).unwrap();
    assert_eq!(reopened.signed, first.signed);
}

#[test]
fn valid_same_identity_book_and_copy_rows_supply_migration_floors() {
    let state = State::new("self-migration");
    let me = node_identity(state.dir()).unwrap();
    let other = identity(&node_of(8), 8);
    let imposter = identity(me.node(), 9);
    let valid = Peer::own(&me, ("old.local", 7100, "0.148.0"), 100);
    let mut forged = Peer::own(&me, ("old.local", 7100, "0.148.0"), 10_000);
    forged.host = "forged.local".into();
    write_book(
        state.dir(),
        &[
            valid,
            forged,
            Peer::own(&other, ("other.local", 7100, "0.149.0"), 20_000),
            Peer::own(&imposter, ("imposter.local", 7100, "0.149.0"), 30_000),
        ],
    );
    let copied = Peer {
        via: Via::IssuerBook,
        ..Peer::own(&me, ("old.local", 7100, "0.148.0"), 200)
    };
    keep_copy(state.dir(), other.node(), &[copied]).unwrap();
    let current = self_entry_at(state.dir(), &me, ("new.local", 7200, "0.149.0"), 1).unwrap();
    assert_eq!(seq(&current), 201);
}

#[test]
fn an_observed_higher_self_row_advances_an_unchanged_local_descriptor() {
    let state = State::new("self-observed");
    let me = node_identity(state.dir()).unwrap();
    let endpoint = ("m7.local", 7100, "0.149.0");
    self_entry_at(state.dir(), &me, endpoint, 1).unwrap();
    let old_high = Peer::own(&me, ("old.local", 7100, "0.148.0"), 1_000);
    hold_book(state.dir())
        .unwrap()
        .observe_self_entries(&me, [old_high].into_iter())
        .unwrap();
    let fresh = self_entry_at(state.dir(), &me, endpoint, 1).unwrap();
    assert_eq!(seq(&fresh), 1_001);
    let same_high = Peer::own(&me, endpoint, 2_000);
    hold_book(state.dir())
        .unwrap()
        .observe_self_entries(&me, [same_high.clone()].into_iter())
        .unwrap();
    let fresh = self_entry_at(state.dir(), &me, endpoint, 1).unwrap();
    assert_eq!(fresh.signed, same_high.signed);
}

#[test]
fn heartbeat_recovers_a_higher_self_version_from_an_authenticated_peer() {
    let state = State::new("self-heartbeat");
    let issuer_state = State::new("self-heartbeat-issuer");
    let secret_bytes = [0x62; SECRET_LEN];
    for dir in [state.dir(), issuer_state.dir()] {
        write_key(dir, &secret_bytes).unwrap();
    }
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let me = node_identity(state.dir()).unwrap();
    let issuer = identity(&node_of(9), 9);
    let log: MemberKeys = [&me, &issuer]
        .into_iter()
        .map(|who| (who.key(), Standing::Member))
        .collect();
    let high = u64::MAX - 10;
    write_book(
        issuer_state.dir(),
        &[Peer::own(&me, ("old.local", 7000, "0.148.0"), high)],
    );
    let serving = Issuing::member(
        secret.clone(),
        issuer_state.dir(),
        issuer.clone(),
        log.clone(),
    );
    write_book(
        state.dir(),
        &[Peer {
            via: Via::Issuer,
            ..Peer::own(&issuer, ("127.0.0.1", serving.port, "0.149.0"), 1)
        }],
    );
    write_private(state.dir(), NAME_FILE, b"m7\n").unwrap();
    let reg = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    for _ in 0..2 {
        let answers = heartbeat_members(
            state.dir(),
            (&secret, &me),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .unwrap();
        assert_eq!(answers.len(), 1);
    }
    let book = read_peers(issuer_state.dir()).unwrap();
    let received = book
        .iter()
        .find(|peer| peer.node.as_deref() == Some(me.node()))
        .unwrap();
    assert_eq!(seq(received), high + 1);
    assert_eq!((received.host.as_str(), received.port), ("127.0.0.1", 7199));
    assert_eq!(received.primary_name(), Some("m7"));
    assert!(
        read_peers(state.dir())
            .unwrap()
            .iter()
            .all(|peer| peer.node.as_deref() != Some(me.node()))
    );
}
