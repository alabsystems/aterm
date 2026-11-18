// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The profile's account read as a standing: what a traced [`account_at`] records, and what
//! moves it.

use super::*;
use aterm_uds::trace::{self, Moved, Now};
use std::time::Instant;

/// A profile as Claude Code writes it: its account, and a count it bumps on every start.
fn profile_text(account: &str, starts: u32) -> String {
    format!(
        "{{\"numStartups\":{starts},\"tipsHistory\":{{\"a\":{starts}}},\
         \"oauthAccount\":{{\"accountUuid\":\"{account}\",\"emailAddress\":\"a@example.com\"}}}}"
    )
}

/// A traced read of the profile's account records a standing of the account alone, and no
/// stamp: a rewrite that keeps the account (Claude Code's own, on every start) leaves it
/// standing, while another account, or the profile gone, moves it. The read answers as
/// [`read`] does.
///
/// NEGATIVE CONTROL (2026-11-15): `account_at` answering `read_account(path)` with the scope
/// armed (its read recorded as a stamp, no standing) failed at the standings' record, left `[]`,
/// right `[".../.claude.json"]`; restored, the file touched.
#[test]
fn a_profile_rewrite_that_keeps_its_account_leaves_a_traced_account_read_standing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(".claude.json");
    std::fs::write(&path, profile_text("one", 1)).expect("the profile");

    let (account, trace) = trace::scope(|| account_at(&path));
    assert_eq!(account, read(&path).map(|profile| profile.key));
    assert_eq!(account, Ok(AccountKey::claude("one")));
    assert_eq!(trace.standings().collect::<Vec<_>>(), [path.as_path()]);
    assert_eq!(trace.files().count(), 0, "{trace:?}");
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let stands = || trace.stands(Now::at(0), Instant::now());
    assert_eq!(stands(), Ok(()));

    std::fs::write(&path, profile_text("one", 12)).expect("a start counted");
    assert_eq!(stands(), Ok(()), "a rewrite that kept the account moved it");

    std::fs::write(&path, profile_text("two", 12)).expect("another account");
    assert_eq!(
        stands(),
        Err(Moved::Standing(path.clone())),
        "another account signed in left the trace standing"
    );
    std::fs::write(&path, profile_text("one", 13)).expect("the account back");
    assert_eq!(stands(), Ok(()));
    std::fs::remove_file(&path).expect("the profile gone");
    assert_eq!(stands(), Err(Moved::Standing(path.clone())));
}

/// A missing profile stands as missing until one is written, and an untraced read answers the
/// same as a traced one.
///
/// NEGATIVE CONTROL (2026-11-15, the same mutant as the test above): `account_at` answering
/// `read_account(path)` with the scope armed failed at the written profile, left
/// `Err(File(".../.claude.json"))`, right `Err(Standing(".../.claude.json"))`; restored, the
/// file touched.
#[test]
fn a_missing_profile_stands_until_one_is_written() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(".claude.json");
    let (account, trace) = trace::scope(|| account_at(&path));
    assert_eq!(account, Err(NoProfile::Missing));
    assert_eq!(account_at(&path), Err(NoProfile::Missing));
    assert_eq!(trace.unkept(), None, "{trace:?}");
    assert_eq!(trace.stands(Now::at(0), Instant::now()), Ok(()));
    std::fs::write(&path, profile_text("one", 1)).expect("the profile");
    assert_eq!(
        trace.stands(Now::at(0), Instant::now()),
        Err(Moved::Standing(path.clone()))
    );
}
