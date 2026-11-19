// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::fs;

fn entry(dir: &Path, name: &str, dynamic: bool) -> Entry {
    let path = dir.join(name);
    let lstat = path.symlink_metadata().unwrap();
    let canonical = path.canonicalize().unwrap();
    let metadata = fs::metadata(&canonical).unwrap();
    let link = lstat.is_symlink().then(|| fs::read_link(&path).unwrap());
    let target_identity = link.as_ref().map(|_| Identity::of(&metadata));
    Entry {
        name: name.into(),
        kind: if link.is_some() { "symlink" } else { "file" }.into(),
        identity: Identity::of(&lstat),
        link,
        canonical: Some(canonical),
        target_identity,
        sha256: dynamic.then(|| crate::test_index::digest(&path.canonicalize().unwrap()).unwrap()),
        size: dynamic.then_some(metadata.len()),
    }
}

fn directory(path: &Path, entries: Vec<Entry>) -> Directory {
    Directory {
        path: path.into(),
        canonical: Some(path.canonicalize().unwrap()),
        identity: Some(Identity::of(&path.metadata().unwrap())),
        entries,
    }
}

#[test]
fn native_dynamic_files_follow_the_original_handle_and_static_membership_is_rechecked() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("native");
    fs::create_dir(&path).unwrap();
    let member = path.join("libpart.so");
    fs::write(&member, b"library").unwrap();
    fs::write(path.join("static.a"), b"archive").unwrap();
    std::os::unix::fs::symlink("libpart.so", path.join("alias")).unwrap();
    let original = directory(
        &path,
        vec![
            entry(&path, "alias", true),
            entry(&path, "libpart.so", true),
            entry(&path, "static.a", false),
        ],
    );
    let held = HeldDirectory::capture(original).unwrap();
    held.validate().unwrap();
    fs::write(path.join("static.a"), b"new archive").unwrap();
    assert!(held.validate().is_err());
    fs::write(member, b"new library").unwrap();
    assert!(held.validate_selected().is_err());
}

#[test]
fn directory_replacement_and_dangling_search_entries_refuse() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("native");
    let absent = || Directory {
        path: path.clone(),
        canonical: None,
        identity: None,
        entries: vec![],
    };
    let missing = HeldDirectory::capture(absent()).unwrap();
    std::os::unix::fs::symlink("missing", &path).unwrap();
    assert!(missing.validate_selected().is_err());
    assert!(HeldDirectory::capture(absent()).is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let old = HeldDirectory::capture(directory(&path, vec![])).unwrap();
    fs::rename(&path, root.path().join("old")).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(old.validate_selected().is_err());
    fs::write(path.join("unaccounted"), b"x").unwrap();
    assert!(HeldDirectory::capture(directory(&path, vec![])).is_err());
}

#[test]
fn wrong_library_digest_or_missing_named_dynamic_receipt_refuses() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    fs::write(path.join("test.so"), b"library").unwrap();
    let mut wrong = entry(&path, "test.so", true);
    wrong.sha256 = Some("0".repeat(64));
    assert!(HeldDirectory::capture(directory(&path, vec![wrong])).is_err());
    assert!(
        HeldDirectory::capture(directory(&path, vec![entry(&path, "test.so", false)])).is_err()
    );
}
