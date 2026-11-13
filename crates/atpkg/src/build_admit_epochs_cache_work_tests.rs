// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::{cache_work::UnavailableReason, cohort_namespace::Namespace};
use std::os::unix::fs::DirBuilderExt as _;

fn completed(sequence: u64) -> Completed {
    // A private table fixture tests binding/publication, not native closure.
    // The maintained Owner probe supplies actual child/settlement evidence.
    let status = Status {
        opened: true,
        retired: true,
        helper_exited: true,
        control_eof: true,
        endpoint_cleaned: true,
        settled_units: 2,
        uncertain_units: 0,
        remaining_units: 0,
        completed_roots: 2,
        failed_roots: 0,
        faulted: false,
        unbound: false,
        not_spawned: false,
        cancellation_started: false,
    };
    Completed {
        row: row(sequence, 2, 2, 42, status, None),
        status,
        outcomes: (0..2)
            .map(|id| {
                Some(RootOutcome {
                    id,
                    pid: 1,
                    code: Some(0),
                    success: true,
                })
            })
            .collect(),
        cancellations: vec![None, None],
        helper: None,
        outputs: BTreeMap::new(),
    }
}

fn owner() -> (aterm_tempfile::TempDir, Owner) {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace = Namespace::create_private(world.path(), &home).unwrap();
    let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
    let owner = Owner::fenced(fence, 1, 16).unwrap();
    (world, owner)
}

#[test]
fn original_completed_binding_publishes_once_without_changing_resource_outcomes() {
    let (_world, mut owner) = owner();
    owner.epochs.history.push_back(completed(1));
    owner.publish(vec![(None, Event::StateChanged)]).unwrap();
    let id = EpochId {
        table: Arc::clone(&owner.epochs.seal),
        sequence: 1,
    };
    let status = owner.epoch_status(&id).unwrap();
    let outcome = owner.epoch_root_outcome(&id, 0).unwrap();
    let before = owner.observer().read(None).unwrap().snapshot;
    let unavailable = Collection::Unavailable(UnavailableReason::MissingSession);
    owner.epoch_record_cache_work(&id, 0, &unavailable).unwrap();
    let after = owner.observer().read(None).unwrap().snapshot;
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(
        after.epochs[0].cache_work,
        vec![RootCacheWork::from_collection(0, &unavailable)]
    );
    assert_eq!(owner.epoch_status(&id).unwrap(), status);
    assert_eq!(owner.epoch_root_outcome(&id, 0).unwrap(), outcome);
    assert!(owner.is_empty());
    assert_eq!(
        owner
            .epoch_record_cache_work(&id, 0, &unavailable)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        owner.observer().read(None).unwrap().snapshot.revision,
        after.revision
    );
    let foreign = EpochId {
        table: Arc::new(()),
        sequence: 1,
    };
    assert_eq!(
        owner
            .epoch_record_cache_work(&foreign, 1, &unavailable)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        owner
            .epoch_record_cache_work(&id, 2, &unavailable)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    owner.epochs.history.clear();
    assert_eq!(
        owner
            .epoch_record_cache_work(&id, 1, &unavailable)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn summary_capacity_refusal_preserves_the_exact_last_resource_snapshot() {
    let (_world, mut owner) = owner();
    for sequence in 1..=MAX_CACHE_WORK as u64 {
        let mut entry = completed(sequence);
        entry.row.cache_work.push(RootCacheWork::from_collection(
            0,
            &Collection::Unavailable(UnavailableReason::NotRecorded),
        ));
        owner.epochs.history.push_back(entry);
    }
    owner.publish(vec![(None, Event::StateChanged)]).unwrap();
    let before = owner.observer().read(None).unwrap().snapshot;
    let id = EpochId {
        table: Arc::clone(&owner.epochs.seal),
        sequence: 1,
    };
    let result = owner.epoch_record_cache_work(
        &id,
        1,
        &Collection::Unavailable(UnavailableReason::QueryFailed),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(*owner.observer().read(None).unwrap().snapshot, *before);
    assert!(before.encode().unwrap().len() < aterm_capacity::host_protocol::MAX_FRAME_BYTES);
}
