// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn actual_registration_descriptor_requires_readonly_unaliased_unchanged_input() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("unit-output");
    std::fs::write(&path, b"actual native input descriptor").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let original = File::open(&path).unwrap();
    let before = readonly(&original).unwrap();
    let copy = original.try_clone().unwrap();
    assert!(same_stamp(&before, &readonly(&copy).unwrap()));
    let writable = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    assert!(readonly(&writable).is_err());
    let alias = scratch.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(
        readonly(&original).is_err(),
        "hardlinked output cannot become original executable custody"
    );
    std::fs::remove_file(alias).unwrap();
    let held = readonly(&original).unwrap();
    std::fs::rename(&path, scratch.path().join("retained-original")).unwrap();
    std::fs::write(&path, b"substituted output").unwrap();
    let substituted = File::open(&path).unwrap();
    assert!(!same_stamp(&held, &readonly(&substituted).unwrap()));
    assert_eq!(
        original.metadata().unwrap().ino(),
        held.ino(),
        "the offered original stays owned across pathname substitution"
    );
    std::fs::set_permissions(
        scratch.path().join("retained-original"),
        std::fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    assert!(
        !same_stamp(&held, &readonly(&original).unwrap()),
        "same-object permission drift is not adopted as a new baseline"
    );
}
