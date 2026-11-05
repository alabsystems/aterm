// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual refused births, original custody and independent native jobs.

use super::*;
use crate::build_admit::service::{EpochId, Owner};
use aterm_capacity::{
    build_cpu::{Owner as CpuOwner, RETAINED_DIRECTORY_NAME, RetainedLease, RetainedPool},
    cohort_namespace::Namespace,
    epoch_controller::Config,
    leadership::Fence,
};
use std::{fs, io::Write as _, num::NonZeroU32, os::unix::fs::MetadataExt as _, time::Duration};

fn pool(world: &Path) -> RetainedPool {
    RetainedPool::create_private(&world.join("cpu.noindex"), NonZeroU32::new(2).unwrap()).unwrap()
}
fn cpu(pool: &RetainedPool) -> Vec<RetainedLease> {
    (0..2)
        .map(|_| pool.try_acquire(CpuOwner::current(None)).unwrap().unwrap())
        .collect()
}
fn config(world: &Path, sequence: u32) -> Config {
    Config {
        helper: world.join("genuine-absent-helper"),
        directory: world.join(format!("transport-{sequence}.noindex")),
        epoch: format!("{sequence:032x}"),
        roots: 1,
    }
}
fn namespace(seat: &Seat<'_>) -> Namespace {
    Namespace::resolve(seat.ledger.as_ref().unwrap().dir())
        .unwrap()
        .unwrap()
}
fn tick(owner: &mut Owner) -> Vec<host::Completion> {
    owner
        .tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {})
        .unwrap()
}
fn finish(owner: &mut Owner, id: &EpochId) {
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let before = owner.epoch_status(id).unwrap().settled_units;
        tick(owner);
        let after = owner.epoch_status(id).unwrap().settled_units;
        assert!(
            after <= before + 1,
            "each tick settles at most one actual unit"
        );
        if after == 2 {
            break;
        }
        assert!(
            Instant::now() < until,
            "original no-child epoch never settled"
        );
        owner.wait(until).unwrap();
    }
    assert!(owner.is_empty());
}
fn slot_bytes(pool: &RetainedPool) -> Vec<Vec<u8>> {
    (0..2)
        .map(|id| {
            fs::read(
                pool.path()
                    .join(RETAINED_DIRECTORY_NAME)
                    .join(format!("slot-{id:08x}")),
            )
            .unwrap()
        })
        .collect()
}
fn identity(held: &Held) -> (u64, u64, Vec<u8>) {
    let metadata = fs::metadata(held.claim().path()).unwrap();
    (
        metadata.dev(),
        metadata.ino(),
        fs::read(held.claim().path()).unwrap(),
    )
}

#[test]
fn epoch_refusal_returns_original_inputs_without_arming_then_keeps_accepted_no_child_custody() {
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(world.path(), &machine);
    seat.own = None;
    let pool = pool(world.path());
    let held = host_table::held(world.path(), &seat);
    let original = identity(&held);
    let caps = cpu(&pool);
    let original_cpu = slot_bytes(&pool);
    let setup = config(world.path(), 1);
    let mut unfenced = Owner::new(1, 2).unwrap();
    let refusal = unfenced.submit_epoch(held, caps, setup).unwrap_err();
    let (_, held, caps, setup) = refusal.into_parts();
    assert_eq!(
        identity(&held),
        original,
        "refusal must precede ARM and preserve the exact journal inode and bytes"
    );
    assert_eq!(
        slot_bytes(&pool),
        original_cpu,
        "actual retained nonces remain unchanged"
    );
    assert!(!setup.directory.exists());
    let mut owner = Owner::fenced(
        Fence::acquire(&namespace(&seat), Instant::now()).unwrap(),
        1,
        2,
    )
    .unwrap();
    // Binding-construction refusal must likewise reassemble the same actual Claim.
    let mut invalid = setup.clone();
    invalid.helper = PathBuf::from("relative-helper");
    let refusal = owner.submit_epoch(held, caps, invalid).unwrap_err();
    let (_, held, caps, _) = refusal.into_parts();
    assert_eq!(identity(&held), original);
    assert_eq!(slot_bytes(&pool), original_cpu);
    let accepted = owner.submit_epoch(held, caps, setup).unwrap();
    assert_eq!(
        accepted.result.unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    let status = owner.epoch_status(&accepted.id).unwrap();
    assert!(status.not_spawned && !status.faulted);
    assert_eq!(status.remaining_units, 2);
    assert!(
        !owner.is_empty(),
        "no root is live, but the original epoch custody remains unresolved"
    );
    assert!(pool.try_acquire(CpuOwner::current(None)).unwrap().is_none());
    assert_eq!(
        unfenced.epoch_status(&accepted.id).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let snapshot = owner.observer().read(None).unwrap().snapshot;
    assert_eq!(snapshot.epochs.len(), 1);
    assert_eq!(snapshot.epochs[0].sequence, accepted.id.sequence());
    assert_eq!(
        aterm_capacity::host_protocol::Snapshot::decode(&snapshot.encode().unwrap()).unwrap(),
        *snapshot
    );
    tick(&mut owner);
    assert_eq!(
        owner.epoch_status(&accepted.id).unwrap().settled_units,
        1,
        "one epoch receives at most one CPU settlement per tick"
    );
    finish(&mut owner, &accepted.id);
    assert!(
        owner
            .epoch_status(&accepted.id)
            .unwrap()
            .cancellation_started
    );
    assert!(
        !owner.epoch_status(&accepted.id).unwrap().retired,
        "cancellation must not manufacture helper retirement"
    );
    assert!(owner.epoch_root_outcome(&accepted.id, 0).unwrap().is_none());
    assert_eq!(
        owner.epoch_close(&accepted.id).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists,
        "settled history supplies no control authority"
    );
    for cap in cpu(&pool) {
        cap.settle().unwrap();
    }
}

#[test]
fn full_epoch_table_refuses_before_arm_and_unrelated_root_still_completes() {
    use std::os::unix::fs::DirBuilderExt as _;
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(world.path(), &machine);
    seat.own = None;
    let pool = pool(world.path());
    let mut owner = Owner::fenced(
        Fence::acquire(&namespace(&seat), Instant::now()).unwrap(),
        1,
        2,
    )
    .unwrap();
    let held = host_table::held(world.path(), &seat);
    let setup = config(world.path(), 1);
    let obstruction = setup.directory.clone();
    let first = owner.submit_epoch(held, cpu(&pool), setup).unwrap();
    assert!(first.result.is_err());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&obstruction)
        .unwrap();
    let overflow_pool = RetainedPool::create_private(
        &world.path().join("overflow-cpu.noindex"),
        NonZeroU32::new(2).unwrap(),
    )
    .unwrap();
    let held = host_table::held(world.path(), &seat);
    let original = identity(&held);
    let caps = cpu(&overflow_pool);
    let before_cpu = slot_bytes(&overflow_pool);
    let setup = config(world.path(), 2);
    let refusal = owner.submit_epoch(held, caps, setup).unwrap_err();
    assert_eq!(refusal.error().kind(), std::io::ErrorKind::WouldBlock);
    let (_, held, caps, setup) = refusal.into_parts();
    assert_eq!(identity(&held), original);
    assert_eq!(slot_bytes(&overflow_pool), before_cpu);
    assert!(!setup.directory.exists());
    // These actual unused capabilities may be explicitly settled by their
    // still-owning caller; no accepted epoch ever held them.
    for cap in caps {
        cap.settle().unwrap();
    }
    drop(held);
    let path = world.path().join("independent-root");
    let (mut command, mut input) = host_table::worker(&path, false);
    let job = owner
        .submit(host_table::held(&path, &seat), &mut command, Duration::ZERO)
        .unwrap();
    input.write_all(b"finish\n").unwrap();
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let completed = tick(&mut owner);
        if let Some(done) = completed
            .iter()
            .find(|done| done.id.sequence() == job.sequence)
        {
            assert!(done.result.as_ref().unwrap().succeeded());
            break;
        }
        assert!(
            Instant::now() < until,
            "ordinary root blocked behind unresolved epoch"
        );
        owner.wait(until).unwrap();
    }
    assert!(!owner.is_empty());
    let status = owner.epoch_status(&first.id).unwrap();
    assert!(status.cancellation_started && !status.endpoint_cleaned);
    assert_eq!(status.remaining_units, 2);
    assert_eq!(status.settled_units, 0);
    assert_eq!(status.uncertain_units, 0);
    fs::remove_dir(&obstruction).unwrap();
    finish(&mut owner, &first.id);
}

#[test]
fn original_no_child_receipt_survives_real_journal_lock_failure_and_recovers_automatically() {
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(world.path(), &machine);
    seat.own = None;
    let pool = pool(world.path());
    let mut owner = Owner::fenced(
        Fence::acquire(&namespace(&seat), Instant::now()).unwrap(),
        1,
        2,
    )
    .unwrap();
    // Both entry points share the same post-transfer retry schedule.
    for manual in [false, true] {
        let held = host_table::held(world.path(), &seat);
        let journal = held.claim().path().to_owned();
        let mutation = journal.parent().unwrap().join("mutation.lock");
        let accepted = owner
            .submit_epoch(
                held,
                cpu(&pool),
                config(world.path(), u32::from(manual) + 1),
            )
            .unwrap();
        assert!(accepted.result.is_err());
        assert!(
            owner.wait(Instant::now()).unwrap().jobs_hint,
            "the actual no-child receipt supplies immediate recovery work"
        );
        let armed = fs::read(&journal).unwrap();
        let inode = fs::metadata(&journal).unwrap().ino();
        let original_cpu = slot_bytes(&pool);
        let obstruction = aterm_uds::file_lock::LockFile::take_existing(
            &mutation,
            Instant::now(),
            "fixture cancellation obstruction",
        )
        .unwrap();
        if manual {
            assert_eq!(
                owner
                    .epoch_cancel_not_spawned(&accepted.id)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
        } else {
            tick(&mut owner);
        }
        let status = owner.epoch_status(&accepted.id).unwrap();
        assert!(status.cancellation_started && !status.not_spawned && !status.faulted);
        assert_eq!(status.remaining_units, 2);
        assert_eq!(status.settled_units, 0);
        assert_eq!(status.uncertain_units, 0);
        assert!(!owner.is_empty());
        assert_eq!(fs::read(&journal).unwrap(), armed);
        assert_eq!(fs::metadata(&journal).unwrap().ino(), inode);
        assert_eq!(slot_bytes(&pool), original_cpu);
        assert!(pool.try_acquire(CpuOwner::current(None)).unwrap().is_none());
        drop(obstruction);
        // Removing the obstacle supplies no receipt and makes no control call.
        // Only the owner's ordinary tick/wait loop completes original custody.
        finish(&mut owner, &accepted.id);
        let status = owner.epoch_status(&accepted.id).unwrap();
        assert!(status.cancellation_started && status.endpoint_cleaned && !status.retired);
        assert_eq!(status.uncertain_units, 0);
        assert!(!journal.exists());
        for cap in cpu(&pool) {
            cap.settle().unwrap();
        }
    }
}

#[test]
fn original_no_child_cleanup_retries_and_settled_history_never_blocks_new_epochs() {
    use std::os::unix::fs::DirBuilderExt as _;
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(world.path(), &machine);
    seat.own = None;
    let pool = pool(world.path());
    let mut owner = Owner::fenced(
        Fence::acquire(&namespace(&seat), Instant::now()).unwrap(),
        1,
        1,
    )
    .unwrap();
    let setup = config(world.path(), 1);
    let path = setup.directory.clone();
    let first = owner
        .submit_epoch(host_table::held(world.path(), &seat), cpu(&pool), setup)
        .unwrap();
    assert!(first.result.is_err());
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    tick(&mut owner);
    let status = owner.epoch_status(&first.id).unwrap();
    assert!(status.cancellation_started && !status.endpoint_cleaned);
    assert_eq!(status.settled_units, 0);
    assert!(pool.try_acquire(CpuOwner::current(None)).unwrap().is_none());
    assert!(!owner.is_empty());
    fs::remove_dir(&path).unwrap(); // Only the empty fixture obstruction we just created.
    finish(&mut owner, &first.id);
    let second = owner
        .submit_epoch(
            host_table::held(world.path(), &seat),
            cpu(&pool),
            config(world.path(), 2),
        )
        .unwrap();
    assert!(second.result.is_err());
    assert_ne!(first.id, second.id);
    assert!(
        owner.epoch_status(&first.id).is_ok(),
        "settled observation remains until bounded eviction"
    );
    finish(&mut owner, &second.id);
    assert_eq!(
        owner.epoch_status(&first.id).unwrap_err().kind(),
        std::io::ErrorKind::NotFound,
        "aged-out history never infers success from a recycled number"
    );
    assert_eq!(
        owner.observer().read(None).unwrap().snapshot.epochs.len(),
        1
    );
    assert_eq!(owner.epoch_status(&second.id).unwrap().settled_units, 2);
}

#[test]
fn revoked_leadership_keeps_original_debt_without_perpetual_immediate_waits() {
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
    let world = aterm_tempfile::tempdir().unwrap();
    let machine = Fixed::new(Reading::of_gib(256, 200, 128));
    let mut seat = cohort_seat(world.path(), &machine);
    seat.own = None;
    let namespace = namespace(&seat);
    let pool = pool(world.path());
    let mut owner =
        Owner::fenced(Fence::acquire(&namespace, Instant::now()).unwrap(), 1, 1).unwrap();
    let accepted = owner
        .submit_epoch(
            host_table::held(world.path(), &seat),
            cpu(&pool),
            config(world.path(), 1),
        )
        .unwrap();
    assert!(accepted.result.is_err());
    owner.epoch_cancel_not_spawned(&accepted.id).unwrap();
    tick(&mut owner);
    assert_eq!(owner.epoch_status(&accepted.id).unwrap().settled_units, 1);
    let epoch = namespace.backend().join(".host-owner.epoch");
    let preserved = namespace.backend().join("original-host-owner.epoch");
    fs::rename(&epoch, &preserved).unwrap();
    tick(&mut owner);
    let status = owner.epoch_status(&accepted.id).unwrap();
    assert_eq!(status.remaining_units, 1);
    assert!(!owner.is_empty());
    // Expired waits ask no native descriptor. A stale local immediate hint
    // would instead zero-poll and return jobs_hint=true, regardless of signals
    // from unrelated test children, so this distinguishes the spin deterministically.
    let wake = owner.wait(Instant::now()).unwrap();
    assert!(wake.deadline && !wake.jobs_hint);
    fs::rename(&preserved, &epoch).unwrap();
    tick(&mut owner);
    assert_eq!(
        owner.epoch_status(&accepted.id).unwrap().remaining_units,
        1,
        "restoring a path never restores a permanently revoked Fence"
    );
    drop(owner);
    let fresh = RetainedPool::open_existing(pool.path()).unwrap().unwrap();
    let available = fresh.try_acquire(CpuOwner::current(None)).unwrap().unwrap();
    assert!(
        fresh
            .try_acquire(CpuOwner::current(None))
            .unwrap()
            .is_none(),
        "lost original capability remains debt after actual descriptor release"
    );
    available.settle().unwrap();
    let evidence = world.keep();
    eprintln!(
        "kept revoked-owner retained debt evidence: {}",
        evidence.display()
    );
}
