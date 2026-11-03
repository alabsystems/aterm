// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signed member descriptors survive older unsigned contact information.

use super::*;

fn changing_member(
    state: &Path,
    secret: FleetSecret,
    me: NodeIdentity,
    log: MemberKeys,
) -> (Issuing, Peer) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let old = Issuer {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port,
        version: "0.148.0".to_owned(),
    };
    let latest = Peer {
        seen_ms: unix_ms_now(),
        ..Peer::own(
            &me,
            (
                "127.0.0.2",
                if port == 7101 { 7102 } else { 7101 },
                "0.149.0",
            ),
            20,
        )
    };
    let running = Arc::new(AtomicBool::new(true));
    let listening = Listening {
        secret: secret.clone(),
        scope: FleetScope::Content,
        node: Some(me.clone()),
        serves: EVERY_OP,
        slots: 8,
        per_source: 8,
        lifetime: HANG,
    };
    let state = state.to_path_buf();
    let newer = latest.clone();
    let worker_running = Arc::clone(&running);
    let serving = std::thread::spawn(move || {
        serve_with_handshake(
            &listener,
            &listening,
            &worker_running,
            HANG,
            move |served, op| {
                let proof = served.peer_node().cloned();
                served.answer(|stream| {
                    stream.set_read_timeout(Some(HANG))?;
                    if op == FleetOp::Book {
                        return serve_book_with_log(
                            stream,
                            &state,
                            || Ok(me.node().to_owned()),
                            unix_ms_now(),
                            (&[], Some(&log), Some(&newer)),
                        )
                        .map(|_| ());
                    }
                    assert_eq!(op, FleetOp::Join);
                    let from = stream.peer_addr()?.ip();
                    serve_join_with_denied(
                        stream,
                        (from, proof.as_ref()),
                        (&state, &secret),
                        || Ok(old.clone()),
                        unix_ms_now().unwrap(),
                        (&[], Some(&log)),
                    )
                    .map(|_| ())
                })
            },
            |_: FleetEvent| {},
        );
    });
    (
        Issuing {
            port,
            running,
            serving: Some(serving),
        },
        latest,
    )
}

#[test]
fn a_newer_signed_book_descriptor_survives_an_earlier_join_reply() {
    let state = State::new("heartbeat-newer-descriptor");
    let far_state = State::new("heartbeat-newer-descriptor-far");
    let secret_bytes = [0x63; SECRET_LEN];
    for dir in [state.dir(), far_state.dir()] {
        write_key(dir, &secret_bytes).unwrap();
    }
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let me = node_identity(state.dir()).unwrap();
    let far = identity(&node_of(0x75), 0x75);
    let log: MemberKeys = [&me, &far]
        .into_iter()
        .map(|who| (who.key(), Standing::Member))
        .collect();
    let (serving, latest) =
        changing_member(far_state.dir(), secret.clone(), far.clone(), log.clone());
    let old = Peer {
        via: Via::Issuer,
        addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
        seen_ms: unix_ms_now(),
        ..Peer::own(&far, ("127.0.0.1", serving.port, "0.148.0"), 10)
    };
    write_book(state.dir(), &[old]);
    let registration = Registration {
        node: me.node().to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 7199,
        version: "0.149.0".to_owned(),
        key: None,
        signed: None,
    };
    let answers = heartbeat_members(
        state.dir(),
        (&secret, &me),
        &registration,
        HANG,
        (Some(&log), &Vec::new),
    )
    .unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].joined.issuer.version, "0.148.0");
    let book = read_peers(state.dir()).unwrap();
    let received = book
        .iter()
        .find(|peer| peer.node.as_deref() == Some(far.node()))
        .unwrap();
    assert!(
        received.signature_holds(),
        "the earlier join reply must not alter signed fields"
    );
    assert_eq!(
        (&received.host, received.port, &received.version),
        (&latest.host, latest.port, &latest.version)
    );
    assert_eq!(received.addr, Some(IpAddr::from(Ipv4Addr::LOCALHOST)));
    assert!(received.seen_ms.is_some());
    let mut next = Vec::new();
    assert_eq!(
        merge_signed(
            &mut next,
            std::slice::from_ref(received),
            (None, unix_ms_now().unwrap()),
            (Some(&log), &[], &Keyring::default())
        ),
        1
    );
    assert!(
        next[0].signature_holds(),
        "the descriptor remains usable by another member"
    );
}

#[test]
fn a_valid_descriptor_repairs_a_corrupted_same_sequence_copy() {
    let me = identity(&node_of(0x76), 0x76);
    let valid = Peer {
        seen_ms: unix_ms_now(),
        ..Peer::own(&me, ("current.local", 7100, "0.149.0"), 20)
    };
    let mut damaged = valid.clone();
    damaged.host = "previous.local".to_owned();
    assert!(!damaged.signature_holds());
    let mut book = vec![damaged];
    assert_eq!(record(&mut book, valid.clone()), Ok(Recorded::Updated));
    assert_eq!(book[0], valid);
    assert!(book[0].signature_holds());
}

#[test]
fn an_unsigned_contact_keeps_only_a_valid_same_key_descriptor() {
    let me = identity(&node_of(0x77), 0x77);
    let other_key = identity(me.node(), 0x78);
    let valid = Peer {
        addr: Some("127.0.0.2".parse().unwrap()),
        seen_ms: Some(100),
        ..Peer::own(&me, ("newer.local", 7200, "0.150.0"), 20)
            .with_primary_name(&me, Some("studio"))
    };
    let mut forged = valid.clone();
    forged.host = "forged.local".to_owned();
    let different = Peer::own(&other_key, ("other-key.local", 7300, "0.150.0"), 30);
    let recent = Peer {
        addr: Some("127.0.0.3".parse().unwrap()),
        seen_ms: Some(300),
        ..valid.clone()
    };
    let unaddressed = Peer {
        addr: None,
        ..recent.clone()
    };
    let contact = Peer {
        node: Some(me.node().to_owned()),
        key: Some(me.key()),
        key_from: None,
        host: "earlier.local".to_owned(),
        addr: Some("127.0.0.1".parse().unwrap()),
        port: 7100,
        version: Some("0.149.0".to_owned()),
        seen_ms: Some(200),
        via: Via::Issuer,
        fleet: Some(secret(7).mark()),
        signed: None,
    };
    for (keeps_signed, held, addr, seen_ms) in [
        (true, valid.clone(), contact.addr, contact.seen_ms),
        (true, recent.clone(), recent.addr, recent.seen_ms),
        (true, unaddressed, contact.addr, recent.seen_ms),
        (false, forged, contact.addr, contact.seen_ms),
        (false, different, contact.addr, contact.seen_ms),
    ] {
        let mut book = vec![held];
        assert_eq!(record(&mut book, contact.clone()), Ok(Recorded::Updated));
        let received = &book[0];
        assert_eq!(received.node, contact.node);
        assert_eq!(received.key, contact.key);
        assert_eq!(received.addr, addr);
        assert_eq!(received.seen_ms, seen_ms);
        assert_eq!(received.via, Via::Issuer);
        assert_eq!(received.fleet, contact.fleet);
        if keeps_signed {
            assert_eq!(
                (
                    &received.host,
                    received.port,
                    &received.version,
                    received.signed
                ),
                (&valid.host, valid.port, &valid.version, valid.signed)
            );
            assert!(received.signature_holds());
            assert_eq!(received.primary_name(), Some("studio"));
        } else {
            assert_eq!(
                (
                    &received.host,
                    received.port,
                    &received.version,
                    received.signed
                ),
                (&contact.host, contact.port, &contact.version, None)
            );
        }
    }
    let mut book = vec![valid.clone()];
    let registration = Peer {
        via: Via::Joined,
        ..contact
    };
    assert_eq!(
        record(&mut book, registration.clone()),
        Ok(Recorded::Updated)
    );
    assert_eq!(book[0].via, Via::Joined);
    assert_eq!(book[0].signed, valid.signed);
    assert_eq!(book[0].addr, registration.addr);
    assert!(book[0].signature_holds());
}
