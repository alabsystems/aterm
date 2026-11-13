// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_an_admitted_ran_witness_releases_a_marked_artifact() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path();
    assert!(
        hold(dir).unwrap().is_none(),
        "genuine missing exec evidence"
    );
    let exec = dir.join(aterm_uds::deadline::EXEC_STAMP);
    let ran = dir.join(aterm_uds::deadline::RAN);
    std::fs::write(&exec, "0").unwrap();
    std::fs::write(&ran, "").unwrap();
    assert!(
        hold(dir).unwrap().is_none(),
        "ordinary empty witness is supported"
    );
    std::fs::remove_file(&ran).unwrap();

    let target = dir.join("other-witness");
    std::fs::write(&target, "unchanged").unwrap();
    std::os::unix::fs::symlink(&target, &ran).unwrap();
    assert!(
        hold(dir).is_err(),
        "an alias cannot supply witness identity"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    std::fs::remove_file(&ran).unwrap();
    std::fs::hard_link(&target, &ran).unwrap();
    assert!(hold(dir).is_err(), "a multiply-linked witness is unknown");
    std::fs::remove_file(&ran).unwrap();
    std::fs::create_dir(&ran).unwrap();
    assert!(hold(dir).is_err(), "a directory cannot be a witness");
    std::fs::remove_dir(&ran).unwrap();
    std::fs::write(&exec, b"\xff").unwrap();
    assert!(hold(dir).is_err(), "invalid UTF-8 is unknown");
    std::fs::write(&exec, vec![b'1'; 65]).unwrap();
    assert!(hold(dir).is_err(), "oversized clock evidence is unknown");
    std::fs::write(&exec, "+1").unwrap();
    assert!(hold(dir).is_err(), "signed clock evidence is unknown");
}
