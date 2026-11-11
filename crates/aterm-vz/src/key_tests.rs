// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_key_disk_is_owner_only_whole_sectors_and_reads_back_its_key() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("key.img");
    let (a, b) = (fresh().unwrap(), fresh().unwrap());
    assert!(!same(&a, &b), "each boot's key is fresh");
    let recorded = aterm_durable::seam::Recording::start();
    write_disk(&path, &a).unwrap();
    assert_eq!(
        recorded.take(),
        [aterm_durable::Flush::Durable, aterm_durable::Flush::Durable],
        "both the staged key disk and its parent directory flush durably"
    );
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.len(), DISK_BYTES);
    assert_eq!(meta.len() % 512, 0);
    assert_eq!(aterm_vz_agent::key::read_disk(&path).unwrap(), Some(a));
    // The next boot's replaces it, owner-only again even after a chmod.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_disk(&path, &b).unwrap();
    assert_eq!(aterm_vz_agent::key::read_disk(&path).unwrap(), Some(b));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// NEGATIVE CONTROL: if the parent directory's flush fails after placing the key disk,
/// `write_disk` surfaces the flush failure rather than claiming durability.
#[test]
fn write_disk_surfaces_parent_directory_flush_failure() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("key.img");
    let key = fresh().unwrap();
    let _fail = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO);
    let err = write_disk(&path, &key).expect_err("parent dir EIO must fail write_disk");
    assert_eq!(err.raw_os_error(), Some(aterm_durable::seam::EIO));
}

/// NEGATIVE CONTROL: `write_disk` refuses a symlinked parent directory without staging or
/// renaming the boot's secret key into the symlink's target.
#[test]
fn write_disk_refuses_symlinked_parent_directory() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let linked = dir.path().join("run");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    let key = fresh().unwrap();

    assert!(write_disk(&linked.join("key.img"), &key).is_err());
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}
