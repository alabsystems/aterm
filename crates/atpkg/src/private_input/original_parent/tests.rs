// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::fd::AsFd as _;
use std::os::unix::fs::DirBuilderExt as _;

#[test]
fn actual_original_parent_creates_fresh_owner_and_descriptor_survives_caller_drop() {
    let root = aterm_tempfile::tempdir().unwrap();
    let original = File::open(root.path()).unwrap();
    let producer =
        Producer::create_original_parent(root.path(), "fresh", original.as_fd()).unwrap();
    drop(original);
    producer.check_current().unwrap();
    assert!(producer.path().is_dir());
    drop(producer);
    assert_eq!(
        std::fs::read(root.path().join("fresh/.input-owner")).unwrap(),
        QUARANTINED
    );
}

#[test]
fn foreign_alias_and_non_directory_restrictions_create_no_namespace() {
    let root = aterm_tempfile::tempdir().unwrap();
    let foreign = aterm_tempfile::tempdir().unwrap();
    let original = File::open(root.path()).unwrap();
    assert!(Producer::create_original_parent(foreign.path(), "fresh", original.as_fd()).is_err());
    assert!(!foreign.path().join("fresh").exists());
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(foreign.path(), &alias).unwrap();
    assert!(Producer::create_original_parent(&alias, "fresh", original.as_fd()).is_err());
    assert!(!foreign.path().join("fresh").exists());
    std::fs::write(root.path().join("file"), b"readonly file").unwrap();
    let file = File::open(root.path().join("file")).unwrap();
    assert!(Producer::create_original_parent(root.path(), "fresh", file.as_fd()).is_err());
    assert!(!root.path().join("fresh").exists());
}

#[test]
fn replaced_parent_cannot_redirect_the_original_creation() {
    let world = aterm_tempfile::tempdir().unwrap();
    let parent = world.path().join("parent");
    let retained = world.path().join("original");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&parent)
        .unwrap();
    let original = File::open(&parent).unwrap();
    let result = Producer::create_with_parent(&parent, "fresh", Some(original.as_fd()), |_| {
        std::fs::rename(&parent, &retained)?;
        std::fs::create_dir(&parent)?;
        std::fs::write(parent.join("sentinel"), b"foreign parent")
    });
    assert!(result.is_err());
    assert!(!parent.join("fresh").exists());
    assert_eq!(
        std::fs::read(parent.join("sentinel")).unwrap(),
        b"foreign parent"
    );
    assert!(retained.join("fresh").is_dir());
}
