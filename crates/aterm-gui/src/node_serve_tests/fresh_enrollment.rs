// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh vault identity reaches authenticated control through the default fleet join.

use super::*;
use aterm_agent::fleet_log::ensure_local_node_key_in_log;
use aterm_agent::{FleetSync, test_sync_from_vault_at};
use aterm_net::fleet::{FleetError, read_key_file};
use aterm_net::fleet_member::{JoinCode, Keyring, Unreached, drive};
use aterm_secrets::client::{self, JoinOptions, JoinOutcome, JoinPassword, Membership, Paths};
use aterm_secrets::commons::binding::Pending;
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_secrets::text::Audience;
use std::path::PathBuf;

struct Machine {
    home: PathBuf,
    vault: Paths,
    state: PathBuf,
    config: PathBuf,
    addr: SocketAddr,
}

impl Machine {
    fn new(root: &Path, name: &str) -> (Self, TcpListener) {
        let home = root.join(name);
        let state = home.join("state");
        aterm_uds::private_file::ensure_private_dir(&state).unwrap();
        aterm_uds::private_file::ensure_private_dir(&home.join(".config/aterm")).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        (
            Self {
                vault: Paths::for_home(&home),
                config: home.join(".config/aterm/aterm.toml"),
                home,
                state,
                addr,
            },
            listener,
        )
    }
}

struct Node {
    stop: Arc<AtomicBool>,
    relay_stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<ExitCode>>,
    log: Arc<LocalLog>,
    _relay: aterm_tempfile::TempDir,
}

impl Node {
    fn start(machine: &Machine, listener: TcpListener) -> Self {
        let relay = crate::test_scratch::short_dir("fresh-enrollment-relay");
        let stop = Arc::new(AtomicBool::new(false));
        let relay_stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(LocalLog::of(&machine.vault));
        let launcher = Arc::new(MockWindowLauncher::new(true, true, Arc::clone(&relay_stop)));
        let bell = Arc::new(aterm_secrets::wake::Bell::default());
        let browse = aterm_secrets::discover::Watch::idle_for(
            aterm_agent::node_service::ALAB_TCP_SERVICE,
            &bell,
            &[],
        );
        let cfg = NodeServeConfig {
            state_dir: machine.state.clone(),
            relay_dir: relay.path().to_path_buf(),
            home_dir: Some(machine.home.clone()),
            config_path: Some(machine.config.clone()),
            vault_paths: Some(machine.vault.clone()),
            members: Some(Arc::clone(&log)),
            alab_browse: Some(Arc::new(browse)),
            prebound_listener: Some(listener),
            tick_every: Duration::from_millis(20),
            wake_timeout: DIAL_WITHIN,
            stop: Arc::clone(&stop),
            launcher,
            lan_unavailable: || None,
            updates: None,
            mesh: NodeMesh::off(),
        };
        Self {
            stop,
            relay_stop,
            worker: Some(std::thread::spawn(move || run_node_serve_with(cfg))),
            log,
            _relay: relay,
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        self.relay_stop.store(true, Ordering::Release);
        assert_eq!(
            self.worker.take().unwrap().join().unwrap(),
            ExitCode::SUCCESS
        );
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.relay_stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Vault(Option<aterm_secrets::server::Running>);

impl Drop for Vault {
    fn drop(&mut self) {
        if let Some(server) = self.0.take() {
            server.stop();
        }
    }
}

fn publish_fleet(issuer: &Machine, code: &str, root: &Path) {
    client::admin_seed_swarm_roster(&issuer.vault, &[]).unwrap();
    client::seed_vault_genesis(&issuer.vault, &root.join("genesis")).unwrap();
    let node = node_identity(&issuer.state).unwrap();
    assert!(ensure_local_node_key_in_log(&issuer.vault, &node, None));
    client::post_records_to_servers(&issuer.vault).unwrap();
    client::admin_mirror_log(&issuer.vault, &[]).unwrap();
    let admin = client::admin_open(&issuer.vault).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        client::FLEET_CODE_ENTRY,
        code.as_bytes(),
    )
    .unwrap();
    doc.set_entry(&Audience::Everyone, "fixture-note", b"fresh enrollment")
        .unwrap();
    client::admin_publish(&issuer.vault, &admin, &doc).unwrap();
}

fn enroll(issuer: &Machine, newcomer: &Machine, source: String) {
    let joined = client::join(
        &newcomer.vault,
        &JoinOptions {
            source,
            host: Some("m2".to_owned()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| panic!("a join with no terminal asks nobody here"),
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    assert!(matches!(joined, JoinOutcome::Pending { .. }));
    assert!(client::test_alab_fleet(&newcomer.vault).unwrap().is_none());
    let waiting = client::requests(&issuer.vault, None).unwrap();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(
        client::approve_request(&issuer.vault, &waiting[0].recipient).unwrap(),
        "m2"
    );
    client::sync(&newcomer.vault, None).unwrap();
    assert!(matches!(
        client::status(&newcomer.vault).unwrap().membership,
        Membership::In { .. }
    ));

    // The automatic Commons enrollment also proves possession; a vault recipient alone is not
    // substituted for a bound log member. These are the same operations the ACE1 exchange uses.
    let owner = Bootstrap::required(&issuer.vault).unwrap();
    let peer = Bootstrap::required(&newcomer.vault).unwrap();
    let request = peer.request().unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
    let answer = peer.device().answer(&request, &challenge).unwrap();
    client::enroll_commons_member_in_log(
        &issuer.vault,
        owner.trust(),
        request,
        pending.verify(&answer).unwrap(),
    )
    .unwrap();
    client::post_records_to_servers(&issuer.vault).unwrap();
    client::admin_mirror_log(&issuer.vault, &[]).unwrap();
    client::sync(&newcomer.vault, None).unwrap();
}

fn target(machine: &Machine, node: &NodeIdentity) -> Target {
    Target {
        host: machine.addr.ip().to_string(),
        addr: Some(machine.addr.ip()),
        port: machine.addr.port(),
        node: Some(node.node().to_owned()),
        key: Some(node.key()),
    }
}

#[test]
fn a_fresh_vault_member_joins_the_default_fleet_and_can_be_driven() {
    // Both node jobs read their scratch homes' checkouts, this process's one last read: without
    // the lock, a read one starts makes another test's own read return unstarted, then replaces
    // the read that test waits for.
    let _one = aterm_agent::harness::repo_work::home::one_test_at_a_time();
    // The vault server this thread reads is the test's own on loopback, which answers.
    let _answering = client::ServersAnswering::within(aterm_spec::wait::MUST_HAPPEN);
    let root = aterm_tempfile::tempdir().unwrap();
    let (issuer, issuer_listener) = Machine::new(root.path(), "issuer");
    let (newcomer, newcomer_listener) = Machine::new(root.path(), "newcomer");
    let issuer_id = seed_id(&issuer.state);
    let code_secret = [0x5a; 20];
    let secret = FleetSecret::new(&code_secret).unwrap();
    write_held(&issuer.state, Began::Here, |book| {
        book.write_key(&code_secret).unwrap();
    })
    .unwrap();
    let code = JoinCode {
        secret: code_secret,
        hint: Some((issuer.addr.ip().to_string(), issuer.addr.port())),
    }
    .written();
    aterm_uds::private_file::write_private(
        &issuer.state,
        aterm_net::fleet_member::CODE_FILE,
        code.as_bytes(),
    )
    .unwrap();
    crate::test_scratch::write_config(
        &issuer.config,
        format!(
            "[fleet]\nlisten = \"{}\"\nallow = \"drive\"\ndrive_from = [\"*\"]\n",
            issuer.addr
        ),
    );
    let server = aterm_secrets::server::start_with_join_change_gap(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("vault-server"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(100),
            keep_awake: false,
        },
        aterm_secrets::server::JOIN_CHANGE_GAP,
    )
    .unwrap();
    let url = format!("http://{}/v1/test", server.addr);
    let _vault = Vault(Some(server));
    let created = client::create(&issuer.vault, &url, None, Some("m1")).unwrap();
    publish_fleet(&issuer, &code, root.path());
    let issuer_node = Node::start(&issuer, issuer_listener);
    let newcomer_node = Node::start(&newcomer, newcomer_listener);
    assert!(client::config(&newcomer.vault).unwrap().is_none());
    assert!(
        aterm_net::fleet_member::read_node_identity(&newcomer.state)
            .unwrap()
            .is_none()
    );
    enroll(&issuer, &newcomer, format!("{url}#{}", created.signer));

    assert!(matches!(
        test_sync_from_vault_at(
            &newcomer.state,
            &newcomer.config,
            &newcomer.vault,
            newcomer.addr
        )
        .unwrap(),
        FleetSync::Joined(_)
    ));
    let newcomer_id = aterm_net::fleet_member::read_node_identity(&newcomer.state)
        .unwrap()
        .unwrap();
    assert_eq!(read_key_file(&newcomer.state).unwrap(), secret);
    let keys = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the node generation binds its key in the real membership log",
        || {
            newcomer_node
                .log
                .keys()
                .filter(|keys| keys.admits(&newcomer_id.key()))
        },
    );
    assert!(keys.admits(&issuer_id.key()));
    let target = target(&newcomer, &newcomer_id);
    let dial = || {
        drive(
            &target,
            &Keyring::read(&issuer.state),
            (&secret, &issuer_id),
            (Instant::now() + DIAL_WITHIN, DIAL_WITHIN),
        )
    };
    let mut driven = dial().expect("the default join must yield usable authenticated control");
    assert_eq!(driven.node, newcomer_id.node());
    driven.write_all(b"spawn agent=claude len=11\n").unwrap();
    driven.flush().unwrap();
    assert_eq!(
        read_line_raw(&mut driven, 4096).unwrap().unwrap(),
        "GO 11\n"
    );
    driven.write_all(b"hello world").unwrap();
    driven.flush().unwrap();
    assert_eq!(read_line_raw(&mut driven, 4096).unwrap().unwrap(), "OK 1\n");
    let result = read_line_raw(&mut driven, 4096).unwrap().unwrap();
    assert!(
        result.contains("sid=s-woken") && result.contains(issuer_id.node()),
        "{result}"
    );
    drop(driven);

    // Mutate the actual persisted producer output, then ask the same live consumer again.
    // No textual grant assertion can fail before the real handshake establishes this boundary.
    let mut config: aterm_toml::edit::DocumentMut = std::fs::read_to_string(&newcomer.config)
        .unwrap()
        .parse()
        .unwrap();
    config
        .as_table_mut()
        .get_mut("fleet")
        .unwrap()
        .as_table_like_mut()
        .unwrap()
        .insert(
            "drive_from",
            aterm_toml::edit::Item::Value(aterm_toml::edit::Array::new().into()),
        );
    crate::test_scratch::write_config(&newcomer.config, config.to_string());
    assert!(matches!(
        dial(),
        Err(Unreached::Handshake(_, FleetError::UnknownOp(_)))
    ));
    newcomer_node.finish();
    issuer_node.finish();
}
