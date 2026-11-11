// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Flush, read_marker, remove_marker, write_marker};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-marker-{tag}-{}-{}",
        std::process::id(),
        aterm_time::since_epoch().unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A record round-trips and re-records in place with no sibling left; an oversized text is
/// refused, an oversized file is not a record, a directory fails the write closed, and on
/// unix a planted link is neither read nor written through.
#[test]
fn markers_are_exclusive_bounded_and_never_written_through() {
    let dir = scratch("roundtrip");
    let marker = dir.join("answered");
    assert_eq!(read_marker(&marker, 64), None, "absent: not a record");
    write_marker(&marker, "first", 64, Flush::Durable).unwrap();
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("first"));
    write_marker(&marker, "second", 64, Flush::Durable).unwrap();
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("second"));
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "the exclusive sibling is renamed away, never left beside the marker"
    );
    let refused = write_marker(&marker, &"x".repeat(65), 64, Flush::Durable).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        read_marker(&marker, 64).as_deref(),
        Some("second"),
        "a refused write changes nothing"
    );
    std::fs::write(&marker, "x".repeat(65)).unwrap();
    assert_eq!(read_marker(&marker, 64), None, "oversized: not a record");
    std::fs::remove_file(&marker).unwrap();
    #[cfg(unix)]
    {
        let target = dir.join("planted-target");
        std::fs::write(&target, "planted").unwrap();
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        assert_eq!(read_marker(&marker, 64), None, "a link is not a record");
        let refused = write_marker(&marker, "through?", 64, Flush::Durable).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            std::fs::symlink_metadata(&marker)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the planted link is left alone"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "planted");
        std::fs::remove_file(&marker).unwrap();
        std::fs::remove_file(&target).unwrap();
    }
    std::fs::create_dir(&marker).unwrap();
    assert_eq!(read_marker(&marker, 64), None);
    assert!(write_marker(&marker, "x", 64, Flush::Durable).is_err());
    assert_eq!(
        remove_marker(&marker).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists,
        "a directory is not a marker and is not removed"
    );
    assert!(std::fs::symlink_metadata(&marker).unwrap().is_dir());
    std::fs::remove_dir(&marker).unwrap();
    write_marker(&marker, "gone soon", 64, Flush::Durable).unwrap();
    remove_marker(&marker).unwrap();
    assert!(!marker.exists());
    remove_marker(&marker).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A durable marker makes two durable flushes, an ordered one a single ordered flush.
#[cfg(unix)]
#[test]
fn a_durable_marker_also_flushes_its_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::TempDir::new().expect("a private marker fixture");
    let marker = root.path().join("answered");
    let recording = aterm_durable::seam::Recording::start();
    write_marker(&marker, "kept", 64, Flush::Durable).unwrap();
    assert_eq!(
        recording.take(),
        [Flush::Durable, Flush::Durable],
        "the sibling and the directory"
    );
    write_marker(&marker, "ordered", 64, Flush::Ordered).unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "the sibling only");
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("ordered"));

    let shut = root.path().join("shut");
    std::fs::create_dir(&shut).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o300)).unwrap();
    let inside = shut.join("answered");
    let written = write_marker(&inside, "renamed", 64, Flush::Durable);
    let flushes = recording.take();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    written.expect("the rename happened: written");
    assert_eq!(
        flushes,
        [Flush::Durable],
        "the sibling only: the directory would not open"
    );
    assert_eq!(read_marker(&inside, 64).as_deref(), Some("renamed"));
}

/// The parent directory is created on demand: a fresh install has no config directory yet.
#[test]
fn the_parent_directory_is_created_on_demand() {
    let dir = scratch("mkdir");
    let marker = dir.join("nested").join("answered");
    write_marker(&marker, "ok", 64, Flush::Durable).unwrap();
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("ok"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed exclusive create owns no temporary file to remove. CONTROL:
/// an ordinary private marker still lands and replaces its own record.
#[test]
fn a_marker_temporary_collision_is_left_with_its_creator() {
    let root = aterm_tempfile::TempDir::new().expect("a private marker fixture");
    let marker = root.path().join("answered");
    write_marker(&marker, "before", 64, Flush::Durable).expect("ordinary marker");
    write_marker(&marker, "standing", 64, Flush::Durable).expect("ordinary replacement");
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("standing"));

    let temporary = root
        .path()
        .join(format!("answered.tmp-{}", std::process::id()));
    std::fs::write(&temporary, b"another creator's bytes").expect("a collision");
    let error = write_marker(&marker, "replacement", 64, Flush::Durable)
        .expect_err("exclusive create refuses");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(read_marker(&marker, 64).as_deref(), Some("standing"));
    assert_eq!(
        std::fs::read(&temporary).ok(),
        Some(b"another creator's bytes".to_vec()),
        "a refused exclusive create has no cleanup authority over the collision"
    );
}

/// NEGATIVE CONTROL: symlinked parent directories are refused by `read_marker`,
/// `write_marker`, and `remove_marker` without touching files in the link target.
#[cfg(unix)]
#[test]
fn markers_refuse_symlinked_parent_directories() {
    let root = aterm_tempfile::TempDir::new().expect("a private marker fixture");
    let target_dir = root.path().join("target-dir");
    std::fs::create_dir(&target_dir).expect("target dir");
    let target_marker = target_dir.join("answered");
    std::fs::write(&target_marker, "untouched").expect("target marker");

    let linked_dir = root.path().join("linked-dir");
    std::os::unix::fs::symlink(&target_dir, &linked_dir).expect("symlink dir");
    let through_link = linked_dir.join("answered");

    assert_eq!(read_marker(&through_link, 64), None);
    let write_err = write_marker(&through_link, "overwritten", 64, Flush::Durable)
        .expect_err("symlinked parent must be refused");
    assert_eq!(write_err.kind(), std::io::ErrorKind::InvalidInput);
    let remove_err =
        remove_marker(&through_link).expect_err("symlinked parent must not unlink target");
    assert_eq!(remove_err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        std::fs::read_to_string(&target_marker).expect("target intact"),
        "untouched"
    );
}
