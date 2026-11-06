// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::{
    fs::OpenOptions,
    io,
    num::NonZeroU32,
    path::Path,
    time::{Duration, Instant},
};

use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
use aterm_uds::spawnfd::{self, Child, Command, Spawned, Stdio};

use crate::{
    Accounting,
    build_cpu::{Owner, RETAINED_DIRECTORY_NAME, RetainedPool, RetainedSlotState},
    cohort::{
        Lifetime,
        launch::{CohortChild, LaunchError},
    },
    leadership::Fence,
    ledger::CohortFixture,
    retained_cohort::RetainedCohort,
};

fn cpu_pool(fixture: &CohortFixture) -> RetainedPool {
    cpu_pool_width(fixture, 1)
}

fn cpu_pool_width(fixture: &CohortFixture, width: u32) -> RetainedPool {
    RetainedPool::create_private(
        &fixture
            .namespace
            .backend()
            .parent()
            .unwrap()
            .join("retained-cpu.noindex"),
        NonZeroU32::new(width).unwrap(),
    )
    .unwrap()
}

#[test]
fn retained_bundle_covers_registered_subgroup_after_original_root_exits() {
    let fixture = CohortFixture::new();
    let other = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let foreign = Fence::acquire(&other.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    assert!(
        retained
            .launch_subgroup(&foreign, &mut root.owned, &mut cat())
            .is_err()
    );
    let mut subgroup = Running::new(
        retained
            .launch_subgroup(&fence, &mut root.owned, &mut cat())
            .unwrap(),
    );
    assert_ne!(root.owned.cohort(), subgroup.owned.cohort());
    assert!(retained.retire_subgroup(&fence, &subgroup.owned).is_err());
    root.end();
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    subgroup.end();
    assert!(retained.retire_subgroup(&foreign, &subgroup.owned).is_err());
    retained.retire_subgroup(&fence, &subgroup.owned).unwrap();
    retained.try_finish(&fence).unwrap();
    assert_settled_cpu_available(&pool).settle().unwrap();
}

#[test]
fn cpu_bundle_releases_only_after_the_original_group_drains() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool_width(&fixture, 3);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let leases = (0..3)
        .map(|_| pool.try_acquire(owner()).unwrap().unwrap())
        .collect();
    let mut retained = RetainedCohort::new_bundle(&fence, fixture.claim(), leases).unwrap();
    assert_eq!(retained.cpu_units(), 3);
    assert!(retained.observation(&fence).is_err());
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let observation = retained.observation(&fence).unwrap();
    assert_eq!(observation.cpu.len(), 3);
    assert_eq!(observation.memory_bytes, 30 * 1024 * 1024 * 1024);
    assert!(
        observation
            .cpu
            .iter()
            .all(|cpu| cpu.manifest == observation.cpu[0].manifest)
    );
    let mut member = Running::new(
        retained
            .launch_member(&fence, &mut leader.owned, &mut cat())
            .unwrap(),
    );
    leader.end();
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    member.end();
    retained.try_finish(&fence).unwrap();
    assert!(retained.is_settled());
    assert!(retained.observation(&fence).is_err());
    assert_eq!(retained.cpu_units(), 3);
    assert!(
        pool.snapshot()
            .unwrap()
            .iter()
            .all(|slot| matches!(slot, RetainedSlotState::Available { .. }))
    );
}

#[test]
fn dropping_cpu_bundle_retains_every_unit_after_root_exit() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool_width(&fixture, 3);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let leases = (0..3)
        .map(|_| pool.try_acquire(owner()).unwrap().unwrap())
        .collect();
    let mut retained = RetainedCohort::new_bundle(&fence, fixture.claim(), leases).unwrap();
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    drop(retained);
    leader.end();
    wait_for_kernel_release(&pool);
    assert_cpu_denied(&pool);
    assert!(
        pool.snapshot()
            .unwrap()
            .iter()
            .all(|slot| matches!(slot, RetainedSlotState::Retained { .. }))
    );
}

#[test]
fn refused_cpu_bundle_returns_all_original_capabilities() {
    let fixture = CohortFixture::new();
    let other = CohortFixture::new();
    let pool = cpu_pool_width(&fixture, 2);
    let fence = Fence::acquire(&other.namespace, Instant::now()).unwrap();
    let leases = (0..2)
        .map(|_| pool.try_acquire(owner()).unwrap().unwrap())
        .collect();
    let error = RetainedCohort::new_bundle(&fence, fixture.claim(), leases).unwrap_err();
    let (_, claim, leases) = error.into_parts();
    assert_eq!(leases.len(), 2);
    assert_cpu_denied(&pool);
    let proper = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new_bundle(&proper, claim, leases).unwrap();
    retained.try_finish(&proper).unwrap();
    drop(retained);
    let error = RetainedCohort::new_bundle(&proper, fixture.claim(), Vec::new()).unwrap_err();
    assert!(error.into_parts().2.is_empty());
}

#[test]
fn cpu_bundle_partial_settlement_is_not_reported_or_retried_as_complete() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool_width(&fixture, 3);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let leases = (0..3)
        .map(|_| pool.try_acquire(owner()).unwrap().unwrap())
        .collect();
    let mut retained = RetainedCohort::new_bundle(&fence, fixture.claim(), leases).unwrap();
    let slots = pool.path().join(RETAINED_DIRECTORY_NAME);
    std::fs::write(slots.join("slot-00000001"), b"interrupted retained record").unwrap();
    assert!(retained.try_finish(&fence).is_err());
    assert!(!retained.is_settled());
    assert!(
        std::fs::read_to_string(slots.join("slot-00000002"))
            .unwrap()
            .contains("free=1")
    );
    assert!(
        std::fs::read_to_string(slots.join("slot-00000000"))
            .unwrap()
            .contains("retained=1")
    );
    assert!(retained.try_finish(&fence).is_err());
    assert!(!retained.is_settled());
}

fn owner() -> Owner {
    Owner::current(Some(123))
}

fn cat() -> Command {
    let mut command = spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/cat")));
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    command
}

trait ChildHandle {
    fn child(&mut self) -> &mut Child;
}

impl ChildHandle for Child {
    fn child(&mut self) -> &mut Child {
        self
    }
}

impl ChildHandle for CohortChild {
    fn child(&mut self) -> &mut Child {
        self.child_mut()
    }
}

impl ChildHandle for Spawned {
    fn child(&mut self) -> &mut Child {
        self.child_mut()
    }
}

struct Running<T: ChildHandle> {
    owned: T,
    input: Option<std::process::ChildStdin>,
}

impl<T: ChildHandle> Running<T> {
    fn new(owned: T) -> Self {
        let mut running = Self { owned, input: None };
        running.input = Some(aterm_uds::deadline::cat_echoes(running.owned.child()));
        running
    }

    fn end(&mut self) {
        self.input.take();
        let status = until_within(MUST_HAPPEN, "owned cat exit and reap", || {
            self.owned.child().try_wait().unwrap()
        });
        assert!(status.success());
    }
}

impl<T: ChildHandle> Drop for Running<T> {
    fn drop(&mut self) {
        self.input.take();
        let child = self.owned.child();
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        let _ = within(MUST_HAPPEN, Duration::from_millis(10), || {
            child.try_wait().ok().flatten()
        });
    }
}

fn assert_cpu_denied(pool: &RetainedPool) {
    assert!(pool.try_acquire(owner()).unwrap().is_none());
    assert!(
        pool.snapshot()
            .unwrap()
            .iter()
            .all(|state| !matches!(state, RetainedSlotState::Available { .. }))
    );
}

fn wait_for_kernel_release(pool: &RetainedPool) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            pool.path()
                .join(RETAINED_DIRECTORY_NAME)
                .join("slot-00000000"),
        )
        .unwrap();
    // A concurrent fork can retain a closed CLOEXEC descriptor until exec.
    // Observe actual kernel release without treating it as durable settlement;
    // callers separately check either the retained debt or the Free record.
    aterm_uds::file_lock::lock_exclusive_until(
        &file,
        Instant::now() + MUST_HAPPEN,
        "retained CPU's actual kernel release",
    )
    .unwrap();
    file.unlock().unwrap();
    drop(file);
}

fn assert_settled_cpu_available(pool: &RetainedPool) -> crate::build_cpu::RetainedLease {
    wait_for_kernel_release(pool);
    assert_eq!(
        pool.snapshot().unwrap(),
        [RetainedSlotState::Available { slot: 0 }]
    );
    pool.try_acquire(owner())
        .unwrap()
        .expect("the settled slot must have an explicit free record")
}

fn assert_kernel_free_but_debt_retained(pool: &RetainedPool) {
    wait_for_kernel_release(pool);
    let fresh = RetainedPool::open_existing(pool.path()).unwrap().unwrap();
    assert_cpu_denied(&fresh);
    assert_eq!(
        fresh.snapshot().unwrap(),
        [RetainedSlotState::Retained {
            slot: 0,
            reported_owner: owner()
        }]
    );
}

#[test]
fn leader_exit_keeps_cpu_until_the_actual_final_member_drains() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let lease = pool.try_acquire(owner()).unwrap().unwrap();
    let original = lease.slot();
    let mut retained = RetainedCohort::new(&fence, fixture.claim(), lease).unwrap();
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let cohort = leader.owned.cohort().clone();
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    let mut member = Running::new(
        retained
            .launch_member(&fence, &mut leader.owned, &mut cat())
            .unwrap(),
    );
    let lineage = aterm_uds::process::read_lineage(member.owned.id() as i32).unwrap();
    assert_eq!(lineage.pgid, cohort.group() as u32);
    assert_eq!(lineage.parent, std::process::id());
    leader.end();
    assert!(leader.owned.child_mut().is_reaped());
    assert!(!member.owned.exited().unwrap());
    assert_eq!(cohort.lifetime(), Lifetime::Present);
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    member.end();
    assert_eq!(cohort.lifetime(), Lifetime::Drained);
    retained.try_finish(&fence).unwrap();
    let replacement = assert_settled_cpu_available(&pool);
    assert_eq!(replacement.slot(), original);
    assert_cpu_denied(&pool);
    replacement.settle().unwrap();
}

#[test]
fn dropping_actual_custody_keeps_cpu_debt_after_kernel_release_and_drain() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let cohort = leader.owned.cohort().clone();
    drop(retained);
    assert_eq!(cohort.lifetime(), Lifetime::Present);
    assert_kernel_free_but_debt_retained(&pool);
    leader.end();
    assert_eq!(cohort.lifetime(), Lifetime::Drained);
    assert_kernel_free_but_debt_retained(&pool);
}

#[test]
fn durable_finish_failure_keeps_cpu_until_original_pending_event_is_retried() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let claim = fixture.claim();
    let claim_path = claim.path().to_owned();
    let mut retained =
        RetainedCohort::new(&fence, claim, pool.try_acquire(owner()).unwrap().unwrap()).unwrap();
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    leader.end();
    let before = std::fs::read(&claim_path).unwrap();
    fixture.fail_next_event_after_write();
    let error = retained.try_finish(&fence).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_ne!(
        std::fs::read(&claim_path).unwrap(),
        before,
        "the real journal write must occur"
    );
    assert_cpu_denied(&pool);
    // The actual Claim owns its pending write/origin. A preflight check of its
    // old committed bytes must not strand this legitimate retry forever.
    retained.try_finish(&fence).unwrap();
    let replacement = assert_settled_cpu_available(&pool);
    assert_eq!(replacement.slot(), 0);
    assert_cpu_denied(&pool);
    replacement.settle().unwrap();
}

#[test]
fn genuine_not_spawned_receipt_must_cancel_arm_before_cpu_can_be_freed() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-program"));
    let LaunchError::NotSpawned(proof) = retained.launch(&fence, &mut missing).unwrap_err() else {
        panic!("a genuine missing target must preserve its exact no-child receipt");
    };
    assert_eq!(proof.error().kind(), io::ErrorKind::NotFound);
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    retained.cancel(&fence, *proof).unwrap();
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Holder
    );
    assert_cpu_denied(&pool);
    retained.try_finish(&fence).unwrap();
    assert_settled_cpu_available(&pool).settle().unwrap();
}

#[test]
fn cancelled_launch_preserves_its_actual_receipt_or_pending_event_until_settlement() {
    for after_write in [false, true] {
        let fixture = CohortFixture::new();
        let other = CohortFixture::new();
        let pool = cpu_pool(&fixture);
        let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
        let foreign = Fence::acquire(&other.namespace, Instant::now()).unwrap();
        let claim = fixture.claim();
        let claim_path = claim.path().to_owned();
        let mut retained =
            RetainedCohort::new(&fence, claim, pool.try_acquire(owner()).unwrap().unwrap())
                .unwrap();
        let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-program"));
        let LaunchError::NotSpawned(proof) = retained.launch(&fence, &mut missing).unwrap_err()
        else {
            panic!("missing target must return its actual no-child receipt");
        };
        let rejected = retained.cancel(&foreign, *proof).unwrap_err();
        let (_, returned) = rejected.into_parts();
        let returned =
            returned.expect("a foreign fence must return the original cancellation receipt");
        assert_eq!(returned.error().kind(), io::ErrorKind::NotFound);
        assert!(retained.try_finish(&fence).is_err());
        assert_cpu_denied(&pool);

        let armed = std::fs::read(&claim_path).unwrap();
        if after_write {
            fixture.fail_next_event_after_write();
        } else {
            fixture.fail_next_event_before_write();
        }
        let failed = retained.cancel(&fence, returned).unwrap_err();
        let (error, returned) = failed.into_parts();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(
            returned.is_none(),
            "the original claim must own the accepted receipt even before an event exists"
        );
        let observed = std::fs::read(&claim_path).unwrap();
        if after_write {
            assert_ne!(observed, armed, "the actual cancellation write must occur");
        } else {
            assert_eq!(
                observed, armed,
                "the injected refusal must precede the cancellation write"
            );
        }
        assert_cpu_denied(&pool);
        retained.try_finish(&fence).unwrap();
        assert!(retained.is_settled());
        assert_settled_cpu_available(&pool).settle().unwrap();
    }
}

#[test]
fn actual_unbound_child_remains_owned_and_denies_cpu_until_bind_retry_and_drain() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    let error = retained.launch_with(&fence, &mut cat(), |_, spawned| {
        assert!(!spawned.child_mut().exited()?);
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected bind refusal after real birth",
        ))
    });
    let LaunchError::Unbound { spawned, error } = error.unwrap_err() else {
        panic!("bind refusal must return the actual owned child");
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let mut running = Running::new(*spawned);
    assert!(!running.owned.child_mut().exited().unwrap());
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    let cohort = retained.retry_bind(&fence, &mut running.owned).unwrap();
    assert_eq!(cohort.lifetime(), Lifetime::Present);
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    running.end();
    assert_eq!(cohort.lifetime(), Lifetime::Drained);
    retained.try_finish(&fence).unwrap();
    assert_settled_cpu_available(&pool).settle().unwrap();
}

#[test]
fn foreign_leadership_cannot_settle_original_custody_even_after_actual_drain() {
    let fixture = CohortFixture::new();
    let other = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let foreign = Fence::acquire(&other.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    leader.end();
    assert!(retained.try_finish(&foreign).is_err());
    assert_cpu_denied(&pool);
    retained.try_finish(&fence).unwrap();
    assert_settled_cpu_available(&pool).settle().unwrap();
}

#[test]
fn a_new_incarnation_in_the_same_namespace_cannot_settle_old_custody() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let original = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let sequence = original.sequence();
    let generation = original.generation();
    let mut retained = RetainedCohort::new(
        &original,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    drop(original);
    let successor = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    assert_eq!(successor.sequence(), sequence + 1);
    assert_ne!(successor.generation(), generation);
    successor.check_current().unwrap();
    assert!(retained.try_finish(&successor).is_err());
    assert!(!retained.is_settled());
    assert_cpu_denied(&pool);
    drop(retained);
    assert_kernel_free_but_debt_retained(&pool);
}

#[test]
fn restored_corrupt_namespace_or_leadership_never_revives_settlement_authority() {
    for component in ["namespace", "epoch"] {
        let fixture = CohortFixture::new();
        let pool = cpu_pool(&fixture);
        let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
        let mut retained = RetainedCohort::new(
            &fence,
            fixture.claim(),
            pool.try_acquire(owner()).unwrap().unwrap(),
        )
        .unwrap();
        let mut leader = Running::new(retained.launch(&fence, &mut cat()).unwrap());
        leader.end();
        let path = match component {
            "namespace" => fixture.namespace.fence(),
            "epoch" => fixture.namespace.backend().join(".host-owner.epoch"),
            _ => unreachable!(),
        };
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"torn authority").unwrap();
        assert!(retained.try_finish(&fence).is_err());
        assert_cpu_denied(&pool);
        std::fs::write(&path, original).unwrap();
        assert!(
            retained.try_finish(&fence).is_err(),
            "restoration revived {component}"
        );
        assert_cpu_denied(&pool);
        drop(retained);
        assert_kernel_free_but_debt_retained(&pool);
    }
}

#[test]
fn foreign_or_already_finished_claim_never_mints_a_new_settlement_authority() {
    for finished in [false, true] {
        let fixture = CohortFixture::new();
        let other = CohortFixture::new();
        let pool = cpu_pool(&fixture);
        let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
        let claim = if finished {
            let mut claim = fixture.claim();
            let mut leader = Running::new(claim.launch_cohort(&mut cat()).unwrap());
            leader.end();
            claim.finish_cohort().unwrap();
            claim
        } else {
            other.claim()
        };
        let original_path = claim.path().to_owned();
        let attempted =
            RetainedCohort::new(&fence, claim, pool.try_acquire(owner()).unwrap().unwrap());
        let (_, returned_claim, returned_cpu) = attempted
            .expect_err("foreign or previously finished custody must be refused")
            .into_parts();
        assert_eq!(returned_claim.path(), original_path);
        assert_eq!(returned_cpu.slot(), 0);
        assert_eq!(returned_cpu.owner(), owner());
        assert_cpu_denied(&pool);
        drop((returned_claim, returned_cpu));
        assert_kernel_free_but_debt_retained(&pool);
    }
}

#[test]
fn unused_admission_settles_once_and_cannot_release_a_later_unit_generation() {
    let fixture = CohortFixture::new();
    let pool = cpu_pool(&fixture);
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let mut retained = RetainedCohort::new(
        &fence,
        fixture.claim(),
        pool.try_acquire(owner()).unwrap().unwrap(),
    )
    .unwrap();
    retained.try_finish(&fence).unwrap();
    let replacement = assert_settled_cpu_available(&pool);
    let path = pool
        .path()
        .join(RETAINED_DIRECTORY_NAME)
        .join("slot-00000000");
    let original = std::fs::read(&path).unwrap();
    retained.try_finish(&fence).unwrap();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-program"));
    assert!(matches!(
        retained.launch(&fence, &mut missing),
        Err(LaunchError::Refused(_))
    ));
    assert_cpu_denied(&pool);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    replacement.settle().unwrap();
}
