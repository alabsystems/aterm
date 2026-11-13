// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// AN INVENTORY SAYS IT IS STILL GOING: it reads up to a million entries, and one that says
/// nothing while it does reads as a hang to whatever waits on the pass.
#[test]
fn an_inventory_ticks_for_each_entry() {
    const ENTRIES: usize = 5;
    let scratch = aterm_tempfile::tempdir().unwrap();
    for i in 0..ENTRIES {
        std::fs::write(scratch.path().join(i.to_string()), b"").unwrap();
    }
    let before = aterm_uds::progress::ticks_here();
    scan(scratch.path()).unwrap();
    assert!(aterm_uds::progress::ticks_here() - before >= ENTRIES);
}

#[test]
fn incomplete_inventories_never_certify_a_lock_free_prefix() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let deep = root.join("verify/.ubd/cells.noindex/linux/triple/debug");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(".cargo-lock"), b"").unwrap();
    assert!(scan_with(root, 100, 2).is_err());
    assert!(scan_with(root, 2, 100).is_err());
    assert_eq!(scan(root).unwrap().locks, [deep.join(".cargo-lock")]);
    assert!(scan(&root.join("missing")).is_err());
}

#[test]
fn directory_links_are_not_walked_and_nonregular_lock_names_are_kept() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().join("root");
    let external = scratch.path().join("external");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join(".cargo-lock"), b"").unwrap();
    std::os::unix::fs::symlink(&external, root.join("alias")).unwrap();
    assert!(scan(&root).unwrap().locks.is_empty());
    assert!(scan(&root.join("alias")).is_err());
    let obstruction = root.join(".CARGO-LOCK");
    std::fs::create_dir(&obstruction).unwrap();
    let seen = scan(&root).unwrap();
    assert_eq!(seen.locks, [obstruction]);
    assert!(super::super::hold(&seen.locks).is_none());
}
