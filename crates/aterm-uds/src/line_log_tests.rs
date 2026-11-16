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

/// A log replaced by a rename, as a compaction replaces it, is another file at once to both
/// reads of its identity, the look at its path and the read through an open handle: on Windows
/// the replacement takes the replaced file's creation time (file system tunneling), which is
/// therefore no identity there. NEGATIVE CONTROL: an append keeps the file's identity and moves
/// only its length.
#[test]
fn a_log_replaced_by_a_rename_is_another_file_at_once() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal");
    append(&path, "one").unwrap();
    let before = identity_at(&path).unwrap();
    assert_eq!(before, hold(&path).unwrap().identity().unwrap());
    assert_eq!(before.len, 4);
    append(&path, "two").unwrap();
    let grown = identity_at(&path).unwrap();
    assert_eq!(
        (grown.dev, grown.ino, grown.len),
        (before.dev, before.ino, 8)
    );
    let next = tmp.path().join("journal.next");
    std::fs::write(&next, b"one\n").unwrap();
    std::fs::rename(&next, &path).unwrap();
    let after = identity_at(&path).unwrap();
    assert_ne!((after.dev, after.ino), (before.dev, before.ino));
    assert_eq!(after.len, 4);
    assert_eq!(after, hold(&path).unwrap().identity().unwrap());
    assert_eq!(
        identity_at(&tmp.path().join("missing")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

/// ONE LOOK NAMES ONE FILE: [`identity_and_modified_at`] reads the identity [`identity_at`]
/// reads and the modified time the path's own metadata reads, of whatever is at the path; a file
/// renamed over it reads as the new file at once. NEGATIVE CONTROL: the replaced file's identity
/// is not the new one's. Nothing there is `NotFound`.
#[test]
fn a_look_reads_one_files_identity_and_modified_time() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let path = tmp.path().join("rows");
    append(&path, "one").unwrap();
    let (before, modified) = identity_and_modified_at(&path).unwrap();
    assert_eq!(before, identity_at(&path).unwrap());
    assert_eq!(
        modified,
        Some(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
        )
    );
    let next = tmp.path().join("rows.next");
    std::fs::write(&next, b"two\nthree\n").unwrap();
    std::fs::rename(&next, &path).unwrap();
    let (after, modified) = identity_and_modified_at(&path).unwrap();
    assert_ne!((after.dev, after.ino), (before.dev, before.ino));
    assert_eq!(after, identity_at(&path).unwrap());
    assert_eq!(after.len, 10);
    assert_eq!(
        modified,
        Some(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
        )
    );
    assert_eq!(
        identity_and_modified_at(&tmp.path().join("missing"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}
