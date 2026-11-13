// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn unsupported_acquisition_is_not_absence_and_does_not_touch_storage() {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    let directory = world.path().join("owner");
    assert!(require_available().is_err());
    assert!(machine_identity(&home).is_err());
    assert!(Store::open_owned(&directory, &home, 4096).is_err());
    assert!(Store::open_owned_existing(&directory, &home, 4096).is_err());
    assert!(registry_lock(&directory).is_err());
    assert_eq!(
        ensure_private_dir(&directory).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        write_private(&directory, "auth.json", b"candidate")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(
        read_private(&directory, "auth.json", 4096)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    assert!(!home.exists());
    assert!(!directory.exists());
}
