// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_directory_cannot_replace_a_write_lock() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("config");
    let lock = target.with_file_name(name(target.file_name().unwrap()));
    std::fs::create_dir(&lock).unwrap();
    let saved = lock.join("saved");
    std::fs::write(&saved, "keep").unwrap();
    assert!(Held::try_take(&target).is_err());
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "keep");
}

#[cfg(unix)]
#[test]
fn a_lock_link_cannot_harden_or_change_another_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("config");
    let other = dir.path().join("other");
    std::fs::write(&other, "keep").unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
    let lock = target.with_file_name(name(target.file_name().unwrap()));
    std::os::unix::fs::symlink(&other, &lock).unwrap();
    assert!(Held::try_take(&target).is_err());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "keep");
    assert_eq!(
        std::fs::metadata(&other).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(
        std::fs::symlink_metadata(lock)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
