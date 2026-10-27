// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cleanup requires complete evidence that the keeper's copied code ran.

use std::io::{self, ErrorKind};
use std::path::PathBuf;

use crate::keeper_world::{World, keeper_code, scan_unrun_copies, unrun_copies};

#[test]
fn a_world_without_copies_can_be_removed() {
    let root = aterm_tempfile::tempdir().expect("scratch world");
    let witness = root.path().join("witness");
    std::fs::write(&witness, b"owned fixture").expect("witness");
    assert!(
        unrun_copies(root.path())
            .expect("no code directory")
            .is_empty()
    );
    drop(World(root.path().to_path_buf()));
    assert!(!witness.exists());
}

#[test]
fn an_unreadable_code_directory_keeps_the_world() {
    let root = aterm_tempfile::tempdir().expect("scratch world");
    let code = keeper_code(root.path());
    std::fs::create_dir_all(code.parent().expect("parent")).expect("code parent");
    std::fs::write(&code, b"not a directory").expect("directory failure witness");
    assert!(unrun_copies(root.path()).is_err());
    drop(World(root.path().to_path_buf()));
    assert_eq!(
        std::fs::read(code).expect("world retained"),
        b"not a directory"
    );
}

#[test]
fn a_directory_or_symlink_is_not_a_childs_execution_marker() {
    let root = aterm_tempfile::tempdir().expect("scratch world");
    let copy = keeper_code(root.path()).join("123-456-789");
    let marker = copy.join(aterm_keeper::code::RAN);
    std::fs::create_dir_all(&marker).expect("malformed marker directory");
    assert_eq!(
        unrun_copies(root.path())
            .expect_err("directory is not a marker")
            .kind(),
        ErrorKind::InvalidData
    );
    drop(World(root.path().to_path_buf()));
    assert!(marker.is_dir());
    std::fs::remove_dir(&marker).expect("remove non-executable fixture directory");
    let target = copy.join("other-file");
    std::fs::write(&target, b"").expect("symlink target fixture");
    std::os::unix::fs::symlink(&target, &marker).expect("malformed marker symlink");
    assert_eq!(
        unrun_copies(root.path())
            .expect_err("symlink is not a marker")
            .kind(),
        ErrorKind::InvalidData
    );
    drop(World(root.path().to_path_buf()));
    assert!(
        std::fs::symlink_metadata(marker)
            .expect("world retained")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn directory_entry_and_marker_errors_never_establish_a_safe_cleanup() {
    let failed_read: io::Result<Vec<io::Result<PathBuf>>> =
        Err(io::Error::from(ErrorKind::PermissionDenied));
    assert_eq!(
        scan_unrun_copies(failed_read, |_| unreachable!("no entries"))
            .expect_err("unreadable directory")
            .kind(),
        ErrorKind::PermissionDenied
    );
    // NotFound is empty only before enumeration; a disappearing entry is uncertain.
    for kind in [ErrorKind::NotFound, ErrorKind::Other] {
        assert_eq!(
            scan_unrun_copies(Ok([Err(io::Error::from(kind))]), |_| {
                unreachable!("unreadable entry")
            })
            .expect_err("entry error")
            .kind(),
            kind
        );
    }
    assert_eq!(
        scan_unrun_copies(Ok([Ok(PathBuf::from("123-456-789"))]), |_| {
            Err(io::Error::from(ErrorKind::PermissionDenied))
        })
        .expect_err("unreadable marker")
        .kind(),
        ErrorKind::PermissionDenied
    );
}
