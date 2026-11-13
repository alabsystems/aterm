// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn unsupported_native_admission_does_not_create_a_lock_namespace() {
    let world = aterm_tempfile::tempdir().unwrap();
    let directory = world.path().join("native-admission");
    assert!(Held::open(&directory, EXCHANGE).is_err());
    assert!(!directory.exists());
}
