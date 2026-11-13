// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_secrets::client;
fn fixture() -> (
    aterm_tempfile::TempDir,
    age::Identity,
    Claimed,
    TargetIntent,
) {
    // Real private encrypted/MAC Book; synthetic consumer/grants only. No
    // supported-image, owner, provider lineage or installed-role proof.
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    let identity = client::machine_identity_as(
        &client::Paths::locate_for_home(&home),
        aterm_secrets::Making {
            test_binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut record = super::super::tests::record();
    record.source.generation = 31;
    record.target.as_mut().unwrap().binding.generation = 41;
    record.target.as_mut().unwrap().credential_floor = 13;
    record.body.expected_assignment = 7;
    record.source_authority = SourceAuthority::Assignment {
        operation: "6".repeat(32),
    };
    let mut book = super::super::tests::book(&record);
    book.clock = 7;
    book.current.generation = 7;
    book.current.previous_generation = 6;
    book.current.phase = Phase::Applied;
    book.current.boundary = Some("7".repeat(32));
    book.schema = 3;
    book.prepared.push(record.clone());
    let intent = TargetIntent {
        caller: book.caller.clone(),
        record,
    };
    let (mut store, _) = crate::harness::pending_native_store::Store::open_owned(
        &root.path().join("book"),
        &home,
        MAX_BOOK,
    )
    .unwrap();
    store
        .save(encoded(&book, &identity).unwrap().as_slice())
        .unwrap();
    (
        root,
        identity,
        Claimed {
            store,
            book,
            pending_install: None,
        },
        intent,
    )
}
fn destination(book: &Book, intent: &TargetIntent, boundary: &str) -> Book {
    let mut next = book.clone();
    next.schema = 4;
    next.prepared[0].commit_boundary = Some(boundary.into());
    next.clock += 1;
    next.current = Current {
        binding: intent.target_binding().unwrap().clone(),
        generation: next.clock,
        previous_generation: book.clock,
        authority: SourceAuthority::Assignment {
            operation: intent.operation().into(),
        },
        boundary: Some(boundary.into()),
        phase: Phase::Installing,
    };
    next
}
#[test]
fn target_install_failure_never_turns_unwritten_memory_into_current_authority() {
    let (root, identity, mut claimed, intent) = fixture();
    let next = destination(&claimed.book, &intent, &"4".repeat(32));
    let path = root.path().join("book/candidate.age");
    let held = path.with_extension("held");
    std::fs::rename(&path, &held).unwrap();
    assert!(write(&mut claimed, &identity, next.clone()).is_err());
    assert_eq!(claimed.book.clock, 7);
    assert!(claimed.pending_install.is_some());
    std::fs::rename(&held, &path).unwrap();
    write(&mut claimed, &identity, next).unwrap();
    assert_eq!(claimed.book.clock, 8);
    assert!(claimed.pending_install.is_none());
    assert!(matches!(
        position_metadata(&claimed.book, &intent, &"4".repeat(32)),
        Ok(Position::Effect)
    ));
    assert!(
        position(&claimed.book, &intent, &"4".repeat(32)).is_err(),
        "synthetic data cannot pass production consumer admission"
    );
}
#[test]
fn target_install_lost_durability_ack_adopts_only_exact_payload_and_never_new_generation() {
    let (_root, identity, mut claimed, intent) = fixture();
    let next = destination(&claimed.book, &intent, &"4".repeat(32));
    let bytes = encoded(&next, &identity).unwrap();
    claimed.pending_install = Some(Pending { book: next, bytes });
    claimed
        .store
        .save(claimed.pending_install.as_ref().unwrap().bytes.as_slice())
        .unwrap();
    assert_eq!(claimed.book.clock, 7);
    recover(&mut claimed, &identity).unwrap();
    assert_eq!(claimed.book.clock, 8);
    let cipher = claimed.store.sealed_digest();
    let replay = claimed.book.clone();
    write(&mut claimed, &identity, replay).unwrap();
    assert_eq!(claimed.store.sealed_digest(), cipher);
    assert_eq!(claimed.book.clock, 8);
    assert!(position_metadata(&claimed.book, &intent, &"5".repeat(32)).is_err());
}
#[test]
fn target_commit_boundary_schema_and_current_join_are_closed() {
    let (_root, _identity, mut claimed, intent) = fixture();
    assert!(claimed.book.valid(&intent.caller));
    assert!(!pending_for_current(&claimed.book));
    claimed.book.prepared[0].commit_boundary = Some("4".repeat(32));
    assert!(
        !claimed.book.valid(&intent.caller),
        "schema3 cannot carry new commit authority"
    );
    claimed.book.schema = 4;
    assert!(claimed.book.valid(&intent.caller));
    assert!(pending_for_current(&claimed.book));
    assert!(position_metadata(&claimed.book, &intent, &"5".repeat(32)).is_err());
    let mut next = destination(&claimed.book, &intent, &"4".repeat(32));
    assert!(next.valid(&intent.caller));
    assert!(
        !pending_for_current(&next),
        "historical A commit must not block current B"
    );
    next.current.boundary = Some("5".repeat(32));
    assert!(!next.valid(&intent.caller));
}
#[test]
fn signed_ack_lost_write_ack_recovers_exact_observation_without_rewriting_history() {
    let (_root, identity, mut claimed, intent) = fixture();
    let mut next = destination(&claimed.book, &intent, &"4".repeat(32));
    next.schema = 5;
    next.current.phase = Phase::Applied;
    let target = intent.record.target.as_ref().unwrap();
    let ack = super::super::super::target_ack::Observation {
        operation: intent.record.body.operation.clone(),
        context: "a".repeat(64),
        resource_operation: "b".repeat(32),
        resource_receipt: "c".repeat(64),
        member: "d".repeat(32),
        registration: "e".repeat(32),
        boundary: "4".repeat(32),
        assignment_generation: 8,
        credential_generation: 13,
        digest: "f".repeat(64),
        account: target.identity.account.clone(),
        user: target.identity.user.clone(),
        auth_generation: 17,
        owner_generation: 19,
        source_generation: 23,
        login_generation: 29,
    };
    next.acknowledged.push(ack.clone());
    assert!(next.valid(&intent.caller));
    let bytes = encoded(&next, &identity).unwrap();
    claimed.pending_install = Some(Pending { book: next, bytes });
    claimed
        .store
        .save(claimed.pending_install.as_ref().unwrap().bytes.as_slice())
        .unwrap();
    assert!(claimed.book.acknowledged.is_empty());
    recover(&mut claimed, &identity).unwrap();
    assert!(claimed.book.acknowledged[0] == ack);
    let cipher = claimed.store.sealed_digest();
    let replay = claimed.book.clone();
    write(&mut claimed, &identity, replay).unwrap();
    assert_eq!(cipher, claimed.store.sealed_digest());
    let mut changed = claimed.book.clone();
    changed.acknowledged[0].boundary = "5".repeat(32);
    assert!(!changed.valid(&intent.caller));
    changed = claimed.book.clone();
    changed.acknowledged.push(ack);
    assert!(!changed.valid(&intent.caller));
    changed = claimed.book.clone();
    changed.schema = 4;
    assert!(
        !changed.valid(&intent.caller),
        "old writers cannot accept ACK authority"
    );
}
