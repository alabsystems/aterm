// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn administrative_capability_is_distinct_from_open_join_policy_and_unknown_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("admin"));
    assert!(!can_administer(&paths).unwrap());
    let vault = root.path().join("closed.vault");
    create(&paths, vault.to_str().unwrap(), None, Some("admin")).unwrap();
    assert!(can_administer(&paths).unwrap());
    let _applying = StateLock::take(&paths).unwrap();
    assert!(
        can_administer(&paths).is_err(),
        "a busy vault is unknown, not a non-admin"
    );
}
