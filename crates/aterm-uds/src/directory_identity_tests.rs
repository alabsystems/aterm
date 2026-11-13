// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn neighboring_directories_share_the_volume_but_have_distinct_inodes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let first = read(&a).unwrap();
    let second = read(&b).unwrap();
    assert_eq!(first.dev, second.dev);
    assert_ne!(first.ino, second.ino);
    assert_eq!(first.volume, second.volume);
}

#[test]
fn the_final_component_is_not_followed_but_parent_aliases_still_work() {
    let root = aterm_tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    let dir = parent.join("directory");
    std::fs::create_dir_all(&dir).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(read(&link).is_err());
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&parent, &alias).unwrap();
    assert_eq!(read(&dir).unwrap(), read(&alias.join("directory")).unwrap());
}
