// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn readiness_is_copied_without_unrelated_build_records() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source/17");
    let target = dir.path().join("target/17");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let ready = atpkg::store::ready_marker_path(&source).unwrap();
    std::fs::write(&ready, b"retained readiness bytes\n").unwrap();
    std::fs::write(source.with_file_name("17.not-admission"), b"diagnostic\n").unwrap();
    copy_records(&source, &target).unwrap();
    assert_eq!(
        std::fs::read(atpkg::store::ready_marker_path(&target).unwrap()).unwrap(),
        b"retained readiness bytes\n"
    );
    assert!(!target.with_file_name("17.not-admission").exists());
    assert_eq!(std::fs::read(ready).unwrap(), b"retained readiness bytes\n");
}

#[test]
fn a_linked_admission_record_is_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("source/17");
    let target = dir.path().join("target/17");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let other = dir.path().join("outside-the-build");
    std::fs::write(&other, b"not an admission record\n").unwrap();
    std::os::unix::fs::symlink(&other, atpkg::store::ready_marker_path(&source).unwrap()).unwrap();
    assert!(copy_records(&source, &target).is_err());
    assert!(!atpkg::store::ready_marker_path(&target).unwrap().exists());
}
