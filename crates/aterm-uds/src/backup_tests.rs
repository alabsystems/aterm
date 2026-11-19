// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_os = "macos")]
use super::Backups;

#[test]
#[cfg(target_os = "macos")]
fn a_build_folder_is_left_out_of_spotlight_and_time_machine() {
    use std::fs::{self, File};

    let root = aterm_tempfile::tempdir().expect("scratch");
    // The shim gives a caller-selected target this quiet, unindexed leaf.
    let build = root.path().join("target/.ubd/build.noindex");
    assert!(
        Backups::SYSTEM
            .build_dir(&build)
            .expect("claim build folder")
    );
    assert!(
        build
            .file_name()
            .unwrap()
            .as_encoded_bytes()
            .ends_with(b".noindex")
    );
    assert!(Backups::SYSTEM.excluded(&File::open(&build).expect("build folder")));

    let control = root.path().join("build/.ubd/control.noindex");
    fs::create_dir_all(&control).expect("unmarked control");
    assert!(!Backups::SYSTEM.excluded(&File::open(&control).expect("control")));
    fs::write(control.join("object"), b"existing build output").unwrap();
    assert!(
        Backups::SYSTEM
            .build_dir(&control)
            .expect("claim existing folder")
    );
    assert!(Backups::SYSTEM.excluded(&File::open(&control).expect("claimed control")));
    assert_eq!(
        fs::read(control.join("object")).unwrap(),
        b"existing build output"
    );
}
