// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn protected_link_targets_are_refused_before_their_first_metadata_query() {
    let scratch = crate::testdir::dir("atpkg-protected-path-");
    let home = std::fs::canonicalize(&scratch).unwrap();
    let guarded = home.join("Documents");
    let link = home.join(".local");
    std::os::unix::fs::symlink(guarded.join("missing"), &link).unwrap();
    for candidate in [link.join("bin"), link.join("../ordinary")] {
        let result = resolve_path(
            &home,
            &candidate,
            |path| {
                assert!(!path.starts_with(&guarded), "queried {}", path.display());
                std::fs::symlink_metadata(path)
            },
            |path| {
                assert!(!path.starts_with(&guarded));
                std::fs::read_link(path)
            },
        );
        assert!(matches!(result, Err(PathRefusal::ConsentFenced)));
    }
}

#[test]
fn ordinary_missing_targets_are_allowed_but_inspection_failures_and_cycles_are_not() {
    let scratch = crate::testdir::dir("atpkg-protected-path-control-");
    let home = std::fs::canonicalize(&scratch).unwrap();
    let link = home.join(".local");
    std::os::unix::fs::symlink("ordinary/missing", &link).unwrap();
    let candidate = link.join("bin");
    let (resolved, metadata) = resolve_path(
        &home,
        &candidate,
        |path| std::fs::symlink_metadata(path),
        |path| std::fs::read_link(path),
    )
    .unwrap();
    assert_eq!(resolved, home.join("ordinary/missing/bin"));
    assert!(metadata.is_none());
    assert!(
        resolve_path(
            &home,
            &candidate,
            |path| {
                if path == link {
                    Err(std::io::ErrorKind::PermissionDenied.into())
                } else {
                    std::fs::symlink_metadata(path)
                }
            },
            |path| std::fs::read_link(path),
        )
        .is_err_and(|why| matches!(why, PathRefusal::Unreadable(_)))
    );
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(".local", &link).unwrap();
    assert!(
        resolve_path(
            &home,
            &candidate,
            |path| std::fs::symlink_metadata(path),
            |path| std::fs::read_link(path),
        )
        .is_err_and(|why| matches!(why, PathRefusal::Unreadable(_)))
    );
}
