// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A jail mark is aterm-lab's spelling; anything else names no jail.
#[test]
fn a_jail_mark_is_the_labs_spelling() {
    assert!(jail_mark("lab.jail.wk3.3a8ec08aab215af4"));
    assert!(jail_mark("lab.jail.probe_1-a"));
    for bad in [
        "",
        "lab.jail.",
        "lab.jailwk3",
        "com.apple.lsd.open",
        "lab.jail.a b",
        "lab.jail.a/b",
    ] {
        assert!(!jail_mark(bad), "{bad:?}");
    }
}

/// The seam keeps the jail's members in the listing's order, and answers only from inside the
/// jail it names: this test process is in none, so it narrows nothing here. CONTROL: a name
/// that is no mark is refused before any process is read.
#[test]
fn the_seam_keeps_the_jails_members_and_answers_only_inside_it() {
    assert_eq!(members(vec![4, 9, 12, 30], |p| p % 3 == 0), [9, 12, 30]);
    let outside = jail_pids("lab.jail.t.0123456789abcdef").expect_err("no jail here");
    assert!(outside.to_string().contains("not in the jail"), "{outside}");
    let bad = jail_pids("com.apple.lsd.open").expect_err("no mark");
    assert!(bad.to_string().contains("is no lab jail mark"), "{bad}");
}
