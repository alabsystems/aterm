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

#[test]
fn handoff_inputs_must_name_ready_builds_in_the_configured_store() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = private_layout(dir.path());
    let original = layout.build_dir("codex", 17);
    let replacement = layout.build_dir("codex", 18);
    let dev = dir.path().join("development/17");
    for path in [&original, &replacement, &dev] {
        std::fs::create_dir_all(path).unwrap();
    }
    atpkg::store::mark_build_ready(&original).unwrap();
    atpkg::store::mark_build_ready(&dev).unwrap();
    let alias = dir.path().join("installed-original");
    std::os::unix::fs::symlink(&original, &alias).unwrap();

    assert_eq!(installed_build(&layout, &alias).unwrap(), 17);
    assert!(installed_build(&layout, &replacement).is_err());
    assert!(installed_build(&layout, &dev).is_err());
    atpkg::store::mark_build_ready(&replacement).unwrap();
    assert_eq!(installed_build(&layout, &replacement).unwrap(), 18);
}

#[test]
fn source_process_must_map_the_exact_admitted_image() {
    let pid = i32::try_from(std::process::id()).unwrap();
    let own_image = Image::at(&std::env::current_exe().unwrap()).unwrap();
    assert_eq!(observe_image(pid, &own_image).unwrap().image, own_image);

    let other = Image::at(Path::new("/usr/bin/true")).unwrap();
    assert!(observe_image(pid, &other).is_none());
    let wrong_inode = Image {
        inode: own_image.inode.wrapping_add(1),
        ..own_image
    };
    assert!(observe_image(pid, &wrong_inode).is_none());
}

#[test]
fn a_cross_release_proof_requires_a_newer_signed_build() {
    assert!(check_successor(2026111601, 2026111701).is_ok());
    assert!(check_successor(2026111701, 2026111701).is_err());
    assert!(check_successor(2026111701, 2026111601).is_err());
}
