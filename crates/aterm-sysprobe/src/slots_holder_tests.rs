// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::MetadataExt as _;

#[test]
fn native_file_possession_requires_exact_birth_and_the_actual_open_vnode() {
    let world = aterm_tempfile::tempdir().unwrap();
    let file = std::fs::File::create(world.path().join("held")).unwrap();
    let meta = file.metadata().unwrap();
    let pid = std::process::id();
    let native = crate::kproc::record_read(pid).unwrap().unwrap();
    let holder = Holder {
        pid,
        birth_us: birth_us(&native),
    };
    assert!(holder.holds_file(meta.dev() as u32, meta.ino()));
    assert!(
        !Holder {
            pid,
            birth_us: holder.birth_us + 1
        }
        .holds_file(meta.dev() as u32, meta.ino())
    );
    std::fs::write(world.path().join("neighbor"), b"not open").unwrap();
    let neighbor = std::fs::metadata(world.path().join("neighbor")).unwrap();
    assert!(!holder.holds_file(neighbor.dev() as u32, neighbor.ino()));
    drop(file);
    assert!(!holder.holds_file(meta.dev() as u32, meta.ino()));
}
