// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, aterm_secrets::age::Identity, Store) {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
    let identity = aterm_secrets::client::machine_identity_as(
        &aterm_secrets::client::Paths::locate_for_home(&home),
        aterm_secrets::Making {
            test_binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    let (store, _) = Store::open_owned(&root.path().join("custody"), &home, 64 * 1024).unwrap();
    (root, identity, store)
}

#[test]
fn exact_owned_payload_recovers_an_uncertain_completed_rename() {
    let (_root, identity, mut store) = fixture();
    store.save(b"prior").unwrap();
    let ciphertext =
        aterm_secrets::age::encrypt(&[identity.recipient()], b"completed-response").unwrap();
    aterm_secrets::fsx::write_atomic(&store.path, &ciphertext, 0o600).unwrap();
    assert!(store.unchanged().is_err());
    assert!(
        store
            .recover_exact_write(&identity, b"completed-response")
            .unwrap()
    );
    assert!(store.unchanged().is_ok());
    store.save(b"acknowledged").unwrap();
    assert!(store.unchanged().is_ok());
}

#[test]
fn recovery_never_adopts_or_overwrites_a_different_payload() {
    let (_root, identity, mut store) = fixture();
    store.save(b"prior").unwrap();
    let before = store.expected;
    let ciphertext =
        aterm_secrets::age::encrypt(&[identity.recipient()], b"different-response").unwrap();
    aterm_secrets::fsx::write_atomic(&store.path, &ciphertext, 0o600).unwrap();
    assert!(
        !store
            .recover_exact_write(&identity, b"completed-response")
            .unwrap()
    );
    assert_eq!(store.expected, before);
    assert_eq!(std::fs::read(&store.path).unwrap(), ciphertext);
    assert!(store.unchanged().is_err());
}

#[test]
fn retired_claim_cannot_recover_or_create_another_record() {
    let (_root, identity, mut store) = fixture();
    store.save(b"completed-response").unwrap();
    store.retire().unwrap();
    assert!(
        store
            .recover_exact_write(&identity, b"completed-response")
            .is_err()
    );
    assert!(!store.path.exists());
}
#[cfg(unix)]
#[test]
fn physical_store_identity_joins_aliases_without_reacquiring_or_granting_a_lock() {
    let (root, _identity, mut store) = fixture();
    store.save(b"namespace-fixture").unwrap();
    let observed = store.physical_identity().unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(store.path.parent().unwrap(), &alias).unwrap();
    let through_alias = PhysicalIdentity::capture(&alias, &store._claim).unwrap();
    assert_eq!(observed, through_alias);
    assert!(observed.current());
    let directory = store.path.parent().unwrap().to_path_buf();
    drop(store);
    let _exclusive = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
    assert!(
        observed.current(),
        "identity observation acquires no competing lock and grants no exclusion"
    );
}

#[cfg(unix)]
#[test]
fn physical_store_identity_refuses_replaced_directory_or_claim_descriptor() {
    let (root, _identity, mut store) = fixture();
    store.save(b"namespace-fixture").unwrap();
    let observed = store.physical_identity().unwrap();
    let directory = store.path.parent().unwrap().to_path_buf();
    std::fs::rename(&directory, root.path().join("retired-custody")).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    let _replacement = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
    assert!(!observed.current());
    assert!(store.physical_identity().is_err());
    assert!(
        PhysicalIdentity::capture(&directory, &store._claim).is_err(),
        "different actual held fd cannot adopt a replacement path"
    );
}
#[test]
fn held_namespace_cannot_be_relabelled_by_a_byte_identical_directory_copy() {
    let (root, identity, mut store) = fixture();
    store.save(b"owned-response").unwrap();
    let expected = store.expected;
    let directory = store.path.parent().unwrap().to_path_buf();
    let displaced = root.path().join("original-held");
    std::fs::rename(&directory, &displaced).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&directory).unwrap();
    std::fs::copy(displaced.join("candidate.age"), &store.path).unwrap();
    let replacement = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
    let before = std::fs::read(&store.path).unwrap();
    assert!(store.unchanged().is_err());
    assert!(store.save(b"must-not-overwrite").is_err());
    assert!(
        store
            .recover_exact_write(&identity, b"owned-response")
            .is_err()
    );
    assert!(store.retire().is_err());
    assert_eq!(store.expected, expected);
    assert_eq!(std::fs::read(&store.path).unwrap(), before);
    drop(replacement);
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::rename(displaced, &directory).unwrap();
    assert!(store.unchanged().is_ok());
    assert!(
        store
            .recover_exact_write(&identity, b"owned-response")
            .unwrap()
    );
}

#[test]
fn identical_ciphertext_cannot_adopt_a_replaced_profile_claim() {
    let (_root, identity, mut store) = fixture();
    store.save(b"owned-response").unwrap();
    let directory = store.path.parent().unwrap().to_path_buf();
    let lock = directory.join(super::super::browser_window::PROFILE_LOCK_FILE);
    let displaced = directory.join("held-original.lock");
    std::fs::rename(&lock, &displaced).unwrap();
    let replacement = super::super::browser_window::ProfileLock::claim(&directory).unwrap();
    let before = std::fs::read(&store.path).unwrap();
    assert!(store.unchanged().is_err());
    assert!(store.save(b"must-not-overwrite").is_err());
    assert!(
        store
            .recover_exact_write(&identity, b"owned-response")
            .is_err()
    );
    assert_eq!(std::fs::read(&store.path).unwrap(), before);
    drop(replacement);
    std::fs::remove_file(&lock).unwrap();
    std::fs::rename(displaced, &lock).unwrap();
    store.unchanged().unwrap();
}
