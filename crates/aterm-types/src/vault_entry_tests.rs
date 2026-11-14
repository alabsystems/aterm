// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::Path;

#[test]
fn the_release_token_is_the_public_owners() {
    assert_eq!(
        super::RELEASE_GH_TOKEN,
        format!("{}_{}", super::DEV_GH_TOKEN, crate::channel::PUBLIC_OWNER)
    );
}

#[test]
fn a_plain_entry_lands_in_the_secrets_folder() {
    assert_eq!(
        super::plain_entry_path(Path::new("/h"), super::DEV_GH_TOKEN),
        Path::new("/h/.secrets/gh_access_token")
    );
    assert_eq!(
        super::plain_entry_pointer(super::DEV_GH_TOKEN),
        "~/.secrets/gh_access_token"
    );
}
