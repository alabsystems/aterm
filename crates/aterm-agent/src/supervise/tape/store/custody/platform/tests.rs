// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, symlink};

fn dir(path: &Path) {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .expect("owned directory");
}

#[test]
fn held_store_handles_do_not_delete_an_ancestor_or_entry_replacement() {
    let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
    let base = guard
        .path()
        .canonicalize()
        .expect("owned canonical fixture");
    let root = base.join("tapes");
    dir(&root);
    let held = Store::open(&root, false).expect("private store");
    let made = held
        .create(Some("@s-1"), "one.atape")
        .expect("exclusive tape");
    assert!(
        held.create(Some("@s-1"), "one.atape").is_err(),
        "occupied name is preserved"
    );
    let moved = base.join("moved");
    std::fs::rename(&root, &moved).expect("move owned root");
    let other = base.join("other");
    dir(&other);
    dir(&other.join("s-1"));
    std::fs::write(
        other.join("s-1/one.atape"),
        b"unrelated ancestor replacement",
    )
    .expect("own bytes");
    symlink(&other, &root).expect("owned replacement link");
    held.retire(&made).expect("retire under original held root");
    assert!(!moved.join("s-1/one.atape").exists());
    assert_eq!(
        std::fs::read(other.join("s-1/one.atape")).expect("replacement retained"),
        b"unrelated ancestor replacement"
    );

    let mut made = held.create(Some("@s-1"), "two.atape").expect("second tape");
    made.file.write_all(b"original").expect("original bytes");
    let path = moved.join("s-1/two.atape");
    std::fs::rename(&path, moved.join("s-1/original-held.atape")).expect("move original entry");
    let mut replacement = super::super::super::create(&path).expect("new replacement");
    replacement
        .write_all(b"unrelated entry replacement")
        .expect("replacement bytes");
    drop(replacement);
    assert!(
        held.retire(&made).is_err(),
        "changed entry refuses retirement"
    );
    assert_eq!(
        std::fs::read(&path).expect("replacement restored"),
        b"unrelated entry replacement"
    );
    assert_eq!(
        std::fs::read(moved.join("s-1/original-held.atape")).expect("original retained"),
        b"original"
    );
}
