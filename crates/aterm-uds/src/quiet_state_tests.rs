// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

#[test]
fn owned_open_quiet_parent_is_narrowed_before_file_and_directory_handoffs() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let cache = scratch.path().join("cache");
    crate::private_file::write_private(scratch.path(), "cache", b"retained cache").unwrap();
    let quiet_parent = destination(&cache).parent().unwrap().to_path_buf();
    std::fs::create_dir(&quiet_parent).unwrap();
    std::fs::set_permissions(&quiet_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache_inode = std::fs::metadata(&cache).unwrap().ino();
    let target = prepare_file_cache(&cache, 1024).unwrap();
    assert_eq!(
        std::fs::metadata(&quiet_parent).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(std::fs::metadata(&target).unwrap().ino(), cache_inode);
    assert_eq!(std::fs::read(target).unwrap(), b"retained cache");

    let history = scratch.path().join("history");
    crate::private_file::create_private_dir(&history).unwrap();
    crate::private_file::write_private(&history, "retained", b"acknowledged history").unwrap();
    let history_inode = std::fs::metadata(&history).unwrap().ino();
    std::fs::set_permissions(&quiet_parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    prepare(&history, || Ok(())).unwrap();
    assert_eq!(
        std::fs::metadata(&quiet_parent).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(destination(&history)).unwrap().ino(),
        history_inode
    );
    assert_eq!(
        std::fs::read(destination(&history).join("retained")).unwrap(),
        b"acknowledged history"
    );
}

#[test]
fn a_linked_quiet_parent_is_not_narrowed_or_used() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = scratch.path().join("cache");
    crate::private_file::write_private(scratch.path(), "cache", b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, destination(&cache).parent().unwrap()).unwrap();
    assert!(prepare_file_cache(&cache, 1024).is_err());
    let history = scratch.path().join("history");
    crate::private_file::create_private_dir(&history).unwrap();
    assert!(
        prepare(&history, || -> io::Result<()> {
            panic!("no legacy lease through a linked parent")
        })
        .is_err()
    );
    assert_eq!(std::fs::metadata(&outside).unwrap().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(std::fs::read(cache).unwrap(), b"untouched");
    assert!(history.is_dir());
}

#[test]
fn repairing_the_quiet_parent_does_not_relax_legacy_store_checks() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let history = scratch.path().join("history");
    std::fs::create_dir(&history).unwrap();
    std::fs::set_permissions(&history, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(prepare(&history, || Ok(())).is_err());
    assert_eq!(std::fs::metadata(&history).unwrap().mode() & 0o777, 0o755);
    assert!(!destination(&history).exists());
    let cache = scratch.path().join("cache");
    std::fs::write(&cache, b"untrusted cache").unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(prepare_file_cache(&cache, 1024).is_err());
    assert_eq!(std::fs::read(&cache).unwrap(), b"untrusted cache");
    assert_eq!(std::fs::metadata(&cache).unwrap().mode() & 0o777, 0o644);
    assert!(!destination(&cache).exists());
}

#[test]
fn shared_or_owner_readonly_quiet_parents_are_preserved() {
    for mode in [0o555, 0o775, 0o777, 0o1755, 0o1777] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let cache = scratch.path().join("cache");
        crate::private_file::write_private(scratch.path(), "cache", b"retained").unwrap();
        let parent = destination(&cache).parent().unwrap().to_path_buf();
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(prepare_file_cache(&cache, 1024).is_err());
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o7777, mode);
        assert_eq!(std::fs::read(&cache).unwrap(), b"retained");
        assert!(!destination(&cache).exists());
        let history = scratch.path().join("history");
        crate::private_file::create_private_dir(&history).unwrap();
        assert!(prepare(&history, || Ok(())).is_err());
        assert!(history.is_dir());
        assert!(!destination(&history).exists());
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o7777, mode);
    }
}

#[test]
fn file_cache_exchange_preserves_the_inode_and_refuses_old_atomic_writers() {
    let scratch = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let old = scratch.path().join("peers.state");
    crate::private_file::write_private(scratch.path(), "peers.state", b"verified hints").unwrap();
    let inode = std::fs::metadata(&old).unwrap().ino();
    let pending = scratch.path().join("old-writer-pending");
    std::fs::write(&pending, b"old publication").unwrap();
    let quiet = prepare_file_cache(&old, 1024).unwrap();
    assert!(aterm_tempfile::is_quiet(&quiet));
    assert_eq!(std::fs::metadata(&quiet).unwrap().ino(), inode);
    assert_eq!(std::fs::read(&quiet).unwrap(), b"verified hints");
    assert!(old.is_dir());
    assert!(
        std::fs::rename(&pending, &old).is_err(),
        "an old rename cannot fork the cache"
    );
    assert!(crate::private_file::write_private(scratch.path(), "peers.state", b"old").is_err());
    assert_eq!(prepare_file_cache(&old, 1024).unwrap(), quiet);
    let fresh = scratch.path().join("fresh.state");
    let target = prepare_file_cache(&fresh, 1024).unwrap();
    assert!(fresh.is_dir());
    assert!(!target.exists());
    crate::private_file::write_private(
        target.parent().unwrap(),
        target.file_name().unwrap().to_str().unwrap(),
        b"new hints",
    )
    .unwrap();
    assert_eq!(prepare_file_cache(&fresh, 1024).unwrap(), target);
}

#[test]
fn staged_file_handoff_resumes_latest_publication_and_preserves_unsafe_stores() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let old = scratch.path().join("peers.state");
    crate::private_file::write_private(scratch.path(), "peers.state", b"before").unwrap();
    // A public-protocol fence reproduces the stage before an exchange. An old
    // writer may still publish while that migration is crashed.
    let donor = scratch.path().join("donor.state");
    prepare_file_cache(&donor, 1024).unwrap();
    std::fs::rename(&donor, destination(&old)).unwrap();
    crate::private_file::write_private(scratch.path(), "peers.state", b"latest").unwrap();
    let inode = std::fs::metadata(&old).unwrap().ino();
    let quiet = prepare_file_cache(&old, 1024).unwrap();
    assert_eq!(std::fs::read(&quiet).unwrap(), b"latest");
    assert_eq!(std::fs::metadata(&quiet).unwrap().ino(), inode);
    let ambiguous = scratch.path().join("ambiguous.state");
    crate::private_file::write_private(scratch.path(), "ambiguous.state", b"original").unwrap();
    crate::private_file::write_private(
        destination(&ambiguous).parent().unwrap(),
        "ambiguous.state.noindex",
        b"other",
    )
    .unwrap();
    assert!(prepare_file_cache(&ambiguous, 1024).is_err());
    assert_eq!(std::fs::read(&ambiguous).unwrap(), b"original");
    assert_eq!(std::fs::read(destination(&ambiguous)).unwrap(), b"other");
    let oversized = scratch.path().join("oversized.state");
    crate::private_file::write_private(scratch.path(), "oversized.state", b"too large").unwrap();
    assert!(prepare_file_cache(&oversized, 1).is_err());
    assert!(oversized.is_file());
    assert!(!destination(&oversized).exists());
    let linked = scratch.path().join("linked.state");
    std::os::unix::fs::symlink(&oversized, &linked).unwrap();
    assert!(prepare_file_cache(&linked, 1024).is_err());
    assert!(oversized.is_file());
}

#[test]
fn migration_preserves_inodes_and_locks_and_fences_legacy_writers() {
    let scratch = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let old = scratch.path().join("history");
    crate::private_file::create_private_dir(&old).unwrap();
    std::fs::write(old.join("retained"), b"acknowledged history").unwrap();
    std::fs::set_permissions(old.join("retained"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let inode = std::fs::metadata(old.join("retained")).unwrap().ino();
    let lease = crate::private_file::PrivateLock::take(
        &old,
        "writer.lock",
        std::time::Duration::ZERO,
        "held",
    )
    .unwrap();
    assert!(
        prepare(&old, || crate::private_file::PrivateLock::take(
            &old,
            "writer.lock",
            std::time::Duration::ZERO,
            "held"
        ))
        .is_err()
    );
    assert_eq!(
        std::fs::read(old.join("retained")).unwrap(),
        b"acknowledged history"
    );
    drop(lease);
    prepare(&old, || {
        crate::private_file::PrivateLock::take(
            &old,
            "writer.lock",
            std::time::Duration::ZERO,
            "held",
        )
    })
    .unwrap();
    let new = destination(&old);
    assert!(aterm_tempfile::is_quiet(&new));
    assert_eq!(
        std::fs::metadata(new.join("retained")).unwrap().ino(),
        inode
    );
    assert_eq!(
        std::fs::read(new.join("retained")).unwrap(),
        b"acknowledged history"
    );
    assert!(old.is_file());
    assert!(
        std::fs::create_dir_all(&old).is_err(),
        "an older writer cannot recreate a parallel store"
    );
    prepare(&old, || -> io::Result<()> {
        panic!("completed migrations take no old lease")
    })
    .unwrap();
}

#[test]
fn fresh_state_is_fenced_and_ambiguous_or_symlinked_stores_are_preserved() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let old = scratch.path().join("fresh");
    prepare(&old, || -> io::Result<()> { panic!("no old writer") }).unwrap();
    assert!(destination(&old).is_dir());
    let old = scratch.path().join("ambiguous");
    crate::private_file::create_private_dir(&old).unwrap();
    crate::private_file::create_private_dir(&destination(&old)).unwrap();
    assert!(prepare(&old, || Ok(())).is_err());
    assert!(old.is_dir());
    let link = scratch.path().join("linked");
    std::os::unix::fs::symlink(&old, &link).unwrap();
    assert!(prepare(&link, || Ok(())).is_err());
    assert!(old.is_dir());
}

#[test]
fn interrupted_exchange_resumes_and_a_replaced_source_is_refused() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let old = scratch.path().join("history");
    crate::private_file::create_private_dir(&old).unwrap();
    crate::private_file::create_private_dir(destination(&old).parent().unwrap()).unwrap();
    // Crash state immediately before RENAME_SWAP: marker staged, history untouched.
    std::fs::write(destination(&old), RETIRED).unwrap();
    std::fs::set_permissions(destination(&old), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(old.join("retained"), b"retained").unwrap();
    assert_eq!(selected(&old), old);
    prepare(&old, || Ok(())).unwrap();
    assert_eq!(selected(&old), destination(&old));
    assert_eq!(
        std::fs::read(destination(&old).join("retained")).unwrap(),
        b"retained"
    );

    let old = scratch.path().join("replaced");
    let saved = scratch.path().join("saved");
    crate::private_file::create_private_dir(&old).unwrap();
    assert!(
        prepare(&old, || {
            std::fs::rename(&old, &saved)?;
            crate::private_file::create_private_dir(&old)
        })
        .is_err()
    );
    assert!(saved.is_dir());
    assert!(old.is_dir());
    assert!(!destination(&old).exists());
}
