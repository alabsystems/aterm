// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::fd::AsFd as _;

/// A folder and the folder holding it are on one volume, read by name and held alike; on
/// macOS that volume has a UUID (the test scratch is on APFS).
#[test]
fn a_folder_reads_its_volume_by_name_and_held_alike() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let folder = scratch.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let by_name = super::at(aterm_dirfd::CWD, &folder).unwrap();
    let open = std::fs::File::open(&folder).unwrap();
    let held = super::held(open.as_fd()).unwrap();
    let holder = super::at(aterm_dirfd::CWD, scratch.path()).unwrap();
    assert_eq!(by_name, held);
    assert_eq!(by_name, holder);
    assert_eq!(by_name.is_some(), cfg!(target_os = "macos"));
}

/// A link at the name is read as itself, never followed: a dangling one still reads (its own
/// volume), and a missing name is an error, never a volume.
#[cfg(target_os = "macos")]
#[test]
fn a_link_is_read_as_itself_and_a_missing_name_is_an_error() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(scratch.path().join("nothing"), &link).unwrap();
    assert_eq!(
        super::at(aterm_dirfd::CWD, &link).unwrap(),
        super::at(aterm_dirfd::CWD, scratch.path()).unwrap()
    );
    assert!(super::at(aterm_dirfd::CWD, scratch.path().join("missing")).is_err());
}
