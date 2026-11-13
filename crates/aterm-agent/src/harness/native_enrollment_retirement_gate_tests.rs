// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::codex_source_gate::PublicationLock;

// Real kernel components only: no qualified FamilyWork, independent grant,
// actual WebSocket, provider, SourceSeal or SourceV1 evidence is claimed.
#[test]
fn shared_source_work_and_metadata_retirement_coexist_without_lending_publication() {
    let world = aterm_tempfile::tempdir().unwrap();
    let body = SourceWorkLease::for_home(world.path()).unwrap();
    let names = ["principal-fixture".into(), "rt-fixture".into()];
    let cleanup = RetirementTurn::acquire_within(
        world.path(),
        &names,
        WakeDeadline::after(std::time::Duration::from_secs(60)),
        || Ok(()),
    )
    .unwrap();
    body.current().unwrap();
    cleanup.current().unwrap();
    assert!(
        PublicationLock::try_for_home(world.path())
            .unwrap()
            .is_none()
    );
    drop(body);
    assert!(
        PublicationLock::try_for_home(world.path())
            .unwrap()
            .is_none()
    );
    drop(cleanup);
    assert!(
        PublicationLock::try_for_home(world.path())
            .unwrap()
            .is_some()
    );
}

#[test]
fn an_actual_publisher_still_excludes_metadata_retirement() {
    let world = aterm_tempfile::tempdir().unwrap();
    let publisher = PublicationLock::for_home(world.path()).unwrap();
    assert!(
        RetirementTurn::try_acquire(world.path(), &[])
            .unwrap()
            .is_none()
    );
    drop(publisher);
    assert!(
        RetirementTurn::try_acquire(world.path(), &[])
            .unwrap()
            .is_some()
    );
}

#[test]
fn principal_and_refresh_exclusions_remain_exact() {
    let world = aterm_tempfile::tempdir().unwrap();
    let a = ["principal-a".into(), "rt-a".into()];
    let b = ["principal-b".into(), "rt-b".into()];
    let first = RetirementTurn::try_acquire(world.path(), &a)
        .unwrap()
        .unwrap();
    assert!(
        RetirementTurn::try_acquire(world.path(), &a)
            .unwrap()
            .is_none()
    );
    let other = RetirementTurn::try_acquire(world.path(), &b)
        .unwrap()
        .unwrap();
    let alias = ["principal-a".into(), "rt-other".into()];
    assert!(
        RetirementTurn::try_acquire(world.path(), &alias)
            .unwrap()
            .is_none()
    );
    first.current().unwrap();
    other.current().unwrap();
}

#[test]
fn actual_cancelled_claim_retires_with_shared_source_custody_and_only_metadata_changes() {
    use crate::harness::{account_work, accounts, codex_enrollment};
    let fixture = codex_enrollment::NativeRowRecoveryFixture::new(&"a".repeat(64));
    fixture.retire_originals().unwrap();
    fixture.cancel_first_after_publication_claim().unwrap();
    let env = fixture.env();
    let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
    let request = fixture.requests()[0].clone();
    let before = store.load_logins(env.now).unwrap().0;
    let expected: aterm_json::Value = aterm_json::from_str(&before.to_json()).unwrap();
    let body = SourceWorkLease::try_for_home(env.home.as_deref().unwrap())
        .unwrap()
        .expect("shared source work is free once its holders drop");
    let lifetime = account_work::Lifetime::default();
    let (work, _) = codex_enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    assert_eq!(
        fixture.run_recovery(work).unwrap().phase,
        codex_enrollment::Phase::Cancelled
    );
    body.current().unwrap();
    let after = store.load_logins(env.now).unwrap().0;
    let mut actual: aterm_json::Value = aterm_json::from_str(&after.to_json()).unwrap();
    // Compare every persisted field, including owner witnesses and B's
    // record, after normalizing only A's permitted status/recovery changes.
    let record = actual
        .as_object_mut()
        .unwrap()
        .get_mut("enrollment")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut(request.id.as_str())
        .unwrap()
        .as_object_mut()
        .unwrap();
    for field in ["status", "recovery"] {
        assert!(record.contains_key(field));
        record.insert(
            field.into(),
            expected["enrollment"][request.id.as_str()][field].clone(),
        );
    }
    assert_eq!(actual, expected);
}

#[test]
fn invalid_source_namespace_is_an_error_not_contention() {
    let world = aterm_tempfile::tempdir().unwrap();
    std::fs::write(world.path().join(".aterm"), "not a directory").unwrap();
    assert!(SourceWorkLease::try_for_home(world.path()).is_err());
}
