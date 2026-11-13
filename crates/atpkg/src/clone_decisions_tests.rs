// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_unreadable_file_identity_is_not_a_difference() {
    let a: FileId = (66_306, 1_594_985);
    let b: FileId = (66_306, 1_594_986);
    assert!(distinct_files(Some(a), Some(b)), "two inodes are two files");
    assert!(
        !distinct_files(Some(a), Some(a)),
        "one inode under two names is a HARD LINK, not a clone"
    );
    assert!(
        !distinct_files(None, None),
        "a platform that cannot read a file's identity has not proven two files apart, \
         and `is_clone_of` must therefore answer `false` rather than collapse into \
         `attributes_match` and read a hard link into the store as a clone"
    );
    assert!(!distinct_files(Some(a), None));
    assert!(!distinct_files(None, Some(a)));
}

#[test]
fn the_volume_verdict_refuses_what_it_cannot_prove() {
    let (src, parent) = (
        Path::new("/store/trust/9192/bin/trustc"),
        Path::new("/rustup/trust/bin"),
    );
    assert!(one_volume(src, parent, Some(66_306), Some(66_306)).is_ok());

    let crossed = one_volume(src, parent, Some(66_306), Some(46)).unwrap_err();
    assert_eq!(crossed.kind(), io::ErrorKind::CrossesDevices);

    let unknown = one_volume(src, parent, None, None).unwrap_err();
    assert_eq!(
        unknown.kind(),
        io::ErrorKind::Unsupported,
        "an unprovable volume is a refusal, not a pass"
    );
    assert_eq!(
        one_volume(src, parent, Some(66_306), None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
}

/// Off Unix the platform answers are the ones pinned above, and `clone_file` refuses.
#[cfg(not(unix))]
#[test]
fn off_unix_the_platform_answers_are_unknown_and_a_clone_is_refused() {
    let here = std::env::current_exe().unwrap();
    let m = std::fs::symlink_metadata(&here).unwrap();
    assert!(
        file_identity(&m).is_none(),
        "no stable std API reads a Windows file id"
    );
    let refused = clone_file(&here, &here.with_extension("clone")).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::Unsupported);
}
