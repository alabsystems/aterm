// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_a_lab_shaped_socket_passes() {
    assert!(lab_socket_shape("/tmp/at.PXaw/a.sock"));
    assert!(!lab_socket_shape("/tmp/at./a.sock"));
    assert!(!lab_socket_shape("/tmp/at.x/y/a.sock"));
    assert!(!lab_socket_shape("/private/tmp/at.x/a.sock"));
    assert!(!lab_socket_shape(
        "/Users//o/Library/Application Support/aterm/aterm.sock"
    ));
}

#[test]
fn a_pattern_crosses_the_wire_as_one_token() {
    assert_eq!(
        wire_pattern("❯|Welcome|trust|Claude Code"),
        "❯|Welcome|trust|Claude\\x20Code"
    );
    assert_eq!(wire_pattern("level=attention"), "level\\x3dattention");
    assert!(!wire_pattern("a b\tc").contains(char::is_whitespace));
    assert_eq!(wire_pattern(r"a\ b\=c"), r"a\x20b\x3dc");
    assert_eq!(wire_pattern(r"a\\ b\\\=c"), r"a\\\x20b\\\x3dc");
    for (pat, sample, non_sample) in [
        (r"a\ b", "a b", r"a\x20b"),
        (r"a\\ b", r"a\ b", "a b"),
        (r"[\ =]+", " = ", r"\x"),
    ] {
        let orig = aterm_regex::Regex::new(pat).unwrap();
        let wired = aterm_regex::Regex::new(&wire_pattern(pat)).unwrap();
        assert_eq!(orig.is_match(sample), wired.is_match(sample), "{pat}");
        assert!(wired.is_match(sample), "{pat}");
        assert_eq!(
            orig.is_match(non_sample),
            wired.is_match(non_sample),
            "{pat}"
        );
        assert!(!wired.is_match(non_sample), "{pat}");
    }
}
