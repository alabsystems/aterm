// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn readonly_posix_mode_does_not_hide_a_real_acl_write_grant() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("object-sealing-input");
    // This checks actual sealing mechanics only, not MH_OBJECT production
    // or an opaque NativeClosed/SDK/output-lock constructor.
    std::fs::write(&path, b"actual bounded original bytes").unwrap();
    let file = File::open(&path).unwrap();
    seal(&file).unwrap();
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o400);
    let result = crate::actor::owner::spawnfd::command("/bin/chmod")
        .args(["+a", "everyone allow write"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(result.success(), "actual fixture ACL setup failed");
    file.set_permissions(Permissions::from_mode(0o400)).unwrap();
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o400);
    assert!(
        seal(&file).is_err(),
        "a real allow ACL survives chmod0400 and must refuse"
    );
    assert!(
        crate::actor::owner::spawnfd::command("/bin/chmod")
            .arg("-N")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    seal(&file).unwrap();
}
