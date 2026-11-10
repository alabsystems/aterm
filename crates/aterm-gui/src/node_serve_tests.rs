// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
use aterm_uds::stop::Stop;

use super::{
    NodeBoot, NodeMesh, NodeServeConfig, NodeUpdater, NodeUpdates, RETRY_LEFT_AFTER, RelayHeader,
    WindowLauncher, WindowRelays, a_window_relay_answers, apply_at_node_boot,
    arm_claims_off_on_quit, claims_off_on_quit, claims_upkeep, has_live_window_relay,
    keeps_port_after, mind_node_checker, own_node_job, query_window_survey_facts, read_line_raw,
    relay_ask, relay_socket_path, restart_for, run_node_serve_with, serve_window_relay_at,
    wanted_for_node, why_the_run_ends,
};
use aterm_session::SessionId;

/// A test window that stands for this Mac's only one: it hosts every session a drive names.
fn hosts_every(_: &SessionId) -> bool {
    true
}

const SECRET_BYTES: [u8; 24] = [0x5a; 24];
/// The node job each test dials is its own, which serves: a dial waits the hang detector.
const DIAL_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg(unix)]
#[test]
fn refused_live_config_withdraws_existing_drive_grants() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&path, b"", 0o600).unwrap();
    let running = super::default_node_wanted();
    let read = super::read_config_text(Some(&path));
    let allowed = super::node_serving_now(&running, &running, read.as_deref());
    assert!(!allowed.grants.is_empty());
    assert_eq!(allowed.scope, aterm_net::fleet::FleetScope::Content);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    let read = super::read_config_text(Some(&path));
    assert!(read.is_none());
    let refused = super::node_serving_now(&running, &running, read.as_deref());
    assert!(refused.grants.is_empty());
    assert_eq!(refused.scope, aterm_net::fleet::FleetScope::Facts);
    std::fs::remove_file(&path).unwrap();
    let read = super::read_config_text(Some(&path));
    assert_eq!(read.as_deref(), Some(""));
    assert!(
        !super::node_serving_now(&running, &running, read.as_deref())
            .grants
            .is_empty()
    );
    for disabled in ["[swarm]\nenabled = false\n", "[fleet]\nlisten = \"\"\n"] {
        let serving = super::node_serving_now(&running, &running, Some(disabled));
        assert!(serving.grants.is_empty());
        assert_eq!(serving.scope, aterm_net::fleet::FleetScope::Facts);
    }
}

#[path = "node_serve_tests/bus_log.rs"]
mod bus_log;
#[path = "node_serve_tests/fresh_enrollment.rs"]
mod fresh_enrollment;
#[path = "node_serve_tests/held_logs.rs"]
mod held_logs;
#[path = "node_serve_tests/mesh_links.rs"]
mod mesh_links;
#[path = "node_serve_tests/needs.rs"]
mod needs;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "node_serve_tests/ports.rs"]
mod ports;
#[path = "node_serve_tests/presence.rs"]
mod presence;
#[path = "node_serve_tests/two_windows.rs"]
mod two_windows;
#[path = "node_serve_tests/vault_pass.rs"]
mod vault_pass;

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

    fn runs_this_app(&self, pid: u32) -> bool {
        pid == self.relay_pid
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
                let _ = serve_window_relay_at(
                    &dir,
                    pid,
                    Some(&stop),
                    hosts_every,
                    |header, mut stream| {
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
                            RelayHeader::Ask { .. } | RelayHeader::Needs => {}
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
                    },
                );
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
        sids: Vec::new(),
    };
    assert_eq!(
        RelayHeader::parse(&drive.encode()).expect("parse drive"),
        drive
    );
    let drive_no_key = RelayHeader::Drive {
        driver: driver.clone(),
        key: None,
        peer: None,
        sids: Vec::new(),
    };
    assert_eq!(
        RelayHeader::parse(&drive_no_key.encode()).expect("parse drive without key"),
        drive_no_key
    );
    // A drive naming sessions carries them, so the window that hosts them takes it.
    let naming = RelayHeader::Drive {
        driver,
        key: Some(aterm_net::fleet::NodeKey::from_bytes([0xcd; 32])),
        peer: Some(peer),
        sids: vec![
            SessionId::new("s-4c56f539e92639ba37ae"),
            SessionId::new("s-b"),
        ],
    };
    assert!(
        naming
            .encode()
            .ends_with(" sids=s-4c56f539e92639ba37ae,s-b\n"),
        "{}",
        naming.encode()
    );
    assert_eq!(
        RelayHeader::parse(&naming.encode()).expect("parse drive naming sessions"),
        naming
    );
    let ask = RelayHeader::Ask { peer };
    assert_eq!(RelayHeader::parse(&ask.encode()).expect("parse ask"), ask);

    // Negative controls: malformed, missing, duplicate, control-injected, or unknown relay headers
    // are rejected.
    for bad in [
        "",
        "PING\n",
        "SURVEY content=2 drive=0 peer=-\n",
        "SURVEY content=1 peer=-\n",
        "SURVEY content=0 content=1 drive=0 peer=-\n",
        "SURVEY content=0 drive=0 drive=1 peer=-\n",
        "SURVEY content=0 drive=0 peer=- peer=-\n",
        "DRIVE node= host=x peer=-\n",
        "DRIVE node=abcd peer=-\n",
        "DRIVE node=abcd node=efgh host=x peer=-\n",
        "DRIVE node=ab%0acd host=x peer=-\n",
        "DRIVE node=abcd host=x%00y peer=-\n",
        "DRIVE node=abcd host=x peer=- sids=\n",
        "DRIVE node=abcd host=x peer=- sids=-\n",
        "DRIVE node=abcd host=x peer=- sids=s-a,,s-b\n",
        "DRIVE node=abcd host=x peer=- sids=s-a,x-b\n",
        "DRIVE node=abcd host=x peer=- sids=s-\n",
        "DRIVE node=abcd host=x peer=- sids=s-a sids=s-b\n",
        "ASK peer=-\n",
        "ASK\n",
        "ASK peer=127.0.0.1:1 more=1\n",
    ] {
        assert!(
            RelayHeader::parse(bad).is_err(),
            "malformed relay header must be rejected: {bad:?}"
        );
    }
    assert_eq!(super::parse_relay_filename("wf-42.sock"), Some(42));
    assert_eq!(super::parse_relay_filename("wf-042.sock"), None);
    assert_eq!(super::parse_relay_filename("wf-0.sock"), None);
}

#[test]
fn query_window_survey_facts_reads_large_account_rosters_beyond_sixty_four_lines() {
    use aterm_agent::survey_facts::{ServerHeardFact, SurveyAccountFact};
    use std::collections::BTreeMap;

    let relay_scratch = crate::test_scratch::short_dir("node-serve-large-survey");
    let relay_dir = relay_scratch.path().to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let pid = std::process::id();

    let accounts: Vec<SurveyAccountFact> = (0..75)
        .map(|idx| SurveyAccountFact {
            pool: ["claude", "codex", "grok", "kimi"][idx % 4].to_owned(),
            id: format!("seat-{idx:02}"),
            condition: "usable".to_owned(),
            enrolled: true,
            usage_5h_pct: Some((idx % 50) as u32),
            usage_7d_pct: Some((idx % 80) as u32),
            week_reset: Some(1_793_500_000),
            walled_until: None,
            last_used: Some(1_793_159_900),
            read_at: None,
            launch: None,
            token: None,
        })
        .collect();
    let expected = SwarmSurveyFacts {
        release_lines: Vec::new(),
        node: Some("11111111111111111111111111111111".to_owned()),
        label: Some("m22".to_owned()),
        roster_unread: false,
        version: "0.150.0".to_owned(),
        window: true,
        window_last: Some(1_793_160_000),
        sessions: 12,
        ssh_host_key: None,
        accounts,
        accounts_unread: Vec::new(),
        session_accounts: BTreeMap::new(),
        moves: None,
        server_heard: vec![ServerHeardFact {
            vault: "alab".to_owned(),
            subject: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p".to_owned(),
            at: 1_793_159_995,
        }],
        home: None,
        home_free_mb: None,
        repos: None,
    };
    let mut reply_lines: Vec<String> = Vec::new();
    let base_wire = expected.to_survey_reply(pid, Some("m22.local"), 1_793_160_000_000, 1, false);
    for (idx, line) in base_wire.lines().skip(1).enumerate() {
        reply_lines.push(line.to_owned());
        if idx == 0 {
            for s in 0..12 {
                reply_lines.push(format!("session sid=s-{s} pid=90{s} program=zsh"));
            }
        }
    }
    assert!(
        reply_lines.len() > 64,
        "fixture must exceed the old 64-line relay cap: {}",
        reply_lines.len()
    );
    let full_wire = format!("OK {}\n{}\n", reply_lines.len(), reply_lines.join("\n"));

    let dir_clone = relay_dir.clone();
    let stop_clone = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let _ = serve_window_relay_at(
            &dir_clone,
            pid,
            Some(&stop_clone),
            hosts_every,
            move |header, mut s| {
                let payload = full_wire.clone();
                std::thread::spawn(move || {
                    if matches!(header, RelayHeader::Survey { .. })
                        && let Ok(Some(line)) = read_line_raw(&mut s, 4096)
                        && line.trim() == "survey"
                    {
                        let _ = s.write_all(payload.as_bytes());
                        let _ = s.flush();
                    }
                });
                true
            },
        );
    });

    let sock_path = relay_socket_path(&relay_dir, pid);
    let wait_until = Instant::now() + DIAL_WITHIN;
    while !sock_path.exists() {
        assert!(
            Instant::now() < wait_until,
            "window relay socket never bound"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let windows = WindowRelays::new(relay_dir.clone(), idle_launcher());
    let parsed = query_window_survey_facts(&windows).expect("full 89-line survey facts");
    assert_eq!(parsed.accounts.len(), 75);
    assert_eq!(parsed.server_heard.len(), 1);
    assert_eq!(parsed, expected);

    stop.store(true, Ordering::Release);
    let _ = handle.join();
}

#[test]
fn windowless_node_answers_survey_and_wakes_window_on_spawn_or_refuses_when_logged_out() {
    // Its node job reads its scratch home's checkouts, this process's one last read.
    let _one = aterm_agent::harness::repo_work::home::one_test_at_a_time();
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

    let stop = Arc::new(Stop::new());
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
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: aterm_spec::wait::MUST_HAPPEN,
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
        updates: None,
        mesh: NodeMesh::off(),
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
        "vault approve invalid\n",
        "vault status\n",
        "vault approve abcd-efgh-ijkl-mnop 0123456789abcdef0123456789abcdef\n",
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
        assert!(
            reply.starts_with("ERR accounts-renew:") || reply.starts_with("ERR vault:"),
            "{reply}"
        );
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
    stop.set();
    assert_eq!(
        server_thread.join().expect("join server"),
        ExitCode::SUCCESS
    );
}

/// A windowless node job over `state` on an ephemeral loopback port, admitting by `members`.
/// The node job serves the drive grants of its fleet state only while that state was taken under
/// the vault configured (`aterm_agent::fleet_vault`, ATERM-KI-1105-10): beside a record of another
/// vault its listener serves no grant, `"*"` included, so no member drives this Mac by one; under
/// the vault recorded, and in a job that reads no vault, it serves them as before.
///
/// NEGATIVE CONTROL (2026-11-05): `node_serving` serving `node_serving_now`'s grants unfiltered
/// (`super::in_effect` dropped) fails "a fleet state taken under another vault serves no grant".
#[cfg(unix)]
#[test]
fn the_node_job_serves_no_grant_beside_a_fleet_taken_under_another_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let _ = seed_fleet_node(&state);
    let vault = aterm_secrets::client::Paths::for_home(root.path());
    let configure = |name: &str, signer: u8| {
        aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
        aterm_secrets::fsx::write_atomic(
            &vault.dir.join("source.toml"),
            format!(
                "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\n\
                 host = \"m22\"\n",
                aterm_secrets::b64::encode_padded(&[signer; 32])
            )
            .as_bytes(),
            0o600,
        )
        .unwrap();
    };
    configure("alab", 1);
    aterm_agent::fleet_vault::keep(&state, Some(&vault));
    assert!(
        aterm_net::fleet_member::read_vault(&state)
            .unwrap()
            .is_some(),
        "PRECONDITION: the fleet state records the vault \"alab\""
    );
    let config = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config,
        "[fleet]\nlisten = \"127.0.0.1:7999\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n",
    );
    let running = super::default_node_wanted();
    let serving = super::node_serving(
        (Some(config.clone()), state.clone(), Some(vault.clone())),
        running.clone(),
        running.clone(),
    );
    assert_eq!(
        serving().grants.len(),
        1,
        "PRECONDITION: the default grant, under the vault it was taken under"
    );
    configure("other", 9);
    assert_eq!(
        serving().grants,
        Vec::new(),
        "a fleet state taken under another vault serves no grant"
    );
    let unread = super::node_serving((Some(config), state, None), running.clone(), running);
    assert_eq!(
        unread().grants.len(),
        1,
        "a job that reads no vault serves its grants as before"
    );
    configure("alab", 1);
    assert_eq!(
        serving().grants.len(),
        1,
        "CONTROL: the vault recorded again"
    );
}

fn serve_node(
    root: &Path,
    relay_dir: &Path,
    state: &Path,
    members: Option<Arc<LocalLog>>,
) -> (SocketAddr, Arc<Stop>, std::thread::JoinHandle<ExitCode>) {
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
) -> (Arc<Stop>, std::thread::JoinHandle<ExitCode>) {
    serve_node_reading(prebound, root, relay_dir, state, (members, None))
}

/// [`serve_node_on`], its deny list read from the vault at `vault_paths` (`None`: none, so it
/// names no one).
fn serve_node_reading(
    prebound: TcpListener,
    root: &Path,
    relay_dir: &Path,
    state: &Path,
    (members, vault_paths): (Option<Arc<LocalLog>>, Option<aterm_secrets::client::Paths>),
) -> (Arc<Stop>, std::thread::JoinHandle<ExitCode>) {
    let addr = prebound.local_addr().unwrap();
    let config_path = root.join(format!("node-{}.toml", addr.port()));
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(Stop::new());
    let cfg = NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: relay_dir.to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths,
        members,
        alab_browse: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        updates: None,
        mesh: NodeMesh::off(),
    };
    (stop, std::thread::spawn(move || run_node_serve_with(cfg)))
}

/// A loopback place the book holds a member at, which the test holds: the node job's heartbeat
/// dials each member its book binds as soon as it starts, and a Mac that joined once a round reads
/// the book after the join, so its dial of this one reaches the test and no Mac's real listener (a
/// member at the fleet's own port reaches this Mac's aterm, at the loopback or under this Mac's
/// name, and another Mac's name reaches that Mac).
struct MemberPlace(TcpListener);

impl MemberPlace {
    fn bind() -> Self {
        Self(TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a member's place"))
    }

    fn port(&self) -> u16 {
        self.0.local_addr().expect("a member's place").port()
    }

    /// Once the heartbeat's dial waits here, it is hung up on, as by a member gone dark.
    fn dialed(self) {
        until_dialed(&self.0);
        drop(
            aterm_uds::accept(
                &self.0,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("take the heartbeat's dial"),
        );
    }
}

/// `driver` joined in the book at `state`, at a [`MemberPlace`].
fn seed_joined_driver(state: &Path, driver: &NodeIdentity) -> MemberPlace {
    let place = MemberPlace::bind();
    let port = place.port();
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
    place
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
    if polled == 0 {
        panic!("no dial came within {:?}", aterm_spec::wait::MUST_HAPPEN);
    }
    assert_eq!(
        polled,
        1,
        "poll for dial failed: {}",
        std::io::Error::last_os_error()
    );
}

/// `id` joins the fleet `secret` opens through the listener at `addr`, registering `place` as
/// where it listens.
fn join_as(
    addr: SocketAddr,
    secret: &FleetSecret,
    id: &NodeIdentity,
    place: &MemberPlace,
) -> Result<Joined, String> {
    let tcp = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    let mut dial = client_handshake_as(tcp, secret, Some(id), FleetOp::Join, DIAL_WITHIN)
        .map_err(|e| e.to_string())?;
    register(
        dial.transport.stream(),
        &Registration {
            node: id.node().to_owned(),
            host: "127.0.0.1".to_owned(),
            port: place.port(),
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
    let joiner_at = MemberPlace::bind();
    let joined = join_as(addr, &secret, &joiner_id, &joiner_at)
        .expect("a member's windowless node job answers a join");
    assert_eq!(joined.issuer.node, member_id.node());
    let recorded = read_peers(&member_state).expect("member book");
    assert!(
        recorded
            .iter()
            .any(|p| p.node.as_deref() == Some(joiner_id.node()) && p.port == joiner_at.port()),
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

    stop.set();
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
    let (kept_at, removed_at) = (MemberPlace::bind(), MemberPlace::bind());
    let joined = join_as(addr, &secret, &kept_id, &kept_at);
    assert!(
        joined.is_ok(),
        "a member the log admits must join: {joined:?}"
    );
    let refused = join_as(addr, &secret, &removed_id, &removed_at);
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

    stop.set();
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

/// A node job that has never read its deny list (its vault's `source.toml` other users may read,
/// which the read refuses) refuses a Mac that proved the fleet secret, answering busy (`NO
/// capacity`, which every dialer dials again after), and records nothing of it; once a read succeeds (no vault configured: a list that names
/// no one) the same Mac joins, and a read that fails again after it keeps that list, so another
/// Mac joins too.
///
/// NEGATIVE CONTROL (2026-11-01): with `update_denied_nodes_cache` answering an empty list while
/// none was read (the shape before), the first join is admitted (`a node job with no deny list
/// read must refuse a proven peer, got Ok(..)`).
#[test]
fn a_node_job_that_has_not_read_its_deny_list_refuses_a_proven_peer_until_it_has() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-unread");
    let state = root.path().join("node-state");
    let (secret, _host_id) = seed_fleet_node(&state);
    let vault = aterm_secrets::client::Paths::for_home(&root.path().join("home"));
    aterm_secrets::fsx::ensure_private_dir(&vault.dir).expect("vault dir");
    let source = vault.dir.join("source.toml");
    let unreadable = || {
        std::fs::write(&source, b"name = \"team\"\n").expect("source");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).expect("widen");
        assert!(
            aterm_secrets::client::swarm_denied_nodes(&vault).is_err(),
            "the fixture's deny list must not read"
        );
    };
    unreadable();
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind node job");
    let addr = prebound.local_addr().unwrap();
    let (stop, server) = serve_node_reading(
        prebound,
        root.path(),
        relay_scratch.path(),
        &state,
        (None, Some(vault.clone())),
    );
    let dial_join = |id: &NodeIdentity| {
        let tcp = TcpStream::connect(addr).expect("connect");
        client_handshake_as(tcp, &secret, Some(id), FleetOp::Join, DIAL_WITHIN).map(|_| ())
    };

    let early = seed_id(&root.path().join("early-state"));
    match dial_join(&early) {
        Err(error) => assert!(
            matches!(error, aterm_net::fleet::FleetError::AddressBusy),
            "expected a busy answer to dial again, got {error:?}"
        ),
        Ok(()) => panic!("a node job with no deny list read must refuse a proven peer, got Ok(..)"),
    }
    assert!(
        !read_peers(&state)
            .expect("book")
            .iter()
            .any(|p| p.node.as_deref() == Some(early.node())),
        "a refused Mac must stay out of the book"
    );

    std::fs::remove_file(&source).expect("unconfigure the vault");
    let (early_at, later_at) = (MemberPlace::bind(), MemberPlace::bind());
    let joined = join_as(addr, &secret, &early, &early_at);
    assert!(
        joined.is_ok(),
        "once a read succeeds the Mac joins: {joined:?}"
    );

    unreadable();
    let later = seed_id(&root.path().join("later-state"));
    let kept = join_as(addr, &secret, &later, &later_at);
    assert!(
        kept.is_ok(),
        "a failed read after a good one keeps that list: {kept:?}"
    );

    stop.set();
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
    browse.hear(true, 1, "alab-moved");

    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind this Mac");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("here.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(Stop::new());
    let cfg = NodeServeConfig {
        state_dir: here.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        members: None,
        alab_browse: Some(browse),
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        updates: None,
        mesh: NodeMesh::off(),
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

    stop.set();
    moved_stop.set();
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert_eq!(moved_server.join().expect("join moved"), ExitCode::SUCCESS);
}

/// A node with no vault to serve refuses an ask while no window runs. Once one does, the
/// request reaches the window's members desk through the relay under its original address.
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
    let stop = Arc::new(Stop::new());
    let cfg = NodeServeConfig {
        state_dir,
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        updates: None,
        mesh: NodeMesh::off(),
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
        records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
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
                hosts_every,
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
    stop.set();
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
}

/// A listener at `addr` once every holder of the one a stopped node job served there has let it
/// go. The job closes its listener as it stops; but where no close-on-fork mark keeps a socket
/// out of a child (Linux, an Apple kernel that ignores the mark), a child another test thread
/// forks meanwhile holds a copy until its `exec` closes it (on Linux the socket goes only as
/// that `exec` returns), and the port stays listening until then. So the bind waits that window
/// out; the minute is a hang detector, never a latency budget, and a listener nothing lets go
/// fails there. On Linux the caller first shows this process holds no copy
/// (`descriptors_of_sockets`), so the wait covers only another process's.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bind_once_released(addr: SocketAddr) -> TcpListener {
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a bind of the port the stopped node job let go, as the GUI's",
        || match aterm_uds::kept_out(|| TcpListener::bind(addr)) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => None,
            Err(error) => panic!("bind {addr}: {error}"),
        },
    )
}

/// The inode of each socket listening at `addr`, an IPv4 one, as `/proc/net/tcp` lists this
/// network namespace's: its address column is the address's four bytes read as a native `u32`
/// and the port, both in hex, and its state `0A` is `LISTEN`.
#[cfg(target_os = "linux")]
fn sockets_listening_at(addr: SocketAddr) -> Vec<u64> {
    let SocketAddr::V4(at) = addr else {
        panic!("{addr}: /proc/net/tcp lists IPv4 sockets");
    };
    let local = format!(
        "{:08X}:{:04X}",
        u32::from_ne_bytes(at.ip().octets()),
        at.port()
    );
    let table = std::fs::read_to_string("/proc/net/tcp").expect("read /proc/net/tcp");
    table
        .lines()
        .skip(1)
        .filter_map(|row| {
            let columns: Vec<&str> = row.split_whitespace().collect();
            (columns.get(1) == Some(&local.as_str()) && columns.get(3) == Some(&"0A"))
                .then(|| columns.get(9)?.parse().ok())
                .flatten()
        })
        .collect()
}

/// Each of this process's descriptors (`/proc/self/fd`) that is one of the sockets `inodes`
/// names, as `fd -> socket:[inode]`.
#[cfg(target_os = "linux")]
fn descriptors_of_sockets(inodes: &[u64]) -> Vec<String> {
    let names: Vec<String> = inodes
        .iter()
        .map(|inode| format!("socket:[{inode}]"))
        .collect();
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let target = std::fs::read_link(entry.path()).ok()?;
            let target = target.to_string_lossy();
            names
                .iter()
                .any(|name| *name == target)
                .then(|| format!("fd {} -> {target}", entry.file_name().to_string_lossy()))
        })
        .collect()
}

/// THE NODE JOB HOLDS ITS PORT AND LETS IT GO: while it runs, `node-job.lock` is held and a
/// second bind of its port is refused; once it stops, the lock is free and the GUI binds the
/// port once every copy of the job's listener is let go ([`bind_once_released`]). The test claims
/// the port for its whole run ([`ports::PortClaim`]), so no other pick, in this binary or
/// another, takes it in the moment between the job letting it go and that bind.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn node_job_lock_prevents_dual_bind_and_releases_when_node_serve_stops() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-lock");
    let state_dir = root.path().join("state");
    let _ = seed_fleet_node(&state_dir);

    let (prebound, _claim) = ports::listener_no_port_zero_bind_takes();
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );

    assert!(!node_job_holds_lock(&state_dir));

    let stop = Arc::new(Stop::new());
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path.clone()),
        vault_paths: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        members: None,
        alab_browse: None,
        updates: None,
        mesh: NodeMesh::off(),
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
        on_generation: None,
        tick_every: Duration::from_millis(30),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::new(Stop::new()),
        launcher: Arc::new(MockWindowLauncher::new(
            false,
            false,
            Arc::new(AtomicBool::new(false)),
        )),
        members: None,
        alab_browse: None,
        updates: None,
        mesh: NodeMesh::off(),
    });
    assert_eq!(second, ExitCode::SUCCESS);

    // The socket the job listens on now, whichever of its generations bound it.
    #[cfg(target_os = "linux")]
    let listening = sockets_listening_at(addr);
    #[cfg(target_os = "linux")]
    assert!(
        !listening.is_empty(),
        "/proc/net/tcp lists the job's listener at {addr}"
    );

    // Stopping the node job releases `node-job.lock` at once (its drop unlocks the open file,
    // whatever child holds a copy of the descriptor) and the TCP port once every copy of its
    // listener is let go. This process holds none once the job has returned: no generation's
    // thread or clone of the listener outlives it, so the wait below covers only a copy another
    // process holds.
    stop.set();
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert!(!node_job_holds_lock(&state_dir));
    #[cfg(target_os = "linux")]
    assert_eq!(
        descriptors_of_sockets(&listening),
        Vec::<String>::new(),
        "this process still holds the stopped node job's listener"
    );
    drop(bind_once_released(addr));
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
        || {
            query_window_survey_facts(&WindowRelays::new(
                relay_scratch.path().to_path_buf(),
                idle_launcher(),
            ))
        },
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
    let stop = Arc::new(Stop::new());
    let relay_stop = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(MockWindowLauncher::new(true, true, Arc::clone(&relay_stop)));
    let cfg = NodeServeConfig {
        state_dir,
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(30),
        wake_timeout: aterm_spec::wait::MUST_HAPPEN,
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        members: None,
        alab_browse: None,
        updates: None,
        mesh: NodeMesh::off(),
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
    stop.set();
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
    let stop = Arc::new(Stop::new());
    let tick = Duration::from_millis(20);
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: tick,
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh::off(),
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
    stop.set();
    assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    assert!(!node_port_holds_lock(&state_dir) && !node_job_holds_lock(&state_dir));
}

/// THE NODE JOB WAITS FOR THE LAN BROKER: while the root LAN broker does not answer
/// (`lan_unavailable` gives its words), the job, whose fleet key and listener are ready, takes no
/// port and so advertises, listens and dials nothing on the LAN; once the broker answers, it
/// serves.
#[test]
fn the_node_job_waits_for_the_lan_broker_before_it_serves_the_lan() {
    static BROKER_ANSWERS: AtomicBool = AtomicBool::new(false);
    static LOOKS: AtomicUsize = AtomicUsize::new(0);
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay_scratch = crate::test_scratch::short_dir("node-serve-lan-broker");
    let state_dir = root.path().join("state");
    let _ = seed_fleet_node(&state_dir);
    let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let addr = prebound.local_addr().unwrap();
    let config_path = root.path().join("aterm.toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
    );
    let stop = Arc::new(Stop::new());
    let cfg = NodeServeConfig {
        state_dir: state_dir.clone(),
        relay_dir: relay_scratch.path().to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: Some(prebound),
        on_generation: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || {
            LOOKS.fetch_add(1, Ordering::AcqRel);
            (!BROKER_ANSWERS.load(Ordering::Acquire))
                .then(|| "no root LAN broker answers".to_owned())
        },
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh::off(),
    };
    let server = std::thread::spawn(move || run_node_serve_with(cfg));
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job looks for the broker three times",
        || (LOOKS.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert!(
        !node_port_holds_lock(&state_dir),
        "the job served the LAN before the broker answered"
    );
    BROKER_ANSWERS.store(true, Ordering::Release);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job serves the port once the broker answers",
        || node_port_holds_lock(&state_dir).then_some(()),
    );
    stop.set();
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
        on_generation: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::new(Stop::new()),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh::off(),
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
            let _ = serve_window_relay_at(
                &dir,
                std::process::id(),
                Some(&stop),
                hosts_every,
                |_, _| false,
            );
        })
    };
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the window's relay", || {
        has_live_window_relay(relay_scratch.path()).then_some(())
    });
    let stop = Arc::new(Stop::new());
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
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&stop),
        members: None,
        alab_browse: None,
        launcher: Arc::clone(&launcher) as Arc<dyn WindowLauncher>,
        updates: None,
        mesh: NodeMesh::off(),
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
    stop.set();
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
    let windows = WindowRelays::new(relay_scratch.path().to_path_buf(), idle_launcher());
    let ask = relay_ask(windows.clone(), None);
    let mut nobody = AskSide(Vec::new());
    ask(&mut nobody, peer).expect("ask");
    let nobody = aterm_codec::pct::decode(&String::from_utf8_lossy(&nobody.0));
    assert!(nobody.contains("window is not running"), "{nobody}");

    let relay_stop = Arc::new(AtomicBool::new(false));
    let window = {
        let dir = relay_scratch.path().to_path_buf();
        let stop = Arc::clone(&relay_stop);
        std::thread::spawn(move || {
            let _ = serve_window_relay_at(
                &dir,
                std::process::id(),
                Some(&stop),
                hosts_every,
                |_, _| false,
            );
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
    let ask = relay_ask(windows, Some(automatic));
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
    let ask = relay_ask(
        WindowRelays::new(root.path().to_path_buf(), idle_launcher()),
        Some(automatic),
    );
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

/// A real quit signal closes both weak-tier logs. The unarmed child is the negative control:
/// reopening at an unknown boot must move its generation after an unclean stop.
#[test]
fn a_sigterm_closes_and_flushes_both_mesh_logs() {
    use aterm_mesh::logs::{MeshConfig, MeshLog};
    use std::os::unix::process::ExitStatusExt as _;
    const STATE: &str = "ATERM_TEST_QUIT_MESH_STATE";
    const ARMED: &str = "ATERM_TEST_QUIT_MESH_ARMED";
    if let Some(state) = std::env::var_os(STATE) {
        let state = std::path::PathBuf::from(state);
        let mesh = aterm_mesh::Mesh::open(MeshConfig {
            state_dir: state.clone(),
            backups: aterm_uds::backup::Backups::INERT,
            clock: aterm_net::fleet::link::ManualClock::new(),
            wall: std::sync::Arc::new(std::time::SystemTime::now),
            standings: std::sync::Arc::new(|| None),
            changed: None,
            watch_every: aterm_spec::wait::MUST_HAPPEN,
            observe: None,
            stores: None,
        })
        .expect("mesh opens");
        let identity = aterm_net::fleet_member::read_node_identity(&state)
            .unwrap()
            .unwrap();
        mesh.apply(aterm_mesh::logs::Standings {
            current: [*identity.key().bytes()].into(),
            ..Default::default()
        });
        for log in MeshLog::ALL {
            let subject = match log {
                MeshLog::Tables => format!("/t/quit/{}", mesh.principal()),
                MeshLog::Metrics => format!("/m/{}/shared/quit", mesh.principal()),
            };
            mesh.publish(log, &subject, b"kept at quit")
                .expect("published");
            std::fs::write(
                state.join(log.word()),
                mesh.broker(log).log_identity().generation.to_string(),
            )
            .expect("generation recorded");
            std::fs::write(
                state.join(format!("{}-head", log.word())),
                mesh.broker(log).head().to_string(),
            )
            .expect("acknowledged head recorded");
            if mesh.broker(log).served_durability().served == astream_broker::Tier::Strict {
                std::fs::write(state.join(format!("{}-strict", log.word())), b"")
                    .expect("served tier");
            }
        }
        if std::env::var_os(ARMED).is_some() {
            arm_claims_off_on_quit(state.join("relay"), || {});
        }
        let pid = libc::pid_t::try_from(std::process::id()).expect("pid");
        // SAFETY: signal this test's stand-in process itself.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        std::thread::sleep(aterm_spec::wait::MUST_HAPPEN);
        panic!("quit did not end the child");
    }
    for (armed, long_path) in [(false, false), (true, false), (false, true), (true, true)] {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("mq")
            .tempdir_in("/tmp")
            .expect("short socket paths");
        let state_path = scratch.path().join(if long_path {
            "s".repeat(110)
        } else {
            "s".to_owned()
        });
        let state = state_path.as_path();
        aterm_uds::private_file::ensure_private_dir(state).expect("scratch state");
        aterm_uds::private_file::write_private(
            state,
            aterm_net::fleet_member::NODE_KEY_FILE,
            format!("{}\n", "17".repeat(32)).as_bytes(),
        )
        .expect("test node key");
        aterm_net::fleet_member::node_identity(state).expect("test node identity");
        let mut child = crate::test_alone::StandIn::this_test();
        child.command().env(STATE, state);
        if armed {
            child.command().env(ARMED, "1");
        }
        let ran = child.spawn().selected();
        assert_eq!(
            ran.status.expect("child exited").signal(),
            Some(libc::SIGTERM),
            "{}",
            ran.stderr
        );
        for log in MeshLog::ALL {
            let path = state.join("mesh").join(log.word()).join(log.file());
            let prepared = aterm_link::at_rest::prepare(&path, aterm_uds::backup::Backups::INERT)
                .expect("seal");
            let mut opts = match log {
                MeshLog::Tables => aterm_link::durability::Durability::Ordered.options(),
                MeshLog::Metrics => aterm_link::durability::Durability::Relaxed.options(),
            };
            opts.at_rest_key = prepared.key.as_ref().map(aterm_link::at_rest::Key::bytes);
            opts.require_bound_at_rest = true;
            opts.boot = astream_broker::Boot::Unknown;
            let broker = astream_broker::Broker::open_opts(&path, opts, || {
                Ok(astream_broker::Prepared::Nothing)
            })
            .expect("reopen after quit");
            let before: u64 = std::fs::read_to_string(state.join(log.word()))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                broker.log_identity().generation == before,
                armed || state.join(format!("{}-strict", log.word())).exists(),
                "{log:?}: clean close preserves generation; unclean weak-tier quit moves it"
            );
            let head: u64 = std::fs::read_to_string(state.join(format!("{}-head", log.word())))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(broker.head(), head, "the acknowledged records survived");
        }
    }
}

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
        // The stand-in starts with SIGTERM at its default (`StandIn`), which the arming needs.
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
    let stuck = run("/bin/sleep", &["60"], Duration::from_millis(300)).expect_err("killed");
    assert!(stuck.contains("/bin/sleep"), "{stuck}");
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
    assert!(table.is_empty(), "a fresh open table is empty");
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

    // A survey connection (`driver: None`) makes the table non-empty, but `cut_unserved` never
    // calls `holds` (so `aterm-node-watch`'s lazy `OnceCell` keyring read is not triggered).
    let (near_survey, _far_survey) = CtlStream::pair().expect("survey pair");
    let open_survey = table
        .admit(true, None, &near_survey, |_, _| true)
        .expect("io")
        .expect("survey admitted");
    assert!(!table.is_empty());
    assert_eq!(
        table.cut_unserved(
            &serving,
            &|_, _| panic!("a survey entry (`driver: None`) must not evaluate `holds`"),
            &[]
        ),
        0
    );
    drop(open_survey);
    assert!(table.is_empty());

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
    drop(open_good);
    assert!(
        table.is_empty(),
        "once all open entries are cut or dropped, the table is empty again"
    );
}

/// An `aterm.toml` whose `[swarm]` cannot be read holds sharing off
/// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9: "a file that cannot be read holds sharing off"),
/// as the advert's `should_advertise_alab_tcp_with` reads it. So
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
        on_generation: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || Some("no root LAN broker answers".to_owned()),
        stop: Arc::new(Stop::new()),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh::off(),
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

/// launchd starts the node job at a soft limit of 256, which shares out half the fleet slots: the
/// real entry serves a loopback handshake before its capacity is read. A window child is told
/// the launcher's limit, which is what that window's shells get. The serving child proves both
/// assertions before ending by SIGKILL, so no node thread outlives its private state.
/// NEGATIVE CONTROLS: remove the raise from `run_node_serve`: the node has too few slots. Omit
/// `window_launcher_limit` on the window's command: its launcher limit reads `None`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_node_job_raises_launchds_descriptor_limit_to_serve_every_fleet_slot() {
    use aterm_sysprobe::jobs::{FdKind, JobsReader, ReadReq, TcpState};
    use std::os::unix::process::ExitStatusExt as _;

    const CHILD: &str = "ATERM_TEST_NODE_DESCRIPTOR_CHILD";
    const WINDOW: &str = "ATERM_TEST_NODE_DESCRIPTOR_WINDOW";
    const PROVED: &str = "node served with every fleet slot and carried launcher limit 256";
    let name = std::thread::current()
        .name()
        .expect("a test thread is named")
        .to_owned();
    let run = |command: &mut aterm_uds::spawnfd::Command| {
        aterm_uds::deadline::output_until(command, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .expect("the child ends within the hang detector")
    };
    if std::env::var_os(WINDOW).is_some() {
        crate::descriptor_limit::settle_incoming_descriptor_limit(
            &crate::handoff_env::HandoffEnv::capture(),
        );
        assert_eq!(
            crate::descriptor_limit::launcher_soft_descriptor_limit(),
            Some(256),
            "a window the job launched hands its shells the launcher's limit"
        );
        return;
    }
    if std::env::var_os(CHILD).is_some() {
        let state = aterm_types::dirs::state_dir().expect("the child's private state");
        let (secret, node) = seed_fleet_node(&state);
        let config = crate::app_config::config_path().expect("the child's private config");
        aterm_uds::private_file::ensure_private_dir(config.parent().unwrap())
            .expect("private config directory");
        crate::test_scratch::write_config(
            &config,
            "[swarm]\nenabled = true\n[fleet]\nlisten = \"127.0.0.1:0\"\nallow = \"facts\"\n\
             [packages]\nenabled = false\nauto_install = false\npeers = []\n",
        );
        let home = aterm_types::dirs::home_dir().expect("the child's private home");
        let store = atpkg::store::resolve_configured().expect("the child's private store");
        assert!(store.prefix.starts_with(home));
        atpkg::peers::turn_off(&store).expect("no auxiliary toolset listener");

        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit.rlim_cur = 256;
        // SAFETY: a change to this child alone that keeps the hard limit as it was.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
            0
        );
        assert!(
            crate::fleet_listen::fleet_slots() < aterm_net::fleet::MAX_SLOTS,
            "CONTROL: launchd's limit serves fewer"
        );
        // The test binary has no root LAN broker: the broker answers, as it does on a Mac
        // with aterm installed.
        let _ = super::NODE_LAN_GATE_FOR_TEST.set(|| None);
        let server = std::thread::spawn(|| super::run_node_serve_owning(|| None));
        let mut reader = JobsReader::new();
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let port = loop {
            assert!(
                !server.is_finished(),
                "the node entry returned before serving"
            );
            let listeners: Vec<_> = reader
                .read(ReadReq {
                    pid: std::process::id(),
                    fds: true,
                })
                .and_then(|process| process.fds)
                .into_iter()
                .flatten()
                .filter_map(|fd| match fd.kind {
                    FdKind::Tcp {
                        state: TcpState::Listen,
                        local_port,
                        ..
                    } => Some(local_port),
                    _ => None,
                })
                .collect();
            assert!(listeners.len() <= 1, "unexpected listeners: {listeners:?}");
            if let [port] = listeners.as_slice() {
                break *port;
            }
            assert!(Instant::now() < deadline, "the node never listened");
            std::thread::sleep(Duration::from_millis(5));
        };
        let tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to the node");
        let dialed = client_handshake_as(tcp, &secret, None, FleetOp::Facts, DIAL_WITHIN)
            .expect("the real node entry serves a fleet handshake");
        assert_eq!(
            dialed.peer.as_ref().map(|proof| proof.node.as_str()),
            Some(node.node()),
            "the listener proves this child's node identity"
        );
        drop(dialed);
        assert_eq!(
            crate::fleet_listen::fleet_slots(),
            aterm_net::fleet::MAX_SLOTS
        );
        let mut window = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        window.envs(crate::descriptor_limit::window_launcher_limit());
        window
            .args([name.as_str(), "--exact", "--nocapture"])
            .env(WINDOW, "1");
        let output = run(&mut window);
        assert!(
            output.status.success(),
            "{}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!("\n{PROVED}");
        std::io::stdout().flush().expect("flush the success proof");
        aterm_uds::process::kill_self_now();
    }
    let scratch = aterm_tempfile::tempdir().expect("the node child's private state");
    let home = scratch.path().join("home");
    aterm_uds::private_file::ensure_private_dir(&home).expect("private home");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .env_clear()
        .args([name.as_str(), "--exact", "--nocapture"])
        .env(CHILD, "1")
        .env("HOME", &home)
        .env("ATERM_STATE_HOME", scratch.path().join("state"))
        .env("XDG_CONFIG_HOME", scratch.path().join("config"))
        .env("XDG_CACHE_HOME", scratch.path().join("cache"))
        .env("XDG_DATA_HOME", scratch.path().join("data"))
        .env("XDG_RUNTIME_DIR", scratch.path().join("run"))
        .env("TMPDIR", scratch.path())
        .env("CI", "1")
        .env(aterm_net::loopback::SEAM, "1")
        .current_dir(scratch.path());
    let output = run(&mut child);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "{}\n{stdout}\n{stderr}",
        output.status
    );
    assert!(
        stdout.lines().any(|line| line == PROVED),
        "the child died without proving its assertions\n{stdout}\n{stderr}"
    );
}

/// The node job reads a linked `aterm.toml`: a `None` here stops it serving this Mac.
#[cfg(unix)]
#[test]
fn the_node_job_reads_a_linked_aterm_toml() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = dir.path().join("dotfiles.toml");
    aterm_uds::atomic_file::write(&real, b"[fleet]\n", 0o600).unwrap();
    let link = dir.path().join("aterm.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        super::read_config_text(Some(&link)).as_deref(),
        Some("[fleet]\n")
    );
    assert_eq!(
        super::read_config_text(Some(&dir.path().join("none.toml"))).as_deref(),
        Some("")
    );
}

/// A link whose target is gone is an unread `[fleet]`, which never widens to the default grant.
#[cfg(unix)]
#[test]
fn the_node_job_does_not_read_a_dangling_link_as_no_config() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let link = dir.path().join("aterm.toml");
    std::os::unix::fs::symlink(dir.path().join("gone.toml"), &link).unwrap();
    assert_eq!(super::read_config_text(Some(&link)), None);
}

/// Lock `mutex`, a poisoned one included (a test that panicked holding it has failed already).
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A scripted [`NodeUpdater`]: its checker stages `stages` as it starts (and [`Self::stage`]
/// stages more), an older build stands in its place once [`Self::older`] says so, its checks
/// setting moved once [`Self::checks_moved`] says so, its clock is its start plus what the test
/// advanced, and it counts each look, checker look, apply and confirmation. Its restart is
/// recorded and ends the run, as production's does.
struct ScriptedUpdater {
    stages: Option<u64>,
    newer: std::sync::Mutex<Option<u64>>,
    older: std::sync::Mutex<Option<u64>>,
    checks_moved: AtomicBool,
    minds: AtomicUsize,
    noticed: std::sync::Mutex<Option<Box<dyn Fn() + Send>>>,
    auto_apply: AtomicBool,
    confirms_land: AtomicBool,
    looks: AtomicUsize,
    applies: AtomicUsize,
    recoveries: AtomicUsize,
    confirms: AtomicUsize,
    trial_pending: AtomicBool,
    trial_reads: AtomicUsize,
    restarts: std::sync::Mutex<Vec<String>>,
    stop: Arc<Stop>,
    started: Instant,
    advanced: std::sync::Mutex<Duration>,
}

impl ScriptedUpdater {
    fn new(stages: Option<u64>) -> Self {
        Self {
            stages,
            newer: std::sync::Mutex::new(None),
            older: std::sync::Mutex::new(None),
            checks_moved: AtomicBool::new(false),
            minds: AtomicUsize::new(0),
            noticed: std::sync::Mutex::new(None),
            auto_apply: AtomicBool::new(true),
            confirms_land: AtomicBool::new(true),
            looks: AtomicUsize::new(0),
            applies: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
            confirms: AtomicUsize::new(0),
            trial_pending: AtomicBool::new(true),
            trial_reads: AtomicUsize::new(0),
            restarts: std::sync::Mutex::new(Vec::new()),
            stop: Arc::new(Stop::new()),
            started: Instant::now(),
            advanced: std::sync::Mutex::new(Duration::ZERO),
        }
    }

    /// The checker staged `build`, and says so.
    fn stage(&self, build: u64) {
        *lock(&self.newer) = Some(build);
        if let Some(noticed) = lock(&self.noticed).as_ref() {
            noticed();
        }
    }

    /// Move the injected clock on by `by`.
    fn advance(&self, by: Duration) {
        *lock(&self.advanced) += by;
    }

    fn restarts(&self) -> Vec<String> {
        lock(&self.restarts).clone()
    }
}

impl NodeUpdater for ScriptedUpdater {
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>) {
        if let Some(build) = self.stages {
            *lock(&self.newer) = Some(build);
            noticed();
        }
        *lock(&self.noticed) = Some(noticed);
    }

    fn newer_build(&self) -> Option<u64> {
        self.looks.fetch_add(1, Ordering::AcqRel);
        *lock(&self.newer)
    }

    fn auto_apply(&self) -> bool {
        self.auto_apply.load(Ordering::Acquire)
    }

    fn apply_at_boot(&self) -> String {
        self.applies.fetch_add(1, Ordering::AcqRel);
        "scripted: nothing swapped".to_owned()
    }

    fn recover_trial_at_boot(&self) -> String {
        self.recoveries.fetch_add(1, Ordering::AcqRel);
        "scripted: trial counted".to_owned()
    }

    fn confirm_trial(&self) -> bool {
        self.confirms.fetch_add(1, Ordering::AcqRel);
        let lands = self.confirms_land.load(Ordering::Acquire);
        if lands {
            self.trial_pending.store(false, Ordering::Release);
        }
        lands
    }

    fn trial_pending(&self) -> bool {
        self.trial_reads.fetch_add(1, Ordering::AcqRel);
        self.trial_pending.load(Ordering::Acquire)
    }

    fn older_build_installed(&self) -> Option<u64> {
        *lock(&self.older)
    }

    fn checks_setting_moved(&self) -> bool {
        self.checks_moved.load(Ordering::Acquire)
    }

    fn mind_checker(&self) {
        self.minds.fetch_add(1, Ordering::AcqRel);
    }

    fn restart(&self, why: &str) {
        lock(&self.restarts).push(why.to_owned());
        self.stop.set();
    }

    fn now(&self) -> Instant {
        self.started + *lock(&self.advanced)
    }

    fn look_every(&self) -> Duration {
        Duration::from_millis(20)
    }
}

/// A window's relay that this test process serves (`wf-<this pid>.sock`), answering a connect
/// until it is dropped, when the window quits and its socket goes.
struct WindowRelay {
    stop: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl WindowRelay {
    fn open(relay_dir: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let dir = relay_dir.to_path_buf();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let _ = serve_window_relay_at(
                    &dir,
                    std::process::id(),
                    Some(&stop),
                    hosts_every,
                    |_, _| true,
                );
            })
        };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the window's relay answers",
            || a_window_relay_answers(relay_dir).then_some(()),
        );
        Self {
            stop,
            serving: Some(serving),
        }
    }
}

impl Drop for WindowRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Leave a relay file named for this live process that nothing listens on: its window crashed.
/// Its socket never listened, so it refuses a connect however many descriptors of it live: the
/// one answered here, which the caller holds while it asks, any copy a child another test
/// spawned took ([`a_relay_file_answers_while_a_copy_of_its_listener_lives`]), and a fork's,
/// which outlives this one ([`a_stale_relay_listener_is_not_kept_alive_by_a_neighbours_fork`]).
fn stale_relay_file(relay_dir: &Path) -> std::os::fd::OwnedFd {
    let stale = relay_socket_path(relay_dir, std::process::id());
    let socket = crate::test_socket::never_listening_socket(&stale);
    assert!(
        stale.exists(),
        "the stale relay's file went with its listener"
    );
    socket
}

/// A [`stale_relay_file`] whose descriptor lives only while `while_open` runs; its file outlasts it.
#[cfg(target_vendor = "apple")]
fn stale_relay_file_with<T>(relay_dir: &Path, while_open: impl FnOnce() -> T) -> T {
    let socket = stale_relay_file(relay_dir);
    let result = while_open();
    drop(socket);
    assert!(
        relay_socket_path(relay_dir, std::process::id()).exists(),
        "the stale relay's file went with its listener"
    );
    result
}

/// A parked fork retains an unmarked listener even after the parent closes it. This is the
/// interference the parallel foreground-reader tests can cause in a stale-relay fixture.
#[cfg(target_vendor = "apple")]
#[test]
fn a_stale_relay_listener_is_not_kept_alive_by_a_neighbours_fork() {
    struct Parked(Option<i32>);
    impl Drop for Parked {
        fn drop(&mut self) {
            let Some(pid) = self.0.take() else {
                return;
            };
            // SAFETY: this is our exact child, which has not been reaped or reused.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the parked child is reaped",
                || {
                    let mut status = 0;
                    // SAFETY: one exact child and a local output slot; never blocks.
                    (unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid).then_some(())
                },
            );
        }
    }
    fn park() -> Parked {
        // Held across the fork alone, as every spawn holds it, so no descriptor another test is
        // making through `aterm_uds::kept_out` is between its creation and its close-on-fork mark
        // when the table is copied. The child never releases its copy.
        let spawning = aterm_uds::spawn_lock::spawning();
        // SAFETY: the child calls only async-signal-safe raise and _exit, with no unwinding.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: the child parks until its parent's SIGKILL, without running Rust code.
            unsafe {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "SIGSTOP parks the inheritance witness without a core signal; its parent ends it by SIGKILL"
                )]
                libc::raise(libc::SIGSTOP);
                libc::_exit(0);
            }
        }
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        drop(spawning);
        Parked(Some(pid))
    }
    let answered = |fixture| {
        let relay = crate::test_scratch::short_dir("stale-relay-fork");
        let mut child = if fixture {
            stale_relay_file_with(relay.path(), park)
        } else {
            let path = relay_socket_path(relay.path(), std::process::id());
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("a relay listener");
            let child = park();
            drop(listener);
            child
        };
        let pid = child.0.expect("the fork is not yet reaped");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the fork is parked before exec",
            || {
                let mut status = 0;
                // SAFETY: our unreaped child and a local output slot; never blocks.
                let found =
                    unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
                if (found == pid && !libc::WIFSTOPPED(status))
                    || (found < 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
                {
                    child.0 = None;
                    panic!("the fork exited before parking: status={status}");
                }
                (found == pid && libc::WIFSTOPPED(status)).then_some(())
            },
        );
        let answers = a_window_relay_answers(relay.path());
        drop(child);
        answers
    };
    assert!(
        answered(false),
        "control: an inherited listener still answers"
    );
    assert!(
        !answered(true),
        "the bound fixture never listens, even while a neighbour forks"
    );
}

/// A NEWER BUILD ENDS THE NODE RUN ONLY WITH NO WINDOW HERE (§4.13): `restart_for` names a
/// staged build while auto-apply is on and no window's relay answers. A window that runs,
/// auto-apply off, or a boot that already failed to run that build within the hour (on the
/// injected clock) holds it. A relay file nothing listens on is no window, though its pid lives.
#[test]
fn a_newer_build_ends_the_node_run_only_with_no_window_here() {
    let relay = crate::test_scratch::short_dir("node-update-restart");
    let updater = ScriptedUpdater::new(None);
    let boot = NodeBoot::nothing_at(updater.started);
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        None,
        "nothing waits"
    );
    updater.stage(2);
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        Some(2),
        "build 2 waits and no window runs"
    );
    updater.auto_apply.store(false, Ordering::Release);
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        None,
        "[update] auto_apply is off"
    );
    updater.auto_apply.store(true, Ordering::Release);

    let stale = stale_relay_file(relay.path());
    assert!(
        has_live_window_relay(relay.path()),
        "CONTROL: by its live pid alone, a relay file whose listener is gone reads as a window"
    );
    assert!(
        !a_window_relay_answers(relay.path()),
        "a relay file nothing listens on answered"
    );
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        Some(2),
        "a relay file that does not answer is no window"
    );
    drop(stale);

    let window = WindowRelay::open(relay.path());
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        None,
        "a window runs here, and applies its own"
    );
    drop(window);
    assert_eq!(
        restart_for(&updater, boot, relay.path()),
        Some(2),
        "the window quit"
    );

    let left = NodeBoot {
        started: updater.now(),
        left: Some(2),
    };
    assert_eq!(
        restart_for(&updater, left, relay.path()),
        None,
        "this run's boot found build 2 and could not run it"
    );
    updater.advance(RETRY_LEFT_AFTER - Duration::from_secs(1));
    assert_eq!(
        restart_for(&updater, left, relay.path()),
        None,
        "within the hour"
    );
    updater.advance(Duration::from_secs(1));
    assert_eq!(
        restart_for(&updater, left, relay.path()),
        Some(2),
        "an hour on, build 2 is tried again"
    );
    updater.stage(3);
    let left = NodeBoot {
        started: updater.now(),
        left: Some(2),
    };
    assert_eq!(
        restart_for(&updater, left, relay.path()),
        Some(3),
        "a build newer than the one the boot left goes at once"
    );
}

/// A RELAY FILE ANSWERS WHILE A COPY OF ITS LISTENER LIVES: a listener closed here still takes a
/// connect through any other descriptor of it, as a child another test spawned while it was open
/// holds (std's spawn takes no spawn lock, so it can copy a descriptor before its close-on-exec
/// mark; a fork holds every one until its `exec`). A stale relay file whose socket never listened
/// answers nothing however long its descriptor lives, so [`stale_relay_file`] leaves that one.
#[test]
fn a_relay_file_answers_while_a_copy_of_its_listener_lives() {
    let relay = crate::test_scratch::short_dir("node-relay-copy");
    let path = relay_socket_path(relay.path(), std::process::id());
    let listener = aterm_uds::kept_out(|| std::os::unix::net::UnixListener::bind(&path))
        .expect("a relay listener");
    let copy = aterm_uds::kept_out(|| listener.try_clone()).expect("a copy of its descriptor");
    drop(listener);
    assert!(
        a_window_relay_answers(relay.path()),
        "CONTROL: a listener closed here answers through a copy of its descriptor"
    );
    drop(copy);
    let held = stale_relay_file(relay.path());
    assert!(
        !a_window_relay_answers(relay.path()),
        "a stale relay file answers nothing while its descriptor lives"
    );
    drop(held);
}

/// THE NODE BOOT APPLIES ONLY WITH NO WINDOW HERE (§4.13): a start runs the boot apply where no
/// relay answers, a stale relay file included, and auto-apply is on, whether or not its own look
/// found a build waiting (the apply reads the stage itself, and with nothing waiting it counts
/// the start against its build's boot trial, so a build that crash-loops under launchd is
/// reverted). While a window runs it neither applies nor counts. With auto-apply off, it only
/// recovers an existing trial. A build it found and the apply did not run is the boot's left build.
#[test]
fn the_node_boot_applies_only_with_no_window_here() {
    let relay = crate::test_scratch::short_dir("node-update-boot");
    let boot_with = |updater: &ScriptedUpdater| {
        let before = updater.applies.load(Ordering::Acquire);
        let recoveries_before = updater.recoveries.load(Ordering::Acquire);
        let boot = apply_at_node_boot(updater, relay.path());
        (
            updater.applies.load(Ordering::Acquire) - before,
            updater.recoveries.load(Ordering::Acquire) - recoveries_before,
            boot.left,
        )
    };
    let updater = ScriptedUpdater::new(None);
    assert_eq!(
        boot_with(&updater),
        (1, 0, None),
        "with nothing waiting the boot apply still counts the start"
    );
    let window = WindowRelay::open(relay.path());
    assert_eq!(
        boot_with(&updater),
        (0, 0, None),
        "a window runs: no boot apply, which could swap a stage under it"
    );
    updater.stage(2);
    assert_eq!(
        boot_with(&updater),
        (0, 0, None),
        "a window runs: build 2 is left to it"
    );
    drop(window);
    assert_eq!(
        boot_with(&updater),
        (1, 0, Some(2)),
        "build 2 waits and no window runs: the boot applies it (the real apply execs it; the \
         scripted one returns, so the boot records it as left)"
    );
    updater.auto_apply.store(false, Ordering::Release);
    assert_eq!(
        boot_with(&updater),
        (0, 1, None),
        "auto-apply off: recover the armed build without admitting the stage"
    );
    let window = WindowRelay::open(relay.path());
    assert_eq!(
        boot_with(&updater),
        (0, 0, None),
        "a live window owns its trial even when automatic installation is off"
    );
    drop(window);
    updater.auto_apply.store(true, Ordering::Release);
    let _stale = stale_relay_file(relay.path());
    assert_eq!(
        boot_with(&updater),
        (1, 0, Some(2)),
        "a relay file that does not answer is no window"
    );
}

/// A node job on scratch state at `state`, its config `config`, its updater `updater`: a run
/// with no vault, on `prebound` if given.
fn serve_updating(
    root: &Path,
    state: &Path,
    relay_dir: &Path,
    updater: &Arc<ScriptedUpdater>,
    (config, prebound): (&str, Option<TcpListener>),
) -> std::thread::JoinHandle<ExitCode> {
    let config_path = root.join("aterm.toml");
    crate::test_scratch::write_config(&config_path, config);
    let cfg = NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: relay_dir.to_path_buf(),
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: prebound,
        on_generation: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::clone(&updater.stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: Some(NodeUpdates::new(
            Arc::clone(updater) as Arc<dyn NodeUpdater>,
            NodeBoot::nothing_at(updater.started),
        )),
        mesh: NodeMesh::off(),
    };
    std::thread::spawn(move || run_node_serve_with(cfg))
}

/// A NODE JOB WITH A WINDOW STAGES AND LEAVES THE APPLY TO IT (§4.13): its checker starts and
/// stages build 2, and while the window's relay answers no look ends the run; once the window
/// quits, the next look ends it, for launchd to start the job again.
#[test]
fn a_node_job_with_a_window_stages_and_ends_its_run_only_once_the_window_quits() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay = crate::test_scratch::short_dir("node-update-window");
    let window = WindowRelay::open(relay.path());
    let updater = Arc::new(ScriptedUpdater::new(Some(2)));
    let server = serve_updating(
        root.path(),
        &root.path().join("state"),
        relay.path(),
        &updater,
        ("[swarm]\nenabled = false\n", None),
    );
    // Its checker staged before the first look, so by the third look two have decided.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job looks three times with build 2 staged",
        || (updater.looks.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert_eq!(
        *lock(&updater.newer),
        Some(2),
        "the job's checker staged nothing"
    );
    assert!(
        updater.restarts().is_empty(),
        "the job ended its run while a window runs here: {:?}",
        updater.restarts()
    );
    drop(window);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job ends its run once the window quit",
        || (!updater.restarts().is_empty()).then_some(()),
    );
    assert_eq!(server.join().expect("the job's thread"), ExitCode::SUCCESS);
    assert_eq!(updater.restarts().len(), 1, "{:?}", updater.restarts());
}

/// A START THAT FINDS THE JOB'S LOCK HELD BOOTS AND STARTS NOTHING (§4.13): no boot apply,
/// which would swap a stage in or count a start against the boot trial of a build the instance
/// holding the lock does not run, no startup of the job's services and no run; it ends as a
/// second instance ends. A lock that cannot be opened orders no start, so that start still
/// boots, which counts a new build that fails there against its trial, and then fails with no
/// service started. CONTROL: once the lock is free, the start takes it, runs its boot while it
/// holds it, then its startup, then the run, which lets it go as it returns.
#[test]
fn a_node_start_that_finds_the_jobs_lock_held_boots_and_starts_nothing() {
    struct Upkeep<'a>(&'a std::sync::Mutex<Vec<&'static str>>, &'a Path);
    impl Drop for Upkeep<'_> {
        fn drop(&mut self) {
            assert!(
                node_job_holds_lock(self.1),
                "upkeep retires under the host lease"
            );
            lock(self.0).push("upkeep-ended");
        }
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let calls = std::sync::Mutex::new(Vec::new());
    let unopenable = root.path().join("a-file-not-a-state-dir");
    std::fs::write(&unopenable, "").expect("a file where the state directory goes");
    assert_eq!(
        own_node_job(
            &unopenable,
            || {
                lock(&calls).push("boot");
                NodeBoot::nothing_at(Instant::now())
            },
            || lock(&calls).push("startup"),
            |_, _, ()| {
                lock(&calls).push("serve");
                ExitCode::SUCCESS
            },
        ),
        ExitCode::FAILURE,
        "a lock that cannot be opened fails the start"
    );
    assert_eq!(
        std::mem::take(&mut *lock(&calls)),
        ["boot"],
        "a start whose lock cannot be opened still boots, and starts nothing"
    );
    let start = || {
        own_node_job(
            &state,
            || {
                lock(&calls).push("boot");
                assert!(
                    node_job_holds_lock(&state),
                    "the boot ran before the job's lock was taken"
                );
                NodeBoot::nothing_at(Instant::now())
            },
            || {
                lock(&calls).push("startup");
                Upkeep(&calls, &state)
            },
            |_, job_lock, upkeep| {
                lock(&calls).push("serve");
                drop(upkeep);
                drop(job_lock);
                ExitCode::FAILURE
            },
        )
    };
    let held = aterm_agent::node_service::NodeJobLock::try_acquire(&state)
        .expect("open the job's lock")
        .expect("take the job's lock");
    assert_eq!(start(), ExitCode::SUCCESS, "a second instance ends as one");
    assert!(
        lock(&calls).is_empty(),
        "a start that found the lock held ran {:?}",
        lock(&calls)
    );
    drop(held);
    assert_eq!(start(), ExitCode::FAILURE, "CONTROL: the run's own status");
    assert_eq!(*lock(&calls), ["boot", "startup", "serve", "upkeep-ended"]);
    assert!(
        !node_job_holds_lock(&state),
        "the run kept the job's lock past its return"
    );
}

/// AN OLDER INSTALLED BUILD ENDS A RUN ONLY ONCE THIS JOB'S IMAGE WAS REPLACED: a bundle whose
/// `Info.plist` names an older build than the executable it holds would otherwise end every run
/// at its first look and loop the job under launchd. CONTROL: a replaced image runs the older
/// build; with none older nothing is asked.
#[test]
fn an_older_installed_build_ends_the_run_only_once_this_jobs_image_was_replaced() {
    assert_eq!(
        super::older_build_to_run(Some(1), || true),
        Some(1),
        "CONTROL"
    );
    assert_eq!(
        super::older_build_to_run(Some(1), || false),
        None,
        "this job's image is still the installed file"
    );
    assert_eq!(
        super::older_build_to_run(None, || panic!("asked with no older build")),
        None
    );
}

/// AN OLDER BUILD IN THIS COPY'S PLACE, OR A CHANGED CHECKS SETTING, ENDS THE NODE RUN WINDOW OR
/// NOT (§4.13): the job serves a build that a revert put back or that no longer stands here, or
/// checks under an `[update] enabled` it read once at its start, and only a restart is its next
/// launch. Each names its reason, the older build first. CONTROL: a newer build still waits
/// while a window runs here, and goes once it quits.
#[test]
fn an_older_build_or_a_changed_checks_setting_ends_the_node_run_window_or_not() {
    let relay = crate::test_scratch::short_dir("node-update-ends");
    let updater = ScriptedUpdater::new(None);
    let boot = NodeBoot::nothing_at(updater.started);
    let window = WindowRelay::open(relay.path());
    updater.stage(3);
    assert_eq!(
        why_the_run_ends(&updater, boot, relay.path()),
        None,
        "CONTROL: a newer build waits while a window runs"
    );
    updater.checks_moved.store(true, Ordering::Release);
    let why = why_the_run_ends(&updater, boot, relay.path())
        .expect("a changed checks setting ends the run while a window runs");
    assert!(why.contains("[update] enabled"), "{why}");
    *lock(&updater.older) = Some(1);
    let why = why_the_run_ends(&updater, boot, relay.path())
        .expect("an older build in this copy's place ends the run while a window runs");
    assert!(why.contains("build 1, older than this one"), "{why}");
    drop(window);
    *lock(&updater.older) = None;
    updater.checks_moved.store(false, Ordering::Release);
    let why = why_the_run_ends(&updater, boot, relay.path())
        .expect("CONTROL: with the window gone the newer build ends the run");
    assert!(why.contains("build 3 waits here"), "{why}");
}

/// THE NODE JOB'S LOOK MINDS ITS CHECKER AND ENDS ITS RUN FOR A CHANGED CHECKS SETTING (§4.13):
/// every look asks the updater to mind its checker, a window or not, and the first look after
/// `[update] enabled` moved ends the run, for launchd to start the job again under it.
#[test]
fn the_node_jobs_look_minds_its_checker_and_ends_its_run_for_a_changed_checks_setting() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let relay = crate::test_scratch::short_dir("node-update-minds");
    let window = WindowRelay::open(relay.path());
    let updater = Arc::new(ScriptedUpdater::new(None));
    let server = serve_updating(
        root.path(),
        &root.path().join("state"),
        relay.path(),
        &updater,
        ("[swarm]\nenabled = false\n", None),
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job's look minds its checker three times",
        || (updater.minds.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert!(
        updater.restarts().is_empty(),
        "the job ended its run with nothing to end it for: {:?}",
        updater.restarts()
    );
    updater.checks_moved.store(true, Ordering::Release);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the job ends its run for the changed setting",
        || (!updater.restarts().is_empty()).then_some(()),
    );
    assert_eq!(server.join().expect("the job's thread"), ExitCode::SUCCESS);
    let restarts = updater.restarts();
    assert_eq!(restarts.len(), 1, "{restarts:?}");
    assert!(restarts[0].contains("[update] enabled"), "{restarts:?}");
    drop(window);
}

/// THE NODE JOB REPLACES A STALLED CHECKER AS A WINDOW DOES (§4.13), since with no window here
/// nothing else would: a stale heartbeat is suspected at one look and replaced at a later one,
/// once per generation. CONTROL: a fresh heartbeat is never replaced.
#[test]
fn the_node_job_replaces_a_stalled_checker_once_and_a_fresh_one_never() {
    use aterm_update::checker_watch::{CheckerBeat, CheckerPhase, STALL_CONFIRM};
    let beat = |phase, at_secs| CheckerBeat {
        generation: 1,
        phase,
        at_secs,
        max_interval_secs: 1_100,
        respawns: 0,
        workers: 1,
        worker_epoch: 0,
        deferrals: 0,
        settings_misses: 0,
        lane: None,
        checks: 0,
    };
    let confirm = STALL_CONFIRM.as_secs();
    let respawned = std::sync::Mutex::new(Vec::new());
    let mut watch = crate::update_checker_watch::CheckerWatchState::default();
    let mut look = |beat, now| {
        mind_node_checker(&mut watch, Some(beat), now, |generation| {
            lock(&respawned).push(generation);
            Some(generation + 1)
        })
        .map(|(stall, replacement)| (stall.generation, replacement))
    };
    let fresh = beat(CheckerPhase::Waiting, 1_000);
    assert_eq!(look(fresh, 1_060), None);
    assert_eq!(
        look(fresh, 1_060 + confirm),
        None,
        "CONTROL: a fresh heartbeat is never replaced"
    );
    let stuck = beat(CheckerPhase::LockWait, 1_000);
    let days = 1_000 + 4 * 86_400;
    assert_eq!(look(stuck, days), None, "the first look only suspects");
    assert_eq!(
        look(stuck, days + confirm),
        Some((1, Some(2))),
        "a later look replaces the stalled checker"
    );
    assert_eq!(
        look(stuck, days + 2 * confirm),
        None,
        "the same stall is acted on once"
    );
    assert_eq!(*lock(&respawned), [1]);
}

/// The node confirms an admitted listener only after its authenticated Census response, or a
/// definite policy hold after a tick. A confirmation that does not land is asked again, and
/// one that lands is not.
#[test]
fn the_node_job_confirms_its_boot_trial_once_it_serves_or_holds_the_lan() {
    for (serves, lands) in [(true, true), (false, true), (false, false)] {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let relay = crate::test_scratch::short_dir("node-update-trial");
        let state = root.path().join("state");
        let (config, prebound) = if serves {
            let _ = seed_fleet_node(&state);
            let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
            let addr = prebound.local_addr().expect("its address");
            (
                format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n"),
                Some(prebound),
            )
        } else {
            ("[swarm]\nenabled = false\n".to_owned(), None)
        };
        let updater = Arc::new(ScriptedUpdater::new(None));
        updater.confirms_land.store(lands, Ordering::Release);
        let server = serve_updating(
            root.path(),
            &state,
            relay.path(),
            &updater,
            (&config, prebound),
        );
        let asks = if lands { 1 } else { 2 };
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the job asks to confirm its boot trial",
            || (updater.confirms.load(Ordering::Acquire) >= asks).then_some(()),
        );
        if serves {
            assert!(
                node_port_holds_lock(&state),
                "the trial was confirmed before the job served its port"
            );
        }
        updater.stop.set();
        assert_eq!(server.join().expect("the job's thread"), ExitCode::SUCCESS);
        if lands {
            assert_eq!(
                updater.confirms.load(Ordering::Acquire),
                1,
                "serves = {serves}: a confirmed trial was asked again"
            );
        }
    }
}

#[test]
fn an_admitted_node_with_invalid_key_or_failed_bind_cannot_confirm_its_trial() {
    for (failure, active) in [
        ("key", true),
        ("bind", true),
        ("lock", true),
        ("key", false),
        ("bind", false),
        ("lock", false),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let relay = crate::test_scratch::short_dir("node-unproven-trial");
        let state = root.path().join("state");
        seed_fleet_node(&state);
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = occupied.local_addr().unwrap();
        if failure == "key" {
            std::fs::write(state.join("fleet.key"), "invalid key").unwrap();
        }
        let _custody = (failure == "lock").then(|| {
            aterm_agent::node_service::NodeJobLock::try_acquire_port(&state)
                .unwrap()
                .unwrap()
        });
        let updater = Arc::new(ScriptedUpdater::new(None));
        updater.trial_pending.store(active, Ordering::Release);
        let config = format!("[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n");
        let server = serve_updating(root.path(), &state, relay.path(), &updater, (&config, None));
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while updater.confirms.load(Ordering::Acquire) == 0
            && updater.restarts().is_empty()
            && (active || updater.trial_reads.load(Ordering::Acquire) == 0)
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        updater.stop.set();
        assert_eq!(server.join().unwrap(), ExitCode::SUCCESS);
        assert_eq!(
            updater.confirms.load(Ordering::Acquire),
            0,
            "{failure}: an admitted node that cannot serve supplied no health evidence"
        );
        assert_eq!(
            updater.restarts().len(),
            usize::from(active),
            "{failure}: only the active trial requests its next recovery boot"
        );
    }
}

#[test]
fn an_owned_port_is_delegated_only_while_a_window_relay_answers() {
    let root = aterm_tempfile::tempdir().unwrap();
    let relay = crate::test_scratch::short_dir("node-window-health-hold");
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let _custody = aterm_agent::node_service::NodeJobLock::try_acquire_port(&state)
        .unwrap()
        .unwrap();
    let _window = WindowRelay::open(relay.path());
    let updater = Arc::new(ScriptedUpdater::new(None));
    let server = serve_updating(
        root.path(),
        &state,
        relay.path(),
        &updater,
        (
            "[fleet]\nlisten = \"127.0.0.1:0\"\nallow = \"facts\"\n",
            None,
        ),
    );
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while updater.confirms.load(Ordering::Acquire) == 0
        && updater.restarts().is_empty()
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    updater.stop.set();
    assert_eq!(server.join().unwrap(), ExitCode::SUCCESS);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 1);
    assert!(updater.restarts().is_empty());
}

#[test]
fn node_health_rechecks_trial_custody_after_delegation_and_confirmation() {
    let updater = Arc::new(ScriptedUpdater::new(None));
    let updates = NodeUpdates::new(
        Arc::clone(&updater) as Arc<dyn NodeUpdater>,
        NodeBoot::nothing_at(updater.started),
    );
    // A window owns this trial now. Its wait can later lapse without restarting
    // this node process; the new headless obligation must not be cached away.
    updater.trial_pending.store(false, Ordering::Release);
    updates.confirm_checkpoint(&updater.stop);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 0);
    updater.trial_pending.store(true, Ordering::Release);
    updates.confirm_checkpoint(&updater.stop);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 1);
    assert!(!updater.trial_pending.load(Ordering::Acquire));

    // The canonical confirmation can also return true while delegating to a
    // window. Even a prior successful callback cannot replace later custody.
    updater.trial_pending.store(true, Ordering::Release);
    updates.confirm_checkpoint(&updater.stop);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 2);
    updates.confirm_checkpoint(&updater.stop);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 2);

    updater.stop.set();
    updater.trial_pending.store(true, Ordering::Release);
    updates.confirm_checkpoint(&updater.stop);
    assert_eq!(updater.confirms.load(Ordering::Acquire), 2);
}

#[test]
fn node_health_rechecks_inputs_after_a_window_relay_answers_or_refuses() {
    for cut in ["policy", "key", "stop"] {
        for answers in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let state = root.path().join("state");
            seed_fleet_node(&state);
            let config = root.path().join("aterm.toml");
            crate::test_scratch::write_config(
                &config,
                "[fleet]\nlisten = \"127.0.0.1:1\"\nallow = \"facts\"\n",
            );
            let updater = Arc::new(ScriptedUpdater::new(None));
            let cfg = NodeServeConfig {
                state_dir: state.clone(),
                relay_dir: root.path().join("relay"),
                home_dir: None,
                config_path: Some(config.clone()),
                vault_paths: None,
                members: None,
                alab_browse: None,
                prebound_listener: None,
                on_generation: None,
                tick_every: Duration::from_millis(20),
                wake_timeout: Duration::from_secs(1),
                stop: Arc::clone(&updater.stop),
                launcher: idle_launcher(),
                lan_unavailable: || None,
                updates: Some(NodeUpdates::new(
                    Arc::clone(&updater) as Arc<dyn NodeUpdater>,
                    NodeBoot::nothing_at(updater.started),
                )),
                mesh: NodeMesh::off(),
            };
            super::checkpoint_idle_with(&cfg, super::IdleCheckpoint::PortOwnedElsewhere, || {
                match cut {
                    "policy" => {
                        crate::test_scratch::write_config(&config, "[swarm]\nenabled = false\n")
                    }
                    "key" => std::fs::write(state.join("fleet.key"), "ab".repeat(24)).unwrap(),
                    "stop" => {
                        updater.stop.set();
                    }
                    _ => unreachable!(),
                }
                answers
            });
            assert_eq!(
                updater.confirms.load(Ordering::Acquire),
                0,
                "{cut}/{answers}"
            );
            assert!(updater.restarts().is_empty(), "{cut}/{answers}");
        }
    }
}

#[test]
fn node_health_fences_probes_and_preserves_only_definite_holds() {
    use std::os::unix::fs::PermissionsExt as _;
    for cut in [
        "stop",
        "policy",
        "key",
        "unchanged",
        "swarm-off",
        "missing-key",
        "missing-node",
        "broker",
        "invalid-config",
        "unread-config",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let (secret, node) = seed_fleet_node(&state);
        let config = root.path().join("aterm.toml");
        let listen = "127.0.0.1:1";
        crate::test_scratch::write_config(
            &config,
            format!("[fleet]\nlisten = \"{listen}\"\nallow = \"facts\"\n"),
        );
        let updater = Arc::new(ScriptedUpdater::new(None));
        let mut cfg = NodeServeConfig {
            state_dir: state.clone(),
            relay_dir: root.path().join("relay"),
            home_dir: None,
            config_path: Some(config.clone()),
            vault_paths: None,
            members: None,
            alab_browse: None,
            prebound_listener: None,
            on_generation: None,
            tick_every: Duration::from_millis(20),
            wake_timeout: Duration::from_secs(1),
            stop: Arc::clone(&updater.stop),
            launcher: idle_launcher(),
            lan_unavailable: || None,
            updates: None,
            mesh: NodeMesh::off(),
        };
        let held = match cut {
            "swarm-off" => {
                crate::test_scratch::write_config(&config, "[swarm]\nenabled = false\n");
                Some(true)
            }
            "missing-key" => {
                std::fs::remove_file(state.join("fleet.key")).unwrap();
                Some(true)
            }
            "missing-node" => {
                std::fs::remove_file(state.join(aterm_net::fleet_member::NODE_KEY_FILE)).unwrap();
                Some(true)
            }
            "broker" => {
                cfg.lan_unavailable = || Some("controlled unavailable broker".into());
                Some(true)
            }
            "invalid-config" => {
                crate::test_scratch::write_config(&config, "invalid TOML [");
                Some(false)
            }
            "unread-config" => {
                std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
                Some(false)
            }
            _ => None,
        };
        if let Some(held) = held {
            cfg.updates = Some(NodeUpdates::new(
                Arc::clone(&updater) as Arc<dyn NodeUpdater>,
                NodeBoot::nothing_at(updater.started),
            ));
            super::checkpoint_idle(&cfg, super::IdleCheckpoint::Waiting);
            assert_eq!(
                updater.confirms.load(Ordering::Acquire),
                usize::from(held),
                "{cut}"
            );
            assert_eq!(updater.restarts().len(), usize::from(!held), "{cut}");
            continue;
        }
        let updates = NodeUpdates::new(
            Arc::clone(&updater) as Arc<dyn NodeUpdater>,
            NodeBoot::nothing_at(updater.started),
        );
        let proved = Some((node.node().to_owned(), node.key()));
        let result = updates.settle_service_with(
            &super::health::Inputs::of(&cfg),
            (listen.parse().unwrap(), &secret, &proved, listen),
            &cfg.stop,
            |_| {
                match cut {
                    "stop" => {
                        cfg.stop.set();
                    }
                    "policy" => crate::test_scratch::write_config(
                        &config,
                        format!("[fleet]\nlisten = \"{listen}\"\nallow = \"content\"\n"),
                    ),
                    "key" => std::fs::write(state.join("fleet.key"), "ab".repeat(24)).unwrap(),
                    _ => {}
                }
                Err("controlled probe failure after its input capture".to_owned())
            },
        );
        assert!(!result);
        assert_eq!(updater.confirms.load(Ordering::Acquire), 0);
        assert_eq!(
            updater.restarts().len(),
            usize::from(cut == "unchanged"),
            "{cut}"
        );
    }
}

/// Names [`a_windowless_node_job_stages_ends_its_run_and_its_next_start_runs_the_new_image`]'s
/// scratch in the child that stands for the node job.
const UPDATE_SCRATCH: &str = "ATERM_TEST_NODE_UPDATE_SCRATCH";
/// What the job's first start says once its run ended for the build it staged.
const FIRST_RUN_ENDED: &str = "the first start ended its run for build 2";
/// What the build the second start installed says as it runs.
const NEW_IMAGE_RUNS: &str = "build 2 runs";

/// The updater of the child that stands for the node job: its checker downloads `download/`
/// into `staged/`, and its boot apply swaps `staged/` in as `installed/` and execs
/// `installed/aterm`, as aterm-update swaps a ready bundle in and execs its binary. The image
/// running is build 1; the stage is build 2. Its restart ends the run, as production's does.
struct FixtureStage {
    scratch: PathBuf,
    stop: Arc<Stop>,
    restarted: AtomicBool,
}

impl NodeUpdater for FixtureStage {
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>) {
        let download = self.scratch.join("download");
        if download.is_dir() {
            std::fs::rename(&download, self.scratch.join("staged")).expect("stage the download");
            noticed();
        }
    }

    fn newer_build(&self) -> Option<u64> {
        self.scratch.join("staged/aterm").is_file().then_some(2)
    }

    fn auto_apply(&self) -> bool {
        true
    }

    fn apply_at_boot(&self) -> String {
        let installed = self.scratch.join("installed");
        if std::fs::rename(self.scratch.join("staged"), &installed).is_err() {
            return "nothing was staged".to_owned();
        }
        // A leading line break ends any line the test harness left open.
        let error = aterm_uds::spawnfd::command(installed.join("aterm"))
            .arg(format!("\n{NEW_IMAGE_RUNS}"))
            .exec();
        format!("the exec of the build it installed failed: {error}")
    }

    fn confirm_trial(&self) -> bool {
        true
    }

    fn trial_pending(&self) -> bool {
        false
    }

    fn recover_trial_at_boot(&self) -> String {
        panic!("this fixture leaves automatic installation on")
    }

    fn older_build_installed(&self) -> Option<u64> {
        None
    }

    fn checks_setting_moved(&self) -> bool {
        false
    }

    fn mind_checker(&self) {}

    fn restart(&self, _why: &str) {
        self.restarted.store(true, Ordering::Release);
        self.stop.set();
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn look_every(&self) -> Duration {
        Duration::from_millis(20)
    }
}

/// The child that stands for the node job: its boot, then a run that holds the LAN, under
/// [`FixtureStage`] in `scratch`.
fn run_staging_node_job(scratch: &Path) {
    let updater = Arc::new(FixtureStage {
        scratch: scratch.to_path_buf(),
        stop: Arc::new(Stop::new()),
        restarted: AtomicBool::new(false),
    });
    let relay = scratch.join("relay");
    // A start that finds the stage applies it here and does not return.
    let boot = apply_at_node_boot(&*updater, &relay);
    assert_eq!(boot.left, None, "the boot did not run the build it found");
    let config_path = scratch.join("aterm.toml");
    crate::test_scratch::write_config(&config_path, "[swarm]\nenabled = false\n");
    let cfg = NodeServeConfig {
        state_dir: scratch.join("state"),
        relay_dir: relay,
        home_dir: None,
        config_path: Some(config_path),
        vault_paths: None,
        prebound_listener: None,
        on_generation: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || Some("no root LAN broker answers".to_owned()),
        stop: Arc::clone(&updater.stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: Some(NodeUpdates::new(
            Arc::clone(&updater) as Arc<dyn NodeUpdater>,
            boot,
        )),
        mesh: NodeMesh::off(),
    };
    assert_eq!(run_node_serve_with(cfg), ExitCode::SUCCESS);
    assert!(
        updater.restarted.load(Ordering::Acquire),
        "the run ended without asking to be started again"
    );
    println!("{FIRST_RUN_ENDED}");
}

/// A start of the node job: this test in a child of the test binary, on `scratch`.
fn node_job_start(scratch: &Path) -> aterm_uds::spawnfd::Command {
    let test = std::thread::current()
        .name()
        .expect("libtest names each test's thread")
        .to_owned();
    let mut start = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    start
        .args([
            "--exact",
            test.as_str(),
            "--nocapture",
            "--test-threads=1",
            "--quiet",
        ])
        .env(UPDATE_SCRATCH, scratch);
    start
}

/// A WINDOWLESS MAC RUNS WHAT ITS NODE JOB STAGED (§4.13), across two starts of a child that
/// stands for the job, with this test as launchd. The first start's checker stages a fixture
/// build (an ad hoc copy of `echo`) and, with no window here, its run ends. The second start's
/// boot swaps the stage in before the job serves and execs the build it installed, which shows
/// it runs by the line it writes.
#[test]
fn a_windowless_node_job_stages_ends_its_run_and_its_next_start_runs_the_new_image() {
    if let Some(scratch) = std::env::var_os(UPDATE_SCRATCH) {
        run_staging_node_job(Path::new(&scratch));
        return;
    }
    let mut scratch = aterm_uds::deadline::KeptUntilRan::new(
        crate::test_scratch::short_dir("node-update-boots").keep(),
    );
    for dir in ["relay", "state", "download"] {
        std::fs::create_dir(scratch.path().join(dir)).expect("a scratch directory");
    }
    // What the first start's checker downloads: the new build.
    aterm_uds::deadline::ad_hoc_copy(
        Path::new("/bin/echo"),
        &scratch.path().join("download/aterm"),
    );

    let first = aterm_uds::deadline::output_until(
        &mut node_job_start(scratch.path()),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the first start ends");
    let said = String::from_utf8_lossy(&first.stdout);
    assert!(
        first.status.success() && said.lines().any(|line| line == FIRST_RUN_ENDED),
        "the first start ({}):\n{said}\n{}",
        first.status,
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        scratch.path().join("staged/aterm").is_file(),
        "the first start staged nothing"
    );

    let stderr = scratch.path().join("second.stderr");
    let mut second = node_job_start(scratch.path())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(std::fs::File::create(&stderr).expect("the second start's stderr"))
        .spawn()
        .expect("the second start");
    aterm_uds::deadline::prints(&mut second, NEW_IMAGE_RUNS, aterm_uds::deadline::ECHO_HANG);
    scratch.ran();
    let status = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the new build exits",
        || second.try_wait().ok().flatten(),
    );
    assert!(
        status.success(),
        "the new build ({status}): {}",
        std::fs::read_to_string(&stderr).unwrap_or_default()
    );
    assert!(
        !scratch.path().join("staged").exists() && scratch.path().join("installed/aterm").is_file(),
        "the second start did not swap the stage in"
    );
}

/// A WINDOWLESS MEMBER'S CHECKOUTS ARE READ AGAIN WITH NO SURVEY (DX-2 review): the node job reads
/// the home on its own clock ([`super::read_checkouts_when_due`]), due at its first tick, so a read
/// past what `--host any` trusts (31 min old) is read again before any survey asks, and the facts
/// its next survey answers carry the new read. Negative control: with the clock not yet due, the
/// stale read stands.
///
/// The home holds no clone, so its read runs no git: what a read finds in a clone is the reader's
/// own (`repo_home`'s tests), and a git read under a loaded machine's contention can run out its
/// own deadline (`repo_work::PROBE_LIMIT`, as long as this test's hang detector) and leave the
/// stale read standing.
#[test]
fn a_windowless_members_checkouts_are_read_again_with_no_survey() {
    use aterm_agent::harness::repo_work::home::{self as repo_home, HomeRepos};
    let _one = repo_home::one_test_at_a_time();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("a home");
    // A read another test left running would replace the planted one when it ends.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "no read of a home running",
        || (!repo_home::reading_now()).then_some(()),
    );
    let now = aterm_time::unix_millis().expect("a clock");
    let stale = now - 31 * 60 * 1000;
    repo_home::plant_latest(
        &home,
        HomeRepos {
            read_ms: stale,
            repos: Vec::new(),
            cut: 0,
        },
    );

    let mut last = Some(Instant::now());
    super::read_checkouts_when_due(Some(&home), &mut last, super::FACTS_HEARTBEAT_EVERY);
    // A read started runs beside the caller: it is running still, or it has ended, the latest
    // read its own (written before it lets go). Neither: none started.
    assert!(
        !repo_home::reading_now(),
        "a clock not yet due starts no read"
    );
    assert_eq!(
        repo_home::latest(&home).map(|read| read.read_ms),
        Some(stale),
        "a clock not yet due reads nothing"
    );

    let mut last = None;
    super::read_checkouts_when_due(Some(&home), &mut last, super::FACTS_HEARTBEAT_EVERY);
    assert!(last.is_some(), "the first tick is due");
    let read = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the node job's own read of the home",
        || repo_home::latest(&home).filter(|read| read.read_ms > stale),
    );
    let refresh_ms = u64::try_from(repo_home::REFRESH.as_millis()).expect("fits");
    let now_s = i64::try_from(now / 1000).expect("fits");
    let facts = SwarmSurveyFacts::collect_at(
        &root.path().join("state"),
        Some(&home),
        None,
        0,
        false,
        None,
        now_s,
    );
    let said = facts.repos.expect("the survey's facts carry the read");
    assert_eq!(said, read);
    assert!(
        aterm_time::unix_millis().expect("a clock") - said.read_ms < refresh_ms,
        "a read within the reader's own refresh period"
    );
    assert!(said.repos.is_empty(), "the home holds no clone");
}
