// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real runtime processes enroll, repair external writes and transfer custody.
//! Only generated identities and isolated loopback state are used.

use super::{
    Network, Runtime, Visibility,
    auto_sync_tests::{Attachment, wait_network},
};
use crate::{
    accounts::proposals::Proposals,
    catalog::{Descriptor, Directory},
    database::Change,
    network_state,
    table::{Scope, Session, store::Store},
};
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::{
    client::Paths,
    commons::{bootstrap::Bootstrap, roster::Role},
    fsx,
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const HANG: Duration = Duration::from_secs(60);

#[test]
#[ignore = "child process fixture for the separate_processes tests"]
fn child_runtime() {
    let home = std::env::var_os("ATERM_TEST_COMMONS_HOME").expect("fixture home");
    let config = std::env::var_os("ATERM_TEST_COMMONS_CONFIG").expect("fixture config");
    let port = std::env::var("ATERM_TEST_COMMONS_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let owner = std::env::var("ATERM_TEST_COMMONS_OWNER")
        .ok()
        .map(|pid| pid.parse::<u32>().unwrap());
    let match_table = std::env::var("ATERM_TEST_COMMONS_MATCH_TABLE").ok();
    let matched_file = std::env::var_os("ATERM_TEST_COMMONS_MATCHED_FILE");
    let reported = AtomicBool::new(false);
    let opens = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::start_on_with_reports(
        Paths::for_home(Path::new(&home)),
        config.into(),
        |_| {},
        Network {
            isolated: Some(Attachment::new(port)),
            opens: Some(opens.clone()),
            ..Network::default()
        },
        move |round| {
            if let (Some(table), Some(file), Some(witness)) =
                (&match_table, &matched_file, round.matching_inventory())
                && aterm_codec::hex::encode_lower(&witness.scope().digest()) == *table
                && !reported.swap(true, Ordering::AcqRel)
            {
                // Retain this event independently of later rounds, including ones
                // for other tables. The parent only observes this test-owned file.
                let proof = format!("{} {table}", std::process::id());
                assert!(fsx::write_new(Path::new(file), proof.as_bytes(), 0o600).unwrap());
            }
        },
    )
    .unwrap();
    wait_network(
        &runtime.network_observation(),
        "child runtime ready",
        |state| {
            state.visibility == Visibility::Active
                && state.observer.is_some()
                && state.via_local_process == owner
        },
    );
    let opened = opens.load(Ordering::Acquire);
    if owner.is_some() {
        assert_eq!(
            opened, 0,
            "a follower must relay without opening a transport"
        );
    } else {
        assert!(opened > 0, "an active owner must have opened its transport");
    }
    // Keep the readiness proof on its own line even after libtest's single-thread prefix.
    println!("\ncommons-runtime-ready");
    std::io::stdout().flush().unwrap();
    if std::io::stdin().read(&mut [0; 1]).unwrap() == 1 {
        aterm_uds::process::kill_self_now();
    }
    runtime.stop();
}

struct Process(aterm_uds::spawnfd::Child);

impl Process {
    fn start(paths: &Paths, config: &Path, port: u16, owner: Option<u32>) -> Self {
        Self::start_with_match(paths, config, port, owner, None)
    }

    fn start_with_match(
        paths: &Paths,
        config: &Path,
        port: u16,
        owner: Option<u32>,
        table: Option<Scope>,
    ) -> Self {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::process_sync_tests::child_runtime",
                "--ignored",
                "--nocapture",
            ])
            .env("ATERM_TEST_COMMONS_HOME", &paths.home)
            .env("ATERM_TEST_COMMONS_CONFIG", config)
            .env("ATERM_TEST_COMMONS_PORT", port.to_string())
            .env_remove("ATERM_TEST_COMMONS_OWNER")
            .env_remove("ATERM_TEST_COMMONS_MATCH_TABLE")
            .env_remove("ATERM_TEST_COMMONS_MATCHED_FILE")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::inherit());
        if let Some(owner) = owner {
            command.env("ATERM_TEST_COMMONS_OWNER", owner.to_string());
        }
        if let Some(table) = table {
            let file = config.with_extension("matched");
            assert!(!file.exists());
            command
                .env(
                    "ATERM_TEST_COMMONS_MATCH_TABLE",
                    aterm_codec::hex::encode_lower(&table.digest()),
                )
                .env("ATERM_TEST_COMMONS_MATCHED_FILE", file);
        }
        let mut process = Self(command.spawn().unwrap());
        aterm_uds::deadline::prints(&mut process.0, "commons-runtime-ready", HANG);
        assert_ne!(process.0.id(), std::process::id());
        println!(
            "commons-process ready pid={} relays={owner:?}",
            process.id()
        );
        process
    }

    fn id(&self) -> u32 {
        self.0.id()
    }

    fn matched(&self, config: &Path, table: Scope) {
        let expected = format!(
            "{} {}",
            self.id(),
            aterm_codec::hex::encode_lower(&table.digest())
        );
        let until = Instant::now() + HANG;
        loop {
            match fsx::read_private(&config.with_extension("matched"), 256).unwrap() {
                Some(proof) => {
                    assert_eq!(proof.as_slice(), expected.as_bytes());
                    return;
                }
                None => assert!(Instant::now() < until, "no matching inventory: {expected}"),
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(&mut self) -> ExitStatus {
        let until = Instant::now() + HANG;
        let mut exit = aterm_uds::deadline::ExitWait::new(&self.0);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "runtime child did not exit");
            exit.wait(until.saturating_duration_since(Instant::now()));
        }
    }

    fn crash(mut self) {
        let pid = self.id();
        self.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        // kill_self_now uses SIGKILL (9) on the supported Unix hosts.
        let signal = self.wait().signal();
        assert_eq!(signal, Some(9));
        println!("commons-process exited pid={pid} signal={signal:?}");
    }

    fn stop(mut self) {
        let pid = self.id();
        drop(self.0.stdin.take());
        assert!(self.wait().success());
        println!("commons-process exited pid={pid} success=true");
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

fn wait_owner(config: &Path, pid: u32) -> aterm_types::network_state::NetworkObservation {
    let until = Instant::now() + HANG;
    loop {
        let state = network_state::read_live_publication(config)
            .map(|publication| publication.observation().clone());
        if let Ok(state) = &state
            && state.via_local_process == Some(pid)
            && state.visibility == Visibility::Active
            && state.observer.is_some()
        {
            return state.clone();
        }
        assert!(
            Instant::now() < until,
            "process {pid} never owned repair: {state:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn separate_processes_transfer_live_repair() {
    transfer_live_repair(false);
}

#[test]
fn separate_configurations_share_live_repair_and_transfer_its_owner() {
    transfer_live_repair(true);
}

fn transfer_live_repair(separate_configs: bool) {
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-proc-")
        .tempdir_in("/tmp")
        .unwrap();
    let local = observers.path().join("local");
    let peer = observers.path().join("peer");
    for directory in [&local, &peer] {
        fsx::ensure_private_dir(directory).unwrap();
    }
    let config = local.join("aterm.toml");
    let follower_config = if separate_configs {
        let directory = observers.path().join("follower");
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    } else {
        config.clone()
    };
    let restart_config = if separate_configs {
        let directory = observers.path().join("restart");
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    } else {
        config.clone()
    };
    let proposals = Proposals::at(lab.paths[0].clone());
    let table = proposals.initialize().unwrap();
    let write = |request, key: &str| {
        proposals
            .write(
                [request; 16],
                vec![Change {
                    collection: "accounts".into(),
                    key: key.into(),
                    value: Some(aterm_json::json!({"provider":"claude","method":"oauth"})),
                }],
            )
            .unwrap()
    };
    let first = write(61, "before-processes");
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let owner = Process::start(&lab.paths[0], &config, port, None);
    let identity = wait_owner(&config, owner.id()).observer.unwrap();
    let progress = Attachment::new(port);
    let receiver = Runtime::start_on(
        lab.paths[1].clone(),
        peer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    progress.wait_for(table.digest(), 1, "peer discovered the initial table");

    let follower = Process::start(&lab.paths[0], &follower_config, port, Some(owner.id()));
    if separate_configs {
        assert!(
            !network_state::read_live_observation(&follower_config)
                .is_ok_and(|state| state.visibility == Visibility::Active),
            "a configuration relay cannot recursively forward the active transport owner"
        );
    }
    assert_eq!(
        super::read_live_publication(&lab.paths[0], &follower_config)
            .unwrap()
            .observation()
            .via_local_process,
        Some(owner.id()),
        "the CLI must read the actual owner directly, without a recursive relay"
    );
    owner.crash();
    assert_eq!(
        wait_owner(&follower_config, follower.id()).observer,
        Some(identity.clone())
    );
    let after_crash = write(62, "after-owner-crash");
    progress.wait_for(
        table.digest(),
        2,
        "peer received the successor's fresh operation",
    );

    let restarted = Process::start(&lab.paths[0], &restart_config, port, Some(follower.id()));
    follower.stop();
    assert_eq!(
        wait_owner(&restart_config, restarted.id()).observer,
        Some(identity)
    );
    let after_exit = write(63, "after-owner-exit");
    progress.wait_for(
        table.digest(),
        3,
        "peer received the next owner's fresh operation",
    );
    restarted.stop();
    receiver.stop();

    let expected = proposals.snapshot().unwrap();
    let actual = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 3);
    assert_eq!(expected.cells.len(), 3);
    assert_eq!(actual.cells, expected.cells);
    let ids = |snapshot: &crate::accounts::proposals::Snapshot| {
        snapshot
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&actual), ids(&expected));
    for receipt in [first, after_crash, after_exit] {
        assert!(
            actual
                .operations
                .iter()
                .any(|op| op.header.id == receipt.id)
        );
    }
    println!("commons-process verified operations=3 values=3");
}

type Records = BTreeMap<[u8; 32], Vec<u8>>;

fn records(paths: &Paths, table: Scope, until: Instant) -> crate::Result<Records> {
    let bootstrap = Bootstrap::required_until(paths, until).map_err(crate::table::failure)?;
    let session = Session::observe_until(&bootstrap, until)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let Some(store) = directory.observe_table_if_exists(&session, table)? else {
        return Ok(Records::new());
    };
    Ok(store
        .catalog(&session)?
        .into_iter()
        .map(|record| (record.header().id, record.payload().to_vec()))
        .collect())
}

fn wait_records(paths: &Paths, table: Scope, expected: &Records) {
    let until = Instant::now() + HANG;
    loop {
        let actual = records(paths, table, until);
        match &actual {
            Ok(actual) if actual == expected => return,
            Err(error) if !error.is_busy() => panic!("table observation failed: {error}"),
            _ => {}
        }
        assert!(Instant::now() < until, "table did not repair: {actual:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn append(paths: &Paths, descriptor: &Descriptor, declare: bool, payload: &[u8]) -> [u8; 32] {
    let until = Instant::now() + HANG;
    let (bootstrap, journal) = Bootstrap::required_with_journal_until(paths, until).unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    if declare {
        directory.publish(&session, descriptor).unwrap();
    }
    Store::open(
        &session,
        &directory.path(descriptor.scope).unwrap(),
        descriptor.scope,
    )
    .unwrap()
    .commit_catalog(&session, payload)
    .unwrap()
    .id
}

#[test]
fn separate_processes_enroll_then_repair_external_writes_and_later_tables() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    assert!(lab.bootstrap(1).witness().unwrap().is_none());
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-warm-proc-")
        .tempdir_in("/tmp")
        .unwrap();
    let configs = [0, 1].map(|n| {
        let directory = observers.path().join(n.to_string());
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    });
    let table =
        Descriptor::application(lab.bootstrap(0).trust(), "warm-processes", "items", 1).unwrap();
    let first = br#"{"key":"before-processes","quantity":1}"#;
    let first_id = append(&lab.paths[0], &table, true, first);
    let mut expected = Records::from([(first_id, first.to_vec())]);
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let source = Process::start_with_match(
        &lab.paths[0],
        &configs[0],
        reservation.port(),
        None,
        Some(table.scope),
    );
    let recipient = Process::start_with_match(
        &lab.paths[1],
        &configs[1],
        reservation.port(),
        None,
        Some(table.scope),
    );
    let identities = [
        wait_owner(&configs[0], source.id()).observer,
        wait_owner(&configs[1], recipient.id()).observer,
    ];
    source.matched(&configs[0], table.scope);
    recipient.matched(&configs[1], table.scope);
    assert!(lab.bootstrap(1).witness().unwrap().is_some());
    wait_records(&lab.paths[1], table.scope, &expected);

    // Both independent processes have completed matching scans. These writes
    // originate outside them; the test neither restarts them nor clears their caches.
    let forward = br#"{"key":"source-after-warmup","quantity":2}"#;
    let forward_id = append(&lab.paths[0], &table, false, forward);
    expected.insert(forward_id, forward.to_vec());
    wait_records(&lab.paths[1], table.scope, &expected);
    let reverse = br#"{"key":"recipient-after-warmup","quantity":3}"#;
    let reverse_id = append(&lab.paths[1], &table, false, reverse);
    expected.insert(reverse_id, reverse.to_vec());
    wait_records(&lab.paths[0], table.scope, &expected);

    let later =
        Descriptor::application(lab.bootstrap(0).trust(), "warm-processes", "later", 1).unwrap();
    let later_payload = br#"{"key":"later-table","quantity":4}"#;
    let later_id = append(&lab.paths[0], &later, true, later_payload);
    let later_expected = Records::from([(later_id, later_payload.to_vec())]);
    wait_records(&lab.paths[1], later.scope, &later_expected);
    for (config, (pid, identity)) in configs.iter().zip([
        (source.id(), &identities[0]),
        (recipient.id(), &identities[1]),
    ]) {
        assert_eq!(&wait_owner(config, pid).observer, identity);
    }
    recipient.stop();
    source.stop();

    // Reopen only after both transport owners stop, then compare every exact
    // retained ID and decrypted value. A contact or receipt count is not enough.
    for paths in &lab.paths {
        let until = Instant::now() + HANG;
        assert_eq!(records(paths, table.scope, until).unwrap(), expected);
        assert_eq!(records(paths, later.scope, until).unwrap(), later_expected);
    }
    println!("commons-process verified enrollment, warm bidirectional writes and later table");
}
