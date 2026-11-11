// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn canonical_control_transitions_cannot_become_a_general_force_operation() {
    let owner = "1".repeat(40);
    let token = "2".repeat(40);
    let next = "3".repeat(40);
    let row = |reference: &str, before: Option<&str>, after: Option<&str>| ControlRefChange {
        reference: reference.into(),
        before: before.map(str::to_owned),
        after: after.map(str::to_owned),
    };
    for changes in [
        vec![row(RELEASE_LEASE_REF, None, Some(&owner))],
        vec![row(RELEASE_LEASE_REF, Some(&owner), None)],
        vec![row(PUBLISHER_FENCE_REF, None, Some(&token))],
        vec![row(PUBLISHER_FENCE_REF, Some(&token), Some(&next))],
        vec![row(PUBLISHER_FENCE_REF, Some(&token), None)],
        vec![
            row(RELEASE_LEASE_REF, Some(&owner), None),
            row(PUBLISHER_FENCE_REF, Some(&token), None),
        ],
    ] {
        assert!(ControlRefTransaction::new(&owner, changes).is_ok());
    }
    for changes in [
        Vec::new(),
        vec![row("refs/heads/main", Some(&owner), Some(&next))],
        vec![row("refs/tags/v1", Some(&token), None)],
        vec![row(RELEASE_LEASE_REF, Some(&owner), Some(&next))],
        vec![row(RELEASE_LEASE_REF, Some(&owner), Some(&owner))],
        vec![row(RELEASE_LEASE_REF, None, None)],
        vec![row(PUBLISHER_FENCE_REF, Some(&owner), Some(&next))],
        vec![
            row(PUBLISHER_FENCE_REF, None, Some(&token)),
            row(PUBLISHER_FENCE_REF, Some(&token), None),
        ],
        vec![
            row(RELEASE_LEASE_REF, None, Some(&owner)),
            row(PUBLISHER_FENCE_REF, None, Some(&token)),
        ],
    ] {
        assert!(ControlRefTransaction::new(&owner, changes).is_err());
    }
    assert!(
        ControlRefTransaction::new(
            &"0".repeat(40),
            vec![row(RELEASE_LEASE_REF, None, Some(&owner))]
        )
        .is_err()
    );
}

#[test]
fn observed_state_and_annotation_bind_the_exact_control_owner() {
    let owner = "1".repeat(40);
    let token = "2".repeat(40);
    let other = "3".repeat(40);
    let make = |after| {
        ControlRefTransaction::new(
            &owner,
            vec![ControlRefChange {
                reference: PUBLISHER_FENCE_REF.into(),
                before: Some(token.clone()),
                after,
            }],
        )
        .unwrap()
    };
    let replace = make(Some(other.clone()));
    assert!(replace.check_before(Some(&owner), Some(&token)).is_ok());
    assert!(replace.check_before(None, Some(&token)).is_err());
    assert!(replace.check_before(Some(&other), Some(&token)).is_err());
    let delete = make(None);
    assert!(delete.check_before(None, Some(&token)).is_ok());
    assert!(delete.check_before(Some(&other), Some(&token)).is_ok());
    assert!(
        replace
            .check_fence_header(
                format!("object {owner}\ntype commit\ntag token\n\nidentity\n").as_bytes()
            )
            .is_ok()
    );
    for body in [
        format!("object {other}\ntype commit\n\n"),
        format!("object {owner}\ntype tag\n\n"),
        format!("object {owner}\ntype commit\nobject {owner}\n\n"),
    ] {
        assert!(replace.check_fence_header(body.as_bytes()).is_err());
    }
}
