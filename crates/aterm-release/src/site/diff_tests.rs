// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_the_changed_lines_and_their_context_are_shown() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
    let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
    assert_eq!(
        unified("p", old, new),
        [
            "--- p",
            "+++ p (planned)",
            "@@ -2,8 +2,9 @@",
            " b",
            " c",
            " d",
            "-e",
            "+E",
            " f",
            " g",
            " h",
            " i",
            "+j",
        ]
    );
    assert!(unified("p", old, old).is_empty());
}
