// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static BEFORE_COMMIT: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

pub(in super::super) fn before_commit() {
    BEFORE_COMMIT.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

pub(in super::super) struct ResetHook;

impl Drop for ResetHook {
    fn drop(&mut self) {
        BEFORE_COMMIT.with(|hook| *hook.borrow_mut() = None);
    }
}

pub(in super::super) fn install_hook(hook: impl FnMut() + 'static) -> ResetHook {
    BEFORE_COMMIT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
    ResetHook
}

struct Writer {
    seed: [u8; 32],
    incarnation: [u8; 16],
    actor: MemberId,
    recipient: Recipient,
}

impl Writer {
    fn name(&self, log: &Log, at: u64) -> Record {
        log.draft(
            &self.actor,
            at,
            Body::Name {
                name: format!("m{at}"),
            },
        )
        .sign(&self.seed, self.incarnation)
        .unwrap()
    }

    fn epoch_and_wrap(&self, log: &Log) -> [Record; 2] {
        let (draft, key) = log
            .draft_epoch(&self.actor, 20_000, EpochAudience::Commons, 1)
            .unwrap();
        let epoch = draft.sign(&self.seed, self.incarnation).unwrap();
        let mut after = log.clone();
        after.insert(epoch.clone()).unwrap();
        let wrap = after
            .draft_wrap(&self.actor, 20_001, *epoch.id(), &key, self.recipient)
            .unwrap()
            .sign(&self.seed, self.incarnation)
            .unwrap();
        [epoch, wrap]
    }
}

fn rooted() -> (Store, Writer) {
    let identity = Identity::from_secret_bytes([0x51; 32]);
    let seed = [0x52; 32];
    let incarnation = [0x53; 16];
    let request = Request::create(
        [0x54; 16],
        [0x55; 32],
        [0x56; 16],
        incarnation,
        &identity,
        &seed,
    )
    .unwrap();
    let actor = request.binding().actor();
    let founder = Entrant {
        request,
        node_key: [0x57; 32],
        name: "m1".into(),
        label: "m1".into(),
    };
    let genesis = Record::found(&[0x58; 32], vec![founder], 1_000).unwrap();
    let mut store = Store::for_vault("author-batch").unwrap();
    store.insert(genesis).unwrap();
    (
        store,
        Writer {
            seed,
            incarnation,
            actor,
            recipient: identity.recipient(),
        },
    )
}

fn prepare(root: &Path, bounded: bool, store: &Store) -> Paths {
    let paths = Paths::for_home(&root.join(if bounded { "bounded" } else { "ordinary" }));
    super::super::save_config(
        &paths,
        &Config {
            source: Source::Path(paths.home.join("test.vault")),
            mirrors: Vec::new(),
            name: "author-batch".into(),
            signer: Some(envelope::signer_public(&[0x58; 32]).unwrap()),
            followed: Vec::new(),
            host: "m1".into(),
        },
    )
    .unwrap();
    fsx::write_atomic(&paths.records_file(), &store.encode(), 0o600).unwrap();
    drop(StateLock::take(&paths).unwrap());
    paths
}

fn author<T>(
    paths: &Paths,
    bounded: bool,
    draft: impl FnMut(&Store) -> crate::Result<(Vec<Record>, T)>,
) -> crate::Result<(Vec<Record>, Store, T)> {
    if bounded {
        author_and_commit_records_until(
            paths,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            draft,
        )
        .map(|done| done.expect("an uncontended author finishes within the hang detector"))
    } else {
        author_and_commit_records(paths, draft)
    }
}

#[test]
fn final_ingress_cannot_publish_only_the_epoch_of_an_authored_batch() {
    crate::crossing::fixture::LEGACY.within(final_ingress_at_a_full_held_store);
}

fn final_ingress_at_a_full_held_store() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut store, writer) = rooted();
    while store.len() < crate::crossing::fixture::LEGACY.held - 5 {
        let record = writer.name(
            store.log().unwrap(),
            1_001 + u64::try_from(store.len()).unwrap(),
        );
        store.insert(record).unwrap();
    }
    let mut outcomes = Vec::new();
    for bounded in [false, true] {
        let paths = prepare(root.path(), bounded, &store);
        let calls = Rc::new(Cell::new(0));
        let last_peer_bytes = Rc::new(RefCell::new(Vec::new()));
        let peer_paths = paths.clone();
        let peer_calls = calls.clone();
        let peer_bytes = last_peer_bytes.clone();
        let peer_writer = Writer {
            seed: writer.seed,
            incarnation: writer.incarnation,
            actor: writer.actor,
            recipient: writer.recipient,
        };
        let _hook = install_hook(move || {
            let _state = StateLock::take(&peer_paths).unwrap();
            let mut current = super::super::load_records(&peer_paths).unwrap();
            let call = peer_calls.get() + 1;
            peer_calls.set(call);
            let incoming = peer_writer.name(current.log().unwrap(), 10_000 + call);
            current.insert(incoming).unwrap();
            let bytes = current.encode();
            fsx::write_atomic(&peer_paths.records_file(), &bytes, 0o600).unwrap();
            *peer_bytes.borrow_mut() = bytes;
        });
        let result = author(&paths, bounded, |snapshot| {
            let batch = writer.epoch_and_wrap(snapshot.log().unwrap()).to_vec();
            Ok((batch, ()))
        });
        let after = std::fs::read(paths.records_file()).unwrap();
        assert_eq!(calls.get(), 4, "three changed snapshots were redrafted");
        outcomes.push((bounded, result.is_err(), after == *last_peer_bytes.borrow()));
    }
    assert_eq!(
        outcomes,
        [(false, true, true), (true, true, true)],
        "each author must refuse the partial batch and retain exactly the peer's bytes"
    );
}

#[test]
fn a_local_batch_that_leaves_a_record_waiting_writes_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, writer) = rooted();
    let [_, waiting] = writer.epoch_and_wrap(store.log().unwrap());
    let mut outcomes = Vec::new();
    for bounded in [false, true] {
        let paths = prepare(root.path(), bounded, &store);
        let before = std::fs::read(paths.records_file()).unwrap();
        let result = author(&paths, bounded, |_| Ok((vec![waiting.clone()], ())));
        let after = std::fs::read(paths.records_file()).unwrap();
        outcomes.push((bounded, result.is_err(), after == before));
    }
    assert_eq!(
        outcomes,
        [(false, true, true), (true, true, true)],
        "waiting is not a committed local record, and neither writer may save a partial batch"
    );
}

#[test]
fn complete_local_batches_accept_known_records_and_promote_children() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, writer) = rooted();
    let [epoch, wrap] = writer.epoch_and_wrap(store.log().unwrap());
    let known = store.log().unwrap().genesis().clone();
    let batch = vec![known, wrap, epoch];
    for bounded in [false, true] {
        let paths = prepare(root.path(), bounded, &store);
        let (minted, after, out) = author(&paths, bounded, |_| Ok((batch.clone(), 17))).unwrap();
        assert_eq!(out, 17);
        assert_eq!(minted.len(), batch.len());
        assert!(
            batch
                .iter()
                .all(|record| after.complete_bytes(record.id()).is_some())
        );
        let on_disk = super::super::load_records(&paths).unwrap();
        assert_eq!(on_disk.inventory(), after.inventory());
        // A wholly known batch is also a successful no-op, even though it adds no records.
        author(&paths, bounded, |_| Ok((batch.clone(), ()))).unwrap();
    }
}

#[test]
#[ignore = "signed authoring observation measurement; run explicitly with --ignored --nocapture"]
fn signed_authoring_observation_cost() {
    use std::hint::black_box;
    const SAMPLES: usize = 32;
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut store, writer) = rooted();
    for n in 1..=1024 {
        if n > 1 {
            let record = writer.name(store.log().unwrap(), 1_000 + n);
            store.insert(record).unwrap();
        }
        if ![1, 128, 1024].contains(&n) {
            continue;
        }
        for bounded in [false, true] {
            let paths = prepare(&root.path().join(n.to_string()), bounded, &store);
            super::super::retained_history::acknowledge(&paths, &store).unwrap();
            let expected = store.log().unwrap().heads();
            author(&paths, bounded, |_| Ok((Vec::new(), ()))).unwrap();
            let mut samples = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                let started = Instant::now();
                let (_, snapshot, ()) =
                    black_box(author(&paths, bounded, |_| Ok((Vec::new(), ())))).unwrap();
                samples.push(started.elapsed().as_nanos());
                assert_eq!(snapshot.log().unwrap().heads(), expected);
            }
            samples.sort_unstable();
            eprintln!(
                "AUTHOR-OBSERVATION records={n} bounded={bounded} samples={SAMPLES} p50_ns={} p95_ns={} p99_ns={}",
                samples[SAMPLES / 2],
                samples[30],
                samples[31],
            );
        }
    }
}
