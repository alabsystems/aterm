// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn exhausted_evidence_budget_never_grants_custody() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let profile = scratch.path().join("profile");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&profile)
        .unwrap();
    let stamp = profile.join("invoked.timestamp");
    std::fs::write(&stamp, b"invoked").unwrap();
    std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o600)).unwrap();
    let held: aterm_dirfd::OwnedFd = std::fs::File::open(scratch.path()).unwrap().into();
    let mut exhausted = 0;
    assert_eq!(super::invocation_at(&held, &[None], &mut exhausted), None);
    let mut enough = 10;
    assert_eq!(
        super::invocation_at(&held, &[None], &mut enough),
        Some(true)
    );
    assert!(enough < 10);
}
