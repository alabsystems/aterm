// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk watch's reserve: real files in a scratch directory.

use super::*;
use crate::harness::disk::tests::Tmp;

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
