// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault-approved device starts with no Commons admission. The real runtime
//! must discover its issuer, obtain membership and repair application data.

use super::{auto_sync_tests::Attachment, *};
use crate::{
    accounts::proposals::Proposals,
    catalog::{Descriptor, Directory},
    database::Change,
    table::{Session, store::Store},
};
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use std::net::Ipv4Addr;

/// A runtime on the isolated attachment at `port`. An `unpolled` one never runs its serving loop's
/// polled checks ([`Network::unpolled`]): only an event of its own, such as the round that enrolls
/// it, moves what it serves.
fn start(
    paths: &Paths,
    observers: &Path,
    machine: usize,
    port: u16,
    unpolled: bool,
) -> (Runtime, Attachment) {
    let attachment = Attachment::new(port);
    let observer = observers.join(machine.to_string());
    aterm_secrets::fsx::ensure_private_dir(&observer).unwrap();
    let runtime = Runtime::start_on(
        paths.clone(),
        observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment.clone()),
            unpolled,
            ..Network::default()
        },
    )
    .unwrap();
    (runtime, attachment)
}

/// The recipient's polled checks never come due ([`Network::unpolled`]): the round that enrolls it
/// ends its enrolling serve at that round's tick, and it rebuilds as a member.
///
/// NEGATIVE CONTROL: drop `serve_selected`'s `if witness_due && witnessed(&bootstrap)?` (the
/// check at the tick an enrolling round completes): the recipient stays an enrolling device, runs
/// no application round, and `wait_for` reports after its silent minute that repair retained
/// nothing.
#[test]
fn automatic_runtime_enrolls_a_vault_approved_device_then_discovers_its_first_table() {
    // The issuer's vault records approval of this exact recipient. Discovery
    // supplies an address only; it cannot manufacture that prior approval.
    let lab = crate::control::enrollment::tests::Lab::new(true);
    let pending = lab.bootstrap(1);
    let recipient_binding = pending.request().unwrap().binding().clone();
    assert!(pending.witness().unwrap().is_none());
    assert!(!pending.access().is_ok_and(|access| access.is_some()));
    assert!(!lab.paths[1].dir.join("commons/tables").exists());

    let value = aterm_json::json!({"provider":"anthropic","method":"oauth"});
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    let receipt = source
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "published-before-enrollment".into(),
                value: Some(value.clone()),
            }],
        )
        .unwrap();

    // Keep observer sockets within Darwin's sockaddr_un bound independently
    // of the deeper generated vault paths. No live configuration is consulted.
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-enroll-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (issuer, _) = start(&lab.paths[0], observers.path(), 0, port, false);
    let (recipient, progress) = start(&lab.paths[1], observers.path(), 1, port, true);
    // The pending transport has no listener. Runtime must obtain its witness
    // and rebuild its own transport before ordinary catalog/table repair can
    // report this inventory. The test never constructs an enrolled endpoint.
    progress.wait_for(
        table.digest(),
        1,
        "newly enrolled recipient discovered its first table",
    );
    recipient.stop();
    issuer.stop();

    let enrolled = lab.bootstrap(1);
    assert!(enrolled.witness().unwrap().is_some());
    let access = enrolled.access().unwrap().unwrap();
    assert_eq!(access.local().role(), Role::Writer);
    assert_eq!(access.local().binding(), &recipient_binding);
    assert_eq!(access.members().len(), 2);
    let issuer_access = lab.bootstrap(0).access().unwrap().unwrap();
    assert!(
        issuer_access
            .members()
            .iter()
            .any(|member| member.binding() == &recipient_binding)
    );

    // Only now read the recipient's existing application store. A receipt
    // count alone is insufficient: require the exact signed operation and
    // its eligible application value, not merely a declaration or ciphertext.
    let received = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert_eq!(received.retained, 1);
    assert_eq!(received.withheld, 0);
    assert_eq!(received.operations.len(), 1);
    assert_eq!(received.operations[0].header.id, receipt.id);
    assert!(received.operations[0].eligible);
    let cell = &received.cells[&("accounts".into(), "published-before-enrollment".into())];
    assert!(!cell.stopped);
    assert_eq!(cell.candidates.len(), 1);
    assert_eq!(cell.candidates[0].operation, receipt.id);
    assert_eq!(cell.candidates[0].value, value);
}

#[test]
fn running_newly_enrolled_device_discovers_a_later_generic_table_and_can_write_back() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    assert!(
        !lab.bootstrap(1)
            .access()
            .is_ok_and(|access| access.is_some())
    );
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-hot-enroll-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (issuer, issuer_progress) = start(&lab.paths[0], observers.path(), 0, port, false);
    let recipient_progress = Attachment::new(port);
    let recipient_observer = observers.path().join("1");
    aterm_secrets::fsx::ensure_private_dir(&recipient_observer).unwrap();
    let (round_tx, round_rx) = std::sync::mpsc::channel();
    let recipient = Runtime::start_on_with_reports(
        lab.paths[1].clone(),
        recipient_observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(recipient_progress.clone()),
            ..Network::default()
        },
        move |round| {
            let _ = round_tx.send(round);
        },
    )
    .unwrap();

    // Complete enrollment and an application scan before publishing anything,
    // so the new table must be found after an already completed empty round.
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let round = round_rx
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .expect("newly enrolled runtime completed its first application scan");
        if round.control.is_ok()
            && round.actor.is_some()
            && !round.cancelled
            && round.application.is_some()
            && round.application_error.is_none()
        {
            break;
        }
    }
    assert!(lab.bootstrap(1).access().unwrap().is_some());
    let descriptor =
        Descriptor::application(lab.bootstrap(0).trust(), "after-enrollment", "items", 1).unwrap();
    let first = br#"{"key":"from-issuer","quantity":7}"#;
    let first_id = {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(&lab.paths[0], bootstrap.trust());
        directory.publish(&session, &descriptor).unwrap();
        Store::open(
            &session,
            &directory.path(descriptor.scope).unwrap(),
            descriptor.scope,
        )
        .unwrap()
        .commit_catalog(&session, first)
        .unwrap()
        .id
    };
    recipient_progress.wait_for(
        descriptor.scope.digest(),
        1,
        "table published after enrollment",
    );

    let second = br#"{"key":"from-recipient","quantity":9}"#;
    let second_id = {
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(&lab.paths[1], bootstrap.trust());
        let path = directory.path(descriptor.scope).unwrap();
        let retained = Store::observe(&session, &path, descriptor.scope)
            .unwrap()
            .catalog(&session)
            .unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].header().id, first_id);
        assert_eq!(retained[0].payload(), first);
        Store::open(&session, &path, descriptor.scope)
            .unwrap()
            .commit_catalog(&session, second)
            .unwrap()
            .id
    };
    issuer_progress.wait_for(
        descriptor.scope.digest(),
        2,
        "newly enrolled writer's reply",
    );
    recipient.stop();
    issuer.stop();

    let expected = std::collections::BTreeMap::from([
        (first_id, first.to_vec()),
        (second_id, second.to_vec()),
    ]);
    for paths in &lab.paths {
        let bootstrap = Bootstrap::required(paths).unwrap();
        let session = Session::observe(&bootstrap).unwrap();
        let directory = Directory::new(paths, bootstrap.trust());
        let store = Store::observe(
            &session,
            &directory.path(descriptor.scope).unwrap(),
            descriptor.scope,
        )
        .unwrap();
        let actual: std::collections::BTreeMap<_, _> = store
            .catalog(&session)
            .unwrap()
            .iter()
            .map(|record| (record.header().id, record.payload().to_vec()))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(store.inventory(&session).unwrap().count(), 2);
    }
}

/// The windowless recipient's polled checks never come due ([`Network::unpolled`]): the round that
/// enrolls it ends its enrolling serve at that round's tick.
///
/// NEGATIVE CONTROL: drop `serve_selected`'s `if witness_due && witnessed(&bootstrap)?`: the
/// recipient stays an enrolling device and `wait_for` reports after its silent minute that repair
/// retained nothing.
#[test]
fn windowless_steward_background_runtime_enrolls_windowless_pending_device() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    let pending = lab.bootstrap(1);
    let recipient_binding = pending.request().unwrap().binding().clone();
    assert!(pending.witness().unwrap().is_none());

    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write(
            [9; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "windowless-steward-enroller".into(),
                value: Some(aterm_json::json!({"provider":"anthropic","method":"oauth"})),
            }],
        )
        .unwrap();

    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-bg-enroll-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();

    let obs_0 = observers.path().join("0");
    let obs_1 = observers.path().join("1");
    aterm_secrets::fsx::ensure_private_dir(&obs_0).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&obs_1).unwrap();
    let issuer = Runtime::start_background_on(
        lab.paths[0].clone(),
        obs_0.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(Attachment::new(port)),
            ..Network::default()
        },
    )
    .unwrap();
    let progress = Attachment::new(port);
    let recipient = Runtime::start_background_on(
        lab.paths[1].clone(),
        obs_1.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(progress.clone()),
            unpolled: true,
            ..Network::default()
        },
    )
    .unwrap();

    progress.wait_for(
        table.digest(),
        1,
        "windowless steward enrolled windowless recipient and replicated first table",
    );
    recipient.stop();
    issuer.stop();

    let enrolled = lab.bootstrap(1);
    assert!(enrolled.witness().unwrap().is_some());
    assert_eq!(
        enrolled.access().unwrap().unwrap().local().binding(),
        &recipient_binding
    );
}
