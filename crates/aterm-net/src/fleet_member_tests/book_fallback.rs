// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Book read judges each address before ending its walk or retaining its exchange.

use super::*;

fn listener(
    state: &State,
    node: &str,
    who: Option<NodeIdentity>,
    serves: &'static [FleetOp],
) -> Issuing {
    let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = socket.local_addr().unwrap().port();
    let log = who
        .as_ref()
        .map(|me| [(me.key(), Standing::Member)].into_iter().collect());
    Issuing::listen(
        socket,
        (secret(5), who),
        (
            state.dir(),
            Issuer {
                node: node.to_owned(),
                host: "127.0.0.1".to_owned(),
                port,
                version: "0.151.0".to_owned(),
            },
        ),
        serves,
        Arc::new(|_| None),
        log,
    )
}

#[test]
fn a_book_read_passes_a_wrong_identity_and_releases_its_recorded_address() {
    let wanted = identity(&node_of(0xc1), 0xc1);
    let other = identity(&node_of(0xc2), 0xc2);
    let impostor = identity(wanted.node(), 0xc3);
    let marker = Peer::own(
        &identity(&node_of(0xc4), 0xc4),
        ("correct-book.test", 7109, "0.151.0"),
        7,
    );
    // An unbound older issuer can still serve its unproved book. A bound issuer must
    // prove its key, including after a different key claims the same node at the old address.
    for (wrong, proven) in [(other.clone(), true), (impostor, true), (other, false)] {
        for serves in [EVERY_OP, OLDER_OPS] {
            let wrong_state = State::new("book-wrong-address");
            let right_state = State::new("book-right-address");
            write_book(right_state.dir(), std::slice::from_ref(&marker));
            let wrong = listener(&wrong_state, wrong.node(), Some(wrong.clone()), serves);
            let right = listener(
                &right_state,
                wanted.node(),
                proven.then(|| wanted.clone()),
                EVERY_OP,
            );
            let old = SocketAddr::from((Ipv4Addr::LOCALHOST, wrong.port));
            let current = SocketAddr::from((Ipv4Addr::LOCALHOST, right.port));
            let target = Target {
                host: "book-peer.test".to_owned(),
                addr: Some(old.ip()),
                port: old.port(),
                node: Some(wanted.node().to_owned()),
                key: proven.then(|| wanted.key()),
            };
            let held = Arc::new(HeldAddrs::default());
            let first = held.enter(old).unwrap_or_else(|_| panic!("first exchange"));
            let released = Arc::new(AtomicBool::new(false));
            let resolver = {
                let held = Arc::clone(&held);
                let released = Arc::clone(&released);
                move |host: &str, _: u16| {
                    assert_eq!(host, "book-peer.test");
                    let reopened = held
                        .enter(old)
                        .unwrap_or_else(|_| panic!("wrong address still held during fallback"));
                    drop(reopened);
                    released.store(true, Ordering::Release);
                    vec![old, current, current]
                }
            };
            let book = fetch_book_with(
                &target,
                (&secret(5), None),
                (Instant::now() + HANG, HANG),
                (Some(&held), Some((old, first))),
                resolver,
            )
            .expect("the issuer's current address is tried");
            assert!(released.load(Ordering::Acquire));
            assert_eq!(book.node, wanted.node());
            assert_eq!(book.proven, proven);
            assert_eq!(book.peers.last(), Some(&marker));
            assert!(
                held.open.lock().unwrap().is_empty(),
                "the returned book retains no connection's exchange"
            );
        }
    }
}

#[test]
fn a_book_refusal_from_the_bound_issuer_ends_without_resolving_another_address() {
    let state = State::new("book-bound-refusal");
    let wanted = identity(&node_of(0xc5), 0xc5);
    let older = listener(&state, wanted.node(), Some(wanted.clone()), OLDER_OPS);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, older.port));
    let target = Target {
        host: "book-peer.test".to_owned(),
        addr: Some(at.ip()),
        port: at.port(),
        node: Some(wanted.node().to_owned()),
        key: Some(wanted.key()),
    };
    let held = HeldAddrs::default();
    let first = held.enter(at).unwrap_or_else(|_| panic!("first exchange"));
    let result = fetch_book_with(
        &target,
        (&secret(5), None),
        (Instant::now() + HANG, HANG),
        (Some(&held), Some((at, first))),
        |_, _| panic!("the bound issuer's refusal is authoritative"),
    );
    assert!(matches!(result, Err(BookUnread::Older(refused)) if refused == at));
    assert!(held.open.lock().unwrap().is_empty());
}

#[test]
fn a_book_read_passes_a_wrong_legacy_header_and_keeps_the_right_unproved_book() {
    for malformed in [false, true] {
        let wrong_state = State::new("book-wrong-legacy-header");
        let right_state = State::new("book-right-legacy-header");
        let wanted = identity(&node_of(0xc6), 0xc6);
        let marker = Peer::own(
            &identity(&node_of(0xc7), 0xc7),
            ("legacy-book.test", 7111, "0.146.0"),
            8,
        );
        write_book(right_state.dir(), std::slice::from_ref(&marker));
        if malformed {
            write_private(wrong_state.dir(), PEERS_FILE, b"peer garbage\n").unwrap();
        }
        let wrong = listener(&wrong_state, &node_of(0xc8), None, EVERY_OP);
        let right = listener(&right_state, wanted.node(), None, EVERY_OP);
        let old = SocketAddr::from((Ipv4Addr::LOCALHOST, wrong.port));
        let current = SocketAddr::from((Ipv4Addr::LOCALHOST, right.port));
        let target = Target {
            host: "legacy-book.test".to_owned(),
            addr: Some(old.ip()),
            port: old.port(),
            node: Some(wanted.node().to_owned()),
            key: None,
        };
        let held = Arc::new(HeldAddrs::default());
        let first = held.enter(old).unwrap_or_else(|_| panic!("first exchange"));
        let resolver = {
            let held = Arc::clone(&held);
            move |_: &str, _: u16| {
                let reopened = held
                    .enter(old)
                    .unwrap_or_else(|_| panic!("legacy wrong-node exchange still held"));
                drop(reopened);
                vec![current]
            }
        };
        let book = fetch_book_with(
            &target,
            (&secret(5), None),
            (Instant::now() + HANG, HANG),
            (Some(&held), Some((old, first))),
            resolver,
        )
        .expect("the wrong legacy header leaves the next address available");
        assert_eq!(book.node, wanted.node());
        assert!(!book.proven, "an accepted legacy book claims no proof");
        assert_eq!(book.peers, [marker]);
        assert!(held.open.lock().unwrap().is_empty());
    }
}

#[test]
fn a_book_error_from_the_bound_issuer_still_ends_the_read() {
    let state = State::new("book-bound-unreadable");
    let wanted = identity(&node_of(0xc9), 0xc9);
    let serving = listener(&state, wanted.node(), Some(wanted.clone()), EVERY_OP);
    write_private(state.dir(), PEERS_FILE, b"peer garbage\n").unwrap();
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, serving.port));
    let target = Target {
        host: "book-peer.test".to_owned(),
        addr: Some(at.ip()),
        port: at.port(),
        node: Some(wanted.node().to_owned()),
        key: Some(wanted.key()),
    };
    let result = fetch_book_with(
        &target,
        (&secret(5), None),
        (Instant::now() + HANG, HANG),
        (None, None),
        |_, _| panic!("the bound issuer's Book error is terminal"),
    );
    assert!(matches!(result, Err(BookUnread::NotNow { at: refused, .. }) if refused == at));
}
