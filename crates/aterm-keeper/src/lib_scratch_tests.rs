// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// NEGATIVE CONTROL: even an oversized UTF-8 tag keeps `.noindex` naming, private `0700`
/// permissions, and a keeper socket path strictly below BSD `SUN_LEN` (`103` bytes).
#[test]
fn test_scratch_dir_stays_private_and_under_unix_sun_len() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("oversized-keeper-tag-ééé-that-would-exceed-sun-len");
    let name = scratch
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 scratch dir name");
    assert!(name.ends_with(".noindex"), "missing .noindex: {name}");
    let mode = std::fs::symlink_metadata(scratch.path())
        .expect("stat scratch")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "scratch mode {mode:o}");
    let sock = scratch.path().join("designated.sock");
    assert!(
        sock.as_os_str().as_bytes().len() < 103,
        "keeper socket path {} ({} bytes) must stay below BSD SUN_LEN (103)",
        sock.display(),
        sock.as_os_str().as_bytes().len()
    );
}
