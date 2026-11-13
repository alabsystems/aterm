// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{newer_for_a_start, older_than};

/// AN OLDER INSTALLED BUILD IS ONLY ONE BELOW THIS ONE: the build a revert put back, never
/// this build or a newer one, which [`newer_for_a_start`] judges.
#[test]
fn only_a_build_below_this_one_is_an_older_installed_build() {
    assert_eq!(older_than(100, Some(99)), Some(99), "a revert put 99 back");
    assert_eq!(older_than(100, Some(100)), None, "this build stands here");
    assert_eq!(
        older_than(100, Some(101)),
        None,
        "a newer build stands here"
    );
    assert_eq!(
        older_than(100, None),
        None,
        "the installed build cannot be read"
    );
}

/// A NODE START RUNS NO YANKED INSTALLED BUILD: the installed arm counts only once the floor
/// reads that it leaves the build, while a staged build above this one counts as it did. The
/// control is the same installed build with a floor that leaves it.
#[test]
fn a_yanked_or_unjudged_installed_build_is_no_newer_build_for_a_start() {
    let leaves = |_: u64| Ok(false);
    let yanks = |build: u64| Ok(build < 102);
    let unread = |_: u64| Err("the floor is unreadable".to_owned());
    assert_eq!(
        newer_for_a_start(100, None, Some(101), leaves),
        Some(101),
        "CONTROL: a window installed 101 over this image and the floor leaves it"
    );
    assert_eq!(
        newer_for_a_start(100, None, Some(101), yanks),
        None,
        "the floor yanks 101: a start would run a build a window refuses to activate"
    );
    assert_eq!(
        newer_for_a_start(100, None, Some(101), unread),
        None,
        "a floor that cannot be read does not clear the installed build"
    );
    assert_eq!(
        newer_for_a_start(100, Some(102), Some(101), yanks),
        Some(102),
        "a staged build above this one still counts beside a yanked installed one"
    );
    assert_eq!(
        newer_for_a_start(100, Some(100), Some(99), leaves),
        None,
        "nothing above this build"
    );
}
