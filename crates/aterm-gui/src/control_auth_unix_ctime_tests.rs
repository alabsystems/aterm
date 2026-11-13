// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ensure_private_dir;

#[cfg(unix)]
#[test]
fn an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = std::env::temp_dir().join(format!("agui-privdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let stamp = || {
        let m = std::fs::metadata(&dir).unwrap();
        (m.ctime(), m.ctime_nsec(), m.mode() & 0o7777)
    };
    let before = stamp();
    assert_eq!(before.2, 0o700);
    ensure_private_dir(&dir).unwrap();
    assert_eq!(stamp(), before, "an unchanged directory was written");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&dir).unwrap();
    assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
    std::fs::remove_dir(&dir).unwrap();
    ensure_private_dir(&dir).unwrap();
    assert_eq!(stamp().2, 0o700, "a removed directory is made again");
    let outside = std::env::temp_dir().join(format!("agui-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = std::env::temp_dir().join(format!("agui-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(ensure_private_dir(&link).is_err());
    assert_eq!(
        std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}
