// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{
    Session,
    store::{SUBJECT, Store},
    tests::Lab,
};
use aterm_secrets::commons::roster::Role;

fn probe(dev_shift: u64, volume: Option<[u8; 16]>) -> Probe {
    Probe { dev_shift, volume }
}

#[test]
fn a_writable_open_witnesses_legacy_volume_but_an_observer_does_not() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, root.path()).unwrap();
    let scope = store.scope();
    store.commit_catalog(&session, b"before witness").unwrap();
    let custody = store.custody_id(&session).unwrap();
    let claim_dir = store.claim_dir.clone();
    let claim_name = store.claim_name.clone();
    let claim = std::fs::read(claim_dir.join(&claim_name)).unwrap();
    drop(store);

    with_probe(probe(0, Some([1; 16])), || {
        let observer = Store::observe(&session, root.path(), scope).unwrap();
        assert_eq!(observer.inventory(&session).unwrap().count(), 1);
        assert!(!claim_dir.join(name(&claim_name)).exists());
        assert_eq!(std::fs::read(claim_dir.join(&claim_name)).unwrap(), claim);
        drop(observer);

        let store = Store::open(&session, root.path(), scope).unwrap();
        assert!(claim_dir.join(name(&claim_name)).exists());
        assert_eq!(store.custody_id(&session).unwrap(), custody);
    });

    with_probe(probe(1, Some([1; 16])), || {
        let mut store = Store::open(&session, root.path(), scope).unwrap();
        assert_eq!(store.custody_id(&session).unwrap(), custody);
        let receipt = store
            .commit_catalog(&session, b"after device reassignment")
            .unwrap();
        assert_eq!(store.records[&receipt.id].header.sequence, 2);
        assert_eq!(store.inventory(&session).unwrap().count(), 2);
        assert_eq!(&store.claim[..WITNESS_AT], &claim[..WITNESS_AT]);
    });
}

#[test]
fn a_changed_legacy_device_is_never_given_a_new_volume_witness() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, root.path()).unwrap();
    store.commit_catalog(&session, b"retained").unwrap();
    let scope = store.scope();
    let claim_path = store.claim_dir.join(&store.claim_name);
    let volume_path = store.claim_dir.join(name(&store.claim_name));
    let claim = std::fs::read(&claim_path).unwrap();
    let data = std::fs::read(root.path().join("table.astream")).unwrap();
    drop(store);

    with_probe(probe(1, Some([1; 16])), || {
        assert!(Store::open(&session, root.path(), scope).is_err());
        assert!(Store::observe(&session, root.path(), scope).is_err());
        assert!(!volume_path.exists());
        assert_eq!(std::fs::read(&claim_path).unwrap(), claim);
        assert_eq!(
            std::fs::read(root.path().join("table.astream")).unwrap(),
            data
        );
    });
}

#[test]
fn another_volume_or_inode_is_refused_even_when_the_device_number_matches() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let original = root.path().join("original");
    let scope = with_probe(probe(0, Some([1; 16])), || {
        Store::create(&session, &original).unwrap().scope()
    });
    with_probe(probe(0, Some([2; 16])), || {
        assert!(Store::open(&session, &original, scope).is_err());
        assert!(Store::observe(&session, &original, scope).is_err());
    });
    with_probe(probe(0, None), || {
        assert!(Store::observe(&session, &original, scope).is_err());
    });

    let copied = root.path().join("copied");
    std::fs::create_dir(&copied).unwrap();
    for entry in std::fs::read_dir(&original).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), copied.join(entry.file_name())).unwrap();
    }
    with_probe(probe(0, Some([1; 16])), || {
        assert!(Store::open(&session, &copied, scope).is_err());
        assert!(Store::observe(&session, &copied, scope).is_err());
    });
}

#[test]
fn a_damaged_or_rebound_volume_witness_is_never_replaced() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    with_probe(probe(0, Some([1; 16])), || {
        let store = Store::create(&session, root.path()).unwrap();
        let scope = store.scope();
        let path = store.claim_dir.join(name(&store.claim_name));
        let original = std::fs::read(&path).unwrap();
        drop(store);
        for index in [0, 5, 37] {
            let mut changed = original.clone();
            changed[index] ^= 1;
            std::fs::write(&path, &changed).unwrap();
            assert!(Store::open(&session, root.path(), scope).is_err());
            assert!(Store::observe(&session, root.path(), scope).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), changed);
        }
    });
}

#[test]
fn tail_recovery_after_device_reassignment_keeps_the_original_custody_pin() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let peer_bootstrap = lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let peer_session = Session::open(&peer_bootstrap).unwrap();
    let path = root.path().join("table");
    let (scope, custody, claim) = with_probe(probe(0, Some([1; 16])), || {
        let mut store = Store::create(&session, &path).unwrap();
        let scope = store.scope();
        store.commit_catalog(&session, b"witnessed").unwrap();
        let custody = store.custody_id(&session).unwrap();
        let claim = store.claim.clone();
        let mut peer = Store::join(&peer_session, &root.path().join("peer"), scope).unwrap();
        let receipt = peer.commit_catalog(&peer_session, b"durable tail").unwrap();
        let mut body = receipt.received_ms.to_le_bytes().to_vec();
        body.extend_from_slice(peer.wire(&peer_session, &receipt.id).unwrap().unwrap());
        store.log.as_mut().unwrap().append(SUBJECT, body).unwrap();
        (scope, custody, claim)
    });
    with_probe(probe(1, Some([1; 16])), || {
        let mut recovered = Store::open(&session, &path, scope).unwrap();
        assert_eq!(recovered.inventory(&session).unwrap().count(), 2);
        assert_eq!(&recovered.claim[..WITNESS_AT], &claim[..WITNESS_AT]);
        assert_ne!(&recovered.claim[WITNESS_AT..], &claim[WITNESS_AT..]);
        assert_eq!(recovered.custody_id(&session).unwrap(), custody);
        let next = recovered.commit_catalog(&session, b"continued").unwrap();
        assert_eq!(recovered.records[&next.id].header.sequence, 2);
    });
}
