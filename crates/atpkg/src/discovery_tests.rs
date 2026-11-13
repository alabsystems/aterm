// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn config_account_overrides_default_but_is_validated() {
    assert_eq!(resolve_account(Some("my-org")).owner, "my-org");
    assert_eq!(resolve_account(Some("my-org")).repo, INDEX_REPO);
    assert_eq!(
        resolve_account(Some("evil.com/x")).owner,
        aterm_update_core::ATPKG_INDEX_OWNER
    );
    assert_eq!(
        resolve_account(Some("a b")).owner,
        aterm_update_core::ATPKG_INDEX_OWNER
    );
    assert_eq!(
        resolve_account(Some("   ")).owner,
        aterm_update_core::ATPKG_INDEX_OWNER
    );
}
