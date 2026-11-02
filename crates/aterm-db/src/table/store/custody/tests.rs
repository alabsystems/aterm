// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::directory::{Probe, with_probe};
use super::*;
use crate::table::{
    store::{SUBJECT, Store, claim_name},
    tests::Lab,
};
use aterm_secrets::commons::roster::Role;

fn files(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn enroll(session: &Session<'_>, dir: &std::path::Path, scope: Scope) -> Store {
    aterm_journal::create_private_dir_durable(dir.parent().unwrap()).unwrap();
    Store::join(session, dir, scope).unwrap()
}

#[test]
fn inspection_does_not_initialize_missing_or_interrupted_enrollment() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let claim_dir = claim_dir(&session).unwrap();
    let path = directory.path(scope).unwrap();
    let absent = inspect(&session, scope.digest()).unwrap();
    assert_eq!(absent.claim, Claim::Missing);
    assert!(absent.directory.is_none() && absent.prefix.is_none());
    assert!(!claim_dir.exists() && !path.exists());

    // Enrollment published its pending claim, but its data path was unavailable.
    aterm_journal::create_private_dir_durable(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"occupied data path").unwrap();
    assert!(Store::join(&session, &path, scope).is_err());
    let claims = files(&claim_dir);
    let interrupted = inspect(&session, scope.digest()).unwrap();
    assert_eq!(interrupted.claim, Claim::Pending);
    assert!(interrupted.directory.is_none() && interrupted.prefix.is_none());
    assert_eq!(files(&claim_dir), claims);
    assert_eq!(std::fs::read(&path).unwrap(), b"occupied data path");
}

#[test]
fn a_changed_legacy_device_keeps_a_verifiable_prefix_but_no_writer_permission() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let path = directory.path(scope).unwrap();
    let mut writer = enroll(&session, &path, scope);
    writer
        .commit_catalog(&session, b"custody-private-sentinel")
        .unwrap();
    let claims = files(&claim_dir(&session).unwrap());
    let data = files(&path);
    with_probe(
        Probe {
            dev_shift: 1,
            volume: Some([1; 16]),
        },
        || {
            // The exclusive writer stays open; inspection takes no journal writer lock.
            let read = inspect(&session, scope.digest()).unwrap();
            let identity = read.directory.as_ref().unwrap();
            assert_eq!(identity.recorded_device + 1, identity.observed.dev);
            assert_eq!(identity.recorded_inode, identity.observed.ino);
            assert!(!identity.volume_witness_present && identity.binding_error.is_some());
            let prefix = read.prefix.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(prefix.acknowledged_records, 1);
            assert_eq!(prefix.committed_records, 1);
            let json = read.json();
            assert_eq!(json["can_resume_writer"], json!(false));
            assert_eq!(json["application_authenticated"], json!(false));
            assert_eq!(json["receipt_prefix"]["state"], json!("verified"));
            assert!(!json.to_string().contains("custody-private-sentinel"));
            assert!(Store::observe(&session, &path, scope).is_err());
            assert_eq!(files(&claim_dir(&session).unwrap()), claims);
            assert_eq!(files(&path), data);
        },
    );
    // The original handle still commits once the unchanged original identity is observed.
    writer
        .commit_catalog(&session, b"inspection did not replace writer")
        .unwrap();
}

#[test]
fn a_foreign_volume_is_reported_without_rebinding_its_witness() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let path = directory.path(scope).unwrap();
    let mut writer = with_probe(
        Probe {
            dev_shift: 0,
            volume: Some([1; 16]),
        },
        || enroll(&session, &path, scope),
    );
    with_probe(
        Probe {
            dev_shift: 0,
            volume: Some([1; 16]),
        },
        || {
            writer.commit_catalog(&session, b"original volume").unwrap();
        },
    );
    let claims = files(&claim_dir(&session).unwrap());
    let data = files(&path);
    with_probe(
        Probe {
            dev_shift: 0,
            volume: Some([2; 16]),
        },
        || {
            let read = inspect(&session, scope.digest()).unwrap();
            let identity = read.directory.as_ref().unwrap();
            assert_eq!(identity.recorded_device, identity.observed.dev);
            assert!(identity.volume_witness_present && identity.binding_error.is_some());
            assert!(read.prefix.as_ref().unwrap().is_ok());
            assert!(Store::observe(&session, &path, scope).is_err());
            assert_eq!(files(&claim_dir(&session).unwrap()), claims);
            assert_eq!(files(&path), data);
        },
    );
}

#[test]
fn an_invalid_receipt_commitment_is_never_reported_as_verified() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let path = directory.path(scope).unwrap();
    let mut writer = enroll(&session, &path, scope);
    writer.commit_catalog(&session, b"retained").unwrap();
    let claim_path = claim_dir(&session).unwrap().join(claim_name(scope));
    let mut claim = std::fs::read(&claim_path).unwrap();
    claim[CLAIM_LEN - 1] ^= 1;
    std::fs::write(&claim_path, &claim).unwrap();
    let data = files(&path);
    let read = inspect(&session, scope.digest()).unwrap();
    assert!(read.directory.as_ref().unwrap().binding_error.is_none());
    assert!(read.prefix.as_ref().unwrap().is_err());
    assert_eq!(read.json()["receipt_prefix"]["state"], json!("refused"));
    assert!(Store::observe(&session, &path, scope).is_err());
    assert_eq!(std::fs::read(&claim_path).unwrap(), claim);
    assert_eq!(files(&path), data);
}

#[test]
fn foreign_claims_and_changes_during_inspection_do_not_escape_as_evidence() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let path = directory.path(scope).unwrap();
    let mut writer = enroll(&session, &path, scope);
    writer.commit_catalog(&session, b"retained").unwrap();
    let claim_path = claim_dir(&session).unwrap().join(claim_name(scope));
    let original = std::fs::read(&claim_path).unwrap();
    for index in [0, 5, 5 + super::super::super::SCOPE_LEN] {
        let mut changed = original.clone();
        changed[index] ^= 1;
        std::fs::write(&claim_path, &changed).unwrap();
        assert!(inspect(&session, scope.digest()).is_err());
        assert_eq!(std::fs::read(&claim_path).unwrap(), changed);
    }
    std::fs::write(&claim_path, &original).unwrap();
    let mut changed = original.clone();
    changed[CLAIM_LEN - 1] ^= 1;
    let failed = inspect_with(&session, scope.digest(), || {
        std::fs::write(&claim_path, &changed).unwrap();
    });
    assert!(failed.unwrap_err().is_busy());
    assert_eq!(std::fs::read(&claim_path).unwrap(), changed);
}

#[test]
fn an_unwitnessed_checkpointed_tail_is_counted_without_being_adopted() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let other = lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let peer_session = Session::open(&other).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory.catalog_scope();
    let path = directory.path(scope).unwrap();
    let mut writer = enroll(&session, &path, scope);
    writer.commit_catalog(&session, b"acknowledged").unwrap();
    let peer_dir = Directory::new(&lab.paths[1], bootstrap.trust())
        .path(scope)
        .unwrap();
    let mut peer = enroll(&peer_session, &peer_dir, scope);
    let receipt = peer
        .commit_catalog(&peer_session, b"not yet witnessed")
        .unwrap();
    let mut body = receipt.received_ms.to_le_bytes().to_vec();
    body.extend_from_slice(peer.wire(&peer_session, &receipt.id).unwrap().unwrap());
    writer.log.as_mut().unwrap().append(SUBJECT, body).unwrap();
    let claims = files(&claim_dir(&session).unwrap());
    let data = files(&path);
    with_probe(
        Probe {
            dev_shift: 1,
            volume: None,
        },
        || {
            let read = inspect(&session, scope.digest()).unwrap().json();
            assert_eq!(read["directory"]["binding_matches"], json!(false));
            assert_eq!(read["receipt_prefix"]["acknowledged_records"], json!(1));
            assert_eq!(read["receipt_prefix"]["committed_records"], json!(2));
            assert_eq!(read["receipt_prefix"]["unwitnessed_records"], json!(1));
            let retained = salvage(&session, &peer_session, scope.digest()).unwrap();
            assert_eq!(
                retained.entries.len(),
                1,
                "the uncertain tail is never imported"
            );
            assert_eq!(files(&claim_dir(&session).unwrap()), claims);
            assert_eq!(files(&path), data);
        },
    );
}
