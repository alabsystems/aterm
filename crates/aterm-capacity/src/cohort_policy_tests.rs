// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::CohortFixture;
use aterm_uds::spawnfd;

#[test]
fn boot_only_durable_death_and_closure() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    claim.require_boot_closure().unwrap();
    let reference = claim.cohort_closure_ref().unwrap();
    let path = claim.path().to_owned();
    let mut command = spawnfd::command("/bin/cat");
    command
        .stdin(spawnfd::Stdio::piped())
        .stdout(spawnfd::Stdio::piped());
    let mut child = claim.launch_cohort(&mut command).unwrap();
    let input = aterm_uds::deadline::cat_echoes(child.child_mut());
    child
        .request_group(crate::cohort::launch::CohortSignal::Kill)
        .unwrap();
    assert!(!child.child_mut().wait().unwrap().success());
    drop(input);
    assert!(claim.require_boot_closure().is_err());
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let head: Value = aterm_json::from_str(text.lines().next().unwrap()).unwrap();
    let namespace = head.get("namespace").unwrap().as_str().unwrap();
    let (_, journal) = Journal::decode(&text, true, namespace).unwrap();
    assert_eq!(journal.policy, ClosurePolicy::BootOnly);
    let State::Bound(key, _) = &journal.state else {
        panic!("not bound")
    };
    let mut forged = journal.clone();
    assert!(
        forged
            .apply(&aterm_json::json!({"phase": "finish", "key": key.encode()}))
            .is_err()
    );
    assert_eq!(journal.lifetime_with(None), Lifetime::Unknown);
    assert_eq!(
        journal.lifetime_with(Some(Lifetime::Unknown)),
        Lifetime::Unknown
    );
    assert_eq!(
        journal.lifetime_with(Some(Lifetime::Drained)),
        Lifetime::Drained
    );
    drop(claim);
    assert!(path.exists());
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        crate::CohortClaimState::Retained
    );
    for policy in [
        None,
        Some(aterm_json::json!({"v": 1})),
        Some(aterm_json::json!({"v": 1, "kind": "unknown"})),
    ] {
        let mut head = head.clone();
        let map = head.as_object_mut().unwrap();
        map.remove("closure_policy");
        if let Some(policy) = policy {
            map.insert("closure_policy".into(), policy);
        }
        let (_, tail) = text.split_once('\n').unwrap();
        let invalid = format!("{}\n{tail}", aterm_json::to_string(&head).unwrap());
        assert!(Journal::decode(&invalid, true, namespace).is_err());
    }
}

#[test]
fn boot_only_policy_commit_failure_recovers_before_spawn() {
    let fixture = CohortFixture::new();
    let mut claim = fixture.claim();
    fixture.fail_next_event_after_write();
    assert!(claim.require_boot_closure().is_err());
    // ARM must cover the pending policy first, rather than reverting to Group.
    let mut command = spawnfd::command("/usr/bin/true");
    let mut child = claim.launch_cohort(&mut command).unwrap();
    child.child_mut().wait().unwrap();
    assert_eq!(
        claim.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let reference = claim.cohort_closure_ref().unwrap();
    drop(claim);
    assert_eq!(
        fixture.ledger.reap_cohort_claim(&reference).unwrap(),
        crate::CohortClaimState::Retained
    );
}

#[test]
fn boot_only_no_child_cancel_and_default_group_close() {
    for boot_only in [false, true] {
        let fixture = CohortFixture::new();
        let mut claim = fixture.claim();
        if boot_only {
            claim.require_boot_closure().unwrap();
            let attempt = claim.arm_cohort().unwrap();
            claim.cancel_cohort(attempt.cancel().unwrap()).unwrap();
            assert!(claim.require_boot_closure().is_err());
        } else {
            let mut command = spawnfd::command("/usr/bin/true");
            let mut child = claim.launch_cohort(&mut command).unwrap();
            child.child_mut().wait().unwrap();
        }
        claim.finish_cohort().unwrap();
        let reference = claim.cohort_closure_ref().unwrap();
        drop(claim);
        assert_eq!(
            fixture.ledger.reap_cohort_claim(&reference).unwrap(),
            crate::CohortClaimState::Closed
        );
    }
}
