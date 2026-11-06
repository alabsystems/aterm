// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault pass in a windowless node service (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13,
//! §11 B10): nothing here runs a subscriber, a window or a listener, and the LAN stays closed.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use aterm_secrets::age::Recipient;
use aterm_secrets::client::{
    JoinOptions, JoinPassword, NodePassPace, Paths, admin_cached, admin_commons_init, admin_open,
    admin_publish_approved, admit_member_in_log, configure_log_member, create, join, load_records,
    machine_identity, node_pass_receipt, remove_member_in_log, save_records, sync,
    test_put_everyone, test_sign_genesis,
};
use aterm_secrets::commons::binding::Pending;
use aterm_secrets::commons::bootstrap::{Bootstrap, Device};
use aterm_secrets::commons::journal::Journal;
use aterm_secrets::membership_log::{Body, Entrant, EpochAudience, Evidence, encode_records};

use super::super::{sharing_held, spawn_node_vault_pass};
use super::{NodeMesh, NodeServeConfig, idle_launcher, run_node_serve_with};

/// Far enough past any first sight for every rank's step and the settle grace.
const PAST_EVERY_RANK: u64 = 1_000 * 60_000;

/// Two members of a vault at a path. `m2` holds the keys and took m5's `Admit` and none of its
/// `Wrap`s: a gap, on a Mac with no record of who else might close it.
struct Lab {
    m1: Paths,
    m2: Paths,
    m5: Recipient,
}

fn a_gap_on_m2(root: &Path) -> Lab {
    let vault = root.join("lab.vault");
    let m1 = Paths::for_home(&root.join("m1"));
    let created = create(&m1, vault.to_str().unwrap(), None, Some("m1")).unwrap();
    admin_commons_init(&m1).unwrap();
    let pinned = format!("{}#{}", vault.display(), created.signer);
    let m2 = Paths::for_home(&root.join("m2"));
    let m3 = Paths::for_home(&root.join("m3"));
    let keys = [&m2, &m3].map(|paths| machine_identity(paths).unwrap().recipient().to_bech32());
    {
        let admin = admin_open(&m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &keys[0]).unwrap();
        doc.add_machine("m3", &keys[1]).unwrap();
        admin_publish_approved(&m1, &admin, &doc, &keys).unwrap();
    }
    for (paths, name) in [(&m2, "m2"), (&m3, "m3")] {
        join(
            paths,
            &JoinOptions {
                source: pinned.clone(),
                host: Some(name.to_owned()),
                interactive: false,
                wait: Duration::from_secs(2),
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        sync(paths, None).unwrap();
        let owner = Bootstrap::required(&m1).unwrap();
        let peer = Bootstrap::required(paths).unwrap();
        let request = peer.request().unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        let answer = peer.device().answer(&request, &challenge).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(&m1)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
            .unwrap();
    }
    sync(&m2, None).unwrap();
    test_sign_genesis(&m1, &root.join("lab.genesis"), &created.signer).unwrap();
    // A removal mints the log's first epochs, which m2 holds the keys of.
    let m3_actor = Bootstrap::required(&m3)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    remove_member_in_log(&m1, m3_actor).unwrap();
    // A version m1 writes from its seat names the log heads its entries were written at, so m2
    // reads its entries from the log from here on, as every Mac of a vault in the log does.
    {
        let admin = admin_open(&m1).unwrap();
        let doc = admin.doc.clone();
        admin_publish_approved(&m1, &admin, &doc, std::slice::from_ref(&keys[0])).unwrap();
    }
    save_records(&m2, &load_records(&m1).unwrap().encode()).unwrap();
    sync(&m2, None).unwrap();

    let m5 = Paths::for_home(&root.join("m5"));
    configure_log_member(&m5, &pinned, "m5").unwrap();
    let recipient = machine_identity(&m5).unwrap().recipient();
    let trust = Bootstrap::required(&m1).unwrap().trust();
    let request = Device::open(&m5)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let minted = admit_member_in_log(
        &m1,
        Entrant {
            request,
            node_key: [0x55; 32],
            name: "m5".to_owned(),
            label: "m5".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    let no_wrap = minted
        .iter()
        .filter(|record| !matches!(record.body(), Body::Wrap { .. }));
    save_records(&m2, &encode_records(no_wrap)).unwrap();
    let lab = Lab {
        m1,
        m2,
        m5: recipient,
    };
    assert!(!lab.healed(), "the fixture leaves a gap");
    lab
}

impl Lab {
    fn healed(&self) -> bool {
        load_records(&self.m2)
            .unwrap()
            .log()
            .unwrap()
            .wraps_maximal_audience_to(&EpochAudience::Everyone, &self.m5)
    }

    /// A record of an entry signed on m1 reaches m2 by no path the pass knows of.
    fn an_entry_reaches_m2(&self) {
        test_put_everyone(&self.m1, "later", b"later-v1").unwrap();
        save_records(&self.m2, &load_records(&self.m1).unwrap().encode()).unwrap();
    }

    fn later(&self) -> std::path::PathBuf {
        self.m2.home.join(".secrets/later")
    }
}

/// `m2`'s node service, no window and the LAN closed, with the mesh `on` and its pass's clock.
fn config_of_m2(
    state: &Path,
    relay: &Path,
    lab: &Lab,
    on: bool,
    clock: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
) -> NodeServeConfig {
    let now = Arc::clone(clock);
    NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: relay.to_path_buf(),
        home_dir: None,
        config_path: None,
        vault_paths: Some(lab.m2.clone()),
        prebound_listener: None,
        tick_every: Duration::from_millis(20),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || Some("no root LAN broker answers".to_owned()),
        stop: Arc::clone(stop),
        members: None,
        alab_browse: None,
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh {
            on,
            pass_now_ms: Arc::new(move || now.load(Ordering::Acquire)),
            pass_pace: NodePassPace {
                look: Duration::from_millis(1),
                ..NodePassPace::LIVE
            },
            pass_converge: Arc::new(|| None),
            bus: None,
            ..NodeMesh::live()
        },
    }
}

#[test]
fn a_windowless_node_heals_a_gap_and_materializes_with_no_subscriber() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, relay) = (root.path().join("state"), root.path().join("relay"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&relay).unwrap();
    let lab = a_gap_on_m2(root.path());
    let start = aterm_time::unix_millis().unwrap();
    let clock = Arc::new(AtomicU64::new(start));
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = config_of_m2(&state, &relay, &lab, true, &clock, &stop);
    let node = std::thread::spawn(move || run_node_serve_with(cfg));

    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the node's first pass sees the gap",
        || node_pass_receipt(&lab.m2).map(|_| ()),
    );
    clock.store(start + PAST_EVERY_RANK, Ordering::Release);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a timer's pass heals the gap",
        || lab.healed().then_some(()),
    );
    lab.an_entry_reaches_m2();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a new record's pass writes its file under ~/.secrets",
        || (std::fs::read(lab.later()).ok()? == b"later-v1").then_some(()),
    );
    stop.store(true, Ordering::Release);
    assert_eq!(node.join().unwrap(), ExitCode::SUCCESS);
}

/// The negative control: with the pass off (the mesh off, as this build's switch has it until R2)
/// no thread starts, so nothing on this Mac closes the gap or writes the file.
#[test]
fn with_the_mesh_off_the_node_runs_no_pass_and_the_gap_stays() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, relay) = (root.path().join("state"), root.path().join("relay"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&relay).unwrap();
    let lab = a_gap_on_m2(root.path());
    let clock = Arc::new(AtomicU64::new(
        aterm_time::unix_millis().unwrap() + PAST_EVERY_RANK,
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = config_of_m2(&state, &relay, &lab, false, &clock, &stop);
    assert!(spawn_node_vault_pass(&cfg).is_none());
    assert!(!lab.healed());
    assert_eq!(node_pass_receipt(&lab.m2), None);
}

/// The node's switches hold the pass as they hold its LAN uses: `[swarm] enabled = false` and a
/// Mac that left the fleet each do, and the pass runs once `[swarm]` lets it.
#[test]
fn the_nodes_switches_hold_the_pass_until_sharing_is_on() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (state, relay) = (root.path().join("state"), root.path().join("relay"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&relay).unwrap();
    let toml = root.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&toml, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    assert!(sharing_held(&state, Some(&toml)));

    let lab = a_gap_on_m2(root.path());
    let clock = Arc::new(AtomicU64::new(aterm_time::unix_millis().unwrap()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut cfg = config_of_m2(&state, &relay, &lab, true, &clock, &stop);
    cfg.config_path = Some(toml.clone());
    let pass = spawn_node_vault_pass(&cfg).expect("the pass starts, held");
    assert_eq!(node_pass_receipt(&lab.m2), None);

    aterm_uds::atomic_file::write(&toml, b"[swarm]\nenabled = true\n", 0o600).unwrap();
    assert!(!sharing_held(&state, Some(&toml)));
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the pass runs once sharing is on",
        || node_pass_receipt(&lab.m2).map(|_| ()),
    );
    stop.store(true, Ordering::Release);
    pass.join().unwrap();

    std::fs::write(state.join(aterm_net::fleet_member::OFF_FILE), b"").unwrap();
    assert!(
        sharing_held(&state, Some(&toml)),
        "a Mac that left the fleet"
    );
}

/// The node's label is the one `aterm secrets status` reads.
#[test]
fn the_label_status_reads_is_the_node_jobs() {
    assert_eq!(
        aterm_secrets::service::NODE_LABEL,
        aterm_agent::node_service::NODE_LABEL
    );
}
