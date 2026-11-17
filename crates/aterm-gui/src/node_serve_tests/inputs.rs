// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node job's watch judges again at its inputs' file events, not on a clock (KI-1108-2).
//! Each job here runs with an old tick of an hour, so every pass a test waits on came from a
//! ring of what the test changed. Scratch state, scratch vaults and loopback sockets only: no
//! real `~/.aterm`, vault server or LAN.

use super::*;
use crate::fleet_listen::fleet_inputs::{
    first_looks_told, fleet_files, judged_by, passes, ring_node,
};
use aterm_net::fleet_member::leave_held;
use aterm_secrets::client::Paths;
use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// A tick no test outlives: a pass at it would come long after any test's hang detector.
const NO_TICK: Duration = Duration::from_secs(3600);

/// A node job serving a loopback port, its `aterm.toml` at `config`.
struct Quiet {
    addr: SocketAddr,
    config: PathBuf,
    state: PathBuf,
    stop: Arc<Stop>,
    served: Arc<AtomicUsize>,
    server: Option<std::thread::JoinHandle<ExitCode>>,
    _relay: aterm_tempfile::TempDir,
}

impl Quiet {
    /// A job on `state` serving `[fleet]` with `fleet` beside its listen, its deny list read
    /// from `vault`, its old tick `tick`, once its first generation serves and has made a pass
    /// past the rings of its watch's first looks, so a pass a test counts is its own change's.
    fn start(root: &Path, state: &Path, fleet: &str, vault: Option<Paths>, tick: Duration) -> Self {
        let relay = crate::test_scratch::short_dir("node-inputs");
        let prebound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind node job");
        let addr = prebound.local_addr().unwrap();
        let config = root.join("aterm.toml");
        crate::test_scratch::write_config(&config, table(addr, fleet));
        let stop = Arc::new(Stop::new());
        let served = Arc::new(AtomicUsize::new(0));
        let told = Arc::clone(&served);
        let cfg = NodeServeConfig {
            state_dir: state.to_path_buf(),
            relay_dir: relay.path().to_path_buf(),
            home_dir: None,
            config_path: Some(config.clone()),
            vault_paths: vault,
            members: None,
            alab_browse: None,
            prebound_listener: Some(prebound),
            on_generation: Some(Arc::new(move |_: &str| {
                told.fetch_add(1, Ordering::AcqRel);
            })),
            tick_every: tick,
            wake_timeout: Duration::from_secs(1),
            lan_unavailable: || None,
            stop: Arc::clone(&stop),
            launcher: idle_launcher(),
            updates: None,
            mesh: NodeMesh::off(),
        };
        let quiet = Self {
            addr,
            config,
            state: state.to_path_buf(),
            stop,
            served,
            server: Some(std::thread::spawn(move || run_node_serve_with(cfg))),
            _relay: relay,
        };
        quiet.until_served(1);
        until_within(MUST_HAPPEN, "the job's first looks are told", || {
            first_looks_told(&quiet.state).then_some(())
        });
        quiet.one_pass();
        quiet
    }

    /// Rewrite `[fleet]` with `fleet` beside its listen.
    fn configure(&self, fleet: &str) {
        crate::test_scratch::write_config(&self.config, table(self.addr, fleet));
    }

    /// How many generations started serving.
    fn served(&self) -> usize {
        self.served.load(Ordering::Acquire)
    }

    fn until_served(&self, generations: usize) {
        until_within(MUST_HAPPEN, "a generation serves", || {
            (self.served() >= generations).then_some(())
        });
    }

    /// Once the watch's pass count is past `than`: the count then.
    fn passed(&self, than: usize) -> usize {
        until_within(MUST_HAPPEN, "a pass of the job's watch", || {
            let now = passes(&self.state);
            (now > than).then_some(now)
        })
    }

    /// One pass made at a ring of this job's inputs alone, with nothing changed.
    fn one_pass(&self) -> usize {
        let before = passes(&self.state);
        ring_node(&self.state);
        self.passed(before)
    }

    fn finish(mut self) {
        self.stop.set();
        let server = self.server.take().expect("the job runs");
        assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

/// `[fleet]` listening at `addr`, with `fleet` beside it.
fn table(addr: SocketAddr, fleet: &str) -> String {
    format!("[fleet]\nlisten = \"{addr}\"\n{fleet}")
}

/// A ROTATED KEY ENDS THE GENERATION AT ITS EVENT: the watch hears `fleet.key` rewritten and ends
/// the generation serving the old key, and the next serves the new one.
/// NEGATIVE CONTROL: the same key written again wakes a pass, which ends nothing.
#[test]
fn a_rotated_key_ends_the_generation_at_its_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let node = Quiet::start(root.path(), &state, "allow = \"facts\"\n", None, NO_TICK);
    let before = passes(&state);
    write_held(&state, Began::Here, |book| {
        book.write_key(&SECRET_BYTES).expect("the same key");
    })
    .expect("write_held");
    node.passed(before);
    assert_eq!(node.served(), 1, "CONTROL: the same key ends nothing");
    write_held(&state, Began::Here, |book| {
        book.write_rotated_key(&[0x6b; 24]).expect("a rotated key");
    })
    .expect("write_held");
    node.until_served(2);
    node.finish();
}

/// LEAVING THE FLEET GIVES THE PORT BACK AT ITS EVENT: the leave removes `fleet.key` and writes
/// `fleet-off`, and the job lets `node-port.lock` go.
/// NEGATIVE CONTROL: while it serves, the job holds the port.
#[test]
fn leaving_the_fleet_gives_the_port_back_at_its_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let node = Quiet::start(root.path(), &state, "allow = \"facts\"\n", None, NO_TICK);
    assert!(
        node_port_holds_lock(&state),
        "CONTROL: a serving job holds the port"
    );
    leave_held(&state, |leaving| {
        leaving
            .remove(aterm_net::fleet::KEY_FILE)
            .expect("remove the key");
        leaving.write_off().expect("mark the leave");
    })
    .expect("leave");
    until_within(MUST_HAPPEN, "the port given back", || {
        (!node_port_holds_lock(&state)).then_some(())
    });
    node.finish();
}

/// A LEVEL TAKEN BACK CUTS AN OPEN CONTENT READ AT ITS EVENT: `aterm.toml` lowered to `facts`
/// cuts a member's open content survey, whose next read is its end.
/// NEGATIVE CONTROL: the same table written again wakes a pass, and the survey still answers.
#[test]
fn a_level_taken_back_cuts_an_open_content_read_at_its_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let (secret, _) = seed_fleet_node(&state);
    let driver = seed_id(&root.path().join("driver-state"));
    let _driver_at = seed_joined_driver(&state, &driver);
    let node = Quiet::start(root.path(), &state, "allow = \"content\"\n", None, NO_TICK);
    let tcp = TcpStream::connect(node.addr).expect("connect");
    let mut dialed = client_handshake_as(tcp, &secret, Some(&driver), FleetOp::Survey, DIAL_WITHIN)
        .expect("a content survey");
    let stream = dialed.transport.stream();
    stream
        .sock
        .set_read_timeout(Some(MUST_HAPPEN))
        .expect("bound the reads by the hang detector");
    let mut version = || {
        stream.write_all(b"version\n").expect("ask");
        stream.flush().expect("ask");
        read_line_raw(stream, 4096).expect("read").expect("a line")
    };
    assert!(version().starts_with("OK version="), "the survey answers");
    let before = passes(&state);
    node.configure("allow = \"content\"\n");
    node.passed(before);
    assert!(
        version().starts_with("OK version="),
        "CONTROL: the same table cuts nothing"
    );
    node.configure("allow = \"facts\"\n");
    let mut buf = [0u8; 64];
    let read = stream.read(&mut buf);
    assert!(
        match &read {
            Ok(0) => true,
            Ok(_) => false,
            Err(error) => !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
        },
        "a cut read ends: {read:?}"
    );
    node.finish();
}

/// A NEW TAKE OF THE FLEET ENDS THE GENERATION AT ITS EVENT: the generation that served under
/// the take the fleet state recorded ends once another take is recorded, and the next serves
/// under the new one.
/// NEGATIVE CONTROL: a pass with no new take ends nothing.
#[test]
fn a_new_take_ends_the_generation_at_its_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let vault = Paths::for_home(&root.path().join("home"));
    aterm_secrets::fsx::ensure_private_dir(&vault.dir).expect("vault dir");
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).expect("take");
    let node = Quiet::start(
        root.path(),
        &state,
        "allow = \"facts\"\n",
        Some(vault.clone()),
        NO_TICK,
    );
    node.one_pass();
    assert_eq!(node.served(), 1, "CONTROL: a pass with no new take");
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).expect("take again");
    node.until_served(2);
    node.finish();
}

/// `paths`' vault, made here, publishing a roster that denies `denied`.
fn deny(paths: &Paths, denied: &[&str]) {
    let admin = aterm_secrets::client::admin_open(paths).expect("admin");
    let mut doc = admin.doc.clone();
    let names: Vec<String> = denied.iter().map(|node| format!("\"{node}\"")).collect();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::client::SWARM_ROSTER_ENTRY,
        format!("deny_nodes = [{}]\n", names.join(", ")).as_bytes(),
    )
    .expect("the roster");
    aterm_secrets::client::admin_publish(paths, &admin, &doc).expect("publish");
}

/// A DENIAL IS JUDGED AT ITS EVENT: a deny list the vault publishes wakes the watch, whose pass
/// judges the job's open reads and drives by it.
/// NEGATIVE CONTROL: the pass before the publish judged by a list that names no one.
#[test]
fn a_published_denial_is_judged_at_its_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let vault = Paths::for_home(&root.path().join("home"));
    let source = root.path().join("fixture.vault");
    aterm_secrets::client::create(&vault, source.to_str().unwrap(), None, Some("m61"))
        .expect("a scratch vault");
    deny(&vault, &[]);
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).expect("take");
    let node = Quiet::start(
        root.path(),
        &state,
        "allow = \"facts\"\n",
        Some(vault.clone()),
        NO_TICK,
    );
    node.one_pass();
    assert_eq!(
        judged_by(&state),
        Some(Some(Vec::new())),
        "CONTROL: judged by a list that names no one"
    );
    let blocked = "9a".repeat(16);
    deny(&vault, &[&blocked]);
    until_within(MUST_HAPPEN, "a pass judged by the denial", || {
        (judged_by(&state) == Some(Some(vec![blocked.clone()]))).then_some(())
    });
    node.finish();
}

/// What a look at each of `files`, and at the entries of `relay_dir`, finds: the inputs whose
/// change wakes a node job.
fn stamps(files: &[PathBuf], relay_dir: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut found: Vec<String> = files
        .iter()
        .map(|file| match std::fs::symlink_metadata(file) {
            Ok(meta) => format!(
                "{} {} {} {} {}",
                file.display(),
                meta.ino(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec()
            ),
            Err(error) => format!("{} {:?}", file.display(), error.kind()),
        })
        .collect();
    let mut entries: Vec<String> = std::fs::read_dir(relay_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    found.extend(entries);
    found
}

/// A PASS WAKES NO PASS: past the job's first passes (its first heartbeat round, with no member
/// in its book to dial), a pass writes none of the files whose change wakes the job, so it does
/// not wake itself.
/// NEGATIVE CONTROL: a change to one of those files is seen by the same look.
#[test]
fn a_pass_writes_none_of_the_files_that_wake_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let node = Quiet::start(root.path(), &state, "allow = \"facts\"\n", None, NO_TICK);
    let watched = fleet_files(&state, Some(&node.config), None);
    let relay_dir = node._relay.path().to_path_buf();
    node.one_pass();
    node.one_pass();
    let before = stamps(&watched, &relay_dir);
    node.one_pass();
    assert_eq!(
        stamps(&watched, &relay_dir),
        before,
        "a pass leaves every input as it found it"
    );
    node.configure("allow = \"content\"\n");
    assert_ne!(
        stamps(&watched, &relay_dir),
        before,
        "CONTROL: the look sees a change"
    );
    node.finish();
}

/// More passes than the rings a job's start still owes once its first looks are told can make:
/// its first heartbeat round's end, and that round's write of the book.
const PAST_THE_START: usize = 8;

/// A VAULT WRITER'S HOLD KEEPS THE OLD TICK: while a writer holds the vault's state lock, which
/// it lets go of with no file event, passes come on the old tick with nothing rung, so the
/// reads it held are made again once it lets go. A job whose watch rang for nothing else would
/// make no more passes than its start still owes within the test's hang detector: its duties are
/// a minute and more away.
/// NEGATIVE CONTROL (2026-11-14): the pace without the writer's hold and the unsettled deny read
/// (`paced` with neither `writer` nor `!deny_settled`) fails this at its hang detector.
#[test]
fn a_vault_writers_hold_keeps_the_old_tick() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let vault = Paths::for_home(&root.path().join("home"));
    let source = root.path().join("fixture.vault");
    aterm_secrets::client::create(&vault, source.to_str().unwrap(), None, Some("m62"))
        .expect("a scratch vault");
    deny(&vault, &[]);
    aterm_agent::fleet_vault::take_fixture(&state, Some(&vault)).expect("take");
    let node = Quiet::start(
        root.path(),
        &state,
        "allow = \"facts\"\n",
        Some(vault.clone()),
        Duration::from_millis(50),
    );
    let writer = aterm_secrets::client::test_hold_state_lock(&vault).expect("hold the state lock");
    let rung = node.one_pass();
    node.passed(rung + PAST_THE_START);
    drop(writer);
    node.finish();
}
