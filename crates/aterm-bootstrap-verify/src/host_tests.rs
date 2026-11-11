// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_acl_rule_is_the_updaters() {
    let outcomes = [None, Some(ENODATA), Some(ENOTSUP), Some(5)];
    for posix in outcomes {
        for nfs4 in outcomes {
            let table = |name: &CStr| match name.to_bytes() {
                b"system.posix_acl_access" => posix,
                b"system.nfs4_acl" => nfs4,
                other => panic!("asked {other:?}"),
            };
            assert_eq!(
                acl_rule(table),
                aterm_uds::shared_write::acl_carried(table),
                "posix {posix:?}, nfs4 {nfs4:?}"
            );
        }
    }
    assert!(!acl_rule(|_| Some(ENODATA)), "no ACL");
    assert!(acl_rule(|_| Some(5)), "an unreadable ACL");
}

#[test]
fn the_remote_filesystems_are_the_updaters() {
    assert_eq!(
        REMOTE_FILESYSTEMS,
        aterm_uds::shared_write::REMOTE_FILESYSTEMS
    );
    for magic in [0xEF53, 0x9123_683E, 0x9FA0, 0, u32::MAX] {
        assert_eq!(
            remote_filesystem_type(magic),
            aterm_uds::shared_write::remote_filesystem(magic),
            "{magic:#x}"
        );
    }
}

/// The type statfs reads is the updater's on this machine's paths.
#[cfg(target_os = "linux")]
#[test]
fn the_filesystem_type_is_the_updaters_on_this_machine() {
    let scratch = aterm_tempfile::Builder::new().tempdir().expect("scratch");
    for path in [
        Path::new("/"),
        Path::new("/proc"),
        scratch.path(),
        &scratch.path().join("no-such-entry"),
    ] {
        assert_eq!(
            filesystem_type(path),
            aterm_uds::shared_write::filesystem_type_at(path),
            "{}",
            path.display()
        );
    }
    assert_eq!(filesystem_type(Path::new("/proc")), Some(0x9FA0));
}
