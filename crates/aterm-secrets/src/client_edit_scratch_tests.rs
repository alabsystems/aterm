// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Paths, Scratch, host_name_hint};
use std::os::unix::fs::PermissionsExt;

#[test]
fn an_unpublished_edit_and_unclaimed_files_survive_later_edits() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    crate::fsx::ensure_private_dir(&paths.dir).expect("state directory");
    let unclaimed = paths.dir.join("edit-unclaimed");
    std::fs::create_dir(&unclaimed).expect("unclaimed directory");
    std::fs::write(unclaimed.join("draft"), b"unclaimed text").expect("unclaimed draft");

    let edit = Scratch::new(&paths).expect("exclusive edit");
    let kept = edit.dir().to_path_buf();
    assert_ne!(kept, unclaimed);
    assert_eq!(
        std::fs::symlink_metadata(&kept)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert!(
        crate::fsx::write_new(&kept.join("draft"), b"unpublished changes", 0o600)
            .expect("private draft")
    );
    drop(edit);
    assert_eq!(
        std::fs::read(kept.join("draft")).expect("kept"),
        b"unpublished changes"
    );

    let changed = Scratch::new(&paths).expect("changed cleanup control");
    let changed_dir = changed.dir().to_path_buf();
    let changed_file = changed_dir.join("draft");
    crate::fsx::write_atomic(&changed_file, b"later unpublished text", 0o600)
        .expect("private later edit");
    assert!(changed.discard(&changed_file, b"published text").is_err());
    assert_eq!(
        std::fs::read(&changed_file).expect("changed draft kept"),
        b"later unpublished text"
    );

    let completed = Scratch::new(&paths).expect("another exclusive edit");
    let completed_dir = completed.dir().to_path_buf();
    assert_ne!(completed_dir, kept);
    assert_eq!(
        std::fs::read(unclaimed.join("draft")).expect("unclaimed kept"),
        b"unclaimed text"
    );
    assert_eq!(
        std::fs::read(kept.join("draft")).expect("interrupted edit kept"),
        b"unpublished changes"
    );
    let published = completed_dir.join("draft");
    crate::fsx::write_atomic(&published, b"published text", 0o600).expect("private published copy");
    std::fs::write(completed_dir.join("editor-note"), b"other unpublished text")
        .expect("editor file");
    completed
        .discard(&published, b"published text")
        .expect("discard only the completed draft");
    assert!(!published.exists());
    assert!(!completed_dir.join(".lock").exists());
    assert_eq!(
        std::fs::read(completed_dir.join("editor-note")).expect("editor file kept"),
        b"other unpublished text"
    );

    let completed = Scratch::new(&paths).expect("empty cleanup control");
    let dir = completed.dir().to_path_buf();
    let published = dir.join("draft");
    crate::fsx::write_atomic(&published, b"published text", 0o600).expect("private published copy");
    completed
        .discard(&published, b"published text")
        .expect("known completed draft removed");
    assert!(!dir.exists(), "an empty owned edit directory is retired");
    assert_eq!(
        std::fs::read(kept.join("draft")).expect("still kept"),
        b"unpublished changes"
    );
}
#[test]
fn a_broadly_readable_completed_draft_is_refused_and_preserved() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let edit = Scratch::new(&Paths::for_home(home.path())).expect("edit");
    let file = edit.dir().join("draft");
    assert!(crate::fsx::write_new(&file, b"published text", 0o600).expect("private draft"));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))
        .expect("exposed draft control");
    assert!(edit.discard(&file, b"published text").is_err());
    assert_eq!(
        std::fs::read(&file).expect("refused draft kept"),
        b"published text"
    );
}

#[test]
fn computer_name_precedes_a_local_hostname_label_hint() {
    assert_eq!(
        host_name_hint(&["Workstation".into(), "m22.local".into()]),
        "workstation"
    );
    assert_eq!(host_name_hint(&["m22-Macbook.local".into()]), "m22");
    assert_eq!(host_name_hint(&[]), "machine");
}
