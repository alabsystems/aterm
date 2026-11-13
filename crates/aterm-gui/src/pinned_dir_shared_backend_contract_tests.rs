// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::PinnedDir;

fn unique_dir(stem: &str) -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "aterm-pinned-shared-{stem}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
}

/// `remove_file_if_exists` removes a file; a directory on an artifact name survives it on
/// every backend.
#[test]
fn remove_file_if_exists_never_removes_a_directory() {
    let root = unique_dir("remove-file-is-file-only");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();

    let occupied = std::ffi::OsStr::new("snapshot.png.txt");
    std::fs::create_dir(root.join(occupied)).unwrap();
    let error = pinned
        .remove_file_if_exists(occupied)
        .expect_err("a directory is not a file this may remove");
    assert!(
        root.join(occupied).is_dir(),
        "the directory on the artifact name must survive a file-scoped removal: {error}"
    );

    let file = std::ffi::OsStr::new("snapshot.png");
    std::fs::write(root.join(file), b"payload").unwrap();
    pinned.remove_file_if_exists(file).unwrap();
    assert!(!root.join(file).exists());
    pinned.remove_file_if_exists(file).unwrap();

    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn closed_file_identity_revalidates_and_rejects_a_replacement() {
    let root = unique_dir("closed-file-identity");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let name = std::ffi::OsStr::new("frame.png");
    let identity = pinned
        .write_new_private(name, b"original")
        .unwrap()
        .into_identity()
        .unwrap();
    assert_eq!(
        pinned.private_file_identity_at_retained(name).unwrap(),
        identity,
        "the retained-entry probe returns the open file's identity"
    );
    identity.validate_at_retained(&pinned, name).unwrap();

    std::fs::rename(root.join(name), root.join("original.png")).unwrap();
    let replacement = pinned.write_new_private(name, b"replacement").unwrap();
    assert!(
        identity.validate_at_retained(&pinned, name).is_err(),
        "the checkpoint must reject a different object at the same name"
    );

    drop(replacement);
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

/// A recording is pinned by its producer and, at the same time, by every
/// `video frames` reader; retention then renames it to a tombstone and
/// clears it through the capability it already holds.
#[test]
fn two_pins_of_one_directory_coexist_and_a_renamed_one_still_clears() {
    let root = unique_dir("two-pins-rename-clear");
    let name = std::ffi::OsStr::new("recording");
    let tombstone = std::ffi::OsStr::new(".prune-recording");
    std::fs::create_dir_all(root.join(name)).unwrap();
    std::fs::write(root.join(name).join("index.json"), b"{}").unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let producer = pinned.child(name).unwrap();

    let reader_root = PinnedDir::open_resolved(&root).unwrap();
    let reader = reader_root
        .child(name)
        .expect("a second pin of the same directory in one process");
    let (bytes, index) = reader
        .read_private_at_retained(std::ffi::OsStr::new("index.json"), 16)
        .unwrap();
    assert_eq!(bytes, b"{}");
    drop(index);

    pinned
        .rename_child_exact(name, &producer, tombstone)
        .expect("a live reader pin does not block the exact retention rename");
    assert!(
        reader.validate_path_identity().is_err(),
        "the reader sees its recording renamed away and fails closed"
    );
    let mut budget = 16;
    let cleared = producer.clear_contents_with_budget(&mut budget);
    cleared
        .result
        .expect("the renamed capability clears the directory it retains");
    assert_eq!(cleared.removed, 1);
    assert!(!root.join(tombstone).join("index.json").exists());

    drop(reader);
    drop(reader_root);
    pinned
        .remove_empty_child_exact(tombstone, &producer)
        .unwrap();
    drop(producer);
    assert!(!root.join(tombstone).exists());
    assert!(!root.join(name).exists());
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

/// An identity probe that could not look says why: the OS error passes
/// through as it came, a child that vanished says it vanished, and only a
/// real replacement reads as "identity changed".
#[test]
fn a_failed_identity_probe_names_its_error_not_a_replacement() {
    #[cfg(unix)]
    const TOO_MANY_OPEN_FILES: i32 = libc::EMFILE;
    #[cfg(windows)]
    const TOO_MANY_OPEN_FILES: i32 = 4; // ERROR_TOO_MANY_OPEN_FILES
    let root = unique_dir("identity-probe-error");
    let name = std::ffi::OsStr::new("recording");
    let tombstone = std::ffi::OsStr::new(".prune-recording");
    std::fs::create_dir_all(root.join(name)).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let recording = pinned.child(name).unwrap();
    let injected = std::io::Error::from_raw_os_error(TOO_MANY_OPEN_FILES).to_string();

    super::fail_next_identity_probe(TOO_MANY_OPEN_FILES);
    let error = pinned
        .rename_child_exact(name, &recording, tombstone)
        .expect_err("a probe that could not look certifies no rename");
    assert_eq!(
        error.raw_os_error(),
        Some(TOO_MANY_OPEN_FILES),
        "the errno passes through: {error}"
    );
    assert!(error.to_string().contains(&injected), "{error}");
    super::fail_next_identity_probe(TOO_MANY_OPEN_FILES);
    let error = pinned
        .remove_empty_child_exact(name, &recording)
        .expect_err("a probe that could not look certifies no removal");
    assert_eq!(
        error.raw_os_error(),
        Some(TOO_MANY_OPEN_FILES),
        "the errno passes through: {error}"
    );
    assert!(root.join(name).is_dir(), "a failed probe moves nothing");

    std::fs::rename(root.join(name), root.join("moved")).unwrap();
    let error = pinned
        .remove_empty_child_exact(name, &recording)
        .expect_err("a vanished child is not removed");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("vanished"), "{error}");
    assert!(!error.to_string().contains("identity changed"), "{error}");

    std::fs::create_dir(root.join(name)).unwrap();
    let error = pinned
        .rename_child_exact(name, &recording, tombstone)
        .expect_err("a replaced child is not renamed");
    assert!(error.to_string().contains("identity changed"), "{error}");
    assert!(root.join(name).is_dir() && root.join("moved").is_dir());
    assert!(!root.join(tombstone).exists());

    std::fs::remove_dir(root.join(name)).unwrap();
    std::fs::write(root.join(name), b"").unwrap();
    let error = pinned
        .remove_empty_child_exact(name, &recording)
        .expect_err("a child replaced by a file is not removed");
    assert!(error.to_string().contains("identity changed"), "{error}");
    assert!(root.join(name).is_file(), "the file in its place stays");

    #[cfg(unix)]
    {
        std::fs::remove_file(root.join(name)).unwrap();
        std::os::unix::fs::symlink(root.join("moved"), root.join(name)).unwrap();
        let error = pinned
            .rename_child_exact(name, &recording, tombstone)
            .expect_err("a child replaced by a symlink to it is not renamed");
        assert!(error.to_string().contains("identity changed"), "{error}");
        assert!(root.join("moved").is_dir() && !root.join(tombstone).exists());
    }

    drop((recording, pinned));
    let _ = std::fs::remove_dir_all(root);
}

/// "One ordinary component" is decided on the raw name, so `"link/"` and its kin are refused.
#[test]
fn one_component_is_decided_on_the_raw_name() {
    use std::ffi::OsStr;
    let refused = [
        "", ".", "..", "a/b", "/a", "./a", "a/..", "link/", "link/.", "link//", "link/./.", "x\0y",
    ];
    for name in refused {
        assert!(
            super::validate_component(OsStr::new(name)).is_err(),
            "{name:?} is not one ordinary component"
        );
    }
    #[cfg(windows)]
    for name in ["a\\b", "link\\", "link\\."] {
        assert!(
            super::validate_component(OsStr::new(name)).is_err(),
            "{name:?} is not one ordinary component"
        );
    }
    let produced = [
        "rec-00000000000000000001-001",
        ".prune-p4242-0123456789abcdef01234567-00000000000000000007",
        ".instance-prune-p4242-x",
        ".aterm-write-p4242-00000000000000000003",
        "p4242-0123456789abcdef01234567",
        ".instance.lease",
        "index.json",
        "index.json.tmp",
        "frame_0001.png",
        "snapshot.png",
        "snapshot.png.txt",
        "snapshot.png.done",
        "images",
        "auto",
        "video",
        ".capture-locks",
    ];
    for name in produced {
        assert!(
            super::validate_component(OsStr::new(name)).is_ok(),
            "{name:?} is one ordinary component"
        );
    }
}
