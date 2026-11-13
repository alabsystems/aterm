// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    FailedPresentRoute, cpu_present_needs_full_copy, cpu_surface_transaction, failed_present_route,
};
use aterm_render::DamageOutcome;

#[derive(Debug)]
struct FakeBuffer;

#[derive(Debug)]
struct FakeSurfaceError;

#[test]
fn acquire_and_commit_failures_are_dropped_not_counted_as_presents() {
    use crate::metrics::PresentDropReason;
    let acquire = cpu_surface_transaction(Err::<FakeBuffer, _>(FakeSurfaceError), |_| Ok(11));
    assert_eq!(acquire, Err(PresentDropReason::CpuAcquire));
    let commit = cpu_surface_transaction(Ok(FakeBuffer), |_| Err::<u64, _>(FakeSurfaceError));
    assert_eq!(commit, Err(PresentDropReason::CpuCommit));
    let ok = cpu_surface_transaction(Ok::<_, FakeSurfaceError>(FakeBuffer), |_| Ok(17));
    assert_eq!(ok, Ok(17));
}

#[test]
fn a_translated_cpu_frame_and_the_one_after_it_copy_in_full() {
    use DamageOutcome::{Full, GateHit};
    for (case, want) in [
        ((false, false, false, GateHit, true), false), // control
        ((false, true, false, GateHit, true), true),   // sub-row frame
        ((false, false, true, GateHit, true), true),   // settle frame
        ((false, false, false, Full, true), true),
        ((false, false, false, GateHit, false), true), // surface lost the frame
        ((true, false, false, GateHit, true), true),   // caller-forced
    ] {
        let (forced, translated, prev, damage, retained) = case;
        assert_eq!(
            cpu_present_needs_full_copy(forced, translated, prev, damage, retained),
            want,
            "{case:?}"
        );
    }
}

#[test]
fn latched_gpu_loss_routes_to_fallback_before_surface_retry() {
    assert_eq!(
        failed_present_route(false, true),
        FailedPresentRoute::RetrySurface
    );
}
