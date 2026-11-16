// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{catalog::Directory, table::tests::Lab};
use aterm_durable::Flush;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct WitnessFailure {
    _hook: aterm_journal::seam::Hook,
    _failure: Rc<RefCell<Option<aterm_uds::atomic_file::publication_seam::FailBeforeRename>>>,
}

/// Install after checkpoint publication, so the dependent claim rename is refused.
fn fail_witness_after_commit() -> WitnessFailure {
    let failure = Rc::new(RefCell::new(None));
    let fail = Rc::clone(&failure);
    let hook = aterm_journal::seam::before_each_commit(move || {
        *fail.borrow_mut() = Some(
            aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
                std::io::ErrorKind::PermissionDenied,
            ),
        );
    });
    WitnessFailure {
        _hook: hook,
        _failure: failure,
    }
}

pub(super) fn chain(session: &Session<'_>, scope: Scope, count: usize) -> Vec<Vec<u8>> {
    let mut previous = None;
    (1..=count)
        .map(|sequence| {
            let wire = session
                .seal(
                    scope,
                    Causal {
                        sequence: sequence as u64,
                        previous,
                        parents: previous.as_slice(),
                    },
                    &(sequence as u64).to_le_bytes(),
                )
                .unwrap();
            previous = Some(aterm_digest::Sha256::digest(&wire));
            wire
        })
        .collect()
}

#[test]
fn ingest_batch_one_checkpoint_one_witness() {
    for strict in [false, true] {
        let _strict = strict.then(aterm_journal::seam::serve_strict);
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::create(&session, &lab._home.path().join("batch")).unwrap();
        let wires = chain(&session, store.scope, 256);
        let borrowed: Vec<_> = wires.iter().map(Vec::as_slice).collect();
        let commits = Rc::new(RefCell::new(0));
        let seen = Rc::clone(&commits);
        let _hook = aterm_journal::seam::before_each_commit(move || *seen.borrow_mut() += 1);
        let delivered = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let seen = Arc::clone(&delivered);
        let table = store.scope.digest();
        let hear = move |record: &Appended<'_>| {
            if record.table == table {
                seen[0].fetch_add(1, Ordering::Relaxed);
                if record.length != 256 {
                    seen[1].fetch_add(1, Ordering::Relaxed);
                }
            }
        };
        let recording = aterm_durable::seam::Recording::start();
        let receipts = with_append_sink(hear, || store.ingest_batch(&session, &borrowed)).unwrap();
        let flushes = recording.take();
        assert_eq!(*commits.borrow(), 1);
        assert_eq!(
            flushes.len(),
            6,
            "one intent, checkpoint, and witness, each file then directory"
        );
        assert_eq!(&flushes[..2], &[Flush::Durable, Flush::Durable]);
        let dependent =
            if store.log.as_ref().unwrap().served().served == astream_broker::Tier::Ordered {
                Flush::Ordered
            } else {
                Flush::Durable
            };
        assert_eq!(&flushes[2..], &[dependent; 4]);
        assert_eq!(receipts.len(), 256);
        assert_eq!(store.witness.count, 256);
        assert_eq!(store.records.len(), 256);
        assert_eq!(store.index().unwrap().project().ordered.len(), 256);
        assert_eq!(delivered[0].load(Ordering::Relaxed), 256);
        assert_eq!(delivered[1].load(Ordering::Relaxed), 0);
        for (offset, receipt) in receipts.iter().enumerate() {
            assert_eq!(receipt.offset, offset as u64);
            assert_eq!(receipt.id, aterm_digest::Sha256::digest(&wires[offset]));
        }
        let scope = store.scope;
        let dir = store.data_dir.clone();
        drop(store);
        let reopened = Store::open(&session, &dir, scope).unwrap();
        assert_eq!(reopened.records.len(), 256);
        assert_eq!(reopened.witness.count, 256);
    }
}

#[test]
fn ingest_batch_coalesces_proposals_and_exact_retries_without_notifications_or_writes() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("duplicates")).unwrap();
    let wires = chain(&session, store.scope, 2);
    let notifications = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&notifications);
    let table = store.scope.digest();
    let hear = move |record: &Appended<'_>| {
        if record.table == table {
            seen.fetch_add(1, Ordering::Relaxed);
        }
    };
    let hear = Rc::new(hear);
    let heard = Rc::clone(&hear);
    let receipts = with_append_sink(
        move |record: &Appended<'_>| heard(record),
        || store.ingest_batch(&session, &[&wires[0], &wires[0], &wires[1], &wires[0]]),
    )
    .unwrap();
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(receipts[0], receipts[3]);
    assert_eq!(store.records.len(), 2);
    assert_eq!(notifications.load(Ordering::Relaxed), 2);
    let recording = aterm_durable::seam::Recording::start();
    store.capacity.operations = 0;
    let retry = with_append_sink(
        move |record: &Appended<'_>| hear(record),
        || store.ingest_batch(&session, &[&wires[1], &wires[0], &wires[1]]),
    )
    .unwrap();
    assert_eq!(retry, [receipts[2], receipts[0], receipts[2]]);
    assert!(recording.take().is_empty());
    assert_eq!(notifications.load(Ordering::Relaxed), 2);
}

#[test]
fn ingest_batch_refuses_late_authentication_and_every_aggregate_capacity_before_permission() {
    for refusal in 0..6 {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::create(&session, &lab._home.path().join("preflight")).unwrap();
        let mut wires = chain(&session, store.scope, 2);
        match refusal {
            0 => {
                let last = wires[1].len() - 1;
                wires[1][last] ^= 1;
            }
            1 => store.capacity.operations = 1,
            2 => store.capacity.bytes = wires[0].len(),
            3 => store.writer_records = 1,
            4 => {
                store.index.set(Index::new(1)).unwrap();
            }
            5 => {
                drop(store.log.take());
                let mut limits = Capacity::default().journal();
                limits.records = 1;
                let mut log = Log::open(
                    &store.data_dir,
                    "table",
                    &store.claim[..ANCHOR_LEN],
                    true,
                    limits,
                )
                .unwrap();
                log.finish_open().unwrap();
                store.log = Some(log);
            }
            _ => unreachable!(),
        }
        let before = [
            std::fs::read(store.data_dir.join("table.astream")).unwrap(),
            std::fs::read(store.data_dir.join("table.checkpoint")).unwrap(),
            std::fs::read(store.claim_dir.join(&store.claim_name)).unwrap(),
        ];
        assert!(
            store
                .ingest_batch(&session, &[&wires[0], &wires[1]])
                .is_err(),
            "case {refusal}"
        );
        assert!(
            !store.poisoned,
            "preflight case {refusal} mutated the writer"
        );
        assert!(store.records.is_empty());
        assert_eq!(store.witness.count, 0);
        assert!(
            !store
                .claim_dir
                .join(group::name(&store.claim_name))
                .exists()
        );
        assert_eq!(
            before,
            [
                std::fs::read(store.data_dir.join("table.astream")).unwrap(),
                std::fs::read(store.data_dir.join("table.checkpoint")).unwrap(),
                std::fs::read(store.claim_dir.join(&store.claim_name)).unwrap(),
            ]
        );
    }
}

#[test]
fn ingest_batch_interleaves_scalar_appends_retained_receipts_and_ordered_witness_recovery() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("mixed")).unwrap();
    let wires = chain(&session, store.scope, 6);
    let first = store
        .ingest_batch(&session, &[&wires[0], &wires[1]])
        .unwrap();
    let prior = store.claim.clone();
    let scalar = store.ingest(&session, &wires[2]).unwrap();
    let (scope, dir, claims, name) = (
        store.scope,
        store.data_dir.clone(),
        store.claim_dir.clone(),
        store.claim_name.clone(),
    );
    let ordered = store.log.as_ref().unwrap().served().served == astream_broker::Tier::Ordered;
    drop(store);
    if ordered {
        // The scalar commit covered the preceding group's witness. Its own Ordered
        // witness can still be lost, leaving that completed group's permit stale.
        private_file::write_private_flushed(&claims, &name, &prior, Flush::Durable).unwrap();
    }
    let mut store = Store::open(&session, &dir, scope).unwrap();
    assert_eq!(store.records.get(&scalar.id).unwrap().receipt, scalar);
    assert_eq!(store.witness.count, 3);
    let mixed = store
        .ingest_batch(
            &session,
            &[&wires[1], &wires[3], &wires[1], &wires[4], &wires[3]],
        )
        .unwrap();
    assert_eq!(mixed[0], first[1]);
    assert_eq!(mixed[2], first[1]);
    assert_eq!(mixed[1], mixed[4]);
    assert_eq!((mixed[1].offset, mixed[3].offset), (3, 4));
    let prior = store.claim.clone();
    let last = store.ingest(&session, &wires[5]).unwrap();
    let ordered = store.log.as_ref().unwrap().served().served == astream_broker::Tier::Ordered;
    drop(store);
    if ordered {
        private_file::write_private_flushed(&claims, &name, &prior, Flush::Durable).unwrap();
    }
    let store = Store::open(&session, &dir, scope).unwrap();
    assert_eq!(store.records.get(&last.id).unwrap().receipt, last);
    assert_eq!(store.witness.count, 6);
    assert_eq!(store.index().unwrap().project().ordered.len(), 6);
}

#[test]
fn ingest_batch_withdraws_new_equivocation_atomically_and_charges_its_evidence() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("fork-batch")).unwrap();
    let first = session
        .seal(
            store.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"first",
        )
        .unwrap();
    let other = session
        .seal(
            store.scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            b"other",
        )
        .unwrap();
    let previous = aterm_digest::Sha256::digest(&other);
    let later = session
        .seal(
            store.scope,
            Causal {
                sequence: 2,
                previous: Some(previous),
                parents: &[previous],
            },
            b"later",
        )
        .unwrap();
    let receipts = store
        .ingest_batch(&session, &[&first, &other, &later])
        .unwrap();
    let view = store.index().unwrap().project();
    assert!(view.ordered.is_empty());
    assert_eq!(view.pending.len(), 3);
    assert!(view.quarantined.contains(&session.actor()));
    assert_eq!(
        store.operation_bytes,
        first.len() + other.len() + later.len()
    );
    let previous = aterm_digest::Sha256::digest(&later);
    let refused = session
        .seal(
            store.scope,
            Causal {
                sequence: 3,
                previous: Some(previous),
                parents: &[previous],
            },
            b"restricted",
        )
        .unwrap();
    assert!(store.ingest_batch(&session, &[&refused]).is_err());
    assert_eq!(store.witness.count, 3);
    assert_eq!(
        store
            .ingest_batch(&session, &[&first, &other, &later])
            .unwrap(),
        receipts
    );
}

#[test]
fn ingest_batch_recovery_requires_its_independent_exact_permission() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("permit")).unwrap();
    let wires = chain(&session, store.scope, 2);
    let failure = fail_witness_after_commit();
    assert!(
        store
            .ingest_batch(&session, &[&wires[0], &wires[1]])
            .is_err()
    );
    drop(failure);
    let (scope, dir, claims, claim_name, original_claim) = (
        store.scope,
        store.data_dir.clone(),
        store.claim_dir.clone(),
        store.claim_name.clone(),
        store.claim.clone(),
    );
    let permit_name = group::name(&claim_name);
    let valid = private_file::read_private(&claims, &permit_name, 1024).unwrap();
    drop(store);
    let mut wrong_hash = valid.clone();
    *wrong_hash.last_mut().unwrap() ^= 1;
    let mut oversized = valid.clone();
    let end_at = valid.len() - Witness::default().bytes().len();
    oversized[end_at..end_at + 8].copy_from_slice(&257u64.to_le_bytes());
    let mut short = valid.clone();
    short.pop();
    for (case, malformed) in [
        ("missing", None),
        ("endpoint hash", Some(wrong_hash)),
        ("oversized", Some(oversized)),
        ("truncated", Some(short)),
    ] {
        if let Some(bytes) = malformed {
            private_file::write_private_flushed(&claims, &permit_name, &bytes, Flush::Durable)
                .unwrap();
        } else {
            private_file::remove_private(&claims, &permit_name).unwrap();
        }
        assert!(
            Store::observe(&session, &dir, scope).is_err(),
            "observer accepted {case} permission"
        );
        assert!(
            Store::open(&session, &dir, scope).is_err(),
            "writer accepted {case} permission"
        );
        assert_eq!(
            private_file::read_private(&claims, &claim_name, CLAIM_LEN).unwrap(),
            original_claim
        );
    }
    private_file::write_private_flushed(&claims, &permit_name, &valid, Flush::Durable).unwrap();
    let other = Store::create(&session, &lab._home.path().join("other-permit")).unwrap();
    private_file::write_private_flushed(
        &other.claim_dir,
        &group::name(&other.claim_name),
        &valid,
        Flush::Durable,
    )
    .unwrap();
    assert!(group::read(&other.claim_dir, &other.claim_name, &other.claim).is_err());
    let recovered = Store::open(&session, &dir, scope).unwrap();
    assert_eq!(recovered.witness.count, 2);
    assert_eq!(recovered.records.len(), 2);
}

#[test]
fn a_checkpoint_inside_a_permitted_group_is_not_a_single_unwitnessed_append() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let store = Store::create(&session, &lab._home.path().join("partial-group")).unwrap();
    let wires = chain(&session, store.scope, 2);
    let before = store.witness;
    let first = before
        .advance(0, aterm_digest::Sha256::digest(&wires[0]), 17)
        .unwrap();
    let end = first
        .advance(1, aterm_digest::Sha256::digest(&wires[1]), 17)
        .unwrap();
    group::prepare(
        &store.claim_dir,
        &store.claim_name,
        &store.claim,
        before,
        end,
    )
    .unwrap();
    let (scope, dir, claim) = (store.scope, store.data_dir.clone(), store.claim.clone());
    drop(store);
    // An adversarial data prefix has a valid checkpoint but stops inside the independently
    // authorized group. The old scalar allowance must not adopt it under that permission.
    let mut log = Log::open(
        &dir,
        "table",
        &claim[..ANCHOR_LEN],
        true,
        Capacity::default().journal(),
    )
    .unwrap();
    log.finish_open().unwrap();
    let mut body = 17u64.to_le_bytes().to_vec();
    body.extend_from_slice(&wires[0]);
    log.append(SUBJECT, body).unwrap();
    drop(log);
    assert!(Store::observe(&session, &dir, scope).is_err());
    assert!(Store::open(&session, &dir, scope).is_err());
}

#[test]
fn ingest_batch_crash_before_witness_relays_nothing_and_recovers_the_exact_group() {
    const HOME: &str = "ATERM_TEST_BATCH_HOME";
    const TABLE: &str = "ATERM_TEST_BATCH_TABLE";
    const GENERATION: &str = "ATERM_TEST_BATCH_GENERATION";
    const PROOF: &str = "batch-group-committed-without-witness";
    const SIGKILL: i32 = 9;
    if let Some(home) = std::env::var_os(HOME) {
        use std::io::Write as _;
        let paths = aterm_secrets::client::Paths::for_home(Path::new(&home));
        let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::cached(&paths)
            .unwrap()
            .unwrap();
        let mut table = [0; 16];
        assert!(aterm_codec::hex::decode_into(
            std::env::var(TABLE).unwrap().as_bytes(),
            &mut table
        ));
        let scope = Scope::new(
            bootstrap.trust(),
            table,
            std::env::var(GENERATION).unwrap().parse().unwrap(),
        )
        .unwrap();
        let session = Session::open(&bootstrap).unwrap();
        let dir = Directory::new(&paths, bootstrap.trust())
            .path(scope)
            .unwrap();
        let mut store = Store::open(&session, &dir, scope).unwrap();
        let wires = chain(&session, scope, 2);
        let failure = fail_witness_after_commit();
        assert!(
            store
                .ingest_batch(&session, &[&wires[0], &wires[1]])
                .is_err()
        );
        drop(failure);
        assert!(store.poisoned);
        assert!(
            read_held(&paths, scope.digest(), false, 0, u64::MAX)
                .unwrap()
                .is_empty()
        );
        let aterm_journal::Committed::Rebuilt { records, .. } = aterm_journal::read_committed(
            &dir,
            "table",
            &store.claim[..ANCHOR_LEN],
            Capacity::default().journal(),
            None,
        )
        .unwrap() else {
            panic!("a complete journal read rebuilds");
        };
        assert_eq!(records.len(), 2);
        println!("{PROOF}");
        std::io::stdout().flush().unwrap();
        aterm_uds::process::kill_self_now();
    }
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let paths = &lab.paths[0];
    let directory = Directory::new(paths, bootstrap.trust());
    let scope = {
        let session = Session::open(&bootstrap).unwrap();
        let scope = session.scope().unwrap();
        let dir = directory.path(scope).unwrap();
        aterm_journal::create_private_dir_durable(dir.parent().unwrap()).unwrap();
        drop(Store::join(&session, &dir, scope).unwrap());
        scope
    };
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child.args(["--exact", "table::store::batch_tests::ingest_batch_crash_before_witness_relays_nothing_and_recovers_the_exact_group", "--nocapture"])
        .env(HOME, &paths.home).env(TABLE, aterm_codec::hex::encode_lower(scope.table()))
        .env(GENERATION, scope.generation().to_string());
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(output.status.signal(), Some(SIGKILL), "{output:?}");
    assert!(String::from_utf8(output.stdout).unwrap().contains(PROOF));
    assert!(
        read_held(paths, scope.digest(), false, 0, u64::MAX)
            .unwrap()
            .is_empty()
    );
    {
        let observer = Session::observe(&bootstrap).unwrap();
        let store = Store::observe(&observer, &directory.path(scope).unwrap(), scope).unwrap();
        assert!(store.records.is_empty());
        assert_eq!(store.witness.count, 0);
    }
    let session = Session::open(&bootstrap).unwrap();
    let store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(store.records.len(), 2);
    assert_eq!(store.witness.count, 2);
    assert_eq!(store.index().unwrap().project().ordered.len(), 2);
    assert_eq!(
        read_held(paths, scope.digest(), false, 0, u64::MAX)
            .unwrap()
            .len(),
        2
    );
}

/// A writer stopped with a group's permit durable and none of its records in the log, then
/// each later point recovered exactly. The writing open retires the permit, and retires it again
/// where a crash before that removal's directory flush kept it. The next scalar append moves
/// custody past the permit's start, and the table opens whether that append's witness was
/// published or its writer stopped after the commit: the one unwitnessed append a table with no
/// pending permit allows. A retry of the group returns the retained record's receipt and
/// commits the other.
#[test]
fn a_permit_whose_group_never_landed_is_retired_before_the_next_scalar_append() {
    for witness_lost in [false, true] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::create(&session, &lab._home.path().join("unlanded")).unwrap();
        let (scope, dir, claims, name) = (
            store.scope,
            store.data_dir.clone(),
            store.claim_dir.clone(),
            store.claim_name.clone(),
        );
        let wires = chain(&session, scope, 2);
        APPEND_FAILS.with(|fails| fails.set(true));
        let stopped = store.ingest_batch(&session, &[&wires[0], &wires[1]]);
        APPEND_FAILS.with(|fails| fails.set(false));
        assert!(stopped.is_err_and(|error| error.0.contains("injected append failure")));
        let boundary = group::read(&claims, &name, &store.claim).unwrap();
        assert_eq!(boundary.acknowledged().count, 0);
        assert_eq!(boundary.end().map(|end| end.count), Some(2));
        drop(store);
        let permit = group::name(&name);
        let pending = private_file::read_private(&claims, &permit, 1024).unwrap();
        for case in [
            "after the permit",
            "before the retirement's directory flush",
        ] {
            let store = Store::open(&session, &dir, scope).unwrap();
            assert_eq!(store.witness.count, 0, "{case}");
            drop(store);
            // A crash before the retirement's directory flush keeps the removed entry.
            private_file::write_private_flushed(&claims, &permit, &pending, Flush::Durable)
                .unwrap();
        }
        let id = aterm_digest::Sha256::digest(&wires[0]);
        let mut store = Store::open(&session, &dir, scope).unwrap();
        if witness_lost {
            let failure = fail_witness_after_commit();
            assert!(store.ingest(&session, &wires[0]).is_err());
            drop(failure);
        } else {
            assert_eq!(store.ingest(&session, &wires[0]).unwrap().offset, 0);
        }
        drop(store);
        assert_eq!(
            Store::observe(&session, &dir, scope).unwrap().witness.count,
            u64::from(!witness_lost)
        );
        let mut store = Store::open(&session, &dir, scope).unwrap();
        assert_eq!(store.witness.count, 1);
        assert!(matches!(
            private_file::read_private(&claims, &permit, 1024),
            Err(PrivateFileError::Missing)
        ));
        let scalar = store.records.get(&id).unwrap().receipt;
        assert_eq!(scalar.offset, 0);
        let retried = store
            .ingest_batch(&session, &[&wires[0], &wires[1]])
            .unwrap();
        assert_eq!(retried[0], scalar);
        assert_eq!(retried[1].offset, 1);
        drop(store);
        assert_eq!(
            Store::observe(&session, &dir, scope).unwrap().witness.count,
            2
        );
        let store = Store::open(&session, &dir, scope).unwrap();
        assert_eq!(store.witness.count, 2);
        assert_eq!(store.index().unwrap().project().ordered.len(), 2);
    }
}

/// A custody check refused after a group's commit still hands each of its new records to the
/// append sink once; the retry that then returns their receipts hands none of them again.
#[test]
fn ingest_batch_announces_its_commit_once_when_the_check_after_it_refuses() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("announced")).unwrap();
    let wires = chain(&session, store.scope, 2);
    let sunk = Rc::new(RefCell::new(Vec::new()));
    let sink = || {
        let sunk = Rc::clone(&sunk);
        move |record: &Appended<'_>| sunk.borrow_mut().push((record.offset, record.length))
    };
    let (claims, volume) = (
        store.claim_dir.clone(),
        format!("{}.volume", store.claim_name),
    );
    let kept = private_file::read_private(&claims, &volume, 1024).ok();
    // The claim's volume witness damaged while the group commits: the check after it refuses.
    let damage = {
        let (claims, volume) = (claims.clone(), volume.clone());
        aterm_journal::seam::before_each_commit(move || {
            private_file::write_private_flushed(&claims, &volume, b"damaged", Flush::Durable)
                .unwrap();
        })
    };
    let refused = with_append_sink(sink(), || {
        store.ingest_batch(&session, &[&wires[0], &wires[1]])
    });
    drop(damage);
    let error = refused.unwrap_err();
    assert!(error.0.contains("volume witness"), "{error}");
    assert_eq!(store.witness.count, 2, "the group committed");
    assert_eq!(*sunk.borrow(), [(0, 2), (1, 2)]);
    match &kept {
        Some(bytes) => {
            private_file::write_private_flushed(&claims, &volume, bytes, Flush::Durable).unwrap();
        }
        None => {
            private_file::remove_private(&claims, &volume).unwrap();
        }
    }
    let receipts = with_append_sink(sink(), || {
        store.ingest_batch(&session, &[&wires[0], &wires[1]])
    })
    .unwrap();
    assert_eq!((receipts[0].offset, receipts[1].offset), (0, 1));
    assert_eq!(*sunk.borrow(), [(0, 2), (1, 2)]);
}

/// A writing open keeps the permit of a group that reached the log until the group's witness is
/// published: an open whose publication fails leaves the permit in place, and the next open
/// adopts the exact group under it.
#[test]
fn a_writing_open_keeps_a_landed_groups_permit_until_its_witness_is_published() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("landed")).unwrap();
    let (scope, dir) = (store.scope, store.data_dir.clone());
    let wires = chain(&session, scope, 2);
    let failure = fail_witness_after_commit();
    assert!(
        store
            .ingest_batch(&session, &[&wires[0], &wires[1]])
            .is_err()
    );
    drop(failure);
    drop(store);
    {
        let _refused = aterm_uds::atomic_file::publication_seam::FailBeforeRename::with_kind(
            std::io::ErrorKind::PermissionDenied,
        );
        assert!(Store::open(&session, &dir, scope).is_err());
    }
    let store = Store::open(&session, &dir, scope).unwrap();
    assert_eq!(store.witness.count, 2);
    assert_eq!(store.records.len(), 2);
}
