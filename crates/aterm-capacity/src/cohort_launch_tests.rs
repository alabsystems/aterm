// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Accounting, Fixed, Ledger, Need, Polled, Reading, Waiter, ledger::CohortFixture};
use aterm_uds::spawnfd::{self, Stdio};

struct Cat {
    child: Child,
    input: Option<std::process::ChildStdin>,
}

impl Cat {
    fn take(spawned: Spawned) -> Self {
        Self::of_child(spawned.into_child())
    }

    fn of_child(child: Child) -> Self {
        let mut running = Self { child, input: None };
        running.input = Some(aterm_uds::deadline::cat_echoes(&mut running.child));
        running
    }

    fn end(&mut self) {
        self.input.take();
        assert!(self.child.wait().unwrap().success());
    }
}

struct Leader {
    launched: CohortChild,
    input: Option<std::process::ChildStdin>,
}

impl Leader {
    fn new(launched: CohortChild) -> Self {
        let mut running = Self {
            launched,
            input: None,
        };
        running.input = Some(aterm_uds::deadline::cat_echoes(
            running.launched.child_mut(),
        ));
        running
    }

    fn end(&mut self) {
        self.input.take();
        assert!(self.launched.child_mut().wait().unwrap().success());
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        self.input.take();
        let child = self.launched.child_mut();
        if matches!(child.exited(), Ok(false)) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

impl Drop for Cat {
    fn drop(&mut self) {
        self.input.take();
        if matches!(self.child.exited(), Ok(false)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn cat() -> Command {
    let mut command = spawnfd::command("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    command
}

#[test]
fn launch_binds_actual_birth_before_returning_success() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let launched = claim.launch_cohort(&mut cat()).unwrap();
    assert!(claim.cohort_bound(launched.cohort()));
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    let (spawned, cohort) = launched.into_parts();
    let mut running = Cat::take(spawned);
    assert!(claim.cohort_bound(&cohort));
    assert_eq!(cohort.lifetime(), super::super::Lifetime::Present);
    running.end();
    assert_eq!(cohort.lifetime(), super::super::Lifetime::Drained);
    claim.finish_cohort().unwrap();
    drop(claim);
    assert!(fixture.ledger.list().unwrap().is_empty());
}

#[test]
fn launch_failure_returns_the_exact_no_child_cancellation_receipt() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut command = spawnfd::command(fixture.namespace.backend().join("missing-program"));
    let LaunchError::NotSpawned(proof) = claim.launch_cohort(&mut command).unwrap_err() else {
        panic!("missing target did not retain its genuine no-child outcome");
    };
    assert_eq!(proof.error().kind(), io::ErrorKind::NotFound);
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    claim.cancel_cohort(*proof).unwrap();
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Holder
    );
    drop(claim);
    assert!(fixture.ledger.list().unwrap().is_empty());
}

#[test]
fn dropping_the_controller_does_not_release_a_running_native_launch() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let launched = claim.launch_cohort(&mut cat()).unwrap();
    let reference = claim.cohort_closure_ref().unwrap();
    let expected = claim.record().need.mem;
    let (spawned, cohort) = launched.into_parts();
    let mut running = Cat::take(spawned);
    drop(claim);
    let retained = fixture.ledger.list().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].need.mem, expected);
    assert_eq!(retained[0].accounting, Accounting::Whole);
    assert_eq!(cohort.lifetime(), super::super::Lifetime::Present);
    running.end();
    assert_eq!(cohort.lifetime(), super::super::Lifetime::Drained);
    assert!(matches!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        crate::CohortClaimState::Closed
    ));
    assert!(fixture.ledger.list().unwrap().is_empty());
}

#[test]
fn failed_binding_returns_the_live_owned_child_for_recovery() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let result = launch_with(&mut claim, &mut cat(), |_, spawned| {
        assert!(!spawned.child_mut().exited()?);
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected bind failure",
        ))
    });
    let LaunchError::Unbound { mut spawned, error } = result.unwrap_err() else {
        panic!("binding failure lost the owned child");
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    // Retry the real durable bind with the original owned-child receipt.
    let cohort = claim.bind_cohort(&mut spawned).unwrap();
    assert!(claim.cohort_bound(&cohort));
    let mut running = Cat::take(*spawned);
    running.end();
    claim.finish_cohort().unwrap();
}

#[test]
fn already_armed_claim_refuses_before_another_target_attempt() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let original = claim.arm_cohort().unwrap();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-program"));
    assert!(matches!(
        claim.launch_cohort(&mut missing),
        Err(LaunchError::Refused(_))
    ));
    claim.cancel_cohort(original.cancel().unwrap()).unwrap();
    drop(claim);
    assert!(fixture.ledger.list().unwrap().is_empty());
}

#[test]
fn holder_only_claim_cannot_launch_a_cohort() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("legacy"));
    let machine = Fixed::new(Reading::of_gib(64, 40, 8));
    let mut claim = match Waiter::enter(&ledger, &machine, Need::new("legacy"))
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("legacy admission failed: {other:?}"),
    };
    let mut missing = spawnfd::command(scratch.path().join("missing-program"));
    assert!(matches!(
        claim.launch_cohort(&mut missing),
        Err(LaunchError::Refused(_))
    ));
    assert_eq!(ledger.list().unwrap()[0].accounting, Accounting::Holder);
}

#[test]
fn concurrent_members_retain_one_full_reservation_until_the_final_drain() {
    use crate::{CohortClaimState, cohort::Lifetime};

    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    let cohort = leader.launched.cohort().clone();
    let reference = claim.cohort_closure_ref().unwrap();
    let expected = claim.record().need.mem;
    let mut first = Cat::of_child(
        claim
            .launch_cohort_member(&mut leader.launched, &mut cat())
            .unwrap(),
    );
    let mut second = Cat::of_child(
        claim
            .launch_cohort_member(&mut leader.launched, &mut cat())
            .unwrap(),
    );
    let machine = Fixed::new(Reading::of_gib(64, 40, 8));
    for child in [&first.child, &second.child] {
        let lineage = aterm_uds::process::read_lineage(child.id() as i32).unwrap();
        assert_eq!(lineage.pgid, cohort.group() as u32);
        assert_eq!(lineage.parent, std::process::id());
    }
    let retained = || {
        let rows = fixture.ledger.list().unwrap();
        assert_eq!(rows.len(), 1, "members share one reservation");
        assert_eq!(rows[0].need.mem, expected);
        assert_eq!(rows[0].accounting, Accounting::Whole);
        assert_eq!(cohort.lifetime(), Lifetime::Present);
        assert_eq!(
            fixture.ledger.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Retained
        );
        assert!(matches!(
            Waiter::enter(
                &fixture.ledger,
                &machine,
                Need::new("competing").mem_gib(20)
            )
            .unwrap()
            .poll(&machine),
            Polled::Waiting(_, _)
        ));
    };
    retained();
    drop(claim);
    leader.end();
    retained();
    first.end();
    retained();
    second.end();
    assert_eq!(cohort.lifetime(), Lifetime::Drained);
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Closed
    );
    assert!(fixture.ledger.list().unwrap().is_empty());
    assert!(matches!(
        Waiter::enter(
            &fixture.ledger,
            &machine,
            Need::new("competing").mem_gib(20)
        )
        .unwrap()
        .poll(&machine),
        Polled::Admitted(_)
    ));
}

#[test]
fn member_launch_refuses_a_different_claim_before_attempting_the_target() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    let other_fixture = CohortFixture::new();
    let mut other = other_fixture.claim();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-member"));
    assert_eq!(
        other
            .launch_cohort_member(&mut leader.launched, &mut missing)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    // Being bound to a different actual group still supplies no matching authority.
    let mut other_leader = Leader::new(other.launch_cohort(&mut cat()).unwrap());
    assert_eq!(
        other
            .launch_cohort_member(&mut leader.launched, &mut missing)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(claim.cohort_bound(leader.launched.cohort()));
    leader.end();
    other_leader.end();
    claim.finish_cohort().unwrap();
    other.finish_cohort().unwrap();
}

#[test]
fn member_launch_requires_the_original_live_leader_even_with_a_surviving_member() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut member = Cat::of_child(
        claim
            .launch_cohort_member(&mut leader.launched, &mut cat())
            .unwrap(),
    );
    leader.end();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-member"));
    assert_eq!(
        claim
            .launch_cohort_member(&mut leader.launched, &mut missing)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(claim.cohort_bound(leader.launched.cohort()));
    assert!(
        claim.finish_cohort().is_err(),
        "a member still holds the entire group"
    );
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    member.end();
    claim.finish_cohort().unwrap();
}

#[test]
fn failed_member_spawn_cannot_cancel_the_existing_binding() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-member"));
    assert_eq!(
        claim
            .launch_cohort_member(&mut leader.launched, &mut missing)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(claim.cohort_bound(leader.launched.cohort()));
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    let mut member = Cat::of_child(
        claim
            .launch_cohort_member(&mut leader.launched, &mut cat())
            .unwrap(),
    );
    leader.end();
    assert!(claim.finish_cohort().is_err());
    member.end();
    claim.finish_cohort().unwrap();
}

#[test]
fn member_launch_refuses_a_restored_description_before_attempting_the_target() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let path = claim.path().to_owned();
    let description = std::fs::read(&path).unwrap();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    std::fs::write(&path, description).unwrap();
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-member"));
    assert_eq!(
        claim
            .launch_cohort_member(&mut leader.launched, &mut missing)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(fixture.ledger.list().is_err());
    leader.end();
    assert!(claim.finish_cohort().is_err());
    drop(claim);
    assert!(
        path.exists(),
        "unknown custody cannot retire a restored description"
    );
}

#[test]
fn member_session_and_tether_refusals_leave_the_bound_reservation_intact() {
    use aterm_uds::spawnfd::Tether as _;

    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut leader = Leader::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut session = cat();
    session.setsid(true);
    assert_eq!(
        claim
            .launch_cohort_member(&mut leader.launched, &mut session)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let mut tethered = cat();
    tethered.tether();
    assert_eq!(
        claim
            .launch_cohort_member(&mut leader.launched, &mut tethered)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(claim.cohort_bound(leader.launched.cohort()));
    assert_eq!(
        fixture.ledger.list().unwrap()[0].accounting,
        Accounting::Whole
    );
    leader.end();
    claim.finish_cohort().unwrap();
}
