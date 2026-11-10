// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests as metadata;
use super::*;

fn stored() -> (aterm_tempfile::TempDir, Env, Record, SourceHandle) {
    // Real encrypted/MACed slot storage, synthetic metadata only. No owner,
    // source process or native SDK qualification is manufactured here.
    let (root, env) = metadata::fixture(true);
    let record = metadata::record(metadata::caller());
    let directory = materialization_location(&env, &record.caller, &record.binding).unwrap();
    aterm_secrets::fsx::ensure_private_dir(directory.parent().unwrap()).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    private_file::write_private(&directory, "auth.json", b"synthetic metadata test").unwrap();
    let mut opened = Opened::open(&env, &record.caller, true).unwrap().unwrap();
    opened.save(record.clone()).unwrap();
    drop(opened);
    let expected = SourceHandle {
        policy: None,
        namespaces: None,
        binding: std::sync::Arc::new(record.binding.clone()),
        slot: slot_key(&record.caller, &record.binding).unwrap(),
        path: directory.join("auth.json").canonicalize().unwrap(),
        account: "1".repeat(64),
        user: "2".repeat(64),
        revision: record.revision.clone(),
        assignment_generation: record.assignment_generation,
        source_authority: record.source_authority.clone(),
    };
    (root, env, record, expected)
}

fn replace(env: &Env, record: &Record) {
    let mut opened = Opened::open(env, &record.caller, false).unwrap().unwrap();
    opened.save(record.clone()).unwrap();
}

#[test]
fn stale_a_scope_cannot_enter_b_owner_or_create_b_materialization() {
    let (_root, env, mut record, expected) = stored();
    record.binding.request.label = "other-grant".into();
    replace(&env, &record);
    let other = materialization_location(&env, &record.caller, &record.binding).unwrap();
    assert!(!other.exists());
    OWNER_ENTRIES.with(|count| count.set(0));
    assert!(source_inner(&env, &record.caller, &"a".repeat(32), Some(&expected)).is_err());
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
    let request = RefreshRequest::new(
        expected.slot().into(),
        "b".repeat(32),
        expected.digest().into(),
    );
    assert!(
        refresh_inner(
            &env,
            &record.caller,
            &request,
            &crate::harness::account_work::Lifetime::default()
                .enter()
                .unwrap(),
            Some(&expected)
        )
        .is_err()
    );
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
    assert!(!other.exists());
}

#[test]
fn same_grant_new_assignment_is_not_old_use_authority() {
    let (_root, env, mut record, expected) = stored();
    record.assignment_generation += 1;
    record.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: "4".repeat(32),
    };
    replace(&env, &record);
    OWNER_ENTRIES.with(|count| count.set(0));
    assert!(source_inner(&env, &record.caller, &"a".repeat(32), Some(&expected)).is_err());
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
}

#[test]
fn distinct_startup_nonce_cannot_collapse_into_native_intent_none() {
    let (_root, env, mut record, mut expected) = stored();
    record.assignment_generation = 0;
    record.source_authority = codex_assignment::SourceAuthority::Startup {
        nonce: "6".repeat(32),
    };
    expected.assignment_generation = 0;
    expected.source_authority = record.source_authority.clone();
    replace(&env, &record);
    assert!(expected.native_intent().is_none());
    assert!(check_record(&env, &record.caller, &record, &expected).is_ok());
    record.source_authority = codex_assignment::SourceAuthority::Startup {
        nonce: "7".repeat(32),
    };
    assert!(record.source_authority.native_intent().is_none());
    replace(&env, &record);
    OWNER_ENTRIES.with(|count| count.set(0));
    assert!(source_inner(&env, &record.caller, &"a".repeat(32), Some(&expected)).is_err());
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
    let request = RefreshRequest::new(
        expected.slot().into(),
        "b".repeat(32),
        expected.digest().into(),
    );
    assert!(
        refresh_inner(
            &env,
            &record.caller,
            &request,
            &crate::harness::account_work::Lifetime::default()
                .enter()
                .unwrap(),
            Some(&expected)
        )
        .is_err()
    );
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
}

#[test]
fn legitimate_same_scope_revision_advance_preserves_identity_and_digest_fences() {
    let (_root, env, mut record, expected) = stored();
    assert!(check_record(&env, &record.caller, &record, &expected).is_ok());
    record.revision.digest = "4".repeat(64);
    assert!(check_record(&env, &record.caller, &record, &expected).is_err());
    record.revision.generation += 1;
    assert!(check_record(&env, &record.caller, &record, &expected).is_ok());
    let mut actual = SourceHandle {
        policy: None,
        namespaces: None,
        binding: expected.binding.clone(),
        slot: expected.slot.clone(),
        path: expected.path.clone(),
        account: expected.account.clone(),
        user: expected.user.clone(),
        revision: record.revision,
        assignment_generation: expected.assignment_generation,
        source_authority: expected.source_authority.clone(),
    };
    assert!(check_source(&SourceProgress::Ready(actual), &expected).is_ok());
    actual = SourceHandle {
        policy: None,
        namespaces: None,
        binding: expected.binding.clone(),
        slot: expected.slot.clone(),
        path: expected.path.clone(),
        account: expected.account.clone(),
        user: "5".repeat(64),
        revision: expected.revision.clone(),
        assignment_generation: expected.assignment_generation,
        source_authority: expected.source_authority.clone(),
    };
    assert!(check_source(&SourceProgress::Ready(actual), &expected).is_err());
    let alternate_path = env.home.as_ref().unwrap().join("other-auth.json");
    private_file::write_private(env.home.as_ref().unwrap(), "other-auth.json", b"synthetic")
        .unwrap();
    let mut wrong_path = expected;
    wrong_path.path = alternate_path.canonicalize().unwrap();
    assert!(
        check_record(
            &env,
            &record.caller,
            &metadata::record(record.caller.clone()),
            &wrong_path
        )
        .is_err()
    );
}

#[test]
fn codex_source_registered_preload_rejects_changed_default_before_owner() {
    let (_root, env, mut record, expected) = stored();
    record.binding.request.label = "different-default".into();
    replace(&env, &record);
    let other = materialization_location(&env, &record.caller, &record.binding).unwrap();
    assert!(!other.exists());
    OWNER_ENTRIES.with(|count| count.set(0));
    assert!(source_matching_registered(&env, &record.caller, &"a".repeat(32), &expected).is_err());
    assert_eq!(OWNER_ENTRIES.with(std::cell::Cell::get), 0);
    assert!(!other.exists());
}
