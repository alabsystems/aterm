// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::fsx;
#[test]
fn immutable_change_or_lost_history_withdraws_without_repairing_storage() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let journal = paths.dir.join("commons/control");
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::ensure_private_dir(&paths.dir.join("commons")).unwrap();
    fsx::ensure_private_dir(&journal).unwrap();
    for path in [
        paths.dir.join("identity"),
        paths.dir.join("commons/device.age"),
        paths.dir.join("commons/device.claim"),
        paths.dir.join("commons/default.pin"),
        journal.join("control.anchor"),
        journal.join("control.astream"),
        journal.join("control.checkpoint"),
    ] {
        fsx::write_atomic(&path, b"fixture", 0o600).unwrap();
    }
    let custody = Custody::capture(&paths, &journal).unwrap();
    fsx::write_atomic(
        &journal.join("control.checkpoint"),
        b"mutable checkpoint",
        0o600,
    )
    .unwrap();
    custody.recheck().unwrap();
    fsx::write_atomic(&paths.dir.join("identity"), b"replacement", 0o600).unwrap();
    assert!(custody.recheck().is_err());
    fsx::write_atomic(&paths.dir.join("identity"), b"fixture", 0o600).unwrap();
    std::fs::remove_file(journal.join("control.astream")).unwrap();
    assert!(custody.recheck().is_err());
    assert!(!journal.join("control.astream").exists());
}
