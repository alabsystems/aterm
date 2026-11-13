// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{alab_roster, headless_oauth::registry_tests as fixture};
use super::*;

/// A registry mailbox that receives every address, its password held, is a catch-all login
/// with no `ai-mailbox.toml` present; one address's own mailbox is not.
/// NEGATIVE CONTROL: with the registry's routes left out of discovery, the catch-all reads
/// `false`.
#[test]
fn a_registry_catch_all_mailbox_is_a_stewards_login() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mut all = fixture::mailbox();
    all.receives = vec!["*".into()];
    let steward = alab_roster::with_mail(vec![all], fixture::secrets(), || {
        super::super::headless_oauth::discover_mailbox_config_files(Some(home.path()), None, None)
            .is_some_and(|cfg| has_catch_all_login(&cfg))
    });
    assert!(steward);
    let mut own = fixture::mailbox();
    own.login = "ai1@example.test".into();
    own.receives = vec!["ai1@example.test".into()];
    let not = alab_roster::with_mail(vec![own], fixture::secrets(), || {
        super::super::headless_oauth::discover_mailbox_config_files(Some(home.path()), None, None)
            .is_some_and(|cfg| has_catch_all_login(&cfg))
    });
    assert!(!not);
    assert_eq!(
        alab_roster::reveals(),
        0,
        "judging a steward reveals nothing"
    );
}
