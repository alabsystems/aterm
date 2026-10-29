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
    NodeServeConfig, RelayHeader, WindowLauncher, claims_upkeep, has_live_window_relay,
    keeps_port_after, query_window_survey_facts, read_line_raw, relay_ask, relay_socket_path,
    run_node_serve_with, serve_window_relay_at, wanted_for_node,
};

const SECRET_BYTES: [u8; 24] = [0x5a; 24];
const DIAL_WITHIN: Duration = Duration::from_secs(5);

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
    write_held(&state_dir, Began::Here, |book| {
        book.update(|peers| {
            aterm_net::fleet_member::record(
                peers,
                Peer {
                    node: Some(driver_id.node().to_owned()),
                    key: Some(driver_id.key()),
                    key_from: None,
                    host: "m12.local".to_owned(),
                    addr: Some(Ipv4Addr::LOCALHOST.into()),
                    port: 7100,
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

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
    let addr = prebound.local_addr().unwrap();
    std::fs::write(
        &config_path,
        format!(
            "[fleet]\nlisten = \"{addr}\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n[ai]\nmachine = \"m7\"\n"
        ),
    )
    .unwrap();

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
        wake_timeout: Duration::from_secs(3),
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
    };

    let server_thread = std::thread::spawn(move || run_node_serve_with(cfg));

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
    let config_path = root.join(format!("node-{}.toml", addr.port()));
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    )
    .unwrap();
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
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
    };
    (
        addr,
        stop,
        std::thread::spawn(move || run_node_serve_with(cfg)),
    )
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
    let (now_at, moved_stop, moved_server) =
        serve_node(root.path(), relay_scratch.path(), &moved, None);
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
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    )
    .unwrap();
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
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

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
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    )
    .unwrap();
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
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    )
    .unwrap();

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

    let lock_acquired =
        aterm_spec::wait::within(Duration::from_secs(5), Duration::from_millis(15), || {
            node_job_holds_lock(&state_dir).then_some(())
        });
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

    let live_facts =
        aterm_spec::wait::within(Duration::from_secs(5), Duration::from_millis(15), || {
            query_window_survey_facts(relay_scratch.path())
        })
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
    write_held(&state_dir, Began::Here, |book| {
        book.update(|peers| {
            aterm_net::fleet_member::record(
                peers,
                Peer {
                    node: Some(driver_id.node().to_owned()),
                    key: Some(driver_id.key()),
                    key_from: None,
                    host: "m12.local".to_owned(),
                    addr: Some(Ipv4Addr::LOCALHOST.into()),
                    port: 7100,
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

    // `sync.identity` writes `[ai]\nmachine = "m7"\n[fleet]\nmachine = "m7"\n` without explicit
    // `listen`, `allow`, or `drive_from`. A member node job must still serve `FleetOp::Drive`
    // under the default `allow = "drive"`, `drive_from = ["*"]` policy.
    let config_path = root.path().join("aterm.toml");
    std::fs::write(
        &config_path,
        "[ai]\nmachine = \"m7\"\n[fleet]\nmachine = \"m7\"\n",
    )
    .unwrap();

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
        wake_timeout: Duration::from_secs(3),
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));

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
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    )
    .unwrap();
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
        stop: Arc::new(AtomicBool::new(false)),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
    };
    let broken = root.path().join("broken.toml");
    std::fs::write(
        &broken,
        "[fleet]\nlisten = \"127.0.0.1:0\"\nallow = \"facts\"\n[broken",
    )
    .unwrap();
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
    write_held(&state_dir, Began::Here, |book| {
        book.update(|peers| {
            aterm_net::fleet_member::record(
                peers,
                Peer {
                    node: Some(driver_id.node().to_owned()),
                    key: Some(driver_id.key()),
                    key_from: None,
                    host: "m12.local".to_owned(),
                    addr: Some(Ipv4Addr::LOCALHOST.into()),
                    port: 7100,
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
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    std::fs::write(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n"),
    )
    .unwrap();
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
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
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
    let ask = relay_ask(relay_scratch.path().to_path_buf());
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
    let mut busy = AskSide(Vec::new());
    ask(&mut busy, peer).expect("ask");
    let busy = aterm_codec::pct::decode(&String::from_utf8_lossy(&busy.0));
    assert!(
        busy.contains("took no fleet connection") && busy.contains("ask again"),
        "{busy}"
    );
    assert!(!busy.contains("not running"), "{busy}");
    relay_stop.store(true, Ordering::Release);
    let _ = window.join();
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
