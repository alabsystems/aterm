// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, symlink};

pub(super) fn private_dir(path: &Path) {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .expect("owned directory");
}

fn owned_tape(path: &Path, bytes: &[u8]) {
    let mut file = create(path).expect("exclusive owned file");
    file.write_all(bytes).expect("owned bytes");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        .expect("fixed mtime");
}

#[test]
fn a_store_refuses_linked_or_nonprivate_roots_before_removing_any_tape() {
    let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
    let base = guard
        .path()
        .canonicalize()
        .expect("owned canonical fixture");
    let store = base.join("ordinary");
    private_dir(&store);
    private_dir(&store.join("s-1"));
    let path = store.join("s-1/a.atape");
    owned_tape(&path, b"ordinary");
    assert_eq!(list(&store, None).len(), 1);
    assert_eq!(
        forget(&store, std::slice::from_ref(&path)).expect("ordinary forget"),
        [path]
    );
    owned_tape(&store.join("s-1/b.atape"), b"ordinary sweep");
    assert_eq!(
        sweep_within(
            &store,
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            0,
            Duration::ZERO
        )
        .len(),
        1
    );
    let unrelated = store.join("s-1/notes.txt");
    owned_tape(&unrelated, b"not a tape");
    assert!(
        sweep_within(
            &store,
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            0,
            Duration::ZERO
        )
        .is_empty()
    );
    assert_eq!(
        std::fs::read(&unrelated).expect("unrelated retained"),
        b"not a tape"
    );

    let outside = base.join("outside");
    private_dir(&outside);
    private_dir(&outside.join("s-1"));
    let target = outside.join("s-1/foreign.atape");
    let alias = base.join("linked-store");
    symlink(&outside, &alias).expect("owned fixture link");
    owned_tape(&target, b"must survive forget");
    let linked_list_refused = list(&alias, None).is_empty();
    let linked_forget_refused = forget(&alias, &[alias.join("s-1/foreign.atape")]).is_err();
    let survived_forget = std::fs::read(&target).is_ok_and(|b| b == b"must survive forget");
    if !target.exists() {
        owned_tape(&target, b"must survive sweep");
    }
    let before_sweep = std::fs::read(&target).expect("owned sweep fixture");
    let swept = sweep_within(
        &alias,
        SystemTime::UNIX_EPOCH + Duration::from_secs(10),
        0,
        Duration::ZERO,
    );
    let survived_sweep = std::fs::read(&target).is_ok_and(|bytes| bytes == before_sweep);

    let nonprivate = base.join("nonprivate");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&nonprivate)
        .expect("owned nonprivate directory");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&nonprivate, std::fs::Permissions::from_mode(0o755))
        .expect("owned nonprivate mode");
    private_dir(&nonprivate.join("s-1"));
    let occupied = nonprivate.join("s-1/occupied.atape");
    owned_tape(&occupied, b"occupied");
    let nonprivate_refused = forget(&nonprivate, std::slice::from_ref(&occupied)).is_err();
    let nonprivate_preserved = std::fs::read(&occupied).is_ok_and(|b| b == b"occupied");
    assert!(
        linked_list_refused
            && linked_forget_refused
            && survived_forget
            && swept.is_empty()
            && survived_sweep
            && nonprivate_refused
            && nonprivate_preserved,
        "hosted tape cleanup must refuse linked or nonprivate roots and preserve unrelated bytes"
    );
}
