// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_size_floor_is_four_machines() {
    let a = |v: &[&str]| parse(&v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert!(a(&["--n", "3"]).is_err());
    assert_eq!(a(&["--n", "4"]).map(|a| a.n).ok(), Some(4));
    assert!(a(&["--keep"]).is_ok_and(|a| a.keep && a.n == 30));
}

/// Verify that `tty::one_line` produces every expected substring in [`HOSTILE_ESCAPED`].
#[test]
fn the_hostile_words_escape_to_the_forms_the_check_looks_for() {
    let escaped = aterm_codec::tty::one_line(fixture::HOSTILE_SAID, 160);
    for form in HOSTILE_ESCAPED {
        assert!(escaped.contains(form), "{form} in {escaped}");
    }
    assert!(!escaped.contains("trust=human"), "{escaped}");
}
