// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(target_vendor = "apple")]
use crate::retained_cohort::GnuLoanClosure;
use std::{fs::File, io::Write as _};

fn prepared(
    fixture: &CohortFixture,
    fence: &Fence,
    width: u32,
) -> (RetainedCohort, RetainedPool, File) {
    let pool = cpu_pool_width(fixture, width);
    let leases = (0..width)
        .map(|_| pool.try_acquire(owner()).unwrap().unwrap())
        .collect();
    let mut retained = RetainedCohort::new_bundle(fence, fixture.claim(), leases).unwrap();
    let ([read, write], _) = retained
        .prepare_gnu(fence, NonZeroU32::new(width).unwrap())
        .unwrap()
        .into_parts();
    drop(read);
    (retained, pool, File::from(write))
}

#[test]
fn native_gnu_loan_reads_real_byte_and_drop_defeats_forged_replacement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_loan_reads_real_byte_and_drop_defeats_forged_replacement",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let (mut retained, pool, mut write) = prepared(&fixture, &fence, 2);
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
    assert!(
        retained.try_loan_gnu(&fence).unwrap().is_none(),
        "the actual sole supplementary byte was consumed"
    );
    write.write_all(b"+").unwrap();
    assert!(
        retained.try_loan_gnu(&fence).unwrap().is_none(),
        "a forged replacement cannot fund another native borrower"
    );
    drop(loan);
    root.end();
    assert_eq!(
        retained.try_finish(&fence).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!retained.is_settled());
    assert_cpu_denied(&pool);
    drop(retained);
    assert_cpu_denied(&pool);
}

#[test]
fn native_gnu_unused_return_requires_same_epoch_and_is_one_use() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_unused_return_requires_same_epoch_and_is_one_use",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let other = CohortFixture::new();
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let foreign = Fence::acquire(&other.namespace, Instant::now()).unwrap();
    let (mut retained, pool, _write) = prepared(&fixture, &fence, 2);
    let (mut second, _second_pool, _second_write) = prepared(&other, &foreign, 2);
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let mut second_root = Running::new(second.launch(&foreign, &mut cat()).unwrap());
    let mut loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
    let mut second_loan = second.try_loan_gnu(&foreign).unwrap().unwrap();
    let proof = retained.cancel_unused_gnu_loan(&fence, &mut loan).unwrap();
    let (_, proof) = second
        .return_gnu_loan(&foreign, &mut second_loan, proof)
        .unwrap_err()
        .into_parts();
    retained
        .return_gnu_loan(&fence, &mut loan, proof.unwrap())
        .unwrap();
    assert!(retained.cancel_unused_gnu_loan(&fence, &mut loan).is_err());
    assert!(
        retained.try_loan_gnu(&fence).unwrap().is_some(),
        "one actual byte returned, without creating another epoch"
    );
    // Losing that new actual loan remains restrictive even after the root dies.
    root.end();
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    let proof = second
        .cancel_unused_gnu_loan(&foreign, &mut second_loan)
        .unwrap();
    second
        .return_gnu_loan(&foreign, &mut second_loan, proof)
        .unwrap();
    second_root.end();
    second.try_finish(&foreign).unwrap();
}

#[test]
fn native_gnu_single_cpu_query_route_is_explicitly_unsupported() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_single_cpu_query_route_is_explicitly_unsupported",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let (mut retained, pool, _write) = prepared(&fixture, &fence, 1);
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    assert_eq!(
        retained.try_loan_gnu(&fence).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    root.end();
    retained.try_finish(&fence).unwrap();
    assert_settled_cpu_available(&pool).settle().unwrap();
}

#[cfg(target_vendor = "apple")]
#[test]
fn native_gnu_loan_returns_only_after_same_owning_reap_and_durable_subgroup() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_loan_returns_only_after_same_owning_reap_and_durable_subgroup",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let (mut retained, pool, _write) = prepared(&fixture, &fence, 2);
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let mut loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
    let mut command = cat();
    let mut child = retained
        .launch_loaned_subgroup(&fence, &mut root.owned, &mut command, &mut loan)
        .unwrap();
    assert!(command.starts_suspended());
    assert!(retained.cancel_unused_gnu_loan(&fence, &mut loan).is_err());
    assert!(
        retained
            .retire_loaned_subgroup(&fence, &mut child, &mut loan)
            .is_err()
    );
    assert!(
        retained
            .retire_loaned_subgroup(&fence, &mut root.owned, &mut loan)
            .is_err()
    );
    child.child_mut().resume().unwrap();
    let mut child = Running::new(child);
    child.end();
    root.end();
    assert!(
        retained.try_finish(&fence).is_err(),
        "owning reap does not clear the epoch ledger"
    );
    assert_cpu_denied(&pool);
    fixture.fail_next_event_before_write();
    assert!(
        retained
            .retire_loaned_subgroup(&fence, &mut child.owned, &mut loan)
            .is_err()
    );
    assert!(retained.try_finish(&fence).is_err());
    assert_cpu_denied(&pool);
    let retired = retained
        .retire_loaned_subgroup(&fence, &mut child.owned, &mut loan)
        .unwrap();
    assert!(
        retained.try_finish(&fence).is_err(),
        "an issued receipt has not yet returned the actual byte"
    );
    retained
        .return_gnu_loan(&fence, &mut loan, GnuLoanClosure::Retired(retired))
        .unwrap();
    assert!(
        retained
            .retire_loaned_subgroup(&fence, &mut child.owned, &mut loan)
            .is_err()
    );
    retained.try_finish(&fence).unwrap();
    assert!(retained.is_settled());
}

#[cfg(target_vendor = "apple")]
#[test]
fn native_gnu_actual_unbound_child_keeps_same_loan_through_bind_retry() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_actual_unbound_child_keeps_same_loan_through_bind_retry",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
    let (mut retained, _pool, _write) = prepared(&fixture, &fence, 2);
    let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
    let mut loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
    let error = retained
        .launch_loaned_subgroup_with(&fence, &mut root.owned, &mut cat(), &mut loan, |_, _| {
            Err(io::Error::other("native loan test interrupted before BIND"))
        })
        .unwrap_err();
    let LaunchError::Unbound { spawned, .. } = error else {
        panic!("the actual suspended child must survive BIND failure")
    };
    assert!(retained.cancel_unused_gnu_loan(&fence, &mut loan).is_err());
    let mut child = retained
        .bind_loaned_subgroup(&fence, *spawned, &mut loan)
        .unwrap();
    child.child_mut().resume().unwrap();
    let mut child = Running::new(child);
    child.end();
    let retired = retained
        .retire_loaned_subgroup(&fence, &mut child.owned, &mut loan)
        .unwrap();
    retained
        .return_gnu_loan(&fence, &mut loan, GnuLoanClosure::Retired(retired))
        .unwrap();
    root.end();
    retained.try_finish(&fence).unwrap();
}

#[cfg(target_vendor = "apple")]
#[test]
fn native_gnu_no_child_refunds_only_after_same_persisted_cancellation() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "retained_cohort_tests::loans::native_gnu_no_child_refunds_only_after_same_persisted_cancellation",
    ) {
        return;
    }
    for fail in [false, true] {
        let fixture = CohortFixture::new();
        let fence = Fence::acquire(&fixture.namespace, Instant::now()).unwrap();
        let (mut retained, pool, _write) = prepared(&fixture, &fence, 2);
        let mut root = Running::new(retained.launch(&fence, &mut cat()).unwrap());
        let mut loan = retained.try_loan_gnu(&fence).unwrap().unwrap();
        let mut missing =
            spawnfd::command(fixture.namespace.backend().join("missing-loaned-target"));
        let LaunchError::NotSpawned(proof) = retained
            .launch_loaned_subgroup(&fence, &mut root.owned, &mut missing, &mut loan)
            .unwrap_err()
        else {
            panic!("missing native target must retain original no-child receipt")
        };
        let foreign = spawnfd::BirthAttempt::new().unwrap().cancel().unwrap();
        assert!(
            retained
                .cancel_loaned_subgroup(&fence, foreign, &mut loan)
                .is_err()
        );
        if fail {
            fixture.fail_next_event_before_write();
        }
        let closed = retained.cancel_loaned_subgroup(&fence, *proof, &mut loan);
        root.end();
        if fail {
            assert!(
                closed.unwrap_err().into_parts().1.is_none(),
                "the actual claim retained cancellation custody"
            );
            assert!(retained.cancel_unused_gnu_loan(&fence, &mut loan).is_err());
            assert!(retained.try_finish(&fence).is_err());
            assert_cpu_denied(&pool);
        } else {
            retained
                .return_gnu_loan(&fence, &mut loan, closed.unwrap())
                .unwrap();
            retained.try_finish(&fence).unwrap();
            assert!(retained.is_settled());
        }
    }
}
