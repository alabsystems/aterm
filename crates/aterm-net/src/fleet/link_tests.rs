// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Neighbour links (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, §11 B5): ops as streams of one
//! `atm/1` connection, each held to its own credit, and the 5-minute re-proof on an injected clock.

use super::super::{
    Denied, DenyFn, FleetEvent, FleetScope, FleetSecret, Listening, NodeIdentity,
    serve_offering_with_ask_within,
};
use super::*;
use std::net::TcpListener;
use std::sync::mpsc;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The hang detector of every wait here: loopback traffic that must happen.
const HANG: Duration = Duration::from_secs(60);

/// Home evidence's freshness (`aterm_secrets::home_lan::FRESH`), which this crate cannot name.
const FRESH: Duration = Duration::from_secs(10 * 60);

fn node(byte: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{byte:02x}").repeat(16), &[byte; 32]).unwrap()
}

fn seating(members: &[&NodeIdentity]) -> Denied {
    Denied {
        members: members.iter().map(|me| me.key()).collect(),
        ..Denied::default()
    }
}

/// One Mac of a loopback mesh: its node, its neighbour table on a manual clock, the proofs it
/// noted, the streams it served, and its listener.
struct Mac {
    me: NodeIdentity,
    table: Arc<Neighbours>,
    clock: Arc<ManualClock>,
    verdict: Arc<Mutex<Denied>>,
    notes: mpsc::Receiver<Proven>,
    served: mpsc::Receiver<(FleetOp, Option<BusLog>)>,
    /// Gates each `Bus metrics` stream's first read: a stream whose reader does not read.
    gate: Arc<(Mutex<bool>, Condvar)>,
    addr: SocketAddr,
    admitted: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
    unread: Arc<Mutex<Vec<super::super::Served>>>,
}

/// What a Mac answers on each stream: it echoes what it reads behind its op's name, refuses
/// `Drive` (a `content` grant's listener), and holds a `Bus metrics` stream's first read until
/// its gate opens.
fn serving(
    served: mpsc::Sender<(FleetOp, Option<BusLog>)>,
    gate: Arc<(Mutex<bool>, Condvar)>,
) -> ServeFn {
    let served = Arc::new(Mutex::new(served));
    Arc::new(move |incoming: Incoming| {
        let _ = lock(&served).send((incoming.op(), incoming.log()));
        if incoming.op() == FleetOp::Drive {
            incoming.refuse("op");
            return;
        }
        let log = incoming.log();
        let mut stream = incoming.accept();
        stream.set_read_timeout(Some(HANG));
        stream.set_write_timeout(Some(HANG));
        if log == Some(BusLog::Metrics) {
            let (open, opened) = &*gate;
            let mut open = lock(open);
            while !*open {
                open = opened.wait(open).unwrap();
            }
        }
        let mut got = Vec::new();
        if stream.read_to_end(&mut got).is_err() {
            return;
        }
        let named = log.map_or_else(
            || stream.op().word().to_owned(),
            |log| format!("{} {}", stream.op().word(), log.word()),
        );
        let _ = stream.write_all(format!("{named}:{}:", got.len()).as_bytes());
        let _ = stream.write_all(&got);
        let _ = stream.finish();
    })
}

impl Mac {
    fn start(me: NodeIdentity, seats: &[&NodeIdentity]) -> Self {
        Self::start_living(me, seats, HANG)
    }

    /// [`Self::start`], its listener cutting each proved connection `lifetime` after its proof.
    fn start_living(me: NodeIdentity, seats: &[&NodeIdentity], lifetime: Duration) -> Self {
        Self::start_reading(me, seats, lifetime, true)
    }

    fn start_reading(
        me: NodeIdentity,
        seats: &[&NodeIdentity],
        lifetime: Duration,
        reads: bool,
    ) -> Self {
        let clock = ManualClock::new();
        let verdict = Arc::new(Mutex::new(seating(seats)));
        let deny: DenyFn = {
            let verdict = Arc::clone(&verdict);
            Arc::new(move || Arc::new(lock(&verdict).clone()))
        };
        let (noted, notes) = mpsc::channel();
        let noted = Arc::new(Mutex::new(noted));
        let (said, served) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let cfg = LinkConfig {
            verdict: Arc::clone(&deny),
            serve: serving(said, Arc::clone(&gate)),
            note: Arc::new(move |proven: &Proven| {
                let _ = lock(&noted).send(proven.clone());
            }),
            clock: Arc::clone(&clock) as Arc<dyn LinkClock>,
        };
        let me = me.dialing_mesh(Some(Arc::clone(&deny)));
        let table = Neighbours::new(me.clone(), cfg);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let admitted = Arc::new(AtomicU64::new(0));
        let unread = Arc::new(Mutex::new(Vec::new()));
        let listening = Listening {
            secret: FleetSecret::new(&[7; FleetSecret::MIN_LEN]).unwrap(),
            scope: FleetScope::Content,
            node: Some(me.clone()),
            serves: &[FleetOp::Neighbour, FleetOp::Survey],
            slots: 4,
            per_source: 4,
            lifetime,
        };
        let serving = {
            let unread = Arc::clone(&unread);
            let (running, admitted, table) = (
                Arc::clone(&running),
                Arc::clone(&admitted),
                Arc::clone(&table),
            );
            let me = me.clone();
            std::thread::spawn(move || {
                let serves: &'static [FleetOp] = &[FleetOp::Neighbour, FleetOp::Survey];
                serve_offering_with_ask_within(
                    &listener,
                    (
                        &listening,
                        Arc::new(move || (FleetScope::Content, serves)),
                        deny,
                    ),
                    None,
                    &running,
                    HANG,
                    move |served, op| {
                        if op != FleetOp::Neighbour {
                            return Ok(());
                        }
                        if !reads {
                            lock(&unread).push(served);
                            return Ok(());
                        }
                        let link = served.into_link(&me, table.config())?;
                        table.adopt(link);
                        Ok(())
                    },
                    move |event| {
                        if matches!(event, FleetEvent::Admitted { .. }) {
                            admitted.fetch_add(1, Ordering::AcqRel);
                        }
                    },
                );
            })
        };
        Self {
            me,
            table,
            clock,
            verdict,
            notes,
            served,
            gate,
            addr,
            admitted,
            running,
            serving: Some(serving),
            unread,
        }
    }

    fn dial(&self, other: &Self) -> Arc<Link> {
        self.table
            .dial(other.addr, HANG)
            .expect("a link to a member")
    }

    /// The link this Mac holds to `other`, once it holds one.
    fn link_to(&self, other: &Self) -> Arc<Link> {
        let until = Instant::now() + HANG;
        loop {
            if let Some(link) = self.table.link_to(other.me.node()) {
                return link;
            }
            assert!(Instant::now() < until, "no link within the hang detector");
            aterm_uds::deadline::poll_yield();
        }
    }

    fn note(&self) -> Proven {
        self.notes
            .recv_timeout(HANG)
            .expect("a noted proof within the hang detector")
    }

    fn open_gate(&self) {
        let (open, opened) = &*self.gate;
        *lock(open) = true;
        opened.notify_all();
    }
}

impl Drop for Mac {
    fn drop(&mut self) {
        self.open_gate();
        self.table.stop();
        self.running.store(false, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// `body` sent on a fresh `op` stream of `link`, and the echo read back.
fn round_trip(link: &Link, op: FleetOp, log: Option<BusLog>, body: &[u8]) -> Vec<u8> {
    let mut stream = link.open(op, log, HANG).expect("an accepted stream");
    stream.set_read_timeout(Some(HANG));
    stream.set_write_timeout(Some(HANG));
    stream.write_all(body).unwrap();
    stream.finish().unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).unwrap();
    back
}

fn echoed(op: &str, body: &[u8]) -> Vec<u8> {
    let mut back = format!("{op}:{}:", body.len()).into_bytes();
    back.extend_from_slice(body);
    back
}

#[test]
fn two_bus_streams_and_a_survey_share_one_link() {
    let (a, b) = (node(0x0a), node(0x0b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    assert_eq!(link.peer().node, b.node());
    let held = mac_b.link_to(&mac_a);
    assert!(!held.dialed());

    let tables = link.open(FleetOp::Bus, Some(BusLog::Tables), HANG).unwrap();
    let metrics = link
        .open(FleetOp::Bus, Some(BusLog::Metrics), HANG)
        .unwrap();
    for stream in [&tables, &metrics] {
        stream.set_read_timeout(Some(HANG));
        stream.set_write_timeout(Some(HANG));
    }
    // More than one stream's window waits on `Bus metrics`, whose reader does not read yet.
    let bulk = vec![0x5au8; 4 * WINDOW as usize];
    let writing = {
        let bulk = bulk.clone();
        std::thread::spawn(move || {
            let mut metrics = metrics;
            metrics.write_all(&bulk).unwrap();
            metrics.finish().unwrap();
            let mut back = Vec::new();
            metrics.read_to_end(&mut back).unwrap();
            back
        })
    };
    // The other streams move while it waits: a Survey, and the second Bus stream.
    assert_eq!(
        round_trip(&link, FleetOp::Survey, None, b"survey --json"),
        echoed("survey", b"survey --json")
    );
    (&tables).write_all(b"record 1").unwrap();
    tables.finish().unwrap();
    let mut back = Vec::new();
    (&tables).read_to_end(&mut back).unwrap();
    assert_eq!(back, echoed("bus tables", b"record 1"));

    mac_b.open_gate();
    assert_eq!(writing.join().unwrap(), echoed("bus metrics", &bulk));
    drop(tables);

    let mut served: Vec<_> = mac_b.served.try_iter().collect();
    served.sort_by_key(|(op, log)| (op.word(), *log));
    assert_eq!(
        served,
        [
            (FleetOp::Bus, Some(BusLog::Tables)),
            (FleetOp::Bus, Some(BusLog::Metrics)),
            (FleetOp::Survey, None),
        ]
    );
    // One connection carried them all.
    assert_eq!(mac_b.admitted.load(Ordering::Acquire), 1);
    assert_eq!(mac_a.table.links().len(), 1);
    assert_eq!(mac_b.table.links().len(), 1);
}

#[test]
fn a_stream_the_peer_refuses_says_why_and_the_link_goes_on() {
    let (a, b) = (node(0x1a), node(0x1b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let refused = link.open(FleetOp::Drive, None, HANG).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(refused.to_string().contains("op"), "{refused}");
    // A Bus stream names its log, and only a log this build knows.
    assert_eq!(
        link.open(FleetOp::Bus, None, HANG).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        link.open(FleetOp::Neighbour, None, HANG)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        round_trip(&link, FleetOp::Units, None, b"GET"),
        echoed("units", b"GET")
    );
    assert!(link.closed().is_none());
}

#[test]
fn a_quiet_link_reproves_once_each_five_minutes_and_stays_fresh() {
    let (a, b) = (node(0x2a), node(0x2b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let handshake = mac_a.note();
    assert_eq!(handshake.by, ProvenBy::Handshake);
    assert_eq!(handshake.peer.node, b.node());
    assert_eq!(handshake.at, Duration::ZERO);

    // Nothing is due before five minutes.
    mac_a.clock.advance(REPROVE_EVERY - Duration::from_secs(1));
    mac_a.table.tick();
    assert_eq!(link.reproofs(), (0, 0));

    mac_a.clock.advance(Duration::from_secs(1));
    mac_a.table.tick();
    let proven = mac_a.note();
    assert_eq!((proven.by, proven.at), (ProvenBy::Reproof, REPROVE_EVERY));
    assert_eq!(proven.peer.key, b.key());
    // Exactly one: the clock has not moved, so another tick asks nothing.
    mac_a.table.tick();
    assert_eq!(link.reproofs(), (1, 1));
    assert!(mac_a.notes.try_recv().is_err());

    // Thirty minutes on, a round each five minutes keeps a proof younger than FRESH.
    for round in 2..=6u32 {
        mac_a.clock.advance(REPROVE_EVERY);
        mac_a.table.tick();
        let proven = mac_a.note();
        assert_eq!(proven.at, REPROVE_EVERY * round);
        assert_eq!(link.reproofs(), (u64::from(round), u64::from(round)));
    }
    let now = mac_a.clock.now();
    assert_eq!(now, Duration::from_secs(30 * 60));
    assert!(now - REPROVE_EVERY * 6 < FRESH);
    assert!(link.closed().is_none());
}

#[test]
fn the_timer_reproves_when_the_clock_reaches_the_deadline() {
    let (a, b) = (node(0x3a), node(0x3b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let _link = mac_a.dial(&mac_b);
    assert_eq!(mac_a.note().by, ProvenBy::Handshake);
    let timer = mac_a.table.spawn_timer().unwrap();
    mac_a.clock.advance(REPROVE_EVERY);
    let proven = mac_a.note();
    assert_eq!((proven.by, proven.at), (ProvenBy::Reproof, REPROVE_EVERY));
    mac_a.table.stop();
    timer.join().unwrap();
}

#[test]
fn a_reproof_left_unanswered_ends_the_link() {
    let (a, b) = (node(0x4a), node(0x4b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    mac_b.link_to(&mac_a).mute_reproofs();
    mac_a.clock.advance(REPROVE_EVERY);
    mac_a.table.tick();
    assert_eq!(link.reproofs(), (1, 0));
    mac_a.clock.advance(REPROVE_WITHIN);
    mac_a.table.tick();
    assert_eq!(
        link.closed().as_deref(),
        Some("the neighbour left a re-proof unanswered")
    );
    assert!(mac_a.table.link_to(b.node()).is_none());
}

#[test]
fn a_key_the_log_took_out_ends_its_link_at_the_next_reproof() {
    let (a, b) = (node(0x5a), node(0x5b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    assert_eq!(mac_a.note().by, ProvenBy::Handshake);
    *lock(&mac_a.verdict) = Denied {
        members: vec![a.key()],
        keys: vec![b.key()],
        ..Denied::default()
    };
    mac_a.clock.advance(REPROVE_EVERY);
    mac_a.table.tick();
    let until = Instant::now() + HANG;
    while link.closed().is_none() {
        assert!(Instant::now() < until, "the link still runs");
        aterm_uds::deadline::poll_yield();
    }
    // Its proof verified, but no answer of a key the log holds out is noted.
    assert_eq!(link.reproofs(), (1, 1));
    assert!(mac_a.notes.try_recv().is_err());
}

#[test]
fn a_forged_or_reflected_reproof_ends_the_link() {
    let (a, b, c) = (node(0x6a), node(0x6b), node(0x6c));
    for (signer, end, why) in [
        (&b, Signer::ReproveDialer, "a reflected re-proof"),
        (&c, Signer::ReproveListener, "does not verify"),
    ] {
        let mac_a = Mac::start(a.clone(), &[&a, &b]);
        let mac_b = Mac::start(b.clone(), &[&a, &b]);
        let link = mac_a.dial(&mac_b);
        mac_b.link_to(&mac_a).mute_reproofs();
        mac_a.clock.advance(REPROVE_EVERY);
        mac_a.table.tick();
        let nonce = lock(&link.shared.state)
            .reproof
            .pending
            .as_ref()
            .unwrap()
            .nonce;
        let material = link.shared.reproof_material(&nonce).unwrap();
        let mut answer = nonce.to_vec();
        answer.extend_from_slice(&signer.sign(end, &material));
        let refused = link.shared.proven(&answer).unwrap_err();
        assert!(refused.contains(why), "{refused}");
        assert_eq!(link.reproofs(), (1, 0));
    }
}

#[test]
fn a_pair_that_dials_each_other_keeps_one_link_the_lower_node_dialed() {
    let (a, b) = (node(0x7a), node(0x7b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let from_b = mac_b.dial(&mac_a);
    let from_a = mac_a.dial(&mac_b);
    // `a`'s node id is the lower: its dial is the one both ends keep.
    assert!(a.node() < b.node());
    let kept_a = mac_a.link_to(&mac_b);
    assert!(kept_a.dialed());
    assert!(Arc::ptr_eq(&kept_a, &from_a));
    let until = Instant::now() + HANG;
    while from_b.closed().is_none() {
        assert!(Instant::now() < until, "the duplicate still runs");
        aterm_uds::deadline::poll_yield();
    }
    let kept_b = mac_b.link_to(&mac_a);
    assert!(!kept_b.dialed());
    assert_eq!(mac_a.table.links().len(), 1);
    assert_eq!(
        round_trip(&kept_a, FleetOp::MailCode, None, b"mail-code x"),
        echoed("mail-code", b"mail-code x")
    );
}

#[test]
fn only_the_mesh_carries_a_link_or_its_ops() {
    for op in [
        FleetOp::Neighbour,
        FleetOp::Bus,
        FleetOp::Inventory,
        FleetOp::Desk,
        FleetOp::Units,
    ] {
        assert!(op.mesh_only() && op.meshed(), "{op:?}");
        assert_eq!(FleetOp::from_word(op.word()), Some(op));
    }
    for op in [FleetOp::Survey, FleetOp::Drive, FleetOp::MailCode] {
        assert!(!op.mesh_only() && op.meshed(), "{op:?}");
    }
    let (a, b) = (node(0x8a), node(0x8b));
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    // A node that judges no mesh listener cannot dial a link: it never offers atf/2 for one.
    let tcp = TcpStream::connect(mac_b.addr).unwrap();
    let refused = dial(tcp, &a, HANG, mac_b.table.config()).unwrap_err();
    assert!(
        matches!(refused.error, FleetError::Alpn),
        "{:?}",
        refused.error
    );
    // An unseated dialer reaches no link.
    let stranger = node(0x8c).dialing_mesh(Some(Arc::new({
        let (b, c) = (b.clone(), node(0x8c));
        move || Arc::new(seating(&[&b, &c]))
    })));
    let tcp = TcpStream::connect(mac_b.addr).unwrap();
    let refused = dial(tcp, &stranger, HANG, mac_b.table.config()).unwrap_err();
    assert!(
        matches!(refused.error, FleetError::UnknownKey),
        "{:?}",
        refused.error
    );
    assert!(mac_b.table.links().is_empty());
}

#[test]
fn a_link_outlives_the_listeners_lifetime_and_holds_no_handshake_slot() {
    let (a, b) = (node(0x9a), node(0x9b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start_living(b.clone(), &[&a, &b], Duration::from_millis(1));
    let link = mac_a.dial(&mac_b);
    // Each accept runs the listener's lifetime cut; more connections than it has slots for are
    // accepted, so the link holds none of them.
    for _ in 0..8 {
        let tcp = TcpStream::connect(mac_b.addr).unwrap();
        tcp.set_read_timeout(Some(HANG)).unwrap();
        // Each says nothing and hangs up its half, so the listener drops it; read to its end.
        tcp.shutdown(Shutdown::Write).unwrap();
        let mut end = Vec::new();
        let _ = (&tcp).read_to_end(&mut end);
    }
    assert_eq!(
        round_trip(&link, FleetOp::Survey, None, b"version"),
        echoed("survey", b"version")
    );
    assert!(link.closed().is_none());
}

/// A peer that answers a stream and drops its end before this end's `End` sends its `Accept`, its
/// answer, its `End` and a `Reset` (`closed`). An opener that wakes only once all four arrived
/// takes the stream and reads the answer to its end, and so does one that wakes only once the
/// link ended behind them: under the full aterm-gui suite on 2026-11-05 the node service's desk
/// stream, so answered, read as refused ("the neighbour refused the `desk` stream: closed").
/// CONTROL: a `Reset` with no `Accept` before it refuses, and a link that ended with nothing
/// answered aborts the open.
#[test]
fn an_answer_whose_reset_arrived_before_its_opener_woke_is_taken_and_read() {
    let (a, b) = (node(0x2a), node(0x2b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    // A stream this end opened, every frame of its answer handled before its opener waits.
    let arrived = |frames: &[(Kind, &[u8])], ended: bool| {
        let id = {
            let mut state = link.shared.link_state();
            let id = state.next_id;
            state.next_id = id.checked_add(2).unwrap();
            state.streams.insert(id, Stream::new(true));
            id
        };
        for &(kind, payload) in frames {
            link.shared
                .handle(kind, id, payload)
                .expect("a frame the link takes");
        }
        if ended {
            link.shared
                .fail("the neighbour closed it: its node service stopped");
        }
        let stream = LinkStream {
            link: Arc::clone(&link.shared),
            id,
            op: FleetOp::Desk,
        };
        Link::answer(stream, HANG)
    };
    const SAID: &[u8] = b"NO why=this%20Mac%27s%20window%20is%20not%20running\n";
    let answer: &[(Kind, &[u8])] = &[
        (Kind::Accept, &[]),
        (Kind::Data, SAID),
        (Kind::End, &[]),
        (Kind::Reset, b"closed"),
    ];
    let read = |stream: io::Result<LinkStream>| {
        let mut stream = stream.expect("an accepted stream");
        stream.set_read_timeout(Some(HANG));
        let mut said = Vec::new();
        stream.read_to_end(&mut said).unwrap();
        said
    };
    assert_eq!(read(arrived(answer, false)), SAID);

    let refused = arrived(&[(Kind::Reset, b"op")], false).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "CONTROL");
    assert!(
        refused
            .to_string()
            .contains("the neighbour refused the `desk` stream: op"),
        "{refused}"
    );
    assert!(link.closed().is_none());

    assert_eq!(
        read(arrived(answer, true)),
        SAID,
        "the link ended behind it"
    );
    let aborted = arrived(&[], true).unwrap_err();
    assert_eq!(aborted.kind(), io::ErrorKind::ConnectionAborted, "CONTROL");
    assert!(
        aborted.to_string().contains("its node service stopped"),
        "{aborted}"
    );
}

/// A link's `Close` goes out in the write that carries the TLS close, so its peer reads both in
/// one socket read: the peer's link ends with the reason the `Close` named, not with the TLS
/// close behind it.
#[test]
fn a_link_its_peer_closes_ends_with_the_reason_the_peer_gave() {
    let (a, b) = (node(0x3a), node(0x3b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let held = mac_b.link_to(&mac_a);
    let (ended, rx) = mpsc::channel();
    let seen = Arc::clone(&link);
    link.on_end(Box::new(move || ended.send(seen.closed()).unwrap()));
    link.close("this Mac's node service stopped");
    let until = Instant::now() + HANG;
    let why = loop {
        if let Some(why) = held.closed() {
            break why;
        }
        assert!(Instant::now() < until, "no end within the hang detector");
        aterm_uds::deadline::poll_yield();
    };
    assert_eq!(
        why,
        "the neighbour closed it: this Mac's node service stopped"
    );
    assert_eq!(
        rx.recv_timeout(HANG).unwrap().as_deref(),
        Some("this Mac's node service stopped"),
        "a successful drain records the same local reason"
    );
}

/// A proved peer keeps its socket but never reads after the handshake. Fill the writer past
/// the socket's capacity, then stop the generation. Only the injected clock decides the drain
/// bound; real waits are one-minute hang detectors. The old queue-only close is the negative
/// control: advancing the same clock leaves it open with its Close still queued.
#[test]
fn an_unreading_peer_cannot_hold_generation_teardown() {
    use std::os::fd::AsRawFd;

    for bounded in [false, true] {
        let (a, b) = (node(0x5a), node(0x5b));
        let mac_a = Mac::start(a.clone(), &[&a, &b]);
        let mac_b = Mac::start_reading(b.clone(), &[&a, &b], HANG, false);
        let link = mac_a.dial(&mac_b);
        let until = Instant::now() + HANG;
        while lock(&mac_b.unread).is_empty() {
            assert!(Instant::now() < until, "the peer keeps its proved socket");
            aterm_uds::deadline::poll_yield();
        }
        {
            let mut state = link.shared.link_state();
            for _ in 0..4096 {
                state.data.push_back(frame(Kind::Data, 1, &[0; FRAME_MAX]));
            }
        }
        link.shared.changed.notify_all();
        loop {
            let mut fd = libc::pollfd {
                fd: link.shared.sock.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: fd names a live socket and points to one initialized pollfd for this call.
            let ready = unsafe { libc::poll(&raw mut fd, 1, 0) };
            assert!(ready >= 0, "poll succeeds");
            if ready == 0 {
                break;
            }
            assert!(
                Instant::now() < until,
                "the unreading peer fills the socket"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert!(!link.shared.link_state().data.is_empty());
        // Keep the queued Close from being prepared even if an ACK briefly frees socket
        // space. The deadline must need neither the writer nor its TLS lock to make progress.
        let tls = lock(&link.shared.tls);
        let (ended, rx) = mpsc::channel();
        let seen = Arc::clone(&link);
        link.on_end(Box::new(move || ended.send(seen.closed()).unwrap()));
        let why = "this Mac's node service stopped";
        if bounded {
            mac_a.table.stop();
        } else {
            // NEGATIVE CONTROL: the former stop only queued Close, with no drain timer.
            assert!(link.shared.queue_close(why).is_some());
        }
        mac_a.clock.advance(CLOSE_DRAIN - Duration::from_nanos(1));
        assert!(link.closed().is_none(), "the deadline has not arrived");
        mac_a.clock.advance(Duration::from_nanos(1));
        if bounded {
            assert_eq!(rx.recv_timeout(HANG).unwrap().as_deref(), Some(why));
            assert_eq!(link.closed().as_deref(), Some(why));
            mac_a.table.stop();
            assert!(mac_a.table.links().is_empty());
            assert!(mac_a.table.adopt(Arc::clone(&link)).is_none());
        } else {
            assert!(link.closed().is_none(), "old stop still waits on its peer");
            assert!(rx.try_recv().is_err());
            link.abort("end the negative control");
            assert_eq!(rx.recv_timeout(HANG).unwrap().as_deref(), Some(why));
        }
        drop(tls);
    }
}

/// What a holder hooks on a link runs once when the link ends, after `closed` reads the end, on
/// either end; one hooked on a link that ended runs at once.
///
/// NEGATIVE CONTROL: the hooks taken before `closed` is set (the `take` moved above the
/// assignment in `fail_held`) run while `closed` still reads `None`, and the first assertion in
/// the hook fails.
#[test]
fn a_hook_on_a_link_runs_once_when_it_ends() {
    let (a, b) = (node(0x3c), node(0x3d));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let held = mac_b.link_to(&mac_a);
    let (ended, rx) = mpsc::channel();
    for (name, end) in [("dialer", &link), ("held", &held)] {
        let (ended, end_seen) = (ended.clone(), Arc::clone(end));
        end.on_end(Box::new(move || {
            assert!(
                end_seen.closed().is_some(),
                "{name}: the end reads closed first"
            );
            ended.send(name).expect("the test waits");
        }));
    }
    link.close("this Mac's node service stopped");
    let mut seen: Vec<&str> = (0..2)
        .map(|_| rx.recv_timeout(HANG).expect("each end's hook runs"))
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, ["dialer", "held"]);
    let at_once = ended;
    held.on_end(Box::new(move || {
        at_once.send("late").expect("the test waits")
    }));
    assert_eq!(
        rx.try_recv(),
        Ok("late"),
        "a hook on an ended link runs at once"
    );
    assert!(rx.try_recv().is_err(), "each hook ran once");
}

/// A stream this end opened on `link`, accepted by its peer or not, registered with no `Open`
/// sent: its peer serves nothing on it, and only frames the test hands the link's handler, or has
/// the peer send for its id, move it.
fn opened_here(link: &Link, accepted: bool) -> u32 {
    let mut state = link.shared.link_state();
    let id = state.next_id;
    state.next_id = id.checked_add(2).unwrap();
    let mut stream = Stream::new(true);
    stream.accepted = accepted;
    state.streams.insert(id, stream);
    id
}

fn taken(link: &Link, id: u32) -> LinkStream {
    LinkStream {
        link: Arc::clone(&link.shared),
        id,
        op: FleetOp::Desk,
    }
}

/// The peer's answer to the re-proof `nonce`, made pending on `link` first: what its reader
/// would hand the handler, signed with the peer's key over the session's own material.
fn reproof_answer(link: &Link, peer: &NodeIdentity, nonce: [u8; NONCE_LEN]) -> Vec<u8> {
    link.shared.link_state().reproof.pending = Some(Pending {
        nonce,
        overdue_at: HANG,
    });
    let material = link
        .shared
        .reproof_material(&nonce)
        .expect("session material");
    let mut answer = nonce.to_vec();
    answer.extend_from_slice(&peer.sign(link.shared.reprove_signer(false), &material));
    answer
}

/// Nothing moves on a link once it ended, whichever thread ended it (the reader goes on with its
/// buffer after a re-proof the membership log refused). Behind the end an `Accept` takes no
/// stream; `Data`, `End` and `Reset` leave an accepted stream's reads at the link's end, not at
/// the late bytes, the stream's end or a reset; a `Credit` grants nothing; an `Open` takes no
/// stream; a re-proof answer that verifies counts and notes nothing. CONTROL: the `Credit` and
/// the re-proof answer while the link is live.
#[test]
fn frames_handled_after_a_link_ended_move_none_of_its_streams() {
    let (a, b) = (node(0x4a), node(0x4b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    assert_eq!(mac_a.note().by, ProvenBy::Handshake);
    let shared = &link.shared;
    let credit = |id: u32| shared.link_state().streams[&id].credit;
    let handled = |kind: Kind, id: u32, payload: &[u8]| {
        shared
            .handle(kind, id, payload)
            .expect("a frame the link takes");
    };

    let granted = opened_here(&link, true);
    shared
        .link_state()
        .streams
        .get_mut(&granted)
        .unwrap()
        .credit = 0;
    handled(Kind::Credit, granted, &4u32.to_be_bytes());
    assert_eq!(credit(granted), 4, "CONTROL");
    handled(Kind::Proven, 0, &reproof_answer(&link, &b, [1; NONCE_LEN]));
    assert_eq!(link.reproofs().1, 1, "CONTROL");
    assert_eq!(mac_a.note().by, ProvenBy::Reproof, "CONTROL");

    let unanswered = opened_here(&link, false);
    let answered = opened_here(&link, true);
    let pending = reproof_answer(&link, &b, [2; NONCE_LEN]);
    let open = shared.link_state().peer_max + 2;
    shared.fail("the membership log took the neighbour's key out");
    handled(Kind::Accept, unanswered, &[]);
    handled(Kind::Data, answered, b"late");
    handled(Kind::End, answered, &[]);
    handled(Kind::Reset, answered, b"op");
    handled(Kind::Credit, granted, &4u32.to_be_bytes());
    handled(Kind::Open, open, b"units");
    handled(Kind::Proven, 0, &pending);

    let aborted = Link::answer(taken(&link, unanswered), HANG).unwrap_err();
    assert_eq!(aborted.kind(), io::ErrorKind::ConnectionAborted);
    assert!(
        aborted.to_string().contains("took the neighbour's key out"),
        "{aborted}"
    );
    let stream = taken(&link, answered);
    stream.set_read_timeout(Some(HANG));
    let read = stream.read_some(&mut [0; 8]);
    assert!(
        matches!(&read, Err(e) if e.kind() == io::ErrorKind::ConnectionAborted),
        "{read:?}"
    );
    assert_eq!(credit(granted), 4);
    // Set under the lock before any op thread starts, so no thread's end can hide it.
    assert_eq!(shared.link_state().peer_max, open - 2);
    assert_eq!(link.reproofs().1, 1);
    assert!(mac_a.notes.try_recv().is_err(), "no note behind the end");
}

/// A stream accepted while its link was live takes no write and no reset once the link ended: a
/// write is aborted with the end's reason though the stream still holds credit, a shutdown of its
/// read half is aborted, and a reset does nothing, so its reads give the answer that arrived
/// before the end and then the end. CONTROL: a write before the end is queued.
#[test]
fn a_stream_on_a_link_that_ended_takes_no_write_or_reset() {
    let (a, b) = (node(0x5a), node(0x5b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let id = {
        let mut state = link.shared.link_state();
        let id = state.next_id;
        state.next_id = id.checked_add(2).unwrap();
        state.streams.insert(id, Stream::new(true));
        id
    };
    link.shared
        .handle(Kind::Accept, id, &[])
        .expect("a frame the link takes");
    let stream = Link::answer(
        LinkStream {
            link: Arc::clone(&link.shared),
            id,
            op: FleetOp::Desk,
        },
        HANG,
    )
    .expect("an accepted stream");
    stream.set_write_timeout(Some(HANG));
    stream.set_read_timeout(Some(HANG));
    assert_eq!(stream.write_some(b"live").unwrap(), 4, "CONTROL");
    link.shared
        .handle(Kind::Data, id, b"answer")
        .expect("a frame the link takes");
    link.shared.fail("the neighbour hung up");
    let aborted = stream.write_some(b"late").unwrap_err();
    assert_eq!(aborted.kind(), io::ErrorKind::ConnectionAborted);
    assert!(aborted.to_string().contains("hung up"), "{aborted}");
    assert!(link.shared.link_state().data.is_empty(), "nothing queued");

    // Nor does it reset: the answer that arrived before the end is still read, then the end.
    let refused = stream.shutdown(Shutdown::Read).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionAborted);
    stream.reset("closed");
    let mut said = [0; 16];
    let n = stream
        .read_some(&mut said)
        .expect("what arrived before the end");
    assert_eq!(&said[..n], b"answer");
    let end = stream.read_some(&mut said).unwrap_err();
    assert_eq!(end.kind(), io::ErrorKind::ConnectionAborted, "{end}");
}

/// A peer's stream whose op thread could not start is refused `busy`, which its opener may try
/// again, not `unserved`: the spawner drops the op it was given, as `std::thread::Builder::spawn`
/// does. CONTROL: a stream whose op ran is answered by its op.
#[test]
fn a_peer_stream_no_op_thread_took_is_refused_busy() {
    let (a, b) = (node(0x6a), node(0x6b));
    let mac_a = Mac::start(a.clone(), &[&a, &b]);
    let mac_b = Mac::start(b.clone(), &[&a, &b]);
    let link = mac_a.dial(&mac_b);
    let held = mac_b.link_to(&mac_a);
    // A stream B opened, as A's reader hands it to the op that serves it.
    let incoming = || {
        let id = opened_here(&held, false);
        link.shared
            .link_state()
            .streams
            .insert(id, Stream::new(false));
        let incoming = Incoming {
            link: Arc::clone(&link.shared),
            id,
            op: FleetOp::Units,
            log: None,
            answered: false,
        };
        (id, incoming)
    };
    let refused = |id: u32| {
        let refused = Link::answer(taken(&held, id), HANG).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
        refused.to_string()
    };
    let serve: ServeFn = Arc::new(|incoming: Incoming| incoming.refuse("op"));
    let (unstarted, its) = incoming();
    serve_spawned(Arc::clone(&serve), its, drop);
    assert!(refused(unstarted).ends_with(": busy"));
    let (ran, its) = incoming();
    serve_spawned(serve, its, |op| op());
    assert!(refused(ran).ends_with(": op"), "CONTROL");
}
