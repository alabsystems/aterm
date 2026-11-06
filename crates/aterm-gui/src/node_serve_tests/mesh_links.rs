// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two node services on one loopback mesh (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B5):
//! every op the mesh carries goes over the one neighbour link between them, a drive is refused
//! under a content grant, and the link re-proves on the injected clock.

use std::sync::Mutex;
use std::sync::mpsc;

use aterm_net::fleet::heard::{ScriptedSight, Sight};
use aterm_net::fleet::link::{
    BusLog, LinkClock as _, ManualClock, Neighbours, Proven, ProvenBy, REPROVE_EVERY,
};

use super::*;

#[path = "mesh_heard.rs"]
mod heard;

/// The wall clock's reading when a node's link clock reads zero: a moment on day 20,833.
const T0: u64 = 1_800_000_000_000;

/// The wall clock a node service's sight reads: [`T0`] plus its link clock.
fn wall(clock: &Arc<ManualClock>) -> Arc<dyn Fn() -> u64 + Send + Sync> {
    let clock = Arc::clone(clock);
    Arc::new(move || T0 + u64::try_from(clock.now().as_millis()).expect("a test's clock"))
}

/// One node service with the mesh on: its address, the neighbour table it holds, the clock its
/// links read, the proofs its links noted, and the switch that ends it.
struct MeshNode {
    addr: SocketAddr,
    table: Arc<Neighbours>,
    clock: Arc<ManualClock>,
    notes: mpsc::Receiver<Proven>,
    stop: Arc<AtomicBool>,
    server: std::thread::JoinHandle<ExitCode>,
}

impl MeshNode {
    /// A windowless node service over `state`, serving `allow` to the members `members` seats,
    /// with the mesh on, its links on a manual clock and its `Units` streams on `units`. Its
    /// sight of its network reads [`wall`] and no route, so it books no network and no day.
    fn start(
        root: &Path,
        relay_dir: &Path,
        state: &Path,
        members: Arc<LocalLog>,
        units: atpkg::store::Layout,
    ) -> Self {
        Self::start_seeing(root, relay_dir, state, (members, units), |clock| {
            Arc::new(ScriptedSight {
                now: wall(clock),
                route: None,
                ends: None,
            })
        })
    }

    /// [`Self::start`]'s node service, its sight of its network the one `sight` builds on its
    /// link clock.
    fn start_seeing(
        root: &Path,
        relay_dir: &Path,
        state: &Path,
        (members, units): (Arc<LocalLog>, atpkg::store::Layout),
        sight: impl FnOnce(&Arc<ManualClock>) -> Arc<dyn Sight>,
    ) -> Self {
        let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind node job");
        let addr = prebound.local_addr().unwrap();
        let config_path = root.join(format!("node-{}.toml", addr.port()));
        crate::test_scratch::write_config(
            &config_path,
            format!("[fleet]\nlisten = \"{addr}\"\nallow = \"content\"\n"),
        );
        let clock = ManualClock::new();
        let sight = sight(&clock);
        let (noted, notes) = mpsc::channel();
        let noted = Mutex::new(noted);
        let (told, tables) = mpsc::channel();
        let told = Mutex::new(told);
        let stop = Arc::new(AtomicBool::new(false));
        let cfg = NodeServeConfig {
            state_dir: state.to_path_buf(),
            relay_dir: relay_dir.to_path_buf(),
            home_dir: None,
            config_path: Some(config_path),
            vault_paths: None,
            members: Some(members),
            alab_browse: None,
            prebound_listener: Some(prebound),
            tick_every: Duration::from_millis(40),
            wake_timeout: Duration::from_secs(1),
            lan_unavailable: || None,
            stop: Arc::clone(&stop),
            launcher: idle_launcher(),
            updates: None,
            mesh: NodeMesh {
                on: true,
                clock: Arc::clone(&clock) as Arc<dyn aterm_net::fleet::link::LinkClock>,
                sight,
                watch: Arc::new(move |proven: &Proven| {
                    let _ = lock(&noted).send(proven.clone());
                }),
                units: Arc::new(move || Some(units.clone())),
                on_neighbours: Some(Arc::new(move |table: &Arc<Neighbours>| {
                    let _ = lock(&told).send(Arc::clone(table));
                })),
                bus: None,
                ..NodeMesh::live()
            },
        };
        let server = std::thread::spawn(move || run_node_serve_with(cfg));
        let table = tables
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the node service never started its neighbour table");
        Self {
            addr,
            table,
            clock,
            notes,
            stop,
            server,
        }
    }

    /// The next proof this node's links noted.
    fn note(&self) -> Proven {
        self.notes
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("no proof was noted")
    }

    fn end(self) {
        self.stop.store(true, Ordering::Release);
        assert_eq!(self.server.join().expect("join node"), ExitCode::SUCCESS);
    }
}

/// A line `stream` answers, after `request` is written to it.
fn ask_line(stream: &aterm_net::fleet::link::LinkStream, request: &str) -> String {
    let mut stream = stream;
    stream.write_all(request.as_bytes()).expect("write request");
    read_line_raw(&mut stream, 8192)
        .expect("read answer")
        .expect("an answer line")
}

/// Everything `stream` answers once its request was written and its write side ended.
fn answered(stream: aterm_net::fleet::link::LinkStream, request: &[u8]) -> Vec<u8> {
    let mut stream = stream;
    stream.write_all(request).expect("write request");
    stream.finish().expect("end the request");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).expect("the answer, whole");
    answer
}

/// TWO NODE SERVICES, ONE LINK, EVERY OP (B5): Mac B dials Mac A's one listener once and every op
/// the mesh carries goes over that link. Survey is surveyed windowless, Desk reaches the
/// window's desk (none runs, so it says so), Units is answered from A's store, and MailCode
/// reaches its server. A mesh log's Bus stream reaches that log's broker, which hands B its
/// grants first (B7a); the fabric's `bus.log` is no mesh log and stays unserved, as Inventory
/// does until B7b; and a drive is refused under A's content grant. Each link re-proves once every five minutes on the injected
/// clock, and after thirty minutes the peer's latest proof is still younger than `FRESH`, so
/// it still counts toward `MEMBERS`.
#[test]
fn two_node_services_carry_every_op_over_one_link_and_reprove_it_on_the_clock() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (relay_a, relay_b) = (
        crate::test_scratch::short_dir("node-mesh-a"),
        crate::test_scratch::short_dir("node-mesh-b"),
    );
    let (state_a, state_b) = (root.path().join("a-state"), root.path().join("b-state"));
    let (_, id_a) = seed_fleet_node(&state_a);
    let (_, id_b) = seed_fleet_node(&state_b);
    let out = seed_id(&root.path().join("out-state"));
    let founders = [(1, &id_a), (2, &id_b), (3, &out)];
    let log_a = lab_log(&root.path().join("records-a"), &founders, 3);
    let log_b = lab_log(&root.path().join("records-b"), &founders, 3);
    let store = |name: &str| atpkg::store::Layout {
        prefix: root.path().join(name),
    };
    let mac_a = MeshNode::start(root.path(), relay_a.path(), &state_a, log_a, store("a-pkg"));
    let mac_b = MeshNode::start(root.path(), relay_b.path(), &state_b, log_b, store("b-pkg"));

    let link = mac_b
        .table
        .dial(mac_a.addr, DIAL_WITHIN)
        .expect("B dials A's one listener");
    assert_eq!(link.peer().node, id_a.node());
    let heard = mac_b.note();
    assert_eq!(
        (heard.by, heard.peer.key),
        (ProvenBy::Handshake, id_a.key())
    );
    let seen = mac_a.note();
    assert_eq!((seen.by, seen.peer.key), (ProvenBy::Handshake, id_b.key()));

    // Survey, windowless, on the link.
    let survey = link
        .open(FleetOp::Survey, None, DIAL_WITHIN)
        .expect("a survey stream");
    let version = ask_line(&survey, "version\n");
    assert!(version.starts_with("OK version="), "{version}");
    drop(survey);

    // A mesh log's Bus stream is A's broker for it (B7a), which hands B its grants first.
    let (_, grants) =
        aterm_mesh::relay::BusDial::new(Arc::clone(&link), aterm_mesh::MeshLog::Tables)
            .open()
            .expect("A serves B its tables.log");
    assert!(!grants.is_empty(), "A hands B its grants");
    // The fabric's bus.log is no mesh log: framed, and refused as unserved.
    let refused = link
        .open(FleetOp::Bus, Some(BusLog::Bus), DIAL_WITHIN)
        .expect_err("bus.log is not served over the mesh");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(refused.to_string().contains("unserved"), "{refused}");

    // A content grant lets no member drive A.
    let refused = link
        .open(FleetOp::Drive, None, DIAL_WITHIN)
        .expect_err("a content grant refuses a drive");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(refused.to_string().contains("op"), "{refused}");

    // Desk: no window runs on A, so its desk says so.
    let desk = link
        .open(FleetOp::Desk, None, DIAL_WITHIN)
        .expect("a desk stream");
    let said = answered(desk, b"");
    assert!(
        String::from_utf8_lossy(&said)
            .starts_with("NO why=this%20Mac%27s%20window%20is%20not%20running"),
        "{}",
        String::from_utf8_lossy(&said)
    );

    // Units: A's store answers a unit it does not hold with a 404, as at port 7487.
    let units = link
        .open(FleetOp::Units, None, DIAL_WITHIN)
        .expect("a units stream");
    let head = answered(
        units,
        format!("GET /unit/{} HTTP/1.1\r\n\r\n", "0".repeat(64)).as_bytes(),
    );
    assert!(
        head.starts_with(b"HTTP/1.1 404"),
        "{}",
        String::from_utf8_lossy(&head)
    );

    // MailCode reaches its server, which answers and ends the stream.
    let mail = link
        .open(FleetOp::MailCode, None, DIAL_WITHIN)
        .expect("a mail-code stream");
    let _ = answered(mail, b"\n");

    // Every one of those went over the one link each Mac holds.
    assert_eq!(mac_b.table.links().len(), 1);
    assert_eq!(mac_a.table.links().len(), 1);
    assert!(Arc::ptr_eq(
        &mac_b.table.link_to(id_a.node()).expect("B's link to A"),
        &link
    ));

    // Five minutes on B's clock: B's timer re-proves A exactly once.
    mac_b.clock.advance(REPROVE_EVERY);
    let proven = mac_b.note();
    assert_eq!(
        (proven.by, proven.at, proven.peer.key),
        (ProvenBy::Reproof, REPROVE_EVERY, id_a.key())
    );
    mac_b.table.tick();
    assert_eq!(link.reproofs(), (1, 1));
    assert!(mac_b.notes.try_recv().is_err());

    // Thirty minutes on: a round each five minutes keeps A's latest proof younger than FRESH.
    let mut latest = proven;
    for round in 2..=6u32 {
        mac_b.clock.advance(REPROVE_EVERY);
        latest = mac_b.note();
        assert_eq!(
            (latest.by, latest.at),
            (ProvenBy::Reproof, REPROVE_EVERY * round)
        );
    }
    assert_eq!(mac_b.clock.now(), Duration::from_secs(30 * 60));
    assert!(mac_b.clock.now() - latest.at < aterm_secrets::home_lan::FRESH);
    assert!(link.closed().is_none(), "{:?}", link.closed());

    mac_b.end();
    mac_a.end();
}

/// ATPKG'S UNITS DIALER (B5): with the mesh on, a download takes a member's unit over a `Units`
/// op of the node service at the member's address, proved by node keys alone. A node that dials
/// no mesh, or an address the node job's cache holds no service at, gives atpkg no connection,
/// so it dials the member's port 7487 instead.
///
/// NEGATIVE CONTROL: `me` dialing no mesh (`dialing_mesh(None)`, the switch off) is given no
/// connection and dials nothing: a listener at the cached address it would dial takes no
/// connection (without `dial_units_at`'s `dials_mesh` guard it takes one, refused only after it).
#[test]
fn atpkg_takes_a_members_unit_over_its_node_services_units_op() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_a = crate::test_scratch::short_dir("node-units-a");
    let state_a = root.path().join("a-state");
    let (_, id_a) = seed_fleet_node(&state_a);
    let id_b = seed_id(&root.path().join("b-state"));
    let out = seed_id(&root.path().join("out-state"));
    let founders = [(1, &id_a), (2, &id_b), (3, &out)];
    let log_a = lab_log(&root.path().join("records-a"), &founders, 3);
    let log_b = lab_log(&root.path().join("records-b"), &founders, 3);
    // A's store holds one unit a build keeps (a second link), as `aterm pkg` lays one.
    let store = atpkg::store::Layout {
        prefix: root.path().join("a-pkg"),
    };
    let unit = b"a toolset archive, over the mesh".to_vec();
    let name = "ab".repeat(32);
    let units = store.prefix.join("units");
    std::fs::create_dir_all(&units).expect("units dir");
    std::fs::write(units.join(&name), &unit).expect("the unit");
    std::fs::hard_link(units.join(&name), root.path().join("a-build")).expect("a build's link");
    let mac_a = MeshNode::start(root.path(), relay_a.path(), &state_a, log_a, store);

    let addresses = [(id_a.node().to_owned(), mac_a.addr)];
    let here = mac_a.addr.ip();
    let unmeshed = id_b.clone().dialing_mesh(None);
    let watched = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a watched place");
    let watched_at = [(id_a.node().to_owned(), watched.local_addr().unwrap())];
    assert!(
        super::super::dial_units_at(&watched_at, &unmeshed, here).is_none(),
        "a node that dials no mesh must leave the unit to port 7487"
    );
    watched.set_nonblocking(true).unwrap();
    assert_eq!(
        watched.accept().map(|_| ()).map_err(|e| e.kind()),
        Err(std::io::ErrorKind::WouldBlock),
        "a node that dials no mesh must dial nothing"
    );
    let me = id_b.dialing_mesh(Some(Arc::new(move || {
        crate::fleet_listen::denied_under(Some(&[]), log_b.keys().as_deref())
    })));
    assert!(
        super::super::dial_units_at(&addresses, &me, "192.0.2.1".parse().unwrap()).is_none(),
        "an address no node service is cached at gives no connection"
    );
    let conn = super::super::dial_units_at(&addresses, &me, here)
        .expect("the node service at A's address answers a Units op");
    conn.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let request = format!("GET /unit/{name} HTTP/1.1\r\n\r\n");
    let mut sent = 0;
    while sent < request.len() {
        sent += conn.write_unit(&request.as_bytes()[sent..]).unwrap();
    }
    conn.flush_unit().unwrap();
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match conn.read_unit(&mut buf).expect("read the answer") {
            0 => break,
            n => answer.extend_from_slice(&buf[..n]),
        }
    }
    assert!(
        answer.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    assert!(answer.ends_with(&unit), "the unit's bytes, whole");
    mac_a.end();
}
