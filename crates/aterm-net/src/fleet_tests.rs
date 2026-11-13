// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::CtlStream;
use std::net::TcpListener;

const HANG: Duration = Duration::from_secs(60);

const EVERY_OP: &[FleetOp] = &[
    FleetOp::Survey,
    FleetOp::Book,
    FleetOp::Join,
    FleetOp::Drive,
];

fn secret(byte: u8) -> FleetSecret {
    FleetSecret::new(&[byte; FleetSecret::MIN_LEN]).unwrap()
}

fn hex32(s: &str) -> Vec<u8> {
    crate::hex_decode(s).unwrap()
}

fn over_loopback<A: Send + 'static, B>(
    listen: impl FnOnce(TcpStream) -> A + Send + 'static,
    dial: impl FnOnce(TcpStream) -> B,
) -> (A, B) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let listening = std::thread::spawn(move || {
        listen(
            aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap()
            .0,
        )
    });
    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(HANG)).unwrap();
    let dialed = dial(tcp);
    (listening.join().unwrap(), dialed)
}

fn raw_accept(tcp: TcpStream) -> TlsTransport<ServerConnection> {
    tcp.set_read_timeout(Some(HANG)).unwrap();
    accept_tls(tcp, server_config()).unwrap().0
}

fn raw_connect(tcp: TcpStream) -> TlsTransport<ClientConnection> {
    connect_tls(tcp, client_config()).unwrap().0
}

fn drain<R: Read>(stream: &mut R) -> Vec<u8> {
    let mut got = Vec::new();
    let _ = stream.read_to_end(&mut got);
    got
}

#[test]
fn framed_handshakes_enable_immediate_writes_for_each_peer() {
    let (server, client) = over_loopback(
        |tcp| {
            let (mut channel, _) =
                server_handshake(tcp, &secret(7), FleetScope::Facts, &[FleetOp::Survey], HANG)
                    .unwrap();
            let socket = channel.stream().get_ref();
            socket.set_read_timeout(Some(HANG)).unwrap();
            socket.set_write_timeout(Some(HANG)).unwrap();
            let immediate = socket.nodelay().unwrap();
            let mut request = [0];
            channel.stream().read_exact(&mut request).unwrap();
            channel.stream().write_all(&request).unwrap();
            channel.stream().flush().unwrap();
            immediate
        },
        |tcp| {
            let (mut channel, _) =
                client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
            let socket = channel.stream().get_ref();
            socket.set_read_timeout(Some(HANG)).unwrap();
            socket.set_write_timeout(Some(HANG)).unwrap();
            let immediate = socket.nodelay().unwrap();
            channel.stream().write_all(&[42]).unwrap();
            channel.stream().flush().unwrap();
            let mut reply = [0];
            channel.stream().read_exact(&mut reply).unwrap();
            assert_eq!(reply, [42]);
            immediate
        },
    );
    assert!(
        server,
        "the framed TLS listener must send short replies promptly"
    );
    assert!(
        client,
        "the framed TLS dialer must send short requests promptly"
    );
}

#[test]
fn the_role_keys_and_a_tag_are_the_answers_recorded_before_the_hkdf_moved() {
    let keys = FleetSecret::new(&[0x0b; 22]).unwrap();
    let recorded = |hex: &str| <[u8; 32]>::try_from(hex32(hex)).unwrap();
    assert_eq!(
        keys.server,
        recorded("4372c139d6e12dd79a1067058c17bc0b80c23ec04408767788a0c3564a3a8105")
    );
    assert_eq!(
        keys.client,
        recorded("58c9589b9c8d82c98a1d00fd0e05a0bed3586ec685ce363c6c46f61623b822ef")
    );
    assert_eq!(
        keys.join,
        recorded("45abb3624f933b8d0104508a3cd486edf83a358c4beec2d326b0d73e8a21e066")
    );
    let survey = tag(&keys.client, &[0xa5; 32], "survey");
    assert_eq!(
        survey,
        recorded("f92e33cf27fc4091ec8577e69a3afae1985bfca7b81d9d98738e89272125c732")
    );

    let mut flipped = [0x0b; 22];
    flipped[21] ^= 0x01;
    let other = FleetSecret::new(&flipped).unwrap();
    assert_ne!(other.server, keys.server);
    assert_ne!(other.client, keys.client);
    assert_ne!(other.join, keys.join);
    assert_ne!(tag(&other.client, &[0xa5; 32], "survey"), survey);
}

#[test]
fn a_secret_is_20_to_32_bytes_and_its_role_keys_differ() {
    assert_eq!(FleetSecret::new(&[1; 19]).unwrap_err(), SecretLength(19));
    assert_eq!(FleetSecret::new(&[1; 33]).unwrap_err(), SecretLength(33));
    let short = FleetSecret::new(&[1; 20]).unwrap();
    let long = FleetSecret::new(&[1; 32]).unwrap();
    assert_ne!(short.server, long.server);
    for keys in [&short, &long] {
        assert_ne!(keys.server, keys.client);
        assert_ne!(keys.server, keys.join);
        assert_ne!(keys.client, keys.join);
    }
    assert_eq!(format!("{short:?}"), "FleetSecret(..)");
}

#[test]
fn a_handshake_proves_both_ends_and_hands_over_the_stream() {
    for (scope, op) in [
        (FleetScope::Facts, FleetOp::Survey),
        (FleetScope::Facts, FleetOp::Book),
        (FleetScope::Content, FleetOp::Join),
    ] {
        let (listened, dialed) = over_loopback(
            move |tcp| {
                let (mut t, op) = server_handshake(tcp, &secret(7), scope, EVERY_OP, HANG).unwrap();
                let alpn = t.stream().conn.alpn_protocol().map(<[u8]>::to_vec);
                t.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
                let mut request = [0u8; 5];
                t.stream().read_exact(&mut request).unwrap();
                t.stream().write_all(b"pong\n").unwrap();
                t.stream().flush().unwrap();
                (op, request, t.exporter().to_vec(), alpn)
            },
            move |tcp| {
                let (mut t, scope) = client_handshake(tcp, &secret(7), op, HANG).unwrap();
                let alpn = t.stream().conn.alpn_protocol().map(<[u8]>::to_vec);
                t.stream().write_all(b"ping\n").unwrap();
                t.stream().flush().unwrap();
                t.stream().get_ref().set_read_timeout(Some(HANG)).unwrap();
                let mut answer = [0u8; 5];
                t.stream().read_exact(&mut answer).unwrap();
                (scope, answer, t.exporter().to_vec(), alpn)
            },
        );
        assert_eq!(
            listened.0, op,
            "the listener learns the op the dialer proved"
        );
        assert_eq!(
            dialed.0, scope,
            "the dialer learns the scope the listener proved"
        );
        assert_eq!(&listened.1, b"ping\n");
        assert_eq!(&dialed.1, b"pong\n");
        assert_eq!(listened.2, dialed.2, "one exporter on both ends");
        assert_eq!(listened.3.as_deref(), Some(ALPN_NODES));
        assert_eq!(dialed.3.as_deref(), Some(ALPN_NODES));
    }
}

fn node(byte: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{byte:02x}").repeat(16), &[byte; 32]).unwrap()
}

/// Two database endpoints meet on the newest database protocol both offer: version 5 is this
/// build (`atdb/5`), 4 is 0.153 (`atdb/4`), 3 and 2 the builds before it, and 1 a fleet
/// endpoint that offers no database protocol.
#[test]
fn database_repair_negotiates_its_order_and_falls_back_with_legacy_node_peers() {
    for (listener_version, dialer_version, expected) in [
        (5, 5, ALPN_DATABASE_DELTA),
        (5, 4, ALPN_DATABASE_FRONTIER),
        (4, 5, ALPN_DATABASE_FRONTIER),
        (5, 3, ALPN_DATABASE_CREDENTIALS),
        (3, 5, ALPN_DATABASE_CREDENTIALS),
        (5, 2, ALPN_DATABASE_REPAIR),
        (2, 5, ALPN_DATABASE_REPAIR),
        (5, 1, ALPN_NODES),
        (1, 5, ALPN_NODES),
        (4, 4, ALPN_DATABASE_FRONTIER),
        (4, 3, ALPN_DATABASE_CREDENTIALS),
        (3, 4, ALPN_DATABASE_CREDENTIALS),
        (4, 2, ALPN_DATABASE_REPAIR),
        (2, 4, ALPN_DATABASE_REPAIR),
        (4, 1, ALPN_NODES),
        (1, 4, ALPN_NODES),
        (3, 3, ALPN_DATABASE_CREDENTIALS),
        (3, 2, ALPN_DATABASE_REPAIR),
        (2, 3, ALPN_DATABASE_REPAIR),
        (2, 2, ALPN_DATABASE_REPAIR),
        (3, 1, ALPN_NODES),
        (1, 3, ALPN_NODES),
        (2, 1, ALPN_NODES),
        (1, 2, ALPN_NODES),
        (1, 1, ALPN_NODES),
    ] {
        let (listened, dialed) = over_loopback(
            move |tcp| {
                let me = node(1);
                let mut accepted = if listener_version == 5 {
                    server_handshake_database_as(tcp, &secret(7), Some(&me), HANG)
                } else if listener_version >= 2 {
                    let mut config = (*database_server_config()).clone();
                    config
                        .alpn_protocols
                        .retain(|alpn| offered_up_to(listener_version, alpn));
                    let watchdog = arm(&tcp, HANG).unwrap();
                    server_handshake_armed(
                        tcp,
                        watchdog,
                        HANG,
                        (&secret(7), Some(&me)),
                        FleetScope::Facts,
                        &[FleetOp::Database],
                        &Denied::default(),
                        |_| true,
                        Arc::new(config),
                    )
                } else {
                    server_handshake_as(
                        tcp,
                        &secret(7),
                        Some(&me),
                        FleetScope::Facts,
                        &[FleetOp::Database],
                        HANG,
                    )
                }
                .unwrap();
                assert_eq!(accepted.op, FleetOp::Database);
                assert_eq!(accepted.peer.unwrap().key, node(2).key());
                let exporter = accepted.transport.exporter().to_vec();
                let stream = accepted.transport.stream();
                stream.get_ref().set_read_timeout(Some(HANG)).unwrap();
                let alpn = stream.conn.alpn_protocol().unwrap().to_vec();
                let mut request = [0; 5];
                stream.read_exact(&mut request).unwrap();
                assert_eq!(&request, b"ping\n");
                stream.write_all(b"pong\n").unwrap();
                stream.flush().unwrap();
                (alpn, exporter)
            },
            move |tcp| {
                let me = node(2);
                let mut dialed = if dialer_version == 5 {
                    client_handshake_database_as(tcp, &secret(7), Some(&me), HANG)
                } else if dialer_version >= 2 {
                    let mut config = (*database_client_config()).clone();
                    config
                        .alpn_protocols
                        .retain(|alpn| offered_up_to(dialer_version, alpn));
                    client_handshake_by_on(
                        tcp,
                        &secret(7),
                        Some(&me),
                        FleetOp::Database,
                        HANG,
                        Arc::new(config),
                    )
                    .map_err(|undialed| undialed.error)
                } else {
                    client_handshake_as(tcp, &secret(7), Some(&me), FleetOp::Database, HANG)
                }
                .unwrap();
                assert_eq!(dialed.scope, FleetScope::Facts);
                assert_eq!(dialed.peer.unwrap().key, node(1).key());
                let exporter = dialed.transport.exporter().to_vec();
                let stream = dialed.transport.stream();
                stream.get_ref().set_read_timeout(Some(HANG)).unwrap();
                let alpn = stream.conn.alpn_protocol().unwrap().to_vec();
                stream.write_all(b"ping\n").unwrap();
                stream.flush().unwrap();
                let mut answer = [0; 5];
                stream.read_exact(&mut answer).unwrap();
                assert_eq!(&answer, b"pong\n");
                (alpn, exporter)
            },
        );
        assert_eq!(listened.0, expected);
        assert_eq!(dialed.0, expected);
        assert_eq!(listened.1, dialed.1);
    }
}

#[test]
fn a_database_listener_does_not_grant_terminal_drive() {
    let (listened, dialed) = over_loopback(
        |tcp| server_handshake_database_as(tcp, &secret(7), Some(&node(1)), HANG).err(),
        |tcp| client_handshake_as(tcp, &secret(7), Some(&node(2)), FleetOp::Drive, HANG).err(),
    );
    assert!(matches!(listened, Some(FleetError::UnknownOp(op)) if op == "drive"));
    assert!(matches!(dialed, Some(FleetError::UnknownOp(op)) if op == "drive"));
}

#[test]
fn each_end_proves_its_node_under_atf2_and_neither_under_atf1() {
    let proof = |me: &NodeIdentity| NodeProof {
        node: me.node().to_owned(),
        key: me.key(),
    };
    for (listener, dialer) in [
        (Some(node(1)), Some(node(2))),
        (Some(node(1)), None),
        (None, Some(node(2))),
    ] {
        let (want_heard, want_dialed) = (dialer.as_ref().map(proof), listener.as_ref().map(proof));
        let (listened, dialed) = over_loopback(
            move |tcp| {
                server_handshake_as(
                    tcp,
                    &secret(7),
                    listener.as_ref(),
                    FleetScope::Facts,
                    EVERY_OP,
                    HANG,
                )
                .map(|accepted| accepted.peer)
            },
            move |tcp| {
                client_handshake_as(tcp, &secret(7), dialer.as_ref(), FleetOp::Survey, HANG)
                    .map(|dialed| dialed.peer)
            },
        );
        assert_eq!(listened.unwrap(), want_heard);
        assert_eq!(dialed.unwrap(), want_dialed);
    }
    let (listened, dialed) = over_loopback(
        |tcp| {
            server_handshake_as(
                tcp,
                &secret(7),
                Some(&node(1)),
                FleetScope::Facts,
                EVERY_OP,
                HANG,
            )
            .map(|accepted| (accepted.op, accepted.peer))
        },
        |tcp| {
            let (mut t, nodes) = connect_tls(tcp, dialer_tls(&[ALPN])).unwrap();
            let exporter = t.exporter().to_vec();
            assert!(!nodes, "atf/1 is spoken");
            dialer_proves(
                t.stream(),
                &exporter,
                (&secret(7), Some(&node(2)), nodes),
                FleetOp::Book,
                &mut None,
            )
        },
    );
    assert_eq!(listened.unwrap(), (FleetOp::Book, None));
    assert_eq!(dialed.unwrap(), (FleetScope::Facts, None));
    let older = listener_tls(CERT_DER, KEY_DER, &[ALPN]);
    let (listened, dialed) = over_loopback(
        move |tcp| {
            tcp.set_read_timeout(Some(HANG)).unwrap();
            let mut conn = ServerConnection::new(older).unwrap();
            let mut tcp = tcp;
            while conn.is_handshaking() {
                conn.complete_io(&mut tcp).unwrap();
            }
            let exporter = export(&conn, Spoken::Fleet { nodes: false }).unwrap();
            let mut stream = StreamOwned::new(conn, tcp);
            listener_proves(
                &mut stream,
                &exporter,
                (&secret(7), None, false),
                FleetScope::Content,
                EVERY_OP,
                Box::<Denied>::default,
                |_| true,
            )
        },
        |tcp| {
            client_handshake_as(tcp, &secret(7), Some(&node(2)), FleetOp::Survey, HANG)
                .map(|dialed| (dialed.scope, dialed.peer))
        },
    );
    assert_eq!(listened.unwrap(), (FleetOp::Survey, None));
    assert_eq!(dialed.unwrap(), (FleetScope::Content, None));
}

#[test]
fn a_node_proof_proves_only_its_own_end_on_its_own_channel() {
    const E: &[u8] = &[0xa5; EXPORTER_LEN];
    const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
    let keys = secret(7);
    let me = node(3);
    let op_line = proof_line("survey", &tag(&keys.client, E, "survey"));
    let forged = |named: &NodeIdentity, signer: &NodeIdentity, end: Signer, exporter: &[u8]| {
        format!(
            "node {} {} {}\n",
            named.node(),
            named.key().hex(),
            hex::encode_lower(&signer.sign(end, exporter))
        )
    };

    let listener_hears = |line: String| {
        let (mut listener, mut dialer) = CtlStream::pair().unwrap();
        dialer.set_read_timeout(Some(HANG)).unwrap();
        let listening = std::thread::spawn(move || {
            listener.set_read_timeout(Some(HANG)).unwrap();
            listener_proves(
                &mut listener,
                E,
                (&secret(7), Some(&node(9)), true),
                FleetScope::Facts,
                EVERY_OP,
                Box::<Denied>::default,
                |_| true,
            )
        });
        let _hello = read_line(&mut dialer).unwrap();
        dialer
            .write_all(format!("{op_line}{line}").as_bytes())
            .unwrap();
        let answer = read_line(&mut dialer).unwrap();
        (listening.join().unwrap(), answer)
    };
    let (heard, answer) = listener_hears(node_line(Some(&me), Signer::Dialer, E));
    assert_eq!(answer.as_deref(), Some("OK"));
    assert!(
        matches!(&heard, Ok((FleetOp::Survey, Some(proof))) if proof.node == me.node() && proof.key == me.key()),
        "{heard:?}"
    );
    let (heard, answer) = listener_hears("node -\n".to_owned());
    assert!(matches!(heard, Ok((FleetOp::Survey, None))), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("OK"));
    for refused in [
        forged(&me, &me, Signer::Dialer, OTHER),
        forged(&me, &me, Signer::Listener, E),
        forged(&me, &node(4), Signer::Dialer, E),
    ] {
        let (heard, answer) = listener_hears(refused.clone());
        assert!(
            matches!(heard, Err(FleetError::NodeProof)),
            "{refused:?}: {heard:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO node"));
    }
    for malformed in ["node\n", "node 1234 5678 9abc\n", "node - -\n"] {
        let (heard, answer) = listener_hears(malformed.to_owned());
        assert!(
            matches!(heard, Err(FleetError::Malformed(_))),
            "{malformed:?}: {heard:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO node"));
    }

    let dialer_hears = |line: String| {
        let (mut dialer, mut listener) = CtlStream::pair().unwrap();
        listener.set_read_timeout(Some(HANG)).unwrap();
        let dialing = std::thread::spawn(move || {
            dialer.set_read_timeout(Some(HANG)).unwrap();
            dialer_proves(
                &mut dialer,
                E,
                (&secret(7), Some(&node(9)), true),
                FleetOp::Survey,
                &mut None,
            )
        });
        let hello = proof_line("facts", &tag(&keys.server, E, "facts"));
        listener.write_all(hello.as_bytes()).unwrap();
        let sent = (
            read_line(&mut listener).unwrap(),
            read_line_within(&mut listener, MAX_NODE_LINE).unwrap(),
        );
        listener
            .write_all(format!("OK\n{line}").as_bytes())
            .unwrap();
        drop(listener);
        (dialing.join().unwrap(), sent)
    };
    let (heard, sent) = dialer_hears(node_line(Some(&me), Signer::Listener, E));
    assert!(
        matches!(&heard, Ok((FleetScope::Facts, Some(proof))) if proof.node == me.node()),
        "{heard:?}"
    );
    assert_eq!(sent.0.as_deref(), Some(op_line.trim_end()));
    assert_eq!(
        sent.1,
        Some(
            node_line(Some(&node(9)), Signer::Dialer, E)
                .trim_end()
                .to_owned()
        ),
        "the dialer's node goes with its op"
    );
    for refused in [
        forged(&me, &me, Signer::Listener, OTHER),
        forged(&me, &me, Signer::Dialer, E),
        forged(&me, &node(4), Signer::Listener, E),
    ] {
        let (heard, _) = dialer_hears(refused.clone());
        assert!(
            matches!(heard, Err(FleetError::NodeProof)),
            "{refused:?}: {heard:?}"
        );
    }
}

#[test]
fn a_wrong_secret_is_named_by_the_end_that_sees_it() {
    let (listened, dialed) = over_loopback(
        |tcp| server_handshake(tcp, &secret(2), FleetScope::Facts, EVERY_OP, HANG).err(),
        |tcp| client_handshake(tcp, &secret(1), FleetOp::Survey, HANG).err(),
    );
    assert!(
        matches!(dialed, Some(FleetError::ListenerWrongSecret)),
        "{dialed:?}"
    );
    assert!(
        matches!(listened, Some(FleetError::DialerHungUp)),
        "{listened:?}"
    );

    let (listened, answer) = over_loopback(
        |tcp| server_handshake(tcp, &secret(2), FleetScope::Facts, EVERY_OP, HANG).err(),
        |tcp| {
            let mut t = raw_connect(tcp);
            let exporter = t.exporter().to_vec();
            let _hello = read_line(t.stream()).unwrap();
            let word = FleetOp::Survey.word();
            write_proof(t.stream(), word, &tag(&secret(1).client, &exporter, word)).unwrap();
            read_line(t.stream()).unwrap()
        },
    );
    assert!(
        matches!(listened, Some(FleetError::DialerWrongSecret)),
        "{listened:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO proof"));
}

#[test]
fn a_reflected_proof_is_refused() {
    let (listened, (reflected, exporter, answer)) = over_loopback(
        |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, HANG).err(),
        |tcp| {
            let mut t = raw_connect(tcp);
            let exporter = t.exporter().to_vec();
            let hello = read_line(t.stream()).unwrap().unwrap();
            t.stream()
                .write_all(format!("{hello}\n").as_bytes())
                .unwrap();
            t.stream().flush().unwrap();
            let (_, reflected) = parse_proof(&hello).unwrap();
            (reflected, exporter, read_line(t.stream()).unwrap())
        },
    );
    assert!(
        matches!(listened, Some(FleetError::Reflection)),
        "{listened:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO proof"));
    assert!(verifies(&secret(7).server, &exporter, "facts", &reflected));
    assert!(!verifies(&secret(7).client, &exporter, "facts", &reflected));

    let (sent, dialed) = over_loopback(
        |tcp| {
            let mut t = raw_accept(tcp);
            let exporter = t.exporter().to_vec();
            let word = FleetScope::Facts.word();
            write_proof(t.stream(), word, &tag(&secret(7).client, &exporter, word)).unwrap();
            drain(t.stream())
        },
        |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
    );
    assert!(matches!(dialed, Some(FleetError::Reflection)), "{dialed:?}");
    assert!(
        sent.is_empty(),
        "the dialer sent {sent:?} to an unproved listener"
    );

    let within = Duration::from_millis(500);
    let (echoed, dialed) = over_loopback(
        |tcp| {
            tcp.set_read_timeout(Some(HANG)).unwrap();
            let Ok((mut t, _)) = accept_tls(tcp, server_config()) else {
                return Vec::new();
            };
            let got = drain(t.stream());
            let _ = t.stream().write_all(&got);
            got
        },
        |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, within).err(),
    );
    assert!(
        matches!(dialed, Some(FleetError::Timeout(w)) if w == within),
        "{dialed:?}"
    );
    assert!(echoed.is_empty(), "the dialer sent {echoed:?} to an echo");
}

#[test]
fn a_tag_proves_only_its_own_word_on_its_own_channel() {
    const E: &[u8] = &[0xa5; EXPORTER_LEN];
    const OTHER: &[u8] = &[0x5a; EXPORTER_LEN];
    let keys = secret(7);

    let listener_hears = |line: String| {
        let (mut listener, mut dialer) = CtlStream::pair().unwrap();
        dialer.set_read_timeout(Some(HANG)).unwrap();
        let listening = std::thread::spawn(move || {
            listener.set_read_timeout(Some(HANG)).unwrap();
            listener_proves(
                &mut listener,
                E,
                (&secret(7), None, false),
                FleetScope::Content,
                EVERY_OP,
                Box::<Denied>::default,
                |_| true,
            )
            .map(|(op, _)| op)
        });
        let _hello = read_line(&mut dialer).unwrap();
        dialer.write_all(line.as_bytes()).unwrap();
        let answer = read_line(&mut dialer).unwrap();
        (listening.join().unwrap(), answer)
    };
    let proof = |key: &[u8; 32], exporter: &[u8], tagged: &str, sent: &str| {
        format!(
            "{sent} {}\n",
            hex::encode_lower(&tag(key, exporter, tagged))
        )
    };
    let (heard, answer) = listener_hears(proof(&keys.client, E, "survey", "survey"));
    assert!(matches!(heard, Ok(FleetOp::Survey)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("OK"));
    let (heard, answer) = listener_hears(proof(&keys.join, E, "join", "join"));
    assert!(matches!(heard, Ok(FleetOp::Join)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("OK"));
    let (heard, answer) = listener_hears(proof(&keys.client, E, "book", "book"));
    assert!(matches!(heard, Ok(FleetOp::Book)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("OK"));
    let (heard, answer) = listener_hears(proof(&keys.client, E, "drive", "drive"));
    assert!(matches!(heard, Ok(FleetOp::Drive)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("OK"));
    for refused in [
        proof(&keys.client, E, "survey", "drive"),
        proof(&keys.client, E, "drive", "survey"),
        proof(&keys.join, E, "drive", "drive"),
        proof(&keys.client, E, "survey", "join"),
        proof(&keys.join, E, "join", "survey"),
        proof(&keys.client, E, "survey", "book"),
        proof(&keys.client, E, "book", "survey"),
        proof(&keys.client, E, "join", "join"),
        proof(&keys.client, OTHER, "survey", "survey"),
    ] {
        let (heard, answer) = listener_hears(refused.clone());
        assert!(
            matches!(heard, Err(FleetError::DialerWrongSecret)),
            "{refused:?}: {heard:?}"
        );
        assert_eq!(answer.as_deref(), Some("NO proof"));
    }
    let (heard, answer) = listener_hears(proof(&keys.client, E, "steer", "steer"));
    assert!(
        matches!(&heard, Err(FleetError::UnknownOp(op)) if op == "steer"),
        "{heard:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO op"));
    let (heard, answer) = listener_hears(format!("survey {}\n", "AB".repeat(32)));
    assert!(matches!(heard, Err(FleetError::Malformed(_))), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO proof"));

    let dialer_hears = |hello: String, answer: &'static str| {
        let (mut dialer, mut listener) = CtlStream::pair().unwrap();
        listener.set_read_timeout(Some(HANG)).unwrap();
        let dialing = std::thread::spawn(move || {
            dialer.set_read_timeout(Some(HANG)).unwrap();
            dialer_proves(
                &mut dialer,
                E,
                (&secret(7), None, false),
                FleetOp::Survey,
                &mut None,
            )
            .map(|(scope, _)| scope)
        });
        listener.write_all(hello.as_bytes()).unwrap();
        let sent = read_line(&mut listener).unwrap();
        if sent.is_some() {
            listener.write_all(answer.as_bytes()).unwrap();
        }
        drop(listener);
        (dialing.join().unwrap(), sent)
    };
    let (heard, sent) = dialer_hears(proof(&keys.server, E, "content", "content"), "OK\n");
    assert!(matches!(heard, Ok(FleetScope::Content)), "{heard:?}");
    assert_eq!(
        sent,
        Some(
            proof(&keys.client, E, "survey", "survey")
                .trim_end()
                .to_owned()
        )
    );
    for refused in [
        proof(&keys.server, E, "content", "facts"),
        proof(&keys.server, OTHER, "facts", "facts"),
    ] {
        let (heard, sent) = dialer_hears(refused.clone(), "OK\n");
        assert!(
            matches!(heard, Err(FleetError::ListenerWrongSecret)),
            "{refused:?}: {heard:?}"
        );
        assert_eq!(
            sent, None,
            "the dialer sent its proof to an unproved listener"
        );
    }
    let hello = proof(&keys.server, E, "facts", "facts");
    let (heard, _) = dialer_hears(hello.clone(), "NO proof\n");
    assert!(matches!(heard, Err(FleetError::Refused)), "{heard:?}");
    let (heard, _) = dialer_hears(hello.clone(), "NO op\n");
    assert!(
        matches!(&heard, Err(FleetError::UnknownOp(op)) if op == "survey"),
        "{heard:?}"
    );
    let (heard, _) = dialer_hears(hello, "MAYBE\n");
    assert!(matches!(heard, Err(FleetError::Malformed(_))), "{heard:?}");
    let (heard, sent) = dialer_hears(proof(&keys.server, E, "drive", "drive"), "OK\n");
    assert!(
        matches!(&heard, Err(FleetError::UnknownScope(s)) if s == "drive"),
        "{heard:?}"
    );
    assert_eq!(sent, None);
}

/// A listener that has not read its deny list refuses a dialer that proved the fleet secret
/// with `NO capacity` on the wire, naming [`FleetError::DenyListUnread`] to itself, so every
/// dialer, one older than this build included, reads it as busy ([`FleetError::AddressBusy`])
/// and dials again; a dialer from this build on reads `NO unread` as
/// [`FleetError::DenyListUnread`].
///
/// NEGATIVE CONTROL (2026-11-01): with the listener answering `NO unread` (the review's
/// finding: a dialer older than this build reads it as a refused proof, "another fleet's
/// secret", and disowns the node), this fails (`left: Some("NO unread"), right: Some("NO
/// capacity")`).
#[test]
fn an_unread_deny_list_answers_busy_on_the_wire() {
    const E: &[u8] = &[0xa5; EXPORTER_LEN];
    let keys = secret(7);
    let (mut listener, mut dialer) = CtlStream::pair().unwrap();
    dialer.set_read_timeout(Some(HANG)).unwrap();
    let listening = std::thread::spawn(move || {
        listener.set_read_timeout(Some(HANG)).unwrap();
        listener_proves(
            &mut listener,
            E,
            (&secret(7), None, false),
            FleetScope::Content,
            EVERY_OP,
            || Box::new(Denied::not_yet_read()),
            |_| true,
        )
    });
    let _hello = read_line(&mut dialer).unwrap();
    dialer
        .write_all(proof_line("survey", &tag(&keys.client, E, "survey")).as_bytes())
        .unwrap();
    let answer = read_line(&mut dialer).unwrap();
    let heard = listening.join().unwrap();
    assert!(
        matches!(heard, Err(FleetError::DenyListUnread)),
        "{heard:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO capacity"));

    let busy = |e: &FleetError| matches!(e, FleetError::AddressBusy);
    let unread = |e: &FleetError| matches!(e, FleetError::DenyListUnread);
    for (said, read) in [
        ("NO capacity\n", &busy as &dyn Fn(&FleetError) -> bool),
        ("NO unread\n", &unread),
    ] {
        let (mut dialer, mut listener) = CtlStream::pair().unwrap();
        listener.set_read_timeout(Some(HANG)).unwrap();
        let dialing = std::thread::spawn(move || {
            dialer.set_read_timeout(Some(HANG)).unwrap();
            dialer_proves(
                &mut dialer,
                E,
                (&secret(7), None, false),
                FleetOp::Survey,
                &mut None,
            )
            .map(|(scope, _)| scope)
        });
        listener
            .write_all(proof_line("facts", &tag(&keys.server, E, "facts")).as_bytes())
            .unwrap();
        let _proof = read_line(&mut listener).unwrap();
        listener.write_all(said.as_bytes()).unwrap();
        drop(listener);
        let heard = dialing.join().unwrap();
        assert!(
            heard.as_ref().err().is_some_and(read),
            "{said:?}: {heard:?}"
        );
    }
}

fn listener_tls(cert: &[u8], key: &[u8], alpn: &[&[u8]]) -> Arc<ServerConfig> {
    tls::init_crypto();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
    let mut cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert.to_vec())], key)
        .unwrap();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Arc::new(cfg)
}

fn dialer_tls(alpn: &[&[u8]]) -> Arc<ClientConfig> {
    let mut cfg = (*client_config()).clone();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Arc::new(cfg)
}

fn raw_tls<C, S>(mut conn: C, mut tcp: TcpStream)
where
    C: std::ops::DerefMut<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    tcp.set_read_timeout(Some(HANG)).unwrap();
    while conn.is_handshaking() {
        if conn.complete_io(&mut tcp).is_err() {
            return;
        }
    }
    let _ = drain(&mut StreamOwned::new(conn, tcp));
}

#[test]
fn a_peer_without_a_fleet_protocol_is_named_so_from_either_end() {
    const NET_CERT: &[u8] = include_bytes!("testdata/cert.der");
    const NET_KEY: &[u8] = include_bytes!("testdata/key.pkcs8.der");
    let listeners = [
        listener_tls(NET_CERT, NET_KEY, &[]),
        listener_tls(CERT_DER, KEY_DER, &[]),
        listener_tls(CERT_DER, KEY_DER, &[b"atf/3"]),
    ];
    for (i, listener) in listeners.into_iter().enumerate() {
        let ((), dialed) = over_loopback(
            move |tcp| raw_tls(ServerConnection::new(listener).unwrap(), tcp),
            |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
        );
        assert!(
            matches!(dialed, Some(FleetError::Alpn)),
            "listener {i}: {dialed:?}"
        );
    }
    for offered in [&[][..], &[&b"h2"[..]][..]] {
        let dialer = dialer_tls(offered);
        let (listened, ()) = over_loopback(
            |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, HANG).err(),
            move |tcp| {
                let name = ServerName::try_from(SERVER_NAME).unwrap();
                raw_tls(ClientConnection::new(dialer, name).unwrap(), tcp);
            },
        );
        assert!(
            matches!(listened, Some(FleetError::Alpn)),
            "{offered:?}: {listened:?}"
        );
    }
    let other = listener_tls(NET_CERT, NET_KEY, &[ALPN]);
    let ((), dialed) = over_loopback(
        move |tcp| raw_tls(ServerConnection::new(other).unwrap(), tcp),
        |tcp| client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).err(),
    );
    assert!(
        matches!(dialed, Some(FleetError::Tls(_))),
        "another certificate under atf/1 is a TLS failure: {dialed:?}"
    );
}

#[test]
fn a_dialer_that_proves_nothing_in_time_is_timed_out() {
    let within = Duration::from_millis(500);
    let (listened, ()) = over_loopback(
        move |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, EVERY_OP, within).err(),
        |tcp| {
            if let Ok((mut t, _)) = connect_tls(tcp, client_config()) {
                let _ = drain(t.stream());
            }
        },
    );
    assert!(
        matches!(listened, Some(FleetError::Timeout(w)) if w == within),
        "{listened:?}"
    );
}

#[test]
fn an_op_the_listener_does_not_serve_is_answered_no_op() {
    const OLDER: &[FleetOp] = &[FleetOp::Survey, FleetOp::Join];
    const READ_ONLY: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];
    for (serves, op) in [
        (&[FleetOp::Survey][..], FleetOp::Join),
        (OLDER, FleetOp::Book),
        (READ_ONLY, FleetOp::Drive),
    ] {
        let (listened, dialed) = over_loopback(
            move |tcp| server_handshake(tcp, &secret(7), FleetScope::Facts, serves, HANG).err(),
            move |tcp| client_handshake(tcp, &secret(7), op, HANG).err(),
        );
        assert!(
            matches!(&listened, Some(FleetError::UnknownOp(word)) if word == op.word()),
            "{listened:?}"
        );
        assert!(
            matches!(&dialed, Some(FleetError::UnknownOp(word)) if word == op.word()),
            "{dialed:?}"
        );
    }
}

#[test]
fn a_refusal_proves_the_node_that_refused() {
    const READ_ONLY: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];
    for (listener, full) in [(Some(node(1)), false), (Some(node(1)), true), (None, false)] {
        let want = listener.as_ref().map(|me| NodeProof {
            node: me.node().to_owned(),
            key: me.key(),
        });
        let ((), dialed) = over_loopback(
            move |tcp| {
                let watchdog = arm(&tcp, HANG).unwrap();
                let me = (&secret(7), listener.as_ref());
                let scope = FleetScope::Facts;
                let _ = server_handshake_armed(
                    tcp,
                    watchdog,
                    HANG,
                    me,
                    scope,
                    READ_ONLY,
                    &Denied::default(),
                    |_| !full,
                    server_config(),
                );
            },
            |tcp| {
                let op = if full {
                    FleetOp::Survey
                } else {
                    FleetOp::Drive
                };
                client_handshake_by(tcp, &secret(7), Some(&node(2)), op, HANG).err()
            },
        );
        let Some(Undialed { error, refused_by }) = dialed else {
            panic!("the listener served it");
        };
        assert!(
            matches!(
                (&error, full),
                (FleetError::UnknownOp(_), false) | (FleetError::AddressBusy, true)
            ),
            "{error:?}"
        );
        assert_eq!(refused_by, want, "{error:?}");
    }
}

#[test]
fn only_loopback_private_and_link_local_sources_are_served() {
    for served in [
        "127.0.0.1",
        "127.255.0.9",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.1.20",
        "169.254.10.10",
        "::1",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "febf::1",
        "::ffff:192.168.1.20",
        "::ffff:127.0.0.1",
    ] {
        assert!(
            is_fleet_source(served.parse().unwrap()),
            "{served} is served"
        );
    }
    for refused in [
        "8.8.8.8",
        "172.15.255.255",
        "172.32.0.1",
        "192.169.0.1",
        "100.64.0.1",
        "198.51.100.7",
        "224.0.0.251",
        "255.255.255.255",
        "0.0.0.0",
        "2001:db8::1",
        "2606:4700::1111",
        "fec0::1",
        "ff02::1",
        "::",
        "::ffff:8.8.8.8",
    ] {
        assert!(
            !is_fleet_source(refused.parse().unwrap()),
            "{refused} is refused"
        );
    }
}

#[test]
fn a_key_file_is_lowercase_hex_on_one_line() {
    let secret = [0xa7_u8; 20];
    let hex = hex::encode_lower(&secret);
    for text in [hex.clone(), format!("{hex}\n"), "5c".repeat(32)] {
        assert!(parse_key_file(text.as_bytes()).is_ok(), "{text:?} reads");
    }
    assert_eq!(
        parse_key_file(format!("{hex}\n").as_bytes())
            .unwrap()
            .server,
        FleetSecret::new(&secret).unwrap().server
    );
    for text in [
        String::new(),
        "ab".repeat(19),
        "ab".repeat(33),
        format!("{hex}0"),
        hex.to_uppercase(),
        format!("{hex}\n\n"),
        format!(" {hex}"),
        format!("{hex}\r\n"),
        format!("{}zz", &hex[..38]),
    ] {
        assert!(
            matches!(
                parse_key_file(text.as_bytes()),
                Err(KeyFileError::Malformed(_))
            ),
            "{text:?} is refused"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_key_file_is_read_only_when_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &std::path::Path, bits: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    let root = std::env::temp_dir().join(format!("aterm-fleet-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    mode(&root, 0o700);
    mode(&state, 0o700);
    let word = |dir: &std::path::Path| read_key_file(dir).err().map(|e| e.word());

    assert_eq!(word(&state), Some("no-key"));
    assert_eq!(word(&root.join("absent")), Some("no-key"));
    mode(&state, 0o755);
    assert_eq!(word(&state), Some("no-key"));
    mode(&state, 0o700);
    let key = state.join(KEY_FILE);
    std::fs::write(&key, format!("{}\n", "3e".repeat(20))).unwrap();
    mode(&key, 0o600);
    assert_eq!(word(&state), None, "a private key file reads");

    mode(&key, 0o640);
    assert_eq!(word(&state), Some("key-mode"));
    mode(&key, 0o604);
    assert_eq!(word(&state), Some("key-mode"));
    mode(&key, 0o600);
    mode(&state, 0o750);
    assert_eq!(word(&state), Some("key-mode"));
    mode(&state, 0o700);

    let second = root.join("second-link");
    std::fs::hard_link(&key, &second).unwrap();
    assert_eq!(word(&state), Some("key-mode"));
    std::fs::remove_file(&second).unwrap();
    assert_eq!(word(&state), None);

    let real = root.join("real.key");
    std::fs::rename(&key, &real).unwrap();
    std::os::unix::fs::symlink(&real, &key).unwrap();
    assert_eq!(word(&state), Some("key-link"));
    std::fs::remove_file(&key).unwrap();
    std::fs::rename(&real, &key).unwrap();
    let linked = root.join("linked-state");
    std::os::unix::fs::symlink(&state, &linked).unwrap();
    assert_eq!(word(&linked), Some("key-link"));
    assert_eq!(word(&state), None);

    std::fs::remove_file(&key).unwrap();
    std::fs::create_dir(&key).unwrap();
    mode(&key, 0o700);
    assert_eq!(word(&state), Some("key-format"));
    std::fs::remove_dir(&key).unwrap();
    std::fs::write(&key, "ab".repeat(64)).unwrap();
    mode(&key, 0o600);
    assert_eq!(word(&state), Some("key-format"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_listener_hands_proved_streams_over_and_cuts_them_when_stopped() {
    use std::sync::mpsc;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let running = Arc::new(Stop::new());
    let (returned, returns) = mpsc::channel();
    let (events, heard) = mpsc::channel::<FleetEvent>();
    let events = Arc::new(Mutex::new(events));
    let listening = Listening {
        secret: secret(7),
        scope: FleetScope::Content,
        node: None,
        serves: &[FleetOp::Survey],
        slots: 1,
        per_source: 1,
        lifetime: HANG,
    };
    let idle_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let idle_addr = idle_listener.local_addr().unwrap();
    let idle_listening = listening.clone();
    let (admitted, admission) = mpsc::channel();
    let idle = {
        let stop = Arc::clone(&running);
        let returned = returned.clone();
        std::thread::spawn(move || {
            serve_with_handshake(
                &idle_listener,
                &idle_listening,
                &stop,
                HANG,
                |served, _| served.answer(echo_all),
                move |event| {
                    if matches!(event, FleetEvent::Admitted { .. }) {
                        let _ = admitted.send(());
                    }
                },
            );
            let _ = returned.send(());
        })
    };
    let serving = {
        let running = Arc::clone(&running);
        std::thread::spawn(move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                |served, _op| served.answer(echo_all),
                move |event| {
                    let _ = events.lock().unwrap().send(event);
                },
            );
            let _ = returned.send(());
        })
    };
    let next = |want: &str| loop {
        let event = heard
            .recv_timeout(HANG)
            .expect("an event within the hang detector");
        let name = format!("{event:?}");
        if name.starts_with(want) {
            return event;
        }
    };

    let tcp = TcpStream::connect(addr).unwrap();
    let (mut held, scope) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    assert_eq!(scope, FleetScope::Content);
    assert!(matches!(
        next("Admitted"),
        FleetEvent::Admitted {
            op: FleetOp::Survey,
            ..
        }
    ));
    held.stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    held.stream().write_all(b"ping\n").unwrap();
    held.stream().flush().unwrap();
    let mut echo = [0u8; 5];
    held.stream().read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping\n");

    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(HANG)).unwrap();
    assert!(client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).is_err());
    assert!(matches!(next("Refused"), FleetEvent::Refused { reason, .. } if reason == AT_CAPACITY));

    // Both actual listeners have registered this generation before it stops. Each must
    // receive the broadcast itself; one listener cannot consume the other's wake byte.
    let tcp = TcpStream::connect(idle_addr).unwrap();
    let idle_client = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    admission
        .recv_timeout(HANG)
        .expect("the second listener proved a connection");
    drop(idle_client);
    running.set();
    let stopped = returns.recv_timeout(HANG).is_ok() && returns.recv_timeout(HANG).is_ok();
    if !stopped {
        // Release an owned native wait even with a broken control-wake producer, so all
        // threads are joined before the assertion reports that lost wake.
        let _ = TcpStream::connect_timeout(&addr, HANG);
        let _ = TcpStream::connect_timeout(&idle_addr, HANG);
    }
    serving.join().unwrap();
    idle.join().unwrap();
    assert!(
        stopped,
        "one stop wakes both listeners without another connection"
    );
    let mut rest = Vec::new();
    let ended = held.stream().read_to_end(&mut rest);
    assert!(
        rest.is_empty()
            && !ended.is_err_and(|e| matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
        "the handed-over stream is cut: {rest:?}"
    );
    assert!(matches!(next("Closed"), FleetEvent::Closed { .. }));
}

#[test]
fn a_peer_that_read_its_end_dials_again_at_once() {
    for (slots, per_source) in [(1, 1), (2, 1)] {
        for end in [End::Relayed, End::Answered] {
            let served = redial(slots, per_source, end);
            assert_eq!(
                served.as_deref().ok(),
                Some("answered"),
                "{end:?}: {slots} slots, a share of {per_source}: {served:?}"
            );
        }
        let held = redial(slots, per_source, End::Kept);
        let refused = match &held {
            Err(FleetError::AddressBusy) => per_source < slots,
            Err(_) => slots == 1,
            Ok(_) => false,
        };
        assert!(
            refused,
            "the control: {slots} slots, a share of {per_source}: {held:?}"
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum End {
    Relayed,
    Answered,
    Kept,
}

fn redial(slots: usize, per_source: usize, end: End) -> Result<String, FleetError> {
    use std::sync::mpsc;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let running = Arc::new(Stop::new());
    let (go, gate) = mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(gate));
    let listening = Listening {
        secret: secret(7),
        scope: FleetScope::Facts,
        node: None,
        serves: &[FleetOp::Survey],
        slots,
        per_source,
        lifetime: HANG,
    };
    let serving = {
        let running = Arc::clone(&running);
        std::thread::spawn(move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                move |served: Served, _op| {
                    let (answered, kept) = match end {
                        End::Answered => (
                            served.answer(|stream| {
                                let mut asked = [0u8; 5];
                                stream.read_exact(&mut asked)?;
                                stream.write_all(b"answered\n")
                            }),
                            None,
                        ),
                        End::Relayed | End::Kept => {
                            let (local, mut answering) = CtlStream::pair()?;
                            let answerer = std::thread::spawn(move || {
                                let mut asked = [0u8; 5];
                                answering.read_exact(&mut asked)?;
                                answering.write_all(b"answered\n")?;
                                answering.flush()?;
                                aterm_uds::hangup::hang_up(&answering);
                                Ok::<(), io::Error>(())
                            });
                            let (relayed, kept) = match end {
                                End::Kept => {
                                    let Served {
                                        transport, release, ..
                                    } = served;
                                    (
                                        tls::relay_answers(
                                            transport,
                                            local,
                                            (),
                                            Arc::new(|| Ok(())),
                                        ),
                                        Some(release),
                                    )
                                }
                                _ => (served.relay_answers(local), None),
                            };
                            let _ = answerer.join();
                            (relayed, kept)
                        }
                    };
                    let _ = gate.lock().unwrap().recv_timeout(HANG);
                    drop(kept);
                    answered
                },
                |_: FleetEvent| {},
            );
        })
    };
    let dial = || {
        let tcp = TcpStream::connect(addr).map_err(FleetError::Io)?;
        let (mut proved, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)?;
        proved
            .stream()
            .get_ref()
            .set_read_timeout(Some(HANG))
            .map_err(FleetError::Io)?;
        Ok::<_, FleetError>(proved)
    };
    let ask = |proved: &mut TlsTransport<ClientConnection>| {
        proved.stream().write_all(b"ping\n")?;
        proved.stream().flush()?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while proved.stream().read(&mut byte)? == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        Ok::<_, io::Error>(String::from_utf8_lossy(&line).into_owned())
    };
    let mut first = dial().expect("the first dial is served");
    assert_eq!(ask(&mut first).expect("its answer"), "answered");
    let mut rest = Vec::new();
    let end = first.stream().read_to_end(&mut rest);
    assert!(
        end.is_ok() && rest.is_empty(),
        "the first connection ends cleanly: {end:?} {rest:?}"
    );
    let second = dial().and_then(|mut proved| ask(&mut proved).map_err(FleetError::Io));
    drop(go);
    running.set();
    serving.join().unwrap();
    second
}

#[cfg(unix)]
#[test]
fn a_relay_attached_after_its_connection_was_cut_is_closed() {
    drop(over_loopback(
        |tcp| {
            let live = Arc::new(LiveConnections::default());
            let wake = Wake::new().unwrap();
            live.set_wake(&wake.signal);
            let _late_signal = Arc::clone(&wake.signal);
            let tenure = live.hold(1, tcp.peer_addr().unwrap().ip(), tcp);
            live.cut_all();
            drop(wake);
            let (local, mut peer) = aterm_uds::kept_out(CtlStream::pair).unwrap();
            peer.set_nonblocking(true).unwrap();
            tenure.relays_to(aterm_uds::kept_out(|| local.try_clone()).unwrap());
            assert!(
                matches!(peer.read(&mut [0]), Ok(0)),
                "a cut must also close a relay attached after its live entry was removed"
            );
            // The late worker's final deadline notification also outlives the native
            // receiver safely; dropping its tenure must not write to a closed peer.
            drop(tenure);
        },
        |tcp| tcp,
    ));
}

/// A listener clock that stands still until its test moves it ([`Still::pass`]): the
/// [`LiveConnections`] it makes ([`Still::live`]) stamp each proof and cut each lifetime on
/// it, so no scheduling delay of the listener's threads ever spends a connection's lifetime.
#[derive(Clone)]
struct Still {
    past: Arc<Mutex<Duration>>,
    live: Arc<LiveConnections>,
}

impl Still {
    fn new() -> Self {
        let base = Instant::now();
        let past = Arc::new(Mutex::new(Duration::ZERO));
        let live = Arc::new(LiveConnections::on_clock({
            let past = Arc::clone(&past);
            move || base + *past.lock().unwrap()
        }));
        Self { past, live }
    }

    fn pass(&self, by: Duration) {
        *self.past.lock().unwrap() += by;
        self.live.changed();
    }

    fn live(&self) -> Arc<LiveConnections> {
        Arc::clone(&self.live)
    }
}

/// The lifetime runs on the listener's clock from the proof's stamp: a connection held an
/// hour before it proved is not overdue, one a millisecond short of its lifetime is held, and
/// one at its lifetime is cut, its peer reading the end. As the listener does, `live` holds a
/// duplicate of the accepted socket while the accepted descriptor stays open through the
/// peer's read, so only the cut's hang-up, not the dropped entry, can end the line.
#[test]
fn a_proof_starts_its_lifetime_on_the_listeners_clock() {
    let lifetime = SERVED_WITHIN;
    let (accepted, mut peer) = over_loopback(
        move |tcp| {
            let clock = Still::new();
            let live = clock.live();
            let held = || live.lock().len();
            let line = aterm_uds::kept_out(|| tcp.try_clone()).unwrap();
            let tenure = live.hold(1, tcp.peer_addr().unwrap().ip(), line);
            clock.pass(Duration::from_secs(3600));
            assert!(
                tenure.proved(false, 1),
                "the only connection proves within its share"
            );
            let _ = live.cut_overdue(lifetime);
            assert_eq!(
                held(),
                1,
                "its lifetime starts at the proof, not the accept"
            );
            clock.pass(lifetime - Duration::from_millis(1));
            let _ = live.cut_overdue(lifetime);
            assert_eq!(held(), 1, "a millisecond short of its lifetime it is held");
            clock.pass(Duration::from_millis(1));
            let _ = live.cut_overdue(lifetime);
            assert_eq!(held(), 0, "at its lifetime it is cut");
            drop(tenure);
            tcp
        },
        |tcp| tcp,
    );
    let end = peer.read(&mut [0]);
    assert!(
        matches!(end, Ok(0)),
        "the cut connection's peer reads its end while the listener's descriptor is open: {end:?}"
    );
    drop(accepted);
}

#[test]
fn the_cut_frees_the_slot_of_a_peer_that_sent_its_fin_and_reads_nothing() {
    the_cut_frees_the_slot_of(|local, answering| {
        let answerer = std::thread::spawn(move || {
            let chunk = vec![b'a'; 64 * 1024];
            while (&answering).write_all(&chunk).is_ok() {}
        });
        let relayed = local();
        let _ = answerer.join();
        relayed
    });
}

/// A local server that never answers holds its end open: the cut reaches that end too, or the
/// relay outlives it and keeps the slot.
#[test]
fn the_cut_frees_the_slot_of_a_relay_whose_local_end_stays_silent() {
    the_cut_frees_the_slot_of(|local, answering| {
        let relayed = local();
        drop(answering);
        relayed
    });
}

/// One slot, a lifetime that passes only on the test's [`Still`] clock, and a member that
/// sends its FIN and reads nothing: `relaying` runs the relay (its first argument) beside the
/// local end that answers it. The connection closes once its lifetime has passed and the next
/// dial is served.
fn the_cut_frees_the_slot_of(
    relaying: impl Fn(Box<dyn FnOnce() -> io::Result<()>>, CtlStream) -> io::Result<()>
    + Send
    + Sync
    + Clone
    + 'static,
) {
    use std::sync::mpsc;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let running = Arc::new(Stop::new());
    let (events, heard) = mpsc::channel::<FleetEvent>();
    let events = Arc::new(Mutex::new(events));
    let listening = Listening {
        secret: secret(7),
        scope: FleetScope::Facts,
        node: None,
        serves: &[FleetOp::Survey],
        slots: 1,
        per_source: 1,
        lifetime: SERVED_WITHIN,
    };
    let clock = Still::new();
    let serving = {
        let running = Arc::clone(&running);
        let live = clock.live();
        std::thread::spawn(move || {
            serve_with_handshake_on(
                &listener,
                &listening,
                &running,
                HANG,
                (
                    move |served: Served, _op| {
                        let (local, answering) = CtlStream::pair()?;
                        relaying(Box::new(move || served.relay_answers(local)), answering)
                    },
                    move |event| {
                        let _ = events.lock().unwrap().send(event);
                    },
                ),
                (Arc::new(Slots::new(1, PREAUTH_GRACE)), live),
            );
        })
    };
    let next = |want: &str| loop {
        let event = heard
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("{want} within the hang detector"));
        if format!("{event:?}").starts_with(want) {
            return event;
        }
    };

    let tcp = TcpStream::connect(addr).unwrap();
    let (silent, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    // Admitted is told once the listener's handshake is over: a lifetime that passed before
    // it would cut the handshake, not the relay.
    assert!(matches!(next("Admitted"), FleetEvent::Admitted { .. }));
    silent.tcp().shutdown(std::net::Shutdown::Write).unwrap();
    clock.pass(SERVED_WITHIN);
    assert!(matches!(next("Closed"), FleetEvent::Closed { .. }));
    // Proved where the clock now stands still, the next dial is never overdue.
    let tcp = TcpStream::connect(addr).unwrap();
    let again = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG);
    assert!(
        again.is_ok(),
        "the next dial is served while the silent member holds its connection: {:?}",
        again.err()
    );
    drop(again);
    running.set();
    serving.join().unwrap();
    drop(silent);
}

#[derive(Default)]
struct Heard {
    refused: Mutex<Vec<String>>,
    at_capacity: std::sync::atomic::AtomicUsize,
    displaced: std::sync::atomic::AtomicUsize,
    refused_v6: Mutex<Vec<String>>,
    admitted: std::sync::atomic::AtomicUsize,
    closed: std::sync::atomic::AtomicUsize,
}

struct Serving {
    heard: Arc<Heard>,
    /// The listener's slots where [`Serving::start_on`] made them, read to see
    /// who holds them ([`Serving::seats`]).
    slots: Option<Arc<Slots>>,
    running: Arc<Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Serving {
    fn start(listener: TcpListener, listening: Listening) -> Self {
        Self::start_on(listener, listening, Arc::default())
    }

    /// [`Serving::start`] holding its connections in `live`, whose clock runs their lifetimes.
    fn start_on(listener: TcpListener, listening: Listening, live: Arc<LiveConnections>) -> Self {
        let heard = Arc::new(Heard::default());
        let slots = Arc::new(Slots::new(listening.slots, PREAUTH_GRACE));
        let running = Arc::new(Stop::new());
        let thread = std::thread::spawn({
            let (heard, running) = (Arc::clone(&heard), Arc::clone(&running));
            let slots = Arc::clone(&slots);
            move || {
                let on_event = move |event| match event {
                    FleetEvent::Refused { peer, reason } => {
                        heard.refused.lock().unwrap().push(reason.clone());
                        if reason == AT_CAPACITY {
                            heard.at_capacity.fetch_add(1, Ordering::SeqCst);
                        }
                        if reason == DISPLACED {
                            heard.displaced.fetch_add(1, Ordering::SeqCst);
                        }
                        if peer.ip().to_canonical().is_ipv6() {
                            heard.refused_v6.lock().unwrap().push(reason);
                        }
                    }
                    FleetEvent::Admitted { .. } => {
                        heard.admitted.fetch_add(1, Ordering::SeqCst);
                    }
                    FleetEvent::Closed { .. } => {
                        heard.closed.fetch_add(1, Ordering::SeqCst);
                    }
                    FleetEvent::Stopped(_) => {}
                };
                serve_with_handshake_on(
                    &listener,
                    &listening,
                    &running,
                    HANG,
                    (echo, on_event),
                    (slots, live),
                );
            }
        });
        Self {
            heard,
            slots: Some(slots),
            running,
            thread: Some(thread),
        }
    }
}

impl Serving {
    /// The listener's slots ([`crate::slots::tests::until_seated`]).
    fn seats(&self) -> &Slots {
        self.slots
            .as_deref()
            .expect("a listener Serving::start_on started")
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.running.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn echo(served: Served, _op: FleetOp) -> io::Result<()> {
    served.answer(echo_all)
}

fn echo_all(stream: &mut tls::Answering<'_, ServerConnection>) -> io::Result<()> {
    let mut got = [0u8; 64];
    loop {
        let n = stream.read(&mut got)?;
        if n == 0 {
            return Ok(());
        }
        stream.write_all(&got[..n])?;
        stream.flush()?;
    }
}

fn echoes(conn: &mut TlsTransport<ClientConnection>) -> bool {
    let mut back = [0u8; 5];
    conn.stream().get_ref().set_read_timeout(Some(HANG)).is_ok()
        && conn.stream().write_all(b"ping\n").is_ok()
        && conn.stream().flush().is_ok()
        && conn.stream().read_exact(&mut back).is_ok()
        && &back == b"ping\n"
}

fn listening(slots: usize, lifetime: Duration) -> Listening {
    Listening {
        secret: secret(7),
        scope: FleetScope::Facts,
        node: None,
        serves: &[FleetOp::Survey],
        slots,
        per_source: slots,
        lifetime,
    }
}

/// A listener whose first connection's policy lookup (its offer, or its deny list) stalls,
/// while `others` more dialers arrive. With room (`slots > 1`) every other dialer finishes its
/// handshake and an echo while the stall lasts; with one slot the stalled worker keeps it.
/// A stalled deny lookup is reached only by a dialer that proved the fleet secret, so that
/// first peer is a real dialer; a stalled offer is reached by a bare TCP connect. A one-slot
/// stalled dialer is served once its lookup returns, or, when the next dialer came after its
/// slot was `PREAUTH_GRACE` old, displaced by it and not served: either way it kept the slot.
fn stalled_policy_keeps_listener_and_slot_limits(stall_deny: bool, slots: usize, others: usize) {
    use std::sync::mpsc;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered, entering) = mpsc::channel();
    let (finished, finishing) = mpsc::channel();
    let (displaced, displacing) = mpsc::channel();
    let lookup: Arc<dyn Fn() + Send + Sync> = Arc::new({
        let gate = Arc::clone(&gate);
        let calls = Arc::clone(&calls);
        move || {
            if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                entered.send(()).unwrap();
                // The test holds this lock through its other peers' admission attempts.
                // Unwinding the test releases it before joining the listener.
                drop(gate.lock().unwrap_or_else(PoisonError::into_inner));
                let _ = finished.send(());
            }
        }
    });
    let running = Arc::new(Stop::new());
    let heard = Arc::new(Heard::default());
    let thread = std::thread::spawn({
        let running = Arc::clone(&running);
        let heard = Arc::clone(&heard);
        move || {
            let for_offer = Arc::clone(&lookup);
            let for_deny = Arc::clone(&lookup);
            let offer = move || {
                if !stall_deny {
                    for_offer();
                }
                (FleetScope::Facts, &[FleetOp::Survey][..])
            };
            let deny = move || {
                if stall_deny {
                    for_deny();
                }
                Arc::new(Denied::default())
            };
            serve_offering_with_ask_within(
                &listener,
                (&listening(slots, HANG), Arc::new(offer), Arc::new(deny)),
                None,
                &running,
                HANG,
                echo,
                move |event| {
                    if let FleetEvent::Refused { reason, .. } = event {
                        if reason == AT_CAPACITY {
                            heard.at_capacity.fetch_add(1, Ordering::SeqCst);
                        }
                        if reason == DISPLACED {
                            let _ = displaced.send(());
                        }
                    }
                },
            );
        }
    });
    let serving = Serving {
        heard,
        slots: None,
        running,
        thread: Some(thread),
    };
    enum First {
        Bare(TcpStream),
        Dialer(std::thread::JoinHandle<Result<bool, FleetError>>),
    }
    let holding_policy = gate.lock().unwrap();
    let first_tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let first_peer = if stall_deny {
        First::Dialer(std::thread::spawn(move || {
            client_handshake(first_tcp, &secret(7), FleetOp::Survey, HANG)
                .map(|(mut conn, _)| echoes(&mut conn))
        }))
    } else {
        First::Bare(first_tcp)
    };
    entering
        .recv_timeout(HANG)
        .expect("the first peer reached its policy lookup");

    let dial = move || {
        let tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
        client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
            .map(|(mut admitted, _)| echoes(&mut admitted))
    };
    if slots == 1 {
        assert!(dial().is_err(), "the blocked worker still owns its slot");
        assert_eq!(serving.heard.at_capacity.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    } else {
        // Every other dialer at once: none waits on the stalled lookup or on another's.
        let dialers: Vec<_> = (0..others).map(|_| std::thread::spawn(dial)).collect();
        for dialer in dialers {
            match dialer.join().unwrap() {
                Ok(echoed) => assert!(echoed, "an admitted peer echoes"),
                Err(error) => {
                    panic!("a stalled policy lookup held another peer's handshake: {error}")
                }
            }
        }
    }
    drop(holding_policy);
    finishing
        .recv_timeout(HANG)
        .expect("the first policy lookup was released");
    match first_peer {
        First::Bare(tcp) => drop(tcp),
        First::Dialer(peer) => match peer.join().unwrap() {
            Ok(echoed) => assert!(
                echoed,
                "the stalled dialer is served once its lookup returns"
            ),
            // With one slot the stalled dialer holds it unauthenticated, its lookup being part
            // of its handshake. Once that slot is `PREAUTH_GRACE` old the next dialer may
            // displace it (`slots::pick_displaced`): its socket is cut and it is not served,
            // though it kept the slot until its worker returned, as the refusal above shows.
            Err(error) if slots == 1 => displacing.recv_timeout(HANG).unwrap_or_else(|_| {
                panic!("the stalled dialer failed without being displaced: {error}")
            }),
            Err(error) => panic!("the stalled dialer was not served: {error}"),
        },
    }
}

#[test]
fn a_stalled_offer_does_not_hold_the_listener() {
    stalled_policy_keeps_listener_and_slot_limits(false, 2, 1);
}

/// While one proved dialer's deny lookup stalls, 32 more dialers each finish a handshake and
/// an echo at once: the lookup holds only its own connection.
///
/// NEGATIVE CONTROL (2026-11-01): with the deny lookup moved back onto the accept loop (the
/// v0.147.0 shape, `let _on_accept_loop = deny();` before the worker spawns), the 32 dialers
/// are never accepted and this fails at its one-minute hang detector ("a stalled policy lookup
/// held another peer's handshake: no fleet handshake within 60s").
#[test]
fn a_stalled_deny_lookup_does_not_hold_the_listener() {
    const OTHERS: usize = 32;
    stalled_policy_keeps_listener_and_slot_limits(true, 1 + OTHERS, OTHERS);
}

/// One slot, whose dialer's deny lookup stalls: the next dialer is turned away at capacity.
/// How long the stalled dialer took to reach its lookup (its TLS handshake and proofs, on a
/// loaded Mac) decides only whether that dialer is later displaced or served.
///
/// CHECK (2026-11-01): with `std::thread::sleep(PREAUTH_GRACE)` before the one-slot dial, the
/// assertion before this change (`peer.join().unwrap().unwrap()`) panicked on the displaced
/// dialer's error ("the listener hung up without answering this machine's proof"), and this
/// one passes. NEGATIVE CONTROL (2026-11-01): with that sleep and the `DISPLACED` refusal no
/// longer passed on, this fails at its one-minute hang detector ("the stalled dialer failed
/// without being displaced: the listener hung up without answering this machine's proof").
#[test]
fn a_stalled_policy_keeps_its_connection_slot() {
    stalled_policy_keeps_listener_and_slot_limits(true, 1, 1);
}

/// A socket that proves nothing costs the listener no deny-list lookup: 64 connections closed
/// before a byte and one dialer holding the wrong secret are all refused without consulting
/// policy. A real dialer then exercises the live policy at admission and during response I/O.
///
/// NEGATIVE CONTROL (2026-11-01): with the lookup also made before the handshake again
/// (`let _eager = deny();` beside `offer()` in the worker, main before this change), every
/// accepted socket pays for one and the zero-lookup assertion fails before the control dial.
#[test]
fn a_socket_that_proves_nothing_costs_no_deny_lookup() {
    const DEAD: usize = 64;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refused = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    let running = Arc::new(Stop::new());
    let thread = std::thread::spawn({
        let (calls, refused, running) = (
            Arc::clone(&calls),
            Arc::clone(&refused),
            Arc::clone(&running),
        );
        move || {
            let deny = move || {
                calls.fetch_add(1, Ordering::AcqRel);
                Arc::new(Denied::default())
            };
            serve_offering_with_ask_within(
                &listener,
                (
                    &listening(DEAD + 2, HANG),
                    Arc::new(|| (FleetScope::Facts, &[FleetOp::Survey][..])),
                    Arc::new(deny),
                ),
                None,
                &running,
                HANG,
                echo,
                move |event| {
                    if matches!(event, FleetEvent::Refused { .. }) {
                        let (count, changed) = &*refused;
                        *count.lock().unwrap() += 1;
                        changed.notify_all();
                    }
                },
            );
        }
    });
    let serving = Serving {
        heard: Arc::new(Heard::default()),
        slots: None,
        running,
        thread: Some(thread),
    };
    for _ in 0..DEAD {
        drop(TcpStream::connect_timeout(&addr, HANG).unwrap());
    }
    let tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
    assert!(matches!(
        client_handshake(tcp, &secret(8), FleetOp::Survey, HANG),
        Err(FleetError::ListenerWrongSecret)
    ));
    let (count, changed) = &*refused;
    let (all, _) = changed
        .wait_timeout_while(count.lock().unwrap(), HANG, |n| *n < DEAD + 1)
        .unwrap();
    assert_eq!(
        *all,
        DEAD + 1,
        "every socket that proved nothing was refused"
    );
    drop(all);
    assert_eq!(calls.load(Ordering::Acquire), 0);

    let tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let (mut served, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    assert!(echoes(&mut served));
    assert!(calls.load(Ordering::Acquire) > 0);
    drop(served);
    drop(serving);
}

#[test]
fn idle_sockets_from_one_address_never_turn_away_a_member_from_another() {
    use crate::slots::tests::{IdleSockets, dual_stack_listener, until_seated};
    use std::net::{Ipv4Addr, Ipv6Addr};
    let test = "idle_sockets_from_one_address_never_turn_away_a_member_from_another";
    let (listener, port) = dual_stack_listener(test);
    let slots = 16;
    let served = Serving::start(listener, listening(slots, HANG));
    let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let member = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

    let idle = IdleSockets::hold(attacker, slots);
    until_seated(served.seats(), attacker);

    let tcp = TcpStream::connect_timeout(&member, HANG).unwrap();
    let (mut admitted, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        .unwrap_or_else(|e| {
            panic!(
                "idle sockets from 127.0.0.1 turned the member from ::1 away: {e} (the \
                 listener said {:?}; {} turned away at capacity, {} displaced)",
                served.heard.refused_v6.lock().unwrap(),
                served.heard.at_capacity.load(Ordering::SeqCst),
                served.heard.displaced.load(Ordering::SeqCst),
            )
        });
    assert!(echoes(&mut admitted), "the admitted member is served");
    // The displaced worker frees its slot before it says so.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the member got in by taking an idle socket's slot, and the listener said so",
        || (served.heard.displaced.load(Ordering::SeqCst) >= 1).then_some(()),
    );
    drop(idle);
}

#[test]
fn idle_sockets_never_lock_a_member_of_their_own_address_out() {
    use crate::slots::tests::{IdleSockets, until_seated};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
    let dial = || {
        let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
        client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
    };

    let (mut control, _) = dial().expect("an unloaded listener admits");
    assert!(echoes(&mut control), "the control is served");
    drop(control);

    let idle = IdleSockets::hold(addr, MAX_SLOTS);
    until_seated(served.seats(), addr);
    let started = Instant::now();
    let (mut admitted, _) = loop {
        match dial() {
            Ok(admitted) => break admitted,
            Err(e) => {
                assert!(
                    started.elapsed() < HANG,
                    "{MAX_SLOTS} idle sockets locked the member out for {HANG:?} (last \
                     refusal: {e}; {} turned away at capacity, {} displaced)",
                    served.heard.at_capacity.load(Ordering::SeqCst),
                    served.heard.displaced.load(Ordering::SeqCst),
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    assert!(echoes(&mut admitted), "the admitted member is served");
    // The displaced worker frees its slot before it says so.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the member got in by taking an idle socket's slot, and the listener said so",
        || (served.heard.displaced.load(Ordering::SeqCst) >= 1).then_some(()),
    );
    drop(idle);
}

#[test]
fn a_slow_handshake_is_never_cut_for_a_heavier_address() {
    use crate::slots::tests::{IdleSockets, dual_stack_listener, until_seated};
    use std::net::{Ipv4Addr, Ipv6Addr};
    let test = "a_slow_handshake_is_never_cut_for_a_heavier_address";
    let (listener, port) = dual_stack_listener(test);
    let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
    let attacker = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let member = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

    let idle = IdleSockets::hold(attacker, MAX_SLOTS);
    until_seated(served.seats(), attacker);

    let late = PREAUTH_GRACE + Duration::from_millis(500);
    let tcp = TcpStream::connect_timeout(&member, HANG).unwrap();
    std::thread::sleep(late);
    let (mut admitted, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
        .unwrap_or_else(|e| {
            panic!(
                "a handshake from ::1 sent {late:?} after its connect was cut while idle \
                 sockets from 127.0.0.1 held the other slots: {e} (the listener said {:?})",
                served.heard.refused_v6.lock().unwrap(),
            )
        });
    assert!(echoes(&mut admitted), "the admitted member is served");
    assert!(
        served.heard.refused_v6.lock().unwrap().is_empty(),
        "nothing from ::1 was turned away"
    );
    drop(idle);
}

#[test]
fn proved_members_still_fill_the_listener_and_are_never_displaced() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Serving::start(listener, listening(MAX_SLOTS, HANG));
    let dial = || {
        let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
        client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
    };

    let mut members: Vec<_> = (0..MAX_SLOTS)
        .map(|i| {
            dial()
                .unwrap_or_else(|e| {
                    panic!(
                        "member {i}: {e}; listener refused: {:?}",
                        served.heard.refused.lock().unwrap()
                    )
                })
                .0
        })
        .collect();
    std::thread::sleep(PREAUTH_GRACE + Duration::from_millis(500));

    assert!(
        dial().is_err(),
        "a newcomer is turned away while every slot holds a proved member"
    );
    assert!(served.heard.at_capacity.load(Ordering::SeqCst) >= 1);
    assert_eq!(served.heard.displaced.load(Ordering::SeqCst), 0);
    for (i, member) in members.iter_mut().enumerate() {
        assert!(echoes(member), "member {i} is still served");
    }
}

#[test]
fn one_address_holds_at_most_its_share_of_proved_connections() {
    use crate::slots::tests::dual_stack_listener;
    use std::net::{Ipv4Addr, Ipv6Addr};
    let test = "one_address_holds_at_most_its_share_of_proved_connections";
    let (listener, port) = dual_stack_listener(test);
    let slots = 8;
    let served = Serving::start(
        listener,
        Listening {
            per_source: per_source(slots),
            ..listening(slots, HANG)
        },
    );
    assert_eq!(per_source(slots), 2);
    let dial = |to: SocketAddr| {
        let tcp = TcpStream::connect_timeout(&to, HANG).map_err(FleetError::Io)?;
        client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
    };
    let busy = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let other = SocketAddr::from((Ipv6Addr::LOCALHOST, port));

    let mut held: Vec<_> = (0..2)
        .map(|i| {
            dial(busy)
                .unwrap_or_else(|e| panic!("connection {i}: {e}"))
                .0
        })
        .collect();
    let third = dial(busy).err();
    assert!(
        matches!(third, Some(FleetError::AddressBusy)),
        "the address past its share is refused by name: {third:?}"
    );
    let (mut member, _) = dial(other).expect("another address still gets a slot");
    assert!(echoes(&mut member), "the other member is served");
    for (i, conn) in held.iter_mut().enumerate() {
        assert!(echoes(conn), "connection {i} is still served");
    }
    drop(held.pop());
    let started = Instant::now();
    while let Err(e) = dial(busy) {
        assert!(
            started.elapsed() < HANG,
            "the address was never admitted again: {e}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(served.heard.displaced.load(Ordering::SeqCst), 0);
}

#[test]
fn a_drive_has_its_own_share_beside_the_reads() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let slots = 8;
    let _served = Serving::start(
        listener,
        Listening {
            serves: &[FleetOp::Survey, FleetOp::Drive],
            per_source: per_source(slots),
            ..listening(slots, HANG)
        },
    );
    assert_eq!((per_source(slots), drive_per_source(slots)), (2, 4));
    let dial = |op| {
        let tcp = TcpStream::connect_timeout(&addr, HANG).map_err(FleetError::Io)?;
        client_handshake(tcp, &secret(7), op, HANG).map(|(conn, _)| conn)
    };
    let mut reads: Vec<_> = (0..2)
        .map(|i| dial(FleetOp::Survey).unwrap_or_else(|e| panic!("read {i}: {e}")))
        .collect();
    assert!(matches!(
        dial(FleetOp::Survey).err(),
        Some(FleetError::AddressBusy)
    ));
    let mut drives: Vec<_> = (0..4)
        .map(|i| dial(FleetOp::Drive).unwrap_or_else(|e| panic!("drive {i}: {e}")))
        .collect();
    let past = dial(FleetOp::Drive).err();
    assert!(
        matches!(past, Some(FleetError::AddressBusy)),
        "a drive past its own share is refused by name: {past:?}"
    );
    for conn in reads.iter_mut().chain(drives.iter_mut()) {
        assert!(echoes(conn), "every one held is still served");
    }
}

#[test]
fn a_proved_connection_is_cut_at_its_lifetime() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let clock = Still::new();
    let served = Serving::start_on(listener, listening(1, SERVED_WITHIN), clock.live());
    let dial = || {
        let tcp = TcpStream::connect(addr).map_err(FleetError::Io)?;
        client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
    };

    let (mut silent, _) = dial().unwrap();
    // Admitted is told once the listener's handshake is over: a lifetime that passed before
    // it would cut the handshake, not the connection it serves.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the listener admits the dial",
        || (served.heard.admitted.load(Ordering::SeqCst) == 1).then_some(()),
    );
    clock.pass(SERVED_WITHIN);
    silent
        .stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    let mut rest = Vec::new();
    let ended = silent.stream().read_to_end(&mut rest);
    assert!(
        rest.is_empty()
            && !ended.is_err_and(|e| matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
        "the silent connection is cut: {rest:?}"
    );
    // Closed is told once the cut connection's answer let its slot go.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the cut connection gives its slot back",
        || (served.heard.closed.load(Ordering::SeqCst) == 1).then_some(()),
    );
    // Proved where the clock now stands still, the next dial is never overdue.
    dial().expect("the next dial is served once the cut gave its slot back");
}

/// An idle listener on the native clock sleeps until a connection comes or its stop is set: it
/// looks once, then not again while a second passes (nothing it polls is ready, and a `poll(2)`
/// a signal interrupts is asked again), and a stop set from another thread ends it at once (a
/// stop it never heard leaves it serving past the hang detector's minute).
#[cfg(unix)]
#[test]
fn an_idle_listener_waits_only_for_a_connection_or_its_stop() {
    let live = Arc::new(LiveConnections::default());
    let looks = || live.looks.load(Ordering::Relaxed);
    let mut served = Serving::start_on(
        TcpListener::bind("127.0.0.1:0").unwrap(),
        listening(1, HANG),
        Arc::clone(&live),
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the listener looks", || {
        (looks() >= 1).then_some(())
    });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        looks(),
        1,
        "an idle listener looked again with nothing come"
    );
    served.running.set();
    let serving = served.thread.take().expect("the listener's thread");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the listener ends at its stop",
        || serving.is_finished().then_some(()),
    );
    serving.join().unwrap();
}

/// Native time cannot spend a manually driven clock's lifetime or make its deadline spin.
/// Advancing that clock explicitly wakes the listener and cuts the proved connection.
#[cfg(unix)]
#[test]
fn a_test_clocks_deadline_never_paces_the_accept_loop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let clock = Still::new();
    let live = clock.live();
    let looks = || live.looks.load(Ordering::Relaxed);
    let served = Serving::start_on(
        listener,
        listening(1, Duration::from_millis(50)),
        Arc::clone(&live),
    );
    let tcp = TcpStream::connect(addr).unwrap();
    let (mut silent, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the listener admits the dial",
        || (served.heard.admitted.load(Ordering::SeqCst) == 1).then_some(()),
    );
    let from = looks();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(1));
    let looked = looks() - from;
    let elapsed = started.elapsed();
    let most = elapsed.as_millis() / ACCEPT_POLL.as_millis() + 2;
    assert!(
        u128::try_from(looked).unwrap() <= most,
        "the accept loop looked {looked} times in {elapsed:?}, more than the bounded {ACCEPT_POLL:?} control"
    );
    clock.pass(Duration::from_millis(50));
    silent
        .stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    let mut rest = Vec::new();
    let ended = silent.stream().read_to_end(&mut rest);
    assert!(
        rest.is_empty()
            && !ended.as_ref().is_err_and(|e| matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
        "the explicit clock wake cuts the silent peer: {ended:?}"
    );
}

/// A proof stamped asynchronously wakes the listener to install its precise native
/// lifetime deadline. A silent peer is cut without another TCP arrival.
#[test]
fn a_silent_proved_connection_is_cut_on_the_native_clock() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let _served = Serving::start(listener, listening(1, Duration::from_secs(2)));
    let tcp = TcpStream::connect(addr).unwrap();
    let (mut silent, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG).unwrap();
    silent
        .stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    let mut rest = Vec::new();
    let ended = silent.stream().read_to_end(&mut rest);
    assert!(
        rest.is_empty()
            && !ended.as_ref().is_err_and(|e| matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )),
        "the silent connection is cut at its lifetime: {rest:?} {ended:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_concurrent_socket_descriptor_scan_during_handshake_never_drains_the_proved_stream() {
    use std::os::fd::AsRawFd as _;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKET_FDINFO_BYTES: libc::c_int = 792;
    unsafe extern "C" {
        fn proc_pidfdinfo(
            pid: libc::c_int,
            fd: libc::c_int,
            flavor: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_fd = listener.as_raw_fd();
    let addr = listener.local_addr().unwrap();
    let _served = Serving::start(listener, listening(8, HANG));

    let stop = Arc::new(AtomicBool::new(false));
    let pid = std::process::id().cast_signed();
    let scanners: Vec<_> = (0..2)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while !stop.load(Ordering::Relaxed) {
                    for fd in base_fd..base_fd.saturating_add(16) {
                        // SAFETY: `buf` holds at least `SOCKET_FDINFO_BYTES` writable bytes.
                        unsafe {
                            let _ = proc_pidfdinfo(
                                pid,
                                fd,
                                PROC_PIDFDSOCKETINFO,
                                buf.as_mut_ptr().cast(),
                                SOCKET_FDINFO_BYTES,
                            );
                        }
                    }
                }
            })
        })
        .collect();

    for round in 0..40 {
        let tcp = TcpStream::connect_timeout(&addr, HANG).unwrap();
        let (mut conn, _) = client_handshake(tcp, &secret(7), FleetOp::Survey, HANG)
            .unwrap_or_else(|e| panic!("round {round} handshake: {e}"));
        assert!(
            echoes(&mut conn),
            "round {round}: the proved stream must not be drained by a concurrent socket scan"
        );
    }

    // NEGATIVE CONTROL: duplicating a live socket (`try_clone`) and closing the duplicate
    // while `PROC_PIDFDSOCKETINFO` inspects it drains the shared socket (`SS_DRAINING`), so a
    // blocking read on the still-open original descriptor fails at once with `EBADF`.
    let mut control_drained = false;
    let started = std::time::Instant::now();
    // One pair serves every look that leaves it whole: a fresh pair per look parks a port in
    // TIME_WAIT each time, and on a loaded machine, where the race shows later, the looks ran
    // out of ephemeral ports (`EADDRNOTAVAIL`) before the drain showed.
    let mut pair = None;
    while !control_drained && started.elapsed() < HANG {
        let (reader, _writer) = pair.get_or_insert_with(|| {
            let pair_listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let pair_addr = pair_listener.local_addr().unwrap();
            let reader = TcpStream::connect_timeout(&pair_addr, HANG).unwrap();
            let (writer, _) = aterm_uds::accept(
                &pair_listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            reader
                .set_read_timeout(Some(Duration::from_millis(2)))
                .unwrap();
            (reader, writer)
        });
        for _ in 0..8 {
            let dup = reader.try_clone().unwrap();
            drop(dup);
        }
        let mut byte = [0u8; 1];
        match (&*reader).read(&mut byte) {
            Err(e) if e.raw_os_error() == Some(libc::EBADF) => control_drained = true,
            // Still whole: the next look reuses it.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            // Anything else spoils it: the next look opens another.
            _ => pair = None,
        }
    }

    stop.store(true, Ordering::Relaxed);
    for scanner in scanners {
        scanner.join().unwrap();
    }
    assert!(
        control_drained,
        "CONTROL: closing a try_clone duplicate under PROC_PIDFDSOCKETINFO drains the socket with EBADF"
    );
}

#[cfg(unix)]
#[test]
fn a_panicking_policy_keeps_the_accepted_fd_until_its_watchdog_joins() {
    use std::os::fd::AsRawFd as _;

    fn peer(fd: std::os::fd::RawFd) -> Option<(u16, u32)> {
        let mut address = std::mem::MaybeUninit::<libc::sockaddr_in>::zeroed();
        let mut length =
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>()).unwrap();
        // SAFETY: getpeername accepts any descriptor (including a closed one), and the
        // writable address and length name initialized storage valid for this call.
        let result = unsafe { libc::getpeername(fd, address.as_mut_ptr().cast(), &mut length) };
        if result != 0 || usize::try_from(length).ok()? != std::mem::size_of::<libc::sockaddr_in>()
        {
            return None;
        }
        // SAFETY: getpeername succeeded and initialized the complete IPv4 address above.
        let address = unsafe { address.assume_init() };
        (i32::from(address.sin_family) == libc::AF_INET)
            .then_some((address.sin_port, address.sin_addr.s_addr))
    }

    for failure in 0..4 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut dialer = TcpStream::connect_timeout(&listener.local_addr().unwrap(), HANG).unwrap();
        dialer.set_read_timeout(Some(HANG)).unwrap();
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let fd = tcp.as_raw_fd();
        let expected = peer(fd).unwrap();
        let (joined, observation) = std::sync::mpsc::channel();
        let mut watchdog = arm(&tcp, HANG).unwrap();
        watchdog.observe_join(move || {
            // The live dialer reserves this exact peer tuple, so an unrelated descriptor
            // reusing the number cannot masquerade as the accepted connection.
            let _ = joined.send(peer(fd) == Some(expected));
        });
        let armed = ArmedHandshake::new(tcp, watchdog);
        let worker = move || {
            let policy = || {
                assert!(armed.tcp().peer_addr().is_ok());
                let offer = || assert_ne!(failure, 1, "injected offer panic");
                // The deny lookup runs inside the handshake, past the dialer's proofs
                // (`listener_proves`).
                let deny = || assert_ne!(failure, 2, "injected deny panic");
                offer();
                armed.finish(|_tcp, _watchdog| {
                    deny();
                    assert_ne!(failure, 3, "injected handshake panic");
                });
            };
            policy();
        };
        if failure == 0 {
            // A failed spawn drops the complete, uncalled worker capture.
            drop(worker);
        } else {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(worker)).is_err());
        }
        assert!(
            observation
                .recv_timeout(HANG)
                .expect("watchdog reached its join")
        );
        assert_eq!(dialer.read(&mut [0u8; 1]).unwrap(), 0);
    }
}

#[cfg(unix)]
#[test]
fn admission_refusal_keeps_accepted_fd_open_until_watchdog_joins() {
    use std::os::fd::AsRawFd as _;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let dialer = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let accepted_fd = accepted.as_raw_fd();
    let watchdog = arm(&accepted, HANG).unwrap();
    let evictor = watchdog.evictor();

    let refused = Admission::inspect(&accepted, Locality::Link(crate::lan::Scope::Lan), None);
    assert!(
        refused.is_err(),
        "loopback socket must be refused under Scope::Lan"
    );

    let mut bystander_client = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let (mut bystander_server, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    bystander_server.set_read_timeout(Some(HANG)).unwrap();
    assert_ne!(
        bystander_client.as_raw_fd(),
        accepted_fd,
        "accepted fd must remain open while watchdog is still armed"
    );
    assert_ne!(
        bystander_server.as_raw_fd(),
        accepted_fd,
        "accepted fd must remain open while watchdog is still armed"
    );

    assert!(evictor.evict());
    assert_eq!(watchdog.finish(), Claim::Evicted);
    drop((accepted, dialer));

    bystander_client.write_all(b"ok").unwrap();
    let mut buf = [0u8; 2];
    bystander_server.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ok");

    // NEGATIVE CONTROL: if `accepted` is closed before `watchdog.finish()` and its descriptor
    // number (`victim_fd`) is reused by a bystander socket, `evictor.evict()` shuts down the
    // bystander socket. `dup2` replaces `victim_fd` atomically while `accepted` still owns it
    // so parallel test threads cannot claim `victim_fd` in between.
    let dialer = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let victim_fd = accepted.as_raw_fd();
    let watchdog = arm(&accepted, HANG).unwrap();
    let evictor = watchdog.evictor();
    let mut bystander_client = TcpStream::connect_timeout(&addr, HANG).unwrap();
    let (bystander_server_orig, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    bystander_server_orig.set_read_timeout(Some(HANG)).unwrap();
    // SAFETY: both `bystander_server_orig` and `accepted` are open descriptors owned by this
    // test; `dup2` atomically closes `accepted`'s descriptor (`victim_fd`) and duplicates
    // `bystander_server_orig` onto `victim_fd` with no unallocated gap for sibling threads.
    let duped = unsafe { libc::dup2(bystander_server_orig.as_raw_fd(), victim_fd) };
    assert_eq!(duped, victim_fd, "dup2 onto victim_fd must succeed");
    std::mem::forget(accepted);
    drop(bystander_server_orig);
    // SAFETY: `victim_fd` now uniquely owns the duplicated `bystander_server` socket.
    let mut bystander_server: TcpStream = unsafe { std::os::fd::FromRawFd::from_raw_fd(victim_fd) };
    let _ = aterm_uds::keep_out_of_children(&bystander_server);
    assert!(evictor.evict());
    assert_eq!(watchdog.finish(), Claim::Evicted);
    drop(dialer);
    let cut = bystander_client.write_all(b"ok").is_err()
        || bystander_server.read_exact(&mut buf).is_err();
    assert!(
        cut,
        "CONTROL: closing accepted before watchdog.finish() shuts down the reused bystander fd"
    );
}

/// A listener serving ask on its own: every op it offers, an ask handler that records the one
/// line it reads and answers `OK asked`, and a hand-over that records each op it is given.
struct Asked {
    addr: SocketAddr,
    ops: Arc<Mutex<Vec<FleetOp>>>,
    lines: Arc<Mutex<Vec<String>>>,
    running: Arc<Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Asked {
    fn start(serves_ask: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ops = Arc::new(Mutex::new(Vec::new()));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(Stop::new());
        let ask: AskHandler = Arc::new({
            let lines = Arc::clone(&lines);
            move |mut stream: &mut dyn Duplex, _peer| {
                let line = read_line(&mut stream)
                    .map_err(|error| io::Error::other(error.to_string()))?
                    .unwrap_or_default();
                lines.lock().unwrap().push(line);
                stream.write_all(b"OK asked\n")
            }
        });
        let thread = std::thread::spawn({
            let (ops, running) = (Arc::clone(&ops), Arc::clone(&running));
            move || {
                let listening = Listening {
                    secret: secret(7),
                    scope: FleetScope::Content,
                    node: None,
                    serves: &[
                        FleetOp::Survey,
                        FleetOp::Book,
                        FleetOp::Join,
                        FleetOp::Drive,
                        FleetOp::Facts,
                        FleetOp::Census,
                    ],
                    slots: 8,
                    per_source: 8,
                    lifetime: HANG,
                };
                let empty = Arc::new(Denied::default());
                let offering = (listening.scope, listening.serves);
                serve_inner(
                    ListenerSocket::Fleet(&listener),
                    (
                        &listening,
                        Arc::new(move || offering),
                        Arc::new(move || Arc::clone(&empty)),
                    ),
                    serves_ask.then_some(ask),
                    &running,
                    Locality::Fleet,
                    HANG,
                    move |served: Served, op| {
                        ops.lock().unwrap().push(op);
                        served.answer(|stream| stream.write_all(b"OK served\n"))
                    },
                    |_| {},
                );
            }
        });
        Self {
            addr,
            ops,
            lines,
            running,
            thread: Some(thread),
        }
    }

    fn dial(&self) -> TcpStream {
        let tcp = TcpStream::connect_timeout(&self.addr, HANG).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        tcp
    }
}

impl Drop for Asked {
    fn drop(&mut self) {
        self.running.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// F10 (`docs/DESIGN-fluid-swarm-2026-10-28.md`): a caller without the fleet secret reaches the
/// ask handler and nothing else. Whatever it writes on [`ALPN_ASK`] (a survey, a drive, a join,
/// the fleet's own proof line) goes to the ask handler, and no op is handed over; on the fleet
/// protocol a wrong secret is refused before any op; and a listener with no ask handler does
/// not speak ask at all.
#[test]
fn a_caller_without_the_fleet_secret_reaches_only_ask() {
    let listening = Asked::start(true);
    let tries = [
        "survey".to_owned(),
        "version".to_owned(),
        format!("drive {}", "0".repeat(64)),
        format!("join {}", "0".repeat(64)),
        format!("survey {}", "f".repeat(64)),
    ];
    for line in &tries {
        let mut asked = ask_handshake(listening.dial(), HANG).expect("an ask is served");
        asked
            .stream()
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
        asked.stream().flush().unwrap();
        let reply = read_line(asked.stream()).unwrap();
        assert_eq!(reply.as_deref(), Some("OK asked"), "{line}");
    }
    assert_eq!(*listening.lines.lock().unwrap(), tries);
    assert!(
        listening.ops.lock().unwrap().is_empty(),
        "an ask reached a fleet op: {:?}",
        listening.ops.lock().unwrap()
    );
    // CONTROL: the fleet secret reaches the hand-over, so the empty record above means something.
    let (mut proved, _) = client_handshake(listening.dial(), &secret(7), FleetOp::Survey, HANG)
        .expect("the secret proves survey");
    assert_eq!(
        read_line(proved.stream()).unwrap().as_deref(),
        Some("OK served")
    );
    assert_eq!(*listening.ops.lock().unwrap(), [FleetOp::Survey]);
    // Answered ask callers that keep their sockets open without sending close_notify or FIN
    // never hold the listener's 8 slots against a member proving the fleet secret.
    let mut held_asks = Vec::with_capacity(8);
    for _ in 0..8 {
        let mut asked = ask_handshake(listening.dial(), HANG).expect("an ask is served");
        asked.stream().write_all(b"join\n").unwrap();
        asked.stream().flush().unwrap();
        assert_eq!(
            read_line(asked.stream()).unwrap().as_deref(),
            Some("OK asked")
        );
        held_asks.push(asked);
    }
    let (mut after_asks, _) = client_handshake(listening.dial(), &secret(7), FleetOp::Survey, HANG)
        .expect("answered asks release their listener slots without waiting for peer EOF");
    assert_eq!(
        read_line(after_asks.stream()).unwrap().as_deref(),
        Some("OK served")
    );
    drop(held_asks);
    // A listener given no ask handler never negotiates ask.
    let closed = Asked::start(false);
    let refused = ask_handshake(closed.dial(), HANG);
    assert!(
        matches!(refused, Err(FleetError::Alpn)),
        "{:?}",
        refused.err()
    );
    assert!(closed.lines.lock().unwrap().is_empty() && closed.ops.lock().unwrap().is_empty());
}

/// A survey or a drive from a caller that does not hold the fleet secret is refused in the
/// handshake on a listener that also serves ask: the ask path is no way around the proof.
#[test]
fn a_survey_or_drive_without_the_fleet_secret_is_refused() {
    let listening = Asked::start(true);
    for op in [FleetOp::Survey, FleetOp::Drive] {
        let wrong = client_handshake(listening.dial(), &secret(9), op, HANG);
        assert!(
            matches!(wrong, Err(FleetError::ListenerWrongSecret)),
            "{op:?}: {:?}",
            wrong.err()
        );
    }
    // A dialer with no secret that ignores the listener's proof and presents its own guess.
    for word in ["survey", "drive"] {
        let (mut raw, _) = connect_tls(listening.dial(), client_config()).expect("tls");
        read_line(raw.stream())
            .unwrap()
            .expect("the listener's proof");
        raw.stream()
            .write_all(format!("{word} {}\n", "0".repeat(64)).as_bytes())
            .unwrap();
        raw.stream().flush().unwrap();
        let reply = read_line(raw.stream()).ok().flatten().unwrap_or_default();
        assert_eq!(reply, "NO proof", "{word}");
    }
    assert!(
        listening.ops.lock().unwrap().is_empty(),
        "an op ran without the secret: {:?}",
        listening.ops.lock().unwrap()
    );
    // CONTROL: the fleet secret reaches each op on the same listener.
    for op in [FleetOp::Survey, FleetOp::Drive] {
        let (mut proved, _) = client_handshake(listening.dial(), &secret(7), op, HANG)
            .expect("the secret proves the op");
        assert_eq!(
            read_line(proved.stream()).unwrap().as_deref(),
            Some("OK served")
        );
    }
    assert_eq!(
        *listening.ops.lock().unwrap(),
        [FleetOp::Survey, FleetOp::Drive]
    );
}

/// The identity point `01 00..00` as a key: a small-order key.
const SMALL_ORDER_KEY: [u8; NodeKey::LEN] = {
    let mut key = [0; NodeKey::LEN];
    key[0] = 1;
    key
};

/// The identity as commitment and `s = 0`: under ring alone, [`SMALL_ORDER_KEY`]'s signature
/// of every message (astream-aead's `a_small_order_key_signs_nothing`).
const FORGED: [u8; SIG_LEN] = {
    let mut sig = [0; SIG_LEN];
    sig[0] = 1;
    sig
};

/// A node line whose key is small-order proves nothing, in each end's role: its signature
/// would verify every exporter under ring alone, so it binds the key to no channel.
///
/// NEGATIVE CONTROL: `NodeKey::verifies` through ring's `UnparsedPublicKey`: every role
/// takes the line as a proof.
#[test]
fn a_small_order_key_proves_no_node_line() {
    let key = NodeKey::from_bytes(SMALL_ORDER_KEY);
    let line = format!(
        "node {} {} {}\n",
        "10".repeat(16),
        key.hex(),
        hex::encode_lower(&FORGED)
    );
    for end in [
        Signer::Dialer,
        Signer::Listener,
        Signer::MeshDialer,
        Signer::MeshListener,
    ] {
        let proved = read_node_proof(&mut line.as_bytes(), end, &[0xa5; EXPORTER_LEN]);
        assert!(
            matches!(proved, Err(FleetError::NodeProof)),
            "{end:?}: {proved:?}"
        );
    }
}

/// A small-order key signs no record: not a book entry's name, not a membership node proof.
///
/// NEGATIVE CONTROL: `NodeKey::verifies` through ring's `UnparsedPublicKey`: both verify.
#[test]
fn a_small_order_key_signs_no_record() {
    let key = NodeKey::from_bytes(SMALL_ORDER_KEY);
    for domain in ["book name", "membership node"] {
        assert!(
            !verifies_record(&key, domain, b"a record body", &FORGED),
            "{domain}"
        );
    }
}
