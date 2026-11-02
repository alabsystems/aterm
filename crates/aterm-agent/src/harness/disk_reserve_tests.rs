// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk watch's reserve: real files in a scratch directory.

use super::*;
use crate::harness::disk::tests::Tmp;

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
