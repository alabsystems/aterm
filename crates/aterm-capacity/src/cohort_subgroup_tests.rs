// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{CohortClaimState, cohort_journal::Journal, ledger::CohortFixture};
use aterm_uds::spawnfd::{self, Stdio};

struct Running {
    launched: CohortChild,
    input: Option<std::process::ChildStdin>,
}

impl Running {
    fn new(mut launched: CohortChild) -> Self {
        let input = Some(aterm_uds::deadline::cat_echoes(launched.child_mut()));
        Self { launched, input }
    }

    fn end(&mut self) {
        self.input.take();
        assert!(self.launched.child_mut().wait().unwrap().success());
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.input.take();
        let child = self.launched.child_mut();
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn cat() -> Command {
    let mut command = spawnfd::command("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    command
}

#[test]
fn subgroup_root_death_and_drop_cannot_close_live_registered_groups() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let reference = claim.cohort_closure_ref().unwrap();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut first = Running::new(
        claim
            .launch_subgroup(&mut root.launched, &mut cat())
            .unwrap(),
    );
    let mut second = Running::new(
        claim
            .launch_subgroup(&mut root.launched, &mut cat())
            .unwrap(),
    );
    assert_ne!(root.launched.cohort.group(), first.launched.cohort.group());
    assert_ne!(
        first.launched.cohort.group(),
        second.launched.cohort.group()
    );
    assert_eq!(
        claim.retire_subgroup(&first.launched).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    root.end();
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(claim);
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Retained
    );
    first.end();
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Retained
    );
    second.end();
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Closed
    );
}

#[test]
fn subgroup_unbound_actual_child_can_retry_after_both_leaders_die() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "cohort::launch::subgroup_tests::subgroup_unbound_actual_child_can_retry_after_both_leaders_die",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let reference = claim.cohort_closure_ref().unwrap();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let error = launch_subgroup_with(&mut claim, &mut root.launched, &mut cat(), |_, _| {
        Err(io::Error::other("injected before BIND"))
    })
    .unwrap_err();
    let LaunchError::Unbound { mut spawned, .. } = error else {
        panic!("actual child lost")
    };
    let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
    root.end();
    drop(input);
    spawned.child_mut().wait().unwrap();
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let subgroup = claim.bind_subgroup(*spawned).unwrap();
    claim.retire_subgroup(&subgroup).unwrap();
    claim.finish_cohort().unwrap();
    drop(claim);
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Closed
    );
}

#[test]
fn subgroup_exact_no_child_receipt_and_interrupted_bind_keep_custody() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut missing = spawnfd::command(fixture.namespace.backend().join("missing-target"));
    let LaunchError::NotSpawned(proof) = claim
        .launch_subgroup(&mut root.launched, &mut missing)
        .unwrap_err()
    else {
        panic!("no exact no-child receipt")
    };
    assert!(
        claim
            .cancel_subgroup(spawnfd::BirthAttempt::new().unwrap().cancel().unwrap())
            .is_err()
    );
    fixture.fail_next_event_after_write();
    assert!(claim.cancel_subgroup(*proof).is_err());
    // A later ARM must first commit this exact retained cancellation.
    for after_write in [false, true] {
        let error = launch_subgroup_with(
            &mut claim,
            &mut root.launched,
            &mut cat(),
            |claim, spawned| {
                if after_write {
                    fixture.fail_next_event_after_write();
                } else {
                    fixture.fail_next_event_before_write();
                }
                claim.bind_subgroup_actual(spawned)
            },
        )
        .unwrap_err();
        let LaunchError::Unbound { mut spawned, .. } = error else {
            panic!("actual child lost")
        };
        let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
        drop(input);
        spawned.child_mut().wait().unwrap();
        let subgroup = claim.bind_subgroup(*spawned).unwrap();
        // The cached actual capture must follow a before-write retry across leader death,
        // so a second binding of the SAME receipt stays idempotent after the first succeeds.
        let (spawned, _) = subgroup.into_parts();
        let subgroup = claim.bind_subgroup(spawned).unwrap();
        claim.retire_subgroup(&subgroup).unwrap();
    }
    root.end();
    claim.finish_cohort().unwrap();
}

#[test]
fn subgroup_partial_bind_preserves_child_and_never_supplies_closure() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let reference = claim.cohort_closure_ref().unwrap();
    let path = claim.path().to_owned();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let error = launch_subgroup_with(
        &mut claim,
        &mut root.launched,
        &mut cat(),
        |claim, spawned| {
            fixture.fail_next_event_partial_write();
            claim.bind_subgroup_actual(spawned)
        },
    )
    .unwrap_err();
    let LaunchError::Unbound { mut spawned, .. } = error else {
        panic!("actual child lost")
    };
    let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
    root.end();
    drop(input);
    spawned.child_mut().wait().unwrap();
    let torn = std::fs::read(&path).unwrap();
    assert!(claim.bind_subgroup(*spawned).is_err());
    assert!(claim.finish_cohort().is_err());
    drop(claim);
    assert!(!matches!(
        fixture.ledger.reap_cohort_claim(&reference),
        Ok(CohortClaimState::Closed)
    ));
    assert_eq!(
        std::fs::read(path).unwrap(),
        torn,
        "unknown partial bytes remain preserved"
    );
}

#[test]
fn subgroup_snapshot_pending_publication_recovers_under_cold_reaper() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let reference = claim.cohort_closure_ref().unwrap();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    fixture.interrupt_next_checkpoint_publication();
    // The original root's complete snapshot is covered before a subgroup ARM may start.
    assert!(matches!(
        claim.launch_subgroup(&mut root.launched, &mut cat()),
        Err(LaunchError::Refused(_))
    ));
    root.end();
    drop(claim);
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Closed
    );
}

#[test]
fn subgroup_retirement_compacts_real_snapshots_across_sequential_campaigns() {
    for boot_only in [false, true] {
        let fixture = CohortFixture::new();
        let mut claim = fixture.claim();
        if boot_only {
            claim.require_boot_closure().unwrap();
        }
        let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
        let mut longest = 0;
        for index in 0..24 {
            let mut subgroup = Running::new(
                claim
                    .launch_subgroup(&mut root.launched, &mut cat())
                    .unwrap(),
            );
            claim.wrote(index).unwrap();
            subgroup.end();
            claim.retire_subgroup(&subgroup.launched).unwrap();
            claim.wrote(index + 1).unwrap();
            let text = std::fs::read_to_string(claim.path()).unwrap();
            longest = longest.max(text.len());
            let (record, _) = Journal::decode(&text, true, fixture.namespace.nonce()).unwrap();
            assert_eq!(record.wrote, Some(index + 1));
            let rows = text
                .lines()
                .filter(|row| {
                    aterm_json::from_str::<aterm_json::Value>(row)
                        .unwrap()
                        .get("cohort")
                        .is_some()
                })
                .count();
            assert_eq!(rows, 1, "only the covered complete snapshot remains");
            assert_eq!(fixture.ledger.list().unwrap().len(), 1);
        }
        assert!(
            longest < 4096,
            "sequential work cannot grow custody history indefinitely"
        );
        root.end();
        let reference = claim.cohort_closure_ref().unwrap();
        if boot_only {
            assert_eq!(
                claim.finish_cohort().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        } else {
            claim.finish_cohort().unwrap();
        }
        drop(claim);
        assert_eq!(
            fixture.ledger.reap_cohort_claim(&reference).unwrap(),
            if boot_only {
                CohortClaimState::Retained
            } else {
                CohortClaimState::Closed
            }
        );
    }
}

#[test]
fn subgroup_snapshot_preserves_unknown_arms_and_rejects_missing_or_changed_custody() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let attempt = claim.arm_subgroup(root.launched.cohort()).unwrap();
    let before = std::fs::read_to_string(claim.path()).unwrap();
    let (_, journal) = Journal::decode(&before, true, fixture.namespace.nonce()).unwrap();
    let after = journal.compact(&before).unwrap();
    assert!(Journal::known_subgroup_compaction(&before, &after));
    let (ordinary, snapshot) = after.rsplit_once("{\"cohort\":").unwrap();
    let snapshot: aterm_json::Value =
        aterm_json::from_str(&format!("{{\"cohort\":{snapshot}")).unwrap();
    for field in ["root", "subgroups", "policy", "v"] {
        let mut invalid = snapshot.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .get_mut("cohort")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        let invalid = format!("{ordinary}{}\n", aterm_json::to_string(&invalid).unwrap());
        assert!(Journal::decode(&invalid, true, fixture.namespace.nonce()).is_err());
    }
    let mut lost_arm = snapshot.clone();
    lost_arm
        .as_object_mut()
        .unwrap()
        .get_mut("cohort")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("subgroups".into(), aterm_json::json!([]));
    let lost_arm = format!("{ordinary}{}\n", aterm_json::to_string(&lost_arm).unwrap());
    assert!(!Journal::known_subgroup_compaction(&before, &lost_arm));
    let changed_accounting = after.replacen("\"mem\":", "\"unknown_memory\":", 1);
    assert!(!Journal::known_subgroup_compaction(
        &before,
        &changed_accounting
    ));
    claim.cancel_subgroup(attempt.cancel().unwrap()).unwrap();
    root.end();
    claim.finish_cohort().unwrap();
}

#[test]
fn subgroup_foreign_root_and_nested_subgroup_cannot_mint_launch_authority() {
    let fixture = CohortFixture::new();
    let other_fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut other = other_fixture.claim();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    assert!(
        other
            .launch_subgroup(&mut root.launched, &mut cat())
            .is_err()
    );
    let mut subgroup = Running::new(
        claim
            .launch_subgroup(&mut root.launched, &mut cat())
            .unwrap(),
    );
    assert!(
        claim
            .launch_subgroup(&mut subgroup.launched, &mut cat())
            .is_err()
    );
    assert!(other.retire_subgroup(&subgroup.launched).is_err());
    subgroup.end();
    claim.retire_subgroup(&subgroup.launched).unwrap();
    root.end();
    assert!(
        claim
            .launch_subgroup(&mut root.launched, &mut cat())
            .is_err()
    );
    claim.finish_cohort().unwrap();
}

#[test]
fn subgroup_limit_preserves_every_unknown_arm_until_exact_cancellation() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "cohort::launch::subgroup_tests::subgroup_limit_preserves_every_unknown_arm_until_exact_cancellation",
    ) {
        return;
    }
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    let mut root = Running::new(claim.launch_cohort(&mut cat()).unwrap());
    let mut attempts = Vec::new();
    // Leave room for the full exact cancellation row as well as the pending ARM row.
    // Admission still uses the real byte bound; this test must be able to cancel every
    // no-child proof instead of deliberately wedging a completely full journal.
    while attempts.len() < crate::ledger::RECORD_MAX as usize / 32 {
        let text = std::fs::read_to_string(claim.path()).unwrap();
        let (_, journal) = Journal::decode(&text, true, fixture.namespace.nonce()).unwrap();
        if journal.compact(&text).unwrap().len() + 2048 >= crate::ledger::RECORD_MAX as usize {
            break;
        }
        attempts.push(claim.arm_subgroup(root.launched.cohort()).unwrap());
    }
    assert!(
        attempts.len() > 28,
        "no arbitrary sixteen-group execution ceiling remains"
    );
    let text = std::fs::read_to_string(claim.path()).unwrap();
    assert!(text.len() as u64 <= crate::ledger::RECORD_MAX);
    let (_, journal) = Journal::decode(&text, true, fixture.namespace.nonce()).unwrap();
    for attempt in &attempts {
        assert!(journal.has_subgroup(attempt.key()));
    }
    root.end();
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    for attempt in attempts {
        claim.cancel_subgroup(attempt.cancel().unwrap()).unwrap();
    }
    claim.finish_cohort().unwrap();
}
