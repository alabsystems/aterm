// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk watch's reserve: real files in a scratch directory.

use super::*;
use crate::harness::disk::tests::Tmp;

fn sparse_reserve(path: &Path, length: u64, prefix: bool) -> u64 {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    if prefix {
        file.write_all(&[0x5a; 4096]).unwrap();
        aterm_durable::ordered(&file).unwrap();
    }
    file.set_len(length).unwrap();
    aterm_durable::ordered(&file).unwrap();
    let meta = file.metadata().unwrap();
    let held = meta.blocks().saturating_mul(512);
    assert_eq!(meta.len(), length);
    assert!(held < length, "the fixture really is sparse: {meta:?}");
    held
}

#[test]
fn reserve_allocation_sparse_files_are_repaired_before_readiness() {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = Tmp::new("reserve-sparse-repair");
    let path = path_for(tmp.path());
    let length = 8 << 20;
    sparse_reserve(&path, length, true);
    assert!(matches!(state(&path), Err(ReserveError::Thin(_))));
    assert_eq!(ensure(&path, length), Ok(true));
    let meta = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!(meta.len(), length);
    assert!(meta.blocks().saturating_mul(512) >= length);
    assert_eq!(state(&path), Ok(State::Present(length)));
    assert_eq!(ensure(&path, length), Ok(false));
    let kept = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!((kept.dev(), kept.ino()), (meta.dev(), meta.ino()));
    assert_eq!(release(&path), Ok(Some(length)));
}

#[test]
fn reserve_allocation_sparse_release_counts_only_held_blocks() {
    for prefix in [false, true] {
        let tmp = Tmp::new("reserve-sparse-release");
        let path = path_for(tmp.path());
        let length = 8 << 20;
        let held = sparse_reserve(&path, length, prefix);
        assert_eq!(release(&path), Ok(Some(held.min(length))));
        assert!(!path.exists());
        assert_eq!(release(&path), Ok(None));
    }
}

#[test]
fn reserve_allocation_shared_inodes_are_left_alone() {
    let tmp = Tmp::new("reserve-shared");
    let path = path_for(tmp.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_blocks(&path, 8192).unwrap();
    let alias = tmp.path().join("another-name");
    std::fs::hard_link(&path, &alias).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(state(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(ensure(&path, 8192), Err(ReserveError::NotOurs(_))));
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&alias).unwrap(), before);
    std::fs::remove_file(alias).unwrap();
    assert_eq!(state(&path), Ok(State::Present(8192)));
    assert_eq!(release(&path), Ok(Some(8192)));
}

#[test]
fn writing_the_reserve_ticks_for_each_block() {
    let tmp = Tmp::new("reserve-ticks");
    let before = aterm_uds::progress::ticks_here();
    write_blocks(&tmp.path().join("reserve"), 3 << 20).expect("written");
    assert_eq!(aterm_uds::progress::ticks_here() - before, 3);
}

#[cfg(target_os = "macos")]
#[test]
fn a_lifetime_pass_guard_refuses_changed_file_privacy_and_read_write_access() {
    use std::os::unix::fs::PermissionsExt as _;

    let home = aterm_tempfile::tempdir().expect("owned text fixture");
    let path = pass_lock_for(home.path());
    let held = take_pass(&path).unwrap().expect("real pass guard");
    std::fs::write(&path, b"original pass lock text\n").unwrap();
    let identity = held.validate_for(&path).expect("private read-write guard");
    for mode in [0o400, 0o200, 0o644] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(held.validate_for(&path).is_err(), "mode {mode:o}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(held.validate_for(&path).unwrap(), identity);
    }
    let alias = path.with_file_name("pass-alias.lock");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(
        held.validate_for(&path).is_err(),
        "another name is not private"
    );
    std::fs::remove_file(alias).unwrap();
    let dir = path.parent().unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(held.validate_for(&path).is_err(), "open parent refused");
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(held.validate_for(&path).unwrap(), identity);
    assert_eq!(std::fs::read(&path).unwrap(), b"original pass lock text\n");
    assert!(take_pass(&path).unwrap().is_none(), "same lock still held");
    drop(held);
}

#[test]
fn the_reserve_is_written_once_and_released_by_one_unlink() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let tmp = aterm_tempfile::TempDir::new().expect("owned scratch directory");
    let path = path_for(tmp.path());
    assert_eq!(state(&path), Ok(State::Absent));
    let folder = path.parent().expect("reserve folder");
    std::fs::create_dir_all(folder).expect("reserve folder");
    let preexisting = folder.join(format!(".{FILE}.{}.tmp", std::process::id()));
    std::fs::write(&preexisting, b"keep this preexisting file").expect("preexisting stage name");
    let recording = aterm_durable::seam::Recording::start();
    assert_eq!(ensure(&path, 3 * 1024 * 1024 + 5), Ok(true));
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "the ballast's blocks are only ordered ahead of its rename"
    );
    let meta = std::fs::symlink_metadata(&path).expect("written");
    assert_eq!(meta.len(), 3 * 1024 * 1024 + 5);
    assert!(
        meta.blocks() * 512 >= meta.len(),
        "blocks written: {meta:?}"
    );
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(ensure(&path, 3 * 1024 * 1024 + 5), Ok(false), "kept as is");
    assert_eq!(release(&path), Ok(Some(3 * 1024 * 1024 + 5)));
    assert!(!path.exists());
    assert_eq!(release(&path), Ok(None));
    assert_eq!(
        std::fs::read(&preexisting).expect("preexisting stage survives"),
        b"keep this preexisting file"
    );
}

#[test]
fn a_link_in_the_reserves_place_is_never_followed_or_removed() {
    let tmp = Tmp::new("reserve-link");
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, b"keep me").expect("victim");
    let path = path_for(&tmp.path().join("home"));
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    std::os::unix::fs::symlink(&victim, &path).expect("link");
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(ensure(&path, 1024), Err(ReserveError::NotOurs(_))));
    assert_eq!(std::fs::read(&victim).expect("victim"), b"keep me");
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("link")
            .file_type()
            .is_symlink()
    );

    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("reserve"), b"not the reserve").expect("file");
    let home = tmp.path().join("home2");
    let folder = path_for(&home).parent().expect("folder").to_path_buf();
    std::fs::create_dir_all(folder.parent().expect("cache")).expect("cache");
    std::os::unix::fs::symlink(&elsewhere, &folder).expect("folder link");
    assert!(matches!(
        release(&path_for(&home)),
        Err(ReserveError::NotOurs(_))
    ));
    assert!(matches!(
        ensure(&path_for(&home), 1024),
        Err(ReserveError::NotOurs(_))
    ));
    assert_eq!(
        std::fs::read(elsewhere.join("reserve")).expect("file"),
        b"not the reserve"
    );
}

#[test]
fn the_pass_lock_lets_one_pass_run_at_a_time() {
    let tmp = Tmp::new("pass-lock");
    let home = tmp.path().join("home");
    let path = pass_lock_for(&home);
    assert_eq!(
        path.parent(),
        path_for(&home).parent(),
        "beside the reserve"
    );
    let first = take_pass(&path).expect("made").expect("taken");
    assert!(take_pass(&path).expect("opened").is_none(), "held");
    drop(first);
    let again = take_pass(&path).expect("opened");
    assert!(again.is_some(), "released by LOCK_UN");
    drop(again);
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, b"keep").expect("victim");
    std::fs::remove_file(&path).expect("unlinked");
    std::os::unix::fs::symlink(&victim, &path).expect("link");
    assert!(take_pass(&path).is_err(), "a link is not followed");
    assert_eq!(std::fs::read(&victim).expect("victim"), b"keep");
}
