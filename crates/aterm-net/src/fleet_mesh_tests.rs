// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `atm/1` beside `atf/2` (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, §11 B3): the
//! role-bound, listener-first handshake, standing judged by each end's membership log, the desk as
//! the one door for an unknown key, and a dialer that speaks the mesh only when its node dials it.

use super::*;
use crate::fleet_member::{MemberKeys, Standing};
use aterm_uds::CtlStream;
use std::net::TcpListener;
use std::sync::mpsc;

/// The hang detector of every wait here: a loopback handshake that must happen.
const HANG: Duration = Duration::from_secs(60);

/// One channel's exporter, for the halves driven over a socket pair.
const E: &[u8] = &[0xa5; EXPORTER_LEN];

fn secret() -> FleetSecret {
    FleetSecret::new(&[7; FleetSecret::MIN_LEN]).unwrap()
}

fn node(byte: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{byte:02x}").repeat(16), &[byte; 32]).unwrap()
}

fn proof_of(me: &NodeIdentity) -> NodeProof {
    NodeProof {
        node: me.node().to_owned(),
        key: me.key(),
    }
}

/// A verdict whose log seats `members` and took `out` out.
fn verdict(members: &[&NodeIdentity], out: &[&NodeIdentity]) -> Denied {
    Denied {
        members: members.iter().map(|me| me.key()).collect(),
        keys: out.iter().map(|me| me.key()).collect(),
        ..Denied::default()
    }
}

fn fixed(denied: Denied) -> DenyFn {
    let denied = Arc::new(denied);
    Arc::new(move || Arc::clone(&denied))
}

/// `me` dialing the mesh, judging listeners by `denied`.
fn meshing(me: &NodeIdentity, denied: Denied) -> NodeIdentity {
    me.clone().dialing_mesh(Some(fixed(denied)))
}

/// A node line `named` claims, signed by `signer` in the role `end` over `exporter`.
fn forged(named: &NodeIdentity, signer: &NodeIdentity, end: Signer, exporter: &[u8]) -> String {
    format!(
        "node {} {} {}\n",
        named.node(),
        named.key().hex(),
        hex::encode_lower(&signer.sign(end, exporter))
    )
}

/// What one connection's served side saw: the op, the proof its dialer gave, and whether it spoke
/// the mesh.
type Seen = (FleetOp, Option<NodeProof>, bool);

/// A listener with `node`, judging dialers by `deny`, serving `serves`, and `ask` where given, on
/// loopback: each proved connection is told to `seen` and echoed.
struct Running {
    addr: SocketAddr,
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
    seen: mpsc::Receiver<Seen>,
}

impl Running {
    fn start(
        node: Option<NodeIdentity>,
        deny: DenyFn,
        serves: &'static [FleetOp],
        ask: Option<AskHandler>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let (tell, seen) = mpsc::channel::<Seen>();
        let tell = Arc::new(Mutex::new(tell));
        let listening = Listening {
            secret: secret(),
            scope: FleetScope::Content,
            node,
            serves,
            slots: 8,
            per_source: 8,
            lifetime: HANG,
        };
        let serving = {
            let running = Arc::clone(&running);
            std::thread::spawn(move || {
                serve_offering_with_ask_within(
                    &listener,
                    (
                        &listening,
                        Arc::new(move || (FleetScope::Content, serves)),
                        deny,
                    ),
                    ask,
                    &running,
                    HANG,
                    move |served, op| {
                        let _ = tell.lock().unwrap().send((
                            op,
                            served.peer_node().cloned(),
                            served.meshed(),
                        ));
                        served.answer(|stream| {
                            let mut got = [0u8; 64];
                            loop {
                                let n = stream.read(&mut got)?;
                                if n == 0 {
                                    return Ok(());
                                }
                                stream.write_all(&got[..n])?;
                                stream.flush()?;
                            }
                        })
                    },
                    |_| {},
                );
            })
        };
        Self {
            addr,
            running,
            serving: Some(serving),
            seen,
        }
    }

    fn dial(&self, me: Option<&NodeIdentity>, op: FleetOp) -> Result<Dialed, Undialed> {
        let tcp = TcpStream::connect(self.addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        client_handshake_by(tcp, &secret(), me, op, HANG)
    }

    fn seen(&self) -> Seen {
        self.seen
            .recv_timeout(HANG)
            .expect("the served side within the hang detector")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

fn alpn(dialed: &mut Dialed) -> Option<Vec<u8>> {
    dialed
        .transport
        .stream()
        .conn
        .alpn_protocol()
        .map(<[u8]>::to_vec)
}

fn echoes(dialed: &mut Dialed) -> bool {
    let stream = dialed.transport.stream();
    let mut back = [0u8; 5];
    stream.get_ref().set_read_timeout(Some(HANG)).is_ok()
        && stream.write_all(b"ping\n").is_ok()
        && stream.flush().is_ok()
        && stream.read_exact(&mut back).is_ok()
        && &back == b"ping\n"
}

#[test]
fn two_members_meet_on_atm1_each_proving_its_node_the_listeners_first() {
    let (listener, dialer) = (node(0x10), node(0x20));
    let running = Running::start(
        Some(listener.clone()),
        fixed(verdict(&[&listener, &dialer], &[])),
        &[FleetOp::Survey],
        None,
    );
    let me = meshing(&dialer, verdict(&[&listener, &dialer], &[]));
    let mut dialed = running.dial(Some(&me), FleetOp::Survey).unwrap();
    assert!(dialed.mesh, "both ends speak the mesh");
    assert_eq!(alpn(&mut dialed).as_deref(), Some(ALPN_MESH));
    assert_eq!(dialed.scope, FleetScope::Content);
    assert_eq!(dialed.peer, Some(proof_of(&listener)));
    let (op, peer, meshed) = running.seen();
    assert_eq!(op, FleetOp::Survey);
    assert_eq!(peer, Some(proof_of(&dialer)), "the listener learns the key");
    assert!(meshed);
    assert!(echoes(&mut dialed), "the stream is handed over");
}

#[test]
fn with_the_mesh_off_the_dialer_still_opens_atf2() {
    let (listener, dialer) = (node(0x10), node(0x20));
    // The listener offers `atm/1` first, and judges by a log that seats both.
    let running = Running::start(
        Some(listener.clone()),
        fixed(verdict(&[&listener, &dialer], &[])),
        &[FleetOp::Survey],
        None,
    );
    // The field the switch is read into, off: this node dials the fleet alone.
    let off = dialer.clone().dialing_mesh(None);
    assert!(!off.dials_mesh());
    let mut dialed = running.dial(Some(&off), FleetOp::Survey).unwrap();
    assert!(!dialed.mesh);
    assert_eq!(alpn(&mut dialed).as_deref(), Some(ALPN_NODES));
    assert_eq!(dialed.peer, Some(proof_of(&listener)));
    let (_, peer, meshed) = running.seen();
    assert_eq!(peer, Some(proof_of(&dialer)));
    assert!(!meshed, "the listener served `atf/2`, proved by the secret");
    assert!(echoes(&mut dialed));

    // The control: the same node with the field on speaks the mesh to the same listener.
    let on = meshing(&dialer, verdict(&[&listener, &dialer], &[]));
    let mut dialed = running.dial(Some(&on), FleetOp::Survey).unwrap();
    assert_eq!(alpn(&mut dialed).as_deref(), Some(ALPN_MESH));
    assert!(running.seen().2);
}

#[test]
fn a_mesh_dialer_meets_an_older_listener_on_atf2() {
    let (listener, dialer) = (node(0x10), node(0x20));
    let older = Arc::new(listener.clone());
    let me = meshing(&dialer, verdict(&[&listener, &dialer], &[]));
    let (listened, dialed) = {
        let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = tcp_listener.local_addr().unwrap();
        let listening = std::thread::spawn(move || {
            let tcp = tcp_listener.accept().unwrap().0;
            // A listener from before the mesh: it offers `atf/2` and `atf/1` alone.
            server_handshake_as(
                tcp,
                &secret(),
                Some(&older),
                FleetScope::Facts,
                &[FleetOp::Survey],
                HANG,
            )
            .map(|accepted| (accepted.mesh, accepted.peer))
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let dialed = client_handshake_by(tcp, &secret(), Some(&me), FleetOp::Survey, HANG);
        (listening.join().unwrap(), dialed)
    };
    let mut dialed = dialed.unwrap();
    assert!(!dialed.mesh);
    assert_eq!(alpn(&mut dialed).as_deref(), Some(ALPN_NODES));
    assert_eq!(listened.unwrap(), (false, Some(proof_of(&dialer))));
}

#[test]
fn an_op_the_mesh_does_not_carry_is_never_offered_on_it_nor_served_there() {
    let (listener, dialer) = (node(0x10), node(0x20));
    let both = verdict(&[&listener, &dialer], &[]);
    let running = Running::start(
        Some(listener.clone()),
        fixed(both.clone()),
        &[FleetOp::Survey, FleetOp::Join],
        None,
    );
    let me = meshing(&dialer, both.clone());
    let mut dialed = running.dial(Some(&me), FleetOp::Join).unwrap();
    assert!(
        !dialed.mesh,
        "a join is the fleet secret's: it never rides the mesh"
    );
    assert_eq!(alpn(&mut dialed).as_deref(), Some(ALPN_NODES));
    assert_eq!(running.seen().0, FleetOp::Join);

    // A dialer that names it on the mesh anyway is answered `NO op`.
    let (heard, answer) = listener_hears(&listener, &both, &[FleetOp::Join], |sent| {
        format!(
            "join\n{}",
            node_line(Some(&dialer), Signer::MeshDialer, sent)
        )
    });
    assert!(
        matches!(heard, Err(FleetError::UnknownOp(ref op)) if op == "join"),
        "{heard:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO op"));
}

/// What a listener decided: the op it serves and the dialer it proved, or why it refused.
type Decided = Result<(FleetOp, Option<NodeProof>), FleetError>;

/// The listener's half over a socket pair: what it decided, and its answer to the dialer's lines
/// `lines` gives (from this channel's exporter), once its own proof and level were read.
fn listener_hears(
    listener: &NodeIdentity,
    denied: &Denied,
    serves: &'static [FleetOp],
    lines: impl FnOnce(&[u8]) -> String,
) -> (Decided, Option<String>) {
    let (mut listening, mut dialing) = CtlStream::pair().unwrap();
    dialing.set_read_timeout(Some(HANG)).unwrap();
    let (me, denied) = (listener.clone(), denied.clone());
    let heard = std::thread::spawn(move || {
        listening.set_read_timeout(Some(HANG)).unwrap();
        listener_meets(
            &mut listening,
            E,
            &me,
            (FleetScope::Facts, serves),
            || Box::new(denied),
            |_| true,
        )
    });
    let proved = read_node_proof(&mut dialing, Signer::MeshListener, E).unwrap();
    assert_eq!(
        proved,
        Some(proof_of(listener)),
        "the listener proves first"
    );
    assert_eq!(read_line(&mut dialing).unwrap().as_deref(), Some("facts"));
    dialing.write_all(lines(E).as_bytes()).unwrap();
    let answer = read_line(&mut dialing).unwrap();
    (heard.join().unwrap(), answer)
}

#[test]
fn refusals_for_a_wrong_key_a_removed_key_and_an_unknown_key() {
    let (listener, member, removed, stranger) = (node(0x10), node(0x20), node(0x30), node(0x40));
    let denied = verdict(&[&listener, &member], &[&removed]);
    let say = |who: &NodeIdentity, signer: &NodeIdentity| {
        let (who, signer) = (who.clone(), signer.clone());
        move |exporter: &[u8]| {
            format!(
                "survey\n{}",
                forged(&who, &signer, Signer::MeshDialer, exporter)
            )
        }
    };
    let survey = &[FleetOp::Survey];

    let (heard, answer) = listener_hears(&listener, &denied, survey, say(&member, &member));
    assert_eq!(
        answer.as_deref(),
        Some("OK"),
        "the control: a member is admitted"
    );
    assert_eq!(heard.unwrap(), (FleetOp::Survey, Some(proof_of(&member))));

    // A wrong key: a member's key named, another key's signature.
    let (heard, answer) = listener_hears(&listener, &denied, survey, say(&member, &stranger));
    assert!(matches!(heard, Err(FleetError::NodeProof)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO node"));
    // A wrong key: a member's node id claimed under the claimant's own key, which no seat holds.
    let impostor = NodeIdentity::from_seed(member.node(), &[0x40; 32]).unwrap();
    let (heard, answer) = listener_hears(&listener, &denied, survey, say(&impostor, &impostor));
    assert!(matches!(heard, Err(FleetError::UnknownKey)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO unknown"));

    let (heard, answer) = listener_hears(&listener, &denied, survey, say(&removed, &removed));
    assert!(matches!(heard, Err(FleetError::DeniedNode)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO denied"));
    let (heard, answer) = listener_hears(&listener, &denied, survey, say(&stranger, &stranger));
    assert!(matches!(heard, Err(FleetError::UnknownKey)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO unknown"));

    // A roster deny by node id or by key stands as the log's removal does.
    for roster in [member.node().to_owned(), member.key().hex()] {
        let denied = Denied {
            nodes: vec![roster],
            ..denied.clone()
        };
        let (heard, answer) = listener_hears(&listener, &denied, survey, say(&member, &member));
        assert!(matches!(heard, Err(FleetError::DeniedNode)), "{heard:?}");
        assert_eq!(answer.as_deref(), Some("NO denied"));
    }
    // An unread verdict judges no one: dial again later.
    let unread = Denied::not_yet_read();
    let (heard, answer) = listener_hears(&listener, &unread, survey, say(&member, &member));
    assert!(
        matches!(heard, Err(FleetError::DenyListUnread)),
        "{heard:?}"
    );
    assert_eq!(answer.as_deref(), Some("NO capacity"));
}

#[test]
fn over_tls_the_removed_key_is_refused_and_the_unknown_key_goes_to_the_desk() {
    let (listener, member, removed, stranger) = (node(0x10), node(0x20), node(0x30), node(0x40));
    let denied = verdict(&[&listener, &member], &[&removed]);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let desk: AskHandler = {
        let asked = Arc::clone(&asked);
        Arc::new(move |stream: &mut dyn Duplex, _from| {
            asked.fetch_add(1, Ordering::SeqCst);
            stream.write_all(b"desk\n")?;
            stream.flush()
        })
    };
    let running = Running::start(
        Some(listener.clone()),
        fixed(denied.clone()),
        &[FleetOp::Survey],
        Some(desk),
    );
    let dialer_view = verdict(&[&listener], &[]);

    let mut dialed = running
        .dial(
            Some(&meshing(&member, dialer_view.clone())),
            FleetOp::Survey,
        )
        .unwrap();
    assert!(dialed.mesh && echoes(&mut dialed), "the control");
    let _ = running.seen();

    let refused = running
        .dial(
            Some(&meshing(&removed, dialer_view.clone())),
            FleetOp::Survey,
        )
        .err()
        .expect("a removed key is refused");
    assert!(
        matches!(refused.error, FleetError::DeniedNode),
        "{refused:?}"
    );
    assert_eq!(refused.refused_by, Some(proof_of(&listener)));
    assert!(!refused.error.falls_back(), "a removal is final");

    let refused = running
        .dial(Some(&meshing(&stranger, dialer_view)), FleetOp::Survey)
        .err()
        .expect("an unknown key is refused");
    assert!(
        matches!(refused.error, FleetError::UnknownKey),
        "{refused:?}"
    );
    assert!(refused.error.falls_back());
    // Its one door is the desk, over `atf-ask/1`, which proves nothing.
    let tcp = TcpStream::connect(running.addr).unwrap();
    let mut asking = ask_handshake(tcp, HANG).unwrap();
    asking
        .stream()
        .get_ref()
        .set_read_timeout(Some(HANG))
        .unwrap();
    assert_eq!(read_line(asking.stream()).unwrap().as_deref(), Some("desk"));
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

#[test]
fn a_removed_node_is_refused_at_once_on_receipt_of_its_remove() {
    let (listener, member) = (node(0x10), node(0x20));
    let log: Arc<Mutex<MemberKeys>> = Arc::new(Mutex::new(
        [
            (listener.key(), Standing::Member),
            (member.key(), Standing::Member),
        ]
        .into_iter()
        .collect(),
    ));
    // The listener reads its log for each connection, as the node service's and the window's do
    // (`Denied::under` over the log's keys).
    let deny: DenyFn = {
        let log = Arc::clone(&log);
        Arc::new(move || Arc::new(Denied::under(Some(&[]), Some(&log.lock().unwrap()))))
    };
    let running = Running::start(Some(listener.clone()), deny, &[FleetOp::Survey], None);
    let me = meshing(&member, verdict(&[&listener], &[]));
    let mut dialed = running.dial(Some(&me), FleetOp::Survey).unwrap();
    assert!(dialed.mesh && echoes(&mut dialed));
    let _ = running.seen();

    // Its `Remove` arrives: the log now holds the key out.
    *log.lock().unwrap() = [
        (listener.key(), Standing::Member),
        (
            member.key(),
            Standing::Out("the membership log removed the Mac that holds it".to_owned()),
        ),
    ]
    .into_iter()
    .collect();
    let refused = running
        .dial(Some(&me), FleetOp::Survey)
        .err()
        .expect("refused from the next handshake on");
    assert!(
        matches!(refused.error, FleetError::DeniedNode),
        "{refused:?}"
    );
    assert!(!refused.error.falls_back());
}

/// The dialer's half over a socket pair, the listener's end writing `lines` (from this channel's
/// exporter): what it decided, and every byte it sent before its end closed.
fn dialer_hears(
    me: &NodeIdentity,
    lines: impl FnOnce(&[u8]) -> String,
    then: Option<&'static str>,
) -> (Result<(FleetScope, NodeProof), FleetError>, Vec<u8>) {
    let (mut listening, mut dialing) = CtlStream::pair().unwrap();
    listening.set_read_timeout(Some(HANG)).unwrap();
    listening.write_all(lines(E).as_bytes()).unwrap();
    if let Some(answer) = then {
        listening.write_all(answer.as_bytes()).unwrap();
    }
    let me = me.clone();
    let decided = std::thread::spawn(move || {
        dialing.set_read_timeout(Some(HANG)).unwrap();
        let mut refused_by = None;
        dialer_meets(&mut dialing, E, &me, FleetOp::Survey, &mut refused_by)
    })
    .join()
    .unwrap();
    let mut sent = Vec::new();
    let _ = listening.read_to_end(&mut sent);
    (decided, sent)
}

#[test]
fn the_client_does_not_prove_to_an_unproven_server() {
    let (listener, dialer, removed, stranger) = (node(0x10), node(0x20), node(0x30), node(0x40));
    let me = meshing(&dialer, verdict(&[&listener, &dialer], &[&removed]));
    let proves = |who: &NodeIdentity, signer: &NodeIdentity| {
        let (who, signer) = (who.clone(), signer.clone());
        move |exporter: &[u8]| {
            format!(
                "{}facts\n",
                forged(&who, &signer, Signer::MeshListener, exporter)
            )
        }
    };

    let (decided, sent) = dialer_hears(&me, proves(&listener, &listener), Some("OK\n"));
    assert_eq!(decided.unwrap(), (FleetScope::Facts, proof_of(&listener)));
    assert!(
        String::from_utf8(sent)
            .unwrap()
            .starts_with(&format!("survey\nnode {} ", dialer.node())),
        "the control: a member's listener gets the op and the dialer's proof"
    );

    for (lines, want) in [
        (proves(&stranger, &stranger), "unknown"),
        (proves(&removed, &removed), "out"),
        (proves(&listener, &stranger), "forged"),
    ] {
        let (decided, sent) = dialer_hears(&me, lines, None);
        match want {
            "unknown" => assert!(
                matches!(decided, Err(FleetError::ListenerUnknown)),
                "{decided:?}"
            ),
            "out" => assert!(
                matches!(decided, Err(FleetError::ListenerOut)),
                "{decided:?}"
            ),
            _ => assert!(matches!(decided, Err(FleetError::NodeProof)), "{decided:?}"),
        }
        assert!(sent.is_empty(), "{want}: the dialer named itself: {sent:?}");
    }

    // A dialer whose log is unread judges no listener, and so proves itself to none.
    let unread = meshing(&dialer, Denied::not_yet_read());
    let (decided, sent) = dialer_hears(&unread, proves(&listener, &listener), None);
    assert!(
        matches!(decided, Err(FleetError::VerdictUnread)),
        "{decided:?}"
    );
    assert!(sent.is_empty());
}

#[test]
fn over_tls_the_client_sends_nothing_to_a_listener_its_log_does_not_seat() {
    let (listener, dialer) = (node(0x10), node(0x20));
    let me = meshing(&dialer, verdict(&[&dialer], &[]));
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = tcp_listener.local_addr().unwrap();
    let proving = listener.clone();
    let listening = std::thread::spawn(move || {
        let tcp = tcp_listener.accept().unwrap().0;
        tcp.set_read_timeout(Some(HANG)).unwrap();
        let config = served_config(false, true);
        let (mut t, _) = accept_tls(tcp, config).unwrap();
        let exporter = t.exporter().to_vec();
        let line = format!(
            "{}content\n",
            node_line(Some(&proving), Signer::MeshListener, &exporter)
        );
        t.stream().write_all(line.as_bytes()).unwrap();
        t.stream().flush().unwrap();
        let mut sent = Vec::new();
        let _ = t.stream().read_to_end(&mut sent);
        sent
    });
    let tcp = TcpStream::connect(addr).unwrap();
    let refused = client_handshake_by(tcp, &secret(), Some(&me), FleetOp::Survey, HANG)
        .err()
        .expect("a listener the log does not seat is not proved to");
    assert!(
        matches!(refused.error, FleetError::ListenerUnknown),
        "{refused:?}"
    );
    assert!(refused.error.falls_back());
    assert!(listening.join().unwrap().is_empty(), "nothing was sent");
}

#[test]
fn a_reflected_proof_is_refused() {
    let (listener, dialer) = (node(0x10), node(0x20));
    let both = verdict(&[&listener, &dialer], &[]);
    // The listener's end sends back a proof in the dialer's role: the dialer's own words, as a
    // man in the middle that relays them would.
    let me = meshing(&dialer, both.clone());
    let reflect = |exporter: &[u8]| {
        format!(
            "{}facts\n",
            node_line(Some(&listener), Signer::MeshDialer, exporter)
        )
    };
    let (decided, sent) = dialer_hears(&me, reflect, None);
    assert!(
        matches!(decided, Err(FleetError::Reflection)),
        "{decided:?}"
    );
    assert!(sent.is_empty());
    // A dialer that sends a proof in the listener's role is refused the same way.
    let (heard, answer) = listener_hears(&listener, &both, &[FleetOp::Survey], |exporter| {
        format!(
            "survey\n{}",
            node_line(Some(&dialer), Signer::MeshListener, exporter)
        )
    });
    assert!(matches!(heard, Err(FleetError::Reflection)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO node"));
    // A fleet proof is no mesh proof, nor a mesh proof a fleet one: each role's label differs.
    let (heard, answer) = listener_hears(&listener, &both, &[FleetOp::Survey], |exporter| {
        format!(
            "survey\n{}",
            node_line(Some(&dialer), Signer::Dialer, exporter)
        )
    });
    assert!(matches!(heard, Err(FleetError::NodeProof)), "{heard:?}");
    assert_eq!(answer.as_deref(), Some("NO node"));
}

#[test]
fn a_seat_is_judged_by_key_and_the_roster_or_the_log_takes_it_out() {
    let (member, removed, stranger) = (node(0x20), node(0x30), node(0x40));
    let denied = verdict(&[&member], &[&removed]);
    assert_eq!(denied.seat(&proof_of(&member)), Seat::Member);
    assert_eq!(denied.seat(&proof_of(&removed)), Seat::Out);
    assert_eq!(denied.seat(&proof_of(&stranger)), Seat::Unknown);
    let claimed = NodeProof {
        node: member.node().to_owned(),
        key: stranger.key(),
    };
    assert_eq!(
        denied.seat(&claimed),
        Seat::Unknown,
        "the id decides nothing"
    );
    let roster = Denied {
        nodes: vec![member.node().to_owned()],
        ..denied.clone()
    };
    assert_eq!(roster.seat(&proof_of(&member)), Seat::Out);
    assert_eq!(
        Denied::not_yet_read().seat(&proof_of(&member)),
        Seat::Unread
    );
    assert_eq!(Denied::default().seat(&proof_of(&member)), Seat::Unknown);
    // From the log's verdict: `Denied::under` holds its members, and an unread log none.
    let log: MemberKeys = [
        (member.key(), Standing::Member),
        (removed.key(), Standing::Out("removed".to_owned())),
    ]
    .into_iter()
    .collect();
    let under = Denied::under(Some(&[]), Some(&log));
    assert_eq!(under.seat(&proof_of(&member)), Seat::Member);
    assert_eq!(under.seat(&proof_of(&removed)), Seat::Out);
    assert_eq!(under.seat(&proof_of(&stranger)), Seat::Unknown);
    let unread = MemberKeys::unreadable("busy");
    assert!(unread.members().is_empty());
    assert_eq!(
        Denied::under(Some(&[]), Some(&unread)).seat(&proof_of(&member)),
        Seat::Unread
    );
}

/// A dial the listener answers `NO unknown` goes again on `atf/2` and offers the listener's desk
/// this Mac's `Node` record once; one whose listener this Mac's log does not seat goes again and
/// offers nothing; one whose listener the log took out goes no further.
///
/// NEGATIVE CONTROL: offering on every fallback (dropping `matches!(error,
/// FleetError::UnknownKey)` in `reach`) offers twice, and the count after the unseated listener
/// fails.
#[test]
fn a_dial_that_meets_an_unknown_key_goes_again_on_atf2_and_a_removal_does_not() {
    let (listener, member, stranger) = (node(0x10), node(0x20), node(0x40));
    // The listener's log seats the member alone: the stranger's seat has no `Node` record there.
    let running = Running::start(
        Some(listener.clone()),
        fixed(verdict(&[&listener, &member], &[])),
        &[FleetOp::Survey],
        None,
    );
    let at = running.addr;
    let dial = |me: &NodeIdentity| {
        crate::fleet_member::dial(
            &at.ip().to_string(),
            Some(at.ip()),
            at.port(),
            (&secret(), Some(me)),
            FleetOp::Survey,
            HANG,
        )
    };

    let reached = dial(&meshing(&member, verdict(&[&listener], &[]))).unwrap();
    assert!(reached.mesh, "the control: a member meets on the mesh");
    assert!(running.seen().2);

    // The offer of this Mac's `Node` record, which only this test provides (this process's one
    // provision); each test's listener has an address of its own.
    assert!(crate::fleet_member::on_unknown_to(heard_unknown));
    let offered = || {
        UNKNOWN_HEARD
            .lock()
            .unwrap()
            .iter()
            .filter(|to| **to == at)
            .count()
    };
    assert_eq!(offered(), 0, "a member's dial offers nothing");

    let reached = dial(&meshing(&stranger, verdict(&[&listener], &[]))).unwrap();
    assert!(
        !reached.mesh,
        "refused `NO unknown`, then proved by the secret"
    );
    assert_eq!(reached.peer, Some(proof_of(&listener)));
    let (_, peer, meshed) = running.seen();
    assert_eq!(peer, Some(proof_of(&stranger)));
    assert!(!meshed);
    assert_eq!(
        offered(),
        1,
        "the listener's desk is offered the Node record"
    );

    // A listener this Mac's log binds to no seat is met on `atf/2` as well, and nothing is
    // offered: it is the listener's key this Mac's log lacks.
    let reached = dial(&meshing(&member, verdict(&[], &[]))).unwrap();
    assert!(!reached.mesh);
    let _ = running.seen();
    assert_eq!(offered(), 1);

    // A listener this Mac's log took out is proved to on neither.
    let unreached = dial(&meshing(&member, verdict(&[], &[&listener])))
        .err()
        .expect("a removed listener is not dialled on the fleet either");
    assert!(
        matches!(
            unreached,
            crate::fleet_member::Unreached::Handshake(_, FleetError::ListenerOut)
        ),
        "{unreached}"
    );
    assert_eq!(offered(), 1, "a removal offers nothing");
}

/// Every address an `atm/1` listener answered `NO unknown` from, in this process.
static UNKNOWN_HEARD: std::sync::Mutex<Vec<std::net::SocketAddr>> =
    std::sync::Mutex::new(Vec::new());

fn heard_unknown(to: std::net::SocketAddr) {
    UNKNOWN_HEARD.lock().unwrap().push(to);
}

/// The mesh switch is read into the node a loader returns: with it on and a verdict provided the
/// node dials `atm/1` first, with it off, or with no verdict, `atf/2` alone.
#[test]
fn a_loaded_node_dials_the_mesh_only_with_the_switch_on_and_a_verdict() {
    use crate::fleet_member::dialing_by;
    let provided = fixed(verdict(&[], &[]));
    assert!(dialing_by(node(0x50), true, Some(&provided)).dials_mesh());
    assert!(!dialing_by(node(0x50), false, Some(&provided)).dials_mesh());
    assert!(!dialing_by(node(0x50), true, None).dials_mesh());
    assert!(
        !dialing_by(
            meshing(&node(0x50), verdict(&[], &[])),
            false,
            Some(&provided)
        )
        .dials_mesh(),
        "the switch off takes a verdict the node carried away"
    );
}
