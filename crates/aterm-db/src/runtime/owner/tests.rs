// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_window_claim_never_creates_the_observers_directory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let dir = crate::network_state::directory(&config);
    assert!(WindowClaim::acquire(&config).is_err());
    assert!(!dir.exists(), "the observer alone prepares its directory");

    private_file::create_private_dir(&dir).unwrap();
    let claim = WindowClaim::acquire(&config).unwrap();
    assert!(window_claimed(&config));
    drop(claim);
    assert!(!window_claimed(&config));
}

#[test]
#[cfg(target_os = "macos")]
fn window_claim_follows_observer_migration_and_preserves_release_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let legacy = config.with_file_name("network-state");
    private_file::create_private_dir(&legacy).unwrap();
    let first = WindowClaim::acquire(&config).unwrap();
    let original = first.file.metadata().unwrap();
    assert!(window_claimed(&config));
    assert_eq!(crate::network_state::directory(&config), legacy);

    let history = crate::network_state::Observer::open(
        &config,
        crate::network_state::ObservationHandle::default(),
    )
    .unwrap();
    let quiet = crate::network_state::directory(&config);
    assert_ne!(quiet, legacy);
    assert!(legacy.is_file(), "the original name is a retirement fence");
    let moved = std::fs::metadata(quiet.join("window.lock")).unwrap();
    assert_eq!((original.dev(), original.ino()), (moved.dev(), moved.ino()));
    assert!(
        window_claimed(&config),
        "the immutable config still resolves the moved claim"
    );

    let second = WindowClaim::acquire(&config).unwrap();
    let same = second.file.metadata().unwrap();
    assert_eq!((original.dev(), original.ino()), (same.dev(), same.ino()));
    drop(first);
    assert!(window_claimed(&config));
    drop(second);
    assert!(!window_claimed(&config));
    let fresh = WindowClaim::acquire(&config).unwrap();
    assert!(window_claimed(&config));
    drop(fresh);
    assert!(!window_claimed(&config));
    drop(history);
}
