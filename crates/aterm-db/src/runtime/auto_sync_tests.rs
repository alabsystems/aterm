// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic owners on isolated native loopback multicast. Generated vaults and
//! admitted devices exercise startup, discovery and repair without live lab data.

use super::*;
use crate::{accounts::proposals::Proposals, database::Change};
use astream_wire::sync::Digest;
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use aterm_types::network_state::NetworkObservation;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

#[derive(Clone)]
pub(super) struct Attachment {
    pub(super) port: u16,
    pub(super) lan_allowed: Arc<std::sync::atomic::AtomicBool>,
    progress: Arc<(Mutex<BTreeMap<Digest, usize>>, Condvar)>,
}
impl Attachment {
    pub(super) fn new(port: u16) -> Self {
        Self {
            port,
            lan_allowed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            progress: Arc::new((Mutex::new(BTreeMap::new()), Condvar::new())),
        }
    }

    pub(super) fn observe(&self, observed: &BTreeMap<Digest, usize>) {
        let (state_mu, changed) = &*self.progress;
        let mut state = state_mu.lock().unwrap_or_else(|p| p.into_inner());
        if *state != *observed {
            state.clone_from(observed);
            changed.notify_all();
        }
    }

    pub(super) fn wait_for(&self, table: Digest, operations: usize, phase: &str) {
        let state = self.wait_for_observation(table, operations);
        assert!(
            state.get(&table).copied().unwrap_or(0) >= operations,
            "{phase}: automatic repair did not retain {operations} operations; observed {state:?}"
        );
    }

    fn wait_for_observation(&self, table: Digest, operations: usize) -> BTreeMap<Digest, usize> {
        let (state_mu, changed) = &*self.progress;
        let mut state = state_mu.lock().unwrap_or_else(|p| p.into_inner());
        let mut high_water = state.get(&table).copied().unwrap_or(0);
        let mut until = Instant::now() + Duration::from_secs(60);
        while state.get(&table).copied().unwrap_or(0) < operations {
            let retained = state.get(&table).copied().unwrap_or(0);
            if retained > high_water {
                high_water = retained;
                until = Instant::now() + Duration::from_secs(60);
            }
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, _) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner());
            state = next;
        }
        // Release the observer's lock before any caller's assertion can unwind.
        // Only new high-water progress on this table extends the hang detector.
        state.clone()
    }
}

fn start(
    lab: &crate::table::tests::Lab,
    observers: &Path,
    machine: usize,
    port: u16,
) -> (Runtime, Attachment) {
    let attachment = Attachment::new(port);
    let observer = observers.join(machine.to_string());
    aterm_secrets::fsx::ensure_private_dir(&observer).unwrap();
    let runtime = Runtime::start_on(
        lab.paths[machine].clone(),
        observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    (runtime, attachment)
}

#[test]
fn a_later_runtime_discovers_the_request_and_the_custodian_takes_a_holders_value_automatically() {
    use crate::credentials;
    use aterm_secrets::{
        fsx,
        need::{self, Purpose, Want},
        text::Audience,
    };
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (custodian, _) = start(&lab, observers.path(), 0, port);
    let (holder, holder_attachment) = start(&lab, observers.path(), 1, port);

    let requester_paths = &lab.paths[2];
    need::raise(
        requester_paths,
        Want::Mailbox("automatic".into()),
        Purpose::ByHand,
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    let scope = crate::catalog::Descriptor::application(
        lab.bootstrap(2).trust(),
        "credentials",
        "requests",
        1,
    )
    .unwrap()
    .scope
    .digest();
    // The requester joins the running owners through discovery; no endpoint/address is supplied.
    let (requester, requester_attachment) = start(&lab, observers.path(), 2, port);
    holder_attachment.wait_for(scope, 1, "a new runtime's request reaches a running holder");
    let source = lab.paths[1].home.join(".secrets");
    fsx::ensure_private_dir(&source).unwrap();
    fsx::write_atomic(
        &source.join("automatic_password"),
        b"automatic-scratch-password",
        0o600,
    )
    .unwrap();
    // This is sync.requests' local holder work. It does not open a connection.
    assert_eq!(credentials::pass(&lab.paths[1]).unwrap().offered, 1);
    requester_attachment.wait_for(
        scope,
        4,
        "an automatic runtime handoff retains Published on the requester",
    );
    requester.stop();
    holder.stop();
    custodian.stop();
    let book = credentials::observe(requester_paths).unwrap().book;
    assert!(book.threads.values().any(
        |thread| thread.want == Want::Mailbox("automatic".into()) && thread.published.is_some()
    ));
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Hosts(vec!["m3".into()]), "automatic_password")
            .unwrap()
            .value
            .as_slice(),
        b"automatic-scratch-password"
    );
    for host in ["m1", "m2"] {
        assert!(
            admin
                .doc
                .entry(&Audience::Hosts(vec![host.into()]), "automatic_password")
                .is_none()
        );
    }
}

#[test]
fn a_request_retirement_reaches_the_holder_and_cleans_its_private_give_without_a_custodian() {
    use crate::credentials;
    use aterm_secrets::{
        SecretBytes,
        need::{self, Purpose, Want},
    };
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let requester_paths = &lab.paths[2];
    need::raise(
        requester_paths,
        Want::Mailbox("withdrawn".into()),
        Purpose::ByHand,
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    let scope = crate::catalog::Descriptor::application(
        lab.bootstrap(2).trust(),
        "credentials",
        "requests",
        1,
    )
    .unwrap()
    .scope
    .digest();
    // The custodian is offline: only the two non-admin runtimes participate.
    let (holder, holder_attachment) = start(&lab, observers.path(), 1, reservation.port());
    let (requester, requester_attachment) = start(&lab, observers.path(), 2, reservation.port());
    holder_attachment.wait_for(scope, 1, "the holder retained the request");
    credentials::give_from(&lab.paths[1], "mailbox:withdrawn", || {
        Ok(SecretBytes::from(b"scratch-withdrawn-password".as_slice()))
    })
    .unwrap();
    requester_attachment.wait_for(scope, 2, "the requester retained the holder's Offer");
    need::retire(
        requester_paths,
        "mailbox:withdrawn",
        false,
        "scratch request ended",
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    holder_attachment.wait_for(scope, 3, "the holder retained the requester's retirement");
    requester.stop();
    holder.stop();
    let retained = credentials::observe(&lab.paths[1]).unwrap();
    assert_eq!(retained.book.threads.len(), 1, "{retained:?}");
    assert!(
        retained.book.threads.values().all(|thread| thread.ended()),
        "{retained:?}"
    );
    credentials::pass(&lab.paths[1]).unwrap();
    assert!(std::fs::read_dir(&lab.paths[1].dir).unwrap().all(|file| {
        !file
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("credential-give-")
    }));
}

#[test]
fn damaged_catalog_recovers_peer_data_automatically_without_resuming_its_writer() {
    for (healthy_primary, retained_recovery) in [(false, false), (true, false), (true, true)] {
        damaged_catalog_recovery(healthy_primary, retained_recovery);
    }
}

fn damaged_catalog_recovery(healthy_primary: bool, retained_recovery: bool) {
    use crate::{
        catalog::Directory,
        table::{Session, store::Store},
    };
    let roles = vec![Role::Writer; if retained_recovery { 3 } else { 2 }];
    let lab = crate::table::tests::Lab::new(&roles);
    let source_bootstrap = lab.bootstrap(0);
    let source_session = Session::open(&source_bootstrap).unwrap();
    let source_directory = Directory::new(&lab.paths[0], source_bootstrap.trust());
    let scope = source_directory
        .open_application(&source_session, "recovery-probe", "records", 1)
        .unwrap();
    let mut source_store = Store::open(
        &source_session,
        &source_directory.path(scope).unwrap(),
        scope,
    )
    .unwrap();
    let marker = source_store
        .commit_catalog(&source_session, b"peer-data-after-catalog-loss")
        .unwrap();
    drop((source_store, source_session));

    let receiver_bootstrap = lab.bootstrap(1);
    let receiver_session = Session::open(&receiver_bootstrap).unwrap();
    let receiver_directory = Directory::new(&lab.paths[1], receiver_bootstrap.trust());
    receiver_directory.snapshot(&receiver_session).unwrap();
    let local_marker = healthy_primary.then(|| {
        assert_eq!(
            receiver_directory
                .open_application(&receiver_session, "recovery-probe", "records", 1)
                .unwrap(),
            scope
        );
        receiver_directory
            .open_store(&receiver_session, scope)
            .unwrap()
            .commit_catalog(
                &receiver_session,
                b"retained-only-on-the-recovering-machine",
            )
            .unwrap()
    });
    let recovered_marker = retained_recovery.then(|| {
        let offline = lab.bootstrap(2);
        let session = Session::open(&offline).unwrap();
        let directory = Directory::new(&lab.paths[2], offline.trust());
        assert_eq!(
            directory
                .open_application(&session, "recovery-probe", "records", 1)
                .unwrap(),
            scope
        );
        let mut store = directory.open_store(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"retained-from-an-offline-third-peer")
            .unwrap();
        let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        Store::join_recovery(
            &receiver_session,
            &receiver_directory.recovery().path(scope).unwrap(),
            scope,
        )
        .unwrap()
        .ingest(&receiver_session, &wire)
        .unwrap();
        receipt
    });
    let catalog_scope = receiver_directory.catalog_scope();
    let original_directory = receiver_directory.path(catalog_scope).unwrap();
    let original_claim_path = crate::table::store::claim_dir(&receiver_session)
        .unwrap()
        .join(crate::table::store::claim_name(catalog_scope));
    let original_claim = std::fs::read(&original_claim_path).unwrap();
    let displaced = lab._home.path().join("preserved-original-catalog");
    std::fs::rename(&original_directory, &displaced).unwrap();
    let original_log = std::fs::read(displaced.join("table.astream")).unwrap();
    assert!(
        receiver_directory
            .snapshot_existing(&receiver_session)
            .is_err()
    );
    drop(receiver_session);

    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let (source, source_attachment) = start(&lab, observers.path(), 0, reservation.port());
    let (receiver, attachment) = start(&lab, observers.path(), 1, reservation.port());
    attachment.wait_for(
        scope.digest(),
        1,
        "a recovery replica retained peer data automatically",
    );
    receiver.stop();
    let session = Session::observe(&receiver_bootstrap).unwrap();
    let store = receiver_directory.observe_table(&session, scope).unwrap();
    assert!(
        store
            .inventory(&session)
            .unwrap()
            .any(|id| *id == marker.id),
        "ordinary readers must see the newly received peer row beside healthy primary data"
    );
    drop((store, session));
    if healthy_primary {
        source_attachment.wait_for(
            scope.digest(),
            2 + usize::from(retained_recovery),
            "a healthy primary row is relayed while its catalog recovers",
        );
    }
    let source_session = Session::open(&source_bootstrap).unwrap();
    let mut source_store = Store::open(
        &source_session,
        &source_directory.path(scope).unwrap(),
        scope,
    )
    .unwrap();
    let next = source_store
        .commit_catalog(&source_session, b"after-recovery-restart")
        .unwrap();
    drop((source_store, source_session));
    let (receiver, attachment) = start(&lab, observers.path(), 1, reservation.port());
    attachment.wait_for(
        scope.digest(),
        2 + usize::from(healthy_primary) + usize::from(retained_recovery),
        "the restarted recovery replica caught up automatically",
    );
    receiver.stop();
    source.stop();

    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let observed = receiver_directory.snapshot_existing(&session).unwrap();
    assert!(observed.recovery);
    assert!(observed.entries.contains_key(&scope.digest()));
    let store = receiver_directory.observe_table(&session, scope).unwrap();
    assert_eq!(store.is_recovery(), !healthy_primary || retained_recovery);
    let records = store.catalog(&session).unwrap();
    let expected: BTreeSet<_> = [marker.id, next.id]
        .into_iter()
        .chain(local_marker.map(|receipt| receipt.id))
        .chain(recovered_marker.map(|receipt| receipt.id))
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|record| record.header().id)
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(std::fs::read(&original_claim_path).unwrap(), original_claim);
    assert_eq!(
        std::fs::read(displaced.join("table.astream")).unwrap(),
        original_log
    );
    assert!(
        !original_directory.exists(),
        "recovery cannot recreate the original writer directory"
    );
    drop((store, session));
    let session = Session::open(&bootstrap).unwrap();
    assert!(
        receiver_directory
            .open_application(&session, "recovery-probe", "records", 1)
            .is_err()
    );
    let recovery_directory = receiver_directory.recovery();
    let fenced_scope = if healthy_primary {
        catalog_scope
    } else {
        scope
    };
    let mut store = recovery_directory
        .open_store(&session, fenced_scope)
        .unwrap();
    assert!(
        store
            .commit_catalog(&session, b"never-resume-the-old-sequence")
            .is_err()
    );
}

pub(super) fn wait_network(
    handle: &ObservationHandle,
    phase: &str,
    complete: impl Fn(&NetworkObservation) -> bool,
) -> NetworkObservation {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let state = handle.snapshot();
        if complete(&state) {
            return state;
        }
        assert!(Instant::now() < until, "{phase}: {state:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn proposal(key: &str) -> Change {
    Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(aterm_json::json!({"provider":"claude","method":"oauth"})),
    }
}

#[test]
fn automatic_round_callbacks_release_control_locks_and_report_real_inventory() {
    use aterm_secrets::commons::journal::Journal;
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-callback-")
        .tempdir_in("/tmp")
        .unwrap();
    let owner_observer = observers.path().join("0");
    aterm_secrets::fsx::ensure_private_dir(&owner_observer).unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([21; 16], vec![proposal("callback-proof")])
        .unwrap();
    let bootstrap = lab.bootstrap(0);
    let journal = bootstrap.journal_dir().to_owned();
    let trust = bootstrap.trust();
    drop(bootstrap);
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (send, receive) = std::sync::mpsc::channel();
    let attachment = Attachment::new(port);
    let owner = Runtime::start_on_with_reports(
        lab.paths[0].clone(),
        owner_observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment),
            ..Network::default()
        },
        move |report| {
            // Waiting for the actual same-custody lock is a hang detector:
            // another worker may briefly own it, but this callback must not.
            let available = Journal::open_existing_until(
                &journal,
                trust,
                Instant::now() + Duration::from_secs(60),
            )
            .map(|_| ());
            let _ = send.send((report, available.map_err(|error| error.to_string())));
        },
    )
    .unwrap();
    let (receiver, progress) = start(&lab, observers.path(), 1, port);
    progress.wait_for(table.digest(), 1, "callback receiver retained data");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (report, available) = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(
            available.is_ok(),
            "callback retained a control lock: {available:?}"
        );
        if report.control.is_ok()
            && report.actor.is_some()
            && report.application.is_some_and(|app| {
                app.table == Some(table.digest())
                    && app.local_known.is_some_and(|summary| summary.count == 1)
            })
        {
            assert!(report.application_error.is_none());
            assert_eq!(report.dropped_before, 0);
            break;
        }
    }
    receiver.stop();
    owner.stop();
}

#[test]
fn automatic_runtime_discovers_tables_catches_up_late_devices_and_transfers_local_ownership() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 3]);
    // Darwin limits Unix socket paths to 103 bytes. Keep the observer sockets
    // short independently of the generated vaults' deeper scratch paths.
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-sync-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([1; 16], vec![proposal("before-start")])
        .unwrap();
    // Neither receiver opens an application table before discovery. This also
    // catches a test accidentally pre-provisioning all three local replicas.
    for path in &lab.paths[1..] {
        assert!(!path.dir.join("commons/tables").exists());
    }

    // An ephemeral receive socket reserves an isolated discovery port. Owners
    // use the native multicast/TCP/TLS path and their ordinary scheduler threads.
    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);
    let owner_state = owner.network_observation();
    let identity = wait_network(&owner_state, "first local owner", |state| {
        state.observer.is_some() && state.visibility == Visibility::Active
    })
    .observer
    .unwrap();
    let (receiver, receiver_progress) = start(&lab, observers.path(), 1, port);
    receiver_progress.wait_for(table.digest(), 1, "receiver discovered the table");

    let (follower, follower_progress) = start(&lab, observers.path(), 0, port);
    let follower_state = follower.network_observation();
    let relayed = wait_network(
        &follower_state,
        "second local runtime uses owner relay",
        |state| {
            state.via_local_process == Some(std::process::id())
                && state.visibility == Visibility::Active
                && state.observer.as_ref() == Some(&identity)
        },
    );
    assert_eq!(relayed.observer.as_ref(), Some(&identity));
    assert!(follower_progress.progress.0.lock().unwrap().is_empty());
    owner.stop();
    let taken_over = wait_network(
        &follower_state,
        "remaining runtime becomes local owner",
        |state| {
            state.via_local_process.is_none()
                && state.visibility == Visibility::Active
                && state.observer.as_ref() == Some(&identity)
        },
    );
    assert_eq!(taken_over.observer.as_ref(), Some(&identity));
    follower_progress.wait_for(table.digest(), 1, "new owner repaired the existing table");

    // This third device is already admitted, but its aterm starts for the first
    // time after publication. No receiver-side table open or manual sync occurs.
    let (late, late_progress) = start(&lab, observers.path(), 2, port);
    late_progress.wait_for(table.digest(), 1, "late device discovered existing data");
    receiver.stop();
    follower.stop();
    let offline = Proposals::at(lab.paths[1].clone());
    let conflict = |email: &str| Change {
        value: Some(aterm_json::json!({"provider":"claude","method":"oauth","email":email})),
        ..proposal("offline-conflict")
    };
    let left = source
        .write([2; 16], vec![conflict("left@example.test")])
        .unwrap();
    let right = offline
        .write([3; 16], vec![conflict("right@example.test")])
        .unwrap();
    for (db, absent) in [(&source, right.id), (&offline, left.id)] {
        let snapshot = db.snapshot().unwrap();
        assert_eq!(snapshot.operations.len(), 2);
        assert!(snapshot.operations.iter().all(|op| op.header.id != absent));
        assert_eq!(
            snapshot.cells[&("accounts".into(), "offline-conflict".into())]
                .candidates
                .len(),
            1,
            "each isolated writer sees only its own edit before reconnection"
        );
    }
    let (restarted_owner, owner_progress) = start(&lab, observers.path(), 0, port);
    late_progress.wait_for(
        table.digest(),
        2,
        "online late device received the next change",
    );
    let (restarted_receiver, restarted_progress) = start(&lab, observers.path(), 1, port);
    restarted_progress.wait_for(
        table.digest(),
        3,
        "offline writer caught up without discarding its own edit",
    );
    owner_progress.wait_for(
        table.digest(),
        3,
        "original writer received the offline edit",
    );
    late_progress.wait_for(
        table.digest(),
        3,
        "late device received both concurrent edits",
    );
    restarted_receiver.stop();
    late.stop();
    restarted_owner.stop();

    // Completed inventories are only synchronization progress. Check actual
    // application values and signed operation identity on every durable replica.
    let expected = source.snapshot().unwrap();
    assert_eq!(expected.operations.len(), 3);
    assert_eq!(expected.cells.len(), 2);
    let concurrent = &expected.cells[&("accounts".into(), "offline-conflict".into())].candidates;
    assert_eq!(
        concurrent.len(),
        2,
        "concurrent values remain an explicit conflict"
    );
    for (receipt, email) in [(left, "left@example.test"), (right, "right@example.test")] {
        assert!(concurrent.iter().any(|candidate| {
            candidate.operation == receipt.id && candidate.value["email"].as_str() == Some(email)
        }));
    }
    for path in &lab.paths[1..] {
        let actual = Proposals::at(path.clone()).snapshot().unwrap();
        assert_eq!(actual.cells, expected.cells);
        assert_eq!(
            actual
                .operations
                .iter()
                .map(|operation| operation.header.id)
                .collect::<Vec<_>>(),
            expected
                .operations
                .iter()
                .map(|operation| operation.header.id)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn windowless_background_runtime_replicates_yields_to_window_and_resumes_with_negative_control() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-bg-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([31; 16], vec![proposal("windowless-first")])
        .unwrap();

    // Negative control ("runtime only in windows"): when no interactive window runs on
    // device 1 and the node job starts no background runtime, device 1 never discovers or
    // replicates the table.
    let window_only_runtime =
        |window_running: bool| -> Option<bool> { window_running.then_some(true) };
    assert_eq!(window_only_runtime(false), None);
    assert!(!lab.paths[1].dir.join("commons/tables").exists());

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);

    let observer_1 = observers.path().join("1");
    aterm_secrets::fsx::ensure_private_dir(&observer_1).unwrap();
    let config_1 = observer_1.join("aterm.toml");
    let bg_progress = Attachment::new(port);
    let (bg_tx, bg_rx) = std::sync::mpsc::channel();
    let bg_runtime = Runtime::start_background_on(
        lab.paths[1].clone(),
        config_1.clone(),
        move |status| {
            let _ = bg_tx.send(status);
        },
        Network {
            isolated: Some(bg_progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();

    // Windowless member replicates while no window runs.
    bg_progress.wait_for(
        table.digest(),
        1,
        "windowless background runtime replicated initial write",
    );
    let bg_obs = bg_runtime.network_observation();
    wait_network(&bg_obs, "background runtime owns observation", |state| {
        state.via_local_process.is_none() && state.visibility == Visibility::Active
    });

    // A new window waiting for its LAN step leaves the working background owner in place.
    let win_progress = Attachment::new(port);
    let window_ready = Arc::new(AtomicBool::new(false));
    let ready = window_ready.clone();
    let (win_tx, win_rx) = std::sync::mpsc::channel();
    let win_runtime = Runtime::start_on(
        lab.paths[1].clone(),
        config_1.clone(),
        move |status| {
            let _ = win_tx.send(status);
        },
        Network {
            lan: LanGate::asking(move || {
                if ready.load(Ordering::Acquire) {
                    LanTurn::Open("the window is ready")
                } else {
                    LanTurn::Held("the window waits for its LAN step")
                }
            }),
            isolated: Some(win_progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    assert_eq!(
        win_rx.recv_timeout(Duration::from_secs(60)).unwrap(),
        Status::LanHeld
    );
    assert!(
        !owner::window_claimed(&crate::network_state::directory(&config_1)),
        "a held window must not make the working background owner yield"
    );
    source
        .write([34; 16], vec![proposal("while-window-waits")])
        .unwrap();
    bg_progress.wait_for(
        table.digest(),
        2,
        "background repairs while the window waits",
    );

    // Once ready, the window claims priority and takes the transport and observation.
    window_ready.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = bg_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("background runtime must yield with Status::Waiting when window opens");
        if status == Status::Waiting {
            break;
        }
    }
    let win_obs = win_runtime.network_observation();
    wait_network(&win_obs, "window runtime takes local ownership", |state| {
        state.via_local_process.is_none() && state.visibility == Visibility::Active
    });
    source
        .write([32; 16], vec![proposal("while-window-owns")])
        .unwrap();
    win_progress.wait_for(
        table.digest(),
        3,
        "window runtime replicated write while holding lease",
    );

    // Closing the window releases WindowClaim and Lease; background runtime resumes and
    // replicates the next change.
    win_runtime.stop();
    source
        .write([33; 16], vec![proposal("after-window-closed")])
        .unwrap();
    bg_progress.wait_for(
        table.digest(),
        4,
        "background runtime resumed after window exit and replicated fourth write",
    );

    bg_runtime.stop();
    owner.stop();

    let expected = source.snapshot().unwrap();
    let actual = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 4);
    assert_eq!(actual.cells, expected.cells);
}

/// A push hands a claims row on now: once the owner's push says a peer took it, the peer reads the
/// row's new revision, though its rounds take turns with another table. A runtime that serves no
/// repair says so at once.
///
/// NEGATIVE CONTROL: drop `board.take()` from `serve_mode`'s loop (the owner never takes the ask),
/// and the push waits out its one-minute deadline: `TimedOut`, not `Reached`.
#[test]
fn a_pushed_claims_row_reaches_a_peer_before_the_push_returns() {
    use crate::accounts::claims;
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-push-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source.write([41; 16], vec![proposal("push")]).unwrap();
    let row = |counter: u64| {
        let now = aterm_time::unix_millis().unwrap() / 1000;
        aterm_json::json!({"counter": counter, "until": now + 3600, "claims": []})
    };
    claims::write(&lab.paths[0], &row(1)).unwrap();
    let me = claims::read(&lab.paths[0]).unwrap().me;
    let counter = |n: usize| {
        claims::read(&lab.paths[n]).ok().and_then(|read| {
            read.rows
                .iter()
                .find(|(machine, _)| *machine == me)
                .and_then(|(_, body)| body["counter"].as_u64())
        })
    };
    assert_eq!(
        claims::push(
            &lab.paths[0],
            &Pusher::default(),
            Instant::now() + Duration::from_secs(60)
        )
        .unwrap(),
        Pushed::NotServing
    );

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);
    let (peer, progress) = start(&lab, observers.path(), 1, port);
    progress.wait_for(table.digest(), 1, "the peer took the other table");
    let until = Instant::now() + Duration::from_secs(60);
    while counter(1) != Some(1) {
        assert!(Instant::now() < until, "the peer never took the first row");
        thread::sleep(Duration::from_millis(10));
    }

    claims::write(&lab.paths[0], &row(2)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let pushed = loop {
        // The owner knows its peer once a round it began with it succeeded.
        match claims::push(&lab.paths[0], &owner.pusher(), deadline).unwrap() {
            Pushed::NoPeers if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10))
            }
            pushed => break pushed,
        }
    };
    assert_eq!(pushed, Pushed::Reached);
    assert_eq!(counter(1), Some(2), "the peer holds the pushed revision");
    peer.stop();
    owner.stop();
}

#[test]
fn two_loopback_islands_write_admit_and_remove_then_join_and_match_inventories() {
    use aterm_secrets::{
        client::{
            JoinOptions, JoinPassword, Paths, admin_cached, admin_commons_init, admin_open,
            admin_publish_approved, admit_member_in_log, create, join, load_records,
            machine_identity, remove_member_in_log, save_records, sync, test_sign_genesis,
        },
        commons::{
            binding::Pending,
            bootstrap::{Bootstrap, Device},
            journal::Journal,
        },
        membership_log::{Entrant, Evidence},
    };
    use std::collections::VecDeque;

    let join_local = |paths: &Paths, source: &str, host: &str| {
        join(
            paths,
            &JoinOptions {
                source: source.to_owned(),
                host: Some(host.to_owned()),
                interactive: false,
                wait: Duration::from_secs(2),
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .expect("join local vault");
    };
    let enroll_v1 = |owner_paths: &Paths, member_paths: &Paths| {
        sync(member_paths, None).expect("sync member");
        let owner = Bootstrap::required(owner_paths).expect("owner bootstrap");
        let peer = Bootstrap::required(member_paths).expect("peer bootstrap");
        let req = peer.request().expect("peer request");
        let (mut pending, ch) =
            Pending::issue(&req, owner.trust().network, owner.trust().authority).unwrap();
        let ans = peer.device().answer(&req, &ch).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(owner_paths)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&ans).unwrap())
            .expect("v1 enroll");
    };

    let root = aterm_tempfile::tempdir().unwrap();
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-f18-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let vault_file = root.path().join("rt.vault");

    // Found `{m1, m2, m3, m4}` where `{m1, m2, m3}` are bound in `Genesis` and `m4` is an unproved
    // founder (`binding: None`) that must enroll automatically over `ACE1` against non-admin `m2`!
    let m1 = Paths::for_home(&root.path().join("m1"));
    let created = create(&m1, vault_file.to_str().unwrap(), None, Some("m1")).unwrap();
    admin_commons_init(&m1).unwrap();
    let pinned = format!("{}#{}", vault_file.display(), created.signer);

    let m2 = Paths::for_home(&root.path().join("m2"));
    let m3 = Paths::for_home(&root.path().join("m3"));
    let m4 = Paths::for_home(&root.path().join("m4"));
    let r2 = machine_identity(&m2).unwrap().recipient().to_bech32();
    let r3 = machine_identity(&m3).unwrap().recipient().to_bech32();
    let r4 = machine_identity(&m4).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &r2).unwrap();
        doc.add_machine("m3", &r3).unwrap();
        doc.add_machine("m4", &r4).unwrap();
        admin_publish_approved(&m1, &admin, &doc, &[r2.clone(), r3.clone(), r4.clone()]).unwrap();
    }
    for (p, h) in [(&m2, "m2"), (&m3, "m3"), (&m4, "m4")] {
        join_local(p, &pinned, h);
    }
    enroll_v1(&m1, &m2);
    enroll_v1(&m1, &m3);
    // Intentionally leave `m4` un-enrolled before `sign_genesis` so `Genesis` lists `m4` with no
    // binding (`unproved`).
    for p in [&m2, &m3, &m4] {
        sync(p, None).unwrap();
    }
    let genesis_out = root.path().join("rt.genesis");
    test_sign_genesis(&m1, &genesis_out, &created.signer).unwrap();
    let seeded_records = load_records(&m1).unwrap().encode();
    for p in [&m2, &m3, &m4] {
        save_records(p, &seeded_records).unwrap();
    }
    let trust = Bootstrap::required(&m1).unwrap().trust();
    assert!(
        Bootstrap::required(&m4)
            .unwrap()
            .log_witness()
            .unwrap()
            .is_none()
    );

    let reports = Arc::new(Mutex::new(BTreeMap::<String, VecDeque<RepairRound>>::new()));
    let start_paths = |paths: &Paths, tag: &str, port: u16| -> (Runtime, Attachment) {
        let att = Attachment::new(port);
        let obs = observers.path().join(tag);
        aterm_secrets::fsx::ensure_private_dir(&obs).unwrap();
        let reports = reports.clone();
        let tag = tag.to_owned();
        let rt = Runtime::start_on_with_reports(
            paths.clone(),
            obs.join("aterm.toml"),
            |_| {},
            Network {
                isolated: Some(att.clone()),
                ..Network::default()
            },
            move |round| {
                let mut reports = reports.lock().unwrap_or_else(|p| p.into_inner());
                let recent = reports.entry(tag.clone()).or_default();
                if recent.len() == 8 {
                    recent.pop_front();
                }
                recent.push_back(round);
            },
        )
        .unwrap();
        (rt, att)
    };

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let res_a = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let res_b = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let res_joined = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port_a = res_a.port();
    let port_b = res_b.port();
    let port_joined = res_joined.port();

    // --- ISLAND A (`port_a`): `m1` initializes `Proposals`, admits newcomer `m5` via the log,
    //     both `m1` and `m5` write, and sync on `port_a`. ---
    let p1 = Proposals::at(m1.clone());
    let table = p1.initialize().unwrap();
    p1.write([41; 16], vec![proposal("island-a-m1")]).unwrap();

    let m5 = Paths::for_home(&root.path().join("m5"));
    aterm_secrets::client::configure_log_member(&m5, &pinned, "m5").unwrap();
    let _ = machine_identity(&m5).unwrap();
    let req5 = Device::open(&m5)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    admit_member_in_log(
        &m1,
        Entrant {
            request: req5,
            node_key: [0x55; 32],
            name: "m5".to_owned(),
            label: "m5".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    save_records(&m5, &load_records(&m1).unwrap().encode()).unwrap();

    let (rt_a1, att_a1) = start_paths(&m1, "a1", port_a);
    let (rt_a5, att_a5) = start_paths(&m5, "a5", port_a);
    att_a5.wait_for(table.digest(), 1, "m5 discovered table on island A");
    Proposals::at(m5.clone())
        .write([45; 16], vec![proposal("island-a-m5")])
        .unwrap();
    att_a1.wait_for(table.digest(), 2, "m1 received m5 write on island A");
    att_a5.wait_for(table.digest(), 2, "m5 retained 2 ops on island A");
    rt_a5.stop();
    rt_a1.stop();

    // --- ISLAND B (`port_b`): non-admin `m2` initializes `Proposals` and writes; unproved founder
    //     `m4` enrolls over `ACE1` against non-admin `m2`, removes `m3`, and writes! ---
    let p2 = Proposals::at(m2.clone());
    assert_eq!(p2.initialize().unwrap(), table);
    p2.write([42; 16], vec![proposal("island-b-m2")]).unwrap();

    let (rt_b2, att_b2) = start_paths(&m2, "b2", port_b);
    let (rt_b4, att_b4) = start_paths(&m4, "b4", port_b);
    att_b4.wait_for(
        table.digest(),
        1,
        "unproved m4 enrolled over ACE1 against non-admin m2 and replicated table",
    );
    // rt_b4 runs on m4: its repair rounds take and release commons.lock.
    assert!(
        Bootstrap::required_until(&m4, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
            .log_witness()
            .unwrap()
            .is_some()
    );

    let actor_m3 = Bootstrap::required(&m3)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    remove_member_in_log(&m4, actor_m3).expect("m4 removes m3 on island B");
    Proposals::at(m4.clone())
        .write([44; 16], vec![proposal("island-b-m4-after-remove")])
        .unwrap();
    att_b2.wait_for(
        table.digest(),
        2,
        "m2 received m4 write and removal on island B",
    );
    att_b4.wait_for(table.digest(), 2, "m4 retained 2 ops on island B");
    rt_b4.stop();
    rt_b2.stop();

    // --- JOIN (`port_joined`): both loopback islands move onto one multicast port, close wrap
    //     gaps over `ACX2`, and converge to identical 4-operation inventories! ---
    let (rt_j1, att_j1) = start_paths(&m1, "j1", port_joined);
    let (rt_j2, att_j2) = start_paths(&m2, "j2", port_joined);
    let (rt_j4, att_j4) = start_paths(&m4, "j4", port_joined);
    let (rt_j5, att_j5) = start_paths(&m5, "j5", port_joined);

    for (name, att) in [
        ("m1", &att_j1),
        ("m2", &att_j2),
        ("m4", &att_j4),
        ("m5", &att_j5),
    ] {
        let observed = att.wait_for_observation(table.digest(), 4);
        if observed.get(&table.digest()).copied().unwrap_or(0) < 4 {
            let rounds = reports.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let peers = [&rt_j1, &rt_j2, &rt_j4, &rt_j5]
                .map(|runtime| runtime.network_observation().snapshot());
            let stores = [&m1, &m2, &m4, &m5].map(|paths| {
                Proposals::at(paths.clone())
                    .snapshot()
                    .map(|snapshot| (snapshot.retained, snapshot.withheld, snapshot.cells.len()))
            });
            panic!(
                "{name} converged after islands joined: automatic repair did not retain 4 \
                 operations; observed {observed:?}; stores (retained, withheld, cells) \
                 for m1/m2/m4/m5 {stores:?}; peers {peers:?}; recent rounds {rounds:?}"
            );
        }
    }

    rt_j5.stop();
    rt_j4.stop();
    rt_j2.stop();
    rt_j1.stop();

    let expected = Proposals::at(m1.clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 4);
    assert_eq!(expected.cells.len(), 4);
    for p in [&m2, &m4, &m5] {
        let actual = Proposals::at(p.clone()).snapshot().unwrap();
        assert_eq!(actual.cells, expected.cells);
        assert_eq!(
            actual
                .operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>(),
            expected
                .operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>()
        );
    }
}
