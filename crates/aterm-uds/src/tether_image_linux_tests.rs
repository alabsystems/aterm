// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn cached_images_require_full_equality_and_detect_same_inode_corruption() {
    let world = aterm_tempfile::tempdir().unwrap();
    let original = world.path().join("source");
    let copy = world.path().join("copy");
    std::fs::write(&original, b"recorded image").unwrap();
    let source = File::open(&original).unwrap();
    std::fs::write(&copy, b"different code").unwrap();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(Retained::read(copy.clone(), &source, 14).is_err());
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&copy, b"recorded image").unwrap();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o500)).unwrap();
    let retained = Retained::read(copy.clone(), &source, 14).unwrap();
    assert_eq!(retained.path(), Some(copy.as_path()));
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&copy, b"different code").unwrap();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o500)).unwrap();
    let meta = std::fs::metadata(&copy).unwrap();
    assert_eq!(retained.identity, (meta.dev(), meta.ino()));
    assert!(retained.path().is_none());
}
