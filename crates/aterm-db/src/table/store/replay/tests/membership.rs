// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeSet;

/// An unavailable historical grant withholds its witnessed record, not an independent record
/// whose current grant and key are available. Restoring the real handoff restores both records.
#[test]
fn unreadable_record_leaves_table_readable() {
    partial_history(false, false);
}

#[test]
fn pending_header_cannot_manufacture_a_writer_fork() {
    partial_history(true, false);
}

#[test]
fn pending_membership_withholds_authenticated_causal_children() {
    partial_history(false, true);
}

fn partial_history(competing: bool, dependent: bool) {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Reader, Role::Writer], 1);
    let a = lab.bootstrap(0);
    let original = lab._home.path().join("original");
    let replica = lab._home.path().join("partial-history");
    let (scope, old_wire, old_id) = {
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &original).unwrap();
        let receipt = store.commit_catalog(&session, b"historical").unwrap();
        (
            store.scope(),
            store.wire(&session, &receipt.id).unwrap().unwrap().to_vec(),
            receipt.id,
        )
    };
    lab.admit(&[Role::Writer, Role::Reader, Role::Writer]);
    let b = lab.bootstrap(1);
    let c = lab.bootstrap(2);
    let known_id = {
        let signer = Session::open(if competing { &a } else { &c }).unwrap();
        let wire = signer
            .seal(
                scope,
                Causal {
                    sequence: 1,
                    previous: None,
                    parents: &[],
                },
                b"known",
            )
            .unwrap();
        let session = Session::open(&b).unwrap();
        Store::join(&session, &replica, scope)
            .unwrap()
            .ingest(&session, &wire)
            .unwrap()
            .id
    };
    let handoff = {
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let access = a.access_in(&journal).unwrap().unwrap();
        a.history_handoff(
            &journal,
            &access,
            a.trust().genesis,
            b.request().unwrap().binding(),
        )
        .unwrap()
    };
    {
        let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
        let access = b.access_in(&journal).unwrap().unwrap();
        b.import_history(&journal, &access, handoff.bytes())
            .unwrap();
    }
    {
        let session = Session::open(&b).unwrap();
        Store::open(&session, &replica, scope)
            .unwrap()
            .ingest(&session, &old_wire)
            .unwrap();
    }
    let child_id = dependent.then(|| {
        let signer = Session::open(&c).unwrap();
        let mut parents = [old_id, known_id];
        parents.sort();
        let wire = signer
            .seal(
                scope,
                Causal {
                    sequence: 2,
                    previous: Some(known_id),
                    parents: &parents,
                },
                b"dependent",
            )
            .unwrap();
        let session = Session::open(&b).unwrap();
        Store::open(&session, &replica, scope)
            .unwrap()
            .ingest(&session, &wire)
            .unwrap()
            .id
    });
    let retained = if dependent { 3 } else { 2 };
    let restored_visible = if competing { 0 } else { retained };
    super::super::super::journals::with_own(
        std::sync::Arc::new(super::super::super::journals::Journals::new(
            1,
            64 * 1024,
            64 * 1024,
        )),
        || {
            {
                let session = Session::observe(&b).unwrap();
                let store = Store::observe(&session, &replica, scope).unwrap();
                assert_eq!(store.catalog(&session).unwrap().len(), restored_visible);
            }
            let held_dir = lab._home.path().join("held-history");
            std::fs::create_dir(&held_dir).unwrap();
            let history = lab.paths[1].dir.join("commons/history");
            let held: Vec<_> = std::fs::read_dir(&history)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.path(), held_dir.join(entry.file_name()))
                })
                .collect();
            assert!(!held.is_empty(), "CONTROL: a real history handoff exists");
            for (from, to) in &held {
                std::fs::rename(from, to).unwrap();
            }
            {
                let session = Session::observe(&b).unwrap();
                let Err(error) = session.authenticate(scope, &old_wire) else {
                    panic!("CONTROL: the unavailable historical key must still be required");
                };
                assert!(error.is_awaiting_membership(), "{error}");
                let store = Store::observe(&session, &replica, scope)
                    .expect("an unavailable record must not hide an independently readable one");
                assert!(
                    store
                        .metadata_evidence(&session)
                        .err()
                        .unwrap()
                        .is_awaiting_membership()
                );
                assert!(
                    store
                        .liability_evidence(&session)
                        .err()
                        .unwrap()
                        .is_awaiting_membership()
                );
                assert!(
                    store.quarantined(&session).unwrap().is_empty(),
                    "missing membership is not a discarded-source certificate"
                );
                let rows = store.catalog(&session).unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].payload(), b"known");
                assert_eq!(
                    store
                        .retained_use(&session, session.actor())
                        .unwrap()
                        .records,
                    retained
                );
                assert_eq!(
                    store
                        .replication_inventory(&session)
                        .unwrap()
                        .copied()
                        .collect::<BTreeSet<_>>(),
                    BTreeSet::from([known_id])
                );
                assert!(
                    crate::untyped_membership(|| Store::observe(&session, &replica, scope))
                        .is_err()
                );
            }
            for (from, to) in &held {
                std::fs::rename(to, from).unwrap();
            }
            let session = Session::observe(&b).unwrap();
            let store = Store::observe(&session, &replica, scope).unwrap();
            assert_eq!(store.catalog(&session).unwrap().len(), restored_visible);
            assert_eq!(
                store
                    .replication_inventory(&session)
                    .unwrap()
                    .copied()
                    .collect::<BTreeSet<_>>(),
                if competing {
                    BTreeSet::new()
                } else {
                    [Some(old_id), Some(known_id), child_id]
                        .into_iter()
                        .flatten()
                        .collect()
                }
            );
        },
    );
}
