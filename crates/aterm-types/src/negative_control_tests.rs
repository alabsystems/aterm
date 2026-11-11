// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{named, on};
use std::ffi::OsStr;

/// One name, a list with spaces and empty slots, an absent variable and a value that is not
/// UTF-8.
/// NEGATIVE CONTROL: splitting on whitespace instead of commas turns `old-gap-rule,
/// unsettled-ok` into one name with a comma in it, and the list case fails.
#[test]
fn the_variable_names_each_control_it_lists() {
    assert_eq!(named(Some(OsStr::new("old-gap-rule"))), ["old-gap-rule"]);
    assert_eq!(
        named(Some(OsStr::new(" old-gap-rule, ,unsettled-ok,"))),
        ["old-gap-rule", "unsettled-ok"]
    );
    assert!(named(None).is_empty());
    assert!(named(Some(OsStr::new(""))).is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert!(named(Some(OsStr::from_bytes(b"old-gap-rule\xff"))).is_empty());
    }
}

/// No name is a control's name: an empty one never matches, whatever the variable says.
#[test]
fn an_empty_name_is_never_on() {
    assert!(!on(""));
}
