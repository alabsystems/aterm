// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_multiline_append_is_refused_without_mutation_or_creation() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("ledger");
    assert_eq!(
        append(&path, "a\nb").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!path.exists());
    assert!(!path.with_file_name("ledger.lock").exists());
    std::fs::write(&path, b"keep").unwrap();
    assert!(append(&path, "\n").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    append(&path, "next").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"keep\nnext\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let created = tmp.path().join("new");
        ensure(&created).unwrap();
        assert_eq!(
            std::fs::metadata(created).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

#[test]
fn a_held_log_reads_what_another_writer_appended_and_appends_after_it() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    let mut first = hold(&path).unwrap();
    assert_eq!(first.append(&["one"]).unwrap(), 4);
    let before = first.identity().unwrap();
    drop(first);
    // Another writer appends an interrupted row, then this one holds the log again.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"two")
        .unwrap();
    let mut again = hold(&path).unwrap();
    let now = again.identity().unwrap();
    assert_eq!((now.dev, now.ino), (before.dev, before.ino));
    assert_eq!(again.read_from(before.len, 64).unwrap(), b"two");
    // The held append repairs the delimiter and writes its lines in one piece after it.
    assert_eq!(again.append(&["three", "four"]).unwrap(), 19);
    assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\nthree\nfour\n");
    assert_eq!(
        again.append(&["fi\nve"]).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // While it is held, another writer waits for the lock and is refused at its deadline.
    assert!(append(&path, "six").is_err());
    drop(again);
    append(&path, "six").unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"one\ntwo\nthree\nfour\nsix\n"
    );
}
