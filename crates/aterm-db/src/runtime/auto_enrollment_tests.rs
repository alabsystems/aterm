// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault-approved device starts with no Commons admission. The real runtime
//! must discover its issuer, obtain membership and repair application data.

use super::{auto_sync_tests::Attachment, *};
use crate::{accounts::proposals::Proposals, database::Change};
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use std::net::Ipv4Addr;

fn start(paths: &Paths, observers: &Path, machine: usize, port: u16) -> (Runtime, Attachment) {
    let attachment = Attachment::new(port);
    let observer = observers.join(machine.to_string());
    aterm_secrets::fsx::ensure_private_dir(&observer).unwrap();
    let runtime = Runtime::start_on(
        paths.clone(),
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
    let (issuer, _) = start(&lab.paths[0], observers.path(), 0, port);
    let (recipient, progress) = start(&lab.paths[1], observers.path(), 1, port);
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
