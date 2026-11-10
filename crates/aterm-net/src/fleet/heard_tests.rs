// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The noting call (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B6): what each end of a real
//! loopback [`ALPN_MESH`](super::super::ALPN_MESH) handshake notes, with the route facts each
//! Mac's [`ScriptedSight`] scripts, never the answers.

use super::super::{
    Denied, DenyFn, FleetOp, FleetScope, FleetSecret, Listening, NodeIdentity, client_handshake_by,
    serve_offering_with_ask_within,
};
use super::*;
use crate::fleet_member::node_principal;
use aterm_uds::stop::Stop;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

/// The hang detector of every wait here: a loopback handshake that must happen.
const HANG: Duration = Duration::from_secs(60);

/// The scripted wall clock: a moment on day 20,833.
const T0: u64 = 1_800_000_000_000;

/// The home LAN's router.
const LAN_HW: [u8; 6] = [0xa0, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5];

/// A private scratch state root, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        let root = std::env::temp_dir().join(format!(
            "aterm-fleet-heard-{}-{}",
            std::process::id(),
            aterm_uds::rand::hex_token::<4>().unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

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

/// A verdict whose log seats `members`.
fn seating(members: &[&NodeIdentity]) -> DenyFn {
    let denied = Arc::new(Denied {
        members: members.iter().map(|me| me.key()).collect(),
        ..Denied::default()
    });
    Arc::new(move || Arc::clone(&denied))
}

/// The default route of a Mac at `own` on the home LAN.
fn lan(own: &str) -> RouteFacts {
    RouteFacts {
        interface: "en0".to_owned(),
        address: own.parse().unwrap(),
        prefix: 24,
        gateway: Some("192.168.1.1".parse().unwrap()),
        gateway_hw: Some(LAN_HW.to_vec()),
    }
}

/// The noting call of a Mac whose state root is `root`, on the home LAN at `own`, its every
/// connection standing between its peer at `peer` and itself; with `ends` false, between the
/// loopback ends its sockets have.
fn noting_at(root: &Path, own: &str, peer: &str, ends: bool) -> Heard {
    Heard::under(
        root,
        Arc::new(ScriptedSight {
            now: Arc::new(|| T0),
            route: Some(lan(own)),
            ends: ends.then(|| (ip(peer), ip(own))),
        }),
    )
}

/// What the network place under `root` heard and booked: its answers, and the members booked on
/// the home LAN today.
fn heard_under(root: &Path) -> (Vec<Answer>, Vec<String>) {
    let dir = aterm_netplace::record::dir(root);
    let (days, passed) = aterm_netplace::heard::read_days(&dir);
    assert_eq!(passed, 0, "the home-day ledger reads whole");
    let booked = days
        .of(&NetworkId::of(&lan("192.168.1.20")).unwrap())
        .and_then(|entry| {
            entry
                .days
                .iter()
                .find(|day| day.day == T0 / aterm_netplace::heard::DAY_MS)
        })
        .map(|day| day.members.clone())
        .unwrap_or_default();
    (aterm_netplace::heard::read(&dir).answers, booked)
}

/// A listener whose node is `me`, judging dialers by `deny`, offering the mesh and serving
/// `Survey`: each connection it handed over tells `seen` whether it spoke the mesh.
struct Listener {
    addr: SocketAddr,
    stop: Arc<Stop>,
    serving: Option<std::thread::JoinHandle<()>>,
    seen: mpsc::Receiver<bool>,
}

impl Listener {
    fn start(me: NodeIdentity, deny: DenyFn) -> Self {
        const SERVES: &[FleetOp] = &[FleetOp::Survey];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(Stop::new());
        let (tell, seen) = mpsc::channel();
        let tell = Arc::new(Mutex::new(tell));
        let listening = Listening {
            secret: secret(),
            scope: FleetScope::Content,
            node: Some(me),
            serves: SERVES,
            slots: 4,
            per_source: 4,
            lifetime: HANG,
        };
        let serving = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                serve_offering_with_ask_within(
                    &listener,
                    (&listening, Arc::new(|| (FleetScope::Content, SERVES)), deny),
                    None,
                    &stop,
                    HANG,
                    move |served, _| {
                        let _ = tell
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .send(served.meshed());
                        Ok(())
                    },
                    |_| {},
                );
            })
        };
        Self {
            addr,
            stop,
            serving: Some(serving),
            seen,
        }
    }

    fn dial(&self, me: &NodeIdentity) -> Result<super::super::Dialed, super::super::Undialed> {
        let tcp = TcpStream::connect(self.addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        client_handshake_by(tcp, &secret(), Some(me), FleetOp::Survey, HANG)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// BOTH ENDS OF AN `atm/1` HANDSHAKE NOTE THE PEER (§11 B6): a member's node dials another's over
/// loopback, each Mac's sight standing the connection on the home LAN; each end's place then
/// holds the other's node-proven answer (lane fleet, logged, from the peer's address through this
/// Mac's, held by its interface), booked on the LAN, and its day in the home-day ledger.
///
/// NEGATIVE CONTROL: with the listener's hook in `serve_inner` removed, the listener's place
/// holds no answer and "the listener notes the dialer" fails; with the dialer's hook in
/// `client_handshake_by_on` removed, "the dialer notes the listener" fails.
#[test]
fn both_ends_of_a_mesh_handshake_note_the_peer_and_book_its_day() {
    let (them, us) = (node(0x10), node(0x20));
    let (listener_root, dialer_root) = (Scratch::new(), Scratch::new());
    let listening = them.clone().noting(Some(noting_at(
        listener_root.path(),
        "192.168.1.20",
        "192.168.1.30",
        true,
    )));
    let running = Listener::start(listening, seating(&[&them, &us]));
    let me = us
        .clone()
        .dialing_mesh(Some(seating(&[&them, &us])))
        .noting(Some(noting_at(
            dialer_root.path(),
            "192.168.1.30",
            "192.168.1.20",
            true,
        )));
    let dialed = running.dial(&me).unwrap();
    assert!(dialed.mesh);
    assert!(
        running.seen.recv_timeout(HANG).unwrap(),
        "served over the mesh"
    );

    let network = NetworkId::of(&lan("192.168.1.20"));
    for (root, peer, from, via, why) in [
        (
            listener_root.path(),
            &us,
            "192.168.1.30",
            "192.168.1.20",
            "the listener notes the dialer",
        ),
        (
            dialer_root.path(),
            &them,
            "192.168.1.20",
            "192.168.1.30",
            "the dialer notes the listener",
        ),
    ] {
        let (answers, booked) = heard_under(root);
        let key = member_key(Lane::Fleet, &node_principal(peer.key()));
        assert_eq!(answers.len(), 1, "{why}");
        let answer = &answers[0];
        assert_eq!(
            (
                answer.lane,
                answer.member.as_str(),
                answer.addr,
                answer.via,
                answer.iface.as_deref(),
                &answer.network,
                answer.logged,
                answer.at_ms,
            ),
            (
                Lane::Fleet,
                key.as_str(),
                Some(ip(from)),
                via.parse().ok(),
                Some("en0"),
                &network,
                true,
                T0,
            ),
            "{why}"
        );
        assert_eq!(booked, vec![key], "{why}: its day booked");
    }
}

/// ONLY THE MESH NOTES: a dialer whose node does not dial the mesh opens `atf/2` with a listener
/// that offers both, and neither end notes anything, whatever their noting calls; nor does a
/// dialer the listener's log does not seat, refused at the handshake.
///
/// NEGATIVE CONTROL: the hooks noting without their `mesh &&` guard note the `atf/2` dialer, and
/// "atf/2 notes nothing" fails.
#[test]
fn a_fleet_connection_or_a_refused_key_notes_nothing() {
    let (them, us, stranger) = (node(0x10), node(0x20), node(0x30));
    let (listener_root, dialer_root) = (Scratch::new(), Scratch::new());
    let noting = |root: &Path, own, peer| Some(noting_at(root, own, peer, true));
    let running = Listener::start(
        them.clone()
            .noting(noting(listener_root.path(), "192.168.1.20", "192.168.1.30")),
        seating(&[&them, &us]),
    );
    // The field the switch is read into, off: this node dials the fleet alone.
    let fleet_only = us
        .clone()
        .noting(noting(dialer_root.path(), "192.168.1.30", "192.168.1.20"));
    let dialed = running.dial(&fleet_only).unwrap();
    assert!(!dialed.mesh);
    assert!(!running.seen.recv_timeout(HANG).unwrap());
    assert_eq!(
        heard_under(listener_root.path()),
        (Vec::new(), Vec::new()),
        "atf/2 notes nothing"
    );
    assert_eq!(
        heard_under(dialer_root.path()),
        (Vec::new(), Vec::new()),
        "atf/2 notes nothing"
    );

    // A stranger meshing to a listener whose log does not seat it is refused at the handshake.
    let stranger = stranger
        .clone()
        .dialing_mesh(Some(seating(&[&them, &stranger])))
        .noting(noting(dialer_root.path(), "192.168.1.30", "192.168.1.20"));
    assert!(running.dial(&stranger).is_err(), "the listener refuses it");
    assert_eq!(
        heard_under(listener_root.path()),
        (Vec::new(), Vec::new()),
        "a refused key"
    );
    assert_eq!(
        heard_under(dialer_root.path()),
        (Vec::new(), Vec::new()),
        "a refused dial"
    );
}

/// AN ANSWER IS BOOKED ON THE NETWORK ONLY WHERE IT CAME OVER THE DEFAULT ROUTE: through the
/// route's address and interface, from another host on its subnet. A loopback connection's ends
/// (no script of its ends), a peer off the subnet, this Mac's own address and an answer through
/// another address or interface are noted but booked on no network; an IPv4-mapped peer is read
/// as its IPv4 address.
///
/// NEGATIVE CONTROL: `answer_of` with its `route.on_link(addr)` check dropped books the peer off
/// the subnet, and "a peer off the subnet" fails.
#[test]
fn an_answer_is_booked_only_where_it_came_over_the_default_route() {
    let peer = proof_of(&node(0x10));
    let route = lan("192.168.1.20");
    let network = NetworkId::of(&route);
    let booked = |addr: &str, via: &str, iface: &str| {
        answer_of(
            &peer,
            (ip(addr), via.parse().ok()),
            (Some(&route), Some(iface.to_owned())),
            (T0 - 1, T0),
        )
        .network
    };
    assert_eq!(booked("192.168.1.30", "192.168.1.20", "en0"), network);
    assert_eq!(
        booked("::ffff:192.168.1.30", "192.168.1.20", "en0"),
        network
    );
    assert_eq!(
        booked("10.9.0.30", "192.168.1.20", "en0"),
        None,
        "a peer off the subnet"
    );
    assert_eq!(
        booked("192.168.1.20", "192.168.1.20", "en0"),
        None,
        "this Mac itself"
    );
    assert_eq!(
        booked("192.168.1.30", "192.168.1.21", "en0"),
        None,
        "another address"
    );
    assert_eq!(
        booked("192.168.1.30", "192.168.1.20", "en7"),
        None,
        "another interface"
    );
    assert_eq!(booked("127.0.0.1", "127.0.0.1", "lo0"), None, "loopback");
    let answer = answer_of(
        &peer,
        (ip("::ffff:192.168.1.30"), None),
        (None, None),
        (T0 - 1, T0),
    );
    assert_eq!(
        (answer.addr, answer.network, answer.logged, answer.lane),
        (Some(ip("192.168.1.30")), None, true, Lane::Fleet)
    );
    assert_eq!(
        answer.member,
        member_key(Lane::Fleet, &node_principal(peer.key))
    );

    // A real loopback connection with no script of its ends is noted for presentation alone.
    let (them, us) = (node(0x10), node(0x20));
    let root = Scratch::new();
    let running = Listener::start(
        them.clone().noting(Some(noting_at(
            root.path(),
            "192.168.1.20",
            "192.168.1.30",
            false,
        ))),
        seating(&[&them, &us]),
    );
    let me = us.clone().dialing_mesh(Some(seating(&[&them, &us])));
    assert!(running.dial(&me).unwrap().mesh);
    assert!(running.seen.recv_timeout(HANG).unwrap());
    assert_eq!(
        heard_under(root.path()),
        (Vec::new(), Vec::new()),
        "nothing for admission"
    );
    let seen = aterm_netplace::heard::read_for_place(&aterm_netplace::record::dir(root.path()));
    assert_eq!(seen.answers.len(), 1, "noted for presentation");
    assert_eq!(
        (seen.answers[0].addr, &seen.answers[0].network),
        (Some(ip("127.0.0.1")), &None)
    );
}

/// ONE MAC IS ONE MEMBER, AND AN ANSWER OFF THE ROUTE STANDS IN FOR NONE: a key that names
/// itself by two node ids is noted as one member, keyed by its principal; and a member heard over
/// the default route, then two minutes later over another of this Mac's addresses, keeps its
/// answer over the route in the admission ledger.
///
/// NEGATIVE CONTROL: `answer_of` keying by the claimed id (`peer.node`) notes two members, and
/// "one key is one member" fails; `Heard::note` sending the off-route answer to the admission
/// ledger replaces the member's answer over the route, and "the answer over the route stands"
/// fails.
#[test]
fn one_key_is_one_member_and_an_off_route_answer_shadows_none() {
    let root = Scratch::new();
    let at = Arc::new(std::sync::atomic::AtomicU64::new(T0));
    let sight = |ends: Option<(IpAddr, IpAddr)>| {
        let at = Arc::clone(&at);
        Heard::under(
            root.path(),
            Arc::new(ScriptedSight {
                now: Arc::new(move || at.load(Ordering::Acquire)),
                route: Some(lan("192.168.1.20")),
                ends,
            }),
        )
    };
    let on_route = sight(Some((ip("192.168.1.30"), ip("192.168.1.20"))));
    let them = node(0x10);
    let named = |node: &str| NodeProof {
        node: node.to_owned(),
        key: them.key(),
    };
    let ends: (SocketAddr, SocketAddr) = (
        "127.0.0.1:4000".parse().unwrap(),
        "127.0.0.1:5000".parse().unwrap(),
    );
    on_route.note(&named(them.node()), ends, T0);
    at.store(T0 + 120_000, Ordering::Release);
    on_route.note(&named(&"ab".repeat(16)), ends, T0 + 120_000);
    let (answers, booked) = heard_under(root.path());
    let key = member_key(Lane::Fleet, &node_principal(them.key()));
    assert_eq!(
        answers
            .iter()
            .map(|answer| answer.member.as_str())
            .collect::<Vec<_>>(),
        vec![key.as_str()],
        "one key is one member"
    );
    assert_eq!(booked, vec![key.clone()]);

    // Two minutes on, the member is heard through this Mac's second address on the subnet.
    at.store(T0 + 240_000, Ordering::Release);
    sight(Some((ip("192.168.1.30"), ip("192.168.1.21")))).note(
        &named(them.node()),
        ends,
        T0 + 240_000,
    );
    let (answers, _) = heard_under(root.path());
    assert_eq!(answers.len(), 1);
    assert_eq!(
        (answers[0].at_ms, &answers[0].network),
        (T0 + 120_000, &NetworkId::of(&lan("192.168.1.20"))),
        "the answer over the route stands"
    );
}

/// A neighbour link's note notes each re-proof, off the link's thread, and hands it on once
/// noted; it passes over the handshake that opened the link, which the node identity of the end
/// that completed it noted, and hands that on at once.
///
/// NEGATIVE CONTROL: `Heard::noting_then` noting every proof notes the handshake too, and "the
/// handshake is the identity's" fails.
#[test]
fn a_links_note_notes_its_reproofs() {
    let root = Scratch::new();
    let heard = noting_at(root.path(), "192.168.1.20", "192.168.1.30", true);
    let (handed, handing) = mpsc::channel();
    let handed = Mutex::new(handed);
    let note = heard.noting_then(Arc::new(move |proven: &Proven| {
        let _ = handed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send(proven.by);
    }));
    let proven = |by| Proven {
        peer: proof_of(&node(0x10)),
        addr: "127.0.0.1:4000".parse().unwrap(),
        local: "127.0.0.1:5000".parse().unwrap(),
        at: Duration::from_secs(300),
        by,
    };
    note(&proven(ProvenBy::Handshake));
    assert_eq!(handing.recv_timeout(HANG), Ok(ProvenBy::Handshake));
    assert_eq!(
        heard_under(root.path()),
        (Vec::new(), Vec::new()),
        "the handshake is the identity's"
    );
    note(&proven(ProvenBy::Reproof));
    assert_eq!(
        handing.recv_timeout(HANG),
        Ok(ProvenBy::Reproof),
        "handed on once noted"
    );
    let (answers, booked) = heard_under(root.path());
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].network, NetworkId::of(&lan("192.168.1.20")));
    assert_eq!(
        booked,
        vec![member_key(Lane::Fleet, &node_principal(node(0x10).key()))]
    );
}
