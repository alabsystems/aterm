// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn kill_group_refuses_non_positive_signals_and_invalid_groups() {
    for sig in [0, -1, -9] {
        assert_eq!(
            kill_group(4242, sig).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "signal {sig}"
        );
    }
    for pgid in [0, 1] {
        assert_eq!(
            kill_group(pgid, SIGKILL).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "pgid {pgid}"
        );
    }
    assert!(!alive(0));
    assert!(!alive(u32::MAX));
    assert!(alive(std::process::id()));
}

/// The declared `O_NOFOLLOW` is this architecture's: an open of a symlink with it
/// fails with `ELOOP` (40 on Linux), where the same open without it follows the link.
#[cfg(target_os = "linux")]
#[test]
fn the_declared_nofollow_refuses_a_symlink() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");
    std::fs::write(&target, b"x").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let open = |flags| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(&link)
    };
    assert!(
        open(O_NONBLOCK | O_CLOEXEC).is_ok(),
        "the link itself opens"
    );
    let refused = open(O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(40), "{refused}");
}
