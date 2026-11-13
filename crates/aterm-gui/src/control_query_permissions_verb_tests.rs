// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PERMISSIONS_USAGE, parse_permissions};
use crate::permission_walk::{Started, Subject};

#[test]
fn the_grammar_is_walk_now_and_the_subjects_in_order() {
    assert_eq!(parse_permissions("walk now"), Ok(vec![]));
    assert_eq!(
        parse_permissions(" walk  now devtools fda "),
        Ok(vec![Subject::DeveloperTools, Subject::FullDiskAccess])
    );
    for junk in [
        "",
        "walk",
        "now walk",
        "walk later",
        "walk now camera",
        "walk now FDA",
    ] {
        assert_eq!(parse_permissions(junk), Err(PERMISSIONS_USAGE), "{junk:?}");
    }
}

#[test]
fn the_reply_names_the_walks_subjects() {
    assert_eq!(
        Started::Walking {
            pages: vec![Subject::DeveloperTools, Subject::FullDiskAccess],
            not_now: Vec::new(),
        }
        .reply(),
        "OK walking devtools fda\n"
    );
    assert_eq!(
        Started::Walking {
            pages: vec![Subject::DeveloperTools],
            not_now: vec![Subject::FullDiskAccess],
        }
        .reply(),
        "OK walking devtools not-now:fda\n",
        "a page held back by a Not now"
    );
    assert_eq!(Started::Granted.reply(), "OK granted\n");
    assert_eq!(Started::Recent.reply(), "OK recent\n");
    assert_eq!(Started::Declined.reply(), "OK declined\n");
    assert!(Started::Headless.reply().starts_with("ERR headless: "));
}
