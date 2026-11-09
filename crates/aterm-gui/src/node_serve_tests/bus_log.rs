// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node service hosts the fabric's `bus.log` (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
//! §11 C2): with the mesh on, its keeper retires the broker job and serves `bus.log` on the
//! same socket under the same mint secret, whatever the generations do; a neighbour's `Bus`
//! stream for it is admitted as a driver is, then served the ring of §4.11. Every wait is the
//! one-minute hang detector.

use std::sync::Mutex;
use std::sync::mpsc;

use aterm_mesh::logs::{
    BUS_LOG, BUS_SOCKET, BusHost, BusHosting, BusKeeperConfig, BusSpec, Durability, RetiredJob,
};
use aterm_net::fleet::link::{ManualClock, Neighbours};

use super::*;

/// The fleet a test's root serves.
const FLEET: &str = "f1";

/// A fabric root with a mint secret, short enough for its socket.
struct Root {
    dir: aterm_tempfile::TempDir,
    secret: [u8; 32],
}

impl Root {
    fn new(for_what: &str, tag: u8) -> Self {
        let dir = crate::test_scratch::short_dir(for_what);
        aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private root");
        let secret = [tag; 32];
        aterm_uds::private_file::write_private(dir.path(), "mint.secret", &secret)
            .expect("the mint secret");
        Self { dir, secret }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The keeper of this root: the fabric on, for [`FLEET`] at the ordered tier, each retire of
    /// the broker job counted in `retired`.
    fn keeper(&self, retired: &Arc<AtomicUsize>) -> BusKeeperConfig {
        let retired = Arc::clone(retired);
        BusKeeperConfig {
            root: self.path().to_path_buf(),
            backups: aterm_uds::backup::Backups::INERT,
            hosting: Arc::new(|| BusHosting::On {
                fleet: FLEET.to_owned(),
                tier: Durability::Ordered,
            }),
            retire: Some(Arc::new(move || {
                retired.fetch_add(1, Ordering::AcqRel);
                Ok(RetiredJob::Absent)
            })),
            every: Duration::from_millis(40),
        }
    }

    /// A local client of the `bus.log` served here, holding `grant` minted under the secret;
    /// `None` while nothing answers.
    fn local(&self, grant: &str) -> Option<astream_broker::Client<std::os::unix::net::UnixStream>> {
        let cap = astream_broker::mint(&self.secret, grant).expect("a grant");
        let mut client = astream_broker::Client::connect(self.path().join(BUS_SOCKET)).ok()?;
        client.attach(&cap.filter, &cap.tag).ok()?;
        Some(client)
    }

    /// Wait until the `bus.log` served here answers a local client.
    fn until_served(&self) -> astream_broker::Client<std::os::unix::net::UnixStream> {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if let Some(client) = self.local(&format!("rw:/f/{FLEET}/>")) {
                return client;
            }
            assert!(
                Instant::now() < deadline,
                "bus.log is served within the hang detector"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Every payload the log here holds under `filter`.
    fn held(&self, filter: &str) -> Vec<String> {
        let mut client = self.local(&format!("ro:{filter}")).expect("a reader");
        let (page, _) = client.fetch(0, filter, 1024).expect("a page");
        page.into_iter()
            .map(|(_, _, body)| String::from_utf8_lossy(&body).into_owned())
            .collect()
    }
}

/// A node job's configuration over `state` with the mesh on, keeping `bus`.
fn with_bus(state: &Path, bus: Option<BusKeeperConfig>) -> NodeServeConfig {
    NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: state.join("relay"),
        home_dir: None,
        config_path: None,
        vault_paths: None,
        members: None,
        alab_browse: None,
        prebound_listener: None,
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::new(Stop::new()),
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh {
            on: true,
            clock: ManualClock::new() as Arc<dyn aterm_net::fleet::link::LinkClock>,
            watch: Arc::new(|_| {}),
            units: Arc::new(|| None),
            on_neighbours: None,
            bus,
            ..NodeMesh::live()
        },
    }
}

/// THE NODE SERVICE HOSTS BUS.LOG: with the mesh on, its keeper retires the broker job, then
/// opens `bus.log` in the fabric root under the root's mint secret and serves it on `bus.sock`,
/// though this Mac holds no fleet key and no generation runs: a local client's cap minted under
/// the secret posts and reads as it did against the job's broker. As the job ends it closes the
/// log cleanly, so the log opens again at once, its records kept.
#[test]
fn the_node_service_hosts_bus_log_with_the_mesh_on() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let root = Root::new("bus-node", 3);
    let retired = Arc::new(AtomicUsize::new(0));
    let cfg = with_bus(&scratch.path().join("state"), Some(root.keeper(&retired)));
    let stop = Arc::clone(&cfg.stop);
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

    let mut client = root.until_served();
    assert!(
        retired.load(Ordering::Acquire) >= 1,
        "the broker job is retired before the open"
    );
    client
        .publish(
            7,
            1,
            &format!("/f/{FLEET}/in/n-0000000000000001/s-1/n-0000000000000002/note"),
            b"hi",
        )
        .expect("a local post lands");
    drop(client);
    assert_eq!(root.held(&format!("/f/{FLEET}/in/>")), ["hi"]);

    stop.set();
    assert_eq!(server.join().expect("join node"), ExitCode::SUCCESS);
    let reopened = BusHost::open(BusSpec {
        root: root.path().to_path_buf(),
        fleet: FLEET.to_owned(),
        tier: Durability::Ordered,
        backups: aterm_uds::backup::Backups::INERT,
    })
    .expect("the job closed bus.log as it ended");
    assert!(root.path().join(BUS_LOG).exists());
    drop(reopened);
}

/// With the mesh off the job keeps no `bus.log` (the broker job keeps it, as before), whatever
/// its configuration names; with it on it keeps one.
#[test]
fn with_the_mesh_off_the_node_service_keeps_no_bus_log() {
    let root = Root::new("bus-off", 4);
    let retired = Arc::new(AtomicUsize::new(0));
    let off = NodeMesh {
        on: false,
        bus: Some(root.keeper(&retired)),
        ..NodeMesh::off()
    };
    assert!(off.keep_bus().is_none(), "the mesh off keeps no bus.log");
    let on = NodeMesh {
        on: true,
        bus: Some(root.keeper(&retired)),
        ..NodeMesh::off()
    };
    let keeper = on.keep_bus().expect("the mesh on keeps bus.log");
    drop(keeper);
}

/// One node service hosting `bus.log`, with the mesh on: its address, the neighbour table it
/// holds, and the switch that ends it.
struct BusNode {
    addr: SocketAddr,
    table: Arc<Neighbours>,
    stop: Arc<Stop>,
    server: std::thread::JoinHandle<ExitCode>,
}

impl BusNode {
    /// A windowless node service over `state`, under the `[fleet]` table `fleet`, seating the
    /// members `members` seats, keeping `bus`.
    fn start(
        scratch: &Path,
        state: &Path,
        fleet: &str,
        members: Arc<LocalLog>,
        bus: Option<BusKeeperConfig>,
    ) -> Self {
        let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind node job");
        let addr = prebound.local_addr().unwrap();
        let config_path = scratch.join(format!("node-{}.toml", addr.port()));
        crate::test_scratch::write_config(
            &config_path,
            format!("[fleet]\nlisten = \"{addr}\"\n{fleet}"),
        );
        let (told, tables) = mpsc::channel();
        let told = Mutex::new(told);
        let stop = Arc::new(Stop::new());
        let relay = crate::test_scratch::short_dir("bus-relay");
        let cfg = NodeServeConfig {
            relay_dir: relay.path().to_path_buf(),
            config_path: Some(config_path),
            members: Some(members),
            prebound_listener: Some(prebound),
            on_generation: None,
            stop: Arc::clone(&stop),
            mesh: NodeMesh {
                on: true,
                clock: ManualClock::new() as Arc<dyn aterm_net::fleet::link::LinkClock>,
                watch: Arc::new(|_| {}),
                units: Arc::new(|| None),
                on_neighbours: Some(Arc::new(move |table: &Arc<Neighbours>| {
                    let _ = told
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .send(Arc::clone(table));
                })),
                bus,
                ..NodeMesh::live()
            },
            ..with_bus(state, None)
        };
        let server = std::thread::spawn(move || {
            let _relay = relay;
            run_node_serve_with(cfg)
        });
        let table = tables
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the node service never started its neighbour table");
        Self {
            addr,
            table,
            stop,
            server,
        }
    }

    fn end(self) {
        self.stop.set();
        assert_eq!(self.server.join().expect("join node"), ExitCode::SUCCESS);
    }
}

/// A's `bus.log` stream for B, over the link B dialed: the client, and the ring A handed.
fn bus_stream(
    link: &aterm_net::fleet::link::Link,
) -> std::io::Result<(
    astream_broker::AnyClient,
    astream_broker::Closer,
    Vec<astream_broker::Capability>,
)> {
    let (stream, grants) = aterm_mesh::logs::dial_bus(link)?;
    let stream: Arc<dyn astream_broker::StreamLink> = Arc::new(stream);
    stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
    let (mut client, closer) = astream_broker::connect_stream(stream);
    client.attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
    Ok((client, closer, grants))
}

/// A MEMBER'S BUS.LOG STREAM IS ADMITTED AS A DRIVER, THEN SERVED ITS RING (§4.11): Mac A hosts
/// `bus.log` and lets members drive it; B's `Bus` stream for it is admitted, handed the ring
/// minted for B, and B posts mail as itself, which lands on A's `bus.log` where A's local
/// clients read it; B's post as A is refused. Under a content grant (no member may drive A),
/// the same stream is refused as an ungranted drive is.
#[test]
fn a_members_bus_log_stream_is_admitted_as_a_driver_and_served_its_ring() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let (state_a, state_b) = (
        scratch.path().join("a-state"),
        scratch.path().join("b-state"),
    );
    let (_, id_a) = seed_fleet_node(&state_a);
    let (_, id_b) = seed_fleet_node(&state_b);
    let out = seed_id(&scratch.path().join("out-state"));
    let founders = [(1, &id_a), (2, &id_b), (3, &out)];
    let log = |name: &str| lab_log(&scratch.path().join(name), &founders, 3);
    let root_a = Root::new("bus-a", 5);
    let retired = Arc::new(AtomicUsize::new(0));
    let mac_a = BusNode::start(
        scratch.path(),
        &state_a,
        "allow = \"drive\"\ndrive_from = [\"*\"]\n",
        log("records-a"),
        Some(root_a.keeper(&retired)),
    );
    let mac_b = BusNode::start(
        scratch.path(),
        &state_b,
        "allow = \"content\"\n",
        log("records-b"),
        None,
    );
    drop(root_a.until_served());
    let link = mac_b
        .table
        .dial(mac_a.addr, DIAL_WITHIN)
        .expect("B dials A's one listener");

    let principal_a = astream_broker::identity_principal(id_a.key().bytes());
    let principal_b = astream_broker::identity_principal(id_b.key().bytes());
    let (mut client, _closer, grants) = bus_stream(&link).expect("A serves B its bus.log");
    assert_eq!(
        grants
            .iter()
            .map(|cap| cap.filter.clone())
            .collect::<Vec<_>>(),
        aterm_mesh::logs::ring(FLEET, &principal_a, &principal_b, &Default::default()),
        "A hands B the ring of §4.11, minted for B (a capability's `filter` is its whole grant)"
    );
    let own = format!("/f/{FLEET}/in/{principal_a}/s-1/{principal_b}/note");
    client
        .publish(
            astream_broker::producer_id_of(&principal_b),
            1,
            &own,
            b"from b",
        )
        .expect("B posts as itself");
    let forged = format!("/f/{FLEET}/in/{principal_a}/s-1/{principal_a}/note");
    assert!(
        client
            .publish(
                astream_broker::producer_id_of(&principal_a),
                2,
                &forged,
                b"as a"
            )
            .is_err(),
        "B cannot post as A"
    );
    assert_eq!(root_a.held(&format!("/f/{FLEET}/in/>")), ["from b"]);
    mac_b.end();
    mac_a.end();

    // The same stream, to a Mac whose content grant lets no member drive it.
    let root_c = Root::new("bus-c", 6);
    let state_c = scratch.path().join("c-state");
    let (_, id_c) = seed_fleet_node(&state_c);
    let (_, id_d) = seed_fleet_node(&scratch.path().join("d-state"));
    let founders = [(1, &id_c), (2, &id_d), (3, &out)];
    let log = |name: &str| lab_log(&scratch.path().join(name), &founders, 3);
    let mac_c = BusNode::start(
        scratch.path(),
        &state_c,
        "allow = \"content\"\n",
        log("records-c"),
        Some(root_c.keeper(&retired)),
    );
    let mac_d = BusNode::start(
        scratch.path(),
        &scratch.path().join("d-state"),
        "allow = \"content\"\n",
        log("records-d"),
        None,
    );
    drop(root_c.until_served());
    let link = mac_d
        .table
        .dial(mac_c.addr, DIAL_WITHIN)
        .expect("D dials C's one listener");
    let refused = bus_stream(&link)
        .err()
        .expect("an ungranted bus.log stream is refused");
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains(aterm_types::control_verbs::DRIVE_UNGRANTED.trim()),
        "{refused}"
    );
    mac_d.end();
    mac_c.end();
}
