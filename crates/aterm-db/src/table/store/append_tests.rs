// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::frontier::built_wide_lab;

/// A retained application payload needs no second cryptographic opening merely to find that it
/// is not a removal cutoff. A real cutoff still opens, under the ordinary fresh key admission.
#[test]
fn retained_non_cut_payloads_avoid_decryption_but_cutoffs_still_open() {
    journals::with_own(
        std::sync::Arc::new(journals::Journals::new(1, 64 * 1024, 64 * 1024)),
        || {
            let lab = crate::table::tests::Lab::new(&[Role::Writer]);
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let directory = lab._home.path().join("retained-payload");
            let mut store = Store::create(&session, &directory).unwrap();
            let scope = store.scope();
            store
                .commit_catalog(&session, b"first application record")
                .unwrap();
            let cut = cutoff::Cut {
                actor: session.actor(),
                through: None,
            };
            store
                .commit_catalog(&session, &cutoff::encode(&cut))
                .unwrap();
            store
                .commit_catalog(&session, b"second application record")
                .unwrap();
            drop((store, session));
            let session = Session::observe(&bootstrap).unwrap();
            let warm = Store::observe(&session, &directory, scope).unwrap();
            assert_eq!(
                warm.records
                    .values()
                    .filter(|record| record.cut.is_some())
                    .count(),
                1
            );
            let before = crate::crypto::test_cipher_constructions();
            let observed = Store::observe(&session, &directory, scope).unwrap();
            for (id, retained) in &warm.records {
                assert_eq!(
                    observed.records[id].wire.as_ptr(),
                    retained.wire.as_ptr(),
                    "replays share immutable ciphertext instead of copying it"
                );
            }
            drop(warm);
            assert_eq!(observed.inventory_unchecked().len(), 3);
            assert_eq!(
                observed
                    .records
                    .values()
                    .filter(|record| record.cut.is_some())
                    .count(),
                1
            );
            assert_eq!(
                crate::crypto::test_cipher_constructions() - before,
                1,
                "only the cutoff needs a fresh payload opening"
            );
        },
    );
}

/// A fact under another key must not hide a real cutoff. This supplies a wrong comparison
/// token on valid signed ciphertext, then takes the real key through ordinary source admission.
#[test]
fn payload_facts_require_the_current_key_and_exact_ciphertext() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = lab._home.path().join("wrong-payload-key");
    let mut store = Store::create(&session, &directory).unwrap();
    let scope = store.scope();
    let cut = cutoff::Cut {
        actor: session.actor(),
        through: None,
    };
    let receipt = store
        .commit_catalog(&session, &cutoff::encode(&cut))
        .unwrap();
    let application = store
        .commit_catalog(&session, b"application payload")
        .unwrap();
    let bytes = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
    let wire = crate::table::signatures::Identified::new(bytes);
    let application = crate::table::signatures::Identified::new(
        store
            .wire(&session, &application.id)
            .unwrap()
            .unwrap()
            .to_vec(),
    );
    let mut invalid = application.wire().to_vec();
    invalid.truncate(invalid.len() - 64);
    *invalid.last_mut().unwrap() ^= 1;
    let signature = session
        .bootstrap
        .device()
        .with_seed(|seed| crypto::signed(seed, crate::table::DOMAIN, &invalid))
        .unwrap();
    invalid.extend_from_slice(&signature);
    let invalid = crate::table::signatures::Identified::new(invalid);
    let envelope = crate::table::Envelope::parse_identified(scope, &wire).unwrap();
    session
        .with_payload_key(scope, &envelope, false, None, |key, _| {
            let mut wrong = zeroize::Zeroizing::new(*key);
            wrong[0] ^= 1;
            wire.remember_non_cut(crypto::payload_key_identity(&wrong));
            Ok(Some(()))
        })
        .unwrap()
        .unwrap();
    let sources = session.local_source_reads().unwrap();
    let before = crate::crypto::test_cipher_constructions();
    let decoded = sources
        .with_store_replay(scope, 2, |batch| {
            let (_, decoded) = batch.replay_metadata(&wire, envelope)?;
            let (_, ordinary) = batch.replay_metadata(
                &application,
                crate::table::Envelope::parse_identified(scope, &application)?,
            )?;
            assert!(ordinary.is_none());
            assert!(
                batch
                    .replay_metadata(
                        &application,
                        crate::table::Envelope::parse_identified(scope, &wire)?
                    )
                    .is_err(),
                "another ciphertext cannot borrow the ordinary payload's fact"
            );
            for _ in 0..2 {
                assert!(
                    batch
                        .replay_metadata(
                            &invalid,
                            crate::table::Envelope::parse_identified(scope, &invalid)?
                        )
                        .is_err(),
                    "a correctly signed but unauthenticated payload never becomes a fact"
                );
            }
            Ok(decoded)
        })
        .unwrap();
    assert_eq!(decoded, Some(cut));
    assert_eq!(crate::crypto::test_cipher_constructions() - before, 4);
}

/// At 129 writers an append needs two joins and its edit, so a bound of 3. A refusal by records,
/// bytes, payload or a factory failure on the second join spends no table slot and writes nothing,
/// and the exact capacity then admits the whole edit. This is the crate's one frontier wide enough
/// for a second join: `catalog_append_bound` gives 2 at 126 heads and 3 from 127, `MAX_PARENTS` is
/// a wire constant, and 128 writers force the same two joins as 129, so the width can shrink by at
/// most one.
#[test]
fn wide_append_rejections_preserve_capacity_for_a_complete_edit() {
    let lab = built_wide_lab(2 * MAX_PARENTS + 1);
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

/// A fence of an observed store stands under a later session where the store's own check would
/// pass, and falls where it refuses: unchanged it stands, an append moves the claim, and a
/// membership change moves the access the store was read under. A writer's store has none.
/// NEGATIVE CONTROL: the custody judgment the fence shares with the check (`Custody::admits`)
/// without its access-observation comparison stands after the membership change.
#[test]
fn a_fence_stands_where_its_stores_check_passes() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let dir = lab._home.path().join("fenced");
    let scope = {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::create(&session, &dir).unwrap();
        store.commit_catalog(&session, b"row").unwrap();
        assert!(store.fence().is_none(), "a writer's store has no fence");
        store.scope()
    };
    let observed = || {
        let bootstrap = lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let store = Store::observe(&session, &dir, scope).unwrap();
        let fence = store.fence().expect("an observed store has a fence");
        assert!(fence.stands(&session).unwrap());
        (store, fence)
    };
    let stands = |store: &Store, fence: &Fence| {
        let bootstrap = lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let stands = fence.stands(&session).unwrap();
        assert_eq!(
            stands,
            store.check(&session).is_ok(),
            "a fence stands as its store's check"
        );
        stands
    };

    let (store, fence) = observed();
    assert!(stands(&store, &fence), "an unchanged store's fence stands");
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &dir, scope)
            .unwrap()
            .commit_catalog(&session, b"another row")
            .unwrap();
    }
    assert!(!stands(&store, &fence), "an append moves the claim");

    let (store, fence) = observed();
    lab.rotate(&[Role::Writer, Role::Reader]);
    assert!(
        !stands(&store, &fence),
        "a membership change moves the access the store was read under"
    );
}

/// What a store counts of each writer ([`Store::writers`]) is what a walk of its records gives,
/// whether it holds them by its own commits, by an ingest of another writer's record, by a
/// writing reopen's replay, by an observation's, or by a combined view taking its primary's
/// records beside the recovery's (one of them held by both), and the writer quota reads that
/// count: a reopened store whose writer holds its quota refuses that writer's next record, and
/// one a record short takes it.
/// NEGATIVE CONTROL: `Store::hold` counting nothing fails the first assertion, `admit_records`
/// reading no count takes the record the quota refuses, and `with_observed_primary` holding the
/// primary's records uncounted fails the combined view's assertion.
#[test]
fn a_stores_writer_counts_are_what_a_walk_of_its_records_gives() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let dir = lab._home.path().join("writer-counts");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let peer = lab.bootstrap(1);
    let signer = Session::open(&peer).unwrap();
    let counted = |store: &Store| {
        store
            .writers
            .iter()
            .map(|(actor, held)| (*actor, (held.records, held.bytes)))
            .collect::<BTreeMap<_, _>>()
    };
    let walked = |store: &Store| {
        let mut walked = BTreeMap::<Actor, (usize, usize)>::new();
        for stored in store.records.values() {
            let held = walked.entry(stored.header.actor).or_default();
            held.0 += 1;
            held.1 += stored.wire.len();
        }
        walked
    };
    let check_usage = |store: &Store, session: &Session<'_>| {
        let walked = walked(store);
        for actor in walked.keys().copied().chain(std::iter::once([255; 48])) {
            let (writer_records, writer_bytes) = walked.get(&actor).copied().unwrap_or_default();
            assert_eq!(
                store.retained_use(session, actor).unwrap(),
                crate::catalog::ring::Used {
                    records: walked.values().map(|held| held.0).sum(),
                    bytes: walked.values().map(|held| held.1).sum(),
                    writer_records,
                    writer_bytes,
                }
            );
        }
    };
    let mut store = Store::create(&session, &dir).unwrap();
    check_usage(&store, &session);
    let scope = store.scope();
    for row in [&b"first"[..], b"second row", b"third, longer row"] {
        store.commit_catalog(&session, row).unwrap();
    }
    let wire = signer
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"the peer's row",
        )
        .unwrap();
    store.ingest(&session, &wire).unwrap();
    assert_eq!(counted(&store), walked(&store), "held by commit and ingest");
    check_usage(&store, &session);
    store.ingest(&session, &wire).unwrap();
    check_usage(&store, &session);
    assert_eq!(
        walked(&store).len(),
        2,
        "CONTROL: the store holds two writers' records"
    );
    drop(store);
    let mut reopened = Store::open(&session, &dir, scope).unwrap();
    check_usage(&reopened, &session);
    assert_eq!(
        counted(&reopened),
        walked(&reopened),
        "held by a writing reopen"
    );
    assert_eq!(reopened.retained_by(&session, session.actor()).unwrap(), 3);
    assert_eq!(reopened.retained_by(&session, signer.actor()).unwrap(), 1);
    reopened.writer_records = 3;
    let refused = reopened.commit_catalog(&session, b"over the quota");
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("writer evidence quota")),
        "a writer holding its quota is refused its next record: {refused:?}"
    );
    reopened.writer_records = 4;
    check_usage(&reopened, &session);
    reopened
        .commit_catalog(&session, b"within the quota")
        .unwrap();
    assert_eq!(counted(&reopened), walked(&reopened));
    check_usage(&reopened, &session);
    drop(reopened);
    let observer = Session::observe(&bootstrap).unwrap();
    let observed = Store::observe(&observer, &dir, scope).unwrap();
    check_usage(&observed, &observer);
    assert_eq!(
        counted(&observed),
        walked(&observed),
        "held by an observation"
    );
    assert_eq!(observed.retained_by(&observer, session.actor()).unwrap(), 4);
    drop(observed);
    let recovery_dir = bootstrap
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut recovery = Store::join_recovery(&session, &recovery_dir, scope).unwrap();
    recovery.ingest(&session, &wire).unwrap();
    drop(recovery);
    let combined = Store::observe_recovery(&session, &recovery_dir, scope)
        .unwrap()
        .with_observed_primary(&session, Store::observe(&session, &dir, scope).unwrap())
        .unwrap();
    assert_eq!(
        counted(&combined),
        walked(&combined),
        "held by a combined view"
    );
    assert_eq!(combined.retained_by(&session, session.actor()).unwrap(), 4);
    assert_eq!(combined.retained_by(&session, signer.actor()).unwrap(), 1);
    check_usage(&combined, &session);
    check_usage(combined.observed_primary.as_ref().unwrap(), &session);
}

/// An open's replay checks each operation record's signature under the identifier its receipt took
/// as the journal was read (`signatures::Identified`), so each operation record's wire is hashed
/// once: a writing reopen and a first observation of a table of three operation records hash
/// three wires each, an observation of the table as it was kept (`journals`) hashes none, and
/// one after a fourth record was committed hashes that record's alone, where a plain check of a
/// record's envelope hashes its wire again.
/// NEGATIVE CONTROL: `replay::restore` parsing its envelope with `Envelope::parse` hashes six.
#[test]
fn a_replay_hashes_each_records_wire_once() {
    use super::journals::{Journals, as_if_later, with_own};
    use crate::table::signatures::test_hashed;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let dir = lab._home.path().join("hashed-once");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &dir).unwrap();
    let scope = store.scope();
    let mut wire = Vec::new();
    for row in [&b"one"[..], b"two", b"three"] {
        let receipt = store.commit_catalog(&session, row).unwrap();
        wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
    }
    drop(store);
    let before = test_hashed();
    let reopened = Store::open(&session, &dir, scope).unwrap();
    assert_eq!(reopened.records.len(), 3);
    assert_eq!(
        test_hashed() - before,
        3,
        "a writing reopen hashes each wire once"
    );
    drop(reopened);
    let observer = Session::observe(&bootstrap).unwrap();
    let journals = std::sync::Arc::new(Journals::new(32, 1 << 30, 1 << 30));
    // An hour on: the journal stood settled at each observation.
    let observed = |records: usize| {
        let before = test_hashed();
        let observed = as_if_later(std::time::Duration::from_secs(3600), || {
            with_own(std::sync::Arc::clone(&journals), || {
                Store::observe(&observer, &dir, scope)
            })
        })
        .unwrap();
        assert_eq!(observed.records.len(), records);
        test_hashed() - before
    };
    assert_eq!(observed(3), 3, "a first observation hashes each wire once");
    assert_eq!(observed(3), 0, "a kept observation hashes none");
    let mut store = Store::open(&session, &dir, scope).unwrap();
    store.commit_catalog(&session, b"four").unwrap();
    drop(store);
    assert_eq!(
        observed(4),
        1,
        "an observation after a commit hashes the new wire alone"
    );
    let before = test_hashed();
    crate::table::Envelope::parse(scope, &wire).unwrap();
    assert_eq!(
        test_hashed() - before,
        1,
        "CONTROL: a plain check hashes its wire"
    );
}
