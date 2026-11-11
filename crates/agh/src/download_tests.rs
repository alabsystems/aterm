// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

/// If a second download replaces the part file, the first neither renames nor removes it.
#[test]
fn a_part_file_another_download_replaced_is_left_to_it() {
    let root = crate::test_fixtures::scratch_dir("part-race");
    let dest = root.path().join("asset.bin");
    let mut first = Part::create(&dest).expect("the first part file");
    first
        .file()
        .expect("open")
        .write_all(b"first")
        .expect("the first bytes");
    let mut second = Part::create(&dest).expect("the second part file");
    second
        .file()
        .expect("open")
        .write_all(b"second")
        .expect("the second bytes");

    let error = first
        .finish(&dest)
        .expect_err("the first part file was replaced");
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.sub, Sub::Local);
    assert!(!dest.exists());
    assert_eq!(
        std::fs::read(part_path(&dest)).expect("the second part file"),
        b"second"
    );

    second.finish(&dest).expect("the second download finishes");
    assert_eq!(std::fs::read(&dest).expect("the destination"), b"second");
    assert!(!part_path(&dest).exists());
}

/// A part file dropped on an error is removed when it is still this download's.
#[test]
fn a_dropped_part_file_is_removed() {
    let root = crate::test_fixtures::scratch_dir("part-drop");
    let dest = root.path().join("asset.bin");
    drop(Part::create(&dest).expect("a part file"));
    assert!(!part_path(&dest).exists());
}

/// NEGATIVE CONTROL: `file_digest` and `refuse_non_file` reject live and dangling symlinks
/// without following them, rather than treating a symlink to a regular file as a regular file
/// or treating a dangling symlink as absent.
#[test]
fn file_digest_and_refuse_non_file_reject_live_and_dangling_symlinks() {
    let root = crate::test_fixtures::scratch_dir("dest-symlink");
    let target = root.path().join("target.bin");
    std::fs::write(&target, b"payload").expect("write target");
    let live_link = root.path().join("live.bin");
    std::os::unix::fs::symlink(&target, &live_link).expect("live symlink");
    let dangling_link = root.path().join("dangling.bin");
    std::os::unix::fs::symlink(root.path().join("missing.bin"), &dangling_link)
        .expect("dangling symlink");

    for link in [&live_link, &dangling_link] {
        let err = refuse_non_file(link).expect_err("symlink destination must be refused");
        assert_eq!(err.kind, Kind::Usage, "{}", err.why);
        let err = file_digest(link).expect_err("file_digest must not follow symlinks");
        assert_eq!(err.kind, Kind::Usage, "{}", err.why);
        assert!(
            std::fs::symlink_metadata(link)
                .expect("symlink intact")
                .file_type()
                .is_symlink()
        );
    }
    assert_eq!(std::fs::read(&target).expect("target intact"), b"payload");
    assert!(
        file_digest(&root.path().join("absent.bin"))
            .unwrap()
            .is_none()
    );
    assert!(file_digest(&target).unwrap().is_some());
}
