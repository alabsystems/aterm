// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn cancelled_intent_reopens_signed_and_cannot_be_admitted_again() {
    // Actual encrypted store/MAC and exact replay, synthetic consumer and
    // target metadata. This does not qualify a supported SDK/owner pair.
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    let paths = aterm_secrets::client::Paths::locate_for_home(&home);
    aterm_secrets::client::machine_identity_as(
        &paths,
        aterm_secrets::Making {
            test_binary: true,
            ..aterm_secrets::Making::default()
        },
    )
    .unwrap();
    let identity = aterm_secrets::client::read_machine_identity(&paths)
        .unwrap()
        .unwrap();
    let directory = root.path().join("book");
    let (store, _) =
        crate::harness::pending_native_store::Store::open_owned(&directory, &home, MAX_BOOK)
            .unwrap();
    let mut record = super::super::tests::record();
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
        record: record.clone(),
        caller: book.caller.clone(),
    };
    let mut claimed = Claimed {
        store,
        book,
        pending_install: None,
    };
    publish(&mut claimed, &identity).unwrap();
    let namespace = claimed.store.physical_identity().unwrap();
    let original = aterm_json::to_vec(&claimed.book.current).unwrap();
    let cancelled = match persist_cancelled(&mut claimed, &identity, &intent, &namespace).unwrap() {
        Progress::Cancelled(receipt) => receipt,
        Progress::EffectsStarted => panic!("no target effect was started"),
    };
    assert!(cancelled.matches(&intent.caller, intent.operation(), &namespace));
    assert_eq!(aterm_json::to_vec(&claimed.book.current).unwrap(), original);
    // Source metadata injection tests matching only; it does not mint a
    // native Slot, supported image, owner or rollback kernel authority.
    use crate::harness::codex_session_slot::metadata_fixture as slot_fixture;
    let source = |binding, authority, book_namespace| {
        slot_fixture::with_namespaces(
            slot_fixture::with_authority(
                slot_fixture::with_assignment(
                    slot_fixture::source(binding, home.join("auth.json"), 1),
                    7,
                    &"6".repeat(32),
                ),
                authority,
            ),
            book_namespace,
            namespace.clone(),
        )
    };
    let actual = source(
        record.source.clone(),
        record.source_authority.clone(),
        namespace.clone(),
    );
    let wrong = source(
        record.target.as_ref().unwrap().binding.clone(),
        record.source_authority.clone(),
        namespace.clone(),
    );
    let wrong_epoch = source(
        record.source.clone(),
        SourceAuthority::Assignment {
            operation: "8".repeat(32),
        },
        namespace.clone(),
    );
    let (foreign, _) = crate::harness::pending_native_store::Store::open_owned(
        &root.path().join("foreign-book"),
        &home,
        MAX_BOOK,
    )
    .unwrap();
    let wrong_namespace = source(
        record.source.clone(),
        record.source_authority.clone(),
        foreign.physical_identity().unwrap(),
    );
    let restored = std::cell::Cell::new(false);
    assert!(
        with_cancelled_claim(&claimed, &intent, &namespace, |permit| {
            assert!(permit.caller() == intent.caller());
            assert!(permit.matches_source(&actual));
            assert!(!permit.matches_source(&wrong));
            assert!(!permit.matches_source(&wrong_epoch));
            assert!(!permit.matches_source(&wrong_namespace));
            restored.set(true);
            Ok(())
        })
        .is_ok()
    );
    assert!(restored.get());
    let saved = claimed.book.prepared[0].commit_boundary.clone();
    claimed.book.prepared[0].commit_boundary = Some("a".repeat(32));
    restored.set(false);
    assert!(
        with_cancelled_claim(&claimed, &intent, &namespace, |_| {
            restored.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(
        !restored.get(),
        "effects must never produce a rollback permit"
    );
    claimed.book.prepared[0].commit_boundary = saved;
    // Retrying an unobserved write returns the same semantic receipt and
    // does not allocate another operation or mutate the current source.
    assert!(matches!(
        persist_cancelled(&mut claimed, &identity, &intent, &namespace).unwrap(),
        Progress::Cancelled(_)
    ));
    assert_eq!(claimed.book.prepared.len(), 1);
    assert!(admitted(&claimed.book.prepared, &record.body).is_err());
    assert!(
        origin_namespace_matches(&claimed.book, &intent).unwrap(),
        "signed cancellation of B does not change already-admitted A's namespace"
    );
    drop(claimed);
    let (_store, bytes) = crate::harness::pending_native_store::Store::open_owned_existing(
        &directory, &home, MAX_BOOK,
    )
    .unwrap()
    .unwrap();
    let envelope: Envelope = aterm_json::from_slice(bytes.unwrap().as_slice()).unwrap();
    assert_eq!(signature(&identity, &envelope.book).unwrap(), envelope.mac);
    assert!(envelope.book.valid(&envelope.book.caller));
    assert!(admitted(&envelope.book.prepared, &record.body).is_err());
    assert!(origin_namespace_matches(&envelope.book, &intent).unwrap());
    assert_eq!(
        aterm_json::to_vec(&envelope.book.current).unwrap(),
        original
    );
    let mut tampered = envelope.book;
    tampered.prepared[0].cancelled = false;
    assert_ne!(signature(&identity, &tampered).unwrap(), envelope.mac);
}
