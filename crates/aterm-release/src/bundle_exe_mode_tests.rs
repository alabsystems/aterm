// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt;

/// The laid executable is exactly r-x, so a copy through a link to it (what
/// `cp build ~/.local/bin/aterm` is) is refused and leaves its bytes, and the bundle
/// holding it still deletes whole, as the updater and `ship dev-app` delete bundles.
#[test]
fn a_copy_through_a_link_to_the_bundle_executable_never_rewrites_it() {
    let scratch = crate::test_scratch_dir("bundle");
    let dir = scratch.path();
    let macos = dir.join("aterm.app/Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("create bundle");
    let built = dir.join("built");
    std::fs::write(&built, b"release").expect("write build");
    let exe = macos.join("aterm");
    copy_exe(&built, &exe).expect("copy_exe");
    let mode = std::fs::metadata(&exe).expect("stat").permissions().mode();
    assert_eq!(mode & 0o7777, EXE_MODE, "{mode:o}");

    let link = dir.join("aterm");
    std::os::unix::fs::symlink(&exe, &link).expect("link");
    let debug = dir.join("debug-aterm");
    std::fs::write(&debug, b"a debug build").expect("write debug build");
    let refused = std::fs::copy(&debug, &link).expect_err("a write through the link");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&exe).expect("read"), b"release");

    std::fs::remove_dir_all(dir.join("aterm.app")).expect("the bundle deletes whole");
}
