// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::MetadataExt as _;

#[test]
fn remembered_cache_moves_without_keys_or_scope_or_age_changes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let scope = [7; std::mem::size_of::<MemoryScope>()];
    let remembered = Remembered {
        actor: [3; 48],
        address: "192.0.2.1:6543".parse().unwrap(),
        at_ms: 1234,
    };
    aterm_uds::private_file::write_private(root.path(), "device.age", b"custody stays here")
        .unwrap();
    let key_inode = std::fs::metadata(root.path().join("device.age"))
        .unwrap()
        .ino();
    aterm_uds::private_file::write_private(
        root.path(),
        REMEMBERED_NAME,
        &Remembered::encode(&scope, &[remembered]),
    )
    .unwrap();
    let cache_inode = std::fs::metadata(root.path().join(REMEMBERED_NAME))
        .unwrap()
        .ino();
    let memory = Memory::prepare(root.path(), scope).unwrap();
    assert!(aterm_tempfile::is_quiet(&memory.dir));
    assert_eq!(
        std::fs::metadata(memory.dir.join(&memory.name))
            .unwrap()
            .ino(),
        cache_inode
    );
    assert_eq!(memory.read(), vec![remembered]);
    assert_eq!(
        std::fs::metadata(root.path().join("device.age"))
            .unwrap()
            .ino(),
        key_inode
    );
    assert_eq!(
        std::fs::read(root.path().join("device.age")).unwrap(),
        b"custody stays here"
    );
    assert!(
        aterm_uds::private_file::write_private(root.path(), REMEMBERED_NAME, b"old writer")
            .is_err()
    );
    let newer = Remembered {
        at_ms: 5678,
        ..remembered
    };
    memory.write(&[newer]);
    assert_eq!(memory.read(), vec![newer]);
    let other = Memory::prepare(root.path(), [8; std::mem::size_of::<MemoryScope>()]).unwrap();
    assert!(
        other.read().is_empty(),
        "another trust/seat cannot become a candidate"
    );
}
