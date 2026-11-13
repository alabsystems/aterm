// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_an_earlier_boot_is_reclaimed_and_links_are_not_followed() {
    let world = aterm_tempfile::tempdir().unwrap();
    let root = world.path().join("images.noindex");
    create_private_dir(&root).unwrap();
    let current = "11111111111111111111111111111111";
    let old = "22222222222222222222222222222222";
    let linked = "33333333333333333333333333333333";
    for name in [current, old, "unrecognized"] {
        create_private_dir(&root.join(name)).unwrap();
        std::fs::write(root.join(name).join("unproved"), b"keep during this boot").unwrap();
    }
    let outside = world.path().join("outside");
    create_private_dir(&outside).unwrap();
    std::fs::write(outside.join("owned"), b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, root.join(linked)).unwrap();
    sweep(&root, current).unwrap();
    assert!(root.join(current).join("unproved").is_file());
    assert!(!root.join(old).exists());
    assert!(root.join("unrecognized").join("unproved").is_file());
    assert!(outside.join("owned").is_file());
    assert!(sweep(&root, "").is_err());
}
