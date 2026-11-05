// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_held_control_journal_remains_contention_after_database_admission() {
    let lab = table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let held = table::Session::open(&bootstrap).unwrap();
    let refused = table::Session::open_until(&bootstrap, std::time::Instant::now())
        .err()
        .expect("the other session owns the control writer");
    assert!(
        refused.is_busy(),
        "a busy control writer must not become damaged or unavailable state: {refused}"
    );
    let endpoint = control::Endpoint::from_bootstrap_until(&bootstrap, std::time::Instant::now())
        .err()
        .expect("an endpoint's writer admission is held too");
    assert!(
        endpoint.is_busy(),
        "endpoint context lost contention: {endpoint}"
    );
    assert!(table::Session::observe(&bootstrap).is_ok());
    drop(held);
    assert!(table::Session::open(&bootstrap).is_ok());
}

#[test]
fn a_held_vault_writer_remains_contention_during_custody_revalidation() {
    let lab = table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let held = aterm_uds::private_file::PrivateLock::take(
        &lab.paths[0].dir,
        ".lock",
        std::time::Duration::ZERO,
        "held",
    )
    .unwrap();
    let refused = table::Session::observe(&bootstrap)
        .err()
        .expect("custody cannot authorize a read while its vault is being written");
    assert!(
        refused.is_busy(),
        "a vault writer must remain transient through custody revalidation: {refused}"
    );
    drop(held);
    assert!(table::Session::observe(&bootstrap).is_ok());
}

#[test]
fn a_new_vault_publication_requires_a_fresh_database_observation() {
    use aterm_secrets::client;
    let lab = table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    client::admin_publish(&lab.paths[0], &admin, &admin.doc).unwrap();
    let stale = table::Session::observe(&bootstrap)
        .err()
        .expect("a previous vault snapshot cannot grant current table access");
    assert!(stale.is_busy(), "a fresh observation is required: {stale}");
    let current = lab.bootstrap(0);
    assert!(table::Session::observe(&current).is_ok());
}

#[test]
fn a_held_custody_observation_remains_contention_through_local_context() {
    use aterm_secrets::{ErrorKind, commons::bootstrap::Bootstrap};
    let lab = table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let held = aterm_uds::private_file::PrivateLock::take(
        &lab.paths[0].dir,
        "commons.lock",
        std::time::Duration::ZERO,
        "held",
    )
    .unwrap();
    let refused = Bootstrap::required_with_journal_until(&lab.paths[0], std::time::Instant::now())
        .err()
        .expect("the other observation holds custody");
    assert_eq!(refused.kind(), ErrorKind::CustodyBusy);
    let original = refused.to_string();
    let error = table::failure(refused).context("table repair bootstrap custody admission");
    assert!(error.is_busy());
    assert!(error.to_string().contains(&original));
    drop(held);
    assert!(
        Bootstrap::required_until(
            &lab.paths[0],
            std::time::Instant::now() + std::time::Duration::from_secs(60)
        )
        .is_ok()
    );
}

#[test]
fn failure_words_do_not_create_a_local_contention_classification() {
    for words in [BUSY, SNAPSHOT_CHANGED] {
        assert!(!Error::new(words).is_busy());
        assert!(!Error::new(words).context("local stage").is_busy());
        let io = std::io::Error::new(std::io::ErrorKind::WouldBlock, words);
        assert!(
            !Error::from(io).is_busy(),
            "a network timeout is not a local writer"
        );
        let opaque = aterm_secrets::Error::new(words);
        assert!(!table::failure(opaque).is_busy());
    }
}

#[test]
fn pending_enrollment_is_typed_and_retains_its_classification_through_context() {
    let pending = Error::enrollment_pending().context("table observation");
    assert!(pending.is_enrollment_pending());
    assert!(
        !pending.is_busy(),
        "enrollment is distinct from writer contention"
    );
    assert!(!Error::new(ENROLLMENT_PENDING).is_enrollment_pending());
    assert!(!Error::busy().is_enrollment_pending());
}

#[test]
fn local_context_retains_known_contention_and_snapshot_changes() {
    use aterm_secrets::ErrorKind;
    let mut errors = vec![Error::busy(), Error::snapshot_changed()];
    for kind in [
        ErrorKind::JournalBusy,
        ErrorKind::CustodyBusy,
        ErrorKind::VaultBusy,
        ErrorKind::VaultChanged,
    ] {
        errors.push(table::failure(aterm_secrets::Error::of_kind(
            kind,
            "fixture detail",
        )));
    }
    for error in errors {
        assert!(error.is_busy());
        let original = error.to_string();
        let nested = error.context("table admission").context("application pass");
        assert!(nested.is_busy());
        assert!(nested.to_string().contains(&original));
    }
}
