// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The causal index a store builds at first use is the one an index built as its open replayed
//! would be (R2), and an observer that never projects or ingests builds none (R3).

use super::*;
use crate::table::{
    conflict::Conflict,
    store::{APPEND_FAILS, INDEX_BUILDS, INDEX_CAPACITY, with_append_sink, with_replay_admitted},
    tests::frontier,
};
use astream_wire::sync::{Header, Index, MAX_PARENTS};
use std::collections::BTreeSet;

/// An index given `headers` in turn, as an open that built its index as it replayed gave them.
fn fold<'h>(headers: impl IntoIterator<Item = &'h Header>) -> Index {
    let mut index = Index::new(INDEX_CAPACITY);
    for header in headers {
        index.insert(header.clone()).unwrap();
    }
    index
}

/// An index given the headers `store` holds: its records', then the discarded sources its
/// certificates keep.
fn holds(store: &Store) -> Index {
    fold(
        store.records.values().map(|stored| &stored.header).chain(
            store
                .conflicts
                .values()
                .filter_map(|stored| stored.conflict.discarded())
                .map(|source| &source.header),
        ),
    )
}

/// `store`'s index answers as `expected` does: its inventory, `get` of each id it holds, of each
/// header a certificate names and of one it holds nowhere, its forks and its projections; and the
/// store's own readers that build no index (`Store::forks`, `Store::header_lookup`) answer the same.
fn conforms(store: &Store, expected: &Index, what: &str) {
    let index = store.index().unwrap();
    assert_eq!(
        index.inventory().collect::<Vec<_>>(),
        expected.inventory().collect::<Vec<_>>(),
        "{what}: inventory"
    );
    let lookup = store.header_lookup();
    let mut asked: BTreeSet<Digest> = expected.inventory().copied().collect();
    asked.extend(
        store
            .conflicts
            .values()
            .flat_map(|stored| stored.conflict.headers().iter().map(|header| header.id)),
    );
    asked.insert([0x5a; 32]);
    for id in &asked {
        assert_eq!(index.get(id), expected.get(id), "{what}: get");
        assert_eq!(lookup(id), expected.get(id), "{what}: header_lookup");
    }
    assert_eq!(index.forks(), expected.forks(), "{what}: forks");
    assert_eq!(store.forks(), expected.forks(), "{what}: Store::forks");
    assert_eq!(index.project(), expected.project(), "{what}: project");
    let closure = store.quarantine_closure();
    assert_eq!(
        index.project_excluding(&closure),
        expected.project_excluding(&closure),
        "{what}: project_excluding"
    );
}

fn first(session: &Session<'_>, scope: Scope, payload: &[u8]) -> Vec<u8> {
    session
        .seal(
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            payload,
        )
        .unwrap()
}

/// R2, an application table of 64 records from two writers: observed, opened, then after an
/// ingest into an index the store had built and one into an index it had not.
#[test]
fn an_index_built_at_first_use_is_the_one_its_open_admitted() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let dirs = [lab._home.path().join("a"), lab._home.path().join("b")];
    let scope = {
        let (sa, sb) = (Session::open(&a).unwrap(), Session::open(&b).unwrap());
        let mut left = Store::create(&sa, &dirs[0]).unwrap();
        let scope = left.scope();
        let mut right = Store::join(&sb, &dirs[1], scope).unwrap();
        for n in 0..32 {
            left.commit_catalog(&sa, format!("left {n}").as_bytes())
                .unwrap();
            let receipt = right
                .commit_catalog(&sb, format!("right {n}").as_bytes())
                .unwrap();
            left.ingest(&sa, right.wire(&sb, &receipt.id).unwrap().unwrap())
                .unwrap();
        }
        scope
    };
    let observer = Session::observe(&a).unwrap();
    let (store, admitted) =
        with_replay_admitted(|| Store::observe(&observer, &dirs[0], scope).unwrap());
    assert_eq!(store.inventory(&observer).unwrap().count(), 64);
    assert_eq!(admitted.len(), 64);
    conforms(&store, &fold(&admitted), "observed");
    drop((store, observer));

    let (sa, sb) = (Session::open(&a).unwrap(), Session::open(&b).unwrap());
    let mut right = Store::open(&sb, &dirs[1], scope).unwrap();
    let mut later = || {
        let receipt = right.commit_catalog(&sb, b"later").unwrap();
        right.wire(&sb, &receipt.id).unwrap().unwrap().to_vec()
    };
    let (mut store, admitted) = with_replay_admitted(|| Store::open(&sa, &dirs[0], scope).unwrap());
    let mut expected = fold(&admitted);
    conforms(&store, &expected, "opened");
    let wire = later();
    store.ingest(&sa, &wire).unwrap();
    expected
        .insert(sa.authenticate(scope, &wire).unwrap().header().clone())
        .unwrap();
    conforms(&store, &expected, "after an ingest into a built index");
    drop(store);
    let (mut store, admitted) = with_replay_admitted(|| Store::open(&sa, &dirs[0], scope).unwrap());
    let mut expected = fold(&admitted);
    let wire = later();
    store.ingest(&sa, &wire).unwrap();
    expected
        .insert(sa.authenticate(scope, &wire).unwrap().header().clone())
        .unwrap();
    conforms(&store, &expected, "after an ingest that built the index");
}

/// R2 under control recovery: a quarantined record, a certificate whose discarded source is that
/// record and one whose source the store holds as no operation, each ingested and then replayed.
#[test]
fn an_index_built_at_first_use_holds_quarantined_records_and_kept_sources() {
    let f = Fixture::new(false, false, false);
    let b = f.lab.bootstrap(1);
    // A second record of the writer whose branch the recovery discards; the store under test
    // never holds it as an operation.
    let unheld = {
        let sb = Session::open(&b).unwrap();
        let mut store = Store::open(&sb, &f.dirs[1], f.scope).unwrap();
        let receipt = store
            .commit_catalog(&sb, b"a discarded source held nowhere")
            .unwrap();
        store.wire(&sb, &receipt.id).unwrap().unwrap().to_vec()
    };
    f.recover();
    let a = f.lab.bootstrap(0);
    let sa = Session::open(&a).unwrap();
    let proofs = [
        Conflict::from_discarded(&sa, f.scope, &f.wire).unwrap(),
        Conflict::from_discarded(&sa, f.scope, &unheld).unwrap(),
    ];
    let (mut store, admitted) =
        with_replay_admitted(|| Store::open(&sa, &f.dirs[0], f.scope).unwrap());
    assert_eq!(store.quarantined(&sa).unwrap().len(), 1);
    let mut expected = fold(&admitted);
    conforms(&store, &expected, "quarantined");
    for proof in &proofs {
        store.ingest_conflict(&sa, proof.wire()).unwrap();
        expected
            .insert(proof.discarded().unwrap().header.clone())
            .unwrap();
        conforms(&store, &expected, "after an ingest_conflict");
    }
    assert_eq!(store.inventory(&sa).unwrap().count(), 1);
    drop((store, sa));
    let observer = Session::observe(&a).unwrap();
    let (store, admitted) =
        with_replay_admitted(|| Store::observe(&observer, &f.dirs[0], f.scope).unwrap());
    assert_eq!(admitted.len(), 3, "the record and both sources");
    conforms(&store, &fold(&admitted), "observed with kept sources");
}

/// R2 for a native fork, the fork certificate a recovery store keeps for it (whose headers the
/// index holds nowhere), and a combined view of both stores.
#[test]
fn an_index_built_at_first_use_reads_forks_certificates_and_a_combined_view() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let session = Session::open(&a).unwrap();
    let original = lab._home.path().join("original");
    let mut source = Store::create(&session, &original).unwrap();
    let scope = source.scope();
    for payload in [b"one".as_slice(), b"two"] {
        source.commit_catalog(&session, payload).unwrap();
    }
    let signer = Session::open(&b).unwrap();
    for payload in [b"left".as_slice(), b"right"] {
        source
            .ingest(&session, &first(&signer, scope, payload))
            .unwrap();
    }
    let recovery_path = a
        .paths()
        .dir
        .join("commons/recovery/tables")
        .join(aterm_codec::hex::encode_lower(&scope.digest()));
    let mut recovery = Store::join_recovery(&session, &recovery_path, scope).unwrap();
    assert!(recovery.retain_replica_batch(&session, &source, 8).unwrap());
    assert_eq!(recovery.conflicts.len(), 1);
    assert_eq!(recovery.records.len(), 2);
    drop((recovery, source));

    let (primary, admitted) =
        with_replay_admitted(|| Store::observe(&session, &original, scope).unwrap());
    assert_eq!(primary.forks().len(), 1);
    conforms(&primary, &fold(&admitted), "a native fork");
    let (kept, admitted) =
        with_replay_admitted(|| Store::observe_recovery(&session, &recovery_path, scope).unwrap());
    conforms(&kept, &fold(&admitted), "a kept fork certificate");
    drop((primary, kept));
    let (combined, admitted) = with_replay_admitted(|| {
        Store::observe_recovery(&session, &recovery_path, scope)
            .unwrap()
            .with_observed_primary(
                &session,
                Store::observe(&session, &original, scope).unwrap(),
            )
            .unwrap()
    });
    assert_eq!(combined.records.len(), 4);
    conforms(&combined, &fold(&admitted), "a combined view");
}

/// R2 for a merge batch refused partway: the join's ingest is held and the edit's append fails,
/// after which the index is the one of what the store holds.
#[test]
fn a_merge_batch_refused_partway_leaves_the_index_of_what_the_store_holds() {
    let lab = frontier::wide_lab(MAX_PARENTS + 1);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("wide-merge")).unwrap();
    let scope = store.scope();
    store.commit_catalog(&session, b"first").unwrap();
    for wire in frontier::first_records(&lab, 1..lab.paths.len(), scope, |_, _| b"peer".to_vec()) {
        store.ingest(&session, &wire).unwrap();
    }
    let (_, bound) = store.metadata_with_append_bound(&session).unwrap();
    assert_eq!(bound, 2, "one join and the edit");
    let before = store.records.len();
    // The join's append succeeds and its sink arms the next append's failure: the edit's.
    let refused = with_append_sink(
        |_| APPEND_FAILS.with(|fails| fails.set(true)),
        || store.commit_catalog_with_merges(&session, b"edit", || Ok(b"join".to_vec())),
    );
    APPEND_FAILS.with(|fails| fails.set(false));
    assert!(refused.is_err_and(|error| error.0.contains("injected append failure")));
    assert_eq!(
        store.records.len(),
        before + 1,
        "the join is held, the edit not"
    );
    conforms(&store, &holds(&store), "a merge batch refused partway");
}

/// R3: two observer opens answer the reads an observer round asks of each (the proof inventory,
/// the operation inventory, the quarantine closure past its early return, the fence) and build no
/// causal index; one projection builds it, and a second reads the one built.
#[test]
fn an_observer_round_builds_no_causal_index() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    let dir = lab._home.path().join("observed");
    let scope = {
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &dir).unwrap();
        for payload in [b"one".as_slice(), b"two", b"three"] {
            store.commit_catalog(&session, payload).unwrap();
        }
        let signer = Session::open(&b).unwrap();
        for payload in [b"left".as_slice(), b"right"] {
            store
                .ingest(&session, &first(&signer, store.scope(), payload))
                .unwrap();
        }
        store.scope()
    };
    let observer = Session::observe(&a).unwrap();
    INDEX_BUILDS.with(|builds| builds.set(0));
    for _ in 0..2 {
        let store = Store::observe(&observer, &dir, scope).unwrap();
        assert_eq!(store.proof_inventory_unchecked().len(), 1);
        assert_eq!(store.inventory_unchecked().count(), 5);
        assert_eq!(store.quarantine_closure().len(), 2);
        assert!(store.fence().is_some());
    }
    assert_eq!(INDEX_BUILDS.get(), 0, "an observer round builds no index");
    let store = Store::observe(&observer, &dir, scope).unwrap();
    assert_eq!(store.catalog(&observer).unwrap().len(), 3);
    assert_eq!(INDEX_BUILDS.get(), 1, "a projection builds the index");
    assert_eq!(store.catalog(&observer).unwrap().len(), 3);
    assert_eq!(
        INDEX_BUILDS.get(),
        1,
        "a second projection reads the one built"
    );
}
