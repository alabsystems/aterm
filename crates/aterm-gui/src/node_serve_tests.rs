// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aterm_agent::fleet_log::LocalLog;
use aterm_agent::node_service::{node_job_holds_lock, node_port_holds_lock};
use aterm_agent::survey_facts::SwarmSurveyFacts;
use aterm_net::fleet::{FleetOp, FleetSecret, NodeIdentity, client_handshake_as};
use aterm_net::fleet_member::{
    Began, Joined, Peer, Registration, Target, Via, fetch_book, node_identity, read_peers,
    register, write_held,
};
use aterm_secrets::age;
use aterm_secrets::commons::binding::Request;
use aterm_secrets::membership_log::{Body, Entrant, Log, NodeStanding, Record, encode_records};

use super::{
    NodeServeConfig, RelayHeader, WindowLauncher, arm_claims_off_on_quit, claims_off_on_quit,
    claims_upkeep, has_live_window_relay, keeps_port_after, query_window_survey_facts,
    read_line_raw, relay_ask, relay_socket_path, run_node_serve_with, serve_window_relay_at,
    wanted_for_node,
};

const SECRET_BYTES: [u8; 24] = [0x5a; 24];
/// The node job each test dials is its own, which serves: a dial waits the hang detector.
const DIAL_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

struct MockWindowLauncher {
    logged_in: AtomicBool,
    launches: AtomicUsize,
    spawn_relay: bool,
    relay_stop: Arc<AtomicBool>,
    relay_pid: u32,
}

impl MockWindowLauncher {
    fn new(logged_in: bool, spawn_relay: bool, relay_stop: Arc<AtomicBool>) -> Self {
        Self {
            logged_in: AtomicBool::new(logged_in),
            launches: AtomicUsize::new(0),
            spawn_relay,
            relay_stop,
            relay_pid: std::process::id(),
        }
    }
}

impl WindowLauncher for MockWindowLauncher {
    fn is_logged_in(&self) -> bool {
        self.logged_in.load(Ordering::Acquire)
    }

    fn launch_window(&self, _state_dir: &Path, relay_dir: &Path) -> Result<(), String> {
        self.launches.fetch_add(1, Ordering::AcqRel);
        if !self.spawn_relay {
            return Ok(());
        }
        let dir = relay_dir.to_path_buf();
        let stop = Arc::clone(&self.relay_stop);
        let pid = self.relay_pid;
        std::thread::Builder::new()
            .name("test-mock-window-relay".to_owned())
            .spawn(move || {
                let _ = serve_window_relay_at(&dir, pid, Some(&stop), |header, mut stream| {
                    std::thread::spawn(move || match header {
                        RelayHeader::Survey { .. } => {
                            while let Ok(Some(line)) = read_line_raw(&mut stream, 4096) {
                                if line.trim().starts_with("survey") {
                                    let _ = stream.write_all(
                                        b"OK 1\ninstance pid=99 host=m7.local version=0.146.0 \
                                          sessions=1 level=content cut=0 at_ms=1793160000000 \
                                          node=11111111111111111111111111111111 instances=1 \
                                          label=m7 window=1 window_last=1793160000 \
                                          ssh_host_key=-\n",
                                    );
                                    let _ = stream.flush();
                                }
                            }
                        }
                        RelayHeader::Ask { .. } => {}
                        RelayHeader::Drive { driver, .. } => {
                            let Ok(Some(head)) = read_line_raw(&mut stream, 4096) else {
                                return;
                            };
                            assert!(
                                head.starts_with("spawn "),
                                "expected forwarded spawn line, got {head:?}"
                            );
                            let _ = stream.write_all(b"GO 11\n");
                            let _ = stream.flush();
                            let mut task = [0u8; 11];
                            stream.read_exact(&mut task).expect("read 11 task bytes");
                            assert_eq!(&task, b"hello world");
                            let reply = format!("OK 1\nsid=s-woken driver={}\n", driver.node);
                            let _ = stream.write_all(reply.as_bytes());
                            let _ = stream.flush();
                        }
                    });
                    true
                });
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn seed_id(state_dir: &Path) -> NodeIdentity {
    aterm_uds::private_file::ensure_private_dir(state_dir).expect("private dir");
    node_identity(state_dir).expect("node_identity")
}

fn seed_fleet_node(state_dir: &Path) -> (FleetSecret, NodeIdentity) {
    let secret = FleetSecret::new(&SECRET_BYTES).expect("secret");
    let id = seed_id(state_dir);
    write_held(state_dir, Began::Here, |book| {
        book.write_key(&SECRET_BYTES).expect("write fleet key");
    })
    .expect("write_held");
    (secret, id)
}

#[test]
fn relay_header_roundtrips_and_rejects_malformed_frames() {
    let peer: SocketAddr = "127.0.0.1:51234".parse().unwrap();
    let survey = RelayHeader::Survey {
        content: true,
        drive: false,
        peer: Some(peer),
    };
    assert_eq!(
        RelayHeader::parse(&survey.encode()).expect("parse survey"),
        survey
    );

    let driver = crate::fabric::Driver {
        node: "ab".repeat(16),
        host: "m12.local (node abababab)".to_owned(),
    };
    let drive = RelayHeader::Drive {
        driver: driver.clone(),
        key: Some(aterm_net::fleet::NodeKey::from_bytes([0xcd; 32])),
        peer: None,
    };
    assert_eq!(
        RelayHeader::parse(&drive.encode()).expect("parse drive"),
        drive
    );
    let drive_no_key = RelayHeader::Drive {
        driver,
        key: None,
        peer: None,
    };
    assert_eq!(
        RelayHeader::parse(&drive_no_key.encode()).expect("parse drive without key"),
        drive_no_key
    );
    let ask = RelayHeader::Ask { peer };
    assert_eq!(RelayHeader::parse(&ask.encode()).expect("parse ask"), ask);

    // Negative controls: malformed, missing, or unknown relay headers are rejected.
    for bad in [
        "",
        "PING\n",
        "SURVEY content=2 drive=0 peer=-\n",
        "SURVEY content=1 peer=-\n",
        "DRIVE node= host=x peer=-\n",
        "DRIVE node=abcd peer=-\n",
        "ASK peer=-\n",
        "ASK\n",
        "ASK peer=127.0.0.1:1 more=1\n",
    ] {
        assert!(
            RelayHeader::parse(bad).is_err(),
            "malformed relay header must be rejected: {bad:?}"
        );
    }
}

#[test]
fn windowless_node_answers_survey_and_wakes_window_on_spawn_or_refuses_when_logged_out() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-wake");
    let state_dir = root.path().join("state");
    let home_dir = root.path().join("home");
    std::fs::create_dir_all(&home_dir).unwrap();
    let config_path = root.path().join("aterm.toml");

    let (secret, _server_id) = seed_fleet_node(&state_dir);
    let driver_dir = root.path().join("driver-state");
    let driver_id = seed_id(&driver_dir);
    let driver_at = seed_joined_driver(&state_dir, &driver_id);

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
    let addr = prebound.local_addr().unwrap();
    crate::test_scratch::write_config(
        &config_path,
        format!(
            "[fleet]\nlisten = \"{addr}\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n[ai]\nmachine = \"m7\"\n"
        ),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let relay_stop = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(MockWindowLauncher::new(
        false,
        true,
        Arc::clone(&relay_stop),
    ));
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: Some(home_dir),
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(40),
        wake_timeout: aterm_spec::wait::MUST_HAPPEN,
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
    };

    let server_thread = std::thread::spawn(move || run_node_serve_with(cfg));
    driver_at.dialed();

    // 1. Windowless Survey: answers `version` and `survey` (`window=0`, `instances=0`).
    let tcp = TcpStream::connect(addr).expect("connect survey");
    let mut dialed =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Survey, DIAL_WITHIN)
            .expect("survey handshake");
    dialed.transport.stream().write_all(b"version\n").unwrap();
    dialed.transport.stream().flush().unwrap();
    let ver_line = read_line_raw(dialed.transport.stream(), 4096)
        .expect("read version")
        .expect("version line");
    assert!(ver_line.starts_with("OK version="), "{ver_line}");

    dialed.transport.stream().write_all(b"survey\n").unwrap();
    dialed.transport.stream().flush().unwrap();
    let head = read_line_raw(dialed.transport.stream(), 4096)
        .expect("read survey head")
        .expect("survey head");
    let count: usize = head
        .trim()
        .strip_prefix("OK ")
        .expect("OK N")
        .parse()
        .expect("line count");
    let mut survey_wire = head;
    for _ in 0..count {
        let row = read_line_raw(dialed.transport.stream(), 8192)
            .expect("read row")
            .expect("row");
        survey_wire.push_str(&row);
    }
    assert!(
        survey_wire.contains(" instances=0 ") && survey_wire.contains(" window=0 "),
        "windowless survey must report instances=0 and window=0: {survey_wire}"
    );
    let parsed_facts = SwarmSurveyFacts::from_survey_reply(&survey_wire).expect("parse C16 facts");
    assert!(!parsed_facts.window);
    assert_eq!(parsed_facts.sessions, 0);
    assert_eq!(parsed_facts.label.as_deref(), Some("m7"));
    drop(dialed);

    // 2. Windowless Drive with non-spawn verb refuses without launching a window.
    let tcp = TcpStream::connect(addr).expect("connect drive non-spawn");
    let mut drive_non_spawn =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
            .expect("drive handshake");
    drive_non_spawn
        .transport
        .stream()
        .write_all(b"@s-1 turn hello\n")
        .unwrap();
    drive_non_spawn.transport.stream().flush().unwrap();
    let non_spawn_reply = read_line_raw(drive_non_spawn.transport.stream(), 4096)
        .expect("read non-spawn reply")
        .expect("reply");
    assert_eq!(non_spawn_reply, "ERR no window open on this Mac\n");
    assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
    drop(drive_non_spawn);

    // Account upkeep is answered by the node even without a window or GUI login.
    for request in [
        "accounts-renew poll=0123456789abcdef0123456789abcdef\n",
        "accounts-renew start=invalid\n",
    ] {
        let tcp = TcpStream::connect(addr).expect("connect renewal");
        let mut renewal =
            client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
                .expect("renewal drive handshake");
        renewal
            .transport
            .stream()
            .write_all(request.as_bytes())
            .unwrap();
        renewal.transport.stream().flush().unwrap();
        let reply = read_line_raw(renewal.transport.stream(), 4096)
            .unwrap()
            .unwrap();
        assert!(reply.starts_with("ERR accounts-renew:"), "{reply}");
        assert!(
            !reply.contains("no window") && !reply.contains("logged out"),
            "{reply}"
        );
    }
    assert_eq!(launcher.launches.load(Ordering::Acquire), 0);

    // 3. Negative control (§7.4): logged-out Mac refuses `spawn` with `"ERR logged out\n"`.
    let tcp = TcpStream::connect(addr).expect("connect drive logged out");
    let mut drive_logged_out =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
            .expect("drive handshake");
    drive_logged_out
        .transport
        .stream()
        .write_all(b"spawn agent=claude len=11\n")
        .unwrap();
    drive_logged_out.transport.stream().flush().unwrap();
    let logged_out_reply = read_line_raw(drive_logged_out.transport.stream(), 4096)
        .expect("read logged-out reply")
        .expect("reply");
    assert_eq!(logged_out_reply, "ERR logged out\n");
    assert_eq!(
        launcher.launches.load(Ordering::Acquire),
        0,
        "logged-out Mac must never attempt to launch a GUI window"
    );
    drop(drive_logged_out);

    // 4. Positive (§7.4, C20): logged-in windowless Mac wakes a window on `spawn`, streams
    //    `GO 11\n` + task payload + `OK 1\nsid=s-woken...`, and subsequent `survey` routes to
    //    that live window (`window=1`).
    launcher.logged_in.store(true, Ordering::Release);
    let tcp = TcpStream::connect(addr).expect("connect drive spawn");
    let mut drive_spawn =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
            .expect("drive handshake");
    drive_spawn
        .transport
        .stream()
        .write_all(b"spawn agent=claude len=11\n")
        .unwrap();
    drive_spawn.transport.stream().flush().unwrap();
    let go_line = read_line_raw(drive_spawn.transport.stream(), 4096)
        .expect("read GO")
        .expect("GO line");
    assert_eq!(go_line, "GO 11\n");
    drive_spawn
        .transport
        .stream()
        .write_all(b"hello world")
        .unwrap();
    drive_spawn.transport.stream().flush().unwrap();
    let ok_head = read_line_raw(drive_spawn.transport.stream(), 4096)
        .expect("read OK")
        .expect("OK head");
    assert_eq!(ok_head, "OK 1\n");
    let spawned_body = read_line_raw(drive_spawn.transport.stream(), 4096)
        .expect("read spawned body")
        .expect("spawned body");
    assert!(
        spawned_body.contains("sid=s-woken") && spawned_body.contains(driver_id.node()),
        "{spawned_body}"
    );
    assert_eq!(launcher.launches.load(Ordering::Acquire), 1);
    drop(drive_spawn);

    // Verify `wf-<pid>.sock` now serves `FleetOp::Survey` (`window=1`).
    assert!(relay_socket_path(relay_scratch.path(), std::process::id()).exists());
    let tcp = TcpStream::connect(addr).expect("connect survey with live window");
    let mut live_survey =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Survey, DIAL_WITHIN)
            .expect("live survey handshake");
    live_survey
        .transport
        .stream()
        .write_all(b"survey\n")
        .unwrap();
    live_survey.transport.stream().flush().unwrap();
    let live_head = read_line_raw(live_survey.transport.stream(), 4096)
        .unwrap()
        .unwrap();
    assert_eq!(live_head, "OK 1\n");
    let live_instance = read_line_raw(live_survey.transport.stream(), 8192)
        .unwrap()
        .unwrap();
    assert!(
        live_instance.contains(" window=1 ") && live_instance.contains(" sessions=1 "),
        "survey must forward to live window relay once woken: {live_instance}"
    );
    drop(live_survey);

    relay_stop.store(true, Ordering::Release);
    stop.store(true, Ordering::Release);
    assert_eq!(
        server_thread.join().expect("join server"),
        ExitCode::SUCCESS
    );
}

/// A windowless node job over `state` on an ephemeral loopback port, admitting by `members`.
fn serve_node(
    root: &Path,
    relay_dir: &Path,
    state: &Path,
    members: Option<Arc<LocalLog>>,
) -> (
    SocketAddr,
    Arc<AtomicBool>,
    std::thread::JoinHandle<ExitCode>,
) {
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind node job");
    let addr = prebound.local_addr().unwrap();
    let (stop, server) = serve_node_on(prebound, root, relay_dir, state, members);
    (addr, stop, server)
}

/// [`serve_node`] on `prebound`, a listener the caller bound.
fn serve_node_on(
    prebound: TcpListener,
    root: &Path,
    relay_dir: &Path,
    state: &Path,
    members: Option<Arc<LocalLog>>,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<ExitCode>) {
    let addr = prebound.local_addr().unwrap();
    let config_path = root.join(format!("node-{}.toml", addr.port()));
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: relay_dir.to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        members,
        alab_browse: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
    };
    (stop, std::thread::spawn(move || run_node_serve_with(cfg)))
}

/// A loopback place the book holds a member at, which the test holds: the node job's heartbeat
/// dials each member its book binds as soon as it starts, so its dial of this one reaches the test
/// and no Mac's real listener (a member seeded at the fleet's own port reaches this Mac's aterm).
struct MemberPlace(TcpListener);

impl MemberPlace {
    /// Once the heartbeat's dial waits here, it is hung up on, as by a member gone dark.
    fn dialed(self) {
        until_dialed(&self.0);
        drop(self.0.accept().expect("take the heartbeat's dial"));
    }
}

/// `driver` joined in the book at `state`, at a [`MemberPlace`].
fn seed_joined_driver(state: &Path, driver: &NodeIdentity) -> MemberPlace {
    let place = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind the driver's place");
    let port = place.local_addr().unwrap().port();
    write_held(state, Began::Here, |book| {
        book.update(|peers| {
            aterm_net::fleet_member::record(
                peers,
                Peer {
                    node: Some(driver.node().to_owned()),
                    key: Some(driver.key()),
                    key_from: None,
                    host: "127.0.0.1".to_owned(),
                    addr: Some(Ipv4Addr::LOCALHOST.into()),
                    port,
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_793_160_000_000),
                    via: Via::Joined,
                    fleet: None,
                    signed: None,
                },
            )
        })
        .expect("record joined driver");
    })
    .expect("seed driver in book");
    MemberPlace(place)
}

/// Once a dial waits in `listener`'s backlog, its connect made and its handshake waiting for
/// whoever accepts it.
fn until_dialed(listener: &TcpListener) {
    use std::os::fd::AsRawFd as _;
    let mut ready = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let within = i32::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one pollfd, of a listener the caller holds open across the call; a minute is a hang
    // detector, not a budget.
    let polled = unsafe { libc::poll(&mut ready, 1, within) };
    assert_eq!(
        polled,
        1,
        "no dial came: {}",
        std::io::Error::last_os_error()
    );
}

/// `id` joins the fleet `secret` opens through the listener at `addr`, registering as `host`.
fn join_as(
    addr: SocketAddr,
    secret: &FleetSecret,
    id: &NodeIdentity,
    host: &str,
) -> Result<Joined, String> {
    let tcp = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    let mut dial = client_handshake_as(tcp, secret, Some(id), FleetOp::Join, DIAL_WITHIN)
        .map_err(|e| e.to_string())?;
    register(
        dial.transport.stream(),
        &Registration {
            node: id.node().to_owned(),
            host: host.to_owned(),
            port: 7100,
            version: "0.146.0".to_owned(),
            key: Some(id.key()),
            signed: None,
        },
    )
    .map_err(|e| e.to_string())
}

/// A lab founded on `founders` (each a tag its keys derive from, and its node identity), whose
/// first founder removed the founder tagged `out`: its two records kept in the record store
/// `file` (F13's `records`), which the returned log reads as a Mac reads the ALab's.
fn lab_log(file: &Path, founders: &[(u8, &NodeIdentity)], out: u8) -> Arc<LocalLog> {
    let request = |tag: u8| {
        let identity = age::Identity::from_secret_bytes([tag ^ 0x55; 32]);
        Request::create(
            [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
        )
        .expect("device request")
    };
    let actor = |tag: u8| request(tag).binding().actor();
    let entrants = founders
        .iter()
        .map(|&(tag, id)| Entrant {
            request: request(tag),
            node_key: *id.key().bytes(),
            name: format!("mac-{tag}"),
            label: format!("m{tag}"),
        })
        .collect();
    let genesis = Record::found(&[99; 32], entrants, 1).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let by = founders[0].0;
    let removal = log
        .draft(
            &actor(by),
            7,
            Body::Remove {
                member: actor(out),
                cutoff: *genesis.id(),
            },
        )
        .sign(&[by; 32], [by; 16])
        .expect("removal");
    log.insert(removal.clone())
        .expect("the log takes the removal");
    let (_, out_id) = founders.iter().find(|(tag, _)| *tag == out).expect("out");
    assert_eq!(
        log.membership().node(out_id.key().bytes()),
        NodeStanding::Removed(actor(out)),
        "the lab must hold the removed Mac's node key out"
    );
    std::fs::write(file, encode_records([&genesis, &removal])).expect("the record store");
    Arc::new(LocalLog::founded_at(file.to_path_buf(), *genesis.id()))
}

#[test]
fn a_member_node_job_answers_join_and_book_with_no_window_open() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-member");
    let member_state = root.path().join("member-state");
    let (secret, member_id) = seed_fleet_node(&member_state);
    let (addr, stop, server) = serve_node(root.path(), relay_scratch.path(), &member_state, None);

    // A Mac joins through the windowless node job of a member that minted no code.
    let joiner_id = seed_id(&root.path().join("m1-state"));
    let joined = join_as(addr, &secret, &joiner_id, "m19.local")
        .expect("a member's windowless node job answers a join");
    assert_eq!(joined.issuer.node, member_id.node());
    let recorded = read_peers(&member_state).expect("member book");
    assert!(
        recorded
            .iter()
            .any(|p| p.node.as_deref() == Some(joiner_id.node()) && p.host == "m19.local"),
        "the member's book must keep the Mac that joined: {recorded:?}"
    );

    // Another Mac reads the book from it.
    let reader_id = seed_id(&root.path().join("m2-state"));
    let target = Target {
        host: "127.0.0.1".to_owned(),
        addr: Some(Ipv4Addr::LOCALHOST.into()),
        port: addr.port(),
        node: Some(member_id.node().to_owned()),
        key: Some(member_id.key()),
    };
    let fetched = fetch_book(
        &target,
        (&secret, Some(&reader_id)),
        (Instant::now() + DIAL_WITHIN, DIAL_WITHIN),
    )
    .expect("a member's windowless node job serves its book");
    assert_eq!(fetched.node, member_id.node());
    assert!(
        fetched
            .peers
            .iter()
            .any(|p| p.node.as_deref() == Some(joiner_id.node())),
        "the book served must hold m19: {:?}",
        fetched.peers
    );

    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

#[test]
fn a_removed_node_is_refused_with_the_old_fleet_secret() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-removed");
    let state = root.path().join("node-state");
    let (secret, host_id) = seed_fleet_node(&state);
    let kept_id = seed_id(&root.path().join("kept-state"));
    let removed_id = seed_id(&root.path().join("removed-state"));
    let members = lab_log(
        &root.path().join("records"),
        &[(1, &host_id), (2, &kept_id), (3, &removed_id)],
        3,
    );
    let (addr, stop, server) = serve_node(root.path(), relay_scratch.path(), &state, Some(members));

    // A member the log admits joins with the fleet secret, and the Mac it removed, which holds
    // that same secret, is refused.
    let joined = join_as(addr, &secret, &kept_id, "m2.local");
    assert!(
        joined.is_ok(),
        "a member the log admits must join: {joined:?}"
    );
    let refused = join_as(addr, &secret, &removed_id, "m3.local");
    assert!(
        refused.is_err(),
        "the Mac the log removed must be refused with the old fleet secret: {refused:?}"
    );
    let book = read_peers(&state).expect("book");
    assert!(
        book.iter()
            .any(|p| p.node.as_deref() == Some(kept_id.node())),
        "the member must be in the book: {book:?}"
    );
    assert!(
        !book
            .iter()
            .any(|p| p.node.as_deref() == Some(removed_id.node())),
        "the removed Mac must stay out of the book: {book:?}"
    );

    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

#[test]
fn a_moved_member_is_found_by_its_alab_tcp_advert() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-moved");
    let here = root.path().join("here-state");
    let (_, _) = seed_fleet_node(&here);
    let moved = root.path().join("moved-state");
    let (_, moved_id) = seed_fleet_node(&moved);

    // This Mac's book holds the member at the place it left: a port nothing listens on now.
    let left = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind the place it left")
        .local_addr()
        .unwrap()
        .port();
    write_held(&here, Began::Here, |book| {
        book.update(|peers| {
            aterm_net::fleet_member::record(
                peers,
                Peer {
                    node: Some(moved_id.node().to_owned()),
                    key: Some(moved_id.key()),
                    key_from: None,
                    host: "127.0.0.1".to_owned(),
                    addr: Some(Ipv4Addr::LOCALHOST.into()),
                    port: left,
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_793_160_000_000),
                    via: Via::Joined,
                    fleet: None,
                    signed: None,
                },
            )
        })
        .expect("record the member where it was");
    })
    .expect("seed the book");

    // The member listens somewhere else now, and only its `_alab._tcp` advert says where.
    let moved_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind where it went");
    let now_at = moved_listener.local_addr().unwrap();
    let bell = Arc::new(aterm_secrets::wake::Bell::default());
    let browse = Arc::new(aterm_secrets::discover::Watch::idle_resolving(
        aterm_agent::node_service::ALAB_TCP_SERVICE,
        &bell,
        &[],
        move |instance| {
            (instance == "alab-moved").then(|| aterm_secrets::discover::Advert {
                instance: instance.to_owned(),
                host: "127.0.0.1".to_owned(),
                port: now_at.port(),
                txt: Vec::new(),
            })
        },
    ));
    browse.feed("Browsing for _alab._tcp.local.\n");
    browse.feed(
        "16:02:11.123  Add        3   1 local.               _alab._tcp.          alab-moved\n",
    );

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind this Mac");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("here.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = NodeServeConfig {
        state_dir: here.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        members: None,
        alab_browse: Some(browse),
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

    // The member answers there only once a person's heartbeat has given up on it, as a loaded
    // machine can make any member answer late: the share of a round that heartbeat gives each
    // dial starts before the dial's connect, which waits in the member's backlog, so a second
    // past the share from that connect it is over. This Mac's first round, whose dials of the
    // test's own members wait the hang detector, still reaches it.
    until_dialed(&moved_listener);
    let share = crate::fleet_listen::HEARTBEAT_ROUND
        / (aterm_net::fleet_member::HEARTBEAT_MEMBERS as u32 + 1);
    std::thread::sleep(share + Duration::from_secs(1));
    let (moved_stop, moved_server) = serve_node_on(
        moved_listener,
        root.path(),
        relay_scratch.path(),
        &moved,
        None,
    );

    // The node job's first heartbeat finds no one where the book says, then reaches the member at
    // its advert and brings its entry there.
    let hang = Instant::now() + Duration::from_secs(60);
    let found = loop {
        let book = read_peers(&here).expect("book");
        let entry = book
            .iter()
            .find(|p| p.node.as_deref() == Some(moved_id.node()))
            .cloned();
        if entry.as_ref().is_some_and(|p| p.port == now_at.port()) || Instant::now() >= hang {
            break entry;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        found.map(|p| p.port),
        Some(now_at.port()),
        "the heartbeat must find the moved member at its _alab._tcp advert (it left {left})"
    );

    stop.store(true, Ordering::Release);
    moved_stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert_eq!(moved_server.join().expect("join moved"), ExitCode::SUCCESS);
}

/// A Mac with no fleet secret that asks the node job's port is refused while no window runs,
/// and once one does its request reaches the window's members desk through the relay, under
/// the address it asked from (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10: until C20 ships,
/// the window serves ask).
#[test]
fn an_ask_on_the_node_jobs_port_reaches_the_windows_desk_or_is_refused_with_none() {
    use aterm_secrets::client::Paths;
    use aterm_secrets::commons::bootstrap::Device;
    use aterm_secrets::members::{desk, joiner};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-ask");
    let state_dir = root.path().join("state");
    let _ = seed_fleet_node(&state_dir);
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = NodeServeConfig {
        state_dir,
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

    let open = |name: &str| {
        let paths = Paths::for_home(&root.path().join(name));
        aterm_secrets::client::machine_identity(&paths).expect("identity");
        Device::open(&paths).expect("device").expect("an identity")
    };
    let trust = aterm_secrets::commons::journal::Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    let member = std::sync::Arc::new(std::sync::Mutex::new(desk::Desk::new(
        desk::Local::with(open("m1"), trust, true, "m1").expect("member"),
    )));
    let device = open("j");
    let joining = joiner::Joining {
        device: &device,
        node: [7; 32],
        name: "j".into(),
        model: "Mac15,3".into(),
    };
    let dial = || {
        aterm_net::fleet::ask_handshake(TcpStream::connect(addr).expect("connect"), DIAL_WITHIN)
            .expect("an ask is served without the fleet secret")
    };

    let mut refused = dial();
    let why = joiner::greeting(refused.stream()).expect_err("no window runs");
    assert!(why.to_string().contains("window is not running"), "{why}");
    drop(refused);

    let relay_stop = Arc::new(AtomicBool::new(false));
    let window = std::thread::spawn({
        let (dir, relay_stop, member) = (
            relay_scratch.path().to_path_buf(),
            Arc::clone(&relay_stop),
            Arc::clone(&member),
        );
        move || {
            let _ = serve_window_relay_at(
                &dir,
                std::process::id(),
                Some(&relay_stop),
                move |header, stream| match header {
                    RelayHeader::Ask { peer } => crate::members_desk::attach_relayed_to(
                        Some(Arc::clone(&member)),
                        stream,
                        peer,
                    ),
                    _ => false,
                },
            );
        }
    });
    let relayed =
        aterm_spec::wait::within(Duration::from_secs(60), Duration::from_millis(15), || {
            let mut asked = dial();
            let greeting = joiner::greeting(asked.stream()).ok()?;
            Some((
                joiner::ask(asked.stream(), &greeting, &joining).expect("asked"),
                greeting,
            ))
        });
    let (answer, greeting) = relayed.expect("the window's desk answered within the hang detector");
    assert_eq!(answer, desk::Answer::Asking);
    assert_eq!(&greeting.key, member.lock().unwrap().local().key());
    let rows = member.lock().unwrap().rows(aterm_time::Instant::now());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].name.as_str(), rows[0].from),
        ("j", std::net::IpAddr::from(Ipv4Addr::LOCALHOST))
    );

    relay_stop.store(true, Ordering::Release);
    window.join().expect("window relay");
    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

#[test]
fn node_job_lock_prevents_dual_bind_and_releases_when_node_serve_stops() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-lock");
    let state_dir = root.path().join("state");
    let _ = seed_fleet_node(&state_dir);

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );

    assert!(!node_job_holds_lock(&state_dir));

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path.clone()),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        members: None,
        alab_browse: None,
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

    let lock_acquired = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(15),
        || node_job_holds_lock(&state_dir).then_some(()),
    );
    assert!(
        lock_acquired.is_some(),
        "running node job must hold node-job.lock so GUI yields port 7100"
    );

    // Negative control: attempting to dual-bind `addr` without yielding to `node_job_holds_lock`
    // fails with `AddrInUse`.
    let dual_bind = TcpListener::bind(addr).expect_err("port must already be bound by node job");
    assert_eq!(dual_bind.kind(), std::io::ErrorKind::AddrInUse);

    // Second `run_node_serve_with` on the same `state_dir` sees `node-job.lock` held and exits 0.
    let second = run_node_serve_with(NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: None,
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::new(AtomicBool::new(false)),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        members: None,
        alab_browse: None,
    });
    assert_eq!(second, ExitCode::SUCCESS);

    // Stopping the node job releases both `node-job.lock` and the TCP port cleanly.
    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert!(!node_job_holds_lock(&state_dir));
    TcpListener::bind(addr).expect("GUI can bind port once node job stops");
}

#[test]
fn query_window_survey_facts_reads_live_window_session_counts_unlike_zero_fallback() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-hb-facts");
    let state_dir = root.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state_dir).unwrap();

    // Negative control: collecting local facts with hardcoded `sessions = 0` reports 0 sessions
    // even when `window == true`.
    let zero_fallback =
        SwarmSurveyFacts::collect_at(&state_dir, None, None, 0, true, None, 1_793_160_000);
    assert_eq!(zero_fallback.sessions, 0);

    let relay_stop = Arc::new(AtomicBool::new(false));
    let launcher = MockWindowLauncher::new(true, true, Arc::clone(&relay_stop));
    launcher
        .launch_window(&state_dir, relay_scratch.path())
        .expect("spawn mock window relay");

    let live_facts = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(15),
        || query_window_survey_facts(relay_scratch.path()),
    )
    .expect("live window relay must answer survey facts");
    assert_eq!(
        live_facts.sessions, 1,
        "node heartbeat must preserve live window session count from wf-<pid>.sock"
    );
    assert!(live_facts.window);
    assert_eq!(live_facts.label.as_deref(), Some("m7"));

    relay_stop.store(true, Ordering::Release);
    let sock = relay_socket_path(relay_scratch.path(), std::process::id());
    let _ = aterm_uds::connect::until(&sock, Instant::now() + Duration::from_millis(100));
}

#[test]
fn member_node_with_identity_only_aterm_toml_preserves_default_drive_grant() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-id-only");
    let state_dir = root.path().join("state");
    let (secret, _) = seed_fleet_node(&state_dir);

    let driver_dir = root.path().join("driver-state");
    let driver_id = seed_id(&driver_dir);
    let driver_at = seed_joined_driver(&state_dir, &driver_id);

    // `sync.identity` writes `[ai]\nmachine = "m7"\n[fleet]\nmachine = "m7"\n` without explicit
    // `listen`, `allow`, or `drive_from`. A member node job must still serve `FleetOp::Drive`
    // under the default `allow = "drive"`, `drive_from = ["*"]` policy.
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        "[ai]\nmachine = \"m7\"\n[fleet]\nmachine = \"m7\"\n",
    );

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
    let addr = prebound.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let relay_stop = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(MockWindowLauncher::new(true, true, Arc::clone(&relay_stop)));
    let cfg = NodeServeConfig {
        state_dir,
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(30),
        wake_timeout: aterm_spec::wait::MUST_HAPPEN,
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
    driver_at.dialed();

    let tcp = TcpStream::connect(addr).expect("connect drive");
    let mut drive_dial =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
            .expect("member with identity-only aterm.toml must offer FleetOp::Drive");
    drive_dial
        .transport
        .stream()
        .write_all(b"spawn agent=claude len=11\n")
        .unwrap();
    drive_dial.transport.stream().flush().unwrap();
    let go_line = read_line_raw(drive_dial.transport.stream(), 4096)
        .expect("read GO")
        .expect("GO line");
    assert_eq!(go_line, "GO 11\n");
    drive_dial
        .transport
        .stream()
        .write_all(b"hello world")
        .unwrap();
    drive_dial.transport.stream().flush().unwrap();
    let ok_head = read_line_raw(drive_dial.transport.stream(), 4096)
        .expect("read OK")
        .expect("OK head");
    assert_eq!(ok_head, "OK 1\n");
    drop(drive_dial);

    relay_stop.store(true, Ordering::Release);
    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

fn idle_launcher() -> Arc<MockWindowLauncher> {
    Arc::new(MockWindowLauncher::new(
        false,
        false,
        Arc::new(AtomicBool::new(false)),
    ))
}

/// A WINDOW YIELDS ONLY TO A NODE THAT SERVES: the job holds `node-port.lock` (which the window
/// yields the port to) only once it can serve the port, never while it idles with no fleet key,
/// and lets it go when it stops.
#[test]
fn the_port_lock_is_held_only_while_the_node_serves_the_port() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-port-lock");
    let state_dir = root.path().join("state");
    let _ = seed_id(&state_dir);
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let tick = Duration::from_millis(20);
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: tick,
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the job runs", || {
        node_job_holds_lock(&state_dir).then_some(())
    });
    // No fleet key yet: the job idles, and the window must not yield the port to it.
    std::thread::sleep(tick * 10);
    assert!(
        !node_port_holds_lock(&state_dir),
        "an idle node job holds the port lock"
    );
    write_held(&state_dir, Began::Here, |book| {
        book.write_key(&SECRET_BYTES).expect("write fleet key");
    })
    .expect("write_held");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job serves the port",
        || node_port_holds_lock(&state_dir).then_some(()),
    );
    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert!(!node_port_holds_lock(&state_dir) && !node_job_holds_lock(&state_dir));
}

/// THE NODE JOB WAITS FOR THE STEP: while System State's Local Network step is still to come
/// (`lan_may_go` says no), the job, whose fleet key and listener are ready, takes no port and
/// so advertises, listens and dials nothing on the LAN; once the step asked macOS, it serves.
#[test]
fn the_node_job_waits_for_the_local_network_step_before_it_serves_the_lan() {
    static STEP_ASKED: AtomicBool = AtomicBool::new(false);
    static LOOKS: AtomicUsize = AtomicUsize::new(0);
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-lan-step");
    let state_dir = root.path().join("state");
    let _ = seed_fleet_node(&state_dir);
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: || {
            LOOKS.fetch_add(1, Ordering::AcqRel);
            STEP_ASKED.load(Ordering::Acquire)
        },
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job looks at the step three times",
        || (LOOKS.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert!(
        !node_port_holds_lock(&state_dir),
        "the job served the LAN before System State's Local Network step asked macOS"
    );
    STEP_ASKED.store(true, Ordering::Release);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job serves the port once the step asked",
        || node_port_holds_lock(&state_dir).then_some(()),
    );
    stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

/// A JOB THAT CANNOT BIND GIVES THE PORT BACK: it keeps `node-port.lock` through a bind refused
/// because a window still holds the port, and lets it go on any other refusal, so the window
/// serves the port again.
#[test]
fn a_bind_the_node_cannot_make_gives_the_port_back() {
    use std::io::{Error, ErrorKind};
    assert!(keeps_port_after(&Error::from(ErrorKind::AddrInUse)));
    let unbindable = TcpListener::bind("192.0.2.1:0").expect_err("TEST-NET-1 is on no interface");
    assert!(!keeps_port_after(&unbindable), "{unbindable}");
    assert!(!keeps_port_after(&Error::from(ErrorKind::PermissionDenied)));
}

/// THE NODE NEVER WIDENS AN UNREAD [fleet]: an aterm.toml that does not parse, or cannot be read,
/// starts no listener at all, never the default `drive` grant for every member; a missing one, or
/// one that names only this Mac, keeps the default.
#[test]
fn an_aterm_toml_the_node_cannot_read_starts_no_listener() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-unread");
    let state_dir = root.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state_dir).unwrap();
    let cfg_at = |config_path: std::path::PathBuf| NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::new(AtomicBool::new(false)),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
    };
    let broken = root.path().join("broken.toml");
    crate::test_scratch::write_config(
        &broken,
        "[fleet]\nlisten = \"127.0.0.1:0\"\nallow = \"facts\"\n[broken",
    );
    assert_eq!(wanted_for_node(&cfg_at(broken)), None, "an unparsed file");
    let unreadable = root.path().join("a-folder.toml");
    std::fs::create_dir(&unreadable).unwrap();
    assert_eq!(wanted_for_node(&cfg_at(unreadable)), None, "an unread file");
    let missing = wanted_for_node(&cfg_at(root.path().join("missing.toml")));
    assert_eq!(
        missing.and_then(|wanted| wanted.allow).as_deref(),
        Some(aterm_net::fleet::DRIVE_LEVEL),
        "no aterm.toml keeps the default policy"
    );
}

/// A BUSY WINDOW IS NOT NO WINDOW: while a window runs here and takes no hand-over, a survey and
/// a drive are answered busy, never `window=0` or "no window open", and a spawn opens no second
/// window.
#[test]
fn a_running_window_that_takes_no_hand_over_is_answered_busy() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-busy");
    let state_dir = root.path().join("state");
    let (secret, _) = seed_fleet_node(&state_dir);
    let driver_id = seed_id(&root.path().join("driver-state"));
    let driver_at = seed_joined_driver(&state_dir, &driver_id);
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n"),
    );
    // A window whose control server takes nothing: its relay answers the hand-over with an ERR.
    let relay_stop = Arc::new(AtomicBool::new(false));
    let window = {
        let dir = relay_scratch.path().to_path_buf();
        let stop = Arc::clone(&relay_stop);
        std::thread::spawn(move || {
            let _ = serve_window_relay_at(&dir, std::process::id(), Some(&stop), |_, _| false);
        })
    };
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the window's relay", || {
        has_live_window_relay(relay_scratch.path()).then_some(())
    });
    let stop = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(MockWindowLauncher::new(
        true,
        false,
        Arc::new(AtomicBool::new(false)),
    ));
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
    driver_at.dialed();
    for (op, line) in [
        (FleetOp::Survey, "survey\n"),
        (FleetOp::Drive, "@s-1 turn hello\n"),
        (FleetOp::Drive, "spawn agent=claude len=11\n"),
    ] {
        let tcp = TcpStream::connect(addr).expect("connect");
        let mut dialed = client_handshake_as(tcp, &secret, Some(&driver_id), op, DIAL_WITHIN)
            .expect("handshake");
        let _ = dialed.transport.stream().write_all(line.as_bytes());
        let _ = dialed.transport.stream().flush();
        let reply = read_line_raw(dialed.transport.stream(), 4096)
            .expect("read")
            .expect("a reply");
        assert!(
            reply.starts_with("ERR busy: an aterm window is open on this Mac")
                && reply.contains("control server unavailable"),
            "{line:?}: {reply}"
        );
        assert!(!reply.contains("window=0") && !reply.contains("no window open"));
    }
    assert_eq!(
        launcher.launches.load(Ordering::Acquire),
        0,
        "no second window"
    );
    let tcp = TcpStream::connect(addr).expect("renewal beside busy window");
    let mut renewal =
        client_handshake_as(tcp, &secret, Some(&driver_id), FleetOp::Drive, DIAL_WITHIN)
            .expect("renewal drive handshake");
    renewal
        .transport
        .stream()
        .write_all(b"accounts-renew poll=0123456789abcdef0123456789abcdef\n")
        .unwrap();
    renewal.transport.stream().flush().unwrap();
    let reply = read_line_raw(renewal.transport.stream(), 4096)
        .unwrap()
        .unwrap();
    assert!(
        reply.starts_with("ERR accounts-renew: unknown renewal request"),
        "{reply}"
    );
    drop(renewal);
    stop.store(true, Ordering::Release);
    relay_stop.store(true, Ordering::Release);
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    let _ = window.join();
}

/// An asking Mac's side of a relayed `ask`: nothing to read, every refusal kept.
struct AskSide(Vec<u8>);

impl std::io::Read for AskSide {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl std::io::Write for AskSide {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// AN ASK TO A BUSY WINDOW IS REFUSED AS BUSY: a Mac asking to join while the window runs and takes
/// no hand-over hears to ask again, never that no window runs; with no window, it hears that.
#[test]
fn an_ask_a_busy_window_takes_no_hand_over_of_is_refused_as_busy() {
    let relay_scratch = crate::test_scratch::short_dir("node-serve-ask-busy");
    let peer: std::net::SocketAddr = "127.0.0.1:7100".parse().unwrap();
    let ask = relay_ask(relay_scratch.path().to_path_buf(), None);
    let mut nobody = AskSide(Vec::new());
    ask(&mut nobody, peer).expect("ask");
    let nobody = aterm_codec::pct::decode(&String::from_utf8_lossy(&nobody.0));
    assert!(nobody.contains("window is not running"), "{nobody}");

    let relay_stop = Arc::new(AtomicBool::new(false));
    let window = {
        let dir = relay_scratch.path().to_path_buf();
        let stop = Arc::clone(&relay_stop);
        std::thread::spawn(move || {
            let _ = serve_window_relay_at(&dir, std::process::id(), Some(&stop), |_, _| false);
        })
    };
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the window's relay", || {
        has_live_window_relay(relay_scratch.path()).then_some(())
    });
    let fallbacks = Arc::new(AtomicUsize::new(0));
    let attempted = Arc::clone(&fallbacks);
    let automatic: aterm_net::fleet::AskHandler = Arc::new(move |_, _| {
        attempted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let ask = relay_ask(relay_scratch.path().to_path_buf(), Some(automatic));
    let mut busy = AskSide(Vec::new());
    ask(&mut busy, peer).expect("ask");
    let busy = aterm_codec::pct::decode(&String::from_utf8_lossy(&busy.0));
    assert!(
        busy.contains("took no fleet connection") && busy.contains("ask again"),
        "{busy}"
    );
    assert!(!busy.contains("not running"), "{busy}");
    assert_eq!(fallbacks.load(Ordering::SeqCst), 0);
    relay_stop.store(true, Ordering::Release);
    let _ = window.join();
}

/// A new Mac can reach the node's automatic admission handler without an open window.
/// The original relay-only path answers a refusal instead of the handler's response.
#[test]
fn a_windowless_node_serves_its_automatic_ask_handler() {
    let root = crate::test_scratch::short_dir("node-automatic-ask");
    let peer: SocketAddr = "192.168.1.40:7100".parse().unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let callback = Arc::clone(&called);
    let automatic: aterm_net::fleet::AskHandler = Arc::new(move |stream, from| {
        assert_eq!(from, peer);
        callback.fetch_add(1, Ordering::SeqCst);
        stream.write_all(b"automatic admission handler\n")
    });
    let ask = relay_ask(root.path().to_path_buf(), Some(automatic));
    let mut reply = AskSide(Vec::new());
    ask(&mut reply, peer).unwrap();
    assert_eq!(reply.0, b"automatic admission handler\n");
    assert_eq!(called.load(Ordering::SeqCst), 1);
}

/// With no window open, the node job looks at the account claims itself (docs/DESIGN-fluid-swarm-
/// 2026-10-28.md F19): its Commons runtime keeps the Mac heard, so only a look here gives up the
/// claims this Mac no longer uses. While a window's relay is live, the window looks and this job
/// does not.
///
/// NEGATIVE CONTROL: make `claims_upkeep` return `false` without calling `look` (no look while no
/// window runs, as before), and the windowless tick looks nothing.
#[test]
fn the_node_job_looks_at_the_account_claims_only_while_no_window_runs() {
    let relay = aterm_tempfile::tempdir().expect("relay dir");
    let looks = AtomicUsize::new(0);
    let look = || {
        looks.fetch_add(1, Ordering::SeqCst);
    };
    assert!(claims_upkeep(relay.path(), &look));
    assert_eq!(looks.load(Ordering::SeqCst), 1);

    std::fs::write(relay_socket_path(relay.path(), std::process::id()), b"")
        .expect("a live window's relay");
    assert!(!claims_upkeep(relay.path(), &look));
    assert_eq!(looks.load(Ordering::SeqCst), 1);
}

/// The node job stopping with no window open gives up this Mac's account claims
/// (docs/DESIGN-fluid-swarm-2026-10-28.md F19): launchd sends it SIGTERM as the person logs out,
/// restarts or shuts down, and with no window its looks were the Mac's only ones. While a window's
/// relay is live the window gives up its own (as the system's quit ends its run, or a terminating
/// signal ends it), and this job does not.
///
/// NEGATIVE CONTROL: make `claims_off_on_quit` return `false` without calling `off` (the job dies
/// of its SIGTERM with nothing written, as before), and the windowless quit gives up nothing.
#[test]
fn the_node_job_gives_up_the_account_claims_as_it_stops_only_while_no_window_runs() {
    let relay = aterm_tempfile::tempdir().expect("relay dir");
    let offs = AtomicUsize::new(0);
    let off = || {
        offs.fetch_add(1, Ordering::SeqCst);
    };
    assert!(claims_off_on_quit(relay.path(), &off));
    assert_eq!(offs.load(Ordering::SeqCst), 1);

    std::fs::write(relay_socket_path(relay.path(), std::process::id()), b"")
        .expect("a live window's relay");
    assert!(!claims_off_on_quit(relay.path(), &off));
    assert_eq!(offs.load(Ordering::SeqCst), 1);
}

/// Where the stand-in of [`a_sigterm_gives_up_the_node_jobs_claims_then_ends_the_job_by_it`]
/// finds its relay directory, and where its `off` writes its marker.
const OFF_RELAY: &str = "ATERM_TEST_NODE_CLAIMS_OFF_RELAY";
const OFF_MARKER: &str = "ATERM_TEST_NODE_CLAIMS_OFF_MARKER";

/// The stand-in's `off`: a marker that it ran.
fn off_marks() {
    if let Some(marker) = std::env::var_os(OFF_MARKER) {
        std::fs::write(marker, b"off").expect("the off marker");
    }
}

/// A SIGTERM TO THE NODE JOB GIVES UP ITS CLAIMS, THEN ENDS THE JOB BY IT
/// (docs/DESIGN-fluid-swarm-2026-10-28.md F19): armed as `run_node_serve` arms it, launchd's
/// SIGTERM runs the job's `off` where no window's relay is live and not where one is, and either
/// way the job then dies of that SIGTERM. A stand-in process of this test is the job, its `off` a
/// marker.
///
/// NEGATIVE CONTROL: drop the stand-in's `arm_claims_off_on_quit` call (the job unarmed, as
/// before), and it dies of its SIGTERM with no marker where no window runs.
#[test]
fn a_sigterm_gives_up_the_node_jobs_claims_then_ends_the_job_by_it() {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(relay) = std::env::var_os(OFF_RELAY) {
        arm_claims_off_on_quit(std::path::PathBuf::from(relay), off_marks);
        let pid = libc::pid_t::try_from(std::process::id()).expect("a pid fits pid_t");
        // SAFETY: a signal to this stand-in process itself.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        // A hang detector: the signal ends this process first.
        std::thread::sleep(aterm_spec::wait::MUST_HAPPEN);
        panic!("the SIGTERM did not end the node job's stand-in");
    }
    for window in [false, true] {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let relay = scratch.path().join("relay");
        std::fs::create_dir(&relay).expect("relay dir");
        if window {
            // This test's own process stands for the window: its pid is live.
            std::fs::write(relay_socket_path(&relay, std::process::id()), b"")
                .expect("a live window's relay");
        }
        let marker = scratch.path().join("off");
        let mut stand_in = crate::test_alone::StandIn::this_test();
        stand_in
            .command()
            .env(OFF_RELAY, &relay)
            .env(OFF_MARKER, &marker);
        let ran = stand_in.spawn().selected();
        let status = ran.status.expect("the stand-in exited");
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "window = {window}: the job did not die of its SIGTERM ({status})\n{}",
            ran.stderr
        );
        assert_eq!(
            marker.exists(),
            !window,
            "{}\n{}",
            if window {
                "a window's relay was live, and the job gave up the window's claims"
            } else {
                "no window ran, and the job died with its claims standing"
            },
            ran.stderr
        );
    }
}

/// A WINDOW'S `open` IS BOUNDED: a launch that outlives its deadline is killed and reported, a
/// failed one is reported, and one that ends in time is no error.
#[cfg(unix)]
#[test]
fn opening_a_window_is_bounded_by_its_deadline() {
    let run = |program: &str, args: &[&str], within: Duration| {
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
            std::path::Path::new(program),
        ));
        cmd.args(args);
        super::run_open_within(&mut cmd, program, within)
    };
    let started = Instant::now();
    let stuck = run("/bin/sleep", &["60"], Duration::from_millis(300)).expect_err("killed");
    assert!(stuck.contains("/bin/sleep"), "{stuck}");
    assert!(
        started.elapsed() < Duration::from_secs(50),
        "the run outlived its deadline"
    );
    let failed = run("/usr/bin/false", &[], aterm_spec::wait::MUST_HAPPEN).expect_err("exit 1");
    assert!(failed.contains("exited with"), "{failed}");
    run("/usr/bin/true", &[], aterm_spec::wait::MUST_HAPPEN).expect("ends in time");
}

#[cfg(unix)]
#[test]
fn node_open_table_cut_marks_wake_drive_cut_and_hangs_up_attached_window_stream() {
    use super::NodeOpenTable;
    use crate::fleet_listen::Serving;
    use aterm_net::fleet::{ALL_MEMBERS, DriveGrant, FleetScope};
    use aterm_uds::CtlStream;

    let table = Arc::new(NodeOpenTable::default());
    let revoked_node = "9a".repeat(16);
    let good_node = "9b".repeat(16);
    let revoked_grant =
        DriveGrant::from_word(&format!("{revoked_node}:{}", "cd".repeat(32))).expect("grant");
    let good_grant =
        DriveGrant::from_word(&format!("{good_node}:{}", "ef".repeat(32))).expect("grant");
    let serving = Serving {
        scope: FleetScope::Content,
        grants: vec![DriveGrant::from_word(ALL_MEMBERS).expect("all")],
    };

    let (near_revoked, mut far_revoked) = CtlStream::pair().expect("pair");
    let (win_revoked, mut win_revoked_peer) = CtlStream::pair().expect("win pair");
    let (near_good, _far_good) = CtlStream::pair().expect("pair");
    let (win_good, mut win_good_peer) = CtlStream::pair().expect("win pair");

    let open_revoked = table
        .admit(true, Some(revoked_grant), &near_revoked, |_, _| true)
        .expect("io")
        .expect("admitted");
    let open_good = table
        .admit(true, Some(good_grant), &near_good, |_, _| true)
        .expect("io")
        .expect("admitted");

    assert!(!open_revoked.is_cut());
    assert!(open_revoked.attach_line(&win_revoked));
    assert!(open_good.attach_line(&win_good));
    // The test keeps its own copies of `near_revoked` and `win_revoked`, as the drive and the
    // window relay keep theirs: only `cut_unserved`'s hang-up of the shared socket ends them.

    let cut = table.cut_unserved(&serving, &|_, _| true, std::slice::from_ref(&revoked_node));
    assert_eq!(cut, 1);
    assert!(open_revoked.is_cut());
    assert!(!open_good.is_cut());
    // Once cut, attempting to attach a late window relay stream is refused.
    let (late_win, _late_peer) = CtlStream::pair().expect("late pair");
    assert!(!open_revoked.attach_line(&late_win));

    // `cut_unserved` hung up the cut open's line and its attached window line, so each peer
    // reads end of file at once; a read still waiting at the hang detector is a line the cut
    // missed.
    let mut buf = [0u8; 8];
    for (what, peer) in [
        ("open", &mut far_revoked),
        ("window", &mut win_revoked_peer),
    ] {
        let _ = peer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
        let read = peer.read(&mut buf);
        assert!(
            matches!(read, Ok(0)),
            "a cut {what} line reads end of file: {read:?}"
        );
    }
    // `win_good` stays open: nothing arrives on it, nor its end.
    let _ = win_good_peer.set_read_timeout(Some(Duration::from_millis(50)));
    assert!(win_good_peer.read(&mut buf).is_err_and(|e| matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )));
}

/// An `aterm.toml` whose `[swarm]` cannot be read holds sharing off
/// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9: "a file that cannot be read holds sharing off"),
/// as `lan_first_use::sharing_on` and the advert's `should_advertise_alab_tcp_with` read it. So
/// the node job's watch ends a generation already serving, its heartbeats and facts pushes with
/// it, unless the switch reads on: never only on a clean `false`. Reads files only; dials
/// nothing.
/// NEGATIVE CONTROL: end a generation only on `Ok(false)` in `swarm_holds`.
#[test]
fn an_unreadable_swarm_switch_ends_the_node_jobs_generation() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-switch");
    let state_dir = root.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state_dir).unwrap();
    let cfg_at = |config_path: std::path::PathBuf| NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_may_go: || false,
        stop: Arc::new(AtomicBool::new(false)),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
    };
    let on = root.path().join("on.toml");
    crate::test_scratch::write_config(&on, "[swarm]\nenabled = true\n");
    assert!(
        !super::fleet_turned_off_or_swarm_disabled(&cfg_at(on)),
        "control: sharing on keeps the generation"
    );
    let off = root.path().join("off.toml");
    crate::test_scratch::write_config(&off, "[swarm]\nenabled = false\n");
    assert!(
        super::fleet_turned_off_or_swarm_disabled(&cfg_at(off)),
        "control: a clean off ends the generation"
    );
    let unreadable = root.path().join("unreadable.toml");
    crate::test_scratch::write_config(&unreadable, "[swarm]\nenabled = \"off\"\n");
    assert!(
        aterm_secrets::swarm::enabled_at(&unreadable).is_err(),
        "the switch cannot read it"
    );
    assert!(
        super::fleet_turned_off_or_swarm_disabled(&cfg_at(unreadable)),
        "a switch that cannot be read holds sharing off, so the generation's LAN uses end"
    );
}
