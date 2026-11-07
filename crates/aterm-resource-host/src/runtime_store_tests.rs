// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::fd::AsFd as _;

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn expectation_compares_held_kernel_directories_instead_of_names_or_copies() {
    let temporary = aterm_tempfile::tempdir().unwrap();
    let selected = temporary.path().join("selected");
    let foreign = temporary.path().join("foreign");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let original = std::fs::File::open(&selected).unwrap();
    let same = original.try_clone().unwrap();
    assert!(same_store_description(&original, same.as_fd()).unwrap());
    assert!(
        !same_store_description(&original, std::fs::File::open(&foreign).unwrap().as_fd()).unwrap()
    );
    std::fs::rename(&selected, temporary.path().join("moved-original")).unwrap();
    std::fs::create_dir(&selected).unwrap();
    assert!(
        !same_store_description(&original, std::fs::File::open(&selected).unwrap().as_fd())
            .unwrap()
    );
    assert!(same_store_description(&original, same.as_fd()).unwrap());
    let file = std::fs::File::create(temporary.path().join("file")).unwrap();
    assert!(same_store_description(&original, file.as_fd()).is_err());
}

#[test]
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unqualified_acl_platform_cannot_match_an_original_store_expectation() {
    let temporary = aterm_tempfile::tempdir().unwrap();
    let original = std::fs::File::open(temporary.path()).unwrap();
    let copy = original.try_clone().unwrap();
    assert_eq!(
        same_store_description(&original, copy.as_fd())
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
}
