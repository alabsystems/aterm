// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn legacy_receipt(directory: &Path) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join(BUNDLE_RECEIPT),
        format!(
            r#"{{"installed_by":"abrew","version":"old","sha256":"{}"}}"#,
            "0".repeat(64)
        ),
    )
    .unwrap();
}

#[test]
fn legacy_status_preserves_state_until_explicit_owned_removal() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    prefix.ensure().unwrap();
    let directory = prefix.homebrew_bundle();
    legacy_receipt(&directory);
    let legacy_data = directory.join("legacy-data");
    std::fs::write(&legacy_data, b"legacy").unwrap();
    let owner_data = prefix.root.join("owner-work");
    std::fs::write(&owner_data, b"keep").unwrap();

    assert!(bundle_record(&directory).is_some());
    assert!(
        !runnable_bundle(&directory),
        "a broken legacy bundle is removable"
    );
    bundle_status(&prefix);
    assert_eq!(std::fs::read(&legacy_data).unwrap(), b"legacy");
    bundle_remove(&prefix).unwrap();
    assert!(!directory.exists());
    assert_eq!(std::fs::read(&owner_data).unwrap(), b"keep");
    bundle_remove(&prefix).unwrap();
}

#[test]
fn bundle_never_removes_foreign_trees_or_follows_receipt_links() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(temp.path());
    let directory = prefix.homebrew_bundle();
    std::fs::create_dir(&directory).unwrap();
    let foreign = directory.join("owner-work");
    std::fs::write(&foreign, b"keep").unwrap();
    assert!(bundle_remove(&prefix).is_err());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    let record = temp.path().join("foreign-record");
    std::fs::write(
        &record,
        format!(
            r#"{{"installed_by":"abrew","version":"old","sha256":"{}"}}"#,
            "0".repeat(64)
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(&record, directory.join(BUNDLE_RECEIPT)).unwrap();
    assert!(bundle_record(&directory).is_none());
    assert!(bundle_remove(&prefix).is_err());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    assert!(std::fs::read(&record).unwrap().starts_with(b"{"));
}

#[test]
fn legacy_removal_does_not_follow_a_linked_bundle_directory() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&temp.path().join("prefix"));
    prefix.ensure().unwrap();
    let outside = temp.path().join("outside");
    legacy_receipt(&outside);
    let owner_data = outside.join("owner-work");
    std::fs::write(&owner_data, b"keep").unwrap();
    let directory = prefix.homebrew_bundle();
    std::os::unix::fs::symlink(&outside, &directory).unwrap();

    assert!(bundle_record(&directory).is_none());
    assert!(bundle_remove(&prefix).is_err());
    assert_eq!(std::fs::read(&owner_data).unwrap(), b"keep");
    assert!(
        directory
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
