// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn acl(path: &Path, words: &[&str]) {
    assert!(
        aterm_uds::spawnfd::command("/bin/chmod")
            .args(words)
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn private_directory_refuses_an_access_list_despite_private_mode() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("vault");
    ensure_private_dir(&dir).unwrap();
    acl(
        &dir,
        &[
            "+a",
            "everyone allow read,search,file_inherit,directory_inherit",
        ],
    );
    assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
    assert!(ensure_private_dir(&dir).is_err());
    assert!(private_dir_settled(&dir).is_err());
    let target = dir.join("credential");
    let produced = std::cell::Cell::new(false);
    assert!(
        write_atomic_with(&target, 0o600, |writer| {
            produced.set(true);
            writer.write_all(b"fixture credential")
        })
        .is_err()
    );
    assert!(
        !produced.get(),
        "privacy admission precedes producing bytes"
    );
    assert!(write_ordered(&target, b"fixture credential", 0o600).is_err());
    assert!(write_new(&target, b"fixture credential", 0o600).is_err());
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

    acl(&dir, &["-N"]);
    ensure_private_dir(&dir).unwrap();
    private_dir_settled(&dir).unwrap();
    write_atomic(&target, b"fixture credential", 0o600).unwrap();
    assert!(read_private(&target, 64).unwrap().is_some());
}

#[test]
fn vault_reads_locks_and_appends_refuse_acl_grants() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = root.path().join("credential");
    write_atomic(&target, b"fixture credential", 0o600).unwrap();
    acl(&target, &["+a", "everyone allow read,write"]);
    assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
    assert!(read_private(&target, 64).is_err());
    assert!(read_owned(&target, 64).is_err());
    assert!(open_lock(&target).is_err());
    assert!(append_at(&target, &stamp(&target).unwrap(), b"tail").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"fixture credential");
    acl(&target, &["-N"]);
    acl(&target, &["+a", "everyone deny write"]);
    assert!(
        read_private(&target, 64).unwrap().is_some(),
        "deny grants no access"
    );
}
