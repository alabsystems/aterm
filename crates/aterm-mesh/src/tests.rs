// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mesh logs and the relay (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7a): in-process
//! Macs, each with its two logs, its neighbour table and its overlay, linked over loopback `atm/1`
//! connections and relaying over their `Bus` streams, on one injected link clock and one injected
//! wall clock. Every wait here is the one-minute hang detector.

use crate::logs::{MESH_DIR, MeshConfig, Standings};
use crate::overlay::{self, Attached, Hints, Overlay, Sight};
use crate::relay::{self, BusDial, Event};
use crate::{Mesh, MeshLog};
use aterm_net::fleet::link::{Incoming, LinkClock, LinkConfig, ManualClock, Neighbours};
use aterm_net::fleet::{
    Denied, DenyFn, FleetEvent, FleetOp, FleetScope, FleetSecret, Listening, NodeIdentity, NodeKey,
    serve_offering_with_ask_within,
};
use aterm_secrets::age;
use aterm_secrets::commons::binding::Request;
use aterm_secrets::membership_log::{Body, Entrant, Evidence, Log, MemberId, Record};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The hang detector of every wait here: loopback traffic that must happen.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// 2026-11-05T00:00:00Z, where every test's wall clock starts.
const T0_MS: u64 = 1_793_836_800_000;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What every Mac of one test shares: the link clock, the wall clock, the address book the
/// overlays dial from (less the Macs a partition cut off), and the keys every link seats.
struct Lab {
    clock: Arc<ManualClock>,
    wall: Arc<AtomicU64>,
    book: Arc<Mutex<BTreeMap<String, SocketAddr>>>,
    cut: Arc<Mutex<BTreeSet<String>>>,
    seated: Arc<Mutex<Vec<NodeKey>>>,
}

impl Lab {
    fn new() -> Self {
        Self {
            clock: ManualClock::new(),
            wall: Arc::new(AtomicU64::new(T0_MS)),
            book: Arc::default(),
            cut: Arc::default(),
            seated: Arc::default(),
        }
    }

    /// Move the wall clock `by` milliseconds.
    fn age(&self, by: u64) {
        self.wall.fetch_add(by, Ordering::AcqRel);
    }

    /// The link clock past every back-off: each relay and each dial waiting on one tries again.
    fn tick(&self) {
        self.clock.advance(overlay::DIAL_MAX);
    }
}

/// A step a test runs on the reporting thread the first time an event it names is reported.
type Between = (Box<dyn Fn(&Event) -> bool + Send>, Box<dyn FnOnce() + Send>);

/// What one Mac reported, waited on, and how often its membership log was read.
#[derive(Default)]
struct Seen {
    events: Mutex<Vec<Event>>,
    moved: Condvar,
    between: Mutex<Option<Between>>,
    reads: AtomicUsize,
}

impl Seen {
    fn note(&self, event: &Event) {
        let step = {
            let mut between = lock(&self.between);
            if between.as_ref().is_some_and(|(when, _)| when(event)) {
                between.take().map(|(_, step)| step)
            } else {
                None
            }
        };
        if let Some(step) = step {
            step();
        }
        lock(&self.events).push(event.clone());
        self.moved.notify_all();
    }

    /// Run `step` on the reporting thread the first time an event `when` names is reported.
    fn between(
        &self,
        when: impl Fn(&Event) -> bool + Send + 'static,
        step: impl FnOnce() + Send + 'static,
    ) {
        *lock(&self.between) = Some((Box::new(when), Box::new(step)));
    }

    /// The membership log was read.
    fn read(&self) {
        let _events = lock(&self.events);
        self.reads.fetch_add(1, Ordering::AcqRel);
        self.moved.notify_all();
    }

    /// Wait until `done` holds of every event so far.
    fn until(&self, what: &str, done: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + HANG;
        let mut events = lock(&self.events);
        while !done(&events) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{what}: not within the hang detector; saw {events:#?}"
            );
            events = self
                .moved
                .wait_timeout(events, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn mark(&self) -> usize {
        lock(&self.events).len()
    }
}

/// Every `(peer, log)` a relay of this Mac followed, from event `since` on.
fn following(events: &[Event], since: usize) -> BTreeSet<(String, MeshLog)> {
    events
        .iter()
        .skip(since)
        .filter_map(|event| match event {
            Event::Following { peer, log } => Some((peer.clone(), *log)),
            _ => None,
        })
        .collect()
}

/// The records every relay of this Mac carried, appended or answered "already held".
fn carried(events: &[Event]) -> (usize, usize) {
    events
        .iter()
        .fold((0, 0), |(appended, deduped), event| match event {
            Event::Burst {
                appended: a,
                deduped: d,
                ..
            } => (appended + a, deduped + d),
            _ => (appended, deduped),
        })
}

/// One running node service's mesh: its logs, its neighbour table and listener, and its overlay.
struct Running {
    mesh: Arc<Mesh>,
    table: Arc<Neighbours>,
    attached: Option<Attached>,
    serving: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

/// One Mac: a state dir holding its node key, the standings its membership log reads, what it
/// reported, and its node service while it runs.
struct Mac {
    dir: aterm_tempfile::TempDir,
    seed: [u8; 32],
    key: [u8; 32],
    principal: String,
    standings: Arc<Mutex<Option<Standings>>>,
    /// What the membership watcher's change probe answers next.
    changed: Arc<AtomicBool>,
    seen: Arc<Seen>,
    run: Option<Running>,
}

impl Mac {
    /// A Mac whose node key derives from `tag`.
    fn new(lab: &Lab, tag: u8) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("am")
            .tempdir_in("/tmp")
            .expect("a scratch state dir");
        aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private state dir");
        let seed = [tag; 32];
        let mut text = aterm_codec::hex::encode_lower(&seed);
        text.push('\n');
        aterm_uds::private_file::write_private(
            dir.path(),
            aterm_net::fleet_member::NODE_KEY_FILE,
            text.as_bytes(),
        )
        .expect("the node key");
        aterm_net::fleet_member::node_identity(dir.path()).expect("the node identity");
        let key = astream_broker::origin::public_key(&seed);
        lock(&lab.seated).push(NodeKey::from_bytes(key));
        Self {
            dir,
            seed,
            key,
            principal: astream_broker::identity_principal(&key),
            standings: Arc::default(),
            changed: Arc::default(),
            seen: Arc::default(),
            run: None,
        }
    }

    fn mesh(&self) -> &Arc<Mesh> {
        &self.run.as_ref().expect("the Mac runs").mesh
    }

    /// Set the standings this Mac's membership log reads.
    fn read(&self, standings: Standings) {
        *lock(&self.standings) = Some(standings);
    }

    /// Read them now, as the watcher does after a change.
    fn follow(&self) {
        self.mesh().follow();
    }

    /// Start this Mac's node service: the mesh, the neighbour table serving `Bus` with it, the
    /// listener, and the overlay dialing from the lab's book.
    fn start(&mut self, lab: &Lab) {
        let standings = Arc::clone(&self.standings);
        let seen = Arc::clone(&self.seen);
        let reads = Arc::clone(&self.seen);
        let changed = Arc::clone(&self.changed);
        let wall = Arc::clone(&lab.wall);
        let mesh = Mesh::open(MeshConfig {
            state_dir: self.dir.path().to_path_buf(),
            backups: aterm_uds::backup::Backups::INERT,
            clock: Arc::clone(&lab.clock) as Arc<dyn LinkClock>,
            wall: Arc::new(move || {
                UNIX_EPOCH + Duration::from_millis(wall.load(Ordering::Acquire))
            }),
            standings: Arc::new(move || {
                let read = lock(&standings).clone();
                reads.read();
                read
            }),
            changed: Some(Arc::new(move || changed.swap(false, Ordering::AcqRel))),
            watch_every: HANG,
            observe: Some(Arc::new(move |event: &Event| seen.note(event))),
        })
        .expect("the mesh opens");
        assert_eq!(mesh.principal(), self.principal);
        let seated = Arc::clone(&lab.seated);
        let deny: DenyFn = Arc::new(move || {
            Arc::new(Denied {
                members: lock(&seated).clone(),
                ..Denied::default()
            })
        });
        let serving_mesh = Arc::clone(&mesh);
        let cfg = LinkConfig {
            verdict: Arc::clone(&deny),
            serve: Arc::new(move |incoming: Incoming| {
                if incoming.op() == FleetOp::Bus {
                    serving_mesh.serve_bus(incoming);
                } else {
                    incoming.refuse("op");
                }
            }),
            note: Arc::new(|_| {}),
            clock: Arc::clone(&lab.clock) as Arc<dyn LinkClock>,
        };
        let me = NodeIdentity::from_seed(&self.principal, &self.seed)
            .expect("a node identity")
            .dialing_mesh(Some(Arc::clone(&deny)));
        let table = Neighbours::new(me.clone(), cfg);
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let addr = listener.local_addr().expect("its address");
        let serving = Arc::new(AtomicBool::new(true));
        let thread = {
            let (serving, table, mesh) =
                (Arc::clone(&serving), Arc::clone(&table), Arc::clone(&mesh));
            std::thread::spawn(move || {
                const SERVES: &[FleetOp] = &[FleetOp::Neighbour];
                let listening = Listening {
                    secret: FleetSecret::new(&[7; FleetSecret::MIN_LEN]).expect("a fleet secret"),
                    scope: FleetScope::Content,
                    node: Some(me.clone()),
                    serves: SERVES,
                    slots: 16,
                    per_source: 16,
                    lifetime: Duration::from_secs(60 * 60),
                };
                serve_offering_with_ask_within(
                    &listener,
                    (&listening, Arc::new(|| (FleetScope::Content, SERVES)), deny),
                    None,
                    &serving,
                    HANG,
                    move |served, op| {
                        if op == FleetOp::Neighbour {
                            let link = served.into_link(&me, table.config())?;
                            table.adopt(link);
                            mesh.poke();
                        }
                        Ok(())
                    },
                    |_: FleetEvent| {},
                );
            })
        };
        lock(&lab.book).insert(self.principal.clone(), addr);
        let (book, cut) = (Arc::clone(&lab.book), Arc::clone(&lab.cut));
        let principal = self.principal.clone();
        let hints: Hints = Arc::new(move || {
            let cut = lock(&cut).clone();
            if cut.contains(&principal) {
                return Vec::new();
            }
            lock(&book)
                .iter()
                .filter(|(node, _)| !cut.contains(*node))
                .map(|(node, addr)| (node.clone(), *addr))
                .collect()
        });
        let attached = mesh.attach(&table, hints).expect("the overlay starts");
        self.run = Some(Running {
            mesh,
            table,
            attached: Some(attached),
            serving,
            listener: Some(thread),
        });
    }

    /// Stop this Mac's node service, as the node job does: the overlay and its relays are told to
    /// stop, the links close (which ends any relay still reading one), the relays are joined,
    /// then the listener ends and the logs close.
    fn stop(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        if let Some(attached) = &run.attached {
            attached.stop_soon();
        }
        run.table.stop();
        drop(run.attached.take());
        run.serving.store(false, Ordering::Release);
        if let Some(listener) = run.listener.take() {
            listener.join().expect("the listener ends");
        }
        run.mesh.close();
    }

    /// Cut this Mac off: it dials no Mac, no Mac dials it, and it closes every link it holds;
    /// each neighbour learns of the end from its own end of the link.
    fn cut(&self, lab: &Lab) {
        lock(&lab.cut).insert(self.principal.clone());
        for link in self.run.as_ref().expect("the Mac runs").table.links() {
            link.close("a partition");
        }
    }

    /// Heal the cut: the overlays dial again once their back-off passes.
    fn heal(&self, lab: &Lab) {
        lock(&lab.cut).remove(&self.principal);
        lab.tick();
    }

    /// Wait until this Mac's relays follow every log to each of `peers`, from event `since` on.
    fn until_following(&self, peers: &[&Mac], since: usize) {
        let want: BTreeSet<(String, MeshLog)> = peers
            .iter()
            .flat_map(|peer| MeshLog::ALL.map(|log| (peer.principal.clone(), log)))
            .collect();
        self.seen.until(
            &format!("{} follows its neighbours", self.principal),
            |events| following(events, since).is_superset(&want),
        );
    }

    /// Publish `payload` on this Mac's table `scope`.
    fn table(&self, scope: &str, payload: &str) -> relay::Published {
        self.mesh()
            .publish(
                MeshLog::Tables,
                &format!("/t/{scope}/{}", self.principal),
                payload.as_bytes(),
            )
            .expect("a table record")
    }

    /// Publish `payload` on this Mac's metrics subject `leaf` (under `shared/`).
    fn metric(&self, leaf: &str, payload: &str) -> relay::Published {
        self.mesh()
            .publish(
                MeshLog::Metrics,
                &format!("/m/{}/shared/{leaf}", self.principal),
                payload.as_bytes(),
            )
            .expect("a metrics row")
    }

    /// Every `(subject, payload)` `log` holds under `filter`.
    fn held(&self, log: MeshLog, filter: &str) -> Vec<(String, String)> {
        let mut client = self
            .mesh()
            .read_client(log, filter, Some(HANG))
            .expect("a reader");
        let mut out = Vec::new();
        let mut from = 0;
        loop {
            let (page, (next, head)) = client.fetch(from, filter, 1024).expect("a page");
            for (_, subject, body) in page {
                out.push((subject, payload_of(&body)));
            }
            if next >= head {
                return out;
            }
            from = next;
        }
    }

    /// Wait until `log` holds every payload of `want` under `filter`.
    fn until_held(&self, log: MeshLog, filter: &str, want: &[&str]) {
        let client = self
            .mesh()
            .read_client(log, filter, Some(HANG))
            .expect("a reader");
        let mut sub = client.subscribe(0, filter).expect("a tail");
        let mut seen = BTreeSet::new();
        // An idle tail is marked every 10 s, so the read timeout alone never fires.
        let deadline = Instant::now() + HANG;
        while !want.iter().all(|w| seen.contains(*w)) {
            assert!(
                Instant::now() < deadline,
                "{} did not hold {want:?} within the hang detector; held {seen:?}",
                self.principal
            );
            match sub.recv_event() {
                Ok(Some(astream_broker::Event::Delivery { body, .. })) => {
                    seen.insert(payload_of(&body));
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("{}'s {} ended", self.principal, log.file()),
                Err(e) => panic!(
                    "{} did not hold {want:?} within the hang detector ({e}); held {seen:?}",
                    self.principal
                ),
            }
        }
    }
}

impl Drop for Mac {
    fn drop(&mut self) {
        self.stop();
    }
}

fn payload_of(body: &[u8]) -> String {
    String::from_utf8_lossy(astream_broker::origin::payload(body).unwrap_or(body)).into_owned()
}

/// Standings that seat every Mac of `macs`.
fn seating(macs: &[&Mac]) -> Standings {
    Standings {
        current: macs.iter().map(|mac| mac.key).collect(),
        ..Standings::default()
    }
}

/// `count` Macs, each reading standings that seat them all, started, every relay following.
fn lab_of(lab: &Lab, count: u8) -> Vec<Mac> {
    let mut macs: Vec<Mac> = (1..=count).map(|tag| Mac::new(lab, tag)).collect();
    let all = seating(&macs.iter().collect::<Vec<_>>());
    for mac in &mut macs {
        mac.read(all.clone());
        mac.start(lab);
    }
    for mac in &macs {
        let peers: Vec<&Mac> = macs.iter().filter(|m| m.key != mac.key).collect();
        mac.until_following(&peers, 0);
    }
    macs
}

/// B1's node id with the mesh on is astream's identity principal of the node key: the principal
/// the mesh names a Mac by, its subjects' owner segment, and its metrics node.
#[test]
fn b1s_node_id_is_the_identity_principal_of_the_node_key() {
    let lab = Lab::new();
    let mut mac = Mac::new(&lab, 9);
    let identity = aterm_net::fleet_member::read_node_identity(mac.dir.path())
        .expect("readable")
        .expect("minted");
    assert_eq!(*identity.key().bytes(), mac.key, "one seed, one key");
    let principal = astream_broker::identity_principal(&mac.key);
    assert_eq!(
        aterm_net::fleet_member::node_principal(identity.key()),
        principal
    );
    assert_eq!(
        aterm_metrics::subject::Node::of_key(&mac.key).to_string(),
        principal
    );
    assert!(aterm_net::fleet::is_node_id(&principal));
    mac.read(seating(&[&mac]));
    mac.start(&lab);
    assert_eq!(mac.mesh().principal(), principal);
}

/// Each log opens in a private folder of its own, sealed under its own `rest.key` and guarded by
/// its own secret, and is served on a socket there; a client with no grant reads nothing, and a
/// reopen keeps the key, the seal and the guard.
#[test]
fn each_log_opens_sealed_guarded_and_served_in_a_folder_of_its_own() {
    let lab = Lab::new();
    let mut mac = Mac::new(&lab, 9);
    mac.read(seating(&[&mac]));
    mac.start(&lab);
    let row = mac.metric("1m/cpu", "one");
    let mesh_dir = mac.dir.path().join(MESH_DIR);
    let mut guards = Vec::new();
    for log in MeshLog::ALL {
        let dir = mesh_dir.join(log.word());
        let file = dir.join(log.file());
        assert!(
            aterm_link::at_rest::is_sealed(&file),
            "{} is sealed",
            log.file()
        );
        for name in [aterm_link::at_rest::REST_KEY, crate::logs::GUARD_SECRET] {
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(dir.join(name)).expect(name).permissions(),
            );
            assert_eq!(mode & 0o777, 0o600, "{name} is owner-only");
        }
        guards.push(std::fs::read(dir.join(crate::logs::GUARD_SECRET)).expect("the guard"));
        let mut bare = astream_broker::Client::connect(dir.join(crate::logs::SOCKET))
            .expect("the log is served on its socket");
        assert!(
            bare.fetch(0, "/>", 16).is_err(),
            "{}: a client with no grant reads nothing",
            log.file()
        );
    }
    assert_ne!(guards[0], guards[1], "each log has a guard of its own");
    mac.stop();
    mac.start(&lab);
    for (log, guard) in MeshLog::ALL.into_iter().zip(&guards) {
        let dir = mesh_dir.join(log.word());
        assert_eq!(
            &std::fs::read(dir.join(crate::logs::GUARD_SECRET)).expect("the guard"),
            guard,
            "a reopen keeps the guard"
        );
    }
    assert_eq!(
        mac.held(MeshLog::Metrics, "/m/>"),
        vec![(
            format!("/m/{}/shared/1m/cpu", mac.principal),
            "one".to_owned()
        )],
        "the sealed log reopens with its row (offset {})",
        row.offset
    );
}

/// Three Macs on a cycle: each record is appended once on every Mac, from whichever neighbour
/// delivers it first, and the cycle does not grow it. Every relay carries each record once, so
/// each Mac carries each record twice (once to each neighbour), six carriages in all, of which
/// two append it.
///
/// NEGATIVE CONTROL: the relays run with `relay: false` (a plain mirror, which publishes under the
/// bridge's own producer id and the source offset): each Mac appends each record once per path,
/// the counts exceed one, and the carried total keeps growing past six per record.
#[test]
fn each_record_is_appended_once_per_mac_and_a_cycle_does_not_grow() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    let mut want = Vec::new();
    for n in 0..20 {
        for (mac, name) in [(a, "a"), (c, "c")] {
            let payload = format!("{name}-{n}");
            mac.table("0a", &payload);
            want.push(payload);
        }
    }
    for n in 0..10 {
        let payload = format!("b-row-{n}");
        b.metric("1m/load", &payload);
        want.push(payload);
    }
    let records = want.len();
    let refs: Vec<&str> = want.iter().map(String::as_str).collect();
    for mac in &macs {
        let tables: Vec<&str> = refs
            .iter()
            .copied()
            .filter(|p| !p.starts_with("b-"))
            .collect();
        let rows: Vec<&str> = refs
            .iter()
            .copied()
            .filter(|p| p.starts_with("b-"))
            .collect();
        mac.until_held(MeshLog::Tables, "/t/>", &tables);
        mac.until_held(MeshLog::Metrics, "/m/*/shared/>", &rows);
    }
    for mac in &macs {
        mac.seen.until(
            "its relays carried every record to both neighbours",
            |events| {
                let (appended, deduped) = carried(events);
                appended + deduped >= 2 * records
            },
        );
    }
    let (appended, deduped) = macs
        .iter()
        .map(|mac| carried(&lock(&mac.seen.events)))
        .fold((0, 0), |(a, d), (a2, d2)| (a + a2, d + d2));
    assert_eq!(
        appended + deduped,
        6 * records,
        "each relay carries each record once"
    );
    assert_eq!(
        appended,
        2 * records,
        "each record lands once on each other Mac"
    );
    for mac in &macs {
        let mut held: Vec<String> = mac
            .held(MeshLog::Tables, "/t/>")
            .into_iter()
            .chain(mac.held(MeshLog::Metrics, "/m/*/shared/>"))
            .map(|(_, payload)| payload)
            .collect();
        held.sort();
        let mut expected = want.clone();
        expected.sort();
        assert_eq!(
            held, expected,
            "{} holds each record exactly once",
            mac.principal
        );
    }
}

/// A partition and its heal within 7 days: the relay's cursor resumes, and what the cut-off Mac
/// missed arrives once and in order. A heal after more than 7 days drops the cursor: the relay
/// starts at its log's head, and what it would have carried is left to `Inventory`.
#[test]
fn a_cursor_resumes_after_a_partition_and_is_kept_7_days() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    a.table("0b", "before");
    b.until_held(MeshLog::Tables, "/t/>", &["before"]);

    let since = a.seen.mark();
    b.cut(&lab);
    a.seen.until("the relays to the cut-off Mac end", |events| {
        events
            .iter()
            .skip(since)
            .filter(|e| matches!(e, Event::Ended { .. }))
            .count()
            >= 2
    });
    for n in 0..5 {
        a.table("0b", &format!("cut-{n}"));
    }
    lab.age(DAY_MS);
    let since = a.seen.mark();
    b.heal(&lab);
    a.until_following(&[b], since);
    b.until_held(
        MeshLog::Tables,
        "/t/>",
        &["cut-0", "cut-1", "cut-2", "cut-3", "cut-4"],
    );
    let held: Vec<String> = b
        .held(MeshLog::Tables, "/t/>")
        .into_iter()
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(
        held,
        ["before", "cut-0", "cut-1", "cut-2", "cut-3", "cut-4"],
        "once each, in order"
    );

    let since = a.seen.mark();
    b.cut(&lab);
    a.seen.until("the relays to the cut-off Mac end", |events| {
        events
            .iter()
            .skip(since)
            .filter(|e| matches!(e, Event::Ended { .. }))
            .count()
            >= 2
    });
    a.table("0b", "stale");
    lab.age(8 * DAY_MS);
    let since = a.seen.mark();
    b.heal(&lab);
    a.until_following(&[b], since);
    a.table("0b", "after");
    b.until_held(MeshLog::Tables, "/t/>", &["after"]);
    assert!(
        !b.held(MeshLog::Tables, "/t/>")
            .iter()
            .any(|(_, payload)| payload == "stale"),
        "a cursor older than 7 days is dropped: the relay starts at the head"
    );
}

/// A relay with no cursor starts with the newest `now` and `quota` rows of its log, each with its
/// origin's key, then follows the head as it read it before them, so a row that lands while they
/// go follows them: an older value of the same subject and the history of a subject that keeps
/// every row stay behind.
#[test]
fn a_new_connection_starts_with_the_newest_rows() {
    let lab = Lab::new();
    let mut a = Mac::new(&lab, 1);
    let mut b = Mac::new(&lab, 2);
    a.read(seating(&[&a]));
    a.start(&lab);
    a.metric("now/presence", "v1");
    a.metric("now/presence", "v2");
    a.metric("quota/claude", "q1");
    a.metric("1m/cpu", "history");
    // A row that lands after the relay read its head, while the newest rows go, is relayed
    // after them.
    let mesh = Arc::downgrade(a.mesh());
    let subject = format!("/m/{}/shared/now/presence", a.principal);
    a.seen.between(
        |event| matches!(event, Event::NewestFirst { .. }),
        move || {
            let mesh = mesh.upgrade().expect("A runs");
            mesh.publish(MeshLog::Metrics, &subject, b"v-mid")
                .expect("a row while the newest go");
        },
    );
    let both = seating(&[&a, &b]);
    b.read(both.clone());
    b.start(&lab);
    a.read(both);
    a.follow();
    a.until_following(&[&b], 0);
    b.until_held(MeshLog::Metrics, "/m/*/shared/>", &["v2", "q1", "v-mid"]);
    a.seen.until("the newest rows went first", |events| {
        events.iter().any(|e| {
            matches!(
                e,
                Event::NewestFirst {
                    rows: 2,
                    log: MeshLog::Metrics,
                    ..
                }
            )
        })
    });
    a.metric("now/presence", "v3");
    b.until_held(MeshLog::Metrics, "/m/*/shared/>", &["v3"]);
    let held: Vec<String> = b
        .held(MeshLog::Metrics, "/m/*/shared/>")
        .into_iter()
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(
        held,
        ["v2", "q1", "v-mid", "v3"],
        "the newest values, then the head as it was read"
    );
}

/// A removed key's `Bus` stream is refused, at the node service and at the broker, and its live
/// stream ends at once; its rows on `metrics.log` are retired.
#[test]
fn a_removed_keys_stream_is_refused_and_its_live_stream_ends() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let tail = Tail::open(a, b);
    assert_eq!(rows_of(b, a), ["a-here"], "A's row is current before");

    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    b.follow();
    tail.refused(a, b);
    assert!(
        rows_of(b, a).is_empty(),
        "a removed Mac's rows are retired from every current view"
    );
}

/// A retired key (one a member bound and since replaced) is revoked on both logs, its live
/// stream ends and a new one is refused, but its rows stay: only a removed Mac's are retired.
#[test]
fn a_retired_keys_stream_ends_and_its_rows_stay() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let tail = Tail::open(a, b);
    b.read(Standings {
        current: [b.key].into(),
        retired: [a.key].into(),
        ..Standings::default()
    });
    b.follow();
    tail.refused(a, b);
    assert_eq!(rows_of(b, a), ["a-here"], "a retired key's rows stay");
}

/// A's live stream of its own subjects on B, opened over the node link with the grants B handed,
/// once B holds A's presence row.
struct Tail {
    dial: BusDial,
    tail: astream_broker::AnySubscription,
    _closer: astream_broker::Closer,
}

impl Tail {
    fn open(a: &Mac, b: &Mac) -> Self {
        a.metric("now/presence", "a-here");
        b.until_held(MeshLog::Metrics, "/m/*/shared/>", &["a-here"]);
        let link = a
            .run
            .as_ref()
            .expect("runs")
            .table
            .links()
            .into_iter()
            .next()
            .expect("A holds a link to B");
        let dial = BusDial::new(link, MeshLog::Tables);
        let (stream, grants) = dial.open().expect("B serves A's Bus stream");
        let stream: Arc<dyn astream_broker::StreamLink> = Arc::new(stream);
        stream.set_read_timeout(Some(HANG)).expect("bounded");
        let (mut client, closer) = astream_broker::connect_stream(stream);
        client
            .attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))
            .expect("A's grants attach");
        let own = MeshLog::Tables.own_filter(&a.principal);
        let tail = client.subscribe(0, &own).expect("a live stream");
        Self {
            dial,
            tail,
            _closer: closer,
        }
    }

    /// B revoked A: the live stream ends (idle marks every 10 s do not keep it), B refuses a new
    /// one at the node service and at each broker.
    fn refused(mut self, a: &Mac, b: &Mac) {
        let deadline = Instant::now() + HANG;
        loop {
            assert!(
                Instant::now() < deadline,
                "the revoked key's live stream must end, not idle"
            );
            match self.tail.recv_event() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    assert_ne!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut,
                        "the revoked key's live stream must end, not idle"
                    );
                    break;
                }
            }
        }
        let refused = self
            .dial
            .open()
            .err()
            .expect("B refuses a revoked key's stream");
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{refused}"
        );
        assert!(refused.to_string().contains("out"), "{refused}");
        for log in MeshLog::ALL {
            let broker = b.mesh().broker(log);
            assert!(
                broker.is_identity_revoked(&a.key),
                "{} revokes A",
                log.file()
            );
            assert!(
                broker.is_principal_revoked(&a.principal),
                "{} revokes A's principal",
                log.file()
            );
            let (_ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a pair");
            let adopted = broker.serve_stream(Arc::new(theirs), a.key);
            assert_eq!(
                adopted.err().map(|e| e.kind()),
                Some(std::io::ErrorKind::PermissionDenied),
                "{}'s broker refuses the key itself",
                log.file()
            );
        }
    }
}

/// The current rows `of`'s metrics show on `mac`.
fn rows_of(mac: &Mac, of: &Mac) -> Vec<String> {
    let filter = format!("/m/{}/>", of.principal);
    mac.mesh()
        .read_client(MeshLog::Metrics, &filter, Some(HANG))
        .expect("a reader")
        .last_all(&filter)
        .expect("the rows")
        .0
        .into_iter()
        .map(|(_, _, body)| payload_of(&body))
        .collect()
}

/// A record whose signature does not verify, one from an origin the log does not seat, and one
/// whose payload was changed under its signature are each refused and counted; the genuine one
/// beside them lands.
///
/// NEGATIVE CONTROL: the receiving broker given no origin keys refuses every relay as not ready
/// (`ERR_RELAY_NOT_READY`), so the genuine record's assertion fails.
#[test]
fn a_forged_origin_is_refused() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let link = a.run.as_ref().expect("runs").table.links().remove(0);
    let (stream, grants) = BusDial::new(link, MeshLog::Tables)
        .open()
        .expect("B serves A's Bus stream");
    let stream: Arc<dyn astream_broker::StreamLink> = Arc::new(stream);
    stream.set_read_timeout(Some(HANG)).expect("bounded");
    let (mut client, _closer) = astream_broker::connect_stream(stream);
    client
        .attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))
        .expect("A's grants attach");
    let base = astream_broker::incarnation_base(9).expect("an incarnation");
    let pid = |principal: &str| astream_broker::producer_id_of(principal);
    let mine = format!("/t/0c/{}", a.principal);
    let stranger_seed = [77; 32];
    let stranger =
        astream_broker::identity_principal(&astream_broker::origin::public_key(&stranger_seed));
    let theirs = format!("/t/0c/{stranger}");
    let mut changed = astream_broker::origin::seal(&a.seed, &mine, base + 3, b"signed");
    let at = changed.len() - 1;
    changed[at] ^= 1;
    let forged = [
        (
            pid(&a.principal),
            base + 1,
            mine.clone(),
            astream_broker::origin::seal(&[55; 32], &mine, base + 1, b"wrong key"),
        ),
        (
            pid(&stranger),
            base + 2,
            theirs.clone(),
            astream_broker::origin::seal(&stranger_seed, &theirs, base + 2, b"unseated"),
        ),
        (pid(&a.principal), base + 3, mine.clone(), changed),
    ];
    let before = b.mesh().broker(MeshLog::Tables).stats().origin_refused;
    for (producer, seq, subject, body) in &forged {
        let refused = client
            .relay(*producer, *seq, subject, body)
            .expect_err("a forged record is refused");
        let refusal = astream_broker::broker_refusal(&refused).expect("a broker refusal");
        assert_eq!(
            refusal.code,
            astream_broker::proto::ERR_ORIGIN_REFUSED,
            "{refusal:?}"
        );
    }
    let genuine = astream_broker::origin::seal(&a.seed, &mine, base + 4, b"genuine");
    let (_, held) = client
        .relay(pid(&a.principal), base + 4, &mine, &genuine)
        .expect("the genuine record lands");
    assert!(!held, "it is new to B");
    assert_eq!(
        b.mesh().broker(MeshLog::Tables).stats().origin_refused - before,
        3,
        "each forgery is counted"
    );
    let held: Vec<String> = b
        .held(MeshLog::Tables, "/t/>")
        .into_iter()
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(held, ["genuine"]);
}

/// The incarnation `seq` was numbered in.
fn incarnation(seq: u64) -> u64 {
    astream_broker::incarnation_of(seq).expect("a numbered record")
}

/// What a restart numbered: the last record before, the incarnation the lease took at the
/// restart, and the first record after.
struct Restarted {
    old: u64,
    opened: u64,
    new: u64,
}

/// A Mac publishes, stops, loses its `seq` file's unflushed block to a power cut, and starts.
fn restart(reuse: bool) -> Restarted {
    let lab = Lab::new();
    let mut a = Mac::new(&lab, 1);
    a.read(seating(&[&a]));
    a.start(&lab);
    let old: Vec<u64> = (0..3)
        .map(|n| a.table("0d", &format!("old-{n}")).seq)
        .collect();
    a.stop();
    let lease = a.dir.path().join(MESH_DIR).join("tables").join("lease");
    std::fs::write(lease.join("seq"), "0").expect("a power cut lost the unflushed block");
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                relay::REUSED_INCARNATION.with(|on| on.set(reuse));
                a.start(&lab);
            })
            .join()
    })
    .expect("the Mac starts again");
    let opened = lock(&a.mesh().hosted(MeshLog::Tables).origin).incarnation();
    let new = a.table("0d", "new").seq;
    Restarted {
        old: old[2],
        opened,
        new,
    }
}

/// A restart takes the next incarnation, so its numbers lie above every number used before, even
/// when the unflushed `seq` file lost its last block: its first record is numbered in the new
/// incarnation as taken, not rolled past a number its own log already held (which is all that
/// would save a reused incarnation here, and nothing saves it where only a neighbour holds the
/// number).
#[test]
fn a_restart_keeps_sequence_numbers_above_the_old_ones() {
    let r = restart(false);
    assert!(
        r.opened > incarnation(r.old),
        "a start takes a new incarnation"
    );
    assert_eq!(
        incarnation(r.new),
        r.opened,
        "the first record is numbered in it"
    );
    assert!(r.new > r.old, "{} is above {}", r.new, r.old);
}

/// NEGATIVE CONTROL (`REUSED_INCARNATION`): a restart that keeps its incarnation numbers its
/// first record with a number its log holds; only the local roll moves it on.
#[test]
fn with_reused_incarnation_a_restart_reuses_numbers() {
    let r = restart(true);
    assert_eq!(
        r.opened,
        incarnation(r.old),
        "the restart kept its incarnation"
    );
    assert_ne!(
        incarnation(r.new),
        r.opened,
        "the first number was held already, so the publish rolled"
    );
}

/// A neighbour that stopped uncleanly and came back in another boot finds its logs at a new
/// generation, with lineage: each relay to it resumes its cursor, so every table record that
/// landed while it was away reaches it, none skipped.
#[test]
fn a_neighbour_back_from_a_reboot_gets_what_it_missed() {
    let lab = Lab::new();
    let mut macs = lab_of(&lab, 2);
    macs[0].table("10", "before");
    macs[1].until_held(MeshLog::Tables, "/t/>", &["before"]);
    let was = macs[1].mesh().broker(MeshLog::Tables).log_identity();
    for log in MeshLog::ALL {
        macs[1].mesh().broker(log).arm_unclean_stop();
    }
    let since = macs[0].seen.mark();
    macs[1].stop();
    let gone = macs[1].principal.clone();
    macs[0].seen.until("A's relays to B ended", |events| {
        MeshLog::ALL.iter().all(|log| {
            events.iter().skip(since).any(|e| {
                matches!(e, Event::Ended { peer, log: ended, .. } if *peer == gone && ended == log)
            })
        })
    });
    for n in 0..3 {
        macs[0].table("10", &format!("away-{n}"));
    }
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                crate::logs::BOOT.with(|boot| boot.set(Some([7; 16])));
                macs[1].start(&lab);
            })
            .join()
    })
    .expect("B starts again, in another boot");
    let now = macs[1].mesh().broker(MeshLog::Tables).log_identity();
    assert!(
        now.id == was.id && now.generation > was.generation,
        "the reboot moved {was} to {now}"
    );
    macs[1].until_held(
        MeshLog::Tables,
        "/t/>",
        &["before", "away-0", "away-1", "away-2"],
    );
}

/// A relay backing off is kicked when its neighbour's own relays come back (their `Bus` streams
/// arrive): it tries again one first step after its last try started, not when its doubled
/// back-off ends.
#[test]
fn a_kicked_relay_tries_again_after_the_first_step() {
    let lab = Lab::new();
    let mut a = Mac::new(&lab, 1);
    let mut b = Mac::new(&lab, 2);
    let both = seating(&[&a, &b]);
    a.read(both.clone());
    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    // B first, so A's overlay has its address when it first steps (B dials no Mac it does not
    // seat).
    b.start(&lab);
    a.start(&lab);
    let refused = |since: usize| {
        a.seen.until("B refuses A's relays", |events| {
            MeshLog::ALL.iter().all(|log| {
                events.iter().skip(since).any(|e| {
                    matches!(e, Event::Ended { peer, log: ended, why }
                        if *peer == b.principal && ended == log && why.contains("out"))
                })
            })
        });
    };
    // Each relay is refused at once and again one step on, so the next waits a doubled 2 s.
    refused(0);
    let since = a.seen.mark();
    lab.clock.advance(relay::BACKOFF_FIRST);
    refused(since);
    // B seats A again: its relays to A start, and their streams arriving at A kick A's.
    let (since_a, since_b) = (a.seen.mark(), b.seen.mark());
    b.read(both);
    b.follow();
    b.until_following(&[&a], since_b);
    lab.clock.advance(relay::BACKOFF_FIRST);
    a.until_following(&[&b], since_a);
}

/// A relay that ends right after a burst it cut at its cap resumes with nothing skipped: astream's
/// push bridge commits one past such a burst, and the relay sends that one record again first.
#[test]
fn a_relay_cut_after_a_full_burst_skips_nothing() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    a.table("0c", "before");
    b.until_held(MeshLog::Tables, "/t/>", &["before"]);
    let since = a.seen.mark();
    b.cut(&lab);
    a.seen.until("the relays to the cut-off Mac end", |events| {
        events
            .iter()
            .skip(since)
            .filter(|e| matches!(e, Event::Ended { .. }))
            .count()
            >= 2
    });
    let payloads: Vec<String> = (0..300).map(|n| format!("r-{n:03}")).collect();
    for payload in &payloads {
        a.table("0c", payload);
    }
    // The moment the first full burst is acked and committed, A is cut off.
    let table = Arc::downgrade(&a.run.as_ref().expect("A runs").table);
    let (cut, me) = (Arc::clone(&lab.cut), a.principal.clone());
    a.seen.between(
        |event| {
            matches!(event, Event::Burst { log: MeshLog::Tables, appended, deduped, .. }
                if appended + deduped == 256)
        },
        move || {
            lock(&cut).insert(me);
            for link in table.upgrade().map(|t| t.links()).unwrap_or_default() {
                link.abort("cut after a full burst");
            }
        },
    );
    let since = a.seen.mark();
    b.heal(&lab);
    let peer = b.principal.clone();
    a.seen.until("a full burst went, then the relay ended", |events| {
        events
            .iter()
            .skip(since)
            .skip_while(|e| !matches!(e, Event::Burst { log: MeshLog::Tables, appended, deduped, .. } if appended + deduped == 256))
            .any(|e| matches!(e, Event::Ended { peer: to, log: MeshLog::Tables, .. } if *to == peer))
    });
    a.heal(&lab);
    let want: Vec<&str> = payloads.iter().map(String::as_str).collect();
    b.until_held(MeshLog::Tables, "/t/>", &want);
    let held: Vec<String> = b
        .held(MeshLog::Tables, "/t/>")
        .into_iter()
        .map(|(_, payload)| payload)
        .collect();
    let mut expected = vec!["before".to_owned()];
    expected.extend(payloads);
    assert_eq!(held, expected, "once each, in order");
}

/// A removed Mac's rows whose retirement failed are retired at the watcher's next round.
#[test]
fn a_failed_retirement_is_tried_again() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    a.metric("now/presence", "a-here");
    b.until_held(MeshLog::Metrics, "/m/*/shared/>", &["a-here"]);
    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    crate::logs::RETIRE_REFUSED.with(|refused| refused.set(true));
    b.follow();
    crate::logs::RETIRE_REFUSED.with(|refused| refused.set(false));
    assert_eq!(rows_of(b, a), ["a-here"], "the retirement failed");
    b.mesh().watch_now();
    b.seen.until("B retired A's rows", |events| {
        events
            .iter()
            .any(|e| matches!(e, Event::Retired { principal } if *principal == a.principal))
    });
    assert!(rows_of(b, a).is_empty(), "retired at the next round");
}

/// A change the watcher saw stays due until the membership log reads: a read that failed is
/// tried again at the next round, with no new change to prompt it.
#[test]
fn a_change_whose_read_failed_is_read_again() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    *lock(&b.standings) = None;
    b.changed.store(true, Ordering::Release);
    let reads = b.seen.reads.load(Ordering::Acquire);
    b.mesh().watch_now();
    b.seen.until("B's watcher read its log and failed", |_| {
        b.seen.reads.load(Ordering::Acquire) > reads
    });
    let since = b.seen.mark();
    b.read(Standings {
        current: [b.key].into(),
        removed: [a.key].into(),
        ..Standings::default()
    });
    b.mesh().watch_now();
    b.seen
        .until("B took A out, so its relays to A end", |events| {
            events
                .iter()
                .skip(since)
                .any(|e| matches!(e, Event::Ended { peer, .. } if *peer == a.principal))
        });
    assert!(b.mesh().broker(MeshLog::Tables).is_identity_revoked(&a.key));
}

/// What a rebuilt Mac's first record was numbered, the high water its neighbour held, and
/// whether the neighbour holds the record.
struct Rebuilt {
    seq: u64,
    high: u64,
    held: bool,
}

/// A Mac publishes, is rebuilt with its node key kept (its mesh folder gone, its wall clock where
/// it was), and publishes again once its first neighbour told it its high water. Its neighbour's
/// cursor named the old log, so it starts again with the newest rows, not where it was.
fn rebuild(reuse: bool) -> Rebuilt {
    let lab = Lab::new();
    let mut macs = lab_of(&lab, 2);
    macs[1].metric("now/presence", "b-now");
    macs[1].metric("1m/cpu", "b-later");
    macs[0].until_held(MeshLog::Metrics, "/m/*/shared/>", &["b-now", "b-later"]);
    let old: Vec<u64> = (0..3)
        .map(|n| macs[0].table("0e", &format!("old-{n}")).seq)
        .collect();
    macs[1].until_held(MeshLog::Tables, "/t/>", &["old-0", "old-1", "old-2"]);
    macs[0].stop();
    std::fs::remove_dir_all(macs[0].dir.path().join(MESH_DIR)).expect("the rebuild");
    let since = macs[0].seen.mark();
    let started = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                relay::REUSED_INCARNATION.with(|on| on.set(reuse));
                macs[0].start(&lab);
            })
            .join()
    });
    started.expect("the rebuilt Mac starts");
    let (a, b) = (&macs[0], &macs[1]);
    a.seen.until("the rebuilt lease settles", |events| {
        events.iter().skip(since).any(|e| {
            matches!(
                e,
                Event::Settled {
                    log: MeshLog::Tables,
                    ..
                }
            )
        })
    });
    let high = lock(&a.seen.events)
        .iter()
        .skip(since)
        .find_map(|e| match e {
            Event::Settled {
                log: MeshLog::Tables,
                high,
                ..
            } => *high,
            _ => None,
        })
        .expect("B held A's records");
    assert_eq!(high, old[2], "B's high water of A is A's last record");
    a.until_following(&[b], since);
    a.until_held(MeshLog::Metrics, "/m/*/shared/>", &["b-now"]);
    let since = a.seen.mark();
    let seq = a.table("0e", "new").seq;
    a.seen.until("A's relay carried the new record", |events| {
        events.iter().skip(since).any(|e| {
            matches!(
                e,
                Event::Burst {
                    log: MeshLog::Tables,
                    ..
                }
            )
        })
    });
    let held = b
        .held(MeshLog::Tables, "/t/>")
        .iter()
        .any(|(_, payload)| payload == "new");
    // Settled, the lease is marked so no longer: with no neighbour left to answer, a restart
    // numbers at once.
    macs[1].stop();
    macs[0].stop();
    macs[0].start(&lab);
    macs[0].table("0e", "after a restart");
    Rebuilt { seq, high, held }
}

/// A rebuilt Mac with its node key kept takes a fresh lease, waits for its first neighbour's high
/// water, and numbers its next record above it, so the neighbour appends it.
#[test]
fn a_rebuild_keeps_sequence_numbers_above_the_old_ones() {
    let rebuilt = rebuild(false);
    assert!(
        rebuilt.seq > rebuilt.high,
        "{} above {}",
        rebuilt.seq,
        rebuilt.high
    );
    assert!(
        rebuilt.held,
        "the neighbour appends the rebuilt Mac's record"
    );
}

/// NEGATIVE CONTROL (`REUSED_INCARNATION`): a rebuilt Mac that settles at its neighbour's
/// incarnation instead of above it numbers its next record with a number the neighbour holds,
/// which answers it "already held": the record never lands.
#[test]
fn with_reused_incarnation_a_rebuilt_macs_record_is_lost() {
    let rebuilt = rebuild(true);
    assert!(
        rebuilt.seq <= rebuilt.high,
        "{} reuses {}",
        rebuilt.seq,
        rebuilt.high
    );
    assert!(
        !rebuilt.held,
        "the neighbour answers the reused number already held"
    );
}

/// A rebuilt Mac with no neighbour publishes nothing: its lease waits for a high water, across a
/// restart too.
#[test]
fn a_fresh_lease_publishes_nothing_until_a_neighbour_answers() {
    let lab = Lab::new();
    let mut a = Mac::new(&lab, 1);
    let b = Mac::new(&lab, 2);
    a.read(seating(&[&a, &b]));
    a.start(&lab);
    let waits = a
        .mesh()
        .publish(MeshLog::Tables, &format!("/t/0f/{}", a.principal), b"early")
        .expect_err("an unsettled lease numbers nothing");
    assert_eq!(waits.kind(), std::io::ErrorKind::WouldBlock, "{waits}");
    let elsewhere = a
        .mesh()
        .publish(
            MeshLog::Tables,
            &format!("/t/0f/{}", b.principal),
            b"theirs",
        )
        .expect_err("another Mac's subject");
    assert_eq!(
        elsewhere.kind(),
        std::io::ErrorKind::InvalidInput,
        "{elsewhere}"
    );
    a.stop();
    a.start(&lab);
    let still = a
        .mesh()
        .publish(MeshLog::Tables, &format!("/t/0f/{}", a.principal), b"early")
        .expect_err("a restart before any neighbour answered leaves the lease unsettled");
    assert_eq!(still.kind(), std::io::ErrorKind::WouldBlock, "{still}");
}

/// Membership fixtures: a Mac's device request by tag, as aterm-secrets' tests make them.
fn request(tag: u8) -> Request {
    Request::create(
        [2; 16],
        [3; 32],
        [tag; 16],
        [tag; 16],
        &age::Identity::from_secret_bytes([tag ^ 0x55; 32]),
        &[tag; 32],
    )
    .expect("a device request")
}

fn actor(tag: u8) -> MemberId {
    request(tag).binding().actor()
}

/// A lab founded on `seats` (each a device tag and its node key).
fn founded(seats: &[(u8, [u8; 32])]) -> (Log, Record) {
    let entrants = seats
        .iter()
        .map(|&(tag, node_key)| Entrant {
            request: request(tag),
            node_key,
            name: format!("mac-{tag}"),
            label: format!("m{tag}"),
        })
        .collect();
    let genesis = Record::found(&[99; 32], entrants, 1).expect("a genesis");
    (Log::new(genesis.clone()).expect("a log"), genesis)
}

/// `by` signs `body` onto `log`.
fn sign(log: &mut Log, by: u8, at: u64, body: Body) {
    let record = log
        .draft(&actor(by), at, body)
        .sign(&[by; 32], [by; 16])
        .expect("a signed record");
    log.insert(record).expect("the log takes it");
}

/// A Mac rebuilt with its node key kept is re-admitted as the `Remove` of its old seat and the
/// `Admit` of a new seat binding the same key (`Decision::Replace`): its key never reads removed,
/// so no broker revokes it and it keeps relaying.
///
/// NEGATIVE CONTROL: applying the `Remove` alone reads the key `Removed`, which the first
/// assertion on the standings shows is what the `Admit` beside it prevents.
#[test]
fn a_mac_readmitted_with_its_key_kept_keeps_relaying() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, c) = (&macs[0], &macs[2]);
    let (mut log, genesis) = founded(&[(1, macs[0].key), (2, macs[1].key), (3, macs[2].key)]);
    for mac in &macs {
        mac.read(Standings::of_log(&log));
        mac.follow();
    }
    let since: Vec<usize> = macs.iter().map(|mac| mac.seen.mark()).collect();
    sign(
        &mut log,
        1,
        8,
        Body::Remove {
            member: actor(3),
            cutoff: *genesis.id(),
        },
    );
    assert!(
        Standings::of_log(&log).removed.contains(&c.key),
        "the Remove alone holds the key out"
    );
    sign(
        &mut log,
        1,
        9,
        Body::Admit {
            entrant: Entrant {
                request: request(103),
                node_key: c.key,
                name: "mac-3".to_owned(),
                label: "m3".to_owned(),
            },
            evidence: Evidence::Manual,
        },
    );
    let replaced = Standings::of_log(&log);
    assert!(
        replaced.is_current(&c.key),
        "the new seat binds the kept key"
    );
    assert!(!replaced.revoked().contains(&c.key));
    for mac in &macs {
        mac.read(replaced.clone());
        mac.follow();
    }
    c.table("10", "after-the-move");
    for mac in &macs[..2] {
        mac.until_held(MeshLog::Tables, "/t/>", &["after-the-move"]);
        for log in MeshLog::ALL {
            assert!(!mac.mesh().broker(log).is_identity_revoked(&c.key));
        }
    }
    for (mac, since) in macs.iter().zip(since) {
        let ended = lock(&mac.seen.events)
            .iter()
            .skip(since)
            .filter(|e| matches!(e, Event::Ended { peer, .. } if *peer == c.principal))
            .count();
        assert_eq!(
            ended, 0,
            "{}'s relays to the moved Mac never ended",
            mac.principal
        );
    }
    assert!(a.mesh().standings().is_some_and(|s| s.is_current(&c.key)));
}

/// A rogue's mass removal, voided by a quorum's removal of the rogue, makes the Macs it removed
/// members again: their keys leave the revoked set at the next pass, every broker un-revokes them
/// with no restart, and they relay again.
///
/// NEGATIVE CONTROL: a pass that does not un-revoke a key that left the revoked set
/// (`unrevoke_identity`, which lifts its principal too) keeps the brokers refusing the Macs'
/// streams, and the wait for them to follow their neighbours hangs past its detector.
#[test]
fn a_voided_removal_unrevokes_the_key_and_the_mac_relays_again() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (b, c, d) = (&macs[0], &macs[1], &macs[2]);
    let rogue = astream_broker::origin::public_key(&[201; 32]);
    let fifth = astream_broker::origin::public_key(&[205; 32]);
    let (mut log, genesis) = founded(&[(1, rogue), (2, b.key), (3, c.key), (4, d.key), (5, fifth)]);
    for mac in &macs {
        mac.read(Standings::of_log(&log));
        mac.follow();
    }
    for who in [2, 3, 4, 5] {
        sign(
            &mut log,
            1,
            8,
            Body::Remove {
                member: actor(who),
                cutoff: *genesis.id(),
            },
        );
    }
    let removed = Standings::of_log(&log);
    assert!(removed.removed.contains(&b.key) && removed.removed.contains(&c.key));
    for mac in &macs {
        mac.read(removed.clone());
        mac.follow();
    }
    for mac in &macs {
        for other in &macs {
            if other.key != mac.key {
                assert!(
                    mac.mesh()
                        .broker(MeshLog::Tables)
                        .is_identity_revoked(&other.key)
                );
            }
        }
    }
    for who in [2, 3, 4] {
        sign(
            &mut log,
            who,
            9,
            Body::Remove {
                member: actor(1),
                cutoff: *genesis.id(),
            },
        );
    }
    let voided = Standings::of_log(&log);
    assert!(voided.is_current(&b.key) && voided.removed.contains(&rogue));
    let since: Vec<usize> = macs.iter().map(|mac| mac.seen.mark()).collect();
    for mac in &macs {
        mac.read(voided.clone());
        mac.follow();
    }
    // A Mac that followed first was refused by those that had not yet: every try that failed
    // started before this tick, so its back-off is over once it parks.
    lab.tick();
    for (mac, since) in macs.iter().zip(&since) {
        let peers: Vec<&Mac> = macs.iter().filter(|m| m.key != mac.key).collect();
        mac.until_following(&peers, *since);
        for log in MeshLog::ALL {
            for other in &peers {
                assert!(!mac.mesh().broker(log).is_identity_revoked(&other.key));
                assert!(
                    !mac.mesh()
                        .broker(log)
                        .is_principal_revoked(&other.principal)
                );
            }
        }
    }
    b.table("11", "back");
    c.until_held(MeshLog::Tables, "/t/>", &["back"]);
    d.until_held(MeshLog::Tables, "/t/>", &["back"]);
}

/// The overlay's rule, stepped by hand.
mod rule {
    use super::*;

    fn seats(n: usize) -> BTreeSet<String> {
        (0..n).map(|i| format!("n-{i:016x}")).collect()
    }

    /// Every seat has an address, and this Mac dialed each link it holds.
    fn sight<'a>(
        now: Duration,
        seats: &'a BTreeSet<String>,
        up: &'a BTreeSet<String>,
        dialing: &'a BTreeSet<String>,
    ) -> Sight<'a> {
        Sight {
            now,
            seats,
            up,
            dialed: up,
            dialing,
            reachable: seats,
        }
    }

    /// With fewer than 6 members every seat is a neighbour, dialed at once, never swapped.
    #[test]
    fn with_fewer_than_six_members_every_seat_is_a_neighbour() {
        let mut overlay = Overlay::new(1);
        let seats = seats(4);
        let none = BTreeSet::new();
        let plan = overlay.step(&sight(Duration::ZERO, &seats, &none, &none));
        assert_eq!(overlay.chosen(), &seats);
        assert_eq!(plan.dial.len(), 4);
        let later = overlay.step(&sight(overlay::SWAP_EVERY * 3, &seats, &seats, &none));
        assert_eq!(overlay.chosen(), &seats, "none is swapped");
        assert!(later.dial.is_empty() && later.drop.is_empty());
    }

    /// With more, it keeps k = 4, linked seats first; it swaps exactly one every 10 minutes and
    /// not before; it replaces a dead one at once.
    #[test]
    fn it_keeps_four_swaps_one_every_ten_minutes_and_replaces_a_dead_one_at_once() {
        let mut overlay = Overlay::new(7);
        let seats = seats(12);
        let linked: BTreeSet<String> = seats.iter().take(5).cloned().collect();
        let none = BTreeSet::new();
        let plan = overlay.step(&sight(Duration::ZERO, &seats, &linked, &none));
        let chosen = overlay.chosen().clone();
        assert_eq!(chosen.len(), overlay::K);
        assert!(chosen.is_subset(&linked), "linked seats first");
        assert!(plan.dial.is_empty(), "all four are up");

        let before = overlay::SWAP_EVERY - Duration::from_secs(1);
        overlay.step(&sight(before, &seats, &chosen, &none));
        assert_eq!(overlay.chosen(), &chosen, "nothing swaps before 10 minutes");

        let plan = overlay.step(&sight(overlay::SWAP_EVERY, &seats, &chosen, &none));
        let swapped = overlay.chosen().clone();
        assert_eq!(swapped.len(), overlay::K);
        assert_eq!(
            swapped.intersection(&chosen).count(),
            overlay::K - 1,
            "one swapped"
        );
        assert_eq!(plan.drop.len(), 1, "the swapped-out link may go");
        assert_eq!(plan.dial.len(), 1, "the new one is dialed");

        let up: BTreeSet<String> = swapped.iter().cloned().collect();
        overlay.step(&sight(overlay::SWAP_EVERY, &seats, &up, &none));
        let dead = up.iter().next().cloned().expect("a neighbour");
        let mut left = up.clone();
        left.remove(&dead);
        let plan = overlay.step(&sight(overlay::SWAP_EVERY, &seats, &left, &none));
        assert!(
            !overlay.chosen().contains(&dead),
            "a dead one is replaced at once"
        );
        assert_eq!(overlay.chosen().len(), overlay::K);
        assert_eq!(plan.dial.len(), 1);
    }

    /// A seat with no address and no link is never chosen while one can be dialed, and a chosen
    /// one that loses its address is replaced; with fewer than 6 members it stays a neighbour but
    /// is not dialed.
    #[test]
    fn a_seat_with_no_address_holds_no_slot() {
        let mut overlay = Overlay::new(5);
        let seats = seats(12);
        let none = BTreeSet::new();
        let silent: BTreeSet<String> = seats.iter().take(6).cloned().collect();
        let reachable: BTreeSet<String> = seats.difference(&silent).cloned().collect();
        let plan = overlay.step(&Sight {
            reachable: &reachable,
            ..sight(Duration::ZERO, &seats, &none, &none)
        });
        assert_eq!(overlay.chosen().len(), overlay::K);
        assert!(
            overlay.chosen().is_subset(&reachable),
            "no address, no slot"
        );
        assert_eq!(plan.dial.len(), overlay::K);

        let lost = overlay
            .chosen()
            .iter()
            .next()
            .cloned()
            .expect("a neighbour");
        let mut fewer = reachable.clone();
        fewer.remove(&lost);
        let plan = overlay.step(&Sight {
            reachable: &fewer,
            ..sight(Duration::from_secs(1), &seats, &none, &none)
        });
        assert!(!overlay.chosen().contains(&lost), "replaced at once");
        assert_eq!(overlay.chosen().len(), overlay::K);
        assert!(!plan.dial.contains(&lost));

        let mut few = Overlay::new(5);
        let four = self::seats(4);
        let three: BTreeSet<String> = four.iter().take(3).cloned().collect();
        let plan = few.step(&Sight {
            reachable: &three,
            ..sight(Duration::ZERO, &four, &none, &none)
        });
        assert_eq!(few.chosen(), &four, "every seat is a neighbour");
        assert_eq!(
            plan.dial.iter().cloned().collect::<BTreeSet<String>>(),
            three,
            "only the ones with an address are dialed"
        );
    }

    /// A swap brings in only a seat with an address; with none to bring in, the four stay and no
    /// link is let go.
    #[test]
    fn a_swap_takes_only_a_seat_with_an_address() {
        let seats = seats(12);
        let none = BTreeSet::new();
        let five: BTreeSet<String> = seats.iter().take(5).cloned().collect();
        let mut overlay = Overlay::new(11);
        overlay.step(&Sight {
            reachable: &five,
            ..sight(Duration::ZERO, &seats, &none, &none)
        });
        let chosen = overlay.chosen().clone();
        assert!(chosen.is_subset(&five));
        let plan = overlay.step(&Sight {
            reachable: &five,
            ..sight(overlay::SWAP_EVERY, &seats, &chosen, &none)
        });
        let into: Vec<String> = overlay.chosen().difference(&chosen).cloned().collect();
        assert_eq!(into.len(), 1, "one swapped");
        assert!(
            five.contains(&into[0]),
            "for the one seat left with an address"
        );
        assert_eq!(plan.drop.len(), 1);

        let four: BTreeSet<String> = seats.iter().take(4).cloned().collect();
        let mut full = Overlay::new(11);
        full.step(&Sight {
            reachable: &four,
            ..sight(Duration::ZERO, &seats, &none, &none)
        });
        assert_eq!(full.chosen(), &four);
        let plan = full.step(&Sight {
            reachable: &four,
            ..sight(overlay::SWAP_EVERY, &seats, &four, &none)
        });
        assert_eq!(full.chosen(), &four, "no seat to swap in: the four stay");
        assert!(plan.drop.is_empty() && plan.dial.is_empty());
    }

    /// A kicked relay tries again the first back-off step after its last try started, never
    /// sooner, however long its doubled back-off had grown; unkicked, it waits that back-off.
    #[test]
    fn a_kick_cuts_the_back_off_to_its_first_step_and_no_further() {
        let started = Duration::from_secs(100);
        let grown = relay::BACKOFF_FIRST * 8;
        assert_eq!(relay::retry_at(started, grown, false), started + grown);
        assert_eq!(
            relay::retry_at(started, grown, true),
            started + relay::BACKOFF_FIRST,
            "a kick waits the first step from the try's start"
        );
        assert_eq!(
            relay::retry_at(started, relay::BACKOFF_FIRST, true),
            started + relay::BACKOFF_FIRST,
            "never sooner"
        );
        assert_eq!(
            relay::next_backoff(relay::BACKOFF_FIRST, false),
            relay::BACKOFF_FIRST * 2
        );
        assert_eq!(
            relay::next_backoff(relay::BACKOFF_MAX, false),
            relay::BACKOFF_MAX
        );
        assert_eq!(relay::next_backoff(grown, true), relay::BACKOFF_FIRST);
    }

    /// A link this Mac dialed to a seat that is not a neighbour is let go, whether the overlay
    /// swapped it out or never chose it; one a peer dialed is kept.
    #[test]
    fn a_dialed_link_to_a_seat_not_chosen_is_let_go() {
        let mut overlay = Overlay::new(9);
        let seats = seats(12);
        let none = BTreeSet::new();
        let linked: BTreeSet<String> = seats.iter().take(6).cloned().collect();
        let plan = overlay.step(&sight(Duration::ZERO, &seats, &linked, &none));
        let chosen = overlay.chosen().clone();
        assert!(chosen.is_subset(&linked));
        let extra: BTreeSet<String> = linked.difference(&chosen).cloned().collect();
        assert_eq!(extra.len(), 2);
        assert_eq!(
            plan.drop.iter().cloned().collect::<BTreeSet<String>>(),
            extra,
            "the dialed links not chosen go"
        );

        let mut kept = Overlay::new(9);
        let plan = kept.step(&Sight {
            dialed: &none,
            ..sight(Duration::ZERO, &seats, &linked, &none)
        });
        assert!(plan.drop.is_empty(), "links peers dialed stay");
    }

    /// A seat whose dial failed waits its back-off, which doubles, before it is dialed again.
    #[test]
    fn a_failed_dial_waits_a_doubling_back_off() {
        let mut overlay = Overlay::new(3);
        let seats = seats(2);
        let none = BTreeSet::new();
        let plan = overlay.step(&sight(Duration::ZERO, &seats, &none, &none));
        assert_eq!(plan.dial.len(), 2);
        for node in &plan.dial {
            overlay.failed(node, Duration::ZERO);
        }
        let plan = overlay.step(&sight(Duration::from_millis(999), &seats, &none, &none));
        assert!(plan.dial.is_empty(), "within the first back-off");
        assert_eq!(overlay.next_wake(Duration::ZERO), Some(overlay::DIAL_FIRST));
        let plan = overlay.step(&sight(overlay::DIAL_FIRST, &seats, &none, &none));
        assert_eq!(plan.dial.len(), 2);
        for node in &plan.dial {
            overlay.failed(node, overlay::DIAL_FIRST);
        }
        assert_eq!(
            overlay.next_wake(overlay::DIAL_FIRST),
            Some(overlay::DIAL_FIRST * 3),
            "the back-off doubled"
        );
    }
}
