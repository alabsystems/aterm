// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::frontier::wide_lab;

#[test]
fn wide_append_rejections_preserve_capacity_for_a_complete_edit() {
    let lab = wide_lab(2 * MAX_PARENTS + 1);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = lab._home.path().join("wide-append");
    let mut store = Store::create(&session, &directory).unwrap();
    let scope = store.scope();
    store.commit_catalog(&session, b"first").unwrap();
    for writer in 1..lab.paths.len() {
        let peer = lab.bootstrap(writer);
        let signer = Session::open(&peer).unwrap();
        let wire = signer
            .seal(
                scope,
                Causal {
                    sequence: 1,
                    previous: None,
                    parents: &[],
                },
                b"peer",
            )
            .unwrap();
        store.ingest(&session, &wire).unwrap();
    }
    let before = store.records.len();
    let (_, bound) = store.metadata_with_append_bound(&session).unwrap();
    assert_eq!(
        bound, 3,
        "two joins and the requested edit need three slots"
    );
    let original_log = std::fs::read(directory.join("table.astream")).unwrap();
    let claim_path = store.claim_dir.join(&store.claim_name);
    let original_claim = std::fs::read(&claim_path).unwrap();

    enum Refusal {
        Records,
        Bytes,
        Payload,
        Factory,
    }
    for refusal in [
        Refusal::Records,
        Refusal::Bytes,
        Refusal::Payload,
        Refusal::Factory,
    ] {
        store.capacity = Capacity::default();
        match refusal {
            Refusal::Records => store.capacity.operations = before + bound - 1,
            Refusal::Bytes => {
                // One maximum-size join fits; the second and the edit do not.
                store.capacity.bytes = store.operation_bytes + crypto::MAX_OPERATION;
            }
            Refusal::Payload | Refusal::Factory => {}
        }
        let payload = if matches!(refusal, Refusal::Payload) {
            vec![0; crypto::MAX_BODY + 1]
        } else {
            b"edit".to_vec()
        };
        let mut joins = 0;
        let result = store.commit_catalog_with_merges(&session, &payload, || {
            joins += 1;
            if matches!(refusal, Refusal::Factory) && joins == 2 {
                return Err(Error::new("merge factory failed"));
            }
            Ok(if matches!(refusal, Refusal::Bytes) {
                vec![0; crypto::MAX_BODY]
            } else {
                b"join".to_vec()
            })
        });
        assert!(result.is_err(), "the complete edit cannot be admitted");
        assert_eq!(store.records.len(), before, "refusal spent a table slot");
        assert_eq!(
            std::fs::read(directory.join("table.astream")).unwrap(),
            original_log,
            "refusal changed the durable journal"
        );
        assert_eq!(
            std::fs::read(&claim_path).unwrap(),
            original_claim,
            "refusal advanced the custody witness"
        );
    }

    store.capacity = Capacity {
        operations: before + bound,
        ..Capacity::default()
    };
    let mut joins = 0;
    let receipt = store
        .commit_catalog_with_merges(&session, b"edit", || {
            joins += 1;
            Ok(b"join".to_vec())
        })
        .unwrap();
    assert_eq!(joins, 2);
    assert_eq!(store.records.len(), before + bound);
    drop(store);
    let store = Store::open(&session, &directory, scope).unwrap();
    let records = store.catalog(&session).unwrap();
    assert_eq!(records.len(), before + bound);
    assert_eq!(records.last().unwrap().header().id, receipt.id);
    assert_eq!(records.last().unwrap().payload(), b"edit");

    let peer = lab.bootstrap(1);
    let peer_session = Session::open(&peer).unwrap();
    let mut replica = Store::join(
        &peer_session,
        &lab._home.path().join("wide-append-replica"),
        scope,
    )
    .unwrap();
    for id in store.inventory(&session).unwrap() {
        replica
            .ingest(&peer_session, store.wire(&session, id).unwrap().unwrap())
            .unwrap();
    }
    let replicated = replica.catalog(&peer_session).unwrap();
    assert_eq!(replicated.len(), records.len());
    assert_eq!(replicated.last().unwrap().header().id, receipt.id);
    assert_eq!(replicated.last().unwrap().payload(), b"edit");
}
