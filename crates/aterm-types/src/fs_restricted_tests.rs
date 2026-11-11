// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Only a directory we own that nobody else can write is safe; group or other READ is the caller's choice.
#[test]
fn private_write_needs_our_ownership_and_no_foreign_write_bit() {
    for (uid, owner, mode, safe) in [
        (501, 501, 0o040700, true),
        (501, 501, 0o040755, true),
        (0, 0, 0o040700, true),
        (501, 0, 0o041777, false),
        (501, 502, 0o040700, false),
        (501, 501, 0o040775, false),
        (501, 501, 0o040757, false),
        (501, 501, 0o040722, false),
    ] {
        assert_eq!(
            dir_safe_for_private_write(uid, owner, mode),
            safe,
            "uid {uid} owner {owner} mode {mode:o}"
        );
    }
}

/// `ensure_private_dir` creates 0700 (parents included), tightens a looser dir of ours, and is idempotent.
#[cfg(unix)]
#[test]
fn ensure_private_dir_creates_0700_tightens_and_is_idempotent() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = crate::test_scratch_dir("priv");
    let root = scratch.path().join("nested-root");
    let dir = root.join("identities").join("worker");
    ensure_private_dir(&dir).expect("created");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(
        mode(&root.join("identities")),
        0o700,
        "parents are 0700 too"
    );
    ensure_private_dir(&dir).expect("idempotent");
    assert_eq!(mode(&dir), 0o700);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&dir).expect("an existing dir of ours is tightened, not refused");
    assert_eq!(mode(&dir), 0o700);
}

/// An already-`0700` directory of ours gets no `chmod(2)`, so its `st_ctime` holds; a drifted mode is still forced back to `0700`.
#[cfg(unix)]
#[test]
fn an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let scratch = crate::test_scratch_dir("ctime");
    let dir = scratch.path().join("privdir");
    ensure_private_dir(&dir).unwrap();
    let stamp = || {
        let m = std::fs::metadata(&dir).unwrap();
        (m.ctime(), m.ctime_nsec(), m.mode() & 0o7777)
    };
    let before = stamp();
    assert_eq!(before.2, 0o700);
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure_private_dir(&dir).unwrap();
    assert_eq!(stamp(), before, "an unchanged directory was written");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&dir).unwrap();
    assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
    let outside = scratch.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(ensure_private_dir(&link).is_err());
    assert_eq!(
        std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

/// A leaf that is a symbolic link or a file is refused in words that name it, with mkdir's
/// kind, and the link's target is neither made private nor written.
#[cfg(unix)]
#[test]
fn a_leaf_that_is_no_real_directory_is_refused_by_name() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = crate::test_scratch_dir("noreal");
    let root = scratch.path();
    let target = root.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let e = create_dir_all_unshared(&link, 0o700).expect_err("a link is no directory of ours");
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert!(
        e.to_string()
            .contains("a symbolic link, not a real directory"),
        "{e}"
    );
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let file = root.join("file");
    std::fs::write(&file, b"").unwrap();
    let e = create_dir_all_unshared(&file, 0o700).expect_err("a file is no directory");
    assert!(e.to_string().contains("a file, not a directory"), "{e}");
    // CONTROL: a real directory is taken as it is.
    create_dir_all_unshared(&target, 0o700).expect("a real directory");
}
