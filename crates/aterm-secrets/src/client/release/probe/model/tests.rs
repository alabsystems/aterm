// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn plan() -> Journal {
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: 501,
            node: "34".repeat(16),
            member: "56".repeat(16),
            writer: "bc".repeat(32),
            incarnation: "78".repeat(16),
            network: "9a".repeat(16),
            authority: "cd".repeat(32),
            genesis: "de".repeat(32),
            vault: "ef".repeat(32),
        },
        repositories: vec![Repository::new("fixture-owner".into())],
        created_ms: 1,
        updated_ms: 1,
        artifact: None,
        submission: None,
        apple_accepted: false,
        pending: None,
        cleanup_only: false,
    }
}

fn signed(plan: &mut Journal) {
    plan.artifact = Some(Artifact {
        sha256: "ab".repeat(32),
        executable_sha256: "cd".repeat(32),
        bytes: 100,
        signing_identity: "CD".repeat(20),
        team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
        executed: true,
    });
}

#[test]
fn unknown_external_writes_are_observed_and_never_issued_again() {
    for action in [
        Action::Submit,
        Action::Tag(0),
        Action::Draft(0),
        Action::Upload(0),
    ] {
        let mut plan = plan();
        signed(&mut plan);
        if action != Action::Submit {
            plan.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
            plan.apple_accepted = true;
            plan.repositories[0].commit = Some("de".repeat(20));
            plan.repositories[0].principal = Some("fixture-writer".into());
        }
        if matches!(action, Action::Draft(_) | Action::Upload(_)) {
            plan.repositories[0].tag_owned = true;
        }
        if matches!(action, Action::Upload(_)) {
            plan.repositories[0].release = Some(1);
        }
        plan.pending = Some(action);
        plan.validate().unwrap();
        assert_eq!(plan.next(), Next::Observe(action));
        plan.cleanup_only = true;
        assert_eq!(plan.next(), Next::Observe(action));
    }
}

#[test]
fn pending_actions_require_their_predecessor_evidence() {
    for action in [
        Action::Submit,
        Action::Tag(0),
        Action::Draft(0),
        Action::Upload(0),
        Action::Readback(0),
    ] {
        let mut plan = plan();
        plan.pending = Some(action);
        assert!(plan.validate().is_err(), "{action:?}");
    }
    let mut plan = plan();
    signed(&mut plan);
    for invalid in [
        "",
        "--profile",
        "some/path",
        "01234567-89ab-cdef-0123-456789abcdeg",
    ] {
        plan.submission = Some(invalid.into());
        assert!(plan.validate().is_err());
    }
}

#[test]
fn a_partial_receipt_cannot_finish_the_probe() {
    let mut plan = plan();
    assert_eq!(plan.next(), Next::Perform(Action::Sign));
    signed(&mut plan);
    assert_eq!(plan.next(), Next::Perform(Action::Submit));
    plan.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    assert_eq!(plan.next(), Next::PollNotary);
    plan.apple_accepted = true;
    assert_eq!(plan.next(), Next::Perform(Action::Fetch(0)));
    let repo = &mut plan.repositories[0];
    repo.commit = Some("de".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    repo.release = Some(1);
    repo.asset = Some(2);
    assert_eq!(plan.next(), Next::Perform(Action::Readback(0)));
    plan.repositories[0].verified = true;
    assert_eq!(plan.next(), Next::Perform(Action::DeleteAsset(0)));
    plan.repositories[0].asset_absent = true;
    assert_eq!(plan.next(), Next::Perform(Action::DeleteDraft(0)));
    plan.repositories[0].release_absent = true;
    assert_eq!(plan.next(), Next::Perform(Action::DeleteTag(0)));
    plan.repositories[0].tag_absent = true;
    assert_eq!(plan.next(), Next::Complete);
    assert!(plan.validate().is_ok());
}

#[test]
fn every_requested_repository_needs_its_own_upload_and_readback() {
    let mut plan = plan();
    signed(&mut plan);
    plan.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    plan.apple_accepted = true;
    let repo = &mut plan.repositories[0];
    repo.commit = Some("de".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    repo.release = Some(1);
    repo.asset = Some(2);
    repo.verified = true;
    plan.repositories
        .push(Repository::new("other-owner".into()));
    assert_eq!(plan.next(), Next::Perform(Action::Fetch(1)));
    assert!(!plan.repositories[0].clean());
}

#[test]
fn cleanup_never_begins_a_missing_signature_or_remote_publication() {
    let mut plan = plan();
    plan.cleanup_only = true;
    assert_eq!(plan.next(), Next::Clean);
    signed(&mut plan);
    plan.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    plan.apple_accepted = true;
    let repo = &mut plan.repositories[0];
    repo.commit = Some("de".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    assert_eq!(plan.next(), Next::Perform(Action::DeleteTag(0)));
}

#[test]
fn absence_receipts_require_a_previously_owned_object() {
    for field in 0..3 {
        let mut plan = plan();
        match field {
            0 => plan.repositories[0].asset_absent = true,
            1 => plan.repositories[0].release_absent = true,
            _ => plan.repositories[0].tag_absent = true,
        }
        assert!(plan.validate().is_err());
    }
}

#[test]
fn malformed_or_incomplete_evidence_is_rejected() {
    let valid = plan();
    assert!(valid.validate().is_ok());
    let mut changed = valid.clone();
    changed.nonce = "not-a-nonce".into();
    assert!(changed.validate().is_err());
    changed = valid.clone();
    changed.apple_accepted = true;
    assert!(changed.validate().is_err());
    changed = valid.clone();
    signed(&mut changed);
    changed.artifact.as_mut().unwrap().executed = false;
    assert!(changed.validate().is_err());
    changed = valid.clone();
    signed(&mut changed);
    changed.artifact.as_mut().unwrap().team = "FOREIGNTEAM".into();
    assert!(changed.validate().is_err());
    changed = valid.clone();
    changed.repositories[0].asset = Some(1);
    assert!(changed.validate().is_err());
    changed = valid.clone();
    changed.repositories.push(changed.repositories[0].clone());
    assert!(changed.validate().is_err());
    changed = valid;
    changed.pending = Some(Action::DeleteDraft(1));
    assert!(changed.validate().is_err());
}
