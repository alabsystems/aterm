// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn dec_string_matches_display() {
    for v in [
        0usize,
        1,
        9,
        10,
        11,
        99,
        100,
        12345,
        65_535,
        1_000_000,
        usize::MAX - 1,
        usize::MAX,
    ] {
        assert_eq!(dec_string(v), format!("{v}"));
    }
}
